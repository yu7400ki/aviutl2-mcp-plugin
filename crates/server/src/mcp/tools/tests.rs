//! tool の説明と入出力 schema の検査。

use crate::mcp::server::tests::{server, tool_named, tools};
use aviutl2_mcp_core::{
    AvailableEffectItem, EffectItemType, ItemValue, ItemWriteError, MAX_DESCRIBED_EFFECTS,
    ReadBackCheck, prepare_item_write, read_back_check,
};
use rmcp::ServerHandler;
use rmcp::model::Tool;
use serde_json::Value;

/// tool が frame / layer を入出力に持ち、0 始まりであることの明記が要るか。
///
/// **未知の tool 名で落とす。** 一覧を const で持つと、どちらにも書かれて
/// いない新しい tool が「明記が要らない」側の既定へ黙って落ちる。
fn takes_zero_based_numbers(name: &str) -> bool {
    match name {
        "list_instances"
        | "get_edit_info"
        | "get_current_scene"
        | "list_layers"
        | "list_objects"
        | "get_object"
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
        | "create_object_section"
        | "delete_object_section"
        | "move_object_section"
        | "set_layer_state"
        | "set_selection"
        | "apply_batch"
        | "render_frame" => true,
        // effect カタログだけを扱い、frame も layer も現れない。
        "list_available_effects" | "describe_effects" => false,
        // 登録物の一覧だけを扱い、frame も layer も現れない。
        "list_fonts" | "list_palettes" | "list_modules" | "list_object_aliases" => false,
        // BPM グリッドはシーンに属し、位置は秒で表す。フレーム番号も
        // レイヤー番号も現れない。
        "set_grid_bpm" => false,
        // シーン設定は名前・解像度・サンプリングレートだけを扱い、0 始まりの
        // 軸を 1 つも持たない。
        "set_scene_settings" => false,
        other => panic!("{other} が 0 始まりの番号を扱うかが定義されていません"),
    }
}

/// tool の入力・出力 schema にレイヤー番号かフレーム番号が現れるか。
///
/// [`takes_zero_based_numbers`] とは別の根拠である。前者は手書きの判定で
/// あり、後者は tool が実際に宣言している形から読める事実である。
fn schema_carries_a_layer_or_frame(tool: &Tool) -> bool {
    let input = Value::Object(tool.input_schema.as_ref().clone()).to_string();
    let output = tool
        .output_schema
        .as_ref()
        .map(|schema| Value::Object(schema.as_ref().clone()).to_string())
        .unwrap_or_default();
    ["layer", "frame"]
        .iter()
        .any(|name| input.contains(name) || output.contains(name))
}

#[test]
fn no_tool_that_declares_a_layer_or_frame_is_exempt_from_stating_the_origin() {
    // 起点の明記が要るかは手書きの判定である。番号を扱う tool をそこで
    // 「扱わない」側へ書き換えると、判定だけを読む検査は 2 つとも黙って
    // 素通りする。免除してよいのは番号を持たない tool だけであることを、
    // schema という別の根拠から確かめる。
    //
    // 逆向きは求めない。schema に番号が現れなくても説明が番号に触れる tool
    // があり、そちらは明記を求める側であって緩める側ではない。
    for tool in tools() {
        if !schema_carries_a_layer_or_frame(&tool) {
            continue;
        }
        assert!(
            takes_zero_based_numbers(&tool.name),
            "{} は schema に番号を宣言しているのに起点の明記を免除されています",
            tool.name
        );
    }
}

/// 読み取り専用の tool。
const READ_TOOLS: &[&str] = &[
    "list_instances",
    "get_edit_info",
    "get_current_scene",
    "list_layers",
    "list_objects",
    "get_object",
    "list_available_effects",
    "describe_effects",
    "get_effect_item_values",
    "get_selection",
    "list_fonts",
    "list_palettes",
    "list_modules",
    "list_object_aliases",
];

/// 編集 tool と、宣言する annotation。
///
/// 値は `destructive_hint` / `idempotent_hint` の組である。`read_only_hint` は
/// 全編集 tool で偽、`open_world_hint` も全 tool で偽であるため表に持たない。
///
/// 作成系を冪等と名乗らないのは、再送で重複して作られ得るためである。
/// 宛先の重複確認と対象の fingerprint により通常は防がれるが、annotation は
/// 「再送が安全である」と主張しない側へ倒す。
const EDIT_TOOL_ANNOTATIONS: &[(&str, bool, bool)] = &[
    ("create_object", false, false),
    ("move_object", false, true),
    ("set_object_name", false, true),
    ("set_object_item", false, true),
    ("add_effect", false, false),
    ("set_effect_enabled", false, true),
    // 同じ位置へ 2 度動かしても列は同じ状態になる。
    ("move_effect", false, true),
    ("delete_effect", true, true),
    ("delete_object", true, true),
    // 中間点の作成は作成系だが冪等と名乗る。あるフレームは境界であるか
    // 無いかのどちらかであり、再送しても重複して作られる余地が無い。
    ("create_object_section", false, true),
    // 中間点を消すとその位置の移動パラメータが失われ、同じ tool では戻せない。
    ("delete_object_section", true, true),
    ("move_object_section", false, true),
    // 表示を切ってもロックを掛けても内容は失われず、同じ tool で戻せる。
    // 同じ状態を 2 度設定しても追加の変更を起こさない。
    ("set_layer_state", false, true),
    ("set_selection", false, true),
    // 一覧全体が置き換わるが、同じ tool で別の一覧を書ける。同じ一覧を 2 度
    // 送っても追加の変更を起こさない。
    ("set_grid_bpm", false, true),
    // 破壊的と名乗る根拠は削除ではなく不可逆性である。削除は取り消しで戻るが、
    // シーン設定は戻らない。同じ値を 2 度設定しても追加の変更は起きないため
    // 冪等と名乗る。
    ("set_scene_settings", true, true),
];

/// 一括適用の tool 名。
const APPLY_BATCH: &str = "apply_batch";

/// 描画の tool 名。
const RENDER_FRAME: &str = "render_frame";

/// 一括適用と描画の tool、および宣言する annotation。
///
/// 値は `read_only_hint` / `destructive_hint` / `idempotent_hint` の組である。
/// `open_world_hint` は全 tool で偽であるため表に持たない。
///
/// 一括適用を冪等と名乗らないのは、冪等かどうかが中身に依存する一方、
/// annotation は tool 単位でしか付けられないためである。作成系と同じく、
/// 「再送が安全である」と主張しない側へ倒す。
const PHASE4_TOOL_ANNOTATIONS: &[(&str, bool, bool)] = &[
    (APPLY_BATCH, false, false),
    // 描画はプロジェクトを変更せず、同じ要求は同じ絵を返す。
    (RENDER_FRAME, true, true),
];

/// 登録済みの tool 名を、annotation の 3 表から引く。
///
/// **共有の一覧（`aviutl2_mcp_core::tool::all_tool_names`）とは別の出所で
/// ある。** 一方は annotation と説明を検査するための手書きの表、もう一方は
/// operation からの導出であり、**両者が router と一致することを別々の試験が
/// 固定する**（[`all_tools_are_registered`] と
/// [`the_registered_tools_match_the_shared_catalog`]）。
fn annotated_tool_names() -> impl Iterator<Item = &'static str> {
    READ_TOOLS
        .iter()
        .copied()
        .chain(EDIT_TOOL_ANNOTATIONS.iter().map(|(name, _, _)| *name))
        .chain(PHASE4_TOOL_ANNOTATIONS.iter().map(|(name, _, _)| *name))
}

/// tool が編集 tool の説明規約に従うか。
///
/// 一括適用は編集 tool の表には属さないが、運ぶ selector も取り消し単位も
/// 編集と同じであるため従う側に置く。読み取りと描画はプロジェクトを
/// 変更しないため従わない。
///
/// **未知の tool 名で落とす。** 一覧を手書きの連結で持つと、そこから外した
/// tool が説明の共通検査から黙って外れる。
fn follows_the_edit_conventions(name: &str) -> bool {
    match name {
        "create_object"
        | "move_object"
        | "set_object_name"
        | "set_object_item"
        | "add_effect"
        | "set_effect_enabled"
        | "move_effect"
        | "delete_effect"
        | "delete_object"
        | "create_object_section"
        | "delete_object_section"
        | "move_object_section"
        | "set_layer_state"
        | "set_selection"
        | "set_grid_bpm"
        | "set_scene_settings"
        | APPLY_BATCH => true,
        "list_instances"
        | "get_edit_info"
        | "get_current_scene"
        | "list_layers"
        | "list_objects"
        | "get_object"
        | "list_available_effects"
        | "describe_effects"
        | "get_effect_item_values"
        | "get_selection"
        | "list_fonts"
        | "list_palettes"
        | "list_modules"
        | "list_object_aliases"
        | RENDER_FRAME => false,
        other => panic!("{other} が編集の説明規約に従うかが定義されていません"),
    }
}

/// 編集の説明規約が掛かる tool。
fn edit_like_tools() -> Vec<&'static str> {
    annotated_tool_names()
        .filter(|name| follows_the_edit_conventions(name))
        .collect()
}

#[test]
fn the_edit_conventions_cover_the_editing_tools_and_the_batch() {
    // 集合そのものを固定する。判定を「従わない」側へ書き換えても、対象が
    // 減ったことに気付けるようにする。
    let covered: std::collections::BTreeSet<&str> = edit_like_tools().into_iter().collect();
    let expected: std::collections::BTreeSet<&str> = EDIT_TOOL_ANNOTATIONS
        .iter()
        .map(|(name, _, _)| *name)
        .chain(std::iter::once(APPLY_BATCH))
        .collect();
    assert_eq!(covered, expected);
}

#[test]
fn all_tools_are_registered() {
    // 公開する tool の集合は 3 つの表の和集合と一致する。表に載せずに登録
    // すると annotation も説明も検査されないまま公開される。
    let names: std::collections::BTreeSet<String> =
        tools().iter().map(|tool| tool.name.to_string()).collect();
    let expected: std::collections::BTreeSet<String> = annotated_tool_names()
        .map(|name| name.to_string())
        .collect();
    assert_eq!(names, expected);
    // 件数そのものも固定する。router と表の両方から同じ tool を落とすと、
    // 集合の一致だけでは検出できない。
    assert_eq!(names.len(), 32, "公開する tool の数が変わりました");
}

