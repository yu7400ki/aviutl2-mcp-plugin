//! 読み取り tool の説明と入出力 schema の検査。

use super::*;

#[test]
fn read_tools_are_annotated_as_read_only() {
    for name in READ_TOOLS {
        let tool = tool_named(name);
        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{name} に annotation がありません"));
        assert_eq!(annotations.read_only_hint, Some(true), "{name}");
        assert_eq!(annotations.destructive_hint, Some(false), "{name}");
        assert_eq!(annotations.idempotent_hint, Some(true), "{name}");
        assert_eq!(annotations.open_world_hint, Some(false), "{name}");
    }
}

#[test]
fn the_effect_catalog_description_names_where_item_names_come_from() {
    // 一覧は設定項目の名前を返さない。どこで得られるかを書かなければ、
    // 名前を推測で組み立てるか、そもそも項目を触らないかのどちらかになる。
    let description = description_of("list_available_effects");
    assert!(
        description.contains("get_object"),
        "項目名の入手先が説明にありません: {description}"
    );
    assert!(
        description.contains("description"),
        "説明が付かない effect があることが書かれていません: {description}"
    );
}

#[test]
fn describe_effects_declares_the_same_limit_it_states() {
    // 上限は 3 か所——core の検証・入力 schema・tool の説明——に現れる。
    // 片方だけを動かすと、宣言と説明と実際の受理範囲が食い違う。
    let tool = tool_named("describe_effects");
    let names = &tool.input_schema["properties"]["effect_names"];
    assert_eq!(
        names["maxItems"],
        serde_json::json!(MAX_DESCRIBED_EFFECTS),
        "宣言した上限が core の上限と違います: {names}"
    );
    assert_eq!(names["minItems"], serde_json::json!(1), "{names}");
    // 重複は畳まずに拒否する。宣言だけを落としても、実際には拒否される。
    assert_eq!(names["uniqueItems"], serde_json::json!(true), "{names}");

    let description = description_of("describe_effects");
    assert!(
        description.contains(&format!("1〜{MAX_DESCRIBED_EFFECTS} 件")),
        "説明が宣言した上限を述べていません: {description}"
    );
}

#[test]
fn describe_effects_neither_declares_nor_promises_a_page() {
    // ページの続きという概念が無い。schema が受け付けないことと、説明が
    // そう述べていることを揃える。
    let tool = tool_named("describe_effects");
    let properties = tool.input_schema["properties"]
        .as_object()
        .expect("入力が properties を宣言していません");
    for field in ["offset", "limit", "snapshot_revision"] {
        assert!(
            !properties.contains_key(field),
            "{field} を宣言しています: {properties:?}"
        );
    }
    assert_eq!(
        tool.input_schema["required"],
        serde_json::json!(["instance_id", "effect_names"]),
        "describe_effects の必須項目"
    );
    assert!(
        description_of("describe_effects").contains("ページ指定を持たない"),
        "ページを取らないことが説明されていません"
    );
}

#[test]
fn the_catalog_tools_do_not_ask_for_a_scene_id() {
    // フォント・パレット・モジュール・エイリアスはシーンに紐づかない。何も
    // 守らない値を必須にすると、要求元は意味の無い値を用意することになる。
    for name in [
        "list_fonts",
        "list_palettes",
        "list_modules",
        "list_object_aliases",
    ] {
        let tool = tool_named(name);
        let schema = Value::Object(tool.input_schema.as_ref().clone()).to_string();
        assert!(
            !schema.contains("expected_scene_id"),
            "{name} がシーン ID を宣言しています"
        );
        assert_eq!(
            tool.input_schema["required"],
            serde_json::json!(["instance_id"]),
            "{name} の必須項目"
        );
    }
}

#[test]
fn the_list_modules_input_declares_exactly_the_types_it_accepts() {
    // 種別は SDK が定めた閉じた集合である。値を落とせば既存の要求が
    // invalid_argument になり、綴りを変えれば同じ要求が通らなくなる。
    // どちらも要求元から見れば契約の破壊である。
    let tool = tool_named("list_modules");
    let names = tool.input_schema["$defs"]["ModuleTypeInput"]["enum"]
        .as_array()
        .expect("種別が値の集合として宣言されていません");
    let names: Vec<&str> = names
        .iter()
        .map(|name| name.as_str().expect("種別名は文字列である"))
        .collect();
    assert_eq!(
        names,
        vec![
            "script_filter",
            "script_object",
            "script_camera",
            "script_track",
            "script_module",
            "plugin_input",
            "plugin_output",
            "plugin_filter",
            "plugin_generic",
        ]
    );
}

#[test]
fn the_catalog_tools_say_that_the_revision_is_not_matched() {
    // 受理するが照合しない値である。黙っていると、要求元は 2 ページ目が
    // 落ちない理由も、添えても取りこぼしが防げない理由も分からない。
    //
    // **述べる場所はフィールドの隣である。** 値を送るかどうかを決める時点で
    // 読まれ、共有の入力型に 1 度書けば該当する tool すべてへ届く。
    for name in catalog_page_tools() {
        let description = field_description(&name, "snapshot_revision");
        for phrase in [
            "受理するがページ間の照合に用いない",
            "revision に連動しない",
            "前のページが返した値をそのまま送り返しても拒否されない",
        ] {
            assert!(
                description.contains(phrase),
                "{name} の snapshot_revision が {phrase} に触れていません: {description}"
            );
        }
        assert!(
            !description_of(&name).contains("snapshot_revision"),
            "{name} の説明がページ指定を写しています"
        );
    }
}

