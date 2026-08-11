//! 編集の失敗を表す型と、応答へ載せる安全な補助情報。

use crate::alias::{AliasAdmissionError, AliasRejection, AliasRowRejection};
use crate::read::error::edit_blocked_details;
use crate::read::{EditState, ReadError};
use aviutl2_mcp_core::{ErrorCode, ItemValue, ItemWriteError, TrackValueError};
use serde_json::{Map, Value, json};

/// 応答の補助情報へ載せる文字列の上限文字数。
///
/// effect 名・設定項目名は要求元が指定を訂正するのに要るが、長さは要求元が
/// 決めるため、そのまま反響させると応答が膨らむ。ホストから読み直した設定値も
/// 同じ理由で切り詰める——長さを決めるのはホストであり、応答の大きさをこちら
/// 側で抑えられなくなる。
const MAX_NAME_CHARS: usize = 1_024;

/// 同じ要求をどう作り直せば通り得るか。
///
/// 再試行可否の真偽値だけでは「そのまま再送してよい」と「読み直して作り直す」を
/// 区別できない。前提条件の不整合は再試行可能だが、同じ selector と同じ前提を
/// そのまま送り直しても永久に失敗する。区別が無いと要求元は解消しない再試行へ
/// 入る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryRequires {
    /// そのまま再送してよい。
    Resend,
    /// 対象を読み直して要求を作り直す。
    Refetch,
    /// 再試行しても解消しない。
    None,
}

impl RetryRequires {
    /// 応答へ載せる機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            RetryRequires::Resend => "resend",
            RetryRequires::Refetch => "refetch",
            RetryRequires::None => "none",
        }
    }
}

/// 対象または SDK が変更に対応しない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// 登録されていない effect 名。
    EffectNotRegistered,
    /// 登録されてはいるが、その effect 名からはオブジェクトを作成できない。
    ///
    /// どの effect が作成の元になれるかは SDK が述べていない。作成の失敗そのもの
    /// でしか判別できないため、種別による事前の絞り込みは行わない。
    EffectNotCreatable,
    /// 出力項目で有効・無効を変更できない。
    EffectStateImmutable,
    /// その effect は列の中で順序を動かせない。
    ///
    /// 順序を動かせるのはフィルタ効果だけである。**名乗るのは発行の前だけで
    /// ある**——カタログの種別を読んで判定しており、名前が主張する内容を確かめ
    /// ている。発行の後の食い違いは
    /// [`EditError::EffectMoveNotApplied`] が名乗る。
    EffectNotMovable,
    /// SDK が対応しないメディアファイル。
    MediaNotSupported,
    /// 設定項目は存在するが、その種別への書き込みを公開していない。
    ///
    /// 対象 effect の設定項目の列挙は未知種別の項目を落とすため、列挙に現れない
    /// 項目が存在し得る。名前で値を読めた場合はここへ来る。
    ItemTypeNotWritable,
    /// 戻り値を持たない変更 API を呼んだが、読み直した状態が要求値と異なる。
    ///
    /// ホストが無言で拒否した場合にここへ来る。成功として返してはならない。
    ChangeNotApplied,
    /// 逆操作の材料が変更前に手元へ揃わない。
    ///
    /// 逆操作を組み立てられない変更は発行しない。実行してから組み立てられないと
    /// 分かる経路を作らないための拒否である。
    ///
    /// 読み取りが落ちた場合だけでなく、**材料を要する判定に材料が渡らなかった
    /// 場合**も含む。どちらも「変更前の生文字列が無い」ことに変わりはなく、
    /// 発行してよい理由にならない。
    InverseUnavailable,
}

impl UnsupportedReason {
    /// 全 variant。
    ///
    /// [`UnsupportedReason::as_str`] が返し得る名前を数え上げるために用いる。
    pub const ALL: &'static [UnsupportedReason] = &[
        UnsupportedReason::EffectNotRegistered,
        UnsupportedReason::EffectNotCreatable,
        UnsupportedReason::EffectStateImmutable,
        UnsupportedReason::EffectNotMovable,
        UnsupportedReason::MediaNotSupported,
        UnsupportedReason::ItemTypeNotWritable,
        UnsupportedReason::ChangeNotApplied,
        UnsupportedReason::InverseUnavailable,
    ];

    /// 応答へ載せる機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            UnsupportedReason::EffectNotRegistered => "effect_not_registered",
            UnsupportedReason::EffectNotCreatable => "effect_not_creatable",
            UnsupportedReason::EffectStateImmutable => "effect_state_immutable",
            UnsupportedReason::EffectNotMovable => "effect_not_movable",
            UnsupportedReason::MediaNotSupported => "media_not_supported",
            UnsupportedReason::ItemTypeNotWritable => "item_type_not_writable",
            UnsupportedReason::ChangeNotApplied => "change_not_applied",
            UnsupportedReason::InverseUnavailable => "inverse_unavailable",
        }
    }
}

impl std::fmt::Display for UnsupportedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            UnsupportedReason::EffectNotRegistered => "指定された effect は登録されていません",
            UnsupportedReason::EffectNotCreatable => {
                "指定された effect からはオブジェクトを作成できません"
            }
            UnsupportedReason::EffectStateImmutable => "対象 effect の有効・無効は変更できません",
            UnsupportedReason::EffectNotMovable => "対象 effect は列の中で順序を動かせません",
            UnsupportedReason::MediaNotSupported => "対応していないメディアファイルです",
            UnsupportedReason::ItemTypeNotWritable => {
                "この種別の設定項目への書き込みには対応していません"
            }
            UnsupportedReason::ChangeNotApplied => "要求した変更が反映されませんでした",
            UnsupportedReason::InverseUnavailable => "逆操作の材料を読み取れませんでした",
        };
        f.write_str(text)
    }
}

/// 変更 API が SDK へ届かずに失敗した理由。
///
/// SDK ラッパーは対象の存在確認・整数変換・NUL 検査を呼び出しの入口で行い、
/// これらに引っ掛かった要求は SDK を呼ばずに戻る。プロジェクトは一切変わって
/// いないため、変更の発行として記録してはならない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotIssuedReason {
    /// 対象がホスト側に存在しない。
    TargetMissing,
    /// 引数を SDK の型へ写せない。
    ArgumentNotRepresentable,
}

impl NotIssuedReason {
    /// 全 variant。
    ///
    /// [`NotIssuedReason::as_str`] が返し得る名前を数え上げるために用いる。
    pub const ALL: &'static [NotIssuedReason] = &[
        NotIssuedReason::TargetMissing,
        NotIssuedReason::ArgumentNotRepresentable,
    ];

    /// 応答へ載せる機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            NotIssuedReason::TargetMissing => "target_missing",
            NotIssuedReason::ArgumentNotRepresentable => "argument_not_representable",
        }
    }
}

impl std::fmt::Display for NotIssuedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            NotIssuedReason::TargetMissing => "変更の対象が存在しません",
            NotIssuedReason::ArgumentNotRepresentable => "指定された値を受け渡せません",
        };
        f.write_str(text)
    }
}

/// 中間点の変更が、読み直した区間の実態と食い違う理由。
///
/// いずれも判定に使う値がオブジェクトの現在の状態であり、要求元が持っているのは
/// 読み取った時点の複製である。その間に UI で中間点が動けば、正しい手続きを
/// 踏んだ要求が落ちる。復帰の手段は対象の読み直しであるため、要求の誤りでは
/// なく前提条件の不整合として扱う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionPreconditionReason {
    /// 中間点を置くフレームがオブジェクトの範囲の外にある。
    FrameOutsideObject,
    /// 指定したフレームが既に区間の開始フレームである。
    SectionBoundaryExists,
    /// 区間番号がオブジェクトの区間数以上である。
    SectionIndexOutOfRange,
    /// 移動先が隣の中間点を越えている。
    ///
    /// 中間点の順序が入れ替わらないことは SDK の不変条件であり、それを崩す
    /// 要求は届く前に落とす。
    SectionMoveCrossesBoundary,
}

impl SectionPreconditionReason {
    /// 全 variant。
    ///
    /// [`SectionPreconditionReason::as_str`] が返し得る名前を数え上げるために
    /// 用いる。
    pub const ALL: &'static [SectionPreconditionReason] = &[
        SectionPreconditionReason::FrameOutsideObject,
        SectionPreconditionReason::SectionBoundaryExists,
        SectionPreconditionReason::SectionIndexOutOfRange,
        SectionPreconditionReason::SectionMoveCrossesBoundary,
    ];

    /// 応答へ載せる機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            SectionPreconditionReason::FrameOutsideObject => "frame_outside_object",
            SectionPreconditionReason::SectionBoundaryExists => "section_boundary_exists",
            SectionPreconditionReason::SectionIndexOutOfRange => "section_index_out_of_range",
            SectionPreconditionReason::SectionMoveCrossesBoundary => {
                "section_move_crosses_boundary"
            }
        }
    }
}

impl std::fmt::Display for SectionPreconditionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            SectionPreconditionReason::FrameOutsideObject => {
                "指定されたフレームはオブジェクトの範囲外です"
            }
            SectionPreconditionReason::SectionBoundaryExists => {
                "指定されたフレームは既に区間の開始位置です"
            }
            SectionPreconditionReason::SectionIndexOutOfRange => {
                "指定された区間番号はオブジェクトの区間数を超えています"
            }
            SectionPreconditionReason::SectionMoveCrossesBoundary => {
                "移動先が隣の中間点を越えています"
            }
        };
        f.write_str(text)
    }
}

/// effect の変更が、読み直した effect の列の実態と食い違う理由。
///
/// 判定に使う値は対象の現在の状態であり、要求元が持っているのは読み取った時点の
/// 複製である。その間に UI で effect が増減すれば、正しい手続きを踏んだ要求が
/// 落ちる。復帰の手段は対象の読み直しであるため、要求の誤りではなく前提条件の
/// 不整合として扱う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectPreconditionReason {
    /// 移動先の位置が effect の列の長さ以上である。
    PositionOutOfRange,
}

impl EffectPreconditionReason {
    /// 全 variant。
    ///
    /// [`EffectPreconditionReason::as_str`] が返し得る名前を数え上げるために
    /// 用いる。
    pub const ALL: &'static [EffectPreconditionReason] =
        &[EffectPreconditionReason::PositionOutOfRange];

    /// 応答へ載せる機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            EffectPreconditionReason::PositionOutOfRange => "effect_position_out_of_range",
        }
    }
}

impl std::fmt::Display for EffectPreconditionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            EffectPreconditionReason::PositionOutOfRange => {
                "指定された移動先は effect の列の長さを超えています"
            }
        };
        f.write_str(text)
    }
}

/// 一括適用が失敗したあと、発行済みの変更をどこまで戻せたか。
///
/// 巻き戻しは 1 件失敗しても止めず、全件を試みたうえで結末を名乗る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackOutcome {
    /// 巻き戻しを試みていない。変更を 1 つも発行していない失敗で用いる。
    NotAttempted,
    /// 発行済みの変更を全て戻した。
    Complete {
        /// 戻した件数。
        count: usize,
    },
    /// 戻せなかった変更がある。プロジェクトは中途半端な状態にある。
    Incomplete {
        /// 戻せた件数。
        ///
        /// **復旧の手掛かりであって、被害の正確な計量ではない。** 移動の
        /// 逆操作が 1 件失敗すると、その対象は先行する移動の元位置を塞いだ
        /// ままになり、個別に試みれば戻せたはずの逆操作が連鎖して失敗し得る。
        /// したがってこの値は「戻せたはずの件数」を下回り得る。
        count: usize,
    },
    /// 巻き戻しを実行できなかった。中途半端な状態が残った可能性がある。
    ///
    /// 区間の panic は逆操作を保持する計画ごと巻き戻すため、どこまで適用した
    /// かも、巻き戻しの途中だったかも分からない。
    Impossible,
}

