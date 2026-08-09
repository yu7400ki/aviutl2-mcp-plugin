//! tool の `outputSchema`。
//!
//! `structuredContent` として返す値は operation の result DTO をそのまま
//! 直列化したものであり、本モジュールの schema はその形を記述する。
//! 応答型は受け手が使わない値が足されていても受理するため、追加プロパティは
//! 禁じない。

use aviutl2_mcp_core::PALETTE_COLOR_COUNT;
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

/// `list_instances` の出力。
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

/// `get_edit_info` の出力。
pub fn edit_info() -> Value {
    object(&[
        ("scene", scene_info()),
        ("cursor", cursor()),
        ("extent", extent()),
        ("display", display_range()),
        ("selected_range", nullable(frame_range())),
        ("grid_bpm", array(grid_bpm())),
        ("project_epoch", string()),
        ("project_revision", unsigned()),
    ])
}

/// `get_current_scene` の出力。
pub fn current_scene() -> Value {
    object(&[("scene", scene_info()), ("project_revision", unsigned())])
}

/// `list_layers` の出力。
pub fn list_layers() -> Value {
    page_of(layer_info())
}

/// `list_objects` の出力。
pub fn list_objects() -> Value {
    page_of(object_summary())
}

/// `get_selection` の出力。
///
/// 編集カーソルとフレーム範囲選択は含まない。どちらも `get_edit_info` が返す。
pub fn get_selection() -> Value {
    object(&[
        ("project_revision", unsigned()),
        ("focus", nullable(object_summary())),
        ("focus_section", nullable_unsigned()),
        ("selected", array(object_summary())),
        ("page", page_meta()),
    ])
}

/// `get_object` の出力。
pub fn object_detail() -> Value {
    object(&[
        ("summary", object_summary()),
        ("alias", string()),
        ("sections", array(section_range())),
        ("effects", array(effect_info())),
        ("project_revision", unsigned()),
    ])
}

/// `list_available_effects` の出力。
pub fn list_available_effects() -> Value {
    page_of(available_effect())
}

/// `describe_effects` の出力。
///
/// ページのメタ情報を持たない。返すのは要求が名指しした effect だけであり、
/// 続きという概念が無い。
pub fn describe_effects() -> Value {
    object(&[
        ("effects", array(effect_description())),
        ("not_found", array(string())),
    ])
}

/// `list_fonts` の出力。
pub fn list_fonts() -> Value {
    page_of(string())
}

/// `list_palettes` の出力。
pub fn list_palettes() -> Value {
    object(&[
        ("current", nullable_string()),
        ("items", array(palette_entry())),
        ("page", page_meta()),
    ])
}

/// `list_modules` の出力。
pub fn list_modules() -> Value {
    page_of(module_entry())
}

/// `list_object_aliases` の出力。
pub fn list_object_aliases() -> Value {
    page_of(object_alias_summary())
}

/// `get_effect_item_values` の出力。
///
/// `frames` は要求のエコーであり、`items` の各 `values` はこれと同じ長さ・同じ
/// 順序で並ぶ。
pub fn effect_item_values() -> Value {
    object(&[
        ("project_revision", unsigned()),
        ("frames", array(number())),
        ("items", array(evaluated_item())),
        ("truncated", boolean()),
    ])
}

/// 評価した設定項目。`type` を判別子とする union。
///
/// 値の型が種別ごとに違うため、1 つの配列へ数値と真偽を混ぜない。
fn evaluated_item() -> Value {
    json!({
        "oneOf": [
            tagged("track", &[
                ("name", string()),
                ("values", array(number())),
                ("group", nullable(track_group())),
            ]),
            tagged("check", &[("name", string()), ("values", array(boolean()))]),
        ]
    })
}

/// トラックバーのグループ。
///
/// `count` と `item_names` の件数は一致するとは限らない。
fn track_group() -> Value {
    object(&[
        ("name", string()),
        ("index", unsigned()),
        ("count", unsigned()),
        ("item_names", array(string())),
    ])
}

/// `create_object` の出力。
///
/// `created` に作成された全件が入り、`object` はその先頭を指す。
pub fn create_object() -> Value {
    edit_outcome(object_summary(), null(), array(object_summary()))
}

