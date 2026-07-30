//! 編集 tool の入力型。
//!
//! 往復型以外は未知フィールドを拒否する。[`ObjectSelectorInput`] /
//! [`EffectSelectorInput`] / [`ItemValueInput`] は応答が返した値をそのまま
//! 送り返す往復型であり、応答へ optional field が増えたときに往復が壊れる
//! ため拒否しない。読み取りが返した値をそのまま書き戻せることは設定項目の
//! 契約であり、入口で拒否すると読める値が書けなくなる。
//!
//! schema の制約は宣言であり、要求がそれを満たすかどうかは検証されない。
//! 宣言した制約は接続前に実際へ確かめ、違反を `invalid_argument` として返す。
//! 対応は次のとおり。
//!
//! - `instance_id` の長さと書式: [`parse_instance_id`](crate::mcp::input::parse_instance_id)
//! - selector の文字列長と fingerprint の書式:
//!   [`ObjectSelectorInput::to_selector`] / [`EffectSelectorInput::to_selector`]
//! - `expected.project_epoch` の長さ: [`ExpectedInput::to_expected`]
//! - `layer` / `frame` の範囲、名前・パス・alias・設定値の長さと文字種:
//!   各 `to_params` が呼ぶ core の検証（要求元と実行側が同じ実装を共有する）
//!
//! 文字列長の宣言は JSON Schema の `maxLength`（文字数）で表すのに対し、core の
//! 検証はバイト数または UTF-16 code unit 数で数える。どちらも文字数以上を数える
//! ため、core を通った値は宣言した文字数上限を必ず満たす。宣言だけがあって
//! 検証されない制約は生じない。

use crate::mcp::failure::{from_code, invalid_argument};
use crate::mcp::input::{
    FINGERPRINT_PATTERN, MAX_ALGORITHM_CHARS, MAX_EPOCH_CHARS, MAX_NAME_CHARS, ObjectSelectorInput,
    UUID_PATTERN, ensure_length,
};
use aviutl2_mcp_core::{
    AddEffectParams, CreateObjectParams, CursorPosition, DeleteEffectParams, DeleteObjectParams,
    Destination, EditInputError, EffectSelector, ErrorObject, Expected, FiniteF64, FocusChange,
    ItemValue, MAX_ALIAS_BYTES, MAX_ITEM_VALUE_BYTES, MAX_PATH_UTF16_UNITS, MoveObjectParams,
    ObjectSource, Placement, RangeChange, SetEffectStateParams, SetObjectItemParams,
    SetObjectNameParams, SetSelectionParams,
};
use schemars::JsonSchema;
use serde::Deserialize;

/// レイヤー番号・フレーム番号に許す最大値。
///
/// ホストは位置を 32bit 符号付き整数で受け渡すため、それに収まることだけを
/// 課す。実際に配置できるかはホストが判定する。
const MAX_POSITION: u32 = i32::MAX as u32;

/// object alias に許す最大文字数。
const MAX_ALIAS_CHARS: u32 = MAX_ALIAS_BYTES as u32;

/// パスに許す最大文字数。
const MAX_PATH_CHARS: u32 = MAX_PATH_UTF16_UNITS as u32;

/// 設定項目の文字列値に許す最大文字数。
const MAX_ITEM_VALUE_CHARS: u32 = MAX_ITEM_VALUE_BYTES as u32;

/// 編集の前提条件。
///
/// 対象の同一性は selector の fingerprint が担うため、ここはプロジェクトの
/// 世代だけを運ぶ。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExpectedInput {
    /// 直前の読み取りまたは編集の応答が返した project_epoch。
    #[schemars(length(min = 1, max = MAX_EPOCH_CHARS))]
    pub project_epoch: String,
    /// 直前の読み取りまたは編集の応答が返した project_revision。
    pub project_revision: u64,
}

impl ExpectedInput {
    /// 前提条件へ変換する。文字数はここで検証される。
    pub(crate) fn to_expected(&self) -> Result<Expected, ErrorObject> {
        ensure_length(
            "expected.project_epoch",
            &self.project_epoch,
            1,
            MAX_EPOCH_CHARS,
        )?;
        Ok(Expected {
            project_epoch: self.project_epoch.clone(),
            project_revision: self.project_revision,
        })
    }
}

