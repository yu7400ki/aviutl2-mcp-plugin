//! 編集 operation の params / result と、要求内容だけで決まる入力検証。
//!
//! params は未知フィールドを拒否する。ただし内側の
//! [`ObjectSelector`] / [`EffectSelector`] は応答が返した値をそのまま送り返す
//! 往復型であり、未知フィールドを拒否しない（応答へ optional field が増えた
//! ときに往復が壊れるため）。
//!
//! プロジェクト境界の照合は、対象を指す [`ObjectSelector`] が運ぶ
//! `project_epoch` で行う。同じ意味の値を 1 要求の 2 か所へ置くと不整合な組を
//! 作れてしまうため、selector を持つ operation は境界の照合用に epoch を別途
//! 受け取らない。selector を持たない [`CreateObjectParams`]（対象がまだ無い）と
//! [`SetSelectionParams`]（`focus` を省略できる）だけが
//! `expected_project_epoch` を持つ。
//!
//! 応答は読み取りの DTO（[`ObjectSummary`] / [`EffectInfo`] / [`Cursor`](crate::edit_info::Cursor) /
//! [`FrameRange`](crate::edit_info::FrameRange)）を再利用する。編集専用の対称型を作ると、クライアントが
//! 読み取りと編集の結果を同じ経路で扱えなくなる。
//!
//! opaque handle は params にも result にも現れない。

mod effect;
mod layer;
mod object;
mod scene;
mod section;
mod selection;

use crate::edit_info::GridBpm;
use crate::effect::EffectInfo;
use crate::error::ErrorCode;
use crate::item_value::ItemWriteError;
use crate::number::FiniteF64;
use crate::object::ObjectSummary;
use crate::render::MAX_RENDER_FRAME_BYTES;
use crate::selector::{EffectSelector, ObjectSelector};
use crate::validation::{PathSyntaxError, TextSyntaxError, validate_control_free, validate_path};
use serde::{Deserialize, Serialize};

pub use effect::{AddEffectParams, DeleteEffectParams, MoveEffectParams, SetEffectEnabledParams};
pub use layer::{LayerNameChange, LayerStateOutcome, SetLayerStateParams};
pub use object::{
    CreateObjectParams, DeleteObjectParams, Destination, MoveObjectParams, ObjectSource, Placement,
    SetObjectItemParams, SetObjectNameParams,
};
pub use scene::{
    GridBpmOutcome, SceneSettingsOutcome, SceneSize, SetGridBpmParams, SetSceneSettingsParams,
};
pub use section::{
    CreateObjectSectionParams, DeleteObjectSectionParams, MoveObjectSectionParams,
    ObjectSectionsOutcome,
};
pub use selection::{
    CursorPosition, DisplayStart, FocusChange, ObservedSelection, RangeChange, SelectionField,
    SelectionState, SetSelectionParams,
};

/// 構造を変更する編集の結果。
///
/// [`ObjectSummary`] / [`EffectInfo`] は selector と fingerprint を内包する
/// ため、応答だけで次の編集を組み立てられる。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditOutcome {
    /// 変更後のプロジェクトの epoch。
    pub project_epoch: String,
    /// 変更を反映したあとの revision。
    pub project_revision: u64,
    /// 変更後の対象オブジェクト。削除では null。
    pub object: Option<ObjectSummary>,
    /// effect を対象とする operation でのみ設定する。削除では null。
    pub effect: Option<EffectInfo>,
    /// 作成で生まれた全てのオブジェクト。作成以外では空。
    ///
    /// 複数オブジェクトを含む alias では 2 件以上になる。`object` はその
    /// 先頭を指す。
    pub created: Vec<ObjectSummary>,
    /// 要求した配置と、実際に生まれた配置が 1 件でも違うか。作成以外では null。
    ///
    /// 真なら `created` の全件を見る。どの 1 件が違うかは名乗らない——実際の
    /// 配置は `created` の各要素が持っている。比べるのは `(レイヤー, 開始
    /// フレーム)` の組だけであり、長さは含まない。
    pub placement_adjusted: Option<bool>,
}

