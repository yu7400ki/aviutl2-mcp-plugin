//! 編集 operation の params / result と、要求内容だけで決まる入力検証。
//!
//! params は未知フィールドを拒否する。ただし内側の
//! [`ObjectSelector`] / [`EffectSelector`] は応答が返した値をそのまま送り返す
//! 往復型であり、未知フィールドを拒否しない（応答へ optional field が増えた
//! ときに往復が壊れるため）。
//!
//! 応答は読み取りの DTO（[`ObjectSummary`] / [`EffectInfo`] / [`Cursor`] /
//! [`FrameRange`]）を再利用する。編集専用の対称型を作ると、クライアントが
//! 読み取りと編集の結果を同じ経路で扱えなくなる。
//!
//! opaque handle は params にも result にも現れない。

use crate::edit_info::{Cursor, FrameRange};
use crate::effect::EffectInfo;
use crate::error::ErrorCode;
use crate::item_value::{ItemValue, ItemWriteError, validate_item_value};
use crate::object::ObjectSummary;
use crate::selector::{EffectSelector, ObjectSelector};
use crate::validation::{
    PathSyntaxError, TextSyntaxError, validate_alias, validate_control_free, validate_name,
    validate_path,
};
use serde::{Deserialize, Serialize};

/// 編集の前提条件。
///
/// 対象の同一性は selector が持つ fingerprint で検証するため、本型は
/// プロジェクトの世代だけを表す。同じ意味の値を 1 要求の 2 個所へ置くと、
/// 不整合な組を作れてしまう。作成系（対象がまだ無い）と既存対象系で同一
/// 構造であり、型を分けない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    /// 応答が返したプロジェクトの epoch。
    pub project_epoch: String,
    /// 応答が返したプロジェクトの revision。
    pub project_revision: u64,
}

/// 作成の配置先。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    /// 現在シーンの一致確認に使う guard。
    pub scene_id: i32,
    /// 0 始まりのレイヤー番号。
    pub layer: u32,
    /// 0 始まりの開始フレーム番号。
    pub frame: u32,
}

impl Placement {
    /// 位置指定の範囲を検証する。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_position(FIELD_LAYER, self.layer)?;
        validate_position(FIELD_FRAME, self.frame)
    }
}

/// 移動の宛先。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    /// 0 始まりのレイヤー番号。
    pub layer: u32,
    /// 0 始まりの開始フレーム番号。
    pub frame: u32,
}

impl Destination {
    /// 位置指定の範囲を検証する。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_position(FIELD_LAYER, self.layer)?;
        validate_position(FIELD_FRAME, self.frame)
    }
}

/// 作成元。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObjectSource {
    /// メディアファイルから作成する。
    MediaFile {
        /// 絶対パス。
        path: String,
    },
    /// object alias から作成する。
    ObjectAlias {
        /// alias ファイルと同じ形式の文字列。
        ///
        /// 複数のオブジェクトを含む alias は全てが作成される。
        alias: String,
    },
}

impl ObjectSource {
    /// 作成元の構文と大きさを検証する。
    pub fn validate(&self) -> Result<(), EditInputError> {
        match self {
            ObjectSource::MediaFile { path } => validate_path_field(FIELD_PATH, path),
            ObjectSource::ObjectAlias { alias } => {
                validate_alias(alias).map_err(|source| EditInputError::Text {
                    field: FIELD_ALIAS,
                    source,
                })
            }
        }
    }
}

/// カーソルの移動先。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorPosition {
    /// 0 始まりのレイヤー番号。
    pub layer: u32,
    /// 0 始まりのフレーム番号。
    pub frame: u32,
}

impl CursorPosition {
    /// 位置指定の範囲を検証する。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_position(FIELD_LAYER, self.layer)?;
        validate_position(FIELD_FRAME, self.frame)
    }
}

/// 選択範囲の変更。
///
/// 解除は値を持たないが、判別子だけを持つ**構造体 variant**として表す。
/// unit variant は判別子以外のフィールドを黙って読み飛ばすため、未知
/// フィールドの拒否が効かない。ワイヤ表現はどちらも `{"type":"clear"}` で
/// 変わらない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RangeChange {
    /// 範囲を設定する。
    Set {
        /// 0 始まりの開始フレーム番号。
        start: u32,
        /// 0 始まりの終了フレーム番号。
        end: u32,
    },
    /// 範囲を解除する。
    Clear {},
}

impl RangeChange {
    /// フレーム番号の範囲を検証する。
    ///
    /// 開始と終了の前後関係は判定しない。ホストが範囲外の値をクランプする
    /// ため、要求値と反映値の差異そのものは失敗ではない。
    pub fn validate(&self) -> Result<(), EditInputError> {
        match self {
            RangeChange::Set { start, end } => {
                validate_position(FIELD_RANGE_START, *start)?;
                validate_position(FIELD_RANGE_END, *end)
            }
            RangeChange::Clear {} => Ok(()),
        }
    }
}