#[test]
fn the_registered_tools_match_the_shared_catalog() {
    // 切替の対象を列挙する側は rmcp の属性を見られないため、名前の一覧を
    // core が operation から導いている。規則から外れる tool を足すと、
    // ここで集合が食い違って落ちる。
    let registered: std::collections::BTreeSet<String> =
        tools().iter().map(|tool| tool.name.to_string()).collect();
    let shared: std::collections::BTreeSet<String> =
        aviutl2_mcp_core::tool::all_tool_names().collect();
    assert_eq!(registered, shared);
    assert!(
        shared.contains(aviutl2_mcp_core::tool::ALWAYS_ENABLED_TOOL),
        "常時有効な tool が一覧に含まれていません"
    );
}

#[test]
fn phase4_tools_are_annotated_as_documented() {
    for (name, read_only, idempotent) in PHASE4_TOOL_ANNOTATIONS {
        let tool = tool_named(name);
        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{name} に annotation がありません"));
        assert_eq!(
            annotations.read_only_hint,
            Some(*read_only),
            "{name} の readOnlyHint"
        );
        // 一括適用にも描画にも削除は入らない。
        assert_eq!(
            annotations.destructive_hint,
            Some(false),
            "{name} の destructiveHint"
        );
        assert_eq!(
            annotations.idempotent_hint,
            Some(*idempotent),
            "{name} の idempotentHint"
        );
        assert_eq!(annotations.open_world_hint, Some(false), "{name}");
    }
}

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
fn edit_tools_are_annotated_as_mutating() {
    for (name, destructive, idempotent) in EDIT_TOOL_ANNOTATIONS {
        let tool = tool_named(name);
        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{name} に annotation がありません"));
        assert_eq!(annotations.read_only_hint, Some(false), "{name}");
        assert_eq!(
            annotations.destructive_hint,
            Some(*destructive),
            "{name} の destructiveHint"
        );
        assert_eq!(
            annotations.idempotent_hint,
            Some(*idempotent),
            "{name} の idempotentHint"
        );
        assert_eq!(annotations.open_world_hint, Some(false), "{name}");
    }
}

/// tool 名から、その tool が返す result の schema を返す。
///
/// 未知の tool 名で落とす。tool を足したときに結線の検査から漏れない。
fn expected_output_schema(name: &str) -> Value {
    use crate::mcp::output_schema as schema;
    match name {
        "list_instances" => schema::list_instances(),
        "get_edit_info" => schema::edit_info(),
        "get_current_scene" => schema::current_scene(),
        "list_layers" => schema::list_layers(),
        "list_objects" => schema::list_objects(),
        "get_object" => schema::object_detail(),
        "list_available_effects" => schema::list_available_effects(),
        "describe_effects" => schema::describe_effects(),
        "list_fonts" => schema::list_fonts(),
        "list_palettes" => schema::list_palettes(),
        "list_modules" => schema::list_modules(),
        "list_object_aliases" => schema::list_object_aliases(),
        "get_effect_item_values" => schema::effect_item_values(),
        "get_selection" => schema::get_selection(),
        "create_object" => schema::create_object(),
        "move_object" => schema::move_object(),
        "set_object_name" => schema::set_object_name(),
        "set_object_item" => schema::set_object_item(),
        "add_effect" => schema::add_effect(),
        "set_effect_enabled" => schema::set_effect_enabled(),
        "move_effect" => schema::move_effect(),
        "delete_effect" => schema::delete_effect(),
        "delete_object" => schema::delete_object(),
        "create_object_section" => schema::create_object_section(),
        "delete_object_section" => schema::delete_object_section(),
        "move_object_section" => schema::move_object_section(),
        "set_layer_state" => schema::set_layer_state(),
        "set_selection" => schema::set_selection(),
        "set_grid_bpm" => schema::set_grid_bpm(),
        "set_scene_settings" => schema::set_scene_settings(),
        "apply_batch" => schema::apply_batch(),
        "render_frame" => schema::render_frame(),
        other => panic!("{other} の outputSchema が定義されていません"),
    }
}

#[test]
fn tools_declare_the_output_schema_of_their_own_result() {
    // schema そのものが DTO と一致していても、tool へ別の result の schema を
    // 結んでしまえば正常な応答が自分の宣言に適合しなくなる。結線まで固定する。
    for tool in tools() {
        let declared = tool
            .output_schema
            .as_ref()
            .unwrap_or_else(|| panic!("{} に outputSchema がありません", tool.name));
        assert_eq!(
            Value::Object(declared.as_ref().clone()),
            expected_output_schema(&tool.name),
            "{} が別の result の schema を宣言しています",
            tool.name
        );
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

/// 説明の一部を取り出す。
fn description_of(name: &str) -> String {
    tool_named(name)
        .description
        .as_ref()
        .unwrap_or_else(|| panic!("{name} に説明がありません"))
        .to_string()
}

/// 前提の epoch を要求が運ぶ tool。
///
/// 対象を指す selector を持たないため、これがプロジェクト境界を照合する
/// 材料になる。
const TOOLS_CARRYING_AN_EXPECTED_EPOCH: &[&str] = &[
    "create_object",
    "set_layer_state",
    "set_selection",
    "set_grid_bpm",
    "set_scene_settings",
];

#[test]
fn every_edit_tool_declares_where_the_project_boundary_is_matched() {
    // プロジェクト境界の照合材料は、要求のどこかに必ず在る——selector の中か、
    // 前提の epoch のどちらかである。**述べる場所は入力 schema である。**
    // tool の説明へ写すと、同じ 5 行が編集 tool の数だけ並ぶ一方、値を書く
    // 時点では読まれない。
    for name in edit_like_tools() {
        let schema = Value::Object(tool_named(name).input_schema.as_ref().clone()).to_string();
        assert!(
            schema.contains("project_epoch"),
            "{name} の入力 schema が境界の照合材料を持ちません"
        );
        assert!(
            !description_of(name).contains("expected_project_epoch"),
            "{name} の説明が入力 schema の写しを持っています"
        );
    }
}

#[test]
fn only_the_tools_that_may_carry_no_selector_ask_for_an_expected_epoch() {
    // 前提の epoch を運ぶのは、要求が selector を 1 つも運ばないことがある
    // tool だけである。必ず運ぶ tool へ宣言すると、同じ意味の値が 1 要求の
    // 2 か所へ並ぶ。どちらの側に属するかを表で固定するので、tool を足した
    // ときに素通りしない。
    for name in edit_like_tools() {
        let properties = tool_named(name).input_schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{name} に properties がありません"))
            .clone();
        if TOOLS_CARRYING_AN_EXPECTED_EPOCH.contains(&name) {
            assert!(
                properties.contains_key("expected_project_epoch"),
                "{name} が前提の epoch を宣言していません"
            );
            continue;
        }
        assert!(
            !properties.contains_key("expected_project_epoch"),
            "{name} が運べない前提の epoch を求めています"
        );
    }
}

/// 入力 schema が受け取った分。
struct InputSchemaLanding {
    /// 入力 schema で述べる句。
    phrase: &'static str,
    /// 句を運ぶ property の名前。
    ///
    /// **空なら schema のどこに在ってもよい**——共有の入力型そのものの説明が
    /// 持つ場合である。名前を挙げた行では、その property の説明だけを見る。
    /// 挙げないと、たまたま同じ語を含む無関係なフィールドが数に入り、
    /// 本来の置き場所が空になっても閾値を満たしてしまう。
    fields: &'static [&'static str],
    /// 句が届く tool の最小数。
    reaches: usize,
}

/// 層 1 から落とした反復句 1 件。
///
/// **行き先は 1 つとは限らない。** 引数の隣に置ける部分と、組み立ての段階で
/// 効く部分の両方を持つ事実があり、単一の行き先しか記録できない形にすると
/// 片方が黙って落ちる。
struct Relocation {
    /// 層 1 が述べていた事実。
    statement: &'static str,
    /// 層 1 で同じことを述べていた tool の数。
    was_stated_by: usize,
    /// その事実が実際に掛かる tool の数。
    ///
    /// **1 なら tool 固有であり、層 1 に残すべきものである。**
    /// [`was_stated_by`] とは別に持つ——複数 tool へ掛かるキーを 1 tool の
    /// 説明だけが解説している状態は、説明の数からは見えない。
    applies_to: usize,
    /// 層 1 から消えたことを確かめる句。
    dropped: &'static [&'static str],
    /// 入力 schema が受け取る分。
    to_input_schema: Option<InputSchemaLanding>,
    /// skill が受け取る分。**skill が本文に書く内容である。**
    to_skill: Option<&'static str>,
}