impl EditOutcome {
    /// 作成の結果を組み立てる（`create_object`）。
    ///
    /// `created` に作成された全件を、`object` にその先頭を載せる。
    ///
    /// `placement_adjusted` は、要求した配置と `created` の配置が 1 件でも違う
    /// かである。呼び出し元だけが要求した配置を知っているため、引数で受け取る。
    pub fn created(
        project_epoch: impl Into<String>,
        project_revision: u64,
        created: Vec<ObjectSummary>,
        placement_adjusted: bool,
    ) -> Self {
        Self {
            project_epoch: project_epoch.into(),
            project_revision,
            object: created.first().cloned(),
            effect: None,
            created,
            placement_adjusted: Some(placement_adjusted),
        }
    }

    /// オブジェクトだけを変更した結果を組み立てる
    /// （`move_object` / `set_object_name` / `delete_effect`）。
    pub fn object_changed(
        project_epoch: impl Into<String>,
        project_revision: u64,
        object: ObjectSummary,
    ) -> Self {
        Self {
            project_epoch: project_epoch.into(),
            project_revision,
            object: Some(object),
            effect: None,
            created: Vec::new(),
            placement_adjusted: None,
        }
    }

    /// effect を伴う変更の結果を組み立てる
    /// （`set_object_item` / `add_effect` / `set_effect_enabled`）。
    ///
    /// `effect` には読み直した値を載せる。ホスト側の正規化により要求値と
    /// 異なり得るが、それは失敗ではない。
    pub fn effect_changed(
        project_epoch: impl Into<String>,
        project_revision: u64,
        object: ObjectSummary,
        effect: EffectInfo,
    ) -> Self {
        Self {
            project_epoch: project_epoch.into(),
            project_revision,
            object: Some(object),
            effect: Some(effect),
            created: Vec::new(),
            placement_adjusted: None,
        }
    }

    /// オブジェクト削除の結果を組み立てる（`delete_object`）。
    pub fn deleted(project_epoch: impl Into<String>, project_revision: u64) -> Self {
        Self {
            project_epoch: project_epoch.into(),
            project_revision,
            object: None,
            effect: None,
            created: Vec::new(),
            placement_adjusted: None,
        }
    }
}

/// `layer` フィールド名。
const FIELD_LAYER: &str = "layer";
/// `frame` フィールド名。
const FIELD_FRAME: &str = "frame";
/// 選択範囲の開始フレームのフィールド名。
const FIELD_RANGE_START: &str = "selected_range.start";
/// 選択範囲の終了フレームのフィールド名。
const FIELD_RANGE_END: &str = "selected_range.end";
/// `path` フィールド名。
const FIELD_PATH: &str = "path";
/// `alias` フィールド名。
const FIELD_ALIAS: &str = "alias";
/// `name` フィールド名。
const FIELD_NAME: &str = "name";
/// `size` フィールド名。
const FIELD_SIZE: &str = "size";
/// 解像度の横幅のフィールド名。
const FIELD_SIZE_WIDTH: &str = "size.width";
/// 解像度の高さのフィールド名。
const FIELD_SIZE_HEIGHT: &str = "size.height";
/// `sample_rate` フィールド名。
const FIELD_SAMPLE_RATE: &str = "sample_rate";
/// `item` フィールド名。
const FIELD_ITEM: &str = "item";
/// `effect_name` フィールド名。
const FIELD_EFFECT_NAME: &str = "effect_name";
/// `position` フィールド名。
const FIELD_POSITION: &str = "position";
/// `enabled` フィールド名。
const FIELD_ENABLED: &str = "enabled";
/// `locked` フィールド名。
const FIELD_LOCKED: &str = "locked";
/// `cursor` フィールド名。
const FIELD_CURSOR: &str = "cursor";
/// `selected_range` フィールド名。
const FIELD_SELECTED_RANGE: &str = "selected_range";
/// `focus` フィールド名。
const FIELD_FOCUS: &str = "focus";
/// `display` フィールド名。
const FIELD_DISPLAY: &str = "display";
/// `section` フィールド名。
const FIELD_SECTION: &str = "section";
/// `entries` フィールド名。
const FIELD_ENTRIES: &str = "entries";
/// BPM 情報のテンポのフィールド名。
const FIELD_TEMPO: &str = "tempo";
/// BPM 情報の拍子のフィールド名。
const FIELD_BEAT: &str = "beat";
/// BPM 情報の開始位置のフィールド名。
const FIELD_START: &str = "start";
/// BPM 情報の拍子オフセットのフィールド名。
const FIELD_OFFSET: &str = "offset";
/// セレクターのレイヤー番号のフィールド名。
const FIELD_SELECTOR_LAYER: &str = "selector.layer";
/// セレクターの開始フレーム番号のフィールド名。
const FIELD_SELECTOR_FRAME: &str = "selector.frame";

