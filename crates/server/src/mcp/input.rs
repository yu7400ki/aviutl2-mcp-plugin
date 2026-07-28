//! read tool の入力型。
//!
//! 未知フィールドを拒否し、文字列長・整数範囲・配列長を schema で制約する。
//! ページ指定は IPC の params と同じ平坦な形（`offset` / `limit` /
//! `snapshot_revision`）で受け取る。

use crate::mcp::failure::invalid_argument;
use aviutl2_mcp_core::{
    DEFAULT_PAGE_LIMIT, EffectType, ErrorObject, GetObjectParams, InstanceId,
    ListAvailableEffectsParams, ListLayersParams, ListObjectsParams, MAX_PAGE_LIMIT, ObjectFilter,
    ObjectSelector, PageRequest,
};
use schemars::JsonSchema;
use serde::Deserialize;

/// `instance_id` が満たすべき UUID の書式。
const UUID_PATTERN: &str =
    r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$";

/// fingerprint が満たすべき書式。
const FINGERPRINT_PATTERN: &str = r"^sha256:[0-9a-f]{64}$";

/// オブジェクト名・レイヤー名に許す最大文字数。
const MAX_NAME_CHARS: u32 = 1_024;

/// fingerprint 算出方式名に許す最大文字数。
const MAX_ALGORITHM_CHARS: u32 = 64;

/// プロジェクト epoch に許す最大文字数。
const MAX_EPOCH_CHARS: u32 = 64;

fn default_limit() -> u32 {
    DEFAULT_PAGE_LIMIT
}

/// `aviutl2_list_instances` の入力。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListInstancesInput {
    /// 取得を開始する 0 始まりの位置。
    #[serde(default)]
    pub offset: u32,
    /// 取得件数。
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: u32,
}

/// インスタンスを 1 つ指定するだけの入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstanceInput {
    /// 対象インスタンスの ID。aviutl2_list_instances が返す値を指定する。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
}

/// ページ指定。IPC の params と同じ平坦な形で受け取る。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PageInput {
    /// 取得を開始する 0 始まりの位置。
    #[serde(default)]
    pub offset: u32,
    /// 取得件数。
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: u32,
    /// 先頭ページが返した snapshot_revision。指定すると一致しない場合に precondition_failed となる。
    #[serde(default)]
    pub snapshot_revision: Option<u64>,
}

impl PageInput {
    /// 共通のページ要求へ変換する。
    fn to_page_request(self) -> Result<PageRequest, ErrorObject> {
        let request = PageRequest {
            offset: self.offset,
            limit: self.limit,
            snapshot_revision: self.snapshot_revision,
        };
        request.validate().map_err(|_| {
            invalid_argument(format!(
                "limit は 1 以上 {MAX_PAGE_LIMIT} 以下である必要があります"
            ))
        })?;
        Ok(request)
    }
}

/// `aviutl2_list_layers` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListLayersInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 列挙対象が現在シーンのままであることを確認するためのシーン ID。
    pub expected_scene_id: i32,
    /// ページ指定。
    #[serde(flatten)]
    pub page: PageInput,
}

/// `aviutl2_list_objects` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListObjectsInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 列挙対象が現在シーンのままであることを確認するためのシーン ID。
    pub expected_scene_id: i32,
    /// レイヤー範囲による絞り込み。
    #[serde(default)]
    pub filter: Option<ObjectFilterInput>,
    /// ページ指定。
    #[serde(flatten)]
    pub page: PageInput,
}

/// オブジェクト列挙の絞り込み条件。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectFilterInput {
    /// 対象とする最小のレイヤー番号。0 始まり。
    #[serde(default)]
    pub layer_min: Option<u32>,
    /// 対象とする最大のレイヤー番号。0 始まり。
    #[serde(default)]
    pub layer_max: Option<u32>,
}

/// `aviutl2_get_object` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetObjectInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象オブジェクトのセレクター。aviutl2_list_objects が返した値をそのまま指定する。
    pub selector: ObjectSelectorInput,
}

/// オブジェクトを再指定するセレクター。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectSelectorInput {
    /// 応答が返したプロジェクトの epoch。
    #[schemars(length(min = 1, max = MAX_EPOCH_CHARS))]
    pub project_epoch: String,
    /// 読み取り時と同じシーンかを確認するためのシーン ID。
    pub scene_id: i32,
    /// 0 始まりのレイヤー番号。
    pub layer: u32,
    /// 0 始まりの開始フレーム番号。
    pub frame: u32,
    /// オブジェクト名。標準名のままなら null。
    #[serde(default)]
    #[schemars(length(max = MAX_NAME_CHARS))]
    pub name: Option<String>,
    /// 同一性検証用の fingerprint。
    #[schemars(pattern(FINGERPRINT_PATTERN))]
    pub fingerprint: String,
    /// fingerprint の算出方式。
    #[schemars(length(min = 1, max = MAX_ALGORITHM_CHARS))]
    pub fingerprint_algorithm: String,
}