/// フォーカス対象の変更。
///
/// 解除を構造体 variant で表す理由は [`RangeChange`] と同じである。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FocusChange {
    /// 対象を選択する。
    Set {
        /// フォーカスするオブジェクト。
        object: ObjectSelector,
    },
    /// 選択を解除する。
    ///
    /// 解決できない対象を指定したときに黙って解除することはない。解除は
    /// この指定があるときだけ行う。
    Clear {},
}

/// `create_object` の params。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateObjectParams {
    /// 作成元。
    pub source: ObjectSource,
    /// 配置先。
    pub placement: Placement,
    /// 前提条件。
    pub expected: Expected,
}

impl CreateObjectParams {
    /// 要求内容だけで決まる検証を行う。
    pub fn validate(&self) -> Result<(), EditInputError> {
        self.source.validate()?;
        self.placement.validate()
    }
}

/// `move_object` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveObjectParams {
    /// 対象オブジェクト。
    pub selector: ObjectSelector,
    /// 移動先。
    pub destination: Destination,
    /// 前提条件。
    pub expected: Expected,
}

impl MoveObjectParams {
    /// 要求内容だけで決まる検証を行う。
    pub fn validate(&self) -> Result<(), EditInputError> {
        self.destination.validate()
    }
}

/// `delete_object` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteObjectParams {
    /// 対象オブジェクト。
    pub selector: ObjectSelector,
    /// 前提条件。
    pub expected: Expected,
}

impl DeleteObjectParams {
    /// 要求内容だけで決まる検証を行う。
    pub fn validate(&self) -> Result<(), EditInputError> {
        Ok(())
    }
}

/// `set_object_name` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetObjectNameParams {
    /// 対象オブジェクト。
    pub selector: ObjectSelector,
    /// 新しい名前。`null` と省略はどちらも標準名へ戻すことを意味する。
    #[serde(default)]
    pub name: Option<String>,
    /// 前提条件。
    pub expected: Expected,
}

impl SetObjectNameParams {
    /// 要求内容だけで決まる検証を行う。
    pub fn validate(&self) -> Result<(), EditInputError> {
        match &self.name {
            Some(name) => validate_name(name).map_err(|source| EditInputError::Text {
                field: FIELD_NAME,
                source,
            }),
            None => Ok(()),
        }
    }
}

/// `set_object_item` の params。
///
/// オブジェクトの設定項目は必ずいずれかの effect に属するため、対象は
/// [`EffectSelector`] で指す。トラックバー項目の値も同じ経路を通る。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetObjectItemParams {
    /// 設定項目を持つ effect。
    pub selector: EffectSelector,
    /// 設定項目名。
    pub item: String,
    /// 設定する値。
    pub value: ItemValue,
    /// 前提条件。
    pub expected: Expected,
}

impl SetObjectItemParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// 設定項目の実在と種別との対応は、対象 effect の設定項目一覧を持つ層が
    /// 判定する。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_name(&self.item).map_err(|source| EditInputError::Text {
            field: FIELD_ITEM,
            source,
        })?;
        validate_item_value(&self.value)?;
        Ok(())
    }
}

/// `add_effect` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddEffectParams {
    /// 付与先のオブジェクト。
    pub object: ObjectSelector,
    /// 付与する effect 名。
    pub effect_name: String,
    /// 前提条件。
    pub expected: Expected,
}

impl AddEffectParams {
    /// 要求内容だけで決まる検証を行う。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_name(&self.effect_name).map_err(|source| EditInputError::Text {
            field: FIELD_EFFECT_NAME,
            source,
        })
    }
}

/// `delete_effect` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteEffectParams {
    /// 対象 effect。
    pub selector: EffectSelector,
    /// 前提条件。
    pub expected: Expected,
}

impl DeleteEffectParams {
    /// 要求内容だけで決まる検証を行う。
    pub fn validate(&self) -> Result<(), EditInputError> {
        Ok(())
    }
}

/// `set_effect_state` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetEffectStateParams {
    /// 対象 effect。
    pub selector: EffectSelector,
    /// 有効・無効。省略時は変更しない。
    #[serde(default)]
    pub enabled: Option<bool>,
    /// ロック状態。省略時は変更しない。
    #[serde(default)]
    pub locked: Option<bool>,
    /// 前提条件。
    pub expected: Expected,
}

impl SetEffectStateParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// `enabled` と `locked` の両方を省略した要求は拒否する。何も変更しない
    /// 編集要求は、成功したのか無視されたのかをクライアントが区別できない。
    pub fn validate(&self) -> Result<(), EditInputError> {
        if self.enabled.is_none() && self.locked.is_none() {
            return Err(EditInputError::NoChangeRequested {
                fields: &[FIELD_ENABLED, FIELD_LOCKED],
            });
        }
        Ok(())
    }
}

