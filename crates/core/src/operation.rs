//! read operation の名前と params / result、および編集 operation の名前。
//!
//! 編集 operation の params / result 型は本モジュールでは定義しない。

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

/// media file / alias からオブジェクトを作成する operation 名。
pub const OPERATION_CREATE_OBJECT: &str = "create_object";

/// オブジェクトのレイヤーと開始フレームを変更する operation 名。
pub const OPERATION_MOVE_OBJECT: &str = "move_object";

/// オブジェクトを削除する operation 名。
pub const OPERATION_DELETE_OBJECT: &str = "delete_object";

/// オブジェクト名を変更する operation 名。
pub const OPERATION_SET_OBJECT_NAME: &str = "set_object_name";

/// オブジェクトの設定項目・track 値を変更する operation 名。
pub const OPERATION_SET_OBJECT_ITEM: &str = "set_object_item";

/// オブジェクトへ effect を付与する operation 名。
pub const OPERATION_ADD_EFFECT: &str = "add_effect";

/// オブジェクトから effect を削除する operation 名。
pub const OPERATION_DELETE_EFFECT: &str = "delete_effect";

/// effect の有効・ロック状態を変更する operation 名。
pub const OPERATION_SET_EFFECT_STATE: &str = "set_effect_state";

/// カーソル・選択範囲・フォーカスを変更する operation 名。
pub const OPERATION_SET_SELECTION: &str = "set_selection";

/// 編集 operation の種別。
///
/// 編集 operation の名前一覧はこの型へ一本化する。文字列表現は
/// [`EditOperation::as_str`]、名前からの解決は
/// [`EditOperation::from_operation_name`]、全 variant は [`EditOperation::ALL`]
/// で得られる。read/edit を分岐する必要がある処理（要求予算の選択、
/// operation の dispatch など）は、operation 名の一覧を個別に持たず、この型を
/// 経由して判定する。そうすることで、新しい編集 operation を追加する際に
/// 一部の判定処理だけへ足し忘れても、他の判定処理は追加前のまま動き続けて
/// しまう、という食い違いを構造的に防ぐ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOperation {
    /// [`OPERATION_CREATE_OBJECT`]。
    CreateObject,
    /// [`OPERATION_MOVE_OBJECT`]。
    MoveObject,
    /// [`OPERATION_DELETE_OBJECT`]。
    DeleteObject,
    /// [`OPERATION_SET_OBJECT_NAME`]。
    SetObjectName,
    /// [`OPERATION_SET_OBJECT_ITEM`]。
    SetObjectItem,
    /// [`OPERATION_ADD_EFFECT`]。
    AddEffect,
    /// [`OPERATION_DELETE_EFFECT`]。
    DeleteEffect,
    /// [`OPERATION_SET_EFFECT_STATE`]。
    SetEffectState,
    /// [`OPERATION_SET_SELECTION`]。
    SetSelection,
}

impl EditOperation {
    /// 全 variant。
    ///
    /// 要素数と内容は `edit_operation_all_is_exhaustive` テストで固定する。
    pub const ALL: [EditOperation; 9] = [
        EditOperation::CreateObject,
        EditOperation::MoveObject,
        EditOperation::DeleteObject,
        EditOperation::SetObjectName,
        EditOperation::SetObjectItem,
        EditOperation::AddEffect,
        EditOperation::DeleteEffect,
        EditOperation::SetEffectState,
        EditOperation::SetSelection,
    ];

    /// operation 名の文字列表現を返す。
    pub const fn as_str(self) -> &'static str {
        match self {
            EditOperation::CreateObject => OPERATION_CREATE_OBJECT,
            EditOperation::MoveObject => OPERATION_MOVE_OBJECT,
            EditOperation::DeleteObject => OPERATION_DELETE_OBJECT,
            EditOperation::SetObjectName => OPERATION_SET_OBJECT_NAME,
            EditOperation::SetObjectItem => OPERATION_SET_OBJECT_ITEM,
            EditOperation::AddEffect => OPERATION_ADD_EFFECT,
            EditOperation::DeleteEffect => OPERATION_DELETE_EFFECT,
            EditOperation::SetEffectState => OPERATION_SET_EFFECT_STATE,
            EditOperation::SetSelection => OPERATION_SET_SELECTION,
        }
    }

    /// operation 名から variant を引く。編集 operation でなければ `None`。
    ///
    /// [`EditOperation::ALL`] を線形探索するだけであり、一覧を別に持たない。
    pub fn from_operation_name(name: &str) -> Option<Self> {
        EditOperation::ALL
            .into_iter()
            .find(|op| op.as_str() == name)
    }
}

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
    /// ページ指定。要求では offset / limit / snapshot_revision として展開される。
    #[serde(flatten)]
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
    /// ページ指定。要求では offset / limit / snapshot_revision として展開される。
    #[serde(flatten)]
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