/// `aviutl2_list_available_effects` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListAvailableEffectsInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 種別による絞り込み。
    #[serde(default)]
    pub effect_type: Option<EffectTypeInput>,
    /// ページ指定。
    #[serde(flatten)]
    pub page: PageInput,
}

/// 絞り込みに指定できる effect の種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum EffectTypeInput {
    Filter,
    Input,
    Transition,
    Control,
    Output,
}

impl From<EffectTypeInput> for EffectType {
    fn from(value: EffectTypeInput) -> Self {
        match value {
            EffectTypeInput::Filter => EffectType::Filter,
            EffectTypeInput::Input => EffectType::Input,
            EffectTypeInput::Transition => EffectType::Transition,
            EffectTypeInput::Control => EffectType::Control,
            EffectTypeInput::Output => EffectType::Output,
        }
    }
}

/// `instance_id` 文字列を識別子へ変換する。
pub fn parse_instance_id(value: &str) -> Result<InstanceId, ErrorObject> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .map_err(|_| invalid_argument("instance_id は UUID である必要があります"))
}

impl ListInstancesInput {
    /// ページ要求へ変換する。
    pub fn to_page_request(self) -> Result<PageRequest, ErrorObject> {
        PageInput {
            offset: self.offset,
            limit: self.limit,
            snapshot_revision: None,
        }
        .to_page_request()
    }
}

impl ListLayersInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<ListLayersParams, ErrorObject> {
        Ok(ListLayersParams {
            expected_scene_id: self.expected_scene_id,
            page: self.page.to_page_request()?,
        })
    }
}

impl ListObjectsInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<ListObjectsParams, ErrorObject> {
        let filter = self.filter.map(ObjectFilterInput::to_filter).transpose()?;
        Ok(ListObjectsParams {
            expected_scene_id: self.expected_scene_id,
            filter,
            page: self.page.to_page_request()?,
        })
    }
}

impl ObjectFilterInput {
    /// 絞り込み条件へ変換し、範囲の整合を検証する。
    fn to_filter(self) -> Result<ObjectFilter, ErrorObject> {
        let filter = ObjectFilter {
            layer_min: self.layer_min.map(|v| v as usize),
            layer_max: self.layer_max.map(|v| v as usize),
        };
        filter
            .validate()
            .map_err(|e| invalid_argument(e.to_string()))?;
        Ok(filter)
    }
}

impl GetObjectInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<GetObjectParams, ErrorObject> {
        Ok(GetObjectParams {
            selector: self.selector.to_selector()?,
        })
    }
}

impl ObjectSelectorInput {
    /// セレクターへ変換する。fingerprint の書式はここで検証される。
    fn to_selector(&self) -> Result<ObjectSelector, ErrorObject> {
        let value = serde_json::json!({
            "project_epoch": self.project_epoch,
            "scene_id": self.scene_id,
            "layer": self.layer,
            "frame": self.frame,
            "name": self.name,
            "fingerprint": self.fingerprint,
            "fingerprint_algorithm": self.fingerprint_algorithm,
        });
        serde_json::from_value(value).map_err(|_| {
            invalid_argument(
                "selector を解釈できません。aviutl2_list_objects が返した値をそのまま指定してください",
            )
        })
    }
}