/// `move_object` の出力。
pub fn move_object() -> Value {
    edit_outcome(object_summary(), null(), nothing_created())
}

/// `set_object_name` の出力。
pub fn set_object_name() -> Value {
    edit_outcome(object_summary(), null(), nothing_created())
}

/// `delete_object` の出力。
///
/// 対象は消えているため `object` は必ず null になる。
pub fn delete_object() -> Value {
    edit_outcome(null(), null(), nothing_created())
}

/// `set_object_item` の出力。
///
/// `effect` には書き込み後に読み直した値が入る。
pub fn set_object_item() -> Value {
    edit_outcome(object_summary(), effect_info(), nothing_created())
}

/// `add_effect` の出力。
///
/// effect の付与はオブジェクトを作らないため `created` は空である。
pub fn add_effect() -> Value {
    edit_outcome(object_summary(), effect_info(), nothing_created())
}

/// `set_effect_enabled` の出力。
pub fn set_effect_enabled() -> Value {
    edit_outcome(object_summary(), effect_info(), nothing_created())
}

/// `move_effect` の出力。
///
/// `effect` には移動後に読み直した値が入る。`position` が移動先である。
pub fn move_effect() -> Value {
    edit_outcome(object_summary(), effect_info(), nothing_created())
}

/// `delete_effect` の出力。
///
/// effect は消えているため `effect` は必ず null になる。
pub fn delete_effect() -> Value {
    edit_outcome(object_summary(), null(), nothing_created())
}

/// `create_object_section` の出力。
pub fn create_object_section() -> Value {
    object_sections()
}

/// `delete_object_section` の出力。
pub fn delete_object_section() -> Value {
    object_sections()
}

/// `move_object_section` の出力。
pub fn move_object_section() -> Value {
    object_sections()
}

/// 中間点を変える 3 つの operation が共通して返す形。
///
/// 返すのは概要であって詳細ではない。alias も設定値も載らず、`sections` が
/// read-back そのものになる。
fn object_sections() -> Value {
    object(&[
        ("project_epoch", string()),
        ("project_revision", unsigned()),
        ("object", object_summary()),
        ("sections", array(section_range())),
    ])
}

/// `set_layer_state` の出力。
///
/// `layer` には変更後に読み直した状態が入る。レイヤーは fingerprint を持たない
/// ため、要求元はこの値で実際の状態を確かめる。
pub fn set_layer_state() -> Value {
    object(&[
        ("project_epoch", string()),
        ("project_revision", unsigned()),
        ("layer", layer_info()),
    ])
}

/// `set_grid_bpm` の出力。
///
/// `entries` には置き換え後に読み直した一覧が入る。要求した値と一致するとは
/// 限らない。
pub fn set_grid_bpm() -> Value {
    object(&[
        ("project_epoch", string()),
        ("project_revision", unsigned()),
        ("entries", array(grid_bpm())),
    ])
}

/// `set_scene_settings` の出力。
///
/// `scene` には変更後に観測したシーンの状態が入る。シーンは fingerprint を
/// 持たないため、要求元はこの値で実際の状態を確かめる。`observed_after_edit` は
/// 解像度とサンプリングレートの観測が編集と原子的でないこと、`non_undoable` は
/// この変更が取り消せないことを示す。
pub fn set_scene_settings() -> Value {
    object(&[
        ("project_epoch", string()),
        ("project_revision", unsigned()),
        ("scene", scene_info()),
        ("observed_after_edit", boolean()),
        ("non_undoable", boolean()),
    ])
}

/// `apply_batch` の出力。
///
/// `results` は入力と同じ位置で並ぶ。revision は要求全体で 1 つだけ持つ。
pub fn apply_batch() -> Value {
    object(&[
        ("project_epoch", string()),
        ("project_revision", unsigned()),
        ("results", array(batch_step_outcome())),
    ])
}

/// 一括適用の 1 sub-operation の結果。
///
/// **`project_revision` と `created` を許さない。** 前者は要求全体で 1 しか
/// 進まない値であり、要素ごとに現れると 1 つの取り消し単位であることと矛盾する。
/// 後者は一括適用に作成が入らないため常に空になる。持たないことを schema で
/// 言い切ることで、混入しても検出できない状態を作らない。
fn batch_step_outcome() -> Value {
    object(&[
        ("object", object_summary()),
        ("effect", nullable(effect_info())),
    ])
}