/// `set_selection` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetSelectionParams {
    /// 現在シーンの一致確認に使う guard。
    ///
    /// カーソルと選択範囲はシーンに属する値であり、対象を指す selector を
    /// 持たない。guard が無いと revision だけが対象同一性の拠り所になる。
    pub expected_scene_id: i32,
    /// カーソル位置。省略時は変更しない。
    #[serde(default)]
    pub cursor: Option<CursorPosition>,
    /// 選択範囲。省略時は変更しない。
    #[serde(default)]
    pub selected_range: Option<RangeChange>,
    /// フォーカス対象。省略時は変更しない。
    #[serde(default)]
    pub focus: Option<FocusChange>,
    /// 前提条件。
    pub expected: Expected,
}

impl SetSelectionParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// 3 つ全ての省略は拒否する。理由は
    /// [`SetEffectStateParams::validate`] と同じである。
    pub fn validate(&self) -> Result<(), EditInputError> {
        if self.cursor.is_none() && self.selected_range.is_none() && self.focus.is_none() {
            return Err(EditInputError::NoChangeRequested {
                fields: &[FIELD_CURSOR, FIELD_SELECTED_RANGE, FIELD_FOCUS],
            });
        }
        if let Some(cursor) = &self.cursor {
            cursor.validate()?;
        }
        if let Some(range) = &self.selected_range {
            range.validate()?;
        }
        Ok(())
    }
}

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
    #[serde(default)]
    pub created: Vec<ObjectSummary>,
}

impl EditOutcome {
    /// 作成の結果を組み立てる（`create_object`）。
    ///
    /// `created` に作成された全件を、`object` にその先頭を載せる。
    pub fn created(
        project_epoch: impl Into<String>,
        project_revision: u64,
        created: Vec<ObjectSummary>,
    ) -> Self {
        Self {
            project_epoch: project_epoch.into(),
            project_revision,
            object: created.first().cloned(),
            effect: None,
            created,
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
        }
    }

    /// effect を伴う変更の結果を組み立てる
    /// （`set_object_item` / `add_effect` / `set_effect_state`）。
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
        }
    }
}

/// カーソル・選択範囲・フォーカスの状態。
///
/// `set_selection` だけが返す。プロジェクトの内容を変えないため
/// [`EditOutcome`] とは別の型である。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectionState {
    /// プロジェクトの epoch。
    pub project_epoch: String,
    /// プロジェクトの revision。
    pub project_revision: u64,
    /// 反映後のカーソル位置。
    pub cursor: Cursor,
    /// 反映後の選択範囲。未選択は null。
    pub selected_range: Option<FrameRange>,
    /// 反映後のフォーカス対象。未選択は null。
    pub focus: Option<ObjectSummary>,
    /// 実際に適用できた項目。部分適用を伝える唯一の手段である。
    pub applied: Vec<SelectionField>,
    /// 反映値が編集と原子的に観測されたものではないことを示す。
    ///
    /// 常に `true` である。反映値は編集の区間を抜けたあとの読み取りで得る
    /// ため、観測までの間に他所からの変更が入り得る。将来、区間内での
    /// 再読み取りが可能になったときに原子的な観測へ切り替えられるよう、
    /// 値の意味をクライアントが解釈できる形で残す。
    pub observed_after_edit: bool,
}

impl SelectionState {
    /// 編集の区間を抜けたあとに観測した状態として組み立てる。
    pub fn observed(
        project_epoch: impl Into<String>,
        project_revision: u64,
        cursor: Cursor,
        selected_range: Option<FrameRange>,
        focus: Option<ObjectSummary>,
        applied: Vec<SelectionField>,
    ) -> Self {
        Self {
            project_epoch: project_epoch.into(),
            project_revision,
            cursor,
            selected_range,
            focus,
            applied,
            observed_after_edit: true,
        }
    }
}

/// 選択状態のうち適用できた項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionField {
    /// カーソル位置。
    Cursor,
    /// 選択範囲。
    SelectedRange,
    /// フォーカス対象。
    Focus,
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
/// `item` フィールド名。
const FIELD_ITEM: &str = "item";
/// `effect_name` フィールド名。
const FIELD_EFFECT_NAME: &str = "effect_name";
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
    /// 変更内容が 1 つも指定されていない。
    #[error("{} のいずれかを指定する必要があります", fields.join(" / "))]
    NoChangeRequested {
        /// いずれかの指定が要るフィールド名。
        fields: &'static [&'static str],
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
    /// 対応するエラーコードを返す。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            EditInputError::ItemValue(error) => error.error_code(),
            EditInputError::PositionOutOfRange { .. }
            | EditInputError::NoChangeRequested { .. }
            | EditInputError::Text { .. }
            | EditInputError::Path { .. } => ErrorCode::InvalidArgument,
        }
    }
}

