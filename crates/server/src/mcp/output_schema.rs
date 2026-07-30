//! tool の `outputSchema`。
//!
//! `structuredContent` として返す値は operation の result DTO をそのまま
//! 直列化したものであり、本モジュールの schema はその形を記述する。
//! 応答型は将来の MINOR 追加を受け入れるため、追加プロパティは禁じない。

use rmcp::model::JsonObject;
use serde_json::{Value, json};
use std::sync::Arc;

/// `Value` として組み立てた schema を tool 定義が受け取る形へ変換する。
///
/// 本モジュールが返すのは常に object の schema であるため、変換は失敗しない。
pub fn as_tool_schema(schema: Value) -> Arc<JsonObject> {
    let Value::Object(object) = schema else {
        unreachable!("schema は object として組み立てられる");
    };
    Arc::new(object)
}

/// `aviutl2_list_instances` の出力。
pub fn list_instances() -> Value {
    object(&[
        ("instances", array(instance_info())),
        ("total_count", unsigned()),
        ("count", unsigned()),
        ("offset", unsigned()),
        ("has_more", boolean()),
        ("next_offset", nullable_unsigned()),
    ])
}

/// `aviutl2_get_edit_info` の出力。
pub fn edit_info() -> Value {
    object(&[
        ("scene", scene_info()),
        ("cursor", cursor()),
        ("extent", extent()),
        ("display", display_range()),
        ("selected_range", nullable(frame_range())),
        ("grid_bpm", array(number())),
        ("project_epoch", string()),
        ("project_revision", unsigned()),
    ])
}

/// `aviutl2_get_current_scene` の出力。
pub fn current_scene() -> Value {
    object(&[("scene", scene_info()), ("project_revision", unsigned())])
}

/// `aviutl2_list_layers` の出力。
pub fn list_layers() -> Value {
    page_of(layer_info())
}

/// `aviutl2_list_objects` の出力。
pub fn list_objects() -> Value {
    page_of(object_summary())
}

/// `aviutl2_get_object` の出力。
pub fn object_detail() -> Value {
    object(&[
        ("summary", object_summary()),
        ("alias", string()),
        ("sections", array(section_range())),
        ("effects", array(effect_info())),
        ("project_revision", unsigned()),
    ])
}

/// `aviutl2_list_available_effects` の出力。
pub fn list_available_effects() -> Value {
    page_of(available_effect())
}

/// `aviutl2_create_object` の出力。
///
/// `created` に作成された全件が入り、`object` はその先頭を指す。
pub fn create_object() -> Value {
    edit_outcome(object_summary(), null(), array(object_summary()))
}

/// `aviutl2_move_object` の出力。
pub fn move_object() -> Value {
    edit_outcome(object_summary(), null(), nothing_created())
}

/// `aviutl2_set_object_name` の出力。
pub fn set_object_name() -> Value {
    edit_outcome(object_summary(), null(), nothing_created())
}

/// `aviutl2_delete_object` の出力。
///
/// 対象は消えているため `object` は必ず null になる。
pub fn delete_object() -> Value {
    edit_outcome(null(), null(), nothing_created())
}

/// `aviutl2_set_object_item` の出力。
///
/// `effect` には書き込み後に読み直した値が入る。
pub fn set_object_item() -> Value {
    edit_outcome(object_summary(), effect_info(), nothing_created())
}

/// `aviutl2_add_effect` の出力。
///
/// effect の付与はオブジェクトを作らないため `created` は空である。
pub fn add_effect() -> Value {
    edit_outcome(object_summary(), effect_info(), nothing_created())
}

/// `aviutl2_set_effect_state` の出力。
pub fn set_effect_state() -> Value {
    edit_outcome(object_summary(), effect_info(), nothing_created())
}

/// `aviutl2_delete_effect` の出力。
///
/// effect は消えているため `effect` は必ず null になる。
pub fn delete_effect() -> Value {
    edit_outcome(object_summary(), null(), nothing_created())
}

/// `aviutl2_set_selection` の出力。
pub fn set_selection() -> Value {
    object(&[
        ("project_epoch", string()),
        ("project_revision", unsigned()),
        ("cursor", cursor()),
        ("selected_range", nullable(frame_range())),
        ("focus", nullable(object_summary())),
        ("applied", array(selection_field())),
        ("not_applied", array(selection_field())),
        ("observed_after_edit", boolean()),
    ])
}