/// 変更を発行した後に落ちたとき、対象を発行前の値へ戻せたか。
///
/// [`RollbackOutcome`] とは別に置く。あちらは複数の sub-operation を件数で
/// 数えるものであり、単独の書き込みで戻す対象は常に 1 つの設定項目である。
/// 件数を持つ型を借りると、常に 0 か 1 しか取らない数を要求元へ渡すことになる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemRestore {
    /// この層では巻き戻していない。
    ///
    /// 一括適用の sub-operation がこれである。要求全体の巻き戻し計画が別に
    /// あり、結末は [`RollbackOutcome`] が名乗る。
    NotAttempted,
    /// 対象は書き込み前の値を持つ。
    ///
    /// **戻した場合と、戻すものが無かった場合の双方を表す。** ホストが値を
    /// 動かさずに書き込みを捨てたとき、対象は書き込み前の値のままであり、
    /// 要求元から見た状態は戻した場合と区別がつかない。区別に名前を与えても
    /// 要求元が取る行動は変わらない。
    Restored,
    /// 戻せなかった。対象は書き込み前の値を持たない。
    Failed,
}

/// 宛先を塞いでいる既存オブジェクトが占めるフレーム範囲。
///
/// 名前も fingerprint も持たない。要求元に要るのは「どこまで塞がっているか」
/// だけであり、他人の対象の同一性を渡す理由が無い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OccupiedRange {
    /// 塞いでいる対象の開始フレーム番号。
    pub frame_start: usize,
    /// 塞いでいる対象の終了フレーム番号。
    pub frame_end: usize,
}

/// 1 要求が epoch を 2 か所から受け取る場合の、食い違った側。
///
/// 出所を名乗るのは、要求が前提の epoch と focus 対象のセレクターの双方から
/// epoch を受け取る場合だけである。1 か所からしか受け取らない要求で出所を
/// 名乗ると、要求元は 1 つしか送っていない値に対して 2 つの分岐を持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochSource {
    /// 要求が前提として運ぶ epoch。
    Expected,
    /// focus 対象のセレクターが運ぶ epoch。
    Focus,
}

/// 前提条件のうち、どれが食い違ったか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mismatch {
    /// プロジェクトの epoch。出所は区別しない。
    ProjectEpoch,
    /// 前提として運ばれたプロジェクトの epoch。
    ExpectedProjectEpoch,
    /// focus 対象のセレクターが運ぶプロジェクトの epoch。
    FocusProjectEpoch,
    /// 現在シーンの ID。
    SceneId,
    /// 対象の fingerprint。
    Fingerprint,
}

impl Mismatch {
    /// 応答へ載せる機械可読な名前。
    fn as_str(self) -> &'static str {
        match self {
            Mismatch::ProjectEpoch => "project_epoch",
            Mismatch::ExpectedProjectEpoch => "expected_project_epoch",
            Mismatch::FocusProjectEpoch => "focus_project_epoch",
            Mismatch::SceneId => "scene_id",
            Mismatch::Fingerprint => "fingerprint",
        }
    }
}