/// オブジェクト内の effect を再指定するセレクター。
///
/// [`ObjectSelectorInput`] と同じく往復型であり、未知フィールドを拒否しない。
/// 内側の `object` も同じ扱いになる。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct EffectSelectorInput {
    /// effect が属するオブジェクト。
    pub object: ObjectSelectorInput,
    /// effect 名。
    #[schemars(length(max = MAX_NAME_CHARS))]
    pub effect_name: String,
    /// 同名 effect のうち何番目か。0 始まり。
    pub effect_index: u32,
    /// 同一性検証用の fingerprint。
    #[schemars(pattern(FINGERPRINT_PATTERN))]
    pub fingerprint: String,
    /// fingerprint の算出方式。
    #[schemars(length(min = 1, max = MAX_ALGORITHM_CHARS))]
    pub fingerprint_algorithm: String,
}

impl EffectSelectorInput {
    /// セレクターへ変換する。文字数と fingerprint の書式はここで検証される。
    pub(crate) fn to_selector(&self) -> Result<EffectSelector, ErrorObject> {
        let object = self.object.to_selector()?;
        ensure_length("selector.effect_name", &self.effect_name, 0, MAX_NAME_CHARS)?;
        ensure_length(
            "selector.fingerprint_algorithm",
            &self.fingerprint_algorithm,
            1,
            MAX_ALGORITHM_CHARS,
        )?;
        let object = serde_json::to_value(&object).map_err(|_| {
            from_code(
                aviutl2_mcp_core::ErrorCode::InternalError,
                "selector を組み立てられませんでした",
            )
        })?;
        let value = serde_json::json!({
            "object": object,
            "effect_name": self.effect_name,
            "effect_index": self.effect_index,
            "fingerprint": self.fingerprint,
            "fingerprint_algorithm": self.fingerprint_algorithm,
        });
        serde_json::from_value(value).map_err(|_| {
            invalid_argument(
                "selector を解釈できません。aviutl2_get_object が返した effect の selector をそのまま指定してください",
            )
        })
    }
}

/// 作成の配置先。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlacementInput {
    /// 現在シーンの一致確認に使うシーン ID。
    pub scene_id: i32,
    /// 0 始まりのレイヤー番号。
    #[schemars(range(max = MAX_POSITION))]
    pub layer: u32,
    /// 0 始まりの開始フレーム番号。
    #[schemars(range(max = MAX_POSITION))]
    pub frame: u32,
}

impl PlacementInput {
    fn to_placement(self) -> Placement {
        Placement {
            scene_id: self.scene_id,
            layer: self.layer,
            frame: self.frame,
        }
    }
}

/// 移動の宛先。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DestinationInput {
    /// 0 始まりのレイヤー番号。
    #[schemars(range(max = MAX_POSITION))]
    pub layer: u32,
    /// 0 始まりの開始フレーム番号。
    #[schemars(range(max = MAX_POSITION))]
    pub frame: u32,
}

impl DestinationInput {
    fn to_destination(self) -> Destination {
        Destination {
            layer: self.layer,
            frame: self.frame,
        }
    }
}

/// 作成元。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObjectSourceInput {
    /// メディアファイルから作成する。
    MediaFile {
        /// 絶対パス。相対パス・device path・代替データストリームは受け付けない。
        #[schemars(length(min = 1, max = MAX_PATH_CHARS))]
        path: String,
    },
    /// object alias から作成する。
    ObjectAlias {
        /// alias ファイルと同じ形式の文字列。複数オブジェクトを含む場合は全てが作成される。
        #[schemars(length(max = MAX_ALIAS_CHARS))]
        alias: String,
    },
}

impl ObjectSourceInput {
    fn to_source(&self) -> ObjectSource {
        match self {
            ObjectSourceInput::MediaFile { path } => ObjectSource::MediaFile { path: path.clone() },
            ObjectSourceInput::ObjectAlias { alias } => ObjectSource::ObjectAlias {
                alias: alias.clone(),
            },
        }
    }
}

/// カーソルの移動先。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CursorPositionInput {
    /// 0 始まりのレイヤー番号。
    #[schemars(range(max = MAX_POSITION))]
    pub layer: u32,
    /// 0 始まりのフレーム番号。
    #[schemars(range(max = MAX_POSITION))]
    pub frame: u32,
}

impl CursorPositionInput {
    fn to_position(self) -> CursorPosition {
        CursorPosition {
            layer: self.layer,
            frame: self.frame,
        }
    }
}

/// 選択範囲の変更。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RangeChangeInput {
    /// 範囲を設定する。
    Set {
        /// 0 始まりの開始フレーム番号。
        #[schemars(range(max = MAX_POSITION))]
        start: u32,
        /// 0 始まりの終了フレーム番号。
        #[schemars(range(max = MAX_POSITION))]
        end: u32,
    },
    /// 範囲を解除する。
    Clear {},
}

