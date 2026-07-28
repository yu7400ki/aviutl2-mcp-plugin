//! read tool の `outputSchema`。
//!
//! `structuredContent` として返す値は read operation の result DTO をそのまま
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
        ("display_name", string()),
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