/// 層 1 から落とした反復句と、その行き先。
///
/// **「落とした」と「消えた」を区別する唯一の表である。** 各行について、
/// 句が tool の説明から消えていることと、行き先に在ることを確かめる。
///
/// **`to_skill` の側は [`the_conventions_handed_to_the_skill_are_in_its_body`]
/// が本文と突き合わせる。** 本表が層 3 側の検査の入力である——句がどこへ
/// 行ったかを記録しているのはここだけであり、skill の本文だけを読んでも
/// 「述べ足りない」ことは分からない。
/// 併せて持ち越す検査は [`CHECKS_HANDED_TO_THE_SKILL`] にある。
const RELOCATED_CONVENTIONS: &[Relocation] = &[
    Relocation {
        statement: "frame 番号と layer 番号はいずれも 0 始まりであり、UI の表示とは 1 ずれる",
        was_stated_by: 25,
        applies_to: 23,
        dropped: &[
            "番号はいずれも 0 始まり",
            "layer 番号は 0 始まり",
            "frame 番号は 0 始まり",
            "UI の表示とは異なる",
        ],
        to_input_schema: Some(InputSchemaLanding {
            phrase: "0 始まり",
            fields: &["layer", "frame", "frames", "layer_min", "layer_max"],
            reaches: 18,
        }),
        // 起点そのものは値の隣で足りるが、**UI と 1 ずれることは引数の隣に
        // 書いても遅い。** 画面で見た番号をそのまま送る判断は、要求を組み立てる
        // 前に起きる。
        to_skill: Some(
            "AviUtl2 の UI はレイヤーとフレームを 1 始まりで表示する。\
             tool が受け渡すのは 0 始まりの番号であり、画面で見た番号より 1 小さい",
        ),
    },
    Relocation {
        statement: "応答が返した selector は組み立て直さず、読み直さずにそのまま次の要求へ渡せる",
        was_stated_by: 12,
        applies_to: 14,
        dropped: &["読み直さずにそのまま次の編集へ渡せる"],
        to_input_schema: Some(InputSchemaLanding {
            phrase: "読み直さずにそのまま次の要求へ渡せる",
            fields: &[],
            reaches: 14,
        }),
        to_skill: Some(
            "selector は自分で組み立てない。読み取りの応答が返した値をそのまま編集へ渡し、\
             編集の応答が返した値をそのまま次の編集へ渡す",
        ),
    },
    Relocation {
        statement: "プロジェクトの世代は selector が運ぶ project_epoch で照合する",
        was_stated_by: 11,
        applies_to: 14,
        dropped: &["selector が運ぶ project_epoch"],
        to_input_schema: Some(InputSchemaLanding {
            phrase: "プロジェクトの世代はこの値で照合",
            fields: &["project_epoch"],
            reaches: 14,
        }),
        to_skill: Some(
            "プロジェクト境界の照合材料は selector が運ぶ project_epoch である。\
             要求が selector を 1 つも運ばないことがある tool（create_object・\
             set_layer_state・set_selection・set_grid_bpm・set_scene_settings）だけが\
             expected_project_epoch を要求し、そちらは省略できない",
        ),
    },
    Relocation {
        statement: "対象が変化していた precondition_failed は、対象の現在の姿を details へ添える。\
                    読み直さずにそのまま次の要求の selector にできる",
        was_stated_by: 11,
        applies_to: 12,
        dropped: &["details.current_object"],
        // **キー名は層 2 に置けない。** 共有型は apply_batch の schema にも
        // 入るが、そちらは同じものを failed_object という別の名前で返す。
        // 層 2 が名乗れるのは「同じ形が添う」ことまでである。
        to_input_schema: Some(InputSchemaLanding {
            phrase: "対象の現在の姿",
            fields: &[],
            reaches: 14,
        }),
        to_skill: Some(
            "対象が変化していた precondition_failed では details.current_object に\
             対象の現在の姿が入り、そのまま次の要求の selector にできる。\
             apply_batch だけは何番目で落ちたかを併せて示すため details.failed_object という\
             別のキーで返す",
        ),
    },
    Relocation {
        statement: "offset と limit（1〜200、既定 50）でページを指定し、\
                    2 ページ目以降は先頭ページが返した snapshot_revision を添える",
        was_stated_by: 11,
        applies_to: 9,
        dropped: &["offset と limit（1〜200、既定 50）"],
        to_input_schema: Some(InputSchemaLanding {
            phrase: "1 以上 200 以下",
            fields: &["limit"],
            reaches: 9,
        }),
        to_skill: None,
    },
    Relocation {
        statement: "カタログ列挙の snapshot_revision は受理されるが照合には用いない",
        was_stated_by: 5,
        applies_to: 5,
        dropped: &["snapshot_revision は受理するがページ間の照合には用いない"],
        to_input_schema: Some(InputSchemaLanding {
            phrase: "受理するがページ間の照合に用いない",
            fields: &["snapshot_revision"],
            reaches: 5,
        }),
        to_skill: None,
    },
    Relocation {
        statement: "要求は project_revision を運ばない。\
                    読み取りから編集までに revision が進んでいても拒否されない",
        was_stated_by: 16,
        applies_to: 16,
        dropped: &["project_revision を運ばない"],
        // 引数に無いものの不在は、引数の隣に書けない。
        to_input_schema: None,
        to_skill: Some(
            "要求は project_revision を運ばない。読み取りから編集までに revision が\
             進んでいても拒否されない。拒否を避けるために revision を取り直す必要は無い",
        ),
    },
    Relocation {
        statement: "変更が起きた編集 tool の呼び出し 1 回が、1 つの取り消し単位になる。\
                    まとめて 1 単位にしたいときは apply_batch を選ぶ",
        was_stated_by: 12,
        applies_to: 16,
        dropped: &["この呼び出し 1 回が 1 つの取り消し単位になる"],
        to_input_schema: None,
        to_skill: Some(
            "変更が起きた編集 tool の呼び出し 1 回が、1 つの取り消し単位になる。\
             まとめて 1 単位にしたいときは apply_batch を選ぶ",
        ),
    },
    Relocation {
        statement: "timeout は変更が無かったことを意味しない。\
                    details.change_applied が \"no\" なら未適用のため再送してよく、\
                    \"unknown\" なら読み直して確認してから再送する",
        was_stated_by: 16,
        applies_to: 16,
        dropped: &["details.change_applied"],
        to_input_schema: None,
        to_skill: Some(
            "timeout は変更が無かったことを意味しない。details.change_applied が \"no\" なら\
             未適用のため再送してよく、\"unknown\" なら読み直して確認してから再送する",
        ),
    },
    Relocation {
        // **層 1 で述べていたのは 1 tool だけだが、キーは汎用である。**
        // 書き込みを発行した後に落ちた失敗すべてに付き、一括適用の
        // sub-operation でも立つ。1 tool の説明が全編集経路のキーを解説して
        // いる状態は、説明の数からは見えない。
        statement: "details.mutation_issued は、その失敗の時点で書き込みが\
                    発行済みだったかを示す",
        was_stated_by: 1,
        applies_to: 16,
        dropped: &["details.mutation_issued"],
        to_input_schema: None,
        to_skill: Some(
            "書き込みを発行した後に落ちた失敗には details.mutation_issued が true で付く。\
             付かない失敗は 1 バイトも書いていないため、対象を読み直さずに要求を直して\
             送り直せる。付く失敗が読み直しを要するかは details.retry_requires が名乗る\
             ——発行した変更が戻っていれば読み直す先は無い",
        ),
    },
    Relocation {
        // 値の書式は値を書く場所の隣が正本である。層 1 にも置くと、
        // 書式が変わったときに片方だけが古くなる。
        statement: "色は 16 進 6 桁で指定する",
        was_stated_by: 1,
        applies_to: 2,
        dropped: &["色は 16 進 6 桁で指定する"],
        to_input_schema: Some(InputSchemaLanding {
            phrase: "16 進 6 桁",
            fields: &[],
            reaches: 2,
        }),
        to_skill: None,
    },
    Relocation {
        // 値の選び方そのものは、どの tool を呼ぶかを決める前に効く。
        statement: "設定項目に書ける値は describe_effects の choices と range から、\
                    フォント名は list_fonts から、表に無い項目は既存オブジェクトの値から得る",
        was_stated_by: 1,
        applies_to: 2,
        dropped: &[
            "選べる値と値域は describe_effects が返す",
            "登録済みのフォント名は list_fonts が返す",
        ],
        to_input_schema: None,
        to_skill: Some(
            "設定項目に何を書けるか分からないときは describe_effects を呼ぶ。\
             choices が候補を、range が値域と小数桁を返す。どちらも null の項目は\
             表に載っていないだけであり、既存オブジェクトの値を get_object で読んで倣う。\
             フォント名は list_fonts が返す",
        ),
    },
];

/// 層 3 が受け取る検査 1 件。
struct HandedCheck {
    /// 層 1 に対して確かめていたこと。
    checked: &'static str,
    /// skill 側で何を確かめる形になるか。
    becomes: &'static str,
}

/// 層 3 へ持ち越す検査。
///
/// **削除ではない。** いずれも「説明が嘘をつかないこと」を守っていた検査で
/// あり、句を動かすなら検査も動かす。skill を書く作業はこの表を入力に取る。
const CHECKS_HANDED_TO_THE_SKILL: &[HandedCheck] = &[
    HandedCheck {
        checked: "編集 tool すべての説明が「要求は project_revision を運ばない」と述べること",
        becomes: "SKILL.md の本文が同じことを 1 度述べること",
    },
    HandedCheck {
        checked: "10 tool の説明が「この呼び出し 1 回が 1 つの取り消し単位になる」と述べること",
        becomes: "SKILL.md が一般則を 1 度述べ、例外（set_selection と set_grid_bpm は\
                  単位を作らない、set_scene_settings は取り消せない）を名指しすること。\
                  層 1 に残る表明は [`undo_statement`] が持つ",
    },
    HandedCheck {
        checked: "編集 tool すべての説明が details.change_applied の 3 値の読み方を述べること",
        becomes: "SKILL.md が timeout を受けた後の手順を 1 度述べること。\
                  値そのものは失敗の text content へ出るため、書くのは読み方だけでよい",
    },
    HandedCheck {
        checked: "set_object_item の説明が、書ける値の入手先（describe_effects・list_fonts・\
                  get_object）を述べること",
        becomes: "SKILL.md が候補を引く経路を 1 度述べること。**候補の値そのものは写さない**\
                  ——正本は describe_effects が返す表である",
    },
];

