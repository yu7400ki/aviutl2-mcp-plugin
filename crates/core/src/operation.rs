//! 読み取り operation の名前と params / result。

use crate::edit_info::SceneInfo;
use crate::effect::{AvailableEffect, EffectType};
use crate::object::{LayerInfo, ObjectSummary};
use crate::page::{PageMeta, PageRequest};
use crate::selector::ObjectSelector;
use serde::{Deserialize, Serialize};

/// 現在の編集情報を取得する operation 名。
pub const OPERATION_GET_EDIT_INFO: &str = "get_edit_info";

/// 現在シーンを取得する operation 名。
pub const OPERATION_GET_CURRENT_SCENE: &str = "get_current_scene";

/// 現在シーンのレイヤーを列挙する operation 名。
pub const OPERATION_LIST_LAYERS: &str = "list_layers";

/// 現在シーンのオブジェクトを列挙する operation 名。
pub const OPERATION_LIST_OBJECTS: &str = "list_objects";

/// オブジェクトの詳細を取得する operation 名。
pub const OPERATION_GET_OBJECT: &str = "get_object";

/// 利用可能な effect を列挙する operation 名。
pub const OPERATION_LIST_AVAILABLE_EFFECTS: &str = "list_available_effects";

/// `get_edit_info` の params。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetEditInfoParams {}

/// `get_current_scene` の params。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetCurrentSceneParams {}

/// `list_layers` の params。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListLayersParams {
    /// 列挙時と同じシーンかを確認するための guard。
    pub expected_scene_id: i32,
    /// ページ指定。
    #[serde(default)]
    pub page: PageRequest,
}

/// `list_objects` の params。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListObjectsParams {
    /// 列挙時と同じシーンかを確認するための guard。
    pub expected_scene_id: i32,
    /// 絞り込み条件。
    #[serde(default)]
    pub filter: Option<ObjectFilter>,
    /// ページ指定。
    #[serde(default)]
    pub page: PageRequest,
}

/// オブジェクト列挙の絞り込み条件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectFilter {
    /// 対象とする最小レイヤー番号。0 始まり。
    #[serde(default)]
    pub layer_min: Option<usize>,
    /// 対象とする最大レイヤー番号。0 始まり。
    #[serde(default)]
    pub layer_max: Option<usize>,
}

/// `get_object` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetObjectParams {
    /// 対象オブジェクトのセレクター。
    pub selector: ObjectSelector,
}

/// `list_available_effects` の params。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListAvailableEffectsParams {
    /// 種別で絞り込む。
    #[serde(default)]
    pub effect_type: Option<EffectType>,
    /// ページ指定。
    #[serde(default)]
    pub page: PageRequest,
}

/// `get_current_scene` の result。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetCurrentSceneResult {
    /// 現在シーンの情報。
    pub scene: SceneInfo,
    /// 取得時点のプロジェクト revision。
    pub project_revision: u64,
}

/// `list_layers` の result。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListLayersResult {
    /// 切り出されたページの要素。
    pub items: Vec<LayerInfo>,
    /// ページのメタ情報。
    pub page: PageMeta,
}

/// `list_objects` の result。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListObjectsResult {
    /// 切り出されたページの要素。
    pub items: Vec<ObjectSummary>,
    /// ページのメタ情報。
    pub page: PageMeta,
}