impl RangeChangeInput {
    fn to_change(self) -> RangeChange {
        match self {
            RangeChangeInput::Set { start, end } => RangeChange::Set { start, end },
            RangeChangeInput::Clear {} => RangeChange::Clear {},
        }
    }
}

/// フォーカス対象の変更。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FocusChangeInput {
    /// 対象を選択する。
    Set {
        /// フォーカスするオブジェクト。
        object: ObjectSelectorInput,
    },
    /// 選択を解除する。解決できない対象を指定しても黙って解除されることはない。
    Clear {},
}

impl FocusChangeInput {
    fn to_change(&self) -> Result<FocusChange, ErrorObject> {
        Ok(match self {
            FocusChangeInput::Set { object } => FocusChange::Set {
                object: object.to_selector()?,
            },
            FocusChangeInput::Clear {} => FocusChange::Clear {},
        })
    }
}

/// 設定項目へ書き込む値。
///
/// 読み取りが返す値と同じ形で受け取り、そのまま書き戻せるようにする。
/// selector と同じ往復型であるため、未知フィールドを拒否しない。応答へ
/// optional field が増えたとき、読み取りが返した値を書き戻せなくなる非対称を
/// 作らないためである。必須フィールドの欠落と型不一致は従来どおり拒否する。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ItemValueInput {
    /// 実数。有限値のみ。
    Number {
        /// 値。
        value: f64,
    },
    /// 整数。
    Integer {
        /// 値。
        value: i64,
    },
    /// 真偽値。
    Bool {
        /// 値。
        value: bool,
    },
    /// 色。
    Color {
        /// 値。
        #[schemars(length(max = MAX_ITEM_VALUE_CHARS))]
        value: String,
    },
    /// 一覧からの選択。
    Choice {
        /// 選択する表示文字列。
        #[schemars(length(max = MAX_ITEM_VALUE_CHARS))]
        value: String,
        /// 読み取りが付ける補助情報。書き込みでは無視される。
        #[serde(default)]
        index: Option<u32>,
    },
    /// ファイルパス。
    File {
        /// 絶対パス。
        #[schemars(length(max = MAX_ITEM_VALUE_CHARS))]
        path: String,
    },
    /// フォルダパス。
    Folder {
        /// 絶対パス。
        #[schemars(length(max = MAX_ITEM_VALUE_CHARS))]
        path: String,
    },
    /// フォント名。
    Font {
        /// フォント名。
        #[schemars(length(max = MAX_ITEM_VALUE_CHARS))]
        name: String,
    },
    /// テキスト。改行とタブを含められる。
    Text {
        /// 値。
        #[schemars(length(max = MAX_ITEM_VALUE_CHARS))]
        value: String,
    },
    /// 未対応種別の生値。読み取りは返すが、書き込みには指定できない。
    Unknown {
        /// 生文字列。
        raw: String,
    },
}

impl ItemValueInput {
    /// 設定値へ変換する。書き込みの可否は変換後の検証が判定する。
    fn to_value(&self) -> Result<ItemValue, ErrorObject> {
        Ok(match self {
            ItemValueInput::Number { value } => ItemValue::Number {
                value: FiniteF64::try_new(*value)
                    .ok_or_else(|| invalid_argument("value は有限の数値である必要があります"))?,
            },
            ItemValueInput::Integer { value } => ItemValue::Integer { value: *value },
            ItemValueInput::Bool { value } => ItemValue::Bool { value: *value },
            ItemValueInput::Color { value } => ItemValue::Color {
                value: value.clone(),
            },
            ItemValueInput::Choice { value, index } => ItemValue::Choice {
                value: value.clone(),
                index: index.map(|index| index as usize),
            },
            ItemValueInput::File { path } => ItemValue::File { path: path.clone() },
            ItemValueInput::Folder { path } => ItemValue::Folder { path: path.clone() },
            ItemValueInput::Font { name } => ItemValue::Font { name: name.clone() },
            ItemValueInput::Text { value } => ItemValue::Text {
                value: value.clone(),
            },
            ItemValueInput::Unknown { raw } => ItemValue::Unknown { raw: raw.clone() },
        })
    }
}

/// `aviutl2_create_object` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateObjectInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 作成元。
    pub source: ObjectSourceInput,
    /// 配置先。
    pub placement: PlacementInput,
    /// 前提条件。
    pub expected: ExpectedInput,
}