/// 要求内容だけで決まる検証の失敗。
///
/// 呼び出し側は [`EditInputError::error_code`] でエラーコードへ写す。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditInputError {
    /// 位置指定が受け付けられる範囲を超えている。
    #[error("{field} は {max} 以下である必要があります: {value}")]
    PositionOutOfRange {
        /// 対象フィールド名。
        field: &'static str,
        /// 指定された値。
        value: u32,
        /// 許容する最大値。
        max: u32,
    },
    /// セレクターが持つ位置指定が受け付けられる範囲を超えている。
    #[error("{field} は {max} 以下である必要があります: {value}")]
    IndexOutOfRange {
        /// 対象フィールド名。
        field: &'static str,
        /// 指定された値。
        value: usize,
        /// 許容する最大値。
        max: usize,
    },
    /// 区間番号が中間点を開始位置に持つ区間を指していない。
    ///
    /// 区間 0 の開始位置はオブジェクトの開始フレームであって中間点ではない。
    /// 対象の状態に依らず常に誤りであり、読み直しても 0 が有効になることは
    /// 無いため、前提条件の不整合ではなく要求の誤りとして扱う。
    #[error("{field} は 1 以上である必要があります: {value}")]
    SectionIndexOutOfRange {
        /// 対象フィールド名。
        field: &'static str,
        /// 指定された値。
        value: u32,
    },
    /// 変更内容が 1 つも指定されていない。
    #[error("{} のいずれかを指定する必要があります", fields.join(" / "))]
    NoChangeRequested {
        /// いずれかの指定が要るフィールド名。
        fields: &'static [&'static str],
    },
    /// シーン設定の値が受け付けられる範囲の外にある。
    ///
    /// 0 は「変更しない」の意味を持たない——省略がその役目を担う。0 以下と
    /// 受け渡せない大きさだけを落とし、それ以外の値の当否はホストが決める。
    #[error("{field} は 1 以上 {max} 以下である必要があります: {value}")]
    SceneValueOutOfRange {
        /// 対象フィールド名。
        field: &'static str,
        /// 指定された値。
        value: u32,
        /// 許容する最大値。
        max: u32,
    },
    /// シーンの解像度が 1 フレームで描ける大きさを超えている。
    ///
    /// 上限は描画と共有する。描けない大きさのシーンを作れてしまうと、作った
    /// 本人がそのシーンを 1 度も描けない。
    #[error("size は 1 フレームが {max} バイト以下に収まる必要があります: {bytes} バイト")]
    SceneFrameTooLarge {
        /// 指定された解像度が要する 1 フレームのバイト数。
        bytes: u64,
        /// 許容する最大バイト数。
        max: u64,
    },
    /// 一覧の要素数が受け付けられる上限を超えている。
    #[error("{field} は {max} 件以下である必要があります: {count}")]
    TooManyEntries {
        /// 対象フィールド名。
        field: &'static str,
        /// 指定された件数。
        count: usize,
        /// 許容する最大件数。
        max: usize,
    },
    /// BPM 情報の値が受け付けられる範囲の外にある。
    #[error("entries[{index}].{field} は{expectation}必要があります")]
    GridBpmOutOfRange {
        /// 一覧の中での位置。
        index: usize,
        /// 対象フィールド名。
        field: &'static str,
        /// 満たすべき条件。
        expectation: &'static str,
    },
    /// BPM 情報の拍子を SDK の型へ写せない。
    ///
    /// 範囲の誤りとは別に扱う。要求元が取る行動が違い、前者は意図した値を
    /// 選び直すのに対し、こちらは値そのものが受け渡せない。
    #[error("entries[{index}].beat を受け渡せません: {value}")]
    GridBpmBeatNotRepresentable {
        /// 一覧の中での位置。
        index: usize,
        /// 指定された値。
        value: i64,
    },
    /// BPM 情報の開始位置が一覧の中で重複している。
    #[error("entries[{index}].start が一覧の中で重複しています")]
    DuplicateGridBpmStart {
        /// 重複した側の、一覧の中での位置。
        index: usize,
    },
    /// 文字列の検証に失敗した。
    #[error("{field}: {source}")]
    Text {
        /// 対象フィールド名。
        field: &'static str,
        /// 失敗の内容。
        #[source]
        source: TextSyntaxError,
    },
    /// パスの検証に失敗した。
    #[error("{field}: {source}")]
    Path {
        /// 対象フィールド名。
        field: &'static str,
        /// 失敗の内容。
        #[source]
        source: PathSyntaxError,
    },
    /// 設定項目の値の検証に失敗した。
    #[error(transparent)]
    ItemValue(#[from] ItemWriteError),
}