/// 構造を変更する編集の結果。
///
/// `object` / `effect` / `created` に何が入るかは operation ごとに決まるため、
/// tool ごとに別の schema として宣言する。1 つへ畳むと「この tool では必ず
/// object が入る」という情報が失われる。
fn edit_outcome(target: Value, effect: Value, created: Value) -> Value {
    object(&[
        ("project_epoch", string()),
        ("project_revision", unsigned()),
        ("object", target),
        ("effect", effect),
        ("created", created),
    ])
}

/// オブジェクトを作らない operation の `created`。
///
/// 空配列しか許さない。`object` / `effect` と同じく、operation ごとに何が
/// 入るかを schema へ残す。緩めると、作成しない operation の応答に対象が
/// 紛れ込んでも検出できない。
fn nothing_created() -> Value {
    json!({ "type": "array", "items": object_summary(), "maxItems": 0 })
}

/// 選択状態のうち適用できた項目。
fn selection_field() -> Value {
    json!({ "type": "string", "enum": ["cursor", "selected_range", "focus"] })
}

/// 一覧応答の共通形。
fn page_of(item: Value) -> Value {
    object(&[("items", array(item)), ("page", page_meta())])
}

fn instance_info() -> Value {
    object(&[
        ("instance_id", string()),
        ("state", string()),
        ("pid", unsigned()),
        ("started_at", string()),
        ("project", nullable(instance_project())),
        ("scene", nullable(scene_ref())),
    ])
}

fn instance_project() -> Value {
    object(&[
        ("display_name", nullable_string()),
        ("path", nullable_string()),
        ("epoch", nullable_string()),
        ("revision", nullable_unsigned()),
        ("modified", nullable_boolean()),
    ])
}

fn scene_ref() -> Value {
    object(&[("id", integer()), ("name", nullable_string())])
}

fn scene_info() -> Value {
    object(&[
        ("id", integer()),
        ("name", nullable_string()),
        ("width", unsigned()),
        ("height", unsigned()),
        ("fps", nullable_number()),
        ("fps_rate", integer()),
        ("fps_scale", integer()),
        ("sample_rate", unsigned()),
    ])
}

fn cursor() -> Value {
    object(&[("frame", unsigned()), ("layer", unsigned())])
}

fn extent() -> Value {
    object(&[("frame_max", unsigned()), ("layer_max", unsigned())])
}

fn display_range() -> Value {
    object(&[
        ("frame_start", unsigned()),
        ("layer_start", unsigned()),
        ("frame_num", unsigned()),
        ("layer_num", unsigned()),
    ])
}

fn frame_range() -> Value {
    object(&[("start", unsigned()), ("end", unsigned())])
}

fn page_meta() -> Value {
    object(&[
        ("total_count", unsigned()),
        ("count", unsigned()),
        ("offset", unsigned()),
        ("has_more", boolean()),
        ("next_offset", nullable_unsigned()),
        ("snapshot_revision", unsigned()),
    ])
}

fn layer_info() -> Value {
    object(&[
        ("index", unsigned()),
        ("name", nullable_string()),
        ("enabled", boolean()),
        ("locked", boolean()),
        ("object_count", unsigned()),
    ])
}

fn object_summary() -> Value {
    object(&[
        ("layer", unsigned()),
        ("frame_start", unsigned()),
        ("frame_end", unsigned()),
        ("name", nullable_string()),
        ("selector", object_selector()),
        ("fingerprint", string()),
        ("fingerprint_algorithm", string()),
    ])
}

fn object_selector() -> Value {
    object(&[
        ("project_epoch", string()),
        ("scene_id", integer()),
        ("layer", unsigned()),
        ("frame", unsigned()),
        ("name", nullable_string()),
        ("fingerprint", string()),
        ("fingerprint_algorithm", string()),
    ])
}

fn effect_selector() -> Value {
    object(&[
        ("object", object_selector()),
        ("effect_name", string()),
        ("effect_index", unsigned()),
        ("fingerprint", string()),
        ("fingerprint_algorithm", string()),
    ])
}

fn section_range() -> Value {
    object(&[("start", unsigned()), ("end", unsigned())])
}

fn effect_info() -> Value {
    object(&[
        ("name", string()),
        ("index", unsigned()),
        ("enabled", boolean()),
        ("locked", boolean()),
        ("items", array(effect_item())),
        ("selector", effect_selector()),
        ("fingerprint", string()),
        ("fingerprint_algorithm", string()),
    ])
}