impl ObjectFilter {
    /// レイヤー範囲の整合を検証する。
    ///
    /// 空集合になる指定は、結果 0 件と区別できるよう要求の誤りとして扱う。
    pub fn validate(&self) -> Result<(), ObjectFilterError> {
        if let (Some(min), Some(max)) = (self.layer_min, self.layer_max)
            && min > max
        {
            return Err(ObjectFilterError::InvertedLayerRange { min, max });
        }
        Ok(())
    }
}

/// 絞り込み条件の検証失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ObjectFilterError {
    /// `layer_min` が `layer_max` を上回っている。
    #[error("layer_min は layer_max 以下である必要があります: {min} > {max}")]
    InvertedLayerRange {
        /// 指定された最小レイヤー番号。
        min: usize,
        /// 指定された最大レイヤー番号。
        max: usize,
    },
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
    /// ページ指定。要求では offset / limit / snapshot_revision として展開される。
    #[serde(flatten)]
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
    use crate::fingerprint::ObjectFingerprintInput;
    use crate::number::FiniteF64;
    use crate::object::ObjectSummary;
    use crate::page::DEFAULT_PAGE_LIMIT;

    fn sample_object_summary() -> ObjectSummary {
        ObjectSummary::new(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            ObjectFingerprintInput {
                scene_id: 0,
                layer: 2,
                frame_start: 120,
                frame_end: 240,
                name: Some("立ち絵"),
                alias: "alias",
                effect_fingerprints: &[],
            },
        )
    }

    fn sample_object_selector() -> ObjectSelector {
        sample_object_summary().selector
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
    fn edit_operation_names_are_snake_case() {
        assert_eq!(OPERATION_CREATE_OBJECT, "create_object");
        assert_eq!(OPERATION_MOVE_OBJECT, "move_object");
        assert_eq!(OPERATION_DELETE_OBJECT, "delete_object");
        assert_eq!(OPERATION_SET_OBJECT_NAME, "set_object_name");
        assert_eq!(OPERATION_SET_OBJECT_ITEM, "set_object_item");
        assert_eq!(OPERATION_ADD_EFFECT, "add_effect");
        assert_eq!(OPERATION_DELETE_EFFECT, "delete_effect");
        assert_eq!(OPERATION_SET_EFFECT_STATE, "set_effect_state");
        assert_eq!(OPERATION_SET_SELECTION, "set_selection");
    }

    #[test]
    fn edit_operation_as_str_matches_the_operation_constants() {
        assert_eq!(
            EditOperation::CreateObject.as_str(),
            OPERATION_CREATE_OBJECT
        );
        assert_eq!(EditOperation::MoveObject.as_str(), OPERATION_MOVE_OBJECT);
        assert_eq!(
            EditOperation::DeleteObject.as_str(),
            OPERATION_DELETE_OBJECT
        );
        assert_eq!(
            EditOperation::SetObjectName.as_str(),
            OPERATION_SET_OBJECT_NAME
        );
        assert_eq!(
            EditOperation::SetObjectItem.as_str(),
            OPERATION_SET_OBJECT_ITEM
        );
        assert_eq!(EditOperation::AddEffect.as_str(), OPERATION_ADD_EFFECT);
        assert_eq!(
            EditOperation::DeleteEffect.as_str(),
            OPERATION_DELETE_EFFECT
        );
        assert_eq!(
            EditOperation::SetEffectState.as_str(),
            OPERATION_SET_EFFECT_STATE
        );
        assert_eq!(
            EditOperation::SetSelection.as_str(),
            OPERATION_SET_SELECTION
        );
    }

    #[test]
    fn edit_operation_from_operation_name_round_trips_through_all() {
        for op in EditOperation::ALL {
            assert_eq!(EditOperation::from_operation_name(op.as_str()), Some(op));
        }
        for name in ["", "ping", OPERATION_GET_EDIT_INFO, "future_operation"] {
            assert_eq!(EditOperation::from_operation_name(name), None);
        }
    }

    /// [`EditOperation::ALL`] が全 variant を含むことを固定する。
    ///
    /// `assert_listed` の中身は網羅 match であり、`EditOperation` へ variant を
    /// 追加すると腕が足りずコンパイルが落ちる。腕を追加した際に対応する
    /// `assert_listed(...)` 呼び出しを下の一覧へ足し忘れると、その variant は
    /// `ALL` に含まれるかを確認されないまま残る。呼び出し一覧と `ALL` は
    /// どちらも本テストの中でだけ手で書く 2 つの独立した表現であり、
    /// 一方だけを更新すると `assert!` が実行時に落ちて食い違いが分かる。
    #[test]
    fn edit_operation_all_is_exhaustive() {
        fn assert_listed(op: EditOperation) {
            match op {
                EditOperation::CreateObject
                | EditOperation::MoveObject
                | EditOperation::DeleteObject
                | EditOperation::SetObjectName
                | EditOperation::SetObjectItem
                | EditOperation::AddEffect
                | EditOperation::DeleteEffect
                | EditOperation::SetEffectState
                | EditOperation::SetSelection => {}
            }
            assert!(
                EditOperation::ALL.contains(&op),
                "{op:?} が EditOperation::ALL に含まれていません"
            );
        }

        assert_listed(EditOperation::CreateObject);
        assert_listed(EditOperation::MoveObject);
        assert_listed(EditOperation::DeleteObject);
        assert_listed(EditOperation::SetObjectName);
        assert_listed(EditOperation::SetObjectItem);
        assert_listed(EditOperation::AddEffect);
        assert_listed(EditOperation::DeleteEffect);
        assert_listed(EditOperation::SetEffectState);
        assert_listed(EditOperation::SetSelection);
        assert_eq!(EditOperation::ALL.len(), 9);
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
    fn list_layers_params_wire_form_is_flat() {
        // ページ指定は入れ子にせず、他の一覧系 params と同じく #[serde(flatten)] で平坦に並べる。
        let params = ListLayersParams {
            expected_scene_id: 0,
            page: PageRequest {
                offset: 10,
                limit: 20,
                snapshot_revision: Some(42),
            },
        };
        assert_eq!(
            serde_json::to_value(params).unwrap(),
            serde_json::json!({
                "expected_scene_id": 0,
                "offset": 10,
                "limit": 20,
                "snapshot_revision": 42,
            })
        );
    }

    #[test]
    fn list_layers_params_defaults_page() {
        let params: ListLayersParams = serde_json::from_str(r#"{"expected_scene_id":0}"#).unwrap();
        assert_eq!(params.page.limit, DEFAULT_PAGE_LIMIT);
        assert_eq!(params.page.offset, 0);
        assert_eq!(params.page.snapshot_revision, None);
    }

    #[test]
    fn list_layers_params_accept_flat_page_fields() {
        let params: ListLayersParams =
            serde_json::from_str(r#"{"expected_scene_id":3,"offset":5,"limit":10}"#).unwrap();
        assert_eq!(params.expected_scene_id, 3);
        assert_eq!(params.page.offset, 5);
        assert_eq!(params.page.limit, 10);
    }

    #[test]
    fn list_layers_params_reject_unknown_field() {
        // 平坦に並べても未知フィールドは拒否する。
        assert!(
            serde_json::from_str::<ListLayersParams>(r#"{"expected_scene_id":0,"future":1}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<ListLayersParams>(
                r#"{"expected_scene_id":0,"limit":10,"future":1}"#
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
    fn object_filter_accepts_valid_layer_range() {
        for filter in [
            ObjectFilter::default(),
            ObjectFilter {
                layer_min: Some(3),
                layer_max: None,
            },
            ObjectFilter {
                layer_min: None,
                layer_max: Some(3),
            },
            ObjectFilter {
                layer_min: Some(3),
                layer_max: Some(3),
            },
            ObjectFilter {
                layer_min: Some(1),
                layer_max: Some(8),
            },
        ] {
            assert_eq!(filter.validate(), Ok(()));
        }
    }

    #[test]
    fn object_filter_rejects_inverted_layer_range() {
        let filter = ObjectFilter {
            layer_min: Some(8),
            layer_max: Some(1),
        };
        assert_eq!(
            filter.validate(),
            Err(ObjectFilterError::InvertedLayerRange { min: 8, max: 1 })
        );
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
    fn list_available_effects_params_accept_flat_page_fields() {
        let params: ListAvailableEffectsParams =
            serde_json::from_str(r#"{"effect_type":"filter","offset":2,"limit":5}"#).unwrap();
        assert_eq!(params.effect_type, Some(EffectType::Filter));
        assert_eq!(params.page.offset, 2);
        assert_eq!(params.page.limit, 5);
    }

    #[test]
    fn list_available_effects_params_reject_unknown_field() {
        assert!(serde_json::from_str::<ListAvailableEffectsParams>(r#"{"future":1}"#).is_err());
    }

    #[test]
    fn list_objects_params_reject_unknown_field() {
        assert!(
            serde_json::from_str::<ListObjectsParams>(r#"{"expected_scene_id":0,"future":1}"#)
                .is_err()
        );
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
            items: vec![sample_object_summary()],
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