/// 位置指定の上限。
///
/// レイヤー番号とフレーム番号は `i32` で受け渡され、0 以上しか意味を持た
/// ないため、符号なしで受けたうえで `i32` に収まることだけを課す。
/// **上限をこれ以上狭めない。** レイヤーの実際の上限はホストが持ち、
/// 「オブジェクトが存在する最大レイヤー」は作成可能な上限ではない。空の
/// レイヤーへの作成を要求内容だけの推測で拒否しない。範囲外の指定は
/// ホストが失敗させる。
const MAX_POSITION: u32 = i32::MAX as u32;

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

/// パスの構文と、そのまま渡せる文字列かを確認する。
fn validate_path_field(field: &'static str, path: &str) -> Result<(), EditInputError> {
    validate_path(path).map_err(|source| EditInputError::Path { field, source })?;
    validate_control_free(path).map_err(|source| EditInputError::Text { field, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::{EffectFingerprintInput, ObjectFingerprintInput};
    use crate::number::FiniteF64;
    use crate::validation::{MAX_ALIAS_BYTES, MAX_NAME_UTF16_UNITS, MAX_PATH_UTF16_UNITS};
    use serde_json::{Value, json};

    const EPOCH: &str = "78be92d1-c8c9-44c6-ae52-387548971468";

    fn sample_summary() -> ObjectSummary {
        ObjectSummary::new(
            EPOCH,
            ObjectFingerprintInput {
                scene_id: 0,
                layer: 2,
                frame_start: 120,
                frame_end: 240,
                name: Some("立ち絵"),
                alias: "alias",
            },
        )
    }

    fn sample_object_selector() -> ObjectSelector {
        sample_summary().selector
    }

    fn sample_effect_info() -> EffectInfo {
        EffectInfo::new(
            sample_object_selector(),
            EffectFingerprintInput {
                effect_name: "動画ファイル",
                effect_index: 0,
                enabled: true,
                locked: false,
                items: &[],
            },
        )
    }

    fn sample_effect_selector() -> EffectSelector {
        sample_effect_info().selector
    }

    fn sample_expected() -> Expected {
        Expected {
            project_epoch: EPOCH.to_string(),
            project_revision: 42,
        }
    }

    fn sample_create() -> CreateObjectParams {
        CreateObjectParams {
            source: ObjectSource::MediaFile {
                path: r"C:\movie.mp4".to_string(),
            },
            placement: Placement {
                scene_id: 0,
                layer: 2,
                frame: 120,
            },
            expected: sample_expected(),
        }
    }

    fn sample_move() -> MoveObjectParams {
        MoveObjectParams {
            selector: sample_object_selector(),
            destination: Destination {
                layer: 3,
                frame: 240,
            },
            expected: sample_expected(),
        }
    }

    fn sample_set_object_item() -> SetObjectItemParams {
        SetObjectItemParams {
            selector: sample_effect_selector(),
            item: "X".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new(12.5).unwrap(),
            },
            expected: sample_expected(),
        }
    }

    fn sample_set_selection() -> SetSelectionParams {
        SetSelectionParams {
            expected_scene_id: 0,
            cursor: Some(CursorPosition {
                layer: 2,
                frame: 120,
            }),
            selected_range: Some(RangeChange::Set { start: 10, end: 20 }),
            focus: Some(FocusChange::Set {
                object: sample_object_selector(),
            }),
            expected: sample_expected(),
        }
    }

    /// params を JSON へ写し、未知フィールドを足した値を返す。
    fn with_unknown_field(value: &impl Serialize) -> Value {
        let mut value = serde_json::to_value(value).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(1));
        value
    }

    /// JSON から 1 つのキーを取り除いた値を返す。
    fn without_field(value: &impl Serialize, key: &str) -> Value {
        let mut value = serde_json::to_value(value).unwrap();
        assert!(
            value.as_object_mut().unwrap().remove(key).is_some(),
            "{key} が存在しません"
        );
        value
    }

    /// JSON への往復で値が変わらないことを確かめる。
    fn assert_roundtrip<T>(params: T)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let s = serde_json::to_string(&params).unwrap();
        let restored: T = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, params);
    }

    #[test]
    fn params_roundtrip() {
        assert_roundtrip(sample_create());
        assert_roundtrip(CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "[vo]\n_name=立ち絵\n".to_string(),
            },
            ..sample_create()
        });
        assert_roundtrip(sample_move());
        assert_roundtrip(DeleteObjectParams {
            selector: sample_object_selector(),
            expected: sample_expected(),
        });
        assert_roundtrip(SetObjectNameParams {
            selector: sample_object_selector(),
            name: Some("立ち絵".to_string()),
            expected: sample_expected(),
        });
        assert_roundtrip(SetObjectNameParams {
            selector: sample_object_selector(),
            name: None,
            expected: sample_expected(),
        });
        assert_roundtrip(sample_set_object_item());
        assert_roundtrip(AddEffectParams {
            object: sample_object_selector(),
            effect_name: "ぼかし".to_string(),
            expected: sample_expected(),
        });
        assert_roundtrip(DeleteEffectParams {
            selector: sample_effect_selector(),
            expected: sample_expected(),
        });
        assert_roundtrip(SetEffectStateParams {
            selector: sample_effect_selector(),
            enabled: Some(false),
            locked: None,
            expected: sample_expected(),
        });
        assert_roundtrip(sample_set_selection());
        assert_roundtrip(SetSelectionParams {
            selected_range: Some(RangeChange::Clear {}),
            focus: Some(FocusChange::Clear {}),
            ..sample_set_selection()
        });
    }

    #[test]
    fn params_reject_unknown_fields() {
        macro_rules! assert_rejects_unknown {
            ($type:ty, $params:expr) => {
                assert!(
                    serde_json::from_value::<$type>(with_unknown_field(&$params)).is_err(),
                    "{} が未知フィールドを受理しました",
                    stringify!($type)
                );
            };
        }

        assert_rejects_unknown!(CreateObjectParams, sample_create());
        assert_rejects_unknown!(MoveObjectParams, sample_move());
        assert_rejects_unknown!(
            DeleteObjectParams,
            DeleteObjectParams {
                selector: sample_object_selector(),
                expected: sample_expected(),
            }
        );
        assert_rejects_unknown!(
            SetObjectNameParams,
            SetObjectNameParams {
                selector: sample_object_selector(),
                name: None,
                expected: sample_expected(),
            }
        );
        assert_rejects_unknown!(SetObjectItemParams, sample_set_object_item());
        assert_rejects_unknown!(
            AddEffectParams,
            AddEffectParams {
                object: sample_object_selector(),
                effect_name: "ぼかし".to_string(),
                expected: sample_expected(),
            }
        );
        assert_rejects_unknown!(
            DeleteEffectParams,
            DeleteEffectParams {
                selector: sample_effect_selector(),
                expected: sample_expected(),
            }
        );
        assert_rejects_unknown!(
            SetEffectStateParams,
            SetEffectStateParams {
                selector: sample_effect_selector(),
                enabled: Some(true),
                locked: Some(false),
                expected: sample_expected(),
            }
        );
        assert_rejects_unknown!(SetSelectionParams, sample_set_selection());
        assert_rejects_unknown!(Expected, sample_expected());
        assert_rejects_unknown!(
            Placement,
            Placement {
                scene_id: 0,
                layer: 0,
                frame: 0,
            }
        );
        assert_rejects_unknown!(Destination, Destination { layer: 0, frame: 0 });
        assert_rejects_unknown!(CursorPosition, CursorPosition { layer: 0, frame: 0 });
        assert_rejects_unknown!(
            ObjectSource,
            ObjectSource::MediaFile {
                path: r"C:\movie.mp4".to_string(),
            }
        );
        assert_rejects_unknown!(RangeChange, RangeChange::Set { start: 0, end: 1 });
        assert_rejects_unknown!(RangeChange, RangeChange::Clear {});
        assert_rejects_unknown!(
            FocusChange,
            FocusChange::Set {
                object: sample_object_selector(),
            }
        );
    }

    #[test]
    fn params_reject_missing_required_fields() {
        for key in ["source", "placement", "expected"] {
            assert!(
                serde_json::from_value::<CreateObjectParams>(without_field(&sample_create(), key))
                    .is_err(),
                "{key} の欠落が受理されました"
            );
        }
        for key in ["selector", "destination", "expected"] {
            assert!(
                serde_json::from_value::<MoveObjectParams>(without_field(&sample_move(), key))
                    .is_err(),
                "{key} の欠落が受理されました"
            );
        }
        for key in ["selector", "item", "value", "expected"] {
            assert!(
                serde_json::from_value::<SetObjectItemParams>(without_field(
                    &sample_set_object_item(),
                    key
                ))
                .is_err(),
                "{key} の欠落が受理されました"
            );
        }
        for key in ["expected_scene_id", "expected"] {
            assert!(
                serde_json::from_value::<SetSelectionParams>(without_field(
                    &sample_set_selection(),
                    key
                ))
                .is_err(),
                "{key} の欠落が受理されました"
            );
        }
        for key in ["project_epoch", "project_revision"] {
            assert!(
                serde_json::from_value::<Expected>(without_field(&sample_expected(), key)).is_err(),
                "{key} の欠落が受理されました"
            );
        }
    }

    #[test]
    fn optional_fields_may_be_omitted() {
        let params: SetEffectStateParams = serde_json::from_value(json!({
            "selector": serde_json::to_value(sample_effect_selector()).unwrap(),
            "enabled": true,
            "expected": serde_json::to_value(sample_expected()).unwrap(),
        }))
        .unwrap();
        assert_eq!(params.enabled, Some(true));
        assert_eq!(params.locked, None);

        // 省略と null の明示はどちらも標準名へ戻すことを意味する。
        let omitted: SetObjectNameParams = serde_json::from_value(json!({
            "selector": serde_json::to_value(sample_object_selector()).unwrap(),
            "expected": serde_json::to_value(sample_expected()).unwrap(),
        }))
        .unwrap();
        let explicit: SetObjectNameParams = serde_json::from_value(json!({
            "selector": serde_json::to_value(sample_object_selector()).unwrap(),
            "name": Value::Null,
            "expected": serde_json::to_value(sample_expected()).unwrap(),
        }))
        .unwrap();
        assert_eq!(omitted, explicit);
        assert_eq!(omitted.name, None);
    }

    #[test]
    fn nested_selectors_still_accept_unknown_fields() {
        // params が未知フィールドを拒否しても、往復型である selector の
        // 扱いは変わらない。応答へ optional field が増えても往復が壊れない。
        let mut value = serde_json::to_value(sample_move()).unwrap();
        value["selector"]
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(1));
        let restored: MoveObjectParams = serde_json::from_value(value).unwrap();
        assert_eq!(restored.selector, sample_object_selector());

        let mut value = serde_json::to_value(sample_set_object_item()).unwrap();
        value["selector"]
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(1));
        value["selector"]["object"]
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(2));
        let restored: SetObjectItemParams = serde_json::from_value(value).unwrap();
        assert_eq!(restored.selector, sample_effect_selector());

        let mut value = serde_json::to_value(sample_set_selection()).unwrap();
        value["focus"]["object"]
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(1));
        let restored: SetSelectionParams = serde_json::from_value(value).unwrap();
        assert_eq!(
            restored.focus,
            Some(FocusChange::Set {
                object: sample_object_selector(),
            })
        );
    }

    #[test]
    fn tagged_enums_use_snake_case_tags() {
        assert_eq!(
            serde_json::to_value(ObjectSource::MediaFile {
                path: r"C:\movie.mp4".to_string(),
            })
            .unwrap(),
            json!({"type": "media_file", "path": r"C:\movie.mp4"})
        );
        assert_eq!(
            serde_json::to_value(ObjectSource::ObjectAlias {
                alias: "[vo]".to_string(),
            })
            .unwrap(),
            json!({"type": "object_alias", "alias": "[vo]"})
        );
        assert_eq!(
            serde_json::to_value(RangeChange::Set { start: 1, end: 2 }).unwrap(),
            json!({"type": "set", "start": 1, "end": 2})
        );
        assert_eq!(
            serde_json::to_value(RangeChange::Clear {}).unwrap(),
            json!({"type": "clear"})
        );
        assert_eq!(
            serde_json::to_value(FocusChange::Clear {}).unwrap(),
            json!({"type": "clear"})
        );
        assert_eq!(
            serde_json::to_value(SelectionField::SelectedRange).unwrap(),
            json!("selected_range")
        );
    }

    #[test]
    fn positions_reject_values_outside_the_representable_range() {
        let over = MAX_POSITION + 1;
        assert_eq!(MAX_POSITION, 2_147_483_647);

        for (field, params) in [
            (
                FIELD_LAYER,
                CreateObjectParams {
                    placement: Placement {
                        scene_id: 0,
                        layer: over,
                        frame: 0,
                    },
                    ..sample_create()
                },
            ),
            (
                FIELD_FRAME,
                CreateObjectParams {
                    placement: Placement {
                        scene_id: 0,
                        layer: 0,
                        frame: over,
                    },
                    ..sample_create()
                },
            ),
        ] {
            assert_eq!(
                params.validate(),
                Err(EditInputError::PositionOutOfRange {
                    field,
                    value: over,
                    max: MAX_POSITION,
                })
            );
        }

        assert_eq!(
            MoveObjectParams {
                destination: Destination {
                    layer: over,
                    frame: 0,
                },
                ..sample_move()
            }
            .validate(),
            Err(EditInputError::PositionOutOfRange {
                field: FIELD_LAYER,
                value: over,
                max: MAX_POSITION,
            })
        );

        assert_eq!(
            SetSelectionParams {
                cursor: Some(CursorPosition {
                    layer: 0,
                    frame: over,
                }),
                selected_range: None,
                focus: None,
                ..sample_set_selection()
            }
            .validate(),
            Err(EditInputError::PositionOutOfRange {
                field: FIELD_FRAME,
                value: over,
                max: MAX_POSITION,
            })
        );

        assert_eq!(
            SetSelectionParams {
                cursor: None,
                selected_range: Some(RangeChange::Set {
                    start: 0,
                    end: over,
                }),
                focus: None,
                ..sample_set_selection()
            }
            .validate(),
            Err(EditInputError::PositionOutOfRange {
                field: FIELD_RANGE_END,
                value: over,
                max: MAX_POSITION,
            })
        );
    }

    #[test]
    fn positions_accept_the_upper_bound() {
        // 上限をこれ以上狭めない。
        assert_eq!(
            CreateObjectParams {
                placement: Placement {
                    scene_id: 0,
                    layer: MAX_POSITION,
                    frame: MAX_POSITION,
                },
                ..sample_create()
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn set_effect_state_rejects_omitting_every_change() {
        let params = SetEffectStateParams {
            selector: sample_effect_selector(),
            enabled: None,
            locked: None,
            expected: sample_expected(),
        };
        let error = params.validate().unwrap_err();
        assert_eq!(
            error,
            EditInputError::NoChangeRequested {
                fields: &["enabled", "locked"],
            }
        );
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);

        for (enabled, locked) in [
            (Some(true), None),
            (None, Some(true)),
            (Some(true), Some(true)),
        ] {
            assert_eq!(
                SetEffectStateParams {
                    selector: sample_effect_selector(),
                    enabled,
                    locked,
                    expected: sample_expected(),
                }
                .validate(),
                Ok(())
            );
        }
    }

    #[test]
    fn set_selection_rejects_omitting_every_change() {
        let params = SetSelectionParams {
            expected_scene_id: 0,
            cursor: None,
            selected_range: None,
            focus: None,
            expected: sample_expected(),
        };
        let error = params.validate().unwrap_err();
        assert_eq!(
            error,
            EditInputError::NoChangeRequested {
                fields: &["cursor", "selected_range", "focus"],
            }
        );
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);

        assert_eq!(sample_set_selection().validate(), Ok(()));
        assert_eq!(
            SetSelectionParams {
                cursor: None,
                selected_range: None,
                focus: Some(FocusChange::Clear {}),
                ..sample_set_selection()
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn create_validates_the_source() {
        assert_eq!(sample_create().validate(), Ok(()));

        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::MediaFile {
                    path: r"..\movie.mp4".to_string(),
                },
                ..sample_create()
            }
            .validate(),
            Err(EditInputError::Path {
                field: FIELD_PATH,
                source: PathSyntaxError::NotAbsolute,
            })
        );

        let path = format!(r"C:\{}", "a".repeat(MAX_PATH_UTF16_UNITS));
        assert!(matches!(
            CreateObjectParams {
                source: ObjectSource::MediaFile { path },
                ..sample_create()
            }
            .validate(),
            Err(EditInputError::Path {
                source: PathSyntaxError::TooLong { .. },
                ..
            })
        ));

        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::ObjectAlias {
                    alias: "a".repeat(MAX_ALIAS_BYTES + 1),
                },
                ..sample_create()
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_ALIAS,
                source: TextSyntaxError::TooLongBytes {
                    bytes: MAX_ALIAS_BYTES + 1,
                    max: MAX_ALIAS_BYTES,
                },
            })
        );
        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::ObjectAlias {
                    alias: "a".repeat(MAX_ALIAS_BYTES),
                },
                ..sample_create()
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn media_file_path_is_bounded_only_by_the_path_limit() {
        // 作成元のパスは設定項目の値ではないため、値としての上限は掛からない。
        let path = format!(r"C:\{}", "a".repeat(MAX_PATH_UTF16_UNITS - 3));
        assert_eq!(path.encode_utf16().count(), MAX_PATH_UTF16_UNITS);
        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::MediaFile { path },
                ..sample_create()
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn names_are_limited_in_utf16_code_units() {
        let name = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2 + 1);
        assert!(matches!(
            SetObjectNameParams {
                selector: sample_object_selector(),
                name: Some(name.clone()),
                expected: sample_expected(),
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_NAME,
                source: TextSyntaxError::TooLongUtf16 { .. },
            })
        ));
        assert!(matches!(
            AddEffectParams {
                object: sample_object_selector(),
                effect_name: name.clone(),
                expected: sample_expected(),
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_EFFECT_NAME,
                source: TextSyntaxError::TooLongUtf16 { .. },
            })
        ));
        assert!(matches!(
            SetObjectItemParams {
                item: name,
                ..sample_set_object_item()
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_ITEM,
                source: TextSyntaxError::TooLongUtf16 { .. },
            })
        ));

        let name = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2);
        assert_eq!(
            SetObjectNameParams {
                selector: sample_object_selector(),
                name: Some(name),
                expected: sample_expected(),
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn set_object_item_rejects_unknown_values() {
        let error = SetObjectItemParams {
            value: ItemValue::Unknown {
                raw: "future=1".to_string(),
            },
            ..sample_set_object_item()
        }
        .validate()
        .unwrap_err();
        assert_eq!(
            error,
            EditInputError::ItemValue(ItemWriteError::UnknownValue)
        );
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    }

    #[test]
    fn edit_outcome_matches_the_operation_table() {
        let created = vec![sample_summary(), sample_summary()];
        // operation ごとの object / effect / created の設定内容。
        let cases: Vec<(&str, EditOutcome, bool, bool, usize)> = vec![
            (
                "create_object",
                EditOutcome::created(EPOCH, 43, created.clone()),
                true,
                false,
                2,
            ),
            (
                "move_object",
                EditOutcome::object_changed(EPOCH, 43, sample_summary()),
                true,
                false,
                0,
            ),
            (
                "delete_object",
                EditOutcome::deleted(EPOCH, 43),
                false,
                false,
                0,
            ),
            (
                "set_object_name",
                EditOutcome::object_changed(EPOCH, 43, sample_summary()),
                true,
                false,
                0,
            ),
            (
                "set_object_item",
                EditOutcome::effect_changed(EPOCH, 43, sample_summary(), sample_effect_info()),
                true,
                true,
                0,
            ),
            (
                "add_effect",
                EditOutcome::effect_changed(EPOCH, 43, sample_summary(), sample_effect_info()),
                true,
                true,
                0,
            ),
            (
                "delete_effect",
                EditOutcome::object_changed(EPOCH, 43, sample_summary()),
                true,
                false,
                0,
            ),
            (
                "set_effect_state",
                EditOutcome::effect_changed(EPOCH, 43, sample_summary(), sample_effect_info()),
                true,
                true,
                0,
            ),
        ];

        for (operation, outcome, has_object, has_effect, created_count) in cases {
            assert_eq!(outcome.object.is_some(), has_object, "{operation}: object");
            assert_eq!(outcome.effect.is_some(), has_effect, "{operation}: effect");
            assert_eq!(outcome.created.len(), created_count, "{operation}: created");
            assert_eq!(outcome.project_epoch, EPOCH, "{operation}");
            assert_eq!(outcome.project_revision, 43, "{operation}");
        }
    }

    #[test]
    fn created_outcome_points_at_the_first_object() {
        let created = vec![sample_summary(), sample_summary()];
        let outcome = EditOutcome::created(EPOCH, 43, created.clone());
        assert_eq!(outcome.object.as_ref(), created.first());
        assert_eq!(outcome.created, created);

        // 作成された件数が 0 の場合は対象を名乗らない。
        let empty = EditOutcome::created(EPOCH, 43, Vec::new());
        assert_eq!(empty.object, None);
        assert!(empty.created.is_empty());
    }

    #[test]
    fn results_roundtrip() {
        let outcome =
            EditOutcome::effect_changed(EPOCH, 43, sample_summary(), sample_effect_info());
        let s = serde_json::to_string(&outcome).unwrap();
        assert_eq!(serde_json::from_str::<EditOutcome>(&s).unwrap(), outcome);

        let state = SelectionState::observed(
            EPOCH,
            42,
            Cursor {
                frame: 120,
                layer: 2,
            },
            Some(FrameRange { start: 10, end: 20 }),
            Some(sample_summary()),
            vec![SelectionField::Cursor, SelectionField::Focus],
        );
        let s = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<SelectionState>(&s).unwrap(), state);
    }

    #[test]
    fn results_allow_unknown_optional_fields() {
        // 応答型は将来の optional field を受け入れる。
        let outcome = EditOutcome::deleted(EPOCH, 43);
        let restored: EditOutcome = serde_json::from_value(with_unknown_field(&outcome)).unwrap();
        assert_eq!(restored, outcome);

        let state = SelectionState::observed(
            EPOCH,
            42,
            Cursor { frame: 0, layer: 0 },
            None,
            None,
            Vec::new(),
        );
        let restored: SelectionState = serde_json::from_value(with_unknown_field(&state)).unwrap();
        assert_eq!(restored, state);
    }

    #[test]
    fn selection_state_is_always_observed_after_the_edit() {
        let state = SelectionState::observed(
            EPOCH,
            42,
            Cursor { frame: 0, layer: 0 },
            None,
            None,
            vec![SelectionField::Cursor],
        );
        assert!(state.observed_after_edit);
    }

    #[test]
    fn results_do_not_expose_handles() {
        let documents = [
            serde_json::to_string(&EditOutcome::created(EPOCH, 43, vec![sample_summary()]))
                .unwrap(),
            serde_json::to_string(&EditOutcome::effect_changed(
                EPOCH,
                43,
                sample_summary(),
                sample_effect_info(),
            ))
            .unwrap(),
            serde_json::to_string(&EditOutcome::deleted(EPOCH, 43)).unwrap(),
            serde_json::to_string(&SelectionState::observed(
                EPOCH,
                42,
                Cursor { frame: 0, layer: 0 },
                Some(FrameRange { start: 0, end: 1 }),
                Some(sample_summary()),
                vec![SelectionField::Focus],
            ))
            .unwrap(),
        ];

        for document in documents {
            let lowered = document.to_lowercase();
            for forbidden in ["handle", "pointer", "0x", "secret", "nonce"] {
                assert!(
                    !lowered.contains(forbidden),
                    "{forbidden} が応答に含まれます: {document}"
                );
            }
        }
    }
}