/// 編集の失敗。
///
/// 補助情報には SDK のハンドル・生ポインタ・設定値・alias・パスを含めない。
/// 含めるのは要求元が次の行動を決められる値だけである。
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// 受付判定・対象解決・read-back で生じた失敗。
    ///
    /// これらは編集区間の内側で行う読み取りであり、読み取り経路と同じ失敗分類を
    /// 持つ。別の列挙へ写し替えると、共有した解決実装の戻り値を機械的に変換する
    /// 層が要り、対応の取り違えを招く。
    #[error(transparent)]
    Read(#[from] ReadError),
    /// 再生中・出力中で編集区間へ入れない。
    ///
    /// [`ReadError::EditBlocked`] とは分ける。[`EditError::Read`] は編集の途中で
    /// 実際に読み取りが落ちる場合にも使われており、そちらは読み取りとして名乗る
    /// のが正しい。どちらの経路で落ちたかは文言だけで読める。
    ///
    /// エラーコードも補助情報も読み取りの拒否と同じものを名乗る。要求元へ求める
    /// 行動（同じ時間だけ待って送り直す）が変わらないためである。
    #[error("{state}のため編集できません")]
    EditBlocked {
        /// 編集を妨げている編集状態。
        state: EditState,
    },
    /// 作成・移動の宛先に既存オブジェクトがある。
    #[error("宛先に既存のオブジェクトがあります")]
    DestinationOccupied {
        /// 宛先のレイヤー番号。
        layer: usize,
        /// 宛先の開始フレーム番号。
        frame: usize,
        /// 宛先を塞いでいる既存オブジェクトが占める範囲。
        occupied_by: OccupiedRange,
    },
    /// epoch を 2 か所から受け取る要求で、どちらかが現在の epoch と異なる。
    ///
    /// 出所を区別しない食い違いは [`ReadError::EpochMismatch`] が表す。
    #[error("プロジェクトの epoch が要求の前提と一致しません")]
    EpochMismatch {
        /// 食い違った側。
        origin: EpochSource,
    },
    /// 対象または宛先のレイヤーがロックされている。
    #[error("レイヤーがロックされています")]
    LayerLocked {
        /// ロックされているレイヤー番号。
        layer: usize,
    },
    /// 解決した effect の fingerprint がセレクターの値と一致しない。
    ///
    /// 所属オブジェクトの照合は既に通っている。オブジェクトの概要を添えても、
    /// それは要求元が送ってきたセレクターと同じ位置・同じ fingerprint であり、
    /// 復帰の手掛かりを 1 つも増やさない。添えれば要求元は「そのまま送り返せば
    /// 通る」という契約に従って同じ失敗を繰り返す。**要求元は effect の一覧を
    /// 読み直す必要がある**ため、現在の姿を名乗らずに読み直しを促す。
    #[error("対象 effect の fingerprint が要求と一致しません")]
    EffectFingerprintMismatch,
    /// セレクターが指す effect が存在しない。
    #[error("セレクターに一致する effect がありません: {effect_name}")]
    EffectNotFound {
        /// 要求された effect 名。
        effect_name: String,
        /// 要求された同名 effect 内の位置。
        effect_index: usize,
    },
    /// 解決した結果、2 つの sub-operation が同じ状態を書き換えると分かった。
    ///
    /// 要求内容だけの検証はセレクターの文字列としての同一性を見る。名前を
    /// 指定したセレクターと指定しないセレクターは文字列として異なるが、同じ
    /// 対象へ解決し得るため、そこだけでは取りこぼす。すり抜けると、後から
    /// 適用する変更の逆操作が先の変更の結果を指すことになり、逆操作を事前に
    /// 完全へ組み立てられない要求がそのまま実行される。
    #[error("同じ状態を書き換える sub-operation が複数あります")]
    DuplicateTarget,
    /// 設定項目への書き込みを受け付けられない。
    #[error(transparent)]
    ItemWrite(ItemWriteError),
    /// 書き込んだ設定値を読み直したところ、要求した値になっていなかった。
    ///
    /// ホストは書き込みの成否を返さない。値域を外れた数値の切り詰め、小数の
    /// 丸め、書式の合わない色の既定値への置き換え、未登録のフォント名と選択肢に
    /// 無い値の黙殺は、いずれも読み直しでしか観測できない。
    ///
    /// **種別ごとに理由を分けない。** 「ホストが拒んだ」と「ホストが別の値へ
    /// 倒した」を区別する材料がこちら側に無く、どちらも「読み直しが要求と違う」
    /// としか観測できない。照合の仕組みが 1 つである以上、名前も 1 つである。
    ///
    /// [`UnsupportedReason::ChangeNotApplied`] とは畳まない。あちらは変更を拒む
    /// 旨をヘッダーが記していない API で起きる想定外の不一致であり、こちらは
    /// 値域も選択肢も知る手段が無い以上、当て推量が外れて頻発する。要求元が取る
    /// 行動も違う——前者は異常として報告し、後者は要求した値が受け付けられ
    /// なかったものとして値を選び直す。
    ///
    /// **書き込みは既に発行済みである。** 単独の書き込みはその後に対象を
    /// 書き込み前の値へ戻す。戻せたかは包みの [`EditError::AfterMutation`] が
    /// 名乗る。
    #[error("書き込んだ設定値が読み直した値と一致しません")]
    ItemValueNotApplied {
        /// 書き込みの後に読み直した値。
        ///
        /// **要求された値ではない。** 要求した値がホストの手でどうなったかを
        /// 示す測定であり、値域を外れた数値ならその境界そのものが現れる。
        ///
        /// **応答が返る時点の現在値ではない。** 巻き戻しが成功していれば、
        /// 対象は書き込み前の値を持つ。
        observed: String,
    },
    /// 書き込むと、対象がいま持つ移動が消える。
    ///
    /// ホストは受理して破壊する。**書き込みは発行していない**——発行してしまえば、
    /// 消えた移動を復元する手段がこちら側に無い。
    ///
    /// [`EditError::ItemValueNotApplied`] とは畳まない。あちらは書き込んだ後に
    /// しか分からない食い違いであり、こちらは書き込む前に分かる。要求元が取る
    /// 行動も違う——前者は受け付けられる値を選び直し、後者は消したい移動を
    /// `mode` が `null` の値で明示するか、対象の移動を含む値を送る。
    #[error("移動を持つ設定項目へは移動を含む値でしか書き込めません")]
    MovementWouldBeLost {
        /// ホストが現在保持している値。
        ///
        /// **要求された値ではない。** 対象がいまどの移動を持つかは、この値に
        /// しか現れない。要求元はこれを読んで、書き戻すか消すかを決める。
        current_value: String,
    },
    /// 対象または SDK が変更に対応しない。
    #[error("{reason}")]
    UnsupportedTarget {
        /// 対応しない理由。
        reason: UnsupportedReason,
    },
    /// 名前で指定された登録済みエイリアスが受け入れ規則を通らない。
    ///
    /// エラーコードも種別の名前も落ちた条件そのものが決める。ここで写し直すと、
    /// 一覧が除外に使う規則と作成が拒否に使う規則が別々の答えを持つ。
    #[error(transparent)]
    AliasRejected(#[from] AliasRejection),
    /// 生テキストのエイリアスが含む行を書き込めない。
    ///
    /// ホストは不正な移動行を失敗として返さず、その行ごと捨てて既定値へ倒す。
    /// 移動行の検証は設定項目を書く経路と共有する。
    ///
    /// テキスト種別の値は、エイリアスから読むときにだけ `\` がエスケープとして
    /// 解かれる。エスケープを組まない綴りは書いたとおりの意味で保たれないため、
    /// この経路でだけ綴りを検証する。
    ///
    /// **どの節のどの項目かを添える。** 1 つのエイリアスは複数のオブジェクトと
    /// 複数の effect を持ち得るため、行を特定できなければ直せない。**値そのものは
    /// 添えない。**
    #[error("エイリアスの行を書き込めません: {source}")]
    AliasRowRejected {
        /// 行が属する節の見出し。どの節にも属さない行では `None`。
        heading: Option<String>,
        /// 行の項目名。
        item: String,
        /// 落ちた書き込みの検証。
        #[source]
        source: ItemWriteError,
    },
    /// 読み直した区間の実態が要求と食い違う。
    #[error("{reason}")]
    SectionPrecondition {
        /// 食い違いの内容。
        reason: SectionPreconditionReason,
    },
    /// 読み直した effect の列の実態が要求と食い違う。
    #[error("{reason}")]
    EffectPrecondition {
        /// 食い違いの内容。
        reason: EffectPreconditionReason,
    },
    /// 発行した effect の移動が、読み直した列に現れていない。
    ///
    /// 移動 API は移動後のインデックスを返すが、その数が列全体を数えたものか
    /// フィルタ効果だけを数えたものかを SDK は述べていない。可否を決めるのは
    /// 列の読み直しだけであり、**ホストが返した値は判定に使わない。**
    ///
    /// **理由は 1 つである。** ホストが拒んだのか別の位置へ倒したのかを、発行の
    /// 後に我々の側から区別できない。どちらも
    /// [`UnsupportedReason::ChangeNotApplied`] を名乗り、**列が動いたかどうかは
    /// 巻き戻しの結末が運ぶ。** 要求元が次に採れる位置は
    /// [`Self::EffectMoveNotApplied::reported_position`] が示す。
    #[error("{}", UnsupportedReason::ChangeNotApplied)]
    EffectMoveNotApplied {
        /// ホストが名乗った移動後のインデックス。
        ///
        /// **可否の判定には使っていない。** 食い違いが起きたときに、どちらの
        /// 解釈だったかを応答から読めるようにするために載せる。
        reported_position: usize,
    },
    /// 事前確認を通した中間点の変更を SDK が拒んだ。
    ///
    /// 食い違っているのは要求元ではなく我々と SDK である。要求元に直せることは
    /// 無く、我々の事前確認の漏れか SDK の未知の制約かのどちらかであるため、
    /// 失敗した SDK 関数名を添えてログを見るよう促す。
    #[error("中間点の変更が拒否されました: {operation}")]
    SectionChangeRejected {
        /// 拒否した SDK 関数の名前。
        operation: &'static str,
    },
    /// SDK の呼び出しが失敗した。
    #[error("SDK の呼び出しに失敗しました: {operation}")]
    Sdk {
        /// 失敗した SDK 関数の名前。
        operation: &'static str,
    },
    /// 変更 API が SDK へ届く前に失敗した。
    ///
    /// プロジェクトは変わっていないため、変更の発行として記録しない。失敗した
    /// SDK 関数名も載せない。呼ばれていない関数を名指しすると、要求元にも
    /// 運用者にも誤った手掛かりを与える。
    #[error("{reason}")]
    NotIssued {
        /// 届かなかった理由。
        reason: NotIssuedReason,
    },
    /// 編集区間の処理で panic を捕捉した。
    #[error("編集処理で panic を捕捉しました")]
    Panicked,
    /// 1 つの前提から 2 つ目の変更許可を取ろうとした。
    ///
    /// 許可はそれぞれ独立に発行を数えるため、2 つ取ると同じ前提のもとで
    /// revision が 2 度進み、応答が返す値がどちらの許可のものか定まらない。
    /// 呼び出し側の組み立ての誤りであり、同じ要求を作り直しても解消しない。
    #[error("1 つの前提から変更の許可を 2 度取ることはできません")]
    MutationPermitReissued,
    /// SDK の変更 API を発行した後に生じた失敗。
    ///
    /// エラーコードは失敗の理由を表すものを保ち、書き換えない。変更が入った
    /// という情報は補助情報の `mutation_issued` が担う。
    ///
    /// **発行した変更を元へ戻したかも、この包みが名乗る。** [`EditError::Batch`]
    /// が要求全体の巻き戻しを名乗るのと同じ形である。**戻せたかという問いは
    /// 失敗の種類に依らない**——読み直した値が要求と違った場合も、読み直し
    /// そのものが落ちた場合も、対象が書き込み前の値を持つかは同じ語彙で答え
    /// られる。葉の失敗へ持たせると、後者が名乗る場所を失う。
    #[error("{source}")]
    AfterMutation {
        /// 発行後に生じた失敗そのもの。
        #[source]
        source: Box<EditError>,
        /// 加算後の revision。
        project_revision: u64,
        /// 発行した変更を元へ戻せたか。
        restore: ItemRestore,
    },
    /// 一括適用の失敗。
    ///
    /// エラーコードは適用相までの失敗の理由をそのまま保つ。要求元が知るべきは
    /// 「なぜ一括適用が失敗したか」であり、巻き戻せたかどうかは別のキーが
    /// 担う。**戻せなかった場合だけは書き換える** — 元の理由が何であれ、要求元
    /// が直面している問題は「プロジェクトが中途半端な状態にある」ことへ
    /// 変わっているからである。
    #[error("{source}")]
    Batch {
        /// 失敗そのもの。
        #[source]
        source: Box<EditError>,
        /// 落ちた sub-operation の 0 始まりの位置。
        ///
        /// どこで落ちたか分からない失敗（区間の panic）では `None`。
        failed_index: Option<usize>,
        /// 発行済みの変更をどこまで戻せたか。
        rollback: RollbackOutcome,
    },
}

/// 受け入れの失敗を、編集の失敗へ振り分ける。
///
/// 規則で落ちた条件はそのまま運び、環境の事実だけを別の失敗へ写す。振り分けは
/// 2 つの腕であり、エラーコードも種別の名前も落ちた側が決める。
impl From<AliasAdmissionError> for EditError {
    fn from(error: AliasAdmissionError) -> Self {
        match error {
            AliasAdmissionError::DirectoryUnavailable => {
                ReadError::AliasDirectoryUnavailable.into()
            }
            AliasAdmissionError::Rejected(rejection) => EditError::AliasRejected(rejection),
        }
    }
}

/// 生テキストの行の失敗を、編集の失敗へ振り分ける。
///
/// 表として解釈できない生テキストは、名前で指定されたエイリアスが同じ条件で
/// 落ちたときと同じ失敗になる。エラーコードも種別の名前も落ちた条件が決める。
impl From<AliasRowRejection> for EditError {
    fn from(error: AliasRowRejection) -> Self {
        match error {
            AliasRowRejection::Rejected(rejection) => EditError::AliasRejected(rejection),
            AliasRowRejection::Row {
                heading,
                item,
                source,
            } => EditError::AliasRowRejected {
                heading,
                item,
                source,
            },
        }
    }
}

impl EditError {
    /// 変更 API を発行した後の失敗として包み直す。
    ///
    /// 既に包まれている場合は最初の revision を保つ。1 要求で revision が
    /// 進むのは 1 度だけであり、後から観測した値で上書きすると要求元へ返す
    /// 値が発行時点のものでなくなる。
    pub fn after_mutation(self, project_revision: u64) -> Self {
        match self {
            EditError::AfterMutation { .. } => self,
            source => EditError::AfterMutation {
                source: Box::new(source),
                project_revision,
                restore: ItemRestore::NotAttempted,
            },
        }
    }

    /// 書き込み検証の失敗が読み直した値。他の失敗では `None`。
    ///
    /// 変更を発行した後の失敗は [`EditError::AfterMutation`] へ包まれて返る
    /// ため、包みの内側まで辿る。**巻き戻すかどうかの判定に要る値であり、
    /// 判定のためだけに対象をもう一度読まないための口である。**
    pub(crate) fn observed_item_value(&self) -> Option<&str> {
        match self {
            EditError::ItemValueNotApplied { observed } => Some(observed),
            EditError::AfterMutation { source, .. } => source.observed_item_value(),
            _ => None,
        }
    }

    /// 発行した変更を元へ戻した結末を書き入れる。
    ///
    /// 巻き戻しは失敗を知って初めて始まるため、結末は失敗を組み立てた後にしか
    /// 決まらない。**変更を発行していない失敗はそのまま素通りする**——戻す
    /// 対象そのものが無い。
    pub(crate) fn with_item_restore(self, restore: ItemRestore) -> Self {
        match self {
            EditError::AfterMutation {
                source,
                project_revision,
                ..
            } => EditError::AfterMutation {
                source,
                project_revision,
                restore,
            },
            other => other,
        }
    }

    /// 発行の包みを 1 枚剥がし、失敗そのものを返す。
    ///
    /// 一括適用の sub-operation は変更を発行し終えてから落ちるため
    /// [`EditError::AfterMutation`] に包まれる。包みが名乗る復旧の結果は常に
    /// [`ItemRestore::NotAttempted`] であり——結末は要求全体の巻き戻しが持つ
    /// ——包んだまま読むと、戻っている失敗まで読み直しへ倒れる。
    fn stopped_by(&self) -> &EditError {
        match self {
            EditError::AfterMutation { source, .. } => source,
            other => other,
        }
    }

    /// 応答へ載せるエラーコードを返す。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            EditError::Read(error) => error.error_code(),
            EditError::EditBlocked { .. } => ErrorCode::EditBlocked,
            EditError::DestinationOccupied { .. }
            | EditError::LayerLocked { .. }
            | EditError::EpochMismatch { .. }
            | EditError::SectionPrecondition { .. }
            | EditError::EffectPrecondition { .. }
            | EditError::EffectFingerprintMismatch => ErrorCode::PreconditionFailed,
            EditError::EffectNotFound { .. } => ErrorCode::NotFound,
            EditError::DuplicateTarget => ErrorCode::InvalidArgument,
            EditError::ItemWrite(error) => error.error_code(),
            EditError::ItemValueNotApplied { .. } => ErrorCode::UnsupportedOperation,
            EditError::MovementWouldBeLost { .. } => ErrorCode::UnsupportedOperation,
            EditError::UnsupportedTarget { .. } | EditError::EffectMoveNotApplied { .. } => {
                ErrorCode::UnsupportedOperation
            }
            EditError::AliasRejected(rejection) => rejection.error_code(),
            EditError::AliasRowRejected { source, .. } => source.error_code(),
            EditError::SectionChangeRejected { .. } | EditError::Sdk { .. } => ErrorCode::SdkError,
            EditError::NotIssued { reason } => match reason {
                NotIssuedReason::TargetMissing => ErrorCode::NotFound,
                NotIssuedReason::ArgumentNotRepresentable => ErrorCode::InvalidArgument,
            },
            EditError::Panicked | EditError::MutationPermitReissued => ErrorCode::InternalError,
            EditError::AfterMutation { source, .. } => source.error_code(),
            // 戻せなかった変更が残っている場合だけ、失敗の理由を
            // 「中途半端な状態にある」ことへ寄せる。
            EditError::Batch {
                rollback: RollbackOutcome::Incomplete { .. },
                ..
            } => ErrorCode::SdkError,
            EditError::Batch { source, .. } => source.error_code(),
        }
    }

    /// 同じ要求をそのまま再送して成功し得るか。
    pub fn retryable(&self) -> bool {
        self.error_code().default_retryable()
    }

    /// 前提条件のうち食い違ったものを返す。前提条件以外の失敗では `None`。
    fn mismatch(&self) -> Option<Mismatch> {
        match self {
            EditError::Read(ReadError::EpochMismatch) => Some(Mismatch::ProjectEpoch),
            EditError::EpochMismatch { origin } => Some(match origin {
                EpochSource::Expected => Mismatch::ExpectedProjectEpoch,
                EpochSource::Focus => Mismatch::FocusProjectEpoch,
            }),
            EditError::Read(ReadError::SceneMismatch { .. }) => Some(Mismatch::SceneId),
            EditError::Read(ReadError::FingerprintMismatch { .. })
            | EditError::EffectFingerprintMismatch => Some(Mismatch::Fingerprint),
            EditError::AfterMutation { source, .. } | EditError::Batch { source, .. } => {
                source.mismatch()
            }
            _ => None,
        }
    }

    /// 要求元が取るべき再試行のしかたを返す。
    fn retry_requires(&self) -> RetryRequires {
        match self {
            // 変更が入った可能性がある以上、そのまま再送してよい状況は無い。
            // 戻せた場合も同じである——戻ったのは我々が戻したからであり、同じ
            // 要求をもう一度送れば同じ書き込みがもう一度発行される。
            //
            // 読み直しを案内するかは復旧の結果が決める。戻せた対象は書き込み前の
            // 値を持ち、読み直した先には要求の前と同じ値が在る。要求元が次に取る
            // 行動（受け付けられる値を選び直す・別の位置を指す）はそこからは得ら
            // れない。戻せなかった場合とこの層で巻き戻していない場合は、次の要求を
            // 組む材料が読み直しにしか無い。
            EditError::AfterMutation { restore, .. } => match restore {
                ItemRestore::Restored => RetryRequires::None,
                ItemRestore::Failed | ItemRestore::NotAttempted => RetryRequires::Refetch,
            },
            // 一括適用では、戻っているかだけが分かれ目である。全て戻った場合と
            // 1 つも発行していない場合は、プロジェクトが要求の前と同じであり、
            // 案内は止めた失敗そのものが決める。戻せなかった場合は中途半端な
            // 状態が残っており、次の編集の前に読み直すほかない。
            EditError::Batch {
                source, rollback, ..
            } => match rollback {
                RollbackOutcome::NotAttempted | RollbackOutcome::Complete { .. } => {
                    source.stopped_by().retry_requires()
                }
                RollbackOutcome::Incomplete { .. } | RollbackOutcome::Impossible => {
                    RetryRequires::Refetch
                }
            },
            EditError::Read(ReadError::NotReady)
            | EditError::Read(ReadError::EditBlocked { .. })
            | EditError::EditBlocked { .. } => RetryRequires::Resend,
            // 読み直せば宛先の空きが分かる。
            EditError::DestinationOccupied { .. } => RetryRequires::Refetch,
            // ロックの解除は別の operation であり、同じ要求を送り直しても対象を
            // 読み直しても解消しない。この 3 値は「この要求をどう扱うか」だけを
            // 表すため、値を増やさず「再試行では解消しない」とする。
            EditError::LayerLocked { .. } => RetryRequires::None,
            other => match other.error_code() {
                ErrorCode::PreconditionFailed => RetryRequires::Refetch,
                _ => RetryRequires::None,
            },
        }
    }

    /// 応答へ載せる補助情報を組み立てる。
    ///
    /// 設定値・alias・パスは含めない。effect 名と設定項目名は登録済みの識別子で
    /// あり要求の訂正に要るため、長さを切り詰めた上で載せる。
    pub fn details(&self) -> Value {
        let mut details = Map::new();
        self.fill_details(&mut details);
        if let Some(mismatch) = self.mismatch() {
            details.insert("mismatch".to_string(), json!(mismatch.as_str()));
        }
        details.insert(
            "retry_requires".to_string(),
            json!(self.retry_requires().as_str()),
        );
        Value::Object(details)
    }

    /// 失敗の種類ごとの補助情報を書き込む。
    fn fill_details(&self, details: &mut Map<String, Value>) {
        match self {
            EditError::Read(error) => merge(details, error.details()),
            EditError::EditBlocked { state } => merge(details, edit_blocked_details(*state)),
            EditError::DestinationOccupied {
                layer,
                frame,
                occupied_by,
            } => {
                details.insert("reason".to_string(), json!("destination_occupied"));
                details.insert("layer".to_string(), json!(layer));
                details.insert("frame".to_string(), json!(frame));
                details.insert(
                    "occupied_by".to_string(),
                    json!({
                        "frame_start": occupied_by.frame_start,
                        "frame_end": occupied_by.frame_end,
                    }),
                );
            }
            EditError::LayerLocked { layer } => {
                details.insert("reason".to_string(), json!("layer_locked"));
                details.insert("layer".to_string(), json!(layer));
            }
            // 食い違った側は `mismatch` が名乗る。
            EditError::EpochMismatch { .. } | EditError::EffectFingerprintMismatch => {}
            EditError::EffectNotFound {
                effect_name,
                effect_index,
            } => {
                details.insert("effect_name".to_string(), json!(truncate(effect_name)));
                details.insert("effect_index".to_string(), json!(effect_index));
            }
            EditError::DuplicateTarget => {
                details.insert("reason".to_string(), json!("duplicate_target"));
            }
            EditError::ItemWrite(error) => fill_item_write_details(details, error),
            // 載せるのはホストが返した実値だけである。**要求された値は反響させ
            // ない。** 読み直した値はホストの現在の状態であって要求元の内容では
            // なく、同じ値は成功した書き込みの応答にも載る。
            //
            // **`current_value` とは名乗らない。** 巻き戻しが済んだ時点で、この
            // 値は現在値ではない。現在値と名乗れば要求元はそのまま送り返し、
            // 巻き戻したはずの状態を自分で再現することになる。
            //
            // **`coerced_value` とも名乗らない。** ホストが値を動かさずに書き
            // 込みを捨てた場合、この値は変更前の値そのものである。「倒した値」と
            // 名乗れば、その応答が「ホストがあなたの値を元の値へ倒した」と読める。
            // `observed_value` は測定そのものを名乗るため 2 つの階級のどちらでも
            // 正しい。
            EditError::ItemValueNotApplied { observed } => {
                details.insert("reason".to_string(), json!("item_value_not_applied"));
                details.insert("observed_value".to_string(), json!(truncate(observed)));
            }
            // 載せるのは対象がいま持つ移動である。要求元はこれを読んで、
            // 書き戻すか消すかを決める。要求された値は反響させない。
            EditError::MovementWouldBeLost { current_value } => {
                details.insert("reason".to_string(), json!("track_movement_present"));
                details.insert("current_value".to_string(), json!(truncate(current_value)));
            }
            EditError::UnsupportedTarget { reason } => {
                details.insert("reason".to_string(), json!(reason.as_str()));
            }
            // 落ちた条件が組み立てた補助情報をそのまま取り込む。名前もファイルの
            // 内容も含まないことは、組み立てる側が保証している。
            EditError::AliasRejected(rejection) => merge(details, rejection.details()),
            // 落ちた検証が組み立てた手掛かりへ、行の在処を添える。名前だけで
            // あり、行が持っていた値は運ばない。
            EditError::AliasRowRejected {
                heading,
                item,
                source,
            } => {
                fill_item_write_details(details, source);
                if let Some(heading) = heading {
                    details.insert("heading".to_string(), json!(truncate(heading)));
                }
                details.insert("item".to_string(), json!(truncate(item)));
            }
            EditError::SectionPrecondition { reason } => {
                details.insert("reason".to_string(), json!(reason.as_str()));
            }
            EditError::EffectPrecondition { reason } => {
                details.insert("reason".to_string(), json!(reason.as_str()));
            }
            // ホストが名乗った位置を添える。判定には使っていないため、応答に
            // 現れるのは照合が食い違ったときだけである。
            EditError::EffectMoveNotApplied { reported_position } => {
                details.insert(
                    "reason".to_string(),
                    json!(UnsupportedReason::ChangeNotApplied.as_str()),
                );
                details.insert("reported_position".to_string(), json!(reported_position));
            }
            EditError::SectionChangeRejected { operation } => {
                details.insert("reason".to_string(), json!("section_change_rejected"));
                details.insert("sdk_operation".to_string(), json!(operation));
            }
            EditError::Sdk { operation } => {
                details.insert("sdk_operation".to_string(), json!(operation));
            }
            EditError::NotIssued { reason } => {
                details.insert("reason".to_string(), json!(reason.as_str()));
            }
            EditError::Panicked | EditError::MutationPermitReissued => {}
            EditError::AfterMutation {
                source,
                project_revision,
                restore,
            } => {
                source.fill_details(details);
                details.insert("mutation_issued".to_string(), json!(true));
                details.insert(
                    "current_project_revision".to_string(),
                    json!(project_revision),
                );
                // **階級は名乗り分けない。** ホストが値を動かさなかった場合も
                // `restored` は真である——戻す書き込みが要らなかっただけで、
                // 対象は書き込み前の値を持つ。要求元が取る行動（受け付けられる
                // 値を選び直す）は動いた場合と変わらず、分ければ分岐だけが増える。
                //
                // 巻き戻せなかったときの語彙は一括適用と同じものを使う。要求元に
                // 求める行動（次の編集の前に読み直す）が同じである以上、名前を
                // 分ける理由が無い。
                match restore {
                    // 巻き戻しを試みていない。一括適用の sub-operation では、
                    // 結末を要求全体の巻き戻しが `rolled_back` として名乗る。
                    ItemRestore::NotAttempted => {}
                    ItemRestore::Restored => {
                        details.insert("restored".to_string(), json!(true));
                    }
                    ItemRestore::Failed => {
                        details.insert("restored".to_string(), json!(false));
                        details.insert("consistency_unknown".to_string(), json!(true));
                    }
                }
            }
            EditError::Batch {
                source,
                failed_index,
                rollback,
            } => {
                source.fill_details(details);
                // 読み直した対象の現在の姿は、何番目の sub-operation の対象か
                // と対でなければ差し替えに使えない。一括適用では位置を伴う名前
                // で載せる。
                if let Some(object) = details.remove("current_object") {
                    details.insert("failed_object".to_string(), object);
                }
                if let Some(index) = failed_index {
                    details.insert("failed_index".to_string(), json!(index));
                }
                match rollback {
                    RollbackOutcome::NotAttempted => {}
                    RollbackOutcome::Complete { count } => {
                        details.insert("rolled_back".to_string(), json!(true));
                        details.insert("rolled_back_count".to_string(), json!(count));
                    }
                    RollbackOutcome::Incomplete { count } => {
                        details.insert("rolled_back".to_string(), json!(false));
                        details.insert("rolled_back_count".to_string(), json!(count));
                        details.insert("consistency_unknown".to_string(), json!(true));
                    }
                    RollbackOutcome::Impossible => {
                        details.insert("consistency_unknown".to_string(), json!(true));
                    }
                }
            }
        }
    }
}

