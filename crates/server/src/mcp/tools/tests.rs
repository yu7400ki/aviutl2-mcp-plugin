//! tool の説明と入出力 schema の検査。

use crate::mcp::server::tests::{server, tool_named, tools};
use aviutl2_mcp_core::{
    AvailableEffectItem, EffectItemType, ItemValue, ItemWriteError, MAX_DESCRIBED_EFFECTS,
    ReadBackCheck, prepare_item_write, read_back_check,
};
use rmcp::ServerHandler;
use rmcp::model::Tool;
use serde_json::Value;

mod edit;
mod read;

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

/// 説明の一部を取り出す。
fn description_of(name: &str) -> String {
    tool_named(name)
        .description
        .as_ref()
        .unwrap_or_else(|| panic!("{name} に説明がありません"))
        .to_string()
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
    ///
    /// **1 続きの文ではなく、言い換えても残る単位で並べる。** 文を丸ごと置けば
    /// 固定されるのは語順であり、内容ではない——限定を前置しただけの本文は
    /// 部分文字列として素通りし、同じことを別の語順で述べた本文は落ちる。
    to_skill: Option<&'static [&'static str]>,
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
        to_skill: Some(&[
            "AviUtl2 の UI はレイヤーとフレームを 1 始まりで表示する",
            "tool が受け渡すのは 0 始まりの番号であり、画面で見た番号より 1 小さい",
        ]),
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
        to_skill: Some(&[
            "selector は自分で組み立てない",
            "読み取りの応答が返した値をそのまま編集へ渡し、\
             編集の応答が返した値をそのまま次の編集へ渡す",
        ]),
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
        to_skill: Some(&[
            "プロジェクト境界の照合材料は selector が運ぶ project_epoch である",
            "要求が selector を 1 つも運ばないことがある tool（create_object・\
             set_layer_state・set_selection・set_grid_bpm・set_scene_settings）だけが\
             expected_project_epoch を要求し、そちらは省略できない",
        ]),
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
        to_skill: Some(&[
            "対象が変化していた precondition_failed では details.current_object に\
             対象の現在の姿が入り、そのまま次の要求の selector にできる",
            "apply_batch だけは何番目で落ちたかを併せて示すため details.failed_object という\
             別のキーで返す",
        ]),
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
        // **層 3 が述べるのは層 1 より狭い。** 運ばないのは編集の要求であり、
        // 一覧の続きを引く要求は先頭ページが返した値を送り返す。層 1 の言い切りを
        // そのまま置くと、本文が足した限定を検査が見ないまま通る。
        to_skill: Some(&[
            "編集の要求は project_revision を運ばない",
            "revision が進んでいても拒否されない",
            "revision を取り直す必要は無い",
            "一覧の続きを引く要求だけは運ぶ",
            "間に編集が挟まると拒否される",
        ]),
    },
    Relocation {
        statement: "変更が起きた編集 tool の呼び出し 1 回が、1 つの取り消し単位になる。\
                    まとめて 1 単位にしたいときは apply_batch を選ぶ",
        was_stated_by: 12,
        applies_to: 16,
        dropped: &["この呼び出し 1 回が 1 つの取り消し単位になる"],
        to_input_schema: None,
        to_skill: Some(&[
            "変更が起きた編集 tool の呼び出し 1 回が、1 つの取り消し単位になる",
            "まとめて 1 単位にしたいときは apply_batch を選ぶ",
        ]),
    },
    Relocation {
        statement: "timeout は変更が無かったことを意味しない。\
                    details.change_applied が \"no\" なら未適用のため再送してよく、\
                    \"unknown\" なら読み直して確認してから再送する",
        was_stated_by: 16,
        applies_to: 16,
        dropped: &["details.change_applied"],
        to_input_schema: None,
        to_skill: Some(&[
            "timeout は変更が無かったことを意味しない",
            "details.change_applied が \"no\" なら未適用のため再送してよく、\
             \"unknown\" なら読み直して確認してから再送する",
        ]),
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
        to_skill: Some(&[
            "書き込みを発行した後に落ちた失敗には details.mutation_issued が true で付く",
            "付かない失敗は 1 バイトも書いていないため、対象を読み直さずに要求を直して\
             送り直せる",
            "付く失敗が読み直しを要するかは details.retry_requires が名乗る\
             ——発行した変更が戻っていれば読み直す先は無い",
        ]),
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
        to_skill: Some(&[
            "設定項目に何を書けるか分からないときは describe_effects を呼ぶ",
            "choices が候補を、range が値域と小数桁を返す",
            "どちらも null の項目は表に載っていないだけであり、\
             既存オブジェクトの値を get_object で読んで倣う",
            "フォント名は list_fonts が返す",
        ]),
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
        becomes: "SKILL.md の本文が、運ばないのは編集の要求であることを 1 度述べること。\
                  一覧の続きを引く要求だけは先頭ページが返した値を送り返すため、\
                  層 1 の言い切りをそのまま写すと述べすぎになる",
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
        if let Some(phrases) = relocation.to_skill {
            // 本文との突き合わせは
            // [`the_conventions_handed_to_the_skill_are_in_its_body`] が行う。
            // ここで見るのは、行き先の宣言が空でないことだけである。
            assert!(!phrases.is_empty(), "skill が受け取る内容が空です");
            for phrase in phrases {
                assert!(!phrase.is_empty(), "skill が受け取る単位が空です");
            }
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
        let Some(phrases) = relocation.to_skill else {
            continue;
        };
        handed += 1;
        for phrase in phrases {
            assert!(
                occurrences(&body, phrase) >= 1,
                "層 1 の {} tool が述べていた事実が skill の本文にありません: {phrase}",
                relocation.was_stated_by
            );
        }
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

    // 1. 編集の要求が project_revision を運ばないこと。**1 度だけ述べる**——
    //    層 1 で 16 tool へ並んでいた句を、層 3 でも並べては移した意味が無い。
    //    **主語まで含めて固定するのは [`RELOCATED_CONVENTIONS`] の側である**
    //    ——ここが見るのは回数だけであり、無限定の言い切りも同じ回数で通る。
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
