//! 編集の失敗を表す型と、応答へ載せる安全な補助情報。

use crate::alias::AliasRejection;
use crate::read::ReadError;
use aviutl2_mcp_core::{ErrorCode, ItemWriteError};
use serde_json::{Map, Value, json};

/// 応答の補助情報へ載せる名前の上限文字数。
///
/// effect 名・設定項目名は要求元が指定を訂正するのに要るが、長さは要求元が
/// 決めるため、そのまま反響させると応答が膨らむ。
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
    /// 選択肢から選ぶ設定項目へ、選択肢に無い値を書こうとした。
    ///
    /// SDK は選択肢を列挙する手段を持たず、選択肢に無い値を渡しても失敗を
    /// 返さずに黙って無視する。書き込んだ直後の読み直しだけが検出できる。
    ///
    /// [`UnsupportedReason::ChangeNotApplied`] と畳まない。あちらは変更を
    /// 拒む旨をヘッダーが記していない API で起きる想定外の不一致であり、
    /// こちらは選択肢を知る手段が無い以上、当て推量が外れて頻発する。
    /// 要求元が取る行動も違う——前者は異常として報告し、後者は既存の
    /// オブジェクトから有効な値を読んで別の値を試す。
    ChoiceValueRejected,
    /// 逆操作の材料を変更前に読み取れない。
    ///
    /// 逆操作を組み立てられない変更は発行しない。実行してから組み立てられないと
    /// 分かる経路を作らないための拒否である。
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
        UnsupportedReason::MediaNotSupported,
        UnsupportedReason::ItemTypeNotWritable,
        UnsupportedReason::ChangeNotApplied,
        UnsupportedReason::ChoiceValueRejected,
        UnsupportedReason::InverseUnavailable,
    ];

    /// 応答へ載せる機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            UnsupportedReason::EffectNotRegistered => "effect_not_registered",
            UnsupportedReason::EffectNotCreatable => "effect_not_creatable",
            UnsupportedReason::EffectStateImmutable => "effect_state_immutable",
            UnsupportedReason::MediaNotSupported => "media_not_supported",
            UnsupportedReason::ItemTypeNotWritable => "item_type_not_writable",
            UnsupportedReason::ChangeNotApplied => "change_not_applied",
            UnsupportedReason::ChoiceValueRejected => "choice_value_rejected",
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
            UnsupportedReason::MediaNotSupported => "対応していないメディアファイルです",
            UnsupportedReason::ItemTypeNotWritable => {
                "この種別の設定項目への書き込みには対応していません"
            }
            UnsupportedReason::ChangeNotApplied => "要求した変更が反映されませんでした",
            UnsupportedReason::ChoiceValueRejected => {
                "指定した値は設定項目の選択肢として受け付けられませんでした"
            }
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
    /// 読み直した区間の実態が要求と食い違う。
    #[error("{reason}")]
    SectionPrecondition {
        /// 食い違いの内容。
        reason: SectionPreconditionReason,
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
    #[error("{source}")]
    AfterMutation {
        /// 発行後に生じた失敗そのもの。
        #[source]
        source: Box<EditError>,
        /// 加算後の revision。
        project_revision: u64,
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
            },
        }
    }

    /// 応答へ載せるエラーコードを返す。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            EditError::Read(error) => error.error_code(),
            EditError::DestinationOccupied { .. }
            | EditError::LayerLocked { .. }
            | EditError::EpochMismatch { .. }
            | EditError::SectionPrecondition { .. }
            | EditError::EffectFingerprintMismatch => ErrorCode::PreconditionFailed,
            EditError::EffectNotFound { .. } => ErrorCode::NotFound,
            EditError::DuplicateTarget => ErrorCode::InvalidArgument,
            EditError::ItemWrite(error) => error.error_code(),
            EditError::UnsupportedTarget { .. } => ErrorCode::UnsupportedOperation,
            EditError::AliasRejected(rejection) => rejection.error_code(),
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
            EditError::AfterMutation { .. } => RetryRequires::Refetch,
            // 一括適用の案内は失敗そのものが決める。巻き戻せなかった場合は
            // 内側が発行後の失敗になっているため、読み直しへ倒れる。
            EditError::Batch { source, .. } => source.retry_requires(),
            EditError::Read(ReadError::NotReady)
            | EditError::Read(ReadError::EditBlocked { .. }) => RetryRequires::Resend,
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
            EditError::UnsupportedTarget { reason } => {
                details.insert("reason".to_string(), json!(reason.as_str()));
            }
            // 落ちた条件が組み立てた補助情報をそのまま取り込む。名前もファイルの
            // 内容も含まないことは、組み立てる側が保証している。
            EditError::AliasRejected(rejection) => merge(details, rejection.details()),
            EditError::SectionPrecondition { reason } => {
                details.insert("reason".to_string(), json!(reason.as_str()));
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
            } => {
                source.fill_details(details);
                details.insert("mutation_issued".to_string(), json!(true));
                details.insert(
                    "current_project_revision".to_string(),
                    json!(project_revision),
                );
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
/// 載せるのは項目名と失敗の種別だけである。値そのものと、種別の照合に用いた
/// 表記は要求元の内容であり、応答へ反響させない。種別の名前はパスも文字列も
/// 含まないため、パス値・文字列値の検証に落ちた場合もそのまま載せられる。
fn fill_item_write_details(details: &mut Map<String, Value>, error: &ItemWriteError) {
    if let ItemWriteError::ItemNotFound { item } = error {
        details.insert("item".to_string(), json!(truncate(item)));
    }
    if let Some(reason) = error.reason() {
        details.insert("reason".to_string(), json!(reason));
    }
}

/// 別に組み立てた補助情報を取り込む。
fn merge(details: &mut Map<String, Value>, source: Value) {
    if let Value::Object(source) = source {
        details.extend(source);
    }
}

/// 名前を応答へ載せられる長さへ切り詰める。
fn truncate(name: &str) -> String {
    name.chars().take(MAX_NAME_CHARS).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::read::EditState;
    use crate::test_support::sample_object_summary;
    use aviutl2_mcp_core::{EffectItemType, PathSyntaxError, TextSyntaxError};

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
                item_type: EffectItemType::Scene.kind_name(),
            }),
            EditError::ItemWrite(ItemWriteError::UnknownValue),
            EditError::ItemWrite(ItemWriteError::ValueKindMismatch {
                item_type: EffectItemType::Integer.kind_name(),
                value_kind: "text",
            }),
            EditError::ItemWrite(ItemWriteError::Text(TextSyntaxError::ContainsNul)),
            EditError::ItemWrite(ItemWriteError::Path(PathSyntaxError::NotAbsolute)),
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
                reason: UnsupportedReason::MediaNotSupported,
            },
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::ItemTypeNotWritable,
            },
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::ChangeNotApplied,
            },
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::ChoiceValueRejected,
            },
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::InverseUnavailable,
            },
            // 受け入れ規則の 4 条件は、名前を持つものと持たないものの双方を通す。
            EditError::AliasRejected(AliasRejection::NotFound),
            EditError::AliasRejected(AliasRejection::WithoutEffect),
            EditError::NotIssued {
                reason: NotIssuedReason::TargetMissing,
            },
            EditError::NotIssued {
                reason: NotIssuedReason::ArgumentNotRepresentable,
            },
            EditError::SectionPrecondition {
                reason: SectionPreconditionReason::FrameOutsideObject,
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
            EditError::DestinationOccupied { .. } => "DestinationOccupied",
            EditError::EpochMismatch { .. } => "EpochMismatch",
            EditError::LayerLocked { .. } => "LayerLocked",
            EditError::EffectFingerprintMismatch => "EffectFingerprintMismatch",
            EditError::EffectNotFound { .. } => "EffectNotFound",
            EditError::DuplicateTarget => "DuplicateTarget",
            EditError::ItemWrite(_) => "ItemWrite",
            EditError::UnsupportedTarget { .. } => "UnsupportedTarget",
            EditError::AliasRejected(_) => "AliasRejected",
            EditError::SectionPrecondition { .. } => "SectionPrecondition",
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
            "DestinationOccupied",
            "EpochMismatch",
            "LayerLocked",
            "EffectFingerprintMismatch",
            "EffectNotFound",
            "DuplicateTarget",
            "ItemWrite",
            "UnsupportedTarget",
            "AliasRejected",
            "SectionPrecondition",
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
            UnsupportedReason::MediaNotSupported,
            UnsupportedReason::ItemTypeNotWritable,
            UnsupportedReason::ChangeNotApplied,
            UnsupportedReason::ChoiceValueRejected,
            UnsupportedReason::InverseUnavailable,
        ];
        for reason in unsupported {
            match reason {
                UnsupportedReason::EffectNotRegistered
                | UnsupportedReason::EffectNotCreatable
                | UnsupportedReason::EffectStateImmutable
                | UnsupportedReason::MediaNotSupported
                | UnsupportedReason::ItemTypeNotWritable
                | UnsupportedReason::ChangeNotApplied
                | UnsupportedReason::ChoiceValueRejected
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
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                // 選択肢に無い値が黙って無視された。読み直しても有効にならない。
                ErrorCode::UnsupportedOperation,
                // 逆操作の材料を読めなかった。
                ErrorCode::UnsupportedOperation,
                // 名前で指定されたエイリアスが受け入れ規則を通らなかった。落ちた
                // 条件がコードを決めるため、不在と構造の欠陥で別の値になる。
                ErrorCode::NotFound,
                ErrorCode::InvalidArgument,
                // 対象が失われていた。要求の対象が無いのだから見つからない。
                ErrorCode::NotFound,
                // 引数を写せなかった。SDK の失敗ではなく要求の誤りである。
                ErrorCode::InvalidArgument,
                // 読み直した区間の実態と食い違った。復帰の手段は読み直しである。
                ErrorCode::PreconditionFailed,
                // 事前確認を通したのに SDK が拒んだ。要求元に直せることが無い。
                ErrorCode::SdkError,
                ErrorCode::SdkError,
                ErrorCode::InternalError,
                // 前提の作り方の誤りであり、要求を作り直しても解消しない。
                ErrorCode::InternalError,
                ErrorCode::SdkError,
                // 一括適用は失敗の理由をそのまま保つ。
                ErrorCode::PreconditionFailed,
                ErrorCode::PreconditionFailed,
                // 戻せなかった変更が残っている場合だけ書き換える。
                ErrorCode::SdkError,
                ErrorCode::InternalError,
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
    fn wrapping_twice_keeps_the_revision_of_the_first_issue() {
        let error = EditError::Panicked.after_mutation(44).after_mutation(99);
        assert_eq!(error.details()["current_project_revision"], json!(44));
    }

    #[test]
    fn details_only_use_allowed_keys() {
        // 補助情報のキーはここで列挙したものに限る。入れ子の内側まで見る。
        // トップレベルだけを見ると、値をオブジェクトで包んだ瞬間に検査が
        // 素通りする。新しいキーを足す際はハンドル・生ポインタ・設定値・
        // alias・パスでないことを確かめる。
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
            let text = format!("{} {}", error, error.details());
            assert!(!text.contains("0x"), "{text}");
            assert!(!text.to_lowercase().contains("handle"), "{text}");
            assert!(!text.to_lowercase().contains("pointer"), "{text}");
            assert!(!text.contains(r"C:\"), "{text}");
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