/// `list_available_effects` の result。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListAvailableEffectsResult {
    /// 切り出されたページの要素。
    pub items: Vec<AvailableEffect>,
    /// ページのメタ情報。
    pub page: PageMeta,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{AvailableEffectItem, EffectFlags, EffectItemType};
    use crate::fingerprint::{FingerprintAlgorithm, object_fingerprint};
    use crate::number::FiniteF64;
    use crate::page::DEFAULT_PAGE_LIMIT;

    fn sample_object_selector() -> ObjectSelector {
        ObjectSelector {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            scene_id: 0,
            layer: 2,
            frame: 120,
            name: Some("立ち絵".to_string()),
            fingerprint: object_fingerprint(
                &FingerprintAlgorithm::RawV1,
                0,
                2,
                120,
                240,
                Some("立ち絵"),
                "alias",
            ),
            fingerprint_algorithm: FingerprintAlgorithm::RawV1,
        }
    }

    fn sample_page_meta() -> PageMeta {
        PageMeta {
            total_count: 3,
            count: 1,
            offset: 0,
            has_more: true,
            next_offset: Some(1),
            snapshot_revision: 42,
        }
    }

    #[test]
    fn operation_names_are_snake_case() {
        assert_eq!(OPERATION_GET_EDIT_INFO, "get_edit_info");
        assert_eq!(OPERATION_GET_CURRENT_SCENE, "get_current_scene");
        assert_eq!(OPERATION_LIST_LAYERS, "list_layers");
        assert_eq!(OPERATION_LIST_OBJECTS, "list_objects");
        assert_eq!(OPERATION_GET_OBJECT, "get_object");
        assert_eq!(OPERATION_LIST_AVAILABLE_EFFECTS, "list_available_effects");
    }

    #[test]
    fn params_without_fields_accept_empty_object() {
        assert_eq!(
            serde_json::from_str::<GetEditInfoParams>("{}").unwrap(),
            GetEditInfoParams {}
        );
        assert_eq!(
            serde_json::from_str::<GetCurrentSceneParams>("{}").unwrap(),
            GetCurrentSceneParams {}
        );
        assert_eq!(serde_json::to_string(&GetEditInfoParams {}).unwrap(), "{}");
    }

    #[test]
    fn params_without_fields_reject_unknown_field() {
        assert!(serde_json::from_str::<GetEditInfoParams>(r#"{"future":1}"#).is_err());
        assert!(serde_json::from_str::<GetCurrentSceneParams>(r#"{"future":1}"#).is_err());
    }

    #[test]
    fn list_layers_params_roundtrip() {
        let params = ListLayersParams {
            expected_scene_id: 0,
            page: PageRequest {
                offset: 10,
                limit: 20,
                snapshot_revision: Some(42),
            },
        };
        let s = serde_json::to_string(&params).unwrap();
        let restored: ListLayersParams = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, params);
    }

    #[test]
    fn list_layers_params_defaults_page() {
        let params: ListLayersParams = serde_json::from_str(r#"{"expected_scene_id":0}"#).unwrap();
        assert_eq!(params.page.limit, DEFAULT_PAGE_LIMIT);
        assert_eq!(params.page.offset, 0);
        assert_eq!(params.page.snapshot_revision, None);
    }

    #[test]
    fn list_layers_params_reject_unknown_field() {
        // ページ指定は入れ子であり、外側にも内側にも未知フィールドを許さない。
        assert!(
            serde_json::from_str::<ListLayersParams>(r#"{"expected_scene_id":0,"limit":10}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<ListLayersParams>(
                r#"{"expected_scene_id":0,"page":{"limit":10,"future":1}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn list_layers_params_require_expected_scene_id() {
        assert!(serde_json::from_str::<ListLayersParams>("{}").is_err());
    }

    #[test]
    fn list_objects_params_roundtrip() {
        let params = ListObjectsParams {
            expected_scene_id: 3,
            filter: Some(ObjectFilter {
                layer_min: Some(1),
                layer_max: Some(8),
            }),
            page: PageRequest::default(),
        };
        let s = serde_json::to_string(&params).unwrap();
        let restored: ListObjectsParams = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, params);
    }

    #[test]
    fn list_objects_params_defaults_filter_to_none() {
        let params: ListObjectsParams = serde_json::from_str(r#"{"expected_scene_id":0}"#).unwrap();
        assert_eq!(params.filter, None);
    }

    #[test]
    fn object_filter_rejects_unknown_field() {
        assert!(serde_json::from_str::<ObjectFilter>(r#"{"name":"x"}"#).is_err());
    }

    #[test]
    fn list_available_effects_params_roundtrip() {
        for effect_type in [
            None,
            Some(EffectType::Filter),
            Some(EffectType::Unknown(42)),
        ] {
            let params = ListAvailableEffectsParams {
                effect_type,
                page: PageRequest::default(),
            };
            let s = serde_json::to_string(&params).unwrap();
            let restored: ListAvailableEffectsParams = serde_json::from_str(&s).unwrap();
            assert_eq!(restored, params);
        }
    }

    #[test]
    fn list_available_effects_params_reject_unknown_field() {
        assert!(serde_json::from_str::<ListAvailableEffectsParams>(r#"{"offset":0}"#).is_err());
    }

    #[test]
    fn get_object_params_roundtrip() {
        let params = GetObjectParams {
            selector: sample_object_selector(),
        };
        let s = serde_json::to_string(&params).unwrap();
        let restored: GetObjectParams = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, params);
    }

    #[test]
    fn get_object_params_reject_unknown_field() {
        let mut value = serde_json::to_value(GetObjectParams {
            selector: sample_object_selector(),
        })
        .unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::json!(1));
        assert!(serde_json::from_value::<GetObjectParams>(value).is_err());
    }

    #[test]
    fn get_current_scene_result_roundtrip() {
        let result = GetCurrentSceneResult {
            scene: SceneInfo {
                id: 0,
                name: Some("Scene 1".to_string()),
                width: 1920,
                height: 1080,
                fps: FiniteF64::try_new(60.0),
                fps_rate: 60,
                fps_scale: 1,
                sample_rate: 48000,
            },
            project_revision: 42,
        };
        let s = serde_json::to_string(&result).unwrap();
        let restored: GetCurrentSceneResult = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, result);
    }

    #[test]
    fn list_layers_result_roundtrip() {
        let result = ListLayersResult {
            items: vec![LayerInfo {
                index: 0,
                name: Some("背景".to_string()),
                enabled: true,
                locked: false,
                object_count: 2,
            }],
            page: sample_page_meta(),
        };
        let s = serde_json::to_string(&result).unwrap();
        let restored: ListLayersResult = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, result);
    }

    #[test]
    fn list_objects_result_roundtrip() {
        let result = ListObjectsResult {
            items: vec![ObjectSummary {
                layer: 2,
                frame_start: 120,
                frame_end: 240,
                name: Some("立ち絵".to_string()),
                fingerprint: sample_object_selector().fingerprint.clone(),
                selector: sample_object_selector(),
            }],
            page: sample_page_meta(),
        };
        let s = serde_json::to_string(&result).unwrap();
        let restored: ListObjectsResult = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, result);
    }

    #[test]
    fn list_available_effects_result_roundtrip() {
        let result = ListAvailableEffectsResult {
            items: vec![AvailableEffect {
                name: "ぼかし".to_string(),
                effect_type: EffectType::Filter,
                flags: EffectFlags::from_raw(9),
                items: vec![AvailableEffectItem {
                    name: "範囲".to_string(),
                    item_type: EffectItemType::Integer,
                }],
            }],
            page: sample_page_meta(),
        };
        let s = serde_json::to_string(&result).unwrap();
        let restored: ListAvailableEffectsResult = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, result);
    }

    #[test]
    fn results_allow_unknown_optional_fields() {
        let mut value = serde_json::to_value(ListLayersResult {
            items: Vec::new(),
            page: sample_page_meta(),
        })
        .unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::json!(1));
        let restored: ListLayersResult = serde_json::from_value(value).unwrap();
        assert!(restored.items.is_empty());
    }
}