/// 設定項目への書き込み失敗の補助情報を書き込む。
///
/// 載せるのは項目名・設定項目の種別・値の形・失敗の種別だけである。値そのものは
/// 要求元の内容であり、応答へ反響させない。種別の名前と値の形の名前はどちらも
/// 値を含まないため、パス値・文字列値の検証に落ちた場合もそのまま載せられる。
///
/// **種別と値の形は、名前を持たない失敗の弁別子である。** 値の形が種別と対応
/// しないことと未対応種別の生値は、名前を割り当てる代わりにこの 2 つのキーで
/// 区別する（[`ItemWriteError::reason`]）。載せなければ、要求元に残るのは
/// エラーコードと説明の文面だけになり、機械可読な手掛かりが 1 つも無くなる。
fn fill_item_write_details(details: &mut Map<String, Value>, error: &ItemWriteError) {
    match error {
        ItemWriteError::ItemNotFound { item } => {
            details.insert("item".to_string(), json!(truncate(item)));
        }
        ItemWriteError::ValueKindMismatch {
            item_type,
            value_kind,
        } => {
            details.insert("item_type".to_string(), json!(truncate(item_type)));
            details.insert("value_kind".to_string(), json!(value_kind));
        }
        // 種別を引く前に落ちるため、載せられるのは値の形だけである。
        ItemWriteError::UnknownValue => {
            details.insert("value_kind".to_string(), json!(ItemValue::UNKNOWN_KIND));
        }
        ItemWriteError::UnsupportedItemType { item_type } => {
            details.insert("item_type".to_string(), json!(truncate(item_type)));
        }
        ItemWriteError::Track(error) => fill_track_error_details(details, error),
        ItemWriteError::IntegerNotRepresentable
        | ItemWriteError::Text(_)
        | ItemWriteError::Path(_) => {}
    }
    if let Some(reason) = error.reason() {
        details.insert("reason".to_string(), json!(reason));
    }
}

/// トラックバーの移動の検証失敗が持つ、要求元が直せる形の手掛かりを書き込む。
///
/// **どちらも要求元が指定した値そのものではない。** `expected`/`actual` は値の
/// 個数であって値の並びではなく、`known` は判定に使ったホストの状態であって
/// 要求元が指定した移動方法の名前ではない。[`ItemValue::kind`] が値の形の名前を
/// 載せてよい理由と同じで、値を運ばない形の情報だからである。
fn fill_track_error_details(details: &mut Map<String, Value>, error: &TrackValueError) {
    match error {
        TrackValueError::ValueCount { expected, actual } => {
            details.insert("expected_value_count".to_string(), json!(expected));
            details.insert("actual_value_count".to_string(), json!(actual));
        }
        TrackValueError::UnknownMode { known } => {
            details.insert("known_movements".to_string(), json!(known));
        }
        TrackValueError::ModeNotWritable
        | TrackValueError::ModeReadsAsNumber
        | TrackValueError::MovementWithoutMode
        | TrackValueError::FlagsNotRepresentable
        | TrackValueError::ExpressionMissing => {}
    }
}