/// `render_frame` の出力。
///
/// 接続先の result とは別の形である。引き渡しの識別子はここに現れない。
pub fn render_frame() -> Value {
    object(&[
        ("project_epoch", string()),
        ("project_revision", unsigned()),
        ("scene_id", integer()),
        ("frame", unsigned()),
        ("width", unsigned()),
        ("height", unsigned()),
        ("artifact", artifact_ref()),
    ])
}

/// 要求元へ渡す成果物の参照。
fn artifact_ref() -> Value {
    object(&[
        ("artifact_id", string()),
        ("uri", string()),
        ("media_type", string()),
        ("byte_length", unsigned()),
        ("sha256", string()),
        ("expires_at", string()),
    ])
}

/// `set_selection` の出力。
pub fn set_selection() -> Value {
    object(&[
        ("project_epoch", string()),
        ("project_revision", unsigned()),
        ("cursor", cursor()),
        ("selected_range", nullable(frame_range())),
        ("focus", nullable(object_summary())),
        ("display", display_range()),
        ("applied", array(selection_field())),
        ("not_applied", array(selection_field())),
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
    json!({ "type": "string", "enum": ["cursor", "selected_range", "focus", "display"] })
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
        ("project", instance_project()),
    ])
}

fn instance_project() -> Value {
    object(&[
        ("display_name", nullable_string()),
        ("path", nullable_string()),
        ("epoch", string()),
        ("revision", unsigned()),
        ("modified", boolean()),
    ])
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

/// BPM グリッドの 1 件。
///
/// 読み取りと書き込みで同じ形である。`start` と `offset` は秒であり、フレーム
/// 番号ではない。
fn grid_bpm() -> Value {
    object(&[
        ("tempo", number()),
        ("beat", integer()),
        ("start", number()),
        ("offset", number()),
    ])
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
    ])
}

fn effect_selector() -> Value {
    object(&[
        ("object", object_selector()),
        ("effect_name", string()),
        ("effect_index", unsigned()),
        ("fingerprint", string()),
    ])
}

fn section_range() -> Value {
    object(&[("start", unsigned()), ("end", unsigned())])
}

fn effect_info() -> Value {
    object(&[
        ("name", string()),
        ("index", unsigned()),
        ("position", unsigned()),
        ("enabled", boolean()),
        ("locked", boolean()),
        ("items", array(effect_item())),
        ("selector", effect_selector()),
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
        ("item_count", unsigned()),
        ("description", nullable_string()),
    ])
}

/// effect 1 件の中身。
///
/// 設定項目は名前・種別・説明だけを持ち、値を 1 つも含まない。値は対象へ
/// 付与したあと `get_object` が返す。
fn effect_description() -> Value {
    object(&[
        ("name", string()),
        ("description", nullable_string()),
        ("items", array(effect_item_description())),
    ])
}

fn effect_item_description() -> Value {
    object(&[
        ("name", string()),
        ("item_type", effect_item_type()),
        ("description", nullable_string()),
        ("choices", nullable(item_choices())),
        ("range", nullable(item_range())),
        ("group", nullable(item_group())),
    ])
}

/// 設定項目が属するグループ。
///
/// **件数の欄を持たない。** 所属アイテム名の件数がそれである。
fn item_group() -> Value {
    object(&[("index", unsigned()), ("item_names", array(string()))])
}

/// 設定項目の選択肢の候補。
///
/// 受け付ける値を宣言するものではない。候補に無い値も書き込みは通り、候補に
/// ある値が必ず通るとも限らない。
fn item_choices() -> Value {
    object(&[("values", array(string())), ("source", table_source())])
}

/// 設定項目の値域と小数桁。
///
/// 受け付ける値を宣言するものではない。この範囲を外れる値も書き込みは通る。
/// **3 つの値は個別に null を取る**——測れた側だけが載るためである。
fn item_range() -> Value {
    object(&[
        ("min", nullable_number()),
        ("max", nullable_number()),
        ("decimals", nullable_unsigned()),
        ("source", table_source()),
    ])
}