impl EditInputError {
    /// 全 variant の代表値。
    ///
    /// [`EditInputError::reason`] が返し得る名前を数え上げるために用いる。
    /// `const` にできないのは、包む失敗が所有文字列を含むためである。
    /// 構文検証と設定値の検証を包む variant は、包む側の全種別を並べる。
    pub fn all() -> Vec<EditInputError> {
        let mut all = vec![
            EditInputError::PositionOutOfRange {
                field: FIELD_LAYER,
                value: 0,
                max: MAX_POSITION,
            },
            EditInputError::IndexOutOfRange {
                field: FIELD_SELECTOR_LAYER,
                value: 0,
                max: MAX_POSITION as usize,
            },
            EditInputError::SectionIndexOutOfRange {
                field: FIELD_SECTION,
                value: 0,
            },
            EditInputError::NoChangeRequested {
                fields: &[FIELD_NAME, FIELD_ENABLED, FIELD_LOCKED],
            },
            EditInputError::SceneValueOutOfRange {
                field: FIELD_SIZE_WIDTH,
                value: 0,
                max: MAX_POSITION,
            },
            EditInputError::SceneFrameTooLarge {
                bytes: 0,
                max: MAX_RENDER_FRAME_BYTES,
            },
            EditInputError::TooManyEntries {
                field: FIELD_ENTRIES,
                count: 0,
                max: MAX_GRID_BPM_ENTRIES,
            },
            EditInputError::GridBpmOutOfRange {
                index: 0,
                field: FIELD_TEMPO,
                expectation: "0 より大きい",
            },
            EditInputError::GridBpmBeatNotRepresentable { index: 0, value: 0 },
            EditInputError::DuplicateGridBpmStart { index: 0 },
        ];
        all.extend(
            TextSyntaxError::ALL
                .iter()
                .map(|source| EditInputError::Text {
                    field: FIELD_NAME,
                    source: *source,
                }),
        );
        all.extend(
            PathSyntaxError::ALL
                .iter()
                .map(|source| EditInputError::Path {
                    field: FIELD_PATH,
                    source: *source,
                }),
        );
        all.extend(
            ItemWriteError::all()
                .into_iter()
                .map(EditInputError::ItemValue),
        );
        all
    }

    /// 失敗の種別を表す機械可読な名前を返す。名前を持たない失敗では `None`。
    ///
    /// 名前は種別だけを表し、検証に落ちたパスも文字列も含まない。どのフィールド
    /// で落ちたかは説明の文面が担う。
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            EditInputError::Text { source, .. } => Some(source.reason()),
            EditInputError::Path { source, .. } => Some(source.reason()),
            EditInputError::ItemValue(error) => error.reason(),
            // 名前が指すのは「区間番号が中間点を開始位置に持たない」という事実
            // であり、同じ事実を実態と照合して見つけた場合の失敗と同じ名前を
            // 名乗る。復帰できるかどうかはエラーコードの側が区別する。
            EditInputError::SectionIndexOutOfRange { .. } => Some("section_index_out_of_range"),
            EditInputError::GridBpmOutOfRange { .. } => Some("grid_bpm_out_of_range"),
            // 指すのは「引数を SDK の型へ写せない」という事実であり、同じ事実を
            // 変更 API の入口で見つけた場合の失敗と同じ名前を名乗る。
            EditInputError::GridBpmBeatNotRepresentable { .. } => {
                Some("argument_not_representable")
            }
            // 指すのは「同じ対象を 2 度指定した」という事実であり、一括適用が
            // 同じ対象を 2 度変更する要求へ与える名前と同じである。
            EditInputError::DuplicateGridBpmStart { .. } => Some("duplicate_target"),
            EditInputError::PositionOutOfRange { .. }
            | EditInputError::IndexOutOfRange { .. }
            | EditInputError::TooManyEntries { .. }
            | EditInputError::SceneValueOutOfRange { .. }
            | EditInputError::SceneFrameTooLarge { .. }
            | EditInputError::NoChangeRequested { .. } => None,
        }
    }

    /// 対応するエラーコードを返す。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            EditInputError::ItemValue(error) => error.error_code(),
            EditInputError::PositionOutOfRange { .. }
            | EditInputError::IndexOutOfRange { .. }
            | EditInputError::SectionIndexOutOfRange { .. }
            | EditInputError::NoChangeRequested { .. }
            | EditInputError::SceneValueOutOfRange { .. }
            | EditInputError::SceneFrameTooLarge { .. }
            | EditInputError::TooManyEntries { .. }
            | EditInputError::GridBpmOutOfRange { .. }
            | EditInputError::GridBpmBeatNotRepresentable { .. }
            | EditInputError::DuplicateGridBpmStart { .. }
            | EditInputError::Text { .. }
            | EditInputError::Path { .. } => ErrorCode::InvalidArgument,
        }
    }
}