impl ListAvailableEffectsInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<ListAvailableEffectsParams, ErrorObject> {
        Ok(ListAvailableEffectsParams {
            effect_type: self.effect_type.map(EffectType::from),
            page: self.page.to_page_request()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::ErrorCode;

    const SAMPLE_ID: &str = "8df98c04-e7c2-4f98-b3ce-fc1c39d76414";
    const SAMPLE_FINGERPRINT: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn selector_json() -> serde_json::Value {
        serde_json::json!({
            "project_epoch": "78be92d1-c8c9-44c6-ae52-387548971468",
            "scene_id": 0,
            "layer": 2,
            "frame": 120,
            "name": "立ち絵",
            "fingerprint": SAMPLE_FINGERPRINT,
            "fingerprint_algorithm": "sha256-raw-v1",
        })
    }

    #[test]
    fn list_instances_input_defaults_match_page_defaults() {
        let input: ListInstancesInput = serde_json::from_str("{}").expect("省略を受理する");
        assert_eq!(input.offset, 0);
        assert_eq!(input.limit, DEFAULT_PAGE_LIMIT);
    }

    #[test]
    fn list_instances_input_rejects_unknown_field() {
        assert!(serde_json::from_str::<ListInstancesInput>(r#"{"snapshot_revision":1}"#).is_err());
    }

    #[test]
    fn list_instances_input_rejects_limit_out_of_range() {
        for limit in [0, MAX_PAGE_LIMIT + 1] {
            let input = ListInstancesInput { offset: 0, limit };
            let error = input.to_page_request().expect_err("範囲外は拒否される");
            assert_eq!(error.code, ErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn page_fields_are_accepted_flat() {
        let input: ListLayersInput = serde_json::from_value(serde_json::json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "offset": 5,
            "limit": 10,
            "snapshot_revision": 7,
        }))
        .expect("平坦なページ指定を受理する");
        let params = input.to_params().expect("params へ変換できる");
        assert_eq!(params.expected_scene_id, 3);
        assert_eq!(params.page.offset, 5);
        assert_eq!(params.page.limit, 10);
        assert_eq!(params.page.snapshot_revision, Some(7));
    }

    #[test]
    fn list_layers_input_rejects_unknown_field() {
        assert!(
            serde_json::from_value::<ListLayersInput>(serde_json::json!({
                "instance_id": SAMPLE_ID,
                "expected_scene_id": 0,
                "future": 1,
            }))
            .is_err()
        );
    }

    #[test]
    fn list_layers_input_requires_instance_id() {
        assert!(
            serde_json::from_value::<ListLayersInput>(
                serde_json::json!({ "expected_scene_id": 0 })
            )
            .is_err()
        );
    }

    #[test]
    fn instance_input_requires_instance_id() {
        assert!(serde_json::from_str::<InstanceInput>("{}").is_err());
    }

    #[test]
    fn instance_id_must_be_uuid() {
        assert!(parse_instance_id("not-a-uuid").is_err());
        assert!(parse_instance_id(SAMPLE_ID).is_ok());
    }

    #[test]
    fn object_filter_rejects_inverted_range() {
        let input = ListObjectsInput {
            instance_id: SAMPLE_ID.to_string(),
            expected_scene_id: 0,
            filter: Some(ObjectFilterInput {
                layer_min: Some(8),
                layer_max: Some(1),
            }),
            page: PageInput {
                offset: 0,
                limit: DEFAULT_PAGE_LIMIT,
                snapshot_revision: None,
            },
        };
        let error = input.to_params().expect_err("逆転した範囲は拒否される");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn object_filter_rejects_unknown_field() {
        assert!(
            serde_json::from_value::<ObjectFilterInput>(serde_json::json!({ "layer": 1 })).is_err()
        );
    }

    #[test]
    fn get_object_input_converts_selector() {
        let input: GetObjectInput = serde_json::from_value(serde_json::json!({
            "instance_id": SAMPLE_ID,
            "selector": selector_json(),
        }))
        .expect("セレクターを受理する");
        let params = input.to_params().expect("params へ変換できる");
        assert_eq!(params.selector.layer, 2);
        assert_eq!(params.selector.frame, 120);
        assert_eq!(params.selector.fingerprint.as_str(), SAMPLE_FINGERPRINT);
    }

    #[test]
    fn get_object_input_rejects_malformed_fingerprint() {
        let mut selector = selector_json();
        selector["fingerprint"] = serde_json::json!("sha256:zzzz");
        let input = GetObjectInput {
            instance_id: SAMPLE_ID.to_string(),
            selector: serde_json::from_value(selector).expect("入力型としては受理される"),
        };
        let error = input
            .to_params()
            .expect_err("書式違反の fingerprint は拒否される");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn get_object_input_rejects_unknown_selector_field() {
        let mut selector = selector_json();
        selector["future"] = serde_json::json!(1);
        assert!(
            serde_json::from_value::<GetObjectInput>(serde_json::json!({
                "instance_id": SAMPLE_ID,
                "selector": selector,
            }))
            .is_err()
        );
    }

    #[test]
    fn effect_type_input_maps_to_core_type() {
        let input: ListAvailableEffectsInput = serde_json::from_value(serde_json::json!({
            "instance_id": SAMPLE_ID,
            "effect_type": "filter",
        }))
        .expect("種別名を受理する");
        let params = input.to_params().expect("params へ変換できる");
        assert_eq!(params.effect_type, Some(EffectType::Filter));
    }

    #[test]
    fn effect_type_input_rejects_unknown_value() {
        assert!(
            serde_json::from_value::<ListAvailableEffectsInput>(serde_json::json!({
                "instance_id": SAMPLE_ID,
                "effect_type": "unknown",
            }))
            .is_err()
        );
    }
}