impl CreateObjectInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<CreateObjectParams, ErrorObject> {
        let params = CreateObjectParams {
            source: self.source.to_source(),
            placement: self.placement.to_placement(),
            expected: self.expected.to_expected()?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `aviutl2_move_object` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveObjectInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象オブジェクトのセレクター。
    pub selector: ObjectSelectorInput,
    /// 移動先。
    pub destination: DestinationInput,
    /// 前提条件。
    pub expected: ExpectedInput,
}

impl MoveObjectInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<MoveObjectParams, ErrorObject> {
        let params = MoveObjectParams {
            selector: self.selector.to_selector()?,
            destination: self.destination.to_destination(),
            expected: self.expected.to_expected()?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `aviutl2_delete_object` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteObjectInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象オブジェクトのセレクター。
    pub selector: ObjectSelectorInput,
    /// 前提条件。
    pub expected: ExpectedInput,
}

impl DeleteObjectInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<DeleteObjectParams, ErrorObject> {
        let params = DeleteObjectParams {
            selector: self.selector.to_selector()?,
            expected: self.expected.to_expected()?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `aviutl2_set_object_name` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetObjectNameInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象オブジェクトのセレクター。
    pub selector: ObjectSelectorInput,
    /// 新しい名前。null と省略はどちらも標準名へ戻すことを意味する。
    #[serde(default)]
    #[schemars(length(max = MAX_NAME_CHARS))]
    pub name: Option<String>,
    /// 前提条件。
    pub expected: ExpectedInput,
}

impl SetObjectNameInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<SetObjectNameParams, ErrorObject> {
        let params = SetObjectNameParams {
            selector: self.selector.to_selector()?,
            name: self.name.clone(),
            expected: self.expected.to_expected()?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `aviutl2_set_object_item` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetObjectItemInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 設定項目を持つ effect のセレクター。
    pub selector: EffectSelectorInput,
    /// 設定項目名。
    #[schemars(length(max = MAX_NAME_CHARS))]
    pub item: String,
    /// 設定する値。
    pub value: ItemValueInput,
    /// 前提条件。
    pub expected: ExpectedInput,
}

impl SetObjectItemInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<SetObjectItemParams, ErrorObject> {
        let params = SetObjectItemParams {
            selector: self.selector.to_selector()?,
            item: self.item.clone(),
            value: self.value.to_value()?,
            expected: self.expected.to_expected()?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `aviutl2_add_effect` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddEffectInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 付与先オブジェクトのセレクター。
    pub object: ObjectSelectorInput,
    /// 付与する effect 名。aviutl2_list_available_effects が返す名前を指定する。
    #[schemars(length(max = MAX_NAME_CHARS))]
    pub effect_name: String,
    /// 前提条件。
    pub expected: ExpectedInput,
}

impl AddEffectInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<AddEffectParams, ErrorObject> {
        let params = AddEffectParams {
            object: self.object.to_selector()?,
            effect_name: self.effect_name.clone(),
            expected: self.expected.to_expected()?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `aviutl2_delete_effect` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteEffectInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象 effect のセレクター。
    pub selector: EffectSelectorInput,
    /// 前提条件。
    pub expected: ExpectedInput,
}

impl DeleteEffectInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<DeleteEffectParams, ErrorObject> {
        let params = DeleteEffectParams {
            selector: self.selector.to_selector()?,
            expected: self.expected.to_expected()?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `aviutl2_set_effect_state` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetEffectStateInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象 effect のセレクター。
    pub selector: EffectSelectorInput,
    /// 有効・無効。省略時は変更しない。
    #[serde(default)]
    pub enabled: Option<bool>,
    /// ロック状態。省略時は変更しない。
    #[serde(default)]
    pub locked: Option<bool>,
    /// 前提条件。
    pub expected: ExpectedInput,
}

impl SetEffectStateInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<SetEffectStateParams, ErrorObject> {
        let params = SetEffectStateParams {
            selector: self.selector.to_selector()?,
            enabled: self.enabled,
            locked: self.locked,
            expected: self.expected.to_expected()?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `aviutl2_set_selection` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetSelectionInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 現在シーンの一致確認に使うシーン ID。
    pub expected_scene_id: i32,
    /// カーソル位置。省略時は変更しない。
    #[serde(default)]
    pub cursor: Option<CursorPositionInput>,
    /// 選択範囲。省略時は変更しない。
    #[serde(default)]
    pub selected_range: Option<RangeChangeInput>,
    /// フォーカス対象。省略時は変更しない。
    #[serde(default)]
    pub focus: Option<FocusChangeInput>,
    /// 前提条件。
    pub expected: ExpectedInput,
}

impl SetSelectionInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<SetSelectionParams, ErrorObject> {
        let focus = self
            .focus
            .as_ref()
            .map(FocusChangeInput::to_change)
            .transpose()?;
        let params = SetSelectionParams {
            expected_scene_id: self.expected_scene_id,
            cursor: self.cursor.map(CursorPositionInput::to_position),
            selected_range: self.selected_range.map(RangeChangeInput::to_change),
            focus,
            expected: self.expected.to_expected()?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// core の入力検証の失敗を tool result のエラーへ写す。
///
/// 説明には検証に失敗した値そのものを含めない。過大な入力をそのまま応答へ
/// 写すと、入力の誤りを伝える応答自体が過大になる。
fn from_input_error(error: EditInputError) -> ErrorObject {
    from_code(error.error_code(), error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::{ErrorCode, ObjectFingerprintInput, ObjectSummary};
    use serde_json::{Value, json};

    const SAMPLE_ID: &str = "8df98c04-e7c2-4f98-b3ce-fc1c39d76414";
    const SAMPLE_EPOCH: &str = "78be92d1-c8c9-44c6-ae52-387548971468";

    fn sample_summary() -> ObjectSummary {
        ObjectSummary::new(
            SAMPLE_EPOCH,
            ObjectFingerprintInput {
                scene_id: 3,
                layer: 2,
                frame_start: 120,
                frame_end: 240,
                name: Some("立ち絵"),
                alias: "alias",
            },
        )
    }

    fn object_selector_json() -> Value {
        serde_json::to_value(sample_summary().selector).expect("直列化できる")
    }

    fn effect_selector_json() -> Value {
        json!({
            "object": object_selector_json(),
            "effect_name": "動画ファイル",
            "effect_index": 0,
            "fingerprint": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "fingerprint_algorithm": "sha256-raw-v1",
        })
    }

    fn expected_json() -> Value {
        json!({ "project_epoch": SAMPLE_EPOCH, "project_revision": 42 })
    }

    #[test]
    fn expected_is_required_on_every_edit_input() {
        // 前提条件の省略を認めると、対象の世代を確かめずに変更が通ってしまう。
        let cases: Vec<Value> = vec![
            json!({ "instance_id": SAMPLE_ID, "source": { "type": "object_alias", "alias": "a" }, "placement": { "scene_id": 3, "layer": 1, "frame": 0 } }),
            json!({ "instance_id": SAMPLE_ID, "selector": object_selector_json(), "destination": { "layer": 1, "frame": 0 } }),
            json!({ "instance_id": SAMPLE_ID, "selector": object_selector_json() }),
            json!({ "instance_id": SAMPLE_ID, "selector": object_selector_json(), "name": "名前" }),
            json!({ "instance_id": SAMPLE_ID, "selector": effect_selector_json(), "item": "X", "value": { "type": "integer", "value": 1 } }),
            json!({ "instance_id": SAMPLE_ID, "object": object_selector_json(), "effect_name": "ぼかし" }),
            json!({ "instance_id": SAMPLE_ID, "selector": effect_selector_json() }),
            json!({ "instance_id": SAMPLE_ID, "selector": effect_selector_json(), "enabled": true }),
            json!({ "instance_id": SAMPLE_ID, "expected_scene_id": 3, "cursor": { "layer": 1, "frame": 2 } }),
        ];
        assert!(serde_json::from_value::<CreateObjectInput>(cases[0].clone()).is_err());
        assert!(serde_json::from_value::<MoveObjectInput>(cases[1].clone()).is_err());
        assert!(serde_json::from_value::<DeleteObjectInput>(cases[2].clone()).is_err());
        assert!(serde_json::from_value::<SetObjectNameInput>(cases[3].clone()).is_err());
        assert!(serde_json::from_value::<SetObjectItemInput>(cases[4].clone()).is_err());
        assert!(serde_json::from_value::<AddEffectInput>(cases[5].clone()).is_err());
        assert!(serde_json::from_value::<DeleteEffectInput>(cases[6].clone()).is_err());
        assert!(serde_json::from_value::<SetEffectStateInput>(cases[7].clone()).is_err());
        assert!(serde_json::from_value::<SetSelectionInput>(cases[8].clone()).is_err());
    }

    #[test]
    fn edit_inputs_reject_unknown_fields() {
        let mut create = json!({
            "instance_id": SAMPLE_ID,
            "source": { "type": "object_alias", "alias": "a" },
            "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
            "expected": expected_json(),
        });
        create["future"] = json!(1);
        assert!(serde_json::from_value::<CreateObjectInput>(create).is_err());

        let mut selection = json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "cursor": { "layer": 1, "frame": 2, "future": 1 },
            "expected": expected_json(),
        });
        assert!(
            serde_json::from_value::<SetSelectionInput>(selection.clone()).is_err(),
            "入れ子の未知フィールドが受理されました"
        );

        selection["cursor"] = json!({ "layer": 1, "frame": 2 });
        selection["expected"] =
            json!({ "project_epoch": SAMPLE_EPOCH, "project_revision": 42, "future": 1 });
        assert!(
            serde_json::from_value::<SetSelectionInput>(selection).is_err(),
            "expected の未知フィールドが受理されました"
        );
    }

    #[test]
    fn selectors_accept_unknown_fields() {
        // 応答へフィールドが増えても、返された selector をそのまま渡せる。
        let mut object = object_selector_json();
        object["future"] = json!(1);
        let mut selector = effect_selector_json();
        selector["object"] = object;
        selector["future"] = json!(1);

        let input: DeleteEffectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": selector,
            "expected": expected_json(),
        }))
        .expect("未知フィールドを含む selector を受理する");