#[test]
fn the_phrases_dropped_from_the_tool_descriptions_live_in_another_layer() {
    // **「落とした」と「消えた」を区別する唯一の検査である。**
    // 層 1 から句が消えたことだけを見ると、どこにも無い状態が通ってしまう。
    let descriptions: Vec<(String, String)> = tools()
        .into_iter()
        .map(|tool| (tool.name.to_string(), description_of(&tool.name)))
        .collect();
    let schemas: Vec<(String, String)> = tools()
        .into_iter()
        .map(|tool| {
            (
                tool.name.to_string(),
                Value::Object(tool.input_schema.as_ref().clone()).to_string(),
            )
        })
        .collect();

    for relocation in RELOCATED_CONVENTIONS {
        assert!(
            relocation.applies_to > 1,
            "1 tool にしか掛からない事実は層 1 に残すものです: {}",
            relocation.statement
        );
        // **表が記録するのは移設であって新設ではない。** 層 1 が 1 度も
        // 述べていなかった事実をここへ足すと、落とした句の帳尻が合わなくなる。
        assert!(
            relocation.was_stated_by >= 1,
            "層 1 が述べていなかった事実が表に在ります: {}",
            relocation.statement
        );
        // 行き先が 1 つも無い行は、落としただけの行である。
        assert!(
            relocation.to_input_schema.is_some() || relocation.to_skill.is_some(),
            "行き先の無い句が表に在ります（層 1 の {} tool が述べていました）: {}",
            relocation.was_stated_by,
            relocation.statement
        );
        for phrase in relocation.dropped {
            for (name, description) in &descriptions {
                assert!(
                    !description.contains(phrase),
                    "{name} の説明が層 1 から落とした句を残しています: {phrase}"
                );
            }
        }
        if let Some(landing) = &relocation.to_input_schema {
            let reached = tools()
                .into_iter()
                .filter(|tool| {
                    let schema = Value::Object(tool.input_schema.as_ref().clone());
                    if landing.fields.is_empty() {
                        // 共有の入力型そのものの説明が持つ。
                        return schema.to_string().contains(landing.phrase);
                    }
                    property_descriptions(&schema).iter().any(|(field, text)| {
                        landing.fields.contains(&field.as_str()) && text.contains(landing.phrase)
                    })
                })
                .count();
            assert!(
                reached >= landing.reaches,
                "{} が入力 schema で {} tool にしか届いていません（{} 以上を期待）",
                relocation.statement,
                reached,
                landing.reaches
            );
        }
        if let Some(statement) = relocation.to_skill {
            // 本文との突き合わせは
            // [`the_conventions_handed_to_the_skill_are_in_its_body`] が行う。
            // ここで見るのは、行き先の宣言が空でないことだけである。
            assert!(!statement.is_empty(), "skill が受け取る内容が空です");
        }
    }

    // schemas は層 1 と層 2 の両方を 1 度に見るために組む。落とした句が
    // schema 側へも現れていないかを、行き先の宣言と突き合わせる。
    for relocation in RELOCATED_CONVENTIONS {
        if relocation.to_input_schema.is_some() {
            continue;
        }
        for phrase in relocation.dropped {
            for (name, schema) in &schemas {
                assert!(
                    !schema.contains(*phrase),
                    "{name} の入力 schema が、入力 schema へ移さないと決めた句を持っています: {phrase}"
                );
            }
        }
    }

    assert!(
        RELOCATED_CONVENTIONS
            .iter()
            .any(|relocation| relocation.to_skill.is_some()),
        "skill が受け取る行が 1 つもありません"
    );
    for check in CHECKS_HANDED_TO_THE_SKILL {
        assert!(
            !check.checked.is_empty() && !check.becomes.is_empty(),
            "持ち越す検査の記録が欠けています"
        );
    }
}

#[test]
fn the_server_instructions_carry_no_convention_that_lives_in_another_layer() {
    // 層 0 が答えるのは「この server は何か」であり、接続時に 1 度だけ
    // 読まれる。**層 1 から落とした句をここへ寄せると、正本が 3 つになる。**
    // 移設の表を入力に取り、行き先が層 2 と層 3 に決まった句が層 0 にも
    // 現れていないことを見る。
    let instructions = ServerHandler::get_info(&server())
        .instructions
        .expect("層 0 の説明がありません");
    for relocation in RELOCATED_CONVENTIONS {
        for phrase in relocation.dropped {
            assert!(
                !instructions.contains(phrase),
                "層 0 が層 1 から落とした句を抱えています: {phrase}"
            );
        }
    }
    // **tool を名指ししない。** 名指しは必ず「その tool がどういうものか」を
    // 伴い、しかも数え漏らしても誰も気付けない——ここに在った
    // 「selector を持たない create_object と set_selection」は、実際には
    // 5 tool を数え落としていた。
    for tool in tools() {
        assert!(
            !instructions.contains(tool.name.as_ref()),
            "層 0 が {} を名指ししています",
            tool.name
        );
    }
    assert!(
        !instructions.contains("expected_project_epoch"),
        "層 0 が入力 schema の写しを持っています"
    );
}

/// 同梱する skill の `SKILL.md` を読む。
///
/// **本文は plugin crate が持つ**——skill は plugin のバイナリへ埋め込まれ、
/// plugin が書き出す。それでも突き合わせをこちら側で行うのは、**層 1 から
/// 何を落としたかの記録がこの crate にしか無い**ためである。本文の側だけを
/// 読んでも、述べ足りない句は見えない。
fn skill_body() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates ディレクトリを辿れません")
        .join("plugin")
        .join("data")
        .join("skills")
        .join("aviutl2-editing")
        .join("SKILL.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} を読めません: {e}", path.display()))
}

/// 本文と句を、markdown の装飾と改行の差を無視して比べられる形へ均す。
///
/// 表が持つ句は 1 続きの文であり、本文では折り返され、語の一部が
/// コードや強調として囲まれる。**均さずに比べると、体裁を整えただけで
/// 検査が落ちる。** 均すのは空白とバッククォートとアスタリスクだけで、
/// 下線は残す——`set_object_item` のような名前が別語に化ける。
fn without_markdown_noise(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_whitespace() && !matches!(ch, '`' | '*'))
        .collect()
}

/// 均した本文の中に句が現れる回数。
fn occurrences(body: &str, phrase: &str) -> usize {
    without_markdown_noise(body)
        .matches(&without_markdown_noise(phrase))
        .count()
}

#[test]
fn the_conventions_handed_to_the_skill_are_in_its_body() {
    // **C-T1 の層 3 側である。** 層 1 から句が消えたことは
    // [`the_phrases_dropped_from_the_tool_descriptions_live_in_another_layer`]
    // が見ている。こちらが見るのは、その行き先に句が在ることである。
    // 両方を持って初めて「落とした」と「消えた」が区別できる。
    let body = skill_body();
    let mut handed = 0usize;
    for relocation in RELOCATED_CONVENTIONS {
        let Some(statement) = relocation.to_skill else {
            continue;
        };
        handed += 1;
        assert!(
            occurrences(&body, statement) >= 1,
            "層 1 の {} tool が述べていた事実が skill の本文にありません: {statement}",
            relocation.was_stated_by
        );
    }
    assert!(handed > 0, "skill が受け取った句が 1 つもありません");
}

#[test]
fn the_checks_handed_to_the_skill_are_satisfied_by_its_body() {
    // **削除ではなく移設であることを、移設先で確かめる。** 4 件はいずれも
    // 「説明が嘘をつかないこと」を守っていた検査であり、層 1 から消した
    // 時点で守り手が居なくなっている。
    let body = skill_body();
    assert_eq!(
        CHECKS_HANDED_TO_THE_SKILL.len(),
        4,
        "持ち越す検査が増減しています。本文側の検査も併せて見直してください"
    );

    // 1. 要求が project_revision を運ばないこと。**1 度だけ述べる**——
    //    層 1 で 16 tool へ並んでいた句を、層 3 でも並べては移した意味が無い。
    assert_eq!(
        occurrences(&body, "project_revision を運ばない"),
        1,
        "project_revision を運ばないことの記述が 1 度ではありません"
    );

    // 2. 取り消し単位。一般則を 1 度述べ、例外を名指しし、確かめていない
    //    ものを確かめていないと述べる。**どれを名指しするかは層 1 の
    //    [`undo_statement`] が持つ**——手書きの一覧にすると、tool を足した
    //    ときに片方だけが古くなる。
    assert_eq!(
        occurrences(&body, "1 つの取り消し単位になる"),
        1,
        "取り消し単位の一般則が 1 度ではありません"
    );
    let mut exceptions = 0usize;
    for name in edit_like_tools() {
        match undo_statement(name) {
            UndoStatement::NoUnitAndJumpsBack => {
                exceptions += 1;
                assert!(
                    body.lines()
                        .any(|line| line.contains(name) && line.contains("取り消し単位を作らない")),
                    "{name} が単位を作らない例外として名指しされていません"
                );
            }
            UndoStatement::NotUndoableAndJumpsBack => {
                exceptions += 1;
                assert!(
                    body.lines()
                        .any(|line| line.contains(name) && line.contains("取り消せない")),
                    "{name} が取り消せない例外として名指しされていません"
                );
            }
            UndoStatement::ItsWholePurpose | UndoStatement::FollowsTheGeneralRule => {}
        }
    }
    assert!(exceptions > 0, "例外が 1 つも名指しされていません");
    // **確かめていないと名乗る行を残さない。** 一般則に従うと分かった tool へ
    // 札が残ると、読み手は 1 回の Undo で戻る手順を避けることになる。
    assert!(
        !body
            .lines()
            .any(|line| line.contains("取り消し") && line.contains("確かめていない")),
        "取り消し単位を確かめていないと述べる行が残っています"
    );

    // 3. timeout を受けた後の手順。**値そのものは失敗の text content へ出る**
    //    ため、本文が持つのは読み方だけでよい。
    assert_eq!(
        occurrences(&body, "details.change_applied"),
        1,
        "timeout の後の手順が 1 度ではありません"
    );

    // 4. 候補を引く経路。**候補の値そのものは写さない**——正本は
    //    describe_effects が返す表である。写しが無いことは plugin crate の
    //    検査が基底の表と突き合わせて見る。
    assert_eq!(
        occurrences(&body, "describe_effects を呼ぶ"),
        1,
        "候補を引く経路が 1 度ではありません"
    );
    assert!(
        occurrences(&body, "正本は describe_effects が返す表") >= 1,
        "候補の正本がどこに在るかを本文が述べていません"
    );
}

/// effect の列の変化が兄弟 effect の selector も無効にすることを述べる tool。
///
/// **述べる場所は層 1 である。** 起こすのはこの 3 tool だけであり、
/// 反復句にならない。加えて、応答が返すのは足した／消した／動かした effect の
/// selector だけであるため、共有の selector 型が述べる「応答が返した新しい
/// selector へ持ち替える」では兄弟を回復できない——回復の手段（get_object を
/// 引き直す）を名指しできるのは、列を変える tool の説明だけである。
///
/// 第 2 要素は、その tool が兄弟を巻き込む理由を述べる語句である。増減と
/// 移動では巻き込み方が違うため、同じ 1 文にはならない。
const TOOLS_THAT_INVALIDATE_SIBLING_EFFECTS: &[(&str, &str)] = &[
    (
        "add_effect",
        "effect の増減は、同じオブジェクトが持つ他の effect の selector も無効にする",
    ),
    (
        "delete_effect",
        "effect の増減は、同じオブジェクトが持つ他の effect の selector も無効にする",
    ),
    ("move_effect", "移動は間にある effect の位置もずらす"),
];