/// 別に組み立てた補助情報を取り込む。
fn merge(details: &mut Map<String, Value>, source: Value) {
    if let Value::Object(source) = source {
        details.extend(source);
    }
}

/// 応答へ載せられる長さへ切り詰める。
fn truncate(text: &str) -> String {
    text.chars().take(MAX_NAME_CHARS).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::sample_object_summary;
    use aviutl2_mcp_core::{EffectItemType, Movement, PathSyntaxError, TextSyntaxError};

    /// 検査で使う移動方法の一覧。書けるものと書けないものを混ぜて持つ。
    fn sample_movements() -> Vec<Movement> {
        vec![
            Movement {
                name: "直線移動".to_string(),
                writable: true,
            },
            Movement {
                name: "移動無し".to_string(),
                writable: false,
            },
        ]
    }

    /// 代表値の一覧を掃いた検査が、実際に対象へ当たったことを確かめる。
    ///
    /// 掃きが守る性質（後から足した variant が抜けない）は、掃く対象が一覧に
    /// 在ることの上に乗っている。当たった件数を表明しなければ、代表値の側を
    /// 書き換えて 0 件掃きにしても全て緑のまま通る。**件数そのものは表明しない**
    /// ——代表値を足すたびに動く数であり、固定すると足すほうが直される。
    fn assert_swept(matched: usize, subject: &str) {
        assert!(matched > 0, "{subject}の代表値が一覧にありません");
    }

    /// 全 variant の代表値。新しい variant を足したらここへも足す。
    pub(crate) fn all_errors() -> Vec<EditError> {
        vec![
            EditError::Read(ReadError::NotReady),
            EditError::Read(ReadError::EditBlocked {
                state: EditState::Preview,
            }),
            EditError::Read(ReadError::SceneMismatch {
                expected: 0,
                current: 3,
            }),
            EditError::Read(ReadError::EpochMismatch),
            EditError::Read(ReadError::FingerprintMismatch {
                current_object: Box::new(sample_object_summary()),
            }),
            EditError::Read(ReadError::ObjectNotFound {
                detected_by: "find_object",
            }),
            EditError::Read(ReadError::AmbiguousObject { candidate_count: 2 }),
            EditError::Read(ReadError::Sdk {
                operation: "get_object_alias",
            }),
            EditError::Read(ReadError::Panicked),
            // 編集の拒否。読み取りの拒否と同じく、状態は 2 通り現れる。
            EditError::EditBlocked {
                state: EditState::Preview,
            },
            EditError::EditBlocked {
                state: EditState::Save,
            },
            EditError::DestinationOccupied {
                layer: 3,
                frame: 240,
                occupied_by: OccupiedRange {
                    frame_start: 200,
                    frame_end: 260,
                },
            },
            EditError::EpochMismatch {
                origin: EpochSource::Expected,
            },
            EditError::EpochMismatch {
                origin: EpochSource::Focus,
            },
            EditError::LayerLocked { layer: 3 },
            EditError::EffectFingerprintMismatch,
            EditError::EffectNotFound {
                effect_name: "ぼかし".to_string(),
                effect_index: 1,
            },
            EditError::DuplicateTarget,
            EditError::ItemWrite(ItemWriteError::ItemNotFound {
                item: "範囲".to_string(),
            }),
            EditError::ItemWrite(ItemWriteError::UnsupportedItemType {
                item_type: EffectItemType::Data.kind_name(),
            }),
            EditError::ItemWrite(ItemWriteError::UnknownValue),
            EditError::ItemWrite(ItemWriteError::ValueKindMismatch {
                item_type: EffectItemType::Integer.kind_name(),
                value_kind: "text",
            }),
            EditError::ItemWrite(ItemWriteError::Text(TextSyntaxError::ContainsNul)),
            EditError::ItemWrite(ItemWriteError::Path(PathSyntaxError::NotAbsolute)),
            // 移動の値の個数が対象の区間数と合わない。個数は値ではなく形の
            // 情報であるため、期待値と実際の値をそのまま運ぶ。
            EditError::ItemWrite(ItemWriteError::Track(TrackValueError::ValueCount {
                expected: 3,
                actual: 2,
            })),
            // 移動方法の名前がホストの一覧に無い。判定に使った一覧をそのまま
            // 運ぶ。要求元が指定した名前ではない。**書けない移動方法を含めた
            // 一覧が運ばれる。**
            EditError::ItemWrite(ItemWriteError::Track(TrackValueError::UnknownMode {
                known: sample_movements(),
            })),
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::EffectNotRegistered,
            },
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::EffectNotCreatable,
            },
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::EffectStateImmutable,
            },
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::EffectNotMovable,
            },
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::MediaNotSupported,
            },
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::ItemTypeNotWritable,
            },
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::ChangeNotApplied,
            },
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::InverseUnavailable,
            },
            // 読み直したホストの値を運ぶ。**パスを運ぶ標本も置く。** 種別に
            // よってはホストが保持する値が利用者のファイルパスであり、色だけを
            // 置くと補助情報の検査がその形を素通りする。
            EditError::ItemValueNotApplied {
                observed: "ffffff".to_string(),
            },
            EditError::ItemValueNotApplied {
                observed: r"C:\assets\bgm.wav".to_string(),
            },
            // 対象がいま持つ移動を運ぶ。
            EditError::MovementWouldBeLost {
                current_value: "-500.00,500.00,直線移動,5".to_string(),
            },
            // 受け入れ規則の 4 条件は、名前を持つものと持たないものの双方を通す。
            EditError::AliasRejected(AliasRejection::NotFound),
            EditError::AliasRejected(AliasRejection::WithoutEffect),
            // 生テキストの行。**一覧を運ぶ検証と運ばない検証の双方を置く**
            // ——片方だけでは、補助情報のキーの検査が一覧の側を素通りする。
            EditError::AliasRowRejected {
                heading: Some("Object.1".to_string()),
                item: "X".to_string(),
                source: TrackValueError::FlagsNotRepresentable.into(),
            },
            EditError::AliasRowRejected {
                heading: Some("Object.1".to_string()),
                item: "中心Z".to_string(),
                source: TrackValueError::UnknownMode {
                    known: sample_movements(),
                }
                .into(),
            },
            // テキスト種別の行。移動行と同じ運びで応答へ載る。
            EditError::AliasRowRejected {
                heading: Some("Object.0".to_string()),
                item: "テキスト".to_string(),
                source: TextSyntaxError::UnescapedBackslash.into(),
            },
            EditError::NotIssued {
                reason: NotIssuedReason::TargetMissing,
            },
            EditError::NotIssued {
                reason: NotIssuedReason::ArgumentNotRepresentable,
            },
            EditError::SectionPrecondition {
                reason: SectionPreconditionReason::FrameOutsideObject,
            },
            EditError::EffectPrecondition {
                reason: EffectPreconditionReason::PositionOutOfRange,
            },
            EditError::EffectMoveNotApplied {
                reported_position: 2,
            },
            EditError::SectionChangeRejected {
                operation: "create_object_section",
            },
            EditError::Sdk {
                operation: "create_effect",
            },
            EditError::Panicked,
            EditError::MutationPermitReissued,
            EditError::Sdk {
                operation: "create_effect",
            }
            .after_mutation(44),
            // 巻き戻しの結末は 3 通りあり、補助情報のキーがそれぞれ違う。
            // 1 つだけ置くと、残りが載せるキーを検査が素通りする。
            EditError::ItemValueNotApplied {
                observed: "ffffff".to_string(),
            }
            .after_mutation(44)
            .with_item_restore(ItemRestore::Restored),
            EditError::ItemValueNotApplied {
                observed: "ffffff".to_string(),
            }
            .after_mutation(44)
            .with_item_restore(ItemRestore::Failed),
            // 事前解決相で落ちた一括適用。変更は 1 つも発行されていない。
            EditError::Batch {
                source: Box::new(EditError::Read(ReadError::FingerprintMismatch {
                    current_object: Box::new(sample_object_summary()),
                })),
                failed_index: Some(2),
                rollback: RollbackOutcome::NotAttempted,
            },
            // 適用相で落ち、発行済みの変更を全て戻した一括適用。
            EditError::Batch {
                source: Box::new(
                    EditError::DestinationOccupied {
                        layer: 3,
                        frame: 240,
                        occupied_by: OccupiedRange {
                            frame_start: 200,
                            frame_end: 260,
                        },
                    }
                    .after_mutation(44),
                ),
                failed_index: Some(1),
                rollback: RollbackOutcome::Complete { count: 1 },
            },
            // 巻き戻しに失敗した一括適用。
            EditError::Batch {
                source: Box::new(
                    EditError::Sdk {
                        operation: "move_object",
                    }
                    .after_mutation(44),
                ),
                failed_index: Some(1),
                rollback: RollbackOutcome::Incomplete { count: 0 },
            },
            // 区間の panic。どこまで適用したかも分からない。
            EditError::Batch {
                source: Box::new(EditError::Panicked.after_mutation(44)),
                failed_index: None,
                rollback: RollbackOutcome::Impossible,
            },
            // 適用相の 1 件目が発行の前に落ちた一括適用。巻き戻しの対象が 0 件で
            // あるため結末は「全て戻した」になり、内側は発行の包みを持たない。
            // **剥がすものが無い側の代表値である**——包みを持つ標本だけでは、
            // 剥がしを素通りする経路を掃きが 1 つも通らない。
            EditError::Batch {
                source: Box::new(EditError::DestinationOccupied {
                    layer: 1,
                    frame: 100,
                    occupied_by: OccupiedRange {
                        frame_start: 100,
                        frame_end: 200,
                    },
                }),
                failed_index: Some(0),
                rollback: RollbackOutcome::Complete { count: 0 },
            },
        ]
    }

    /// variant を表す名前を返す。
    ///
    /// 網羅 match で書く。variant を足すとここがコンパイルエラーになり、すぐ下の
    /// 一覧と [`all_errors`] へ足す必要があることが分かる。名前だけを返すのは、
    /// 代表値の作り方が variant ごとに違うためである。
    fn variant_name(error: &EditError) -> &'static str {
        match error {
            EditError::Read(_) => "Read",
            EditError::EditBlocked { .. } => "EditBlocked",
            EditError::DestinationOccupied { .. } => "DestinationOccupied",
            EditError::EpochMismatch { .. } => "EpochMismatch",
            EditError::LayerLocked { .. } => "LayerLocked",
            EditError::EffectFingerprintMismatch => "EffectFingerprintMismatch",
            EditError::EffectNotFound { .. } => "EffectNotFound",
            EditError::DuplicateTarget => "DuplicateTarget",
            EditError::ItemWrite(_) => "ItemWrite",
            EditError::ItemValueNotApplied { .. } => "ItemValueNotApplied",
            EditError::MovementWouldBeLost { .. } => "MovementWouldBeLost",
            EditError::UnsupportedTarget { .. } => "UnsupportedTarget",
            EditError::AliasRejected(_) => "AliasRejected",
            EditError::AliasRowRejected { .. } => "AliasRowRejected",
            EditError::SectionPrecondition { .. } => "SectionPrecondition",
            EditError::EffectPrecondition { .. } => "EffectPrecondition",
            EditError::EffectMoveNotApplied { .. } => "EffectMoveNotApplied",
            EditError::SectionChangeRejected { .. } => "SectionChangeRejected",
            EditError::Sdk { .. } => "Sdk",
            EditError::NotIssued { .. } => "NotIssued",
            EditError::Panicked => "Panicked",
            EditError::MutationPermitReissued => "MutationPermitReissued",
            EditError::AfterMutation { .. } => "AfterMutation",
            EditError::Batch { .. } => "Batch",
        }
    }

    #[test]
    fn all_errors_covers_every_variant() {
        // 代表値の一覧は手書きであり、足し忘れても他のテストは緑のまま通る。
        // 落ちたものは応答コードも再試行の案内も許可キーも守られない。
        const VARIANTS: &[&str] = &[
            "Read",
            "EditBlocked",
            "DestinationOccupied",
            "EpochMismatch",
            "LayerLocked",
            "EffectFingerprintMismatch",
            "EffectNotFound",
            "DuplicateTarget",
            "ItemWrite",
            "ItemValueNotApplied",
            "MovementWouldBeLost",
            "UnsupportedTarget",
            "AliasRejected",
            "AliasRowRejected",
            "SectionPrecondition",
            "EffectPrecondition",
            "EffectMoveNotApplied",
            "SectionChangeRejected",
            "Sdk",
            "NotIssued",
            "Panicked",
            "MutationPermitReissued",
            "AfterMutation",
            "Batch",
        ];
        let covered: Vec<&str> = all_errors().iter().map(variant_name).collect();
        for variant in VARIANTS {
            assert!(
                covered.contains(variant),
                "{variant} の代表値が一覧にありません"
            );
        }
        // 逆向きも見る。variant を消したときに VARIANTS だけが取り残されると、
        // 一覧は無くなった variant を数え続け、残りの網羅を主張できなくなる。
        for variant in &covered {
            assert!(
                VARIANTS.contains(variant),
                "{variant} が網羅すべき variant の一覧にありません"
            );
        }
    }

    #[test]
    fn all_errors_covers_every_reason() {
        // 理由ごとに応答の補助情報が変わるため、variant を 1 つ挙げるだけでは
        // 足りない。網羅 match を添えて、理由を足したときに気付ける形にする。
        let unsupported = [
            UnsupportedReason::EffectNotRegistered,
            UnsupportedReason::EffectNotCreatable,
            UnsupportedReason::EffectStateImmutable,
            UnsupportedReason::EffectNotMovable,
            UnsupportedReason::MediaNotSupported,
            UnsupportedReason::ItemTypeNotWritable,
            UnsupportedReason::ChangeNotApplied,
            UnsupportedReason::InverseUnavailable,
        ];
        for reason in unsupported {
            match reason {
                UnsupportedReason::EffectNotRegistered
                | UnsupportedReason::EffectNotCreatable
                | UnsupportedReason::EffectStateImmutable
                | UnsupportedReason::EffectNotMovable
                | UnsupportedReason::MediaNotSupported
                | UnsupportedReason::ItemTypeNotWritable
                | UnsupportedReason::ChangeNotApplied
                | UnsupportedReason::InverseUnavailable => {}
            }
        }
        let not_issued = [
            NotIssuedReason::TargetMissing,
            NotIssuedReason::ArgumentNotRepresentable,
        ];
        for reason in not_issued {
            match reason {
                NotIssuedReason::TargetMissing | NotIssuedReason::ArgumentNotRepresentable => {}
            }
        }

        let reasons: Vec<String> = all_errors()
            .iter()
            .filter_map(|error| {
                error
                    .details()
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        for expected in unsupported
            .iter()
            .map(|reason| reason.as_str())
            .chain(not_issued.iter().map(|reason| reason.as_str()))
        {
            assert!(
                reasons.iter().any(|reason| reason == expected),
                "{expected} の代表値が一覧にありません"
            );
        }
    }

    #[test]
    fn error_codes_match_the_edit_mapping() {
        let mapped: Vec<ErrorCode> = all_errors().iter().map(EditError::error_code).collect();
        assert_eq!(
            mapped,
            vec![
                ErrorCode::HostBusy,
                ErrorCode::EditBlocked,
                ErrorCode::PreconditionFailed,
                ErrorCode::PreconditionFailed,
                ErrorCode::PreconditionFailed,
                ErrorCode::NotFound,
                ErrorCode::AmbiguousSelector,
                ErrorCode::SdkError,
                ErrorCode::InternalError,
                // 編集の拒否。読み取りの拒否と同じコードで返る。
                ErrorCode::EditBlocked,
                ErrorCode::EditBlocked,
                ErrorCode::PreconditionFailed,
                // 前提の epoch と focus の epoch を別々に名乗る 2 つ。
                ErrorCode::PreconditionFailed,
                ErrorCode::PreconditionFailed,
                ErrorCode::PreconditionFailed,
                // 所属オブジェクトは一致したが effect が変わっていた。
                ErrorCode::PreconditionFailed,
                ErrorCode::NotFound,
                // 解決した結果、同じ状態を書き換える組が現れた。要求の誤りで
                // あり、対象を読み直しても解消しない。
                ErrorCode::InvalidArgument,
                ErrorCode::NotFound,
                ErrorCode::UnsupportedOperation,
                ErrorCode::InvalidArgument,
                ErrorCode::InvalidArgument,
                ErrorCode::InvalidArgument,
                ErrorCode::InvalidArgument,
                // 移動の値の個数と、移動方法の名前が一覧に無いこと。どちらも
                // 要求内容の誤りであり、対象を読み直しても解消しない。
                ErrorCode::InvalidArgument,
                ErrorCode::InvalidArgument,
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                // 順序を動かせない effect である。
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                // 逆操作の材料を読めなかった。
                ErrorCode::UnsupportedOperation,
                // 書き込んだ値が要求どおりに入らなかった。読み直しても有効に
                // ならない。色を運ぶ標本とパスを運ぶ標本の 2 つ。
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                // 書き込むと対象の移動が消える。値の形を変えるほかない。
                ErrorCode::UnsupportedOperation,
                // 名前で指定されたエイリアスが受け入れ規則を通らなかった。落ちた
                // 条件がコードを決めるため、不在と構造の欠陥で別の値になる。
                ErrorCode::NotFound,
                ErrorCode::InvalidArgument,
                // 生テキストの行が検証を通らなかった。要求内容の誤りであり、
                // 対象を読み直しても解消しない。移動行 2 つとテキスト行 1 つ。
                ErrorCode::InvalidArgument,
                ErrorCode::InvalidArgument,
                ErrorCode::InvalidArgument,
                // 対象が失われていた。要求の対象が無いのだから見つからない。
                ErrorCode::NotFound,
                // 引数を写せなかった。SDK の失敗ではなく要求の誤りである。
                ErrorCode::InvalidArgument,
                // 読み直した区間の実態と食い違った。復帰の手段は読み直しである。
                ErrorCode::PreconditionFailed,
                // 移動先が effect の列の長さを超えていた。同じく読み直しである。
                ErrorCode::PreconditionFailed,
                // 発行した移動が列に現れなかった。
                ErrorCode::UnsupportedOperation,
                // 事前確認を通したのに SDK が拒んだ。要求元に直せることが無い。
                ErrorCode::SdkError,
                ErrorCode::SdkError,
                ErrorCode::InternalError,
                // 前提の作り方の誤りであり、要求を作り直しても解消しない。
                ErrorCode::InternalError,
                ErrorCode::SdkError,
                // 巻き戻しの結末はコードを変えない。戻せたかは補助情報が名乗る。
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                // 一括適用は失敗の理由をそのまま保つ。
                ErrorCode::PreconditionFailed,
                ErrorCode::PreconditionFailed,
                // 戻せなかった変更が残っている場合だけ書き換える。
                ErrorCode::SdkError,
                ErrorCode::InternalError,
                // 発行の前に落ちた 1 件目。理由をそのまま保つ。
                ErrorCode::PreconditionFailed,
            ]
        );
    }

    #[test]
    fn a_rejected_alias_keeps_the_code_and_the_reason_the_admission_rule_gave() {
        // 一覧の除外と作成の拒否は同じ戻り値を見る。ここで写し直すと、片方だけに
        // 条件を足せる形になり「一覧に載る ⇒ 作成できる」が構造で保てなくなる。
        for rejection in crate::alias::tests::all_rejections() {
            let error = EditError::AliasRejected(rejection);
            assert_eq!(error.error_code(), rejection.error_code(), "{rejection}");
            assert_eq!(
                error.details().get("reason").and_then(Value::as_str),
                rejection.reason(),
                "{rejection}"
            );
        }
    }

    #[test]
    fn cancelled_is_never_produced() {
        for error in all_errors() {
            assert_ne!(
                error.error_code(),
                ErrorCode::Cancelled,
                "{error} が cancelled になりました"
            );
        }
    }

    #[test]
    fn precondition_failures_ask_for_a_refetch_unless_a_reread_cannot_help() {
        for error in all_errors() {
            if error.error_code() != ErrorCode::PreconditionFailed {
                continue;
            }
            assert!(error.retryable(), "{error} が再試行不可になりました");
            let details = error.details();
            // ロックの解除は別の operation である。読み直しを案内すると、
            // 要求元は解消しない読み直しを繰り返す。
            let expected = if details["reason"] == json!("layer_locked") {
                "none"
            } else {
                "refetch"
            };
            assert_eq!(
                details["retry_requires"],
                json!(expected),
                "{error} が案内する再試行のしかたが想定と異なります"
            );
        }
    }

    #[test]
    fn an_occupied_destination_reports_how_far_it_is_blocked() {
        // 塞いでいる範囲を返さないと、要求元は次の宛先を選ぶために読み直す
        // ほかない。走査は事前確認で済んでおり、追加の呼び出しは要らない。
        let error = EditError::DestinationOccupied {
            layer: 3,
            frame: 240,
            occupied_by: OccupiedRange {
                frame_start: 200,
                frame_end: 260,
            },
        };
        let details = error.details();
        assert_eq!(
            details["occupied_by"],
            json!({"frame_start": 200, "frame_end": 260})
        );
        // 塞いでいる対象の同一性は渡さない。要求元に要るのは範囲だけである。
        let mut keys: Vec<&str> = details["occupied_by"]
            .as_object()
            .expect("occupied_by がオブジェクトではありません")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["frame_end", "frame_start"]);
    }

    #[test]
    fn an_occupied_destination_still_asks_for_a_refetch() {
        // 宛先の空きは読み直せば分かる。ロックと同じ扱いにはしない。
        let error = EditError::DestinationOccupied {
            layer: 3,
            frame: 240,
            occupied_by: OccupiedRange {
                frame_start: 200,
                frame_end: 260,
            },
        };
        assert_eq!(error.details()["retry_requires"], json!("refetch"));
    }

    #[test]
    fn transient_states_allow_a_plain_resend() {
        assert_eq!(
            EditError::Read(ReadError::NotReady).details()["retry_requires"],
            json!("resend")
        );
        assert_eq!(
            EditError::Read(ReadError::EditBlocked {
                state: EditState::Save
            })
            .details()["retry_requires"],
            json!("resend")
        );
        assert_eq!(
            EditError::EditBlocked {
                state: EditState::Save
            }
            .details()["retry_requires"],
            json!("resend")
        );
    }

    #[test]
    fn a_blocked_edit_says_it_cannot_edit_while_a_blocked_read_says_it_cannot_read() {
        // どちらの経路で落ちたかが文言だけで読める。同じ状態を指す 2 つの失敗が
        // 同じ文を名乗ると、要求元も運用者も切り分けられない。
        let edit = EditError::EditBlocked {
            state: EditState::Preview,
        };
        let read = EditError::Read(ReadError::EditBlocked {
            state: EditState::Preview,
        });
        assert_eq!(edit.to_string(), "プレビュー再生中のため編集できません");
        assert_eq!(read.to_string(), "プレビュー再生中のため読み取りできません");
        // 文言以外は 1 つも変わらない。要求元へ求める行動が同じであるため、
        // コードも案内も同じものを名乗る。
        assert_eq!(edit.error_code(), read.error_code());
        assert_eq!(edit.details(), read.details());
        assert_eq!(edit.details()["edit_state"], json!("preview"));
        assert_eq!(edit.details()["retry_after_ms"], json!(2_000));
    }

    #[test]
    fn precondition_failures_name_the_failing_check() {
        let expected = [
            (EditError::Read(ReadError::EpochMismatch), "project_epoch"),
            (
                EditError::Read(ReadError::SceneMismatch {
                    expected: 0,
                    current: 1,
                }),
                "scene_id",
            ),
            (
                EditError::Read(ReadError::FingerprintMismatch {
                    current_object: Box::new(sample_object_summary()),
                }),
                "fingerprint",
            ),
            // 対象と effect のどちらが食い違っても名乗る値は同じである。要求元が
            // 分岐に使うのは食い違いの種類であり、どの層で落ちたかではない。
            (EditError::EffectFingerprintMismatch, "fingerprint"),
        ];
        for (error, mismatch) in expected {
            assert_eq!(error.details()["mismatch"], json!(mismatch), "{error}");
        }
    }

    #[test]
    fn no_failure_names_the_revision_as_the_failing_check() {
        // revision は照合しない。食い違いとして名乗る失敗が現れれば、照合が
        // 戻ったか、要求元へ訂正できない再送を促していることになる。
        for error in all_errors() {
            assert_ne!(
                error.details().get("mismatch"),
                Some(&json!("project_revision")),
                "{error} が revision の食い違いを名乗りました"
            );
        }
    }

    #[test]
    fn failures_outside_preconditions_do_not_name_a_mismatch() {
        for error in all_errors() {
            if error.error_code() == ErrorCode::PreconditionFailed {
                continue;
            }
            assert!(
                error.details().get("mismatch").is_none(),
                "{error} が前提条件の食い違いを名乗りました"
            );
        }
    }

    #[test]
    fn a_content_mismatch_reaches_the_edit_response_with_the_current_object() {
        // 読み取り経路が組み立てた補助情報は編集経路が併合する。写し替えの層を
        // 挟まないため、現在の姿はそのまま要求元へ届く。
        let summary = sample_object_summary();
        let error = EditError::Read(ReadError::FingerprintMismatch {
            current_object: Box::new(summary.clone()),
        });
        let details = error.details();
        assert_eq!(details["mismatch"], json!("fingerprint"));
        assert_eq!(details["retry_requires"], json!("refetch"));
        assert_eq!(
            details["current_object"],
            serde_json::to_value(&summary).unwrap()
        );
    }

    #[test]
    fn only_an_object_content_mismatch_carries_the_current_object() {
        // effect の食い違いは含まれない。所属オブジェクトの照合はその手前で
        // 通っており、概要を添えても要求元が送ってきた値と同じものになる。
        //
        // 一括適用は同じ値を位置つきの名前で載せる。どちらの名前で載っている
        // かは問わず、載っているかどうかだけを見る。
        for error in all_errors() {
            let details = error.details();
            let carried =
                details.get("current_object").is_some() || details.get("failed_object").is_some();
            assert_eq!(carried, resolves_to_an_object_mismatch(&error), "{error}");
        }
    }

    #[test]
    fn a_batch_names_the_failing_object_together_with_its_position() {
        // 一括適用の応答は「何番目が落ちたか」と「その対象がいまどうなって
        // いるか」を対で運ぶ。位置を伴わない名前で載せると、要求元は多数の
        // sub-operation のどれを差し替えればよいか分からない。
        let summary = sample_object_summary();
        let error = EditError::Batch {
            source: Box::new(EditError::Read(ReadError::FingerprintMismatch {
                current_object: Box::new(summary.clone()),
            })),
            failed_index: Some(2),
            rollback: RollbackOutcome::NotAttempted,
        };
        let details = error.details();
        assert_eq!(details["failed_index"], json!(2));
        assert_eq!(
            details["failed_object"],
            serde_json::to_value(&summary).unwrap()
        );
        assert!(details.get("current_object").is_none());
        assert_eq!(details["mismatch"], json!("fingerprint"));
        assert_eq!(details["retry_requires"], json!("refetch"));
    }

    #[test]
    fn an_effect_mismatch_in_a_batch_names_only_the_position() {
        // effect の食い違いで返せる対象の姿は、要求元が既に持っている値と
        // 同じになる。位置だけを名乗り、差し替えの材料にならない値は載せない。
        let error = EditError::Batch {
            source: Box::new(EditError::EffectFingerprintMismatch),
            failed_index: Some(1),
            rollback: RollbackOutcome::NotAttempted,
        };
        let details = error.details();
        assert_eq!(details["failed_index"], json!(1));
        assert!(details.get("failed_object").is_none());
        assert_eq!(details["mismatch"], json!("fingerprint"));
    }

    #[test]
    fn a_rolled_back_batch_keeps_the_code_of_the_failure_that_stopped_it() {
        // 要求元が知るべきは「なぜ一括適用が失敗したか」である。巻き戻せた
        // かどうかは別のキーが担う。
        let error = EditError::Batch {
            source: Box::new(
                EditError::DestinationOccupied {
                    layer: 3,
                    frame: 240,
                    occupied_by: OccupiedRange {
                        frame_start: 200,
                        frame_end: 260,
                    },
                }
                .after_mutation(44),
            ),
            failed_index: Some(1),
            rollback: RollbackOutcome::Complete { count: 1 },
        };
        assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
        let details = error.details();
        assert_eq!(details["rolled_back"], json!(true));
        assert_eq!(details["rolled_back_count"], json!(1));
        assert_eq!(details["mutation_issued"], json!(true));
        assert_eq!(details["retry_requires"], json!("refetch"));
        // 元へ戻ったのだから、中途半端な状態は残っていない。
        assert!(details.get("consistency_unknown").is_none());
    }

    #[test]
    fn a_rolled_back_batch_lets_the_failure_that_stopped_it_choose_the_retry() {
        // 全て戻ったのだから、プロジェクトは要求の前と同じである。要求元が次に
        // 取る行動は、変更が 1 つも入らなかった場合と変わらない——受け付けられる
        // 値を選び直すのであって、対象を読み直すのではない。
        let error = EditError::Batch {
            source: Box::new(
                EditError::ItemValueNotApplied {
                    observed: "ffffff".to_string(),
                }
                .after_mutation(44),
            ),
            failed_index: Some(1),
            rollback: RollbackOutcome::Complete { count: 2 },
        };
        let details = error.details();
        assert_eq!(details["rolled_back"], json!(true));
        assert_eq!(details["reason"], json!("item_value_not_applied"));
        assert_eq!(details["retry_requires"], json!("none"));
    }

    #[test]
    fn no_failure_after_a_change_allows_a_plain_resend() {
        // 剥がす形は内側の失敗に案内を決めさせる。後から足した失敗が再送を
        // 持ち込む余地はここにだけ残る。
        let mut matched = 0;
        for error in all_errors() {
            let details = error.details();
            if details.get("mutation_issued").is_none() && details.get("rolled_back").is_none() {
                continue;
            }
            matched += 1;
            assert_ne!(
                details["retry_requires"],
                json!("resend"),
                "{error} が変更の後にそのままの再送を促しました"
            );
        }
        assert_swept(matched, "変更の後に落ちた失敗");
    }

    #[test]
    fn a_batch_that_could_not_be_rolled_back_reports_an_unknown_state() {
        // 元の失敗の理由が何であれ、要求元が直面している問題は「プロジェクトが
        // 中途半端な状態にある」ことへ変わっている。
        let error = EditError::Batch {
            source: Box::new(
                EditError::DestinationOccupied {
                    layer: 3,
                    frame: 240,
                    occupied_by: OccupiedRange {
                        frame_start: 200,
                        frame_end: 260,
                    },
                }
                .after_mutation(44),
            ),
            failed_index: Some(2),
            rollback: RollbackOutcome::Incomplete { count: 1 },
        };
        assert_eq!(error.error_code(), ErrorCode::SdkError);
        assert!(!error.retryable());
        let details = error.details();
        assert_eq!(details["consistency_unknown"], json!(true));
        assert_eq!(details["rolled_back"], json!(false));
        assert_eq!(details["rolled_back_count"], json!(1));
        assert_eq!(details["retry_requires"], json!("refetch"));
    }

    /// 変更の発行後に包み直された失敗も辿って、対象の食い違いかを判定する。
    ///
    /// 外側の variant だけを見ると、包み直した代表値を足したときに正しい挙動が
    /// 落ちる。
    fn resolves_to_an_object_mismatch(error: &EditError) -> bool {
        match error {
            EditError::Read(ReadError::FingerprintMismatch { .. }) => true,
            EditError::AfterMutation { source, .. } | EditError::Batch { source, .. } => {
                resolves_to_an_object_mismatch(source)
            }
            _ => false,
        }
    }

    #[test]
    fn failures_after_a_mutation_keep_the_original_code() {
        let error = EditError::Sdk {
            operation: "get_effect_list",
        }
        .after_mutation(44);
        assert_eq!(error.error_code(), ErrorCode::SdkError);
        let details = error.details();
        assert_eq!(details["mutation_issued"], json!(true));
        assert_eq!(details["current_project_revision"], json!(44));
        assert_eq!(details["retry_requires"], json!("refetch"));
        assert_eq!(details["sdk_operation"], json!("get_effect_list"));
    }

    #[test]
    fn a_failure_whose_target_was_restored_needs_no_refetch() {
        // 対象は書き込み前の値を持つ。読み直した先には要求の前と同じ値が在り、
        // 要求元が次に取る行動（受け付けられる値を選び直す）はそこからは得られ
        // ない。
        let error = EditError::ItemValueNotApplied {
            observed: "ffffff".to_string(),
        }
        .after_mutation(44)
        .with_item_restore(ItemRestore::Restored);
        let details = error.details();
        assert_eq!(details["restored"], json!(true));
        assert_eq!(details["retry_requires"], json!("none"));
    }

    #[test]
    fn a_failure_whose_target_could_not_be_restored_asks_for_a_refetch() {
        // 戻せていない以上、要求元は読み直す必要が現にある。復旧の結果を分ける
        // 値がここで意味を持つ。
        let error = EditError::ItemValueNotApplied {
            observed: "ffffff".to_string(),
        }
        .after_mutation(44)
        .with_item_restore(ItemRestore::Failed);
        let details = error.details();
        assert_eq!(details["restored"], json!(false));
        assert_eq!(details["retry_requires"], json!("refetch"));
    }

    #[test]
    fn no_restored_failure_asks_for_a_refetch() {
        // 個別の検査だけでは、後から足した variant が抜ける。
        let mut matched = 0;
        for error in all_errors() {
            let details = error.details();
            if details.get("restored") != Some(&json!(true)) {
                continue;
            }
            matched += 1;
            assert_ne!(
                details["retry_requires"],
                json!("refetch"),
                "{error} が戻っている対象の読み直しを促しました"
            );
        }
        assert_swept(matched, "戻せた対象を持つ失敗");
    }

    #[test]
    fn every_failure_that_leaves_the_state_unknown_asks_for_a_refetch() {
        // 中途半端な状態が残っていれば、次の編集の前に読み直すほかない。
        let mut matched = 0;
        for error in all_errors() {
            let details = error.details();
            if details.get("consistency_unknown").is_none() {
                continue;
            }
            matched += 1;
            assert_eq!(
                details["retry_requires"],
                json!("refetch"),
                "{error} が読み直さずに済むと名乗りました"
            );
        }
        assert_swept(matched, "中途半端な状態を名乗る失敗");
    }

    #[test]
    fn wrapping_twice_keeps_the_revision_of_the_first_issue() {
        let error = EditError::Panicked.after_mutation(44).after_mutation(99);
        assert_eq!(error.details()["current_project_revision"], json!(44));
    }

    #[test]
    fn details_only_use_allowed_keys() {
        // 補助情報のキーはここで列挙したものに限る。入れ子の内側まで見る。
        // トップレベルだけを見ると、値をオブジェクトで包んだ瞬間に検査が
        // 素通りする。
        //
        // 新しいキーを足す際は、ハンドル・生ポインタでないこと、そして
        // **要求元が与えた内容をそのまま返すものでないこと**を確かめる。
        // 反響させないのは要求元の内容であって、ホストの状態ではない——
        // ホストから読み直した設定値は、同じ値が成功応答にも載るものであり、
        // 失敗の応答でだけ伏せる理由が無い。要求元が書いた設定値・alias・
        // パスをそのまま返すキーは足さない。
        //
        // 個数はそのどちらでもない**値の形の情報**である。`actual_value_count`
        // は要求に現れた配列の長さであって要素の値ではなく、
        // `expected_value_count` はホストの状態（対象の区間数）から決まる数で
        // ある。[`ItemValue::kind`] が種別名を載せてよい理由（値そのものを
        // 含まない）と同じ理由で、どちらも足してよい。
        //
        // **例外が 1 つある。** 生テキストのエイリアスで拒否された行を指す
        // `heading` と `item` は、要求元が送った本文の部分文字列である。1 つの
        // エイリアスは複数のオブジェクトと複数の effect を持ち得るため、行を
        // 特定できなければ直せない。**運ぶのは名前だけであり、行が持っていた値は
        // 運ばない。**
        const ALLOWED: &[&str] = &[
            "frame_start",
            "frame_end",
            "retry_after_ms",
            "edit_state",
            "expected_scene_id",
            "current_scene_id",
            "current_project_revision",
            "mismatch",
            "candidate_count",
            "reason",
            "layer",
            "frame",
            "occupied_by",
            "effect_name",
            "effect_index",
            "item",
            // 設定項目の種別と、書き込もうとした値の形。どちらも名前だけであり、
            // 値も表記も含まない。名前を持たない失敗の弁別子である。
            "item_type",
            "value_kind",
            // トラックバーの移動の検証が返す、値の形の情報。要求に現れた配列の
            // 長さと、対象の区間数から決まる期待値、判定に使ったホストの
            // 移動方法の一覧。いずれも要求元が指定した値そのものではない。
            "expected_value_count",
            "actual_value_count",
            "known_movements",
            // 一覧の要素が名乗る、その名前で移動を書けるか。ホストの状態から
            // 決まる真偽値であり、設定値そのものではない。
            "writable",
            // 生テキストのエイリアスで、拒否された行が属する節の見出し。要求元が
            // どの行を直すかを決めるのに要る名前であり、行が持っていた値ではない。
            "heading",
            "sdk_operation",
            "retry_requires",
            "mutation_issued",
            "change_applied",
            "mutation_origin",
            // 一括適用。件数と真偽値だけであり、設定値も元値も漏らさない。
            "failed_index",
            "rolled_back",
            "rolled_back_count",
            "consistency_unknown",
            // 落ちた sub-operation の対象の現在の概要。単独編集の
            // `current_object` と同じ値であり、alias もパスも持たない。
            "failed_object",
            // 読み直した対象の概要と、それが内包するセレクター。概要は要約で
            // あり alias も設定値もパスも持たない。
            "current_object",
            // 対象がいま持つ移動。書き込みを発行する前に落ちるため、文字どおり
            // 現在値である。要求元が与えた値ではない。
            "current_value",
            // 照合で読んだ設定値と、書き込み前の値へ戻せたか。
            // 前者は要求元が与えた値ではなく、成功した書き込みが返すものと同じ
            // である。後者は真偽値だけであり、値も元値も漏らさない。
            "observed_value",
            "restored",
            // ホストが名乗った移動後のインデックス。要求元が与えた値ではなく、
            // 可否の判定にも使っていない測定である。
            "reported_position",
            "name",
            "selector",
            "fingerprint",
            "project_epoch",
            "scene_id",
        ];
        /// 入れ子を含む全てのキーを集める。
        fn keys(value: &Value, into: &mut Vec<String>) {
            match value {
                Value::Object(object) => {
                    for (key, value) in object {
                        into.push(key.clone());
                        keys(value, into);
                    }
                }
                Value::Array(items) => items.iter().for_each(|item| keys(item, into)),
                _ => {}
            }
        }

        for error in all_errors() {
            let details = error.details();
            assert!(
                details.is_object(),
                "{error} の補助情報がオブジェクトではありません"
            );
            let mut found = Vec::new();
            keys(&details, &mut found);
            for key in found {
                assert!(
                    ALLOWED.contains(&key.as_str()),
                    "{error} の補助情報に未許可のキー {key} が含まれています"
                );
            }
        }
    }

    #[test]
    fn item_write_failures_without_a_reason_carry_a_machine_readable_discriminator() {
        // 名前を持たない失敗は、種別と値の形で区別する。**どちらも載せなければ、
        // 要求元に残るのはエラーコードと説明の文面だけになる。**
        let mismatch = EditError::ItemWrite(ItemWriteError::ValueKindMismatch {
            item_type: EffectItemType::Check.kind_name(),
            value_kind: "track",
        });
        let details = mismatch.details();
        assert!(
            details.get("reason").is_none(),
            "名前を持つ失敗になりました"
        );
        assert_eq!(details["item_type"], json!("check"));
        assert_eq!(details["value_kind"], json!("track"));

        let unknown = EditError::ItemWrite(ItemWriteError::UnknownValue);
        let details = unknown.details();
        assert!(
            details.get("reason").is_none(),
            "名前を持つ失敗になりました"
        );
        // 種別を引く前に落ちるため、値の形だけが載る。
        assert_eq!(details["value_kind"], json!("unknown"));
        assert!(details.get("item_type").is_none());

        // 名前を持たない失敗が、応答の上で互いに区別できる。
        assert_ne!(mismatch.details(), unknown.details());
    }

    #[test]
    fn a_type_that_is_not_writable_names_the_type() {
        // 名前だけでは、どの種別が書けないのかが分からない。種別の名前は値も
        // 表記も含まないため、そのまま載せられる。
        let error = EditError::ItemWrite(ItemWriteError::UnsupportedItemType {
            item_type: EffectItemType::Data.kind_name(),
        });
        let details = error.details();
        assert_eq!(details["reason"], json!("item_type_not_writable"));
        assert_eq!(details["item_type"], json!("data"));
    }

    #[test]
    fn a_track_value_count_failure_names_the_expected_and_actual_counts() {
        // reason だけでは、要求元は何個にすれば通るかを知れない。区間数から
        // 決まる期待値と、要求に現れた実際の個数の両方を運ぶ。
        let error = EditError::ItemWrite(ItemWriteError::Track(TrackValueError::ValueCount {
            expected: 4,
            actual: 2,
        }));
        let details = error.details();
        assert_eq!(details["reason"], json!("track_value_count"));
        assert_eq!(details["expected_value_count"], json!(4));
        assert_eq!(details["actual_value_count"], json!(2));
    }

    #[test]
    fn a_track_mode_unknown_failure_names_the_movements_the_host_accepts() {
        // 判定に使った一覧をそのまま運ぶ。要求元はこれを見て、対象を読み直さ
        // ずに通り得る名前を選び直せる。**名前だけでは足りない**——一覧には
        // 書けない移動方法も並ぶため、要素ごとに可否を名乗る。
        let error = EditError::ItemWrite(ItemWriteError::Track(TrackValueError::UnknownMode {
            known: sample_movements(),
        }));
        let details = error.details();
        assert_eq!(details["reason"], json!("track_mode_unknown"));
        assert_eq!(
            details["known_movements"],
            json!([
                { "name": "直線移動", "writable": true },
                { "name": "移動無し", "writable": false },
            ])
        );
    }

    #[test]
    fn a_mode_that_cannot_be_written_is_drawn_with_its_own_reason_and_no_list() {
        // 復旧手順が違うため、一覧に無い名前とは別の名前で描く。一覧を添えても
        // 次の一手は変わらないため、値は運ばない。
        //
        // **どの入力がこの失敗になるかは、ここでは見ていない。** 見ているのは
        // 組み立てた失敗の描き方だけである。
        let error = EditError::ItemWrite(ItemWriteError::Track(TrackValueError::ModeNotWritable));
        let details = error.details();
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
        assert_eq!(details["reason"], json!("track_mode_not_writable"));
        assert!(details.get("known_movements").is_none(), "{details}");
    }

    #[test]
    fn item_write_failures_name_the_syntax_rule_they_broke() {
        // 実行側で落ちた検証も、要求内容だけで落ちた検証と同じ名前を返す。
        // ここで名前を落とすと、要求元は説明の文面を解析するほかない。
        for source in PathSyntaxError::ALL {
            let error = EditError::ItemWrite(ItemWriteError::Path(*source));
            assert_eq!(error.error_code(), ErrorCode::InvalidArgument, "{error}");
            assert_eq!(error.details()["reason"], json!(source.reason()), "{error}");
        }
        for source in TextSyntaxError::ALL {
            let error = EditError::ItemWrite(ItemWriteError::Text(*source));
            assert_eq!(error.error_code(), ErrorCode::InvalidArgument, "{error}");
            assert_eq!(error.details()["reason"], json!(source.reason()), "{error}");
        }
    }

    #[test]
    fn details_and_messages_do_not_expose_values_or_pointers() {
        // 設定値・alias・パスを含む失敗を作り、応答へ現れないことを確かめる。
        // 検証の失敗種別は補助情報へ載るようになったため、**名前ごと**確かめる。
        // 種別の名前はパスも設定値も含まないため、載せても方針は変わらない。
        let secrets: Vec<EditError> = PathSyntaxError::ALL
            .iter()
            .map(|source| EditError::ItemWrite(ItemWriteError::Path(*source)))
            .chain(
                TextSyntaxError::ALL
                    .iter()
                    .map(|source| EditError::ItemWrite(ItemWriteError::Text(*source))),
            )
            .chain([EditError::ItemWrite(ItemWriteError::ValueKindMismatch {
                item_type: EffectItemType::Integer.kind_name(),
                value_kind: "text",
            })])
            .collect();
        for error in all_errors().into_iter().chain(secrets) {
            let mut details = error.details();
            // ホストから読み直した設定値だけは取り除いてから見る。**この検査が
            // 守るのは「要求元が与えた内容を反響させない」ことであり、ホストの
            // 現在の状態はその対象ではない。** パス種別の設定項目では、ここに
            // 利用者のファイルパスが現れるのが正しい姿である。
            //
            // 欄は 2 つある。発行前に落ちた失敗が運ぶ現在値と、発行した後の
            // 照合で読んだ値である。1 つの失敗が両方を持つことはない。
            let object = details.as_object_mut().expect("補助情報はオブジェクトです");
            let host_value = object
                .remove("current_value")
                .or_else(|| object.remove("observed_value"));
            let text = format!("{} {}", error, details);
            assert!(!text.contains("0x"), "{text}");
            assert!(!text.to_lowercase().contains("handle"), "{text}");
            assert!(!text.to_lowercase().contains("pointer"), "{text}");
            assert!(!text.contains(r"C:\"), "{text}");
            // 取り除く口が別の内容の抜け道にならないことを、失敗そのものが持つ
            // 値と突き合わせて確かめる。
            let expected =
                error
                    .observed_item_value()
                    .map(str::to_string)
                    .or_else(|| match &error {
                        EditError::MovementWouldBeLost { current_value } => {
                            Some(current_value.clone())
                        }
                        _ => None,
                    });
            assert_eq!(
                host_value
                    .as_ref()
                    .and_then(Value::as_str)
                    .map(str::to_string),
                expected,
                "ホストから読み直した値の欄が別の内容を運んでいます: {error}"
            );
        }
    }

    #[test]
    fn the_reason_of_a_syntax_failure_carries_no_value() {
        // 名前は種別だけを表す。長さや位置が混ざれば、応答へ載せた時点で
        // 検証対象の内容が漏れる。
        let error = EditError::ItemWrite(ItemWriteError::Text(TextSyntaxError::TooLongBytes {
            bytes: 9_000,
            max: 8_192,
        }));
        let reason = error.details()["reason"].as_str().unwrap().to_string();
        assert_eq!(reason, "too_long");
        assert!(!reason.contains(|c: char| c.is_ascii_digit()), "{reason}");
    }

    #[test]
    fn names_are_truncated_before_they_reach_the_response() {
        let error = EditError::EffectNotFound {
            effect_name: "あ".repeat(MAX_NAME_CHARS * 2),
            effect_index: 0,
        };
        let details = error.details();
        let name = details["effect_name"].as_str().unwrap();
        assert_eq!(name.chars().count(), MAX_NAME_CHARS);
    }
}