        let params = input.to_params().expect("params へ変換できる");
        assert_eq!(params.selector.effect_name, "動画ファイル");
        assert_eq!(params.selector.effect_index, 0);
        assert_eq!(params.selector.object.layer, 2);
        assert_eq!(params.selector.object.frame, 120);
    }

    #[test]
    fn effect_selector_rejects_malformed_fingerprint() {
        let mut selector = effect_selector_json();
        selector["fingerprint"] = json!("sha256:zzzz");
        let input: DeleteEffectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": selector,
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("書式違反は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn effect_selector_strings_are_bounded() {
        for (field, value) in [
            ("effect_name", "あ".repeat(MAX_NAME_CHARS as usize + 1)),
            (
                "fingerprint_algorithm",
                "x".repeat(MAX_ALGORITHM_CHARS as usize + 1),
            ),
        ] {
            let mut selector = effect_selector_json();
            selector[field] = json!(value);
            let input: DeleteEffectInput = serde_json::from_value(json!({
                "instance_id": SAMPLE_ID,
                "selector": selector,
                "expected": expected_json(),
            }))
            .expect("入力型としては受理される");
            let error = input
                .to_params()
                .err()
                .unwrap_or_else(|| panic!("{field} の上限超過が受理されました"));
            assert_eq!(error.code, ErrorCode::InvalidArgument, "{field}");
        }
    }

    #[test]
    fn expected_epoch_is_bounded() {
        let input: DeleteObjectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": object_selector_json(),
            "expected": { "project_epoch": "e".repeat(MAX_EPOCH_CHARS as usize + 1), "project_revision": 1 },
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("上限超過は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn positions_beyond_the_declared_maximum_are_rejected() {
        // 宣言した上限は u32 の内側にあるため、入力型を通り抜けた値を検証で拒む。
        let over = MAX_POSITION + 1;

        let create: CreateObjectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "source": { "type": "object_alias", "alias": "a" },
            "placement": { "scene_id": 3, "layer": over, "frame": 0 },
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            create.to_params().expect_err("範囲外は拒否される").code,
            ErrorCode::InvalidArgument
        );

        let move_object: MoveObjectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": object_selector_json(),
            "destination": { "layer": 0, "frame": over },
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            move_object
                .to_params()
                .expect_err("範囲外は拒否される")
                .code,
            ErrorCode::InvalidArgument
        );

        let selection: SetSelectionInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "cursor": { "layer": 0, "frame": over },
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            selection.to_params().expect_err("範囲外は拒否される").code,
            ErrorCode::InvalidArgument
        );