#[test]
fn the_tools_that_change_the_effect_column_say_the_siblings_go_stale() {
    // 実測では、effect を足して消すとオブジェクトと兄弟 effect の fingerprint が
    // いずれも足す前の値へ完全に戻った。fingerprint は純粋な内容ハッシュで
    // あり、列の変化は兄弟まで巻き込む。述べなければ、要求元は手元の兄弟
    // selector を使い続けて precondition_failed を踏み、対象を読み直すことに
    // なる。
    for (name, cause) in TOOLS_THAT_INVALIDATE_SIBLING_EFFECTS {
        let description = description_of(name);
        for phrase in [*cause, "兄弟 effect", "get_object を引き直す"] {
            assert!(
                description.contains(phrase),
                "{name} の説明が {phrase} に触れていません: {description}"
            );
        }
    }
    // 起こさない tool が述べると、掛からない制約として読まれる。
    for name in edit_like_tools() {
        if TOOLS_THAT_INVALIDATE_SIBLING_EFFECTS
            .iter()
            .any(|(listed, _)| *listed == name)
        {
            continue;
        }
        assert!(
            !description_of(name).contains("兄弟 effect"),
            "{name} の説明が起こさない無効化を述べています"
        );
    }
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
fn the_create_object_input_declares_exactly_the_sources_it_accepts() {
    // 作成元は判別子つきの union であり、未知フィールドを拒否する。variant を
    // 落とせば既存の要求が invalid_argument になり、タグ名を変えれば同じ要求が
    // 通らなくなる。どちらも要求元から見れば契約の破壊である。
    // 出力側と違い、入力 schema を丸ごと固定する検査は無い。作成元だけは
    // ここで塞ぐ。
    let tool = tool_named("create_object");
    let variants = tool.input_schema["$defs"]["ObjectSourceInput"]["oneOf"]
        .as_array()
        .expect("作成元が判別子つきの union として宣言されていません");

    let tags: Vec<&str> = variants
        .iter()
        .map(|variant| {
            variant["properties"]["type"]["const"]
                .as_str()
                .expect("判別子が固定値として宣言されていません")
        })
        .collect();
    assert_eq!(
        tags,
        vec!["media_file", "object_alias", "effect", "alias_name"]
    );

    // 判別子と対になる値のフィールド名も固定する。タグだけが合っていても、
    // 値の名前が動けば要求は通らない。
    let fields: Vec<Vec<&str>> = variants
        .iter()
        .map(|variant| {
            variant["required"]
                .as_array()
                .expect("必須フィールドが宣言されていません")
                .iter()
                .map(|field| field.as_str().expect("フィールド名"))
                .collect()
        })
        .collect();
    assert_eq!(
        fields,
        vec![
            vec!["type", "path"],
            vec!["type", "alias"],
            vec!["type", "name"],
            vec!["type", "name"],
        ]
    );
}

/// 応答が返す位置が要求した宛先と一致するとは限らない tool。
///
/// ホストが配置を調整し得るため、成功を「要求どおりの位置」と読むと、
/// 呼び出し側が組み立てた次の要求は別の場所を指す。どちらの側に属するかを
/// 表で固定するので、tool を足したときに素通りしない。
const TOOLS_WHOSE_RESPONSE_CARRIES_THE_ACTUAL_PLACEMENT: &[&str] =
    &["create_object", "move_object"];

#[test]
fn tools_that_can_land_elsewhere_say_the_response_carries_the_actual_placement() {
    for name in edit_like_tools() {
        let description = description_of(name);
        if TOOLS_WHOSE_RESPONSE_CARRIES_THE_ACTUAL_PLACEMENT.contains(&name) {
            for keyword in [
                "応答が返す位置は要求した宛先と異なり得る",
                "配置を確かめるには応答の値を見る",
            ] {
                assert!(
                    description.contains(keyword),
                    "{name} の説明に {keyword} がありません"
                );
            }
            continue;
        }
        assert!(
            !description.contains("実際の配置"),
            "{name} の説明が持たない性質を述べています"
        );
    }
}

/// tool の説明が取り消しについて述べる内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UndoStatement {
    /// 呼び出し全体が 1 つの取り消し単位になると述べる。
    ///
    /// **一括適用にとってはそれが tool の目的そのものである。** 一般則の
    /// 言い換えではないため層 1 に残る。
    ItsWholePurpose,
    /// 一般則（1 回の呼び出しが 1 つの取り消し単位になる）に従う。
    ///
    /// **層 1 では述べない。** 編集 tool すべてに掛かる規約であり、層 1 へ
    /// 写すと同じ 1 行が tool の数だけ並ぶ。組み立ての段階で効く——
    /// `apply_batch` を選ぶかの判断材料である。
    FollowsTheGeneralRule,
    /// 取り消し単位を作らず、取り消しが 1 つ前の編集へ飛ぶと述べる。
    ///
    /// **一般則の例外である。** 例外は一般則を置いた層には書けない——
    /// 一般則を読む全 tool へ伝わってしまう。
    NoUnitAndJumpsBack,
    /// 取り消せないことを説明の冒頭で述べ、取り消しが 1 つ前の編集へ飛ぶと
    /// 述べる。
    ///
    /// **冒頭に置くことまでを含めて固定する。** 末尾では、説明を要約する
    /// 要求元が落とす。
    NotUndoableAndJumpsBack,
}

/// tool 名から、説明が取り消しについて述べる内容を引く。
///
/// 未知の tool 名で落とす。**説明は保証である**ため、述べるか黙るかの判断を
/// tool ごとに 1 か所へ置き、tool を足したときに素通りしないようにする。
fn undo_statement(name: &str) -> UndoStatement {
    match name {
        "apply_batch" => UndoStatement::ItsWholePurpose,
        "create_object"
        | "move_object"
        | "set_object_name"
        | "set_object_item"
        | "add_effect"
        | "set_effect_enabled"
        | "move_effect"
        | "delete_effect"
        | "delete_object"
        | "set_layer_state"
        | "create_object_section"
        | "delete_object_section"
        | "move_object_section" => UndoStatement::FollowsTheGeneralRule,
        // BPM グリッドはオブジェクトの編集ではない。SDK が編集区間の中で
        // Undo へ登録すると述べているのはオブジェクトについてであり、実機でも
        // 直後の取り消しは 1 つ前の編集へ飛んだ。
        "set_selection" | "set_grid_bpm" => UndoStatement::NoUnitAndJumpsBack,
        // SDK は 3 つの setter を Undo 非対応と明記している。取り消せない
        // ことは、要求を出す前に読まれる場所へ置く。
        "set_scene_settings" => UndoStatement::NotUndoableAndJumpsBack,
        other => panic!("{other} の取り消しの説明が定義されていません"),
    }
}

#[test]
fn edit_tool_descriptions_state_the_undo_boundary() {
    for name in edit_like_tools() {
        let description = description_of(name);
        match undo_statement(name) {
            UndoStatement::ItsWholePurpose => assert!(
                description.contains("1 つの取り消し単位"),
                "{name} の説明に取り消し単位がありません"
            ),
            // 一般則に従う tool は層 1 で黙る。言い換えも塞ぐ——1 つの語だけを
            // 見ていると、別の言い回しで同じ保証が入り込む。
            UndoStatement::FollowsTheGeneralRule => {
                for forbidden in ["取り消し単位", "取り消し", "元に戻", "Undo", "undo"]
                {
                    assert!(
                        !description.contains(forbidden),
                        "{name} の説明が層 3 の一般則か未確認の挙動に触れています: {forbidden}"
                    );
                }
            }
            UndoStatement::NoUnitAndJumpsBack => {
                // 「戻る保証が無い」は「戻るかもしれない」と読める。実際は
                // 戻らないうえに取り消しが 1 つ前の編集まで飛ぶため、失う
                // ものを名指しする。
                assert!(
                    description.contains("取り消し単位を作らない"),
                    "{name} の説明に取り消し単位を作らない旨がありません"
                );
                assert!(
                    description.contains("その前に行った編集が取り消される"),
                    "{name} の説明が取り消しの飛び先を述べていません"
                );
                assert!(
                    !description.contains("1 つの取り消し単位"),
                    "{name} の説明が取り消し単位を作ると読めます"
                );
            }
            UndoStatement::NotUndoableAndJumpsBack => {
                // 要約する要求元は末尾を落とすため、冒頭に在ることまでを
                // 固定する。
                assert!(
                    description.starts_with("この操作は取り消せない"),
                    "{name} の説明が取り消せないことを冒頭で述べていません"
                );
                assert!(
                    description.contains("その前に行った編集が取り消される"),
                    "{name} の説明が取り消しの飛び先を述べていません"
                );
                assert!(
                    !description.contains("1 つの取り消し単位"),
                    "{name} の説明が取り消し単位を作ると読めます"
                );
            }
        }
    }
}

/// 説明が「選択肢から選ぶ種別」として挙げている名前を並びごと取り出す。
///
/// 語を含むかではなく、挙げている一覧そのものを取り出す。含むかだけを見ると、
/// 一覧から種別が落ちても増えても気付けない。
fn choice_item_types_named_in(description: &str) -> Vec<String> {
    const OPENING: &str = "選択肢から選ぶ種別（";
    let start = description
        .find(OPENING)
        .expect("説明が選択肢から選ぶ種別を挙げていません")
        + OPENING.len();
    let rest = &description[start..];
    let end = rest.find('）').expect("種別の一覧が閉じていません");
    rest[..end].split('・').map(str::to_string).collect()
}

/// 説明が `item_type が …` の形で挙げている種別の一覧を、並びごと取り出す。
///
/// `closing` の直前に在る一覧を取る。同じ形が 1 つの説明に複数現れるため、
/// 一覧を閉じる語で位置を決める。語を含むかではなく一覧そのものを取り出す。
/// 含むかだけを見ると、一覧から種別が落ちても増えても気付けない。
fn item_types_named_before(description: &str, closing: &str) -> Vec<String> {
    const OPENING: &str = "item_type が ";
    let end = description
        .find(closing)
        .unwrap_or_else(|| panic!("説明が {closing} を述べていません: {description}"));
    let head = &description[..end];
    let start = head
        .rfind(OPENING)
        .unwrap_or_else(|| panic!("説明が種別を挙げていません: {description}"))
        + OPENING.len();
    head[start..].split('・').map(str::to_string).collect()
}

/// 移動を含まない値を渡すときの対象。
///
/// 移動の検証は対象を見なければ成立しないが、ここで見るのは種別と値の形の
/// 対応であり、移動は渡さない。
fn no_track_target() -> aviutl2_mcp_core::TrackWriteTarget<'static> {
    aviutl2_mcp_core::TrackWriteTarget {
        section_count: 0,
        movements: &[],
    }
}