/// tool がページ指定を共有の入力型から受けるか。
///
/// 型を共有しているため、`snapshot_revision` の説明は該当する tool すべてで
/// 同じ文になる。
///
/// **未知の tool 名で落とす。** 一覧を手書きの連結で持つと、共有の入力型へ
/// 相乗りした tool をそこへ足し忘れたときに、説明の共有も照合しない旨の
/// 明記も黙って未検査になる。
fn takes_the_catalog_page(name: &str) -> bool {
    match name {
        "list_available_effects"
        | "list_fonts"
        | "list_palettes"
        | "list_modules"
        | "list_object_aliases" => true,
        "list_instances"
        | "get_edit_info"
        | "get_current_scene"
        | "list_layers"
        | "list_objects"
        | "get_object"
        // 名前を名指しして引くため、続きのページという概念が無い。
        | "describe_effects"
        | "get_effect_item_values"
        | "get_selection"
        | "create_object"
        | "move_object"
        | "set_object_name"
        | "set_object_item"
        | "add_effect"
        | "set_effect_enabled"
        | "move_effect"
        | "delete_effect"
        | "delete_object"
        | "set_selection"
        | "set_layer_state"
        | "create_object_section"
        | "delete_object_section"
        | "move_object_section"
        | "set_grid_bpm"
        | "set_scene_settings"
        | "apply_batch"
        | "render_frame" => false,
        other => panic!("{other} がページ指定を共有するかが決まっていません"),
    }
}

/// ページ指定を共有の入力型から受ける tool の名前を、登録済みの集合から拾う。
fn catalog_page_tools() -> Vec<String> {
    tools()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .filter(|name| takes_the_catalog_page(name))
        .collect()
}

#[test]
fn the_catalog_tools_share_one_wording_for_the_unmatched_revision() {
    // 入力型を分けると一致が崩れる。文言を特定の対象へ寄せると、共有して
    // いる tool のうち 1 つにしか当てはまらない説明が残りの schema へ載る。
    let names = catalog_page_tools();
    assert!(
        names.len() > 1,
        "共有を確かめるには 2 つ以上の tool が要ります"
    );
    let wordings: Vec<String> = names
        .iter()
        .map(|name| {
            tool_named(name).input_schema["properties"]["snapshot_revision"]["description"]
                .as_str()
                .unwrap_or_else(|| panic!("{name} が snapshot_revision の説明を持ちません"))
                .to_string()
        })
        .collect();
    for (name, wording) in names.iter().zip(&wordings) {
        assert_eq!(
            wording, &wordings[0],
            "{name} の snapshot_revision の説明が他の tool と違います"
        );
        for target in ["effect", "フォント", "パレット", "モジュール", "エイリアス"]
        {
            assert!(
                !wording.contains(target),
                "{name} の snapshot_revision の説明が {target} を名指ししています"
            );
        }
    }
}

#[test]
fn the_list_object_aliases_input_flattens_the_page_and_asks_only_for_the_instance() {
    // ページ指定は他の列挙 tool と同じ平坦な形で受ける。入れ子で現れると、
    // 同じ意味の要求が tool ごとに違う形になる。
    let tool = tool_named("list_object_aliases");
    let properties = tool.input_schema["properties"]
        .as_object()
        .expect("入力が properties を宣言していません");
    for field in ["offset", "limit", "snapshot_revision", "label"] {
        assert!(
            properties.contains_key(field),
            "{field} が宣言されていません"
        );
    }
    assert!(
        !Value::Object(tool.input_schema.as_ref().clone())
            .to_string()
            .contains(r#""page""#),
        "ページ指定が入れ子として現れています"
    );
    assert_eq!(
        tool.input_schema["required"],
        serde_json::json!(["instance_id"]),
        "list_object_aliases の必須項目"
    );
    // 宣言した上限は接続前に実際へ確かめる。宣言だけを消しても要求元から
    // 見えるのは schema であり、検証が残っていることは伝わらない。
    assert_eq!(
        properties["label"]["maxLength"],
        serde_json::json!(crate::mcp::input::MAX_NAME_CHARS),
        "label の上限が宣言されていません"
    );
}

#[test]
fn the_effect_item_values_input_schema_declares_the_uniqueness_it_enforces() {
    // 重複した要求は `invalid_argument` で落ちる。件数の境界だけを宣言して
    // 一意性を伏せると、同じ 1 つのフィールドについて契約の一部だけが
    // 要求元から見えなくなる。検証の実体は core の validate である。
    let tool = tool_named("get_effect_item_values");
    for (field, max) in [
        ("frames", aviutl2_mcp_core::MAX_EVALUATED_FRAMES),
        ("items", aviutl2_mcp_core::MAX_EVALUATED_ITEMS),
    ] {
        let property = tool.input_schema["properties"][field].clone();
        assert_eq!(
            property["uniqueItems"],
            serde_json::json!(true),
            "{field} が一意性を宣言していません: {property}"
        );
        // 一意性が件数の宣言と同じ位置に付いていることまで見る。位置が
        // ずれると、宣言は在るのに要求元の検証器が読まない。
        assert_eq!(property["minItems"], serde_json::json!(1), "{field}");
        assert_eq!(property["maxItems"], serde_json::json!(max), "{field}");
    }
}