        let range: SetSelectionInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "selected_range": { "type": "set", "start": 0, "end": over },
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            range.to_params().expect_err("範囲外は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn alias_over_the_limit_is_rejected() {
        let input: CreateObjectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "source": { "type": "object_alias", "alias": "a".repeat(MAX_ALIAS_CHARS as usize + 1) },
            "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("上限超過は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn media_path_syntax_is_validated_before_the_request_is_sent() {
        for path in [
            "",
            r"..\movie.mp4",
            r"\\.\PhysicalDrive0",
            r"C:\movie.mp4:stream",
            "movie.mp4",
        ] {
            let input: CreateObjectInput = serde_json::from_value(json!({
                "instance_id": SAMPLE_ID,
                "source": { "type": "media_file", "path": path },
                "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
                "expected": expected_json(),
            }))
            .expect("入力型としては受理される");
            let error = input
                .to_params()
                .err()
                .unwrap_or_else(|| panic!("{path} が受理されました"));
            assert_eq!(error.code, ErrorCode::InvalidArgument, "{path}");
        }
    }

    #[test]
    fn absolute_media_path_is_accepted() {
        let input: CreateObjectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "source": { "type": "media_file", "path": r"C:\movie.mp4" },
            "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        assert!(input.to_params().is_ok());
    }

    #[test]
    fn unknown_item_value_is_rejected_for_write() {
        let input: SetObjectItemInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": effect_selector_json(),
            "item": "テキスト",
            "value": { "type": "unknown", "raw": "opaque" },
            "expected": expected_json(),
        }))
        .expect("読み取りが返した形をそのまま受理する");
        assert_eq!(
            input.to_params().expect_err("未対応種別は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn item_value_strings_are_checked_for_length_and_control_characters() {
        for value in [
            json!({ "type": "text", "value": "a".repeat(MAX_ITEM_VALUE_CHARS as usize + 1) }),
            json!({ "type": "color", "value": "赤\u{1b}" }),
            json!({ "type": "file", "path": r"relative\path.png" }),
        ] {
            let input: SetObjectItemInput = serde_json::from_value(json!({
                "instance_id": SAMPLE_ID,
                "selector": effect_selector_json(),
                "item": "項目",
                "value": value,
                "expected": expected_json(),
            }))
            .expect("入力型としては受理される");
            assert!(input.to_params().is_err(), "{value} が受理されました");
        }
    }

    #[test]
    fn multiline_text_item_value_is_accepted() {
        // 読み取りが返した複数行のテキストを書き戻せる。
        let input: SetObjectItemInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": effect_selector_json(),
            "item": "テキスト",
            "value": { "type": "text", "value": "1 行目\n2 行目" },
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        assert!(input.to_params().is_ok());
    }

    /// 読み取りが返す設定値の標本を、種別を 1 つずつ辿って組み立てる。
    ///
    /// 次の種別は [`following`] の網羅 `match` が決める。手で並べた配列では、
    /// 種別が増えたときに更新しなくても通ってしまい、増えた種別が書き戻せない
    /// ことに気づけない。
    fn every_item_value() -> Vec<ItemValue> {
        let mut values = vec![ItemValue::Number {
            value: FiniteF64::try_new(1.5).expect("有限値"),
        }];
        while let Some(next) = following(values.last().expect("先頭がある")) {
            assert!(
                !values.iter().any(|value| value.kind() == next.kind()),
                "種別 {} を二度辿っています",
                next.kind()
            );
            values.push(next);
        }
        values
    }

    /// 標本で `previous` の次に置く値を返す。末尾では `None`。
    ///
    /// `_` を使わない網羅 `match` であるため、設定値へ種別が増えるとここが
    /// コンパイルエラーになる。腕を足すには増えた種別を標本の連なりへ繋ぐ
    /// 必要があり、書き戻せるかを確かめないまま通過できない。
    fn following(previous: &ItemValue) -> Option<ItemValue> {
        Some(match previous {
            ItemValue::Number { .. } => ItemValue::Integer { value: -3 },
            ItemValue::Integer { .. } => ItemValue::Bool { value: true },
            ItemValue::Bool { .. } => ItemValue::Color {
                value: "#ff8800".to_string(),
            },
            ItemValue::Color { .. } => ItemValue::Choice {
                value: "通常".to_string(),
                index: Some(2),
            },
            ItemValue::Choice { .. } => ItemValue::File {
                path: r"C:\movie.mp4".to_string(),
            },
            ItemValue::File { .. } => ItemValue::Folder {
                path: r"C:\assets".to_string(),
            },
            ItemValue::Folder { .. } => ItemValue::Font {
                name: "Meiryo".to_string(),
            },
            ItemValue::Font { .. } => ItemValue::Text {
                value: "字幕".to_string(),
            },
            ItemValue::Text { .. } => ItemValue::Unknown {
                raw: "future=1".to_string(),
            },
            ItemValue::Unknown { .. } => return None,
        })
    }

    /// 入力 schema が受け付ける種別名。
    fn declared_item_value_kinds() -> std::collections::BTreeSet<String> {
        let schema = serde_json::to_value(schemars::schema_for!(ItemValueInput))
            .expect("schema は直列化できる");
        schema["oneOf"]
            .as_array()
            .expect("種別ごとの分岐がある")
            .iter()
            .map(|branch| {
                branch["properties"]["type"]["const"]
                    .as_str()
                    .expect("判別子がある")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn every_read_item_value_round_trips_through_the_input_type() {
        // 読み取りが返す値の形と入力型がずれると、読めた値を書き戻せなくなる。
        let values = every_item_value();

        // 標本が入力 schema の全分岐を覆っていること。覆えていない種別は、
        // 読み取りが返しても書き戻せないまま素通りする。
        let covered: std::collections::BTreeSet<String> = values
            .iter()
            .map(|value| value.kind().to_string())
            .collect();
        assert_eq!(covered, declared_item_value_kinds());

        for value in values {
            let encoded = serde_json::to_value(&value).expect("直列化できる");
            let input: ItemValueInput =
                serde_json::from_value(encoded.clone()).unwrap_or_else(|e| {
                    panic!("読み取りの値を入力型で受け取れません: {encoded} ({e})")
                });
            assert_eq!(input.to_value().expect("変換できる"), value);
        }
    }

    #[test]
    fn item_value_input_accepts_unknown_fields() {
        // 設定値も selector と同じ往復型である。応答へ optional field が増えた
        // とき、読み取りが返した値をそのまま書き戻せなくなる非対称を作らない。
        let input: SetObjectItemInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": effect_selector_json(),
            "item": "種類",
            "value": { "type": "choice", "value": "通常", "index": 2, "future": 1 },
            "expected": expected_json(),
        }))
        .expect("未知フィールドを含む設定値を受理する");

        let params = input.to_params().expect("params へ変換できる");
        assert_eq!(
            params.value,
            ItemValue::Choice {
                value: "通常".to_string(),
                index: Some(2),
            },
            "既知フィールドが失われています"
        );
    }

    #[test]
    fn item_value_input_still_rejects_missing_fields_and_type_mismatch() {
        // 寛容にするのは余分な field だけである。
        for value in [
            json!({ "type": "integer" }),
            json!({ "type": "integer", "value": "1" }),
            json!({ "type": "vector", "x": 1 }),
        ] {
            assert!(
                serde_json::from_value::<ItemValueInput>(value.clone()).is_err(),
                "{value} が受理されました"
            );
        }
    }

    #[test]
    fn effect_state_requires_at_least_one_change() {
        let input: SetEffectStateInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": effect_selector_json(),
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("全省略は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn selection_requires_at_least_one_change() {
        let input: SetSelectionInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("全省略は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn selection_focus_selector_is_converted() {
        let input: SetSelectionInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "focus": { "type": "set", "object": object_selector_json() },
            "selected_range": { "type": "clear" },
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        let params = input.to_params().expect("params へ変換できる");
        assert!(matches!(
            params.focus,
            Some(aviutl2_mcp_core::FocusChange::Set { .. })
        ));
        assert_eq!(params.selected_range, Some(RangeChange::Clear {}));
    }

    #[test]
    fn item_and_effect_names_over_the_limit_are_rejected() {
        let too_long = "a".repeat(MAX_NAME_CHARS as usize + 1);

        let item: SetObjectItemInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": effect_selector_json(),
            "item": too_long,
            "value": { "type": "integer", "value": 1 },
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            item.to_params().expect_err("上限超過は拒否される").code,
            ErrorCode::InvalidArgument
        );

        let effect: AddEffectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "object": object_selector_json(),
            "effect_name": too_long,
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            effect.to_params().expect_err("上限超過は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn object_name_over_the_limit_is_rejected() {
        let input: SetObjectNameInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": object_selector_json(),
            "name": "a".repeat(MAX_NAME_CHARS as usize + 1),
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("上限超過は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn validation_errors_do_not_echo_the_input() {
        // 応答へ入力そのものを写すと、誤りを伝える応答自体が過大になる。
        let input: CreateObjectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "source": { "type": "media_file", "path": r"C:\secret\movie.mp4:stream" },
            "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
            "expected": expected_json(),
        }))
        .expect("入力型としては受理される");
        let error = input.to_params().expect_err("拒否される");
        assert!(!error.message.contains("secret"), "{}", error.message);
    }
}