/// 選択肢の値を書き込める設定項目種別を、書き込みの検証そのものから集める。
///
/// 判定を書き写さず、公開されている入口へ選択肢の値を渡して受理されるかで
/// 決める。書き込みを公開する種別と、種別が受け付ける値の形の、どちらが
/// 動いてもここが動く。
fn item_types_accepting_a_choice() -> Vec<String> {
    let mut names = Vec::new();
    for item_type in EffectItemType::ALL {
        let items = vec![AvailableEffectItem {
            name: "項目".to_string(),
            item_type: item_type.clone(),
        }];
        let value = ItemValue::Choice {
            value: "四角形".to_string(),
        };
        if prepare_item_write(&items, "項目", &value, no_track_target()).is_ok() {
            names.push(item_type.kind_name());
        }
    }
    names
}

#[test]
fn the_description_names_every_item_type_that_takes_a_choice() {
    // 説明は保証である。挙げた種別だけが選択肢として書けると読まれるため、
    // 一覧が実態と食い違えば、書ける種別が使われないか、書けない種別が
    // 当て推量で試される。並びごと突き合わせるため、落としても足しても
    // 順序を変えても落ちる。
    let named = choice_item_types_named_in(&description_of("set_object_item"));
    assert!(!named.is_empty(), "説明が種別を 1 つも挙げていません");
    assert_eq!(
        named,
        item_types_accepting_a_choice(),
        "説明が挙げる種別と、選択肢の値を受け付ける種別が食い違います"
    );
}

/// 書き込みを公開する種別のうち、書き込み後に照合しないものを集める。
///
/// 判定を書き写さず、公開されている入口の答えから決める。公開の可否と照合の
/// しかたの、どちらが動いてもここが動く。
fn writable_item_types_without_read_back() -> Vec<String> {
    let mut names = Vec::new();
    for item_type in EffectItemType::ALL {
        let items = vec![AvailableEffectItem {
            name: "項目".to_string(),
            item_type: item_type.clone(),
        }];
        let probe = ItemValue::Text {
            value: "文字列".to_string(),
        };
        // 種別への書き込みを公開しているかは、値の形の照合より先に決まる。
        // 形が合わない値を渡しても判定は変わらない。
        let writable = !matches!(
            prepare_item_write(&items, "項目", &probe, no_track_target()),
            Err(ItemWriteError::UnsupportedItemType { .. })
        );
        if writable
            && matches!(
                read_back_check(item_type, &probe),
                ReadBackCheck::Declared { .. }
            )
        {
            names.push(item_type.kind_name());
        }
    }
    names
}

#[test]
fn the_description_states_that_every_write_is_verified_by_reading_back() {
    // 説明は保証である。照合しない種別が生まれたのに説明が「全ての種別」を
    // 名乗り続けると、要求元は掛かっていない検査を前提に書き込みを組む。
    // 実装だけを直した場合も、説明だけを直した場合も落ちる。
    let unverified = writable_item_types_without_read_back();
    assert!(
        unverified.is_empty(),
        "照合しない種別 {unverified:?} を説明が挙げていません"
    );
    assert!(
        description_of("set_object_item")
            .contains("書き込みは全ての種別で、対象を読み直してから設定値を読んで照合する"),
        "説明が全種別の照合を述べていません"
    );
}

/// 設定値の種別ごとの分岐に付いた説明を取り出す。
fn item_value_description(kind: &str) -> String {
    let tool = tool_named("set_object_item");
    let variants = tool.input_schema["$defs"]["ItemValueInput"]["oneOf"]
        .as_array()
        .expect("設定値が判別子つきの union として宣言されていません")
        .clone();
    variants
        .iter()
        .find(|variant| variant["properties"]["type"]["const"] == kind)
        .unwrap_or_else(|| panic!("{kind} 種別の分岐がありません"))["description"]
        .as_str()
        .unwrap_or_else(|| panic!("{kind} 種別に説明がありません"))
        .to_string()
}

/// 与えた値を書き込める設定項目種別を、書き込みの検証そのものから集める。
///
/// 判定を書き写さず、公開されている入口へ値を渡して受理されるかで決める。
/// 書き込みを公開する種別と、種別が受け付ける値の形の、どちらが動いても
/// ここが動く。
fn item_types_accepting(value: &ItemValue) -> Vec<String> {
    let mut names = Vec::new();
    for item_type in EffectItemType::ALL {
        let items = vec![AvailableEffectItem {
            name: "項目".to_string(),
            item_type: item_type.clone(),
        }];
        if prepare_item_write(&items, "項目", value, no_track_target()).is_ok() {
            names.push(item_type.kind_name());
        }
    }
    names
}

#[test]
fn the_numeric_item_descriptions_name_every_item_type_that_takes_them() {
    // 挙げた種別だけが書けると読まれるため、一覧が実態と食い違えば、書ける
    // 種別が使われないか、書けない種別が当て推量で試される。しかも失敗するのは
    // invalid_argument であり、値を選び直しても直らない。**2 つは対称であり、
    // 片方が受ける種別はもう片方が拒む種別そのものである**——片方だけを直すと
    // 残りが黙って古くなる。
    let integer = ItemValue::Integer { value: 1 };
    let number = ItemValue::Number {
        value: aviutl2_mcp_core::FiniteF64::try_new(1.0).expect("有限である"),
    };
    for (kind, accepted, refused) in [
        ("integer", &integer, &number),
        ("number", &number, &integer),
    ] {
        let description = item_value_description(kind);
        assert_eq!(
            item_types_named_before(&description, " の項目に書ける"),
            item_types_accepting(accepted),
            "{kind} が書ける種別と説明が食い違います: {description}"
        );
        assert_eq!(
            item_types_named_before(&description, " の項目へ書くと invalid_argument"),
            item_types_accepting(refused),
            "{kind} が落ちる種別と説明が食い違います: {description}"
        );
    }
}

/// 書き込みを公開していない既知の設定項目種別を、書き込みの検証から集める。
fn item_types_refused_for_write() -> Vec<String> {
    let mut names = Vec::new();
    for item_type in EffectItemType::ALL {
        let items = vec![AvailableEffectItem {
            name: "項目".to_string(),
            item_type: item_type.clone(),
        }];
        let probe = ItemValue::Text {
            value: "文字列".to_string(),
        };
        if matches!(
            prepare_item_write(&items, "項目", &probe, no_track_target()),
            Err(ItemWriteError::UnsupportedItemType { .. })
        ) {
            names.push(item_type.kind_name());
        }
    }
    names
}

#[test]
fn the_description_names_every_item_type_that_cannot_be_written() {
    // 説明は保証である。書けない種別を広く名乗れば書ける項目が使われず、
    // 狭く名乗れば要求元は通らない要求を組み立てる。並びごと突き合わせる。
    assert_eq!(
        item_types_named_before(&description_of("set_object_item"), " のものと、"),
        item_types_refused_for_write(),
        "説明が挙げる種別と、書き込みを公開しない種別が食い違います"
    );
}

#[test]
fn the_color_item_description_states_which_notation_the_host_accepts() {
    // 説明は保証である。受理される書式を挙動から導く材料が要求元の側に
    // 無い——外れた書式は失敗するが、何が正解かは失敗からは分からない。
    // **実測で確定した 2 点だけを書く。** 8 桁のアルファ付きなどは観測して
    // いないため、通るとも通らないとも述べない。
    assert_eq!(
        item_value_description("color"),
        "色。16 進 6 桁（例 `ff8800`）で指定する。読み直すと小文字で返る。\n\
         `#` を付けた表記と 3 桁の省略形は受け付けられず、指定した色にならない\n\
         だけでなく元の色も失われて白（`ffffff`）になる。\n\
         受け付けられなかったことは書き込みの応答が\n\
         unsupported_operation で伝える。"
    );
}

#[test]
fn the_font_item_description_points_at_the_registered_names() {
    // 登録済みの名前を得る手段を示さなければ、要求元は当て推量を繰り返す。
    // 外れた名前は失敗し、設定項目は変更前のまま残る。
    assert_eq!(
        item_value_description("font"),
        "フォント名。list_fonts が返す登録済みの名前をそのまま指定する。\n\
         登録されていない名前は書き込みが unsupported_operation となり、\n\
         設定項目は変更前の値のまま残る。"
    );
}

#[test]
fn the_text_item_description_states_what_survives_the_round_trip() {
    // 説明は保証である。書いた値がそのまま返ること、CRLF が LF になること、
    // 単独の CR を受け付けないことは、挙動から導く材料が要求元の側に無い。
    // 文言そのものを固定する。
    assert_eq!(
        item_value_description("text"),
        "テキスト。改行とタブを含めて書き込め、読み直すと書いたとおりに返る。\n\
         バックスラッシュも書いたとおりに保たれるため、Windows パス・正規表現・\n\
         LaTeX をそのまま指定できる。CRLF は LF として保存される。単独の CR は\n\
         受け付けない——保存はされるが描画では行が分かれず、意図を推測できない。\n\
         長さの上限は保存される表記に掛かり、`\\` と改行はそれぞれ 2 バイトを\n\
         占める。"
    );
}

#[test]
fn the_move_effect_description_never_voids_the_selector_unconditionally() {
    // 列の位置が変わらなかった移動は、成功しても selector を無効にしない。
    // **握るのは言い回しではなく、断定の手前に条件が在ることである。**
    // 後続の句が条件を述べていても、断定が先に立てば要約として読まれ、
    // 要求元は動かなかった列に対しても対象を読み直す。
    let description = description_of("move_effect");
    let (before, _) = description
        .split_once("selector は使えなくなる")
        .expect("move_effect の説明が selector の無効化を述べていません");
    let clause = before.rsplit('。').next().expect("句を切り出せません");
    assert!(
        ["れば", "場合", "とき", "なら"]
            .iter()
            .any(|mark| clause.contains(mark)),
        "move_effect の説明が selector の無効化を無条件で述べています: {clause}"
    );
}

/// tool の説明がレイヤーのロックについて述べる内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayerLockStatement {
    /// ロックで拒否されることと、解除の手段を述べる。
    StoppedAndNamesTheWayOut,
    /// ロックが止める範囲と、自身が影響を受けないことを述べる。
    DescribesTheScope,
    /// ロックについて何も述べない。
    Silent,
}