/// 表が述べたことの由来。候補にも値域にも同じ 2 値が付く。
fn table_source() -> Value {
    json!({ "type": "string", "enum": ["builtin_table", "sidecar"] })
}

fn effect_flags() -> Value {
    object(&[
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

fn palette_entry() -> Value {
    object(&[("name", string()), ("colors", palette_colors())])
}

/// パレットの色。件数はアプリケーションが固定しており増減しない。
fn palette_colors() -> Value {
    json!({
        "type": "array",
        "items": rgba(),
        "minItems": PALETTE_COLOR_COUNT,
        "maxItems": PALETTE_COLOR_COUNT,
    })
}

fn rgba() -> Value {
    object(&[("r", byte()), ("g", byte()), ("b", byte()), ("a", byte())])
}

fn module_entry() -> Value {
    object(&[
        ("module_type", module_type()),
        ("name", string()),
        ("information", string()),
    ])
}

/// 登録済みオブジェクトエイリアス 1 件の要約。
///
/// **エイリアスの中身を持たない。** 載るのは名前・ラベル・オブジェクト数と、
/// 含まれる effect 名だけである。
fn object_alias_summary() -> Value {
    object(&[
        ("name", string()),
        ("label", nullable_string()),
        ("object_count", nullable_unsigned()),
        ("effects", array(string())),
    ])
}

/// モジュールの種別。既知は名前、未知は raw 保持のオブジェクト。
fn module_type() -> Value {
    kind(&[
        "script_filter",
        "script_object",
        "script_camera",
        "script_track",
        "script_module",
        "plugin_input",
        "plugin_output",
        "plugin_filter",
        "plugin_generic",
    ])
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
            tagged("choice", &[("value", string())]),
            tagged("file", &[("path", string())]),
            tagged("folder", &[("path", string())]),
            tagged("font", &[("name", string())]),
            tagged("text", &[("value", string())]),
            tagged("track", &[
                ("values", array(number())),
                ("mode", nullable(string())),
                ("params", array(number())),
                ("accelerate", boolean()),
                ("decelerate", boolean()),
                ("twopoint", boolean()),
                ("reserved_flags", unsigned()),
            ]),
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

fn integer() -> Value {
    json!({ "type": "integer" })
}

fn unsigned() -> Value {
    json!({ "type": "integer", "minimum": 0 })
}

/// 0..=255 の整数。
fn byte() -> Value {
    json!({ "type": "integer", "minimum": 0, "maximum": 255 })
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
        AvailableEffect, Cursor, DescribeEffectsResult, DisplayRange, EditInfo, EditOutcome,
        EffectDescription, EffectFingerprintInput, EffectFlags, EffectInfo, EffectItem,
        EffectItemDescription, EffectItemType, EffectItemValues, EffectType, EvaluatedItem, Extent,
        FiniteF64, FrameRange, GetCurrentSceneResult, GridBpm, InstanceId, InstanceInfo,
        InstanceProject, InstanceState, ItemChoices, ItemGroup, ItemRange, ItemValue, LayerInfo,
        ListAvailableEffectsResult, ListFontsResult, ListLayersResult, ListModulesResult,
        ListObjectAliasesResult, ListObjectsResult, ListPalettesResult, ModuleEntry, ModuleType,
        ObjectAliasSummary, ObjectDetail, ObjectFingerprintInput, ObjectSummary, ObservedSelection,
        PageMeta, PaletteEntry, Rgba, SceneInfo, SectionRange, SelectionField, SelectionSnapshot,
        SelectionState, TableSource, TrackGroup, TrackInfo, TrackValue,
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
                // 下限も検査する。上限だけを見ると、固定長として宣言した配列に
                // 足りない件数を返しても適合してしまう。
                if let Some(min) = schema.get("minItems").and_then(Value::as_u64)
                    && (items.len() as u64) < min
                {
                    return Err(format!(
                        "{path}: 要素数が {min} 件に足りません: {}",
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
            ItemValue::Track(TrackValue {
                values: vec![
                    FiniteF64::try_new(0.0).expect("有限値"),
                    FiniteF64::try_new(100.0).expect("有限値"),
                ],
                mode: Some("直線移動".to_string()),
                params: vec![FiniteF64::try_new(15.0).expect("有限値")],
                accelerate: true,
                decelerate: false,
                twopoint: false,
                reserved_flags: 16,
            }),
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
            grid_bpm: vec![GridBpm {
                tempo: FiniteF64::try_new(120.0).expect("有限値"),
                beat: 4,
                start: FiniteF64::try_new(1.5).expect("有限値"),
                offset: FiniteF64::try_new(0.25).expect("有限値"),
            }],
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
                project: InstanceProject {
                    display_name: Some("Test".to_string()),
                    path: Some(r"C:\test.aup2".to_string()),
                    epoch: "epoch".to_string(),
                    revision: 3,
                    modified: false,
                },
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
        let project = &mut response.instances[0].project;
        project.display_name = None;
        project.path = None;
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
    fn get_selection_schema_matches_dto() {
        // フォーカスの有無で形が変わる。両方の状態を同じ schema へ通す。
        for (focus, focus_section) in [
            (Some(sample_object_summary()), Some(1)),
            (Some(sample_object_summary()), None),
            (None, None),
        ] {
            let snapshot = SelectionSnapshot {
                project_revision: 42,
                focus,
                focus_section,
                selected: vec![sample_object_summary()],
                page: sample_page_meta(),
            };
            assert_conforms(get_selection(), &to_value(&snapshot));
        }
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
                    item_count: 1,
                    description: Some("ぼかします\n範囲を指定します".to_string()),
                },
                AvailableEffect {
                    name: "未知".to_string(),
                    effect_type: EffectType::Unknown(42),
                    flags: EffectFlags::from_raw(0),
                    item_count: 0,
                    description: None,
                },
            ],
            page: sample_page_meta(),
        };
        assert_conforms(list_available_effects(), &to_value(&result));
    }

    #[test]
    fn describe_effects_schema_matches_dto() {
        // 説明を持つ effect と持たない effect、説明を持つ項目と持たない項目、
        // 候補を持つ項目と持たない項目、値域の 3 つの値が個別に欠けた項目、
        // 由来の 2 値、未知の種別、そして見つからなかった名前を 1 度に通す。
        let result = DescribeEffectsResult {
            effects: vec![
                EffectDescription {
                    name: "図形".to_string(),
                    description: Some(
                        "単色の図形を作成します\nsvgファイルから読み込めます".to_string(),
                    ),
                    items: vec![
                        EffectItemDescription {
                            name: "図形の種類".to_string(),
                            item_type: EffectItemType::Figure,
                            description: Some("図形の種類を選択します".to_string()),
                            choices: Some(ItemChoices {
                                values: vec!["円".to_string(), "四角形".to_string()],
                                source: TableSource::BuiltinTable,
                            }),
                            range: None,
                            group: Some(ItemGroup {
                                index: 0,
                                item_names: vec!["図形の種類".to_string(), "サイズ".to_string()],
                            }),
                        },
                        EffectItemDescription {
                            name: "合成モード".to_string(),
                            item_type: EffectItemType::Select,
                            description: None,
                            choices: Some(ItemChoices {
                                values: vec!["通常".to_string(), "加算".to_string()],
                                source: TableSource::Sidecar,
                            }),
                            range: None,
                            group: None,
                        },
                        EffectItemDescription {
                            name: "サイズ".to_string(),
                            item_type: EffectItemType::Integer,
                            description: None,
                            choices: None,
                            range: Some(ItemRange {
                                min: FiniteF64::try_new(1.0),
                                max: FiniteF64::try_new(4000.0),
                                decimals: Some(0),
                                source: TableSource::BuiltinTable,
                            }),
                            group: Some(ItemGroup {
                                index: 1,
                                item_names: vec!["図形の種類".to_string(), "サイズ".to_string()],
                            }),
                        },
                        EffectItemDescription {
                            name: "上限だけ測れた".to_string(),
                            item_type: EffectItemType::Number,
                            description: None,
                            choices: None,
                            range: Some(ItemRange {
                                min: None,
                                max: FiniteF64::try_new(100.0),
                                decimals: None,
                                source: TableSource::Sidecar,
                            }),
                            group: None,
                        },
                        EffectItemDescription {
                            name: "未知".to_string(),
                            item_type: EffectItemType::Unknown(42),
                            description: None,
                            choices: None,
                            range: None,
                            group: None,
                        },
                    ],
                },
                EffectDescription {
                    name: "グロー".to_string(),
                    description: None,
                    items: Vec::new(),
                },
            ],
            not_found: vec!["ぐろー".to_string()],
        };
        assert_conforms(describe_effects(), &to_value(&result));
    }

    #[test]
    fn list_fonts_schema_matches_dto() {
        let result = ListFontsResult {
            items: vec!["MS UI Gothic".to_string(), "游ゴシック".to_string()],
            page: sample_page_meta(),
        };
        assert_conforms(list_fonts(), &to_value(&result));
    }

    #[test]
    fn list_palettes_schema_matches_dto() {
        // 現在のパレット名が取れた場合と取れなかった場合の両方を通す。
        for current in [Some("[標準.既定]".to_string()), None] {
            let result = ListPalettesResult {
                current,
                items: vec![PaletteEntry {
                    name: "既定".to_string(),
                    colors: sample_palette_colors(),
                }],
                page: sample_page_meta(),
            };
            assert_conforms(list_palettes(), &to_value(&result));
        }
    }

    #[test]
    fn the_palette_schema_declares_the_fixed_number_of_colors() {
        // 固定長として宣言してあることを、件数の足りない値が退けられることで
        // 確かめる。上限だけを宣言すると、欠けた組が適合したまま通る。
        let mut colors = sample_palette_colors();
        colors.pop();
        let result = ListPalettesResult {
            current: None,
            items: vec![PaletteEntry {
                name: "既定".to_string(),
                colors,
            }],
            page: sample_page_meta(),
        };
        assert!(
            check(&list_palettes(), &to_value(&result), "$").is_err(),
            "63 件の色が固定長の宣言へ適合しました"
        );
    }

    #[test]
    fn list_modules_schema_matches_dto() {
        // 既知の種別と、型としては表せる未知の種別の両方を通す。
        let result = ListModulesResult {
            items: vec![
                ModuleEntry {
                    module_type: ModuleType::ScriptObject,
                    name: "テキスト".to_string(),
                    information: "標準搭載".to_string(),
                },
                ModuleEntry {
                    module_type: ModuleType::Unknown(42),
                    name: "未知".to_string(),
                    information: "説明".to_string(),
                },
            ],
            page: sample_page_meta(),
        };
        assert_conforms(list_modules(), &to_value(&result));
    }

    #[test]
    fn list_object_aliases_schema_matches_dto() {
        // ラベルとオブジェクト数はどちらも欠け得る。欠けた側と揃った側の両方を
        // 通す。
        let result = ListObjectAliasesResult {
            items: vec![
                ObjectAliasSummary {
                    name: "立ち絵".to_string(),
                    label: Some("キャラ".to_string()),
                    object_count: Some(2),
                    effects: vec!["テキスト".to_string(), "標準描画".to_string()],
                },
                ObjectAliasSummary {
                    name: "手置き".to_string(),
                    label: None,
                    object_count: None,
                    effects: Vec::new(),
                },
            ],
            page: sample_page_meta(),
        };
        assert_conforms(list_object_aliases(), &to_value(&result));
    }

    fn sample_palette_colors() -> Vec<Rgba> {
        (0..PALETTE_COLOR_COUNT)
            .map(|index| Rgba {
                r: index as u8,
                g: 0,
                b: 0,
                a: 255,
            })
            .collect()
    }

    #[test]
    fn effect_item_values_schema_matches_dto() {
        // グループを持つ項目と持たない項目、種別の違う項目をすべて含める。
        // 片方だけを見ると、もう片方の分岐が DTO と食い違っても気付けない。
        let result = EffectItemValues {
            project_revision: 42,
            frames: vec![
                FiniteF64::try_new(120.0).expect("有限値"),
                FiniteF64::try_new(120.5).expect("有限値"),
            ],
            items: vec![
                EvaluatedItem::Track {
                    name: "X".to_string(),
                    values: vec![
                        FiniteF64::try_new(0.0).expect("有限値"),
                        FiniteF64::try_new(1.5).expect("有限値"),
                    ],
                    group: Some(TrackGroup {
                        name: "座標".to_string(),
                        index: 0,
                        count: 3,
                        item_names: vec!["X".to_string(), "Y".to_string()],
                    }),
                },
                EvaluatedItem::Track {
                    name: "拡大率".to_string(),
                    values: vec![
                        FiniteF64::try_new(100.0).expect("有限値"),
                        FiniteF64::try_new(100.0).expect("有限値"),
                    ],
                    group: None,
                },
                EvaluatedItem::Check {
                    name: "反転".to_string(),
                    values: vec![true, false],
                },
            ],
            truncated: true,
        };
        assert_conforms(effect_item_values(), &to_value(&result));
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
            ObservedSelection {
                cursor: Cursor {
                    frame: 120,
                    layer: 2,
                },
                selected_range: Some(FrameRange { start: 0, end: 10 }),
                focus: Some(sample_object_summary()),
                display: DisplayRange {
                    frame_start: 60,
                    layer_start: 1,
                    frame_num: 600,
                    layer_num: 10,
                },
            },
            vec![
                SelectionField::Cursor,
                SelectionField::SelectedRange,
                SelectionField::Focus,
                SelectionField::Display,
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
        for schema in [
            set_object_item(),
            add_effect(),
            set_effect_enabled(),
            move_effect(),
        ] {
            assert_conforms(schema, &value);
        }
    }

    #[test]
    fn set_layer_state_schema_matches_dto() {
        let outcome = aviutl2_mcp_core::LayerStateOutcome {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            project_revision: 43,
            layer: LayerInfo {
                index: 2,
                name: Some("背景".to_string()),
                enabled: false,
                locked: true,
                object_count: 3,
            },
        };
        assert_conforms(set_layer_state(), &to_value(&outcome));

        // 標準名のままのレイヤーは名前を持たない。
        let mut value = to_value(&outcome);
        value["layer"]["name"] = json!(null);
        assert_conforms(set_layer_state(), &value);
    }

    #[test]
    fn set_scene_settings_schema_matches_dto() {
        let outcome = aviutl2_mcp_core::SceneSettingsOutcome {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            project_revision: 43,
            scene: sample_scene_info(),
            observed_after_edit: true,
            non_undoable: true,
        };
        assert_conforms(set_scene_settings(), &to_value(&outcome));

        // 名前もフレームレートも持たないシーンが返り得る。
        let mut value = to_value(&outcome);
        value["scene"]["name"] = json!(null);
        value["scene"]["fps"] = json!(null);
        assert_conforms(set_scene_settings(), &value);
    }

    #[test]
    fn object_sections_schemas_match_dto() {
        let outcome = aviutl2_mcp_core::ObjectSectionsOutcome {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            project_revision: 43,
            object: sample_object_summary(),
            sections: vec![
                SectionRange {
                    start: 120,
                    end: 179,
                },
                SectionRange {
                    start: 180,
                    end: 240,
                },
            ],
        };
        let value = to_value(&outcome);
        for schema in [
            create_object_section(),
            delete_object_section(),
            move_object_section(),
        ] {
            assert_conforms(schema, &value);
        }
    }

    #[test]
    fn object_sections_schema_does_not_declare_an_alias() {
        // 返すのは概要であって詳細ではない。schema が alias を宣言していれば、
        // 応答へ載せる実装が入っても検出できなくなる。
        let properties = create_object_section()["properties"]
            .as_object()
            .expect("properties がある")
            .clone();
        assert!(properties.get("alias").is_none());
        assert!(
            properties["object"]["properties"]
                .as_object()
                .expect("object の properties がある")
                .get("alias")
                .is_none()
        );
    }

    #[test]
    fn set_scene_settings_schema_declares_no_alias_path_or_item_value() {
        // 応答が運ぶのはシーンの状態だけである。schema が alias・パス・設定値を
        // 宣言していれば、応答へ載せる実装が入っても検出できなくなる。
        let declared = set_scene_settings().to_string();
        for forbidden in ["alias", "path", "value"] {
            assert!(
                !declared.contains(forbidden),
                "{forbidden} が宣言されています: {declared}"
            );
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
            ("set_effect_enabled", set_effect_enabled()),
            ("move_effect", move_effect()),
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
        for schema in [
            set_object_item(),
            add_effect(),
            set_effect_enabled(),
            move_effect(),
        ] {
            assert!(
                check(&schema, &value, "$").is_err(),
                "effect の欠落を検出できていません"
            );
        }
    }

    /// 移動と設定変更を 1 件ずつ含む一括適用の結果。
    fn sample_batch_outcome() -> aviutl2_mcp_core::BatchOutcome {
        aviutl2_mcp_core::BatchOutcome {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            project_revision: 43,
            results: vec![
                aviutl2_mcp_core::BatchStepOutcome {
                    object: sample_object_summary(),
                    effect: None,
                },
                aviutl2_mcp_core::BatchStepOutcome {
                    object: sample_object_summary(),
                    effect: Some(sample_effect_info()),
                },
            ],
        }
    }

    #[test]
    fn apply_batch_schema_matches_dto() {
        assert_conforms(apply_batch(), &to_value(&sample_batch_outcome()));
    }

    #[test]
    fn apply_batch_schema_accepts_an_empty_result_list() {
        let mut outcome = sample_batch_outcome();
        outcome.results.clear();
        assert_conforms(apply_batch(), &to_value(&outcome));
    }

    #[test]
    fn batch_step_schema_refuses_a_revision_and_a_created_list() {
        // 要素ごとの revision は「各 sub-operation が自分の世代を持つ」と読める。
        // 作成の一覧は一括適用に作成が入らないため常に空である。どちらも
        // フィールドを持たないことを schema で言い切る。
        let schema = apply_batch();
        let declared: std::collections::BTreeSet<&str> =
            schema["properties"]["results"]["items"]["properties"]
                .as_object()
                .expect("要素の properties がある")
                .keys()
                .map(String::as_str)
                .collect();
        assert_eq!(
            declared,
            std::collections::BTreeSet::from(["object", "effect"]),
            "要素の schema が持たないはずのフィールドを宣言しています"
        );

        for key in ["project_revision", "created"] {
            let mut value = to_value(&sample_batch_outcome());
            value["results"][0]
                .as_object_mut()
                .expect("object")
                .insert(key.to_string(), json!(1));
            assert!(
                check(&apply_batch(), &value, "$").is_err(),
                "{key} の混入を検出できていません"
            );
        }
    }

    /// 描画の応答。
    fn sample_render_output() -> crate::mcp::render::RenderFrameOutput {
        use crate::mcp::render::{ArtifactRef, RenderFrameOutput};
        RenderFrameOutput {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            project_revision: 42,
            scene_id: 3,
            frame: 120,
            width: 1920,
            height: 1080,
            artifact: ArtifactRef {
                artifact_id: "5d0b6f7a-1f2e-4a3b-9c8d-7e6f5a4b3c2d".to_string(),
                uri: "aviutl2://artifacts/5d0b6f7a-1f2e-4a3b-9c8d-7e6f5a4b3c2d".to_string(),
                media_type: "image/png".to_string(),
                byte_length: 4096,
                sha256: format!("sha256:{}", "0".repeat(64)),
                expires_at: "2026-01-01T00:10:00+00:00".to_string(),
            },
        }
    }

    #[test]
    fn render_frame_schema_matches_dto() {
        assert_conforms(render_frame(), &to_value(&sample_render_output()));
    }

    #[test]
    fn render_frame_schema_refuses_a_handoff_token() {
        // 接続先の result をそのまま流すと引き渡しの識別子が漏れる。型を分けた
        // うえで、schema の側からも混入を検出できるようにしておく。
        assert!(
            !serde_json::to_string(&render_frame())
                .expect("直列化できる")
                .contains("handoff_token"),
            "schema が引き渡しの識別子を宣言しています"
        );

        for path in ["$", "artifact"] {
            let mut value = to_value(&sample_render_output());
            let target = if path == "$" {
                &mut value
            } else {
                &mut value[path]
            };
            target
                .as_object_mut()
                .expect("object")
                .insert("handoff_token".to_string(), json!("0123456789abcdef"));
            assert!(
                check(&render_frame(), &value, "$").is_err(),
                "{path} への混入を検出できていません"
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