fn effect_item() -> Value {
    object(&[
        ("name", string()),
        ("item_type", effect_item_type()),
        ("value", item_value()),
        ("track", nullable(track_info())),
    ])
}

fn track_info() -> Value {
    object(&[
        ("mode", string()),
        ("params", array(number())),
        ("accelerate", boolean()),
        ("decelerate", boolean()),
        ("twopoint", boolean()),
        ("timecontrol", boolean()),
        ("group_num", unsigned()),
        ("group_index", unsigned()),
        ("group_name", nullable_string()),
    ])
}

fn available_effect() -> Value {
    object(&[
        ("name", string()),
        ("effect_type", effect_type()),
        ("flags", effect_flags()),
        ("items", array(available_effect_item())),
    ])
}

fn available_effect_item() -> Value {
    object(&[("name", string()), ("item_type", effect_item_type())])
}

fn effect_flags() -> Value {
    object(&[
        ("raw", unsigned()),
        ("video", boolean()),
        ("audio", boolean()),
        ("filter", boolean()),
        ("camera", boolean()),
    ])
}

/// effect の種別。既知は名前、未知は raw 保持のオブジェクト。
fn effect_type() -> Value {
    kind(&["filter", "input", "transition", "control", "output"])
}

/// effect 設定項目の種別。既知は名前、未知は raw 保持のオブジェクト。
fn effect_item_type() -> Value {
    kind(&[
        "integer", "number", "check", "text", "string", "file", "color", "select", "scene",
        "range", "combo", "mask", "font", "figure", "data", "folder",
    ])
}

/// effect 設定項目の値。`type` を判別子とする union。
fn item_value() -> Value {
    json!({
        "oneOf": [
            tagged("number", &[("value", number())]),
            tagged("integer", &[("value", integer())]),
            tagged("bool", &[("value", boolean())]),
            tagged("color", &[("value", string())]),
            tagged("choice", &[("value", string()), ("index", nullable_unsigned())]),
            tagged("file", &[("path", string())]),
            tagged("folder", &[("path", string())]),
            tagged("font", &[("name", string())]),
            tagged("text", &[("value", string())]),
            tagged("unknown", &[("raw", string())]),
        ]
    })
}

/// 既知の名前、または `{"type":"unknown","raw":<整数>}` を受け入れる schema。
fn kind(names: &[&str]) -> Value {
    json!({
        "oneOf": [
            { "type": "string", "enum": names },
            tagged("unknown", &[("raw", integer())]),
        ]
    })
}

/// `type` を判別子に持つオブジェクトの schema。
fn tagged(tag: &str, properties: &[(&str, Value)]) -> Value {
    let mut all = vec![("type", json!({ "type": "string", "const": tag }))];
    all.extend(
        properties
            .iter()
            .map(|(name, schema)| (*name, schema.clone())),
    );
    object(&all)
}

/// 全プロパティを必須とする object の schema。
fn object(properties: &[(&str, Value)]) -> Value {
    let mut map = serde_json::Map::new();
    let mut required = Vec::with_capacity(properties.len());
    for (name, schema) in properties {
        map.insert((*name).to_string(), schema.clone());
        required.push(Value::String((*name).to_string()));
    }
    json!({
        "type": "object",
        "properties": Value::Object(map),
        "required": Value::Array(required),
    })
}

fn array(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}

fn string() -> Value {
    json!({ "type": "string" })
}

fn nullable_string() -> Value {
    json!({ "type": ["string", "null"] })
}

fn boolean() -> Value {
    json!({ "type": "boolean" })
}

fn nullable_boolean() -> Value {
    json!({ "type": ["boolean", "null"] })
}

fn integer() -> Value {
    json!({ "type": "integer" })
}

fn unsigned() -> Value {
    json!({ "type": "integer", "minimum": 0 })
}

fn nullable_unsigned() -> Value {
    json!({ "type": ["integer", "null"], "minimum": 0 })
}

fn number() -> Value {
    json!({ "type": "number" })
}

fn nullable_number() -> Value {
    json!({ "type": ["number", "null"] })
}

/// object 以外も許す nullable な schema。
fn nullable(inner: Value) -> Value {
    json!({ "anyOf": [inner, { "type": "null" }] })
}