/// tool 名から、説明がレイヤーのロックについて述べる内容を引く。
///
/// 未知の tool 名で落とす。ロックが止める範囲を決めるのはホストであり、
/// 対象を 1 か所へ置いて tool を足したときに素通りしないようにする。
fn layer_lock_statement(name: &str) -> LayerLockStatement {
    match name {
        "create_object"
        | "move_object"
        | "delete_object"
        | "create_object_section"
        | "delete_object_section"
        | "move_object_section"
        // 一括適用が止まるのは move_object を含む場合だけだが、止まり方も
        // 解き方も同じであるため、案内する側に属する。
        | "apply_batch" => LayerLockStatement::StoppedAndNamesTheWayOut,
        "set_layer_state" => LayerLockStatement::DescribesTheScope,
        "set_object_name"
        | "set_object_item"
        | "add_effect"
        | "set_effect_enabled"
        | "move_effect"
        | "delete_effect"
        | "set_selection"
        // BPM グリッドとシーン設定はシーンに属し、どのレイヤーの対象にも
        // 触れない。
        | "set_grid_bpm"
        | "set_scene_settings" => LayerLockStatement::Silent,
        other => panic!("{other} のレイヤーロックの説明が定義されていません"),
    }
}

#[test]
fn tools_stopped_by_a_layer_lock_name_the_way_out() {
    // layer_locked の retry_requires は none である。案内が無ければ、契約に
    // 従う要求元は解ける状況で停止する。
    for name in edit_like_tools() {
        let description = description_of(name);
        match layer_lock_statement(name) {
            LayerLockStatement::StoppedAndNamesTheWayOut => {
                assert!(
                    description.contains("layer_locked"),
                    "{name} の説明がロックによる拒否を述べていません"
                );
                assert!(
                    description.contains("set_layer_state"),
                    "{name} の説明がロックの解除手段を示していません"
                );
            }
            LayerLockStatement::DescribesTheScope => {
                assert!(
                    description.contains("この tool 自身はロックの影響を受けない"),
                    "{name} の説明が自身にロックが掛からないことを述べていません"
                );
            }
            LayerLockStatement::Silent => assert!(
                !description.contains("layer_locked"),
                "{name} の説明が掛からないロックによる拒否を述べています"
            ),
        }
    }
}

/// tool 名から、失敗応答が対象の現在の姿を返し得るかを引く。
///
/// 未知の tool 名で落とす。返し得るのは対象を指す selector を解決する tool
/// だけであり、作成は対象がまだ無く、レイヤーの状態変更は対象が selector も
/// fingerprint も持たない。**一覧を const で持つと、どちらにも書かれていない
/// 新しい tool が「触れない」側の既定へ黙って落ちる。**
fn returns_a_current_object(name: &str) -> bool {
    match name {
        "move_object"
        | "set_object_name"
        | "set_object_item"
        | "add_effect"
        | "set_effect_enabled"
        | "move_effect"
        | "delete_effect"
        | "delete_object"
        | "set_selection"
        | "create_object_section"
        | "delete_object_section"
        | "move_object_section" => true,
        // 一括適用は 100 件のうちどれが落ちたかを併せて示す必要があるため、
        // 別のキー（failed_object）で返す。
        "create_object" | "set_layer_state" | "set_grid_bpm" | "set_scene_settings"
        | "apply_batch" => false,
        other => panic!("{other} が現在の姿を返すかが定義されていません"),
    }
}

#[test]
fn no_tool_description_repeats_how_to_read_the_current_object() {
    // `details` の値は失敗の text content へキーごと出るようになった。
    // **キーが在ることを説明する必要はもう無く、要るのは値の使い方である。**
    // それは selector の使い方そのものであるため共有型の説明が持ち、
    // 11 tool の説明から落ちる。
    //
    // 一覧そのものは [`returns_a_current_object`] が保ち続ける。落とすのは
    // 説明であって事実ではない。
    for name in edit_like_tools() {
        assert!(
            !description_of(name).contains("details.current_object"),
            "{name} の説明が共有型の写しを持っています"
        );
    }
    assert!(
        shared_type_description("get_object", "ObjectSelectorInput").contains("対象の現在の姿"),
        "selector の説明が現在の姿の使い方を述べていません"
    );
    // **キー名を述べる tool は apply_batch だけである。** そちらは 100 件の
    // うちどれが落ちたかを併せて示す必要があるため、別のキーで返す。
    assert!(
        description_of(APPLY_BATCH).contains("details.failed_object"),
        "一括適用の説明が自分のキーを述べていません"
    );
    // 表が古びないよう、編集 tool の集合と突き合わせる。どちらの側に属するかは
    // tool ごとに決まる事実であり、説明を落としても失われない。
    let returning: Vec<&str> = edit_like_tools()
        .into_iter()
        .filter(|name| returns_a_current_object(name))
        .collect();
    assert_eq!(
        returning.len(),
        12,
        "現在の姿を返す tool の数が変わりました: {returning:?}"
    );
}

#[test]
fn input_schemas_declare_the_expected_epoch_only_where_it_is_used() {
    // 前提の epoch を持つのは、要求が対象を指す selector を 1 つも運ばない
    // ことがある tool だけである。必ず運ぶ tool へ宣言すると、同じ意味の値が
    // 1 要求の 2 か所へ並ぶ。
    for name in edit_like_tools() {
        let tool = tool_named(name);
        let properties = tool
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .unwrap_or_else(|| panic!("{name} に properties がありません"));

        let carries = TOOLS_CARRYING_AN_EXPECTED_EPOCH.contains(&name);
        assert_eq!(
            properties.contains_key("expected_project_epoch"),
            carries,
            "{name} の入力 schema と前提の epoch の要否が食い違います"
        );
        let required = tool
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|items| items.contains(&serde_json::json!("expected_project_epoch")))
            .unwrap_or(false);
        assert_eq!(required, carries, "{name} の必須指定が食い違います");
    }
}

#[test]
fn only_deleting_a_section_is_annotated_as_destructive() {
    // 中間点を消すとその位置の移動パラメータが失われ、同じ tool では戻せない。
    // 作成と移動は戻せるため、3 つを 1 つの tool へまとめず annotation を
    // 分けている。
    let destructive = |name: &str| {
        tool_named(name)
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.destructive_hint)
    };
    assert_eq!(destructive("delete_object_section"), Some(true));
    assert_eq!(destructive("create_object_section"), Some(false));
    assert_eq!(destructive("move_object_section"), Some(false));
}