/// `i32` で受け渡される値の上限。
///
/// レイヤー番号・フレーム番号・シーンの解像度・サンプリングレートはいずれも
/// `i32` で受け渡され、0 以上しか意味を持たないため、符号なしで受けたうえで
/// `i32` に収まることだけを課す。
/// **上限をこれ以上狭めない。** レイヤーの実際の上限はホストが持ち、
/// 「オブジェクトが存在する最大レイヤー」は作成可能な上限ではない。空の
/// レイヤーへの作成を要求内容だけの推測で拒否しない。範囲外の指定は
/// ホストが失敗させる。
///
/// **入力 schema が宣言する上限もこの値である。** 別に定義すると、片方だけを
/// 動かしたときに schema へ適合する要求が検証で拒否される。
pub const MAX_POSITION: u32 = i32::MAX as u32;

/// BPM 情報の一覧に受け付ける最大件数。
///
/// SDK は上限を定めていない。上限が無い要求を受け付けないための、我々の側の
/// 制約である。数そのものに根拠は無い。
pub const MAX_GRID_BPM_ENTRIES: usize = 256;

/// レイヤー番号とフレーム番号の組が受け渡せる範囲に収まることを確認する。
///
/// タイムライン上の 1 点を指す型はいずれも同じ規則に従う。型ごとに書き分けると、
/// 一方だけを直したときに規則が分かれる。
fn validate_layer_frame(layer: u32, frame: u32) -> Result<(), EditInputError> {
    validate_position(FIELD_LAYER, layer)?;
    validate_position(FIELD_FRAME, frame)
}

/// 位置指定が受け渡せる範囲に収まることを確認する。
fn validate_position(field: &'static str, value: u32) -> Result<(), EditInputError> {
    if value > MAX_POSITION {
        return Err(EditInputError::PositionOutOfRange {
            field,
            value,
            max: MAX_POSITION,
        });
    }
    Ok(())
}

/// シーン設定の値が受け渡せる範囲に収まることを確認する。
///
/// 解像度もサンプリングレートも `i32` で受け渡され、0 以下は意味を持たない。
/// 上限をこれ以上狭めない——ホストが受け付ける値の一覧は我々の側に無く、
/// 狭めれば実際には通る指定を我々が拒むことになる。
fn validate_scene_value(field: &'static str, value: u32) -> Result<(), EditInputError> {
    if value == 0 || value > MAX_POSITION {
        return Err(EditInputError::SceneValueOutOfRange {
            field,
            value,
            max: MAX_POSITION,
        });
    }
    Ok(())
}

/// BPM 情報の一覧を検証する。
///
/// 要求内容だけで決まる検証であり、server と plugin の双方がこれを呼ぶ。
/// 片方だけが検証すると、受理する要求の集合が経路ごとに分かれる。
///
/// **開始位置の昇順は求めない。** 並べ替えはホストの仕事であり、求めなかった
/// 順序を強制すると、read-back の順序と要求の順序が食い違ったときに説明が要る。
/// 順序が定まらない一覧だけを拒む——開始位置が等しい 2 件は前後を決められない。
fn validate_grid_bpm_entries(entries: &[GridBpm]) -> Result<(), EditInputError> {
    if entries.len() > MAX_GRID_BPM_ENTRIES {
        return Err(EditInputError::TooManyEntries {
            field: FIELD_ENTRIES,
            count: entries.len(),
            max: MAX_GRID_BPM_ENTRIES,
        });
    }
    for (index, entry) in entries.iter().enumerate() {
        validate_grid_bpm(index, entry)?;
    }
    for (index, entry) in entries.iter().enumerate() {
        let start = entry.start.get();
        if entries[..index]
            .iter()
            .any(|earlier| earlier.start.get() == start)
        {
            return Err(EditInputError::DuplicateGridBpmStart { index });
        }
    }
    Ok(())
}