/// null しか取らない schema。
fn null() -> Value {
    json!({ "type": "null" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ListInstancesResponse;
    use aviutl2_mcp_core::{
        AvailableEffect, AvailableEffectItem, Cursor, DisplayRange, EditInfo, EditOutcome,
        EffectFingerprintInput, EffectFlags, EffectInfo, EffectItem, EffectItemType, EffectType,
        Extent, FiniteF64, FrameRange, GetCurrentSceneResult, InstanceId, InstanceInfo,
        InstanceProject, InstanceState, ItemValue, LayerInfo, ListAvailableEffectsResult,
        ListLayersResult, ListObjectsResult, ObjectDetail, ObjectFingerprintInput, ObjectSummary,
        PageMeta, SceneInfo, SceneRef, SectionRange, SelectionField, SelectionState, TrackInfo,
    };

    /// 値が schema に適合するかを再帰的に検査する。
    ///
    /// object は property 名の集合が一致することまで求める。DTO にフィールドが
    /// 増減した場合、schema を直していなければここで検出される。
    fn check(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
        if let Some(branches) = schema.get("oneOf").or_else(|| schema.get("anyOf")) {
            let branches = branches.as_array().ok_or("oneOf / anyOf は配列")?;
            let mut failures = Vec::new();
            for branch in branches {
                match check(branch, value, path) {
                    Ok(()) => return Ok(()),
                    Err(e) => failures.push(e),
                }
            }
            return Err(format!("{path}: どの分岐にも適合しません: {failures:?}"));
        }

        if let Some(expected) = schema.get("const")
            && expected != value
        {
            return Err(format!("{path}: const {expected} と一致しません"));
        }
        if let Some(Value::Array(allowed)) = schema.get("enum")
            && !allowed.contains(value)
        {
            return Err(format!("{path}: enum に含まれません: {value}"));
        }

        let actual = json_type_name(value);
        match schema.get("type") {
            Some(Value::String(expected)) if !type_matches(expected, actual) => {
                return Err(format!("{path}: type {expected} ではなく {actual}"));
            }
            Some(Value::Array(expected)) => {
                let accepted = expected
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|name| type_matches(name, actual));
                if !accepted {
                    return Err(format!("{path}: type {expected:?} ではなく {actual}"));
                }
            }
            _ => {}
        }

        match value {
            Value::Object(map) => {
                let Some(Value::Object(properties)) = schema.get("properties") else {
                    return Ok(());
                };
                let declared: std::collections::BTreeSet<&String> = properties.keys().collect();
                let present: std::collections::BTreeSet<&String> = map.keys().collect();
                if declared != present {
                    return Err(format!(
                        "{path}: property が一致しません schema={declared:?} value={present:?}"
                    ));
                }
                for (key, item) in map {
                    check(&properties[key], item, &format!("{path}.{key}"))?;
                }
                Ok(())
            }
            Value::Array(items) => {
                if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
                    && items.len() as u64 > max
                {
                    return Err(format!(
                        "{path}: 要素数が {max} 件を超えています: {}",
                        items.len()
                    ));
                }
                let Some(item_schema) = schema.get("items") else {
                    return Ok(());
                };
                for (index, item) in items.iter().enumerate() {
                    check(item_schema, item, &format!("{path}[{index}]"))?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn json_type_name(value: &Value) -> &'static str {
        match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(n) if n.is_f64() => "number",
            Value::Number(_) => "integer",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    /// JSON Schema の型名と実際の型が両立するか。整数は number にも適合する。
    fn type_matches(expected: &str, actual: &str) -> bool {
        expected == actual || (expected == "number" && actual == "integer")
    }

    fn assert_conforms(schema: Value, value: &Value) {
        if let Err(e) = check(&schema, value, "$") {
            panic!("schema に適合しません: {e}");
        }
    }

    fn to_value<T: serde::Serialize>(value: &T) -> Value {
        serde_json::to_value(value).expect("DTO は直列化できる")
    }

    fn sample_scene_info() -> SceneInfo {
        SceneInfo {
            id: 0,
            name: Some("Scene 1".to_string()),
            width: 1920,
            height: 1080,
            fps: FiniteF64::try_new(60.0),
            fps_rate: 60,
            fps_scale: 1,
            sample_rate: 48_000,
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
            },
        )
    }

    /// すべての種別を 1 つずつ含む設定項目。
    fn sample_effect_items() -> Vec<EffectItem> {
        let values = [
            ItemValue::Number {
                value: FiniteF64::try_new(1.5).expect("有限値"),
            },
            ItemValue::Integer { value: 3 },
            ItemValue::Bool { value: true },
            ItemValue::Color {
                value: "#ffffff".to_string(),
            },
            ItemValue::Choice {
                value: "標準".to_string(),
                index: Some(0),
            },
            ItemValue::File {
                path: r"C:\clip.mp4".to_string(),
            },
            ItemValue::Folder {
                path: r"C:\clips".to_string(),
            },
            ItemValue::Font {
                name: "MS Gothic".to_string(),
            },
            ItemValue::Text {
                value: "字幕".to_string(),
            },
            ItemValue::Unknown {
                raw: "raw".to_string(),
            },
        ];
        values
            .into_iter()
            .enumerate()
            .map(|(index, value)| EffectItem {
                name: format!("項目{index}"),
                item_type: if index == 0 {
                    EffectItemType::Unknown(99)
                } else {
                    EffectItemType::Number
                },
                value,
                track: (index == 0).then(|| TrackInfo {
                    mode: "直線移動".to_string(),
                    params: vec![FiniteF64::try_new(0.0).expect("有限値")],
                    accelerate: false,
                    decelerate: true,
                    twopoint: false,
                    timecontrol: false,
                    group_num: 2,
                    group_index: 1,
                    group_name: Some("グループ".to_string()),
                }),
            })
            .collect()
    }

    fn sample_object_detail() -> ObjectDetail {
        let summary = sample_object_summary();
        let items = sample_effect_items();
        let effect = EffectInfo::new(
            summary.selector.clone(),
            EffectFingerprintInput {
                effect_name: "動画ファイル",
                effect_index: 0,
                position: 0,
                effect_count: 1,
                enabled: true,
                locked: false,
                items: &items,
            },
        );
        ObjectDetail {
            summary,
            alias: "[vo]\n_name=立ち絵\n".to_string(),
            sections: vec![SectionRange {
                start: 120,
                end: 240,
            }],
            effects: vec![effect],
            project_revision: 42,
        }
    }

    fn sample_edit_info() -> EditInfo {
        EditInfo {
            scene: sample_scene_info(),
            cursor: Cursor { frame: 0, layer: 0 },
            extent: Extent {
                frame_max: 240,
                layer_max: 4,
            },
            display: DisplayRange {
                frame_start: 0,
                layer_start: 0,
                frame_num: 100,
                layer_num: 10,
            },
            selected_range: Some(FrameRange { start: 0, end: 10 }),
            grid_bpm: vec![FiniteF64::try_new(120.0).expect("有限値")],
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            project_revision: 42,
        }
    }

    fn sample_instances_response() -> ListInstancesResponse {
        ListInstancesResponse {
            instances: vec![InstanceInfo {
                instance_id: InstanceId::new_v4(),
                state: InstanceState::Ready,
                pid: 1234,
                started_at: "2026-01-01T00:00:00.0000000Z".to_string(),
                project: Some(InstanceProject {
                    display_name: Some("Test".to_string()),
                    path: Some(r"C:\test.aup2".to_string()),
                    epoch: Some("epoch".to_string()),
                    revision: Some(3),
                    modified: Some(false),
                }),
                scene: Some(SceneRef {
                    id: 0,
                    name: Some("Scene 1".to_string()),
                }),
            }],
            total_count: 1,
            count: 1,
            offset: 0,
            has_more: false,
            next_offset: None,
        }
    }

    #[test]
    fn list_instances_schema_matches_dto() {
        assert_conforms(list_instances(), &to_value(&sample_instances_response()));
    }

    #[test]
    fn list_instances_schema_accepts_a_project_without_a_file() {
        // 未保存プロジェクトは表示名もパスも持たないが、実測した状態は運ばれる。
        let mut response = sample_instances_response();
        let project = response.instances[0]
            .project
            .as_mut()
            .expect("標本は project を持つ");
        project.display_name = None;
        project.path = None;
        assert_conforms(list_instances(), &to_value(&response));
    }

    #[test]
    fn list_instances_schema_accepts_absent_project_and_scene() {
        let mut response = sample_instances_response();
        response.instances[0].project = None;
        response.instances[0].scene = None;
        assert_conforms(list_instances(), &to_value(&response));
    }

    #[test]
    fn edit_info_schema_matches_dto() {
        assert_conforms(edit_info(), &to_value(&sample_edit_info()));
    }

    #[test]
    fn edit_info_schema_accepts_absent_selection() {
        let mut info = sample_edit_info();
        info.selected_range = None;
        info.scene.name = None;
        info.scene.fps = None;
        assert_conforms(edit_info(), &to_value(&info));
    }

    #[test]
    fn current_scene_schema_matches_dto() {
        let result = GetCurrentSceneResult {
            scene: sample_scene_info(),
            project_revision: 42,
        };
        assert_conforms(current_scene(), &to_value(&result));
    }

    #[test]
    fn list_layers_schema_matches_dto() {
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
        assert_conforms(list_layers(), &to_value(&result));
    }

    #[test]
    fn list_objects_schema_matches_dto() {
        let result = ListObjectsResult {
            items: vec![sample_object_summary()],
            page: sample_page_meta(),
        };
        assert_conforms(list_objects(), &to_value(&result));
    }

    #[test]
    fn object_detail_schema_matches_dto() {
        assert_conforms(object_detail(), &to_value(&sample_object_detail()));
    }

    #[test]
    fn list_available_effects_schema_matches_dto() {
        let result = ListAvailableEffectsResult {
            items: vec![
                AvailableEffect {
                    name: "ぼかし".to_string(),
                    effect_type: EffectType::Filter,
                    flags: EffectFlags::from_raw(9),
                    items: vec![AvailableEffectItem {
                        name: "範囲".to_string(),
                        item_type: EffectItemType::Integer,
                    }],
                },
                AvailableEffect {
                    name: "未知".to_string(),
                    effect_type: EffectType::Unknown(42),
                    flags: EffectFlags::from_raw(0),
                    items: vec![AvailableEffectItem {
                        name: "未知項目".to_string(),
                        item_type: EffectItemType::Unknown(77),
                    }],
                },
            ],
            page: sample_page_meta(),
        };
        assert_conforms(list_available_effects(), &to_value(&result));
    }

    /// 応答へ載せる effect。全種別の設定項目を 1 つずつ含む。
    fn sample_effect_info() -> EffectInfo {
        let items = sample_effect_items();
        EffectInfo::new(
            sample_object_summary().selector,
            EffectFingerprintInput {
                effect_name: "動画ファイル",
                effect_index: 0,
                position: 0,
                effect_count: 1,
                enabled: true,
                locked: false,
                items: &items,
            },
        )
    }

    fn sample_selection_state() -> SelectionState {
        SelectionState::observed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            42,
            Cursor {
                frame: 120,
                layer: 2,
            },
            Some(FrameRange { start: 0, end: 10 }),
            Some(sample_object_summary()),
            vec![
                SelectionField::Cursor,
                SelectionField::SelectedRange,
                SelectionField::Focus,
            ],
            Vec::new(),
        )
    }

    #[test]
    fn create_object_schema_matches_dto() {
        let outcome = EditOutcome::created(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            vec![sample_object_summary(), sample_object_summary()],
        );
        assert_conforms(create_object(), &to_value(&outcome));
    }

    #[test]
    fn move_object_schema_matches_dto() {
        let outcome = EditOutcome::object_changed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            sample_object_summary(),
        );
        assert_conforms(move_object(), &to_value(&outcome));
    }

    #[test]
    fn set_object_name_schema_matches_dto() {
        let outcome = EditOutcome::object_changed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            sample_object_summary(),
        );
        assert_conforms(set_object_name(), &to_value(&outcome));
    }

    #[test]
    fn delete_effect_schema_matches_dto() {
        let outcome = EditOutcome::object_changed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            sample_object_summary(),
        );
        assert_conforms(delete_effect(), &to_value(&outcome));
    }

    #[test]
    fn delete_object_schema_matches_dto() {
        let outcome = EditOutcome::deleted("78be92d1-c8c9-44c6-ae52-387548971468", 43);
        assert_conforms(delete_object(), &to_value(&outcome));
    }

    #[test]
    fn effect_changing_schemas_match_dto() {
        let outcome = EditOutcome::effect_changed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            sample_object_summary(),
            sample_effect_info(),
        );
        let value = to_value(&outcome);
        for schema in [set_object_item(), add_effect(), set_effect_state()] {
            assert_conforms(schema, &value);
        }
    }

    #[test]
    fn set_selection_schema_matches_dto() {
        assert_conforms(set_selection(), &to_value(&sample_selection_state()));
    }

    #[test]
    fn set_selection_schema_accepts_absent_range_and_focus() {
        let mut state = sample_selection_state();
        state.selected_range = None;
        state.focus = None;
        state.applied = vec![SelectionField::Cursor];
        assert_conforms(set_selection(), &to_value(&state));
    }

    #[test]
    fn deleting_schemas_require_the_removed_target_to_be_null() {
        // 「この tool では対象が消える」という情報を schema に残す。畳んで
        // nullable にすると、削除の応答が対象を返しても検出できなくなる。
        let outcome = EditOutcome::object_changed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            sample_object_summary(),
        );
        assert!(
            check(&delete_object(), &to_value(&outcome), "$").is_err(),
            "削除の応答に残った対象を検出できていません"
        );

        let outcome = EditOutcome::effect_changed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            sample_object_summary(),
            sample_effect_info(),
        );
        assert!(
            check(&delete_effect(), &to_value(&outcome), "$").is_err(),
            "削除の応答に残った effect を検出できていません"
        );
    }

    #[test]
    fn non_creating_schemas_require_an_empty_created_list() {
        // 作成しない operation の応答に対象が紛れ込んでも、`created` が
        // 素の配列のままでは検出できない。
        let mut value = to_value(&EditOutcome::object_changed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            sample_object_summary(),
        ));
        value["created"] = json!([to_value(&sample_object_summary())]);
        for (name, schema) in [
            ("move_object", move_object()),
            ("set_object_name", set_object_name()),
            ("delete_object", delete_object()),
            ("delete_effect", delete_effect()),
            ("set_object_item", set_object_item()),
            ("add_effect", add_effect()),
            ("set_effect_state", set_effect_state()),
            ("set_selection", set_selection()),
        ] {
            if name == "set_selection" {
                // 選択状態は `created` を持たない。property 名の照合で落ちる。
                assert!(check(&schema, &value, "$").is_err(), "{name}");
                continue;
            }
            assert!(
                check(&schema, &value, "$").is_err(),
                "{name} が作成された対象を素通ししています"
            );
        }
    }

    #[test]
    fn creating_schema_accepts_multiple_created_objects() {
        // 作成だけは複数件を返す。空しか許さない側へ倒すと、複数オブジェクトを
        // 含む alias の応答が自分の宣言に適合しなくなる。
        let outcome = EditOutcome::created(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            vec![sample_object_summary(), sample_object_summary()],
        );
        assert_conforms(create_object(), &to_value(&outcome));
    }

    #[test]
    fn effect_changing_schemas_require_an_effect() {
        // effect を伴う operation で effect が欠けた応答を通さない。
        let outcome = EditOutcome::object_changed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            sample_object_summary(),
        );
        let value = to_value(&outcome);
        for schema in [set_object_item(), add_effect(), set_effect_state()] {
            assert!(
                check(&schema, &value, "$").is_err(),
                "effect の欠落を検出できていません"
            );
        }
    }

    #[test]
    fn checker_detects_added_field() {
        let mut value = to_value(&sample_edit_info());
        value
            .as_object_mut()
            .expect("object")
            .insert("future".to_string(), json!(1));
        assert!(
            check(&edit_info(), &value, "$").is_err(),
            "増えたフィールドを検出できていません"
        );
    }

    #[test]
    fn checker_detects_removed_field() {
        let mut value = to_value(&sample_edit_info());
        value.as_object_mut().expect("object").remove("cursor");
        assert!(
            check(&edit_info(), &value, "$").is_err(),
            "欠けたフィールドを検出できていません"
        );
    }

    #[test]
    fn checker_detects_wrong_type() {
        let mut value = to_value(&sample_edit_info());
        value["project_revision"] = json!("42");
        assert!(
            check(&edit_info(), &value, "$").is_err(),
            "型の誤りを検出できていません"
        );
    }

    #[test]
    fn checker_detects_nested_field_drift() {
        let mut value = to_value(&sample_object_detail());
        value["summary"]["selector"]
            .as_object_mut()
            .expect("object")
            .remove("fingerprint");
        assert!(
            check(&object_detail(), &value, "$").is_err(),
            "入れ子のフィールド差分を検出できていません"
        );
    }

    #[test]
    fn as_tool_schema_keeps_properties() {
        let schema = as_tool_schema(list_layers());
        assert_eq!(schema["type"], json!("object"));
        assert!(schema["properties"]["items"].is_object());
    }
}