#[test]
fn the_batch_input_schema_does_not_accept_a_section_change() {
    // 削除した中間点の移動パラメータを復元する手段が無く、
    // delete_object_section の逆操作を構築できない。3 つのうち一部だけを
    // 入れる形も採らない。
    let schema = tool_named(APPLY_BATCH).input_schema.clone();
    let declared = Value::Object(schema.as_ref().clone()).to_string();
    // BPM グリッドの置き換えは戻り値を持たず、成否を戻り値から知れない。
    // read-back で確認できるが、選定の基準は「戻り値で成否が分かる」ことで
    // あり例外を作らない。
    for name in [
        "create_object_section",
        "delete_object_section",
        "move_object_section",
        "set_grid_bpm",
    ] {
        assert!(
            !declared.contains(name),
            "{name} が一括適用の入力 schema に現れています"
        );
    }
    // 受け付ける 2 種は現れる。走査そのものが働いていることを併せて固定する。
    for name in ["move_object", "set_object_item"] {
        assert!(declared.contains(name), "{name} が入力 schema にありません");
    }
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

#[test]
fn the_grid_bpm_input_schema_declares_the_limits_it_enforces() {
    // 宣言した制約は server 側で実際に検証する。検証していない宣言を
    // schema に残さない。検証の実体は core の validate である。
    let tool = tool_named("set_grid_bpm");
    let entries = tool.input_schema["properties"]["entries"].clone();
    assert_eq!(
        entries["maxItems"],
        serde_json::json!(aviutl2_mcp_core::MAX_GRID_BPM_ENTRIES)
    );
    // 0 件はグリッドを消す指定である。下限を宣言すると手段が無くなる。
    assert!(entries.get("minItems").is_none());
    let beat = tool.input_schema["$defs"]["GridBpmInput"]["properties"]["beat"].clone();
    assert_eq!(beat["minimum"], serde_json::json!(1));
    assert_eq!(beat["maximum"], serde_json::json!(i32::MAX));
}

/// 入力 schema の必須フィールドを宣言順に取り出す。
fn required_fields(tool: &Tool) -> Vec<&str> {
    tool.input_schema["required"]
        .as_array()
        .expect("必須フィールドが宣言されていません")
        .iter()
        .map(|field| field.as_str().expect("フィールド名"))
        .collect()
}

#[test]
fn the_scene_settings_input_declares_three_optional_axes_and_two_preconditions() {
    // 出力側と違い、入力 schema を丸ごと固定する検査は無い。新設の tool は
    // 軸の名前・入れ子の形・前提条件の必須指定をここで塞ぐ。軸を落とせば
    // 既存の要求が invalid_argument になり、前提条件が省略可能へ緩めば
    // プロジェクト境界を照合する材料が消える。
    let tool = tool_named("set_scene_settings");
    let properties = tool.input_schema["properties"]
        .as_object()
        .expect("properties がある");
    let required = required_fields(&tool);
    for axis in ["name", "size", "sample_rate"] {
        assert!(properties.contains_key(axis), "{axis} が宣言されていません");
        assert!(!required.contains(&axis), "{axis} が必須になっています");
    }
    assert_eq!(
        required,
        vec!["instance_id", "expected_scene_id", "expected_project_epoch"]
    );

    // 解像度は組でしか綴れない。片方だけの指定は必須欠落として落ちる。
    let size = tool.input_schema["$defs"]["SceneSizeInput"].clone();
    assert_eq!(size["type"], serde_json::json!("object"));
    assert_eq!(
        size["required"]
            .as_array()
            .expect("必須フィールドが宣言されていません")
            .iter()
            .map(|field| field.as_str().expect("フィールド名"))
            .collect::<Vec<_>>(),
        vec!["width", "height"]
    );

    // 組でしか綴れないことは、値を書く場所の隣が述べる。
    assert!(
        field_description("set_scene_settings", "size").contains("width と height は組で指定する"),
        "size が組であることを述べていません"
    );

    // 綴りの誤った軸が黙って無視されないこと。
    assert_eq!(
        tool.input_schema.get("additionalProperties"),
        Some(&serde_json::json!(false))
    );
}

#[test]
fn the_scene_settings_input_does_not_try_to_express_the_at_least_one_rule() {
    // 「3 つのいずれかを必ず指定する」は schema で表せない。組み合わせを
    // oneOf で並べると、要求元から見た schema が 7 通りに割れる。判定は
    // 実行時の検証が担うため、表そうとしていないことを固定する。
    let tool = tool_named("set_scene_settings");
    for keyword in [
        "oneOf",
        "anyOf",
        "allOf",
        "not",
        "minProperties",
        "dependentRequired",
    ] {
        assert!(
            tool.input_schema.get(keyword).is_none(),
            "入力 schema が {keyword} で軸の組み合わせを表そうとしています"
        );
    }
}

#[test]
fn the_scene_settings_tool_says_it_cannot_be_undone_before_it_is_called() {
    // 要求を出す前に読まれる口は 2 つある。人と要求を組み立てる LLM が読む
    // 説明の冒頭と、機械が読む destructiveHint である。要求のあとに読める
    // 3 つ目の口（応答の non_undoable）は統合テストが確かめる。
    let tool = tool_named("set_scene_settings");
    let description = tool.description.as_ref().expect("説明がある");
    assert!(
        description.starts_with("この操作は取り消せない"),
        "説明の冒頭に取り消せない旨がありません: {description}"
    );
    // destructiveHint の根拠は削除ではなく不可逆性である。削除系と同じ組を
    // 採るが、削除は取り消しで戻るのに対しこれは戻らない。
    assert_eq!(
        tool.annotations
            .as_ref()
            .expect("annotation がある")
            .destructive_hint,
        Some(true)
    );
}

#[test]
fn the_section_input_schemas_declare_the_lower_bound_they_enforce() {
    // 宣言した制約は server 側で実際に検証する。検証していない宣言を
    // schema に残さない。検証の実体は core の validate である。
    for name in ["delete_object_section", "move_object_section"] {
        let tool = tool_named(name);
        let section = tool.input_schema["properties"]["section"].clone();
        assert_eq!(
            section["minimum"],
            serde_json::json!(1),
            "{name} の section が下限を宣言していません"
        );
    }
    // 追加は区間番号を取らない。宣言する下限も無い。
    assert!(
        tool_named("create_object_section").input_schema["properties"]
            .get("section")
            .is_none()
    );
}

#[test]
fn section_tool_descriptions_explain_the_index_correspondence() {
    // 「区間の番号」と「中間点の番号」が 1 つずれることは、要求元が自力で
    // 気付ける情報ではない。
    //
    // **区間番号を引数に取る 2 tool では、値を書く場所の隣が述べる。**
    // 追加は区間番号を取らないため、応答の sections の形を述べる側として
    // 説明が持つ。
    for name in ["delete_object_section", "move_object_section"] {
        let field = field_description(name, "section");
        for keyword in [
            "sections[i] が区間番号 i",
            "sections[0].start はオブジェクトの開始フレームであって中間点ではない",
        ] {
            assert!(
                field.contains(keyword),
                "{name} の section に {keyword} がありません: {field}"
            );
        }
        assert!(
            !description_of(name).contains("sections[i] が区間番号 i"),
            "{name} の説明が入力 schema の写しを持っています"
        );
    }
    for keyword in [
        "sections[i] が区間番号 i",
        "sections[0].start はオブジェクトの開始フレームであって中間点ではない",
    ] {
        assert!(
            description_of("create_object_section").contains(keyword),
            "create_object_section の説明に {keyword} がありません"
        );
    }
    // 応答の sections の形は 3 つとも同じであり、いずれも説明が述べる。
    for name in [
        "create_object_section",
        "delete_object_section",
        "move_object_section",
    ] {
        assert!(
            description_of(name).contains("sections の末尾の end はオブジェクトの終了フレーム"),
            "{name} の説明が応答の sections の形を述べていません"
        );
    }
    // フレームの意味も要求元が自力では決められない。
    for name in ["create_object_section", "move_object_section"] {
        assert!(
            description_of(name).contains("シーンの絶対フレーム番号"),
            "{name} の説明が frame の意味を述べていません"
        );
    }
}

#[test]
fn the_batch_input_schema_declares_the_operation_count_it_actually_enforces() {
    // 宣言した制約は server 側で実際に検証する。宣言だけがあって検証されない
    // 制約を schema に残さない。件数は core の検証が判定する。
    let tool = tool_named(APPLY_BATCH);
    let operations = tool.input_schema["properties"]["operations"].clone();
    assert_eq!(operations["minItems"], serde_json::json!(1));
    assert_eq!(
        operations["maxItems"],
        serde_json::json!(aviutl2_mcp_core::MAX_BATCH_OPERATIONS)
    );
}

/// 入力 schema に現れる property を、名前と説明の対で集める。
///
/// `$defs` の中も辿る。共有の入力型はそこへ展開されるため、辿らなければ
/// selector も設定値も 1 つも見えない。
fn property_descriptions(schema: &Value) -> Vec<(String, String)> {
    let mut found = Vec::new();
    collect_property_descriptions(schema, &mut found);
    found
}

fn collect_property_descriptions(value: &Value, found: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(properties)) = map.get("properties") {
                for (name, property) in properties {
                    if let Some(description) = property.get("description").and_then(Value::as_str) {
                        found.push((name.clone(), description.to_string()));
                    }
                }
            }
            for child in map.values() {
                collect_property_descriptions(child, found);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_property_descriptions(item, found);
            }
        }
        _ => {}
    }
}

/// 共有の入力型に付いた説明を取り出す。
fn shared_type_description(tool: &str, definition: &str) -> String {
    tool_named(tool).input_schema["$defs"][definition]["description"]
        .as_str()
        .unwrap_or_else(|| panic!("{definition} に説明がありません"))
        .to_string()
}

/// tool の入力 schema の直下にある property の説明を取り出す。
fn field_description(tool: &str, field: &str) -> String {
    tool_named(tool).input_schema["properties"][field]["description"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool} の {field} に説明がありません"))
        .to_string()
}

#[test]
fn the_fields_that_take_a_number_state_where_it_starts() {
    // 起点を取り違えると別の場所へ書く。**値を書く場所そのものが述べる。**
    // tool の説明へ写す形は、番号を扱う tool の数だけ同じ文を増やす一方、
    // 引数を埋める時点では読まれない。
    let mut checked = 0;
    for tool in tools() {
        let schema = Value::Object(tool.input_schema.as_ref().clone());
        for (field, description) in property_descriptions(&schema) {
            if !description.contains("レイヤー番号") && !description.contains("フレーム番号")
            {
                continue;
            }
            // 番号ではないことを述べる説明は対象外である。BPM グリッドの
            // 位置は秒であり、起点を持たない。
            if description.contains("フレーム番号ではない") {
                continue;
            }
            checked += 1;
            assert!(
                description.contains("0 始まり"),
                "{} の {field} が番号の起点を述べていません: {description}",
                tool.name
            );
        }
    }
    assert!(
        checked >= 20,
        "番号を取るフィールドを検査できていません: {checked} 件"
    );
}

#[test]
fn the_selector_types_state_that_they_travel_back_unchanged() {
    // selector を組み立て直さずに往復させることは、selector を受け取る全 tool に
    // 掛かる規約である。共有型に 1 度書けば schemars が各 tool の schema へ配る。
    let object = shared_type_description("get_object", "ObjectSelectorInput");
    for phrase in [
        "そのまま送り返す",
        "読み直さずにそのまま次の要求へ渡せる",
        "fingerprint が変わる",
        "precondition_failed",
        "対象の現在の姿",
    ] {
        assert!(
            object.contains(phrase),
            "オブジェクトの selector の説明が {phrase} に触れていません: {object}"
        );
    }
    // **キー名は名乗らない。** 同じ型が apply_batch の schema にも入るが、
    // そちらは同じものを details.failed_object という別の名前で返す。
    // 共有型が片方の名前を名乗ると、もう片方の tool に対して嘘になる。
    assert!(
        !object.contains("details."),
        "共有型が失敗応答のキーを名乗っています: {object}"
    );

    let effect = shared_type_description("set_object_item", "EffectSelectorInput");
    for phrase in [
        "そのまま送り返す",
        "読み直さずにそのまま次の要求へ渡せる",
        "fingerprint が変わる",
    ] {
        assert!(
            effect.contains(phrase),
            "effect の selector の説明が {phrase} に触れていません: {effect}"
        );
    }
}

#[test]
fn the_page_fields_explain_how_to_page() {
    // ページ指定は共有の入力型が配る。tool の説明へ写すと、同じ 3 行が
    // ページを取る tool の数だけ並ぶ。
    let mut checked = 0;
    for tool in tools() {
        let properties = tool.input_schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{} に properties がありません", tool.name));
        if !properties.contains_key("limit") {
            continue;
        }
        checked += 1;
        let offset = field_description(&tool.name, "offset");
        for phrase in ["0 始まりの位置", "has_more と next_offset で終端"] {
            assert!(
                offset.contains(phrase),
                "{} の offset が {phrase} に触れていません: {offset}",
                tool.name
            );
        }
        let limit = field_description(&tool.name, "limit");
        for phrase in ["1 以上 200 以下", "省略すると 50"] {
            assert!(
                limit.contains(phrase),
                "{} の limit が {phrase} に触れていません: {limit}",
                tool.name
            );
        }
    }
    assert!(checked >= 4, "ページ指定を持つ tool を検査していません");
}

#[test]
fn the_expected_epoch_fields_say_why_they_cannot_be_omitted() {
    // 省略できない理由——対象を指す selector が無いこと——は、値を書く場所の
    // 隣に在る。
    for name in TOOLS_CARRYING_AN_EXPECTED_EPOCH {
        let description = field_description(name, "expected_project_epoch");
        for phrase in ["省略はできない", "プロジェクト境界を照合する"] {
            assert!(
                description.contains(phrase),
                "{name} の expected_project_epoch が {phrase} に触れていません: {description}"
            );
        }
    }
}

#[test]
fn input_schemas_reject_unknown_fields() {
    for tool in tools() {
        assert_eq!(
            tool.input_schema.get("additionalProperties"),
            Some(&serde_json::json!(false)),
            "{} の入力 schema が未知フィールドを許しています",
            tool.name
        );
    }
}

#[test]
fn instance_id_is_required_except_for_list_instances() {
    for tool in tools() {
        let required = tool
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|items| items.contains(&serde_json::json!("instance_id")))
            .unwrap_or(false);
        if tool.name == "list_instances" {
            assert!(!required, "一覧取得は instance_id を要求しない");
        } else {
            assert!(required, "{} は instance_id を必須にする", tool.name);
        }
    }
}
