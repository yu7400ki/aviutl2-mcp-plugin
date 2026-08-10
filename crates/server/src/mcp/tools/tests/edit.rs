//! 編集 tool の説明と入出力 schema の検査。

use super::*;

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