/// BPM 情報 1 件の値を検証する。
fn validate_grid_bpm(index: usize, entry: &GridBpm) -> Result<(), EditInputError> {
    let out_of_range = |field, expectation| EditInputError::GridBpmOutOfRange {
        index,
        field,
        expectation,
    };
    if entry.tempo.get() <= 0.0 {
        return Err(out_of_range(FIELD_TEMPO, "0 より大きい"));
    }
    if entry.beat < 1 {
        return Err(out_of_range(FIELD_BEAT, "1 以上である"));
    }
    if entry.start.get() < 0.0 {
        return Err(out_of_range(FIELD_START, "0 以上である"));
    }
    // ホストは tempo と offset を単精度で受け取る。単精度で無限大になる値を
    // 書き込むと、以後の読み取りが非有限値として失敗する。
    //
    // tempo は 0 へ潰れる側も見る。単精度で 0 になる値は、上の判定を通ったのに
    // 0 のテンポとして書き込まれる。丸めそのものは受け入れる——拒むのは、
    // 丸めた結果がここで課した範囲を外れる場合だけである。
    let single_tempo = as_single(entry.tempo);
    if !single_tempo.is_finite() || single_tempo <= 0.0 {
        return Err(out_of_range(FIELD_TEMPO, "単精度で表しても 0 より大きい"));
    }
    // offset は 0 を許すため、見るのは無限大への溢れだけである。
    if !as_single(entry.offset).is_finite() {
        return Err(out_of_range(FIELD_OFFSET, "単精度で表せる"));
    }
    // 拍子は SDK の 32bit 符号付き整数へそのまま渡す。
    if i32::try_from(entry.beat).is_err() {
        return Err(EditInputError::GridBpmBeatNotRepresentable {
            index,
            value: entry.beat,
        });
    }
    Ok(())
}

/// ホストが受け取る単精度へ写した値。
fn as_single(value: FiniteF64) -> f32 {
    value.get() as f32
}

/// 区間番号が中間点を指し得る範囲に収まることを確認する。
///
/// 見るのは 0 でないことと受け渡せる範囲に収まることだけである。区間の総数との
/// 比較は対象の現在の状態を要するため、変更を適用する側が行う。
fn validate_section(value: u32) -> Result<(), EditInputError> {
    if value == 0 {
        return Err(EditInputError::SectionIndexOutOfRange {
            field: FIELD_SECTION,
            value,
        });
    }
    validate_position(FIELD_SECTION, value)
}

/// セレクターの位置指定が受け渡せる範囲に収まることを確認する。
///
/// セレクターは応答が返した値をそのまま送り返す往復型であり、正常な値は必ず
/// 範囲内に収まる。それは**信頼の前提であって検証ではない**。範囲外の値をその
/// まま解決へ渡すと、対象の探索が整数変換で落ちて SDK の失敗として返る。範囲外は
/// 要求の誤りであって SDK の失敗ではないうえ、呼ばれてもいない SDK 関数を
/// 名指しする補助情報が付く。
fn validate_selector_position(selector: &ObjectSelector) -> Result<(), EditInputError> {
    validate_index(FIELD_SELECTOR_LAYER, selector.layer)?;
    validate_index(FIELD_SELECTOR_FRAME, selector.frame)
}

/// effect セレクターが含む位置指定を検証する。
fn validate_effect_selector_position(selector: &EffectSelector) -> Result<(), EditInputError> {
    validate_selector_position(&selector.object)
}

/// 添字が受け渡せる範囲に収まることを確認する。
fn validate_index(field: &'static str, value: usize) -> Result<(), EditInputError> {
    let max = MAX_POSITION as usize;
    if value > max {
        return Err(EditInputError::IndexOutOfRange { field, value, max });
    }
    Ok(())
}

/// パスの構文と、そのまま渡せる文字列かを確認する。
fn validate_path_field(field: &'static str, path: &str) -> Result<(), EditInputError> {
    validate_path(path).map_err(|source| EditInputError::Path { field, source })?;
    validate_control_free(path).map_err(|source| EditInputError::Text { field, source })
}
#[cfg(test)]
mod tests;
