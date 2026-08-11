//! 設定項目の値変更の統合テスト。

use super::*;

#[test]
fn an_item_value_the_host_clamps_is_reported_as_a_failure() {
    // クライアントは要求した値を得ていない。成功として返すと、逸脱に気付く
    // 手段が要求元にも利用者にも無い。**読み直した実値を添えることで、要求元は
    // 要求した値がホストの手でどうなったかを知る**——切り詰めであれば、その値が
    // 値域の境界そのものである。
    let harness = Harness::new();
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            item: "範囲".to_string(),
            value: ItemValue::Integer {
                value: MAX_ITEM_VALUE + 150,
            },
        })
        .expect_err("切り詰められた値が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("item_value_not_applied"));
    assert_eq!(
        error.details()["observed_value"],
        json!(MAX_ITEM_VALUE.to_string())
    );
    // 巻き戻したため、読み直した値は応答が返る時点の現在値ではない。
    assert_eq!(error.details()["restored"], json!(true));
}

#[test]
fn an_item_value_within_the_host_limits_is_reported_as_read_back() {
    // 応答が返すのはホストが保持している値である。要求値をそのまま返すと、
    // 照合を通った後でも応答が実態を表さなくなる。**標本は要求値と実値が
    // 異なるものでなければならない。** 同じ値だと、要求を反響させるだけの実装
    // でも通る。
    let requested = ItemValue::Color {
        value: "FFAA00".to_string(),
    };
    let stored = ItemValue::Color {
        value: "ffaa00".to_string(),
    };
    assert_ne!(
        requested, stored,
        "標本の要求値と実値が同じで、反響しているだけの実装と区別できません"
    );

    let harness = harness_with_choice_effect();
    let outcome = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 300, SHAPE, 0),
            item: "色".to_string(),
            value: requested,
        })
        .expect("ホストが受理する値が失敗として扱われました");

    assert_eq!(changed_item(&outcome, "色"), stored);
}

#[test]
fn an_unknown_item_name_is_not_found() {
    let harness = Harness::new();
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            item: "存在しない項目".to_string(),
            value: ItemValue::Integer { value: 1 },
        })
        .expect_err("存在しない設定項目へ書き込めました");

    assert_eq!(error.error_code(), ErrorCode::NotFound);
    assert_eq!(error.details()["item"], json!("存在しない項目"));
    assert_eq!(
        harness
            .host
            .calls()
            .iter()
            .filter(|call| **call == ITEM_VALUE)
            .count(),
        1,
        "項目の存在を確かめる読み取りが行われていません"
    );
    harness.assert_untouched();
}

#[test]
fn an_item_missing_from_the_listing_but_readable_is_not_writable() {
    // 列挙は未知種別の項目を落とす。落ちた項目への書き込みを「項目が見つから
    // ない」として返すと、要求元は存在しない問題を指す失敗を受け取る。
    let harness = harness_with_unlisted_item();
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            item: "未知種別の項目".to_string(),
            value: ItemValue::Integer { value: 1 },
        })
        .expect_err("未知種別の項目へ書き込めました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("item_type_not_writable"));
    harness.assert_untouched();
}

#[test]
fn a_successful_write_reads_the_value_back_exactly_once() {
    // ホストは書き込みの成否を返さない。成功経路でも読み直さなければ、要求した
    // 値が入ったことを誰も確かめていない。**費用は 1 回に留める。**
    let harness = harness_with_unlisted_item();
    let selector = harness.effect_selector(1, 100, "ぼかし", 0);
    harness.host.clear_calls();

    harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector,
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 30 },
        })
        .expect("設定項目の変更に失敗しました");

    let calls = harness.host.calls();
    let first = first_mutation(&calls).expect("変更 API が呼ばれていません");
    assert_eq!(
        count(&calls[first..], ITEM_VALUE),
        1,
        "照合の読み直しは 1 回だけです: {calls:?}"
    );
}

#[test]
fn a_successful_write_reads_the_object_detail_once_after_the_write() {
    // 照合が読み直した対象をそのまま応答へ回す。応答の組み立てが自分で読み直せば
    // 同じ状態を 2 度読むことになり、効果を多く持つ対象では詳細の 1 回が SDK
    // 呼び出しの数十回になる。
    let harness = harness_with_unlisted_item();
    let selector = harness.effect_selector(1, 100, "ぼかし", 0);
    harness.host.clear_calls();

    harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector,
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 30 },
        })
        .expect("設定項目の変更に失敗しました");

    let calls = harness.host.calls();
    let first = first_mutation(&calls).expect("変更 API が呼ばれていません");
    assert_eq!(
        count(&calls[first..], "object_detail"),
        1,
        "変更の後に同じ対象を 2 度読みました: {calls:?}"
    );
}

#[test]
fn a_verified_write_reads_the_value_once_before_the_write() {
    // **書き込みの前に読むのは巻き戻しの材料である。** 照合が落ちたときに
    // 書き戻す生文字列は、発行してしまえば失われる。したがって組み合わせに
    // よらず 1 回読む——移動の事前確認が要らない要求（移動を書く要求、移動を
    // 持ち得ない種別）でも読む。移動の有無の判定は同じ文字列を使うため、
    // 読み取りが 2 回になることはない。
    let cases: [(&str, ItemValue, usize); 3] = [
        (
            STATIC_ITEM,
            ItemValue::Number {
                value: FiniteF64::try_new(30.0).expect("有限値"),
            },
            1,
        ),
        (STATIC_ITEM, movement(&[0.0, 50.0, 100.0], "直線移動"), 1),
        (
            // 移動を持ち得ない種別の代表。選択肢の effect が持つ。
            "メモ",
            ItemValue::Text {
                value: "覚書".to_string(),
            },
            1,
        ),
    ];
    for (item, value, expected) in cases {
        let text_item = item == "メモ";
        let harness = match text_item {
            true => harness_with_choice_effect(),
            false => harness_with_track_effect(),
        };
        let selector = match text_item {
            true => harness.effect_selector(1, 300, SHAPE, 0),
            false => harness.effect_selector(1, 100, COORDINATE, 0),
        };
        harness.host.clear_calls();
        harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector,
                item: item.to_string(),
                value: value.clone(),
            })
            .unwrap_or_else(|error| panic!("{item} の書き込みに失敗しました: {error}"));

        let calls = harness.host.calls();
        let first = first_mutation(&calls).expect("変更 API が呼ばれていません");
        assert_eq!(
            count(&calls[..first], ITEM_VALUE),
            expected,
            "{item} へ {} を書く前の読み取り回数が想定と異なります: {calls:?}",
            value.kind()
        );
    }
}

/// 選択肢から選ぶ設定項目の名前。種別はそれぞれ別である。
const CHOICE_ITEMS: [&str; 3] = ["図形の種類", "マスクの種類", "形状"];

/// 選択肢を持つ項目への書き込み要求を組み立てる。
fn set_choice_item(harness: &Harness, item: &str, value: &str) -> SetObjectItemParams {
    SetObjectItemParams {
        selector: harness.effect_selector(1, 300, SHAPE, 0),
        item: item.to_string(),
        value: ItemValue::Choice {
            value: value.to_string(),
        },
    }
}

/// 選択肢を持つ項目のうち 1 つへの書き込み要求を組み立てる。
fn set_choice(harness: &Harness, value: &str) -> SetObjectItemParams {
    set_choice_item(harness, CHOICE_ITEMS[0], value)
}

#[test]
fn the_choice_items_of_the_fake_have_distinct_item_types() {
    // 名前が 3 つあっても種別が 1 つなら、種別ごとに経路が分かれても選択肢の
    // 試験群は気付けない。
    let items = shape(0).items;
    let types: Vec<EffectItemType> = CHOICE_ITEMS
        .iter()
        .map(|name| {
            items
                .iter()
                .find(|item| item.name == *name)
                .unwrap_or_else(|| panic!("設定項目 {name} がありません"))
                .item_type
                .clone()
        })
        .collect();
    assert_eq!(
        types,
        vec![
            EffectItemType::Select,
            EffectItemType::Mask,
            EffectItemType::Figure,
        ]
    );
}

/// レイヤー範囲とシーン参照の設定項目を持つ effect を足したフェイクを組む。
///
/// カタログと対象オブジェクトの双方へ同じ effect を足す。種別はカタログの
/// 一覧から引かれるため、両方を揃えないと本番と同じ経路を通らない。
fn harness_with_reference_effect() -> Harness {
    Harness::with(|host| {
        host.catalog.push(group_control_catalog_entry());
        host.scene.get_mut().unwrap().layers[1].objects[1]
            .effects
            .push(group_control(0));
    })
}

/// レイヤー範囲・シーン参照の項目への書き込み要求を組み立てる。
fn set_reference_item(harness: &Harness, item: &str, value: ItemValue) -> SetObjectItemParams {
    SetObjectItemParams {
        selector: harness.effect_selector(1, 300, GROUP_CONTROL, 0),
        item: item.to_string(),
        value,
    }
}

#[test]
fn a_layer_range_and_a_scene_reference_take_an_integer() {
    // 作成の経路が書ける値を、編集の経路も書ける。書いた値は読み直しの照合を
    // 通り、応答が返す effect に整数として載る。
    for item in [LAYER_RANGE_ITEM, SCENE_ITEM] {
        let harness = harness_with_reference_effect();
        let outcome = harness
            .edit
            .set_object_item(&set_reference_item(
                &harness,
                item,
                ItemValue::Integer { value: 1 },
            ))
            .unwrap_or_else(|error| panic!("{item} への整数の書き込みが拒否されました: {error}"));
        assert_eq!(
            changed_item(&outcome, item),
            ItemValue::Integer { value: 1 },
            "{item}"
        );
    }
}

#[test]
fn a_layer_range_and_a_scene_reference_refuse_every_other_value_shape() {
    // 受ける形を整数に限ることが、数値として解釈できない綴りをホストへ届かせ
    // ない形である。実数も文字列も種別と形の照合で落ちる。
    let harness = harness_with_reference_effect();
    for item in [LAYER_RANGE_ITEM, SCENE_ITEM] {
        for value in [
            ItemValue::Number {
                value: FiniteF64::try_new(1.0).expect("有限値"),
            },
            ItemValue::Text {
                value: "1".to_string(),
            },
        ] {
            let kind = value.kind();
            let error = harness
                .edit
                .set_object_item(&set_reference_item(&harness, item, value))
                .expect_err("整数以外の形が受理されました");
            assert_eq!(
                error.error_code(),
                ErrorCode::InvalidArgument,
                "{item}/{kind}"
            );
            assert_eq!(error.details()["value_kind"], json!(kind), "{item}/{kind}");
        }
    }
    harness.assert_untouched();
}

#[test]
fn a_value_the_host_rewrites_is_reported_as_a_failure() {
    // ホストは書き込みの成否を返さない。読み直して照合しなければ、値が書き
    // 換えられたことを利用者もクライアントも知る手段が無い。
    for (item, requested, current) in rewritten_item_cases() {
        let harness = harness_with_choice_effect();
        let error = harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector: harness.effect_selector(1, 300, SHAPE, 0),
                item: item.to_string(),
                value: requested,
            })
            .expect_err("ホストが書き換えた値が成功として返りました");

        assert_eq!(
            error.error_code(),
            ErrorCode::UnsupportedOperation,
            "{item}"
        );
        assert_eq!(
            error.details()["reason"],
            json!("item_value_not_applied"),
            "{item}"
        );
        assert_eq!(error.details()["observed_value"], json!(current), "{item}");
    }
}

#[test]
fn the_failure_carries_the_host_value_and_not_the_requested_one() {
    // 応答へ反響させてよいのはホストの現在の状態だけである。要求された値は
    // 要求元の内容であり、載せない。
    let harness = harness_with_choice_effect();
    let requested = "NoSuchFont12345";
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 300, SHAPE, 0),
            item: "フォント".to_string(),
            value: ItemValue::Font {
                name: requested.to_string(),
            },
        })
        .expect_err("未登録のフォント名が成功として返りました");

    let details = error.details().to_string();
    assert!(
        details.contains(DEFAULT_FONT),
        "読み直した実値が載っていません: {details}"
    );
    assert!(
        !details.contains(requested),
        "要求された値が反響しています: {details}"
    );
}

/// 対象がいま持っている設定値を読み取り経路から得る。
///
/// 応答が返す値ではなく、プロジェクトの状態そのものを見る。応答だけを見ると、
/// 巻き戻したと名乗るだけで戻していない実装を区別できない。
fn stored_shape_item(harness: &Harness, item: &str) -> ItemValue {
    let selector = harness.selector(1, 300);
    harness
        .read
        .get_object(&selector)
        .expect("対象の詳細")
        .effects
        .into_iter()
        .find(|effect| effect.name == SHAPE)
        .expect("effect がありません")
        .items
        .into_iter()
        .find(|entry| entry.name == item)
        .unwrap_or_else(|| panic!("設定項目 {item} がありません"))
        .value
}

/// 書き込み検証が落ちる要求と、対象を戻す書き込みが要るか。
///
/// **2 つの階級を並べる。** 拒否ではホストが値を動かさず、戻すものが無い。
/// 倒しでは値が動くため書き戻す。要求元へ返す失敗はどちらも同じであり、違うのは
/// 我々が発行する書き込みの数だけである。
fn failed_verification_cases() -> Vec<(&'static str, ItemValue, bool)> {
    vec![
        // 拒否。選択肢に無い値は黙殺され、値も fingerprint も動かない。
        (
            "図形の種類",
            ItemValue::Choice {
                value: "存在しない形".to_string(),
            },
            false,
        ),
        // 拒否。書式の合わない色は既定値の白へ落ちるが、変更前の値が既に白で
        // ある。**ホストが書き込んだかどうかではなく、値が動いたかで分ける。**
        (
            "色",
            ItemValue::Color {
                value: "#ff0000".to_string(),
            },
            false,
        ),
        // 拒否。未登録のフォント名は黙殺される。
        (
            "フォント",
            ItemValue::Font {
                name: "NoSuchFont12345".to_string(),
            },
            false,
        ),
        // 倒し。値域を外れた数値は境界へ切り詰められ、変更前の値が失われる。
        (
            "サイズ",
            ItemValue::Number {
                value: FiniteF64::try_new((MAX_ITEM_VALUE + 400) as f64).expect("有限値"),
            },
            true,
        ),
        // 倒し。桁の多い小数は項目の桁へ丸められる。
        (
            "サイズ",
            ItemValue::Number {
                value: FiniteF64::try_new(1.2345).expect("有限値"),
            },
            true,
        ),
    ]
}

#[test]
fn the_failed_verification_cases_cover_both_classes() {
    // 片方の階級しか無い一覧では、「常に戻す」実装も「一度も戻さない」実装も
    // 検査を通り抜ける。**費用の検査は 2 つの階級の差でしか成立しない。**
    let classes: Vec<bool> = failed_verification_cases()
        .into_iter()
        .map(|(_, _, restoring)| restoring)
        .collect();
    assert!(classes.contains(&true), "倒しの階級の標本がありません");
    assert!(classes.contains(&false), "拒否の階級の標本がありません");

    // ホストが値を書き換える標本は、階級を割り当てないまま取り残さない。
    for (item, requested, _) in rewritten_item_cases() {
        assert!(
            failed_verification_cases()
                .iter()
                .any(|(name, value, _)| *name == item && *value == requested),
            "{item} へ {} を書く標本が階級を持ちません",
            requested.kind()
        );
    }
}

#[test]
fn a_failed_verification_leaves_the_item_at_its_value_before_the_write() {
    // **失敗が状態を残さない。** ホストが値を倒した場合も、要求元から見た対象は
    // 書き込みの前と同じ値を持つ。
    for (item, requested, _) in failed_verification_cases() {
        let harness = harness_with_choice_effect();
        let before = stored_shape_item(&harness, item);
        let error = harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector: harness.effect_selector(1, 300, SHAPE, 0),
                item: item.to_string(),
                value: requested,
            })
            .err()
            .unwrap_or_else(|| panic!("{item} の書き込み検証が落ちませんでした"));

        assert_eq!(
            error.details()["reason"],
            json!("item_value_not_applied"),
            "{item}"
        );
        assert_eq!(
            stored_shape_item(&harness, item),
            before,
            "{item} が書き込み前の値へ戻っていません"
        );
    }
}

#[test]
fn a_value_the_host_refuses_costs_no_restoring_write() {
    // **費用の検査である。** 値が正しいことだけを見ると、戻すものが無い階級で
    // 無駄な書き込みを発行しても通る。発行の回数そのものを数える。
    for (item, requested, restoring) in failed_verification_cases() {
        let harness = harness_with_choice_effect();
        let selector = harness.effect_selector(1, 300, SHAPE, 0);
        let origin = raw_item_value(&stored_shape_item(&harness, item));
        harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector,
                item: item.to_string(),
                value: requested,
            })
            .err()
            .unwrap_or_else(|| panic!("{item} の書き込み検証が落ちませんでした"));

        let writes = harness.host.item_value_arguments();
        assert_eq!(
            writes.len(),
            1 + usize::from(restoring),
            "{item} へ発行した書き込みの回数が想定と違います: {writes:?}"
        );
        if restoring {
            // 書き戻すのはホストが直前に返した生文字列そのものである。読み取り
            // 経路が解釈した値を組み立て直していれば、ここで食い違う。
            assert_eq!(writes[1], origin, "{item} の巻き戻しが別の値を書きました");
        }
    }
}

#[test]
fn a_selector_survives_a_failed_verification() {
    // **A2 の本体である。** 戻せば fingerprint も戻る——内容ハッシュであり、
    // 同じ内容へ戻せば同じ値が返る。要求元は失敗の後も同じ selector で続けられ、
    // 復旧に get_object を要さない。
    //
    // 倒しの階級で見る。拒否の階級は巻き戻しが無くても selector が生き残るため、
    // 復元したことを確かめられない。
    let harness = harness_with_choice_effect();
    let selector = harness.effect_selector(1, 300, SHAPE, 0);
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: selector.clone(),
            item: "サイズ".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new((MAX_ITEM_VALUE + 400) as f64).expect("有限値"),
            },
        })
        .expect_err("切り詰められた値が成功として返りました");
    assert_eq!(error.details()["reason"], json!("item_value_not_applied"));

    // 失敗の前に得たオブジェクトの selector で読み直せる。
    harness
        .read
        .get_object(&selector.object)
        .expect("失敗の後にオブジェクトの selector が死にました");
    // 同じ effect の selector で次の書き込みも通る。effect の fingerprint も
    // 戻っている。
    harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector,
            item: "サイズ".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new(2.0).expect("有限値"),
            },
        })
        .expect("失敗の後の書き込みが古い selector で拒否されました");
}

#[test]
fn a_restore_that_does_not_take_effect_names_the_state_as_unknown() {
    // 巻き戻しの書き込みも失敗し得る。**「書き込み API が真を返した」を成功と
    // 読まない**——読み直して元の文字列と一致することだけが根拠である。
    let harness = harness_with_choice_effect();
    let selector = harness.effect_selector(1, 300, SHAPE, 0);
    let before = stored_shape_item(&harness, "サイズ");
    harness
        .host
        .arm(|knobs| knobs.fault = Some(Fault::IgnoreItemRestore));

    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector,
            item: "サイズ".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new((MAX_ITEM_VALUE + 400) as f64).expect("有限値"),
            },
        })
        .expect_err("切り詰められた値が成功として返りました");

    assert_eq!(error.details()["reason"], json!("item_value_not_applied"));
    assert_eq!(error.details()["consistency_unknown"], json!(true));
    // 巻き戻しは発行したが効かなかった。効かなかったことは対象の値に現れる。
    assert_eq!(harness.host.item_value_arguments().len(), 2);
    assert_ne!(
        stored_shape_item(&harness, "サイズ"),
        before,
        "戻せていないのに戻ったと名乗っています"
    );
}

#[test]
fn a_read_back_that_fails_restores_and_names_the_state_as_unknown() {
    // 書き込んだ後の読み直しそのものが落ちると、適用されたかを確かめられない。
    // **材料は手元にあるため戻しに行く。** 戻せたことも確かめられないため、
    // 「戻せた」とは名乗らない。**確かめずに戻せたと名乗る形が Phase 4.5 の
    // 出発点である。**
    let harness = harness_with_choice_effect();
    let selector = harness.effect_selector(1, 300, SHAPE, 0);
    harness
        .host
        .arm(|knobs| knobs.fault = Some(Fault::ItemValueUnreadableAfterMutation));

    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector,
            item: "サイズ".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new(2.0).expect("有限値"),
            },
        })
        .expect_err("読み直せないまま成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    let details = error.details();
    assert_eq!(details["sdk_operation"], json!("get_effect_item_value"));
    assert_eq!(details["mutation_issued"], json!(true));
    // 巻き戻しを試みている。書き込みは前向きと戻しの 2 回発行された。
    assert_eq!(
        harness.host.item_value_arguments().len(),
        2,
        "巻き戻しを試みていません"
    );
    assert_eq!(details["restored"], json!(false));
    assert_eq!(details["consistency_unknown"], json!(true));
}

#[test]
fn the_observed_value_and_the_current_value_are_not_interchanged() {
    // **2 つのキーは別の時点を指す。** `observed_value` は書き込みの後に
    // 読み直した値であり、応答が返る時点の現在値ではない——巻き戻しが済んで
    // いる。`current_value` は書き込みを発行する前に落ちた失敗が運ぶ値であり、
    // 文字どおり現在値である。取り違えれば、要求元は戻したはずの状態を自分で
    // 再現する要求を組み立てる。
    let harness = harness_with_choice_effect();
    let not_applied = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 300, SHAPE, 0),
            item: "サイズ".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new((MAX_ITEM_VALUE + 400) as f64).expect("有限値"),
            },
        })
        .expect_err("切り詰められた値が成功として返りました");
    let details = not_applied.details();
    assert_eq!(details["reason"], json!("item_value_not_applied"));
    assert_eq!(
        details["observed_value"],
        json!(format!("{MAX_ITEM_VALUE}.00"))
    );
    assert!(
        details.get("current_value").is_none(),
        "巻き戻した後の値を現在値として名乗っています: {details}"
    );

    let track = harness_with_track_effect();
    let would_be_lost = track
        .edit
        .set_object_item(&set_movement(
            &track,
            ItemValue::Number {
                value: FiniteF64::try_new(0.0).expect("有限値"),
            },
        ))
        .expect_err("移動を消す書き込みが成功として返りました");
    let details = would_be_lost.details();
    assert_eq!(details["reason"], json!("track_movement_present"));
    assert!(details["current_value"].is_string());
    assert!(
        details.get("observed_value").is_none(),
        "書き込みを発行していない失敗が読み直した値を名乗っています: {details}"
    );
}

#[test]
fn the_restore_outcome_is_named_for_both_classes() {
    // **拒否の階級でも `restored` は真である。** 戻す書き込みが要らなかった
    // だけで、対象は書き込み前の値を持つ。要求元が取る行動は倒しの階級と
    // 変わらない。
    for (item, requested, _) in failed_verification_cases() {
        let harness = harness_with_choice_effect();
        let error = harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector: harness.effect_selector(1, 300, SHAPE, 0),
                item: item.to_string(),
                value: requested,
            })
            .err()
            .unwrap_or_else(|| panic!("{item} の書き込み検証が落ちませんでした"));

        let details = error.details();
        assert_eq!(details["restored"], json!(true), "{item}");
        assert!(
            details.get("consistency_unknown").is_none(),
            "{item} は戻せているのに中途半端な状態を名乗りました: {details}"
        );
    }

    // 戻せなかったときだけ偽になり、`consistency_unknown` が対で立つ。
    let harness = harness_with_choice_effect();
    let selector = harness.effect_selector(1, 300, SHAPE, 0);
    harness
        .host
        .arm(|knobs| knobs.fault = Some(Fault::IgnoreItemRestore));
    let details = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector,
            item: "サイズ".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new((MAX_ITEM_VALUE + 400) as f64).expect("有限値"),
            },
        })
        .expect_err("切り詰められた値が成功として返りました")
        .details();
    assert_eq!(details["restored"], json!(false));
    assert_eq!(details["consistency_unknown"], json!(true));
}

#[test]
fn a_restored_write_advances_the_revision_at_most_once() {
    // 巻き戻しは同じ許可で発行する。許可は最初の発行で確定した revision を
    // 保つため、書き込みが 2 回でも revision は 1 つしか進まない。
    let harness = harness_with_choice_effect();
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 300, SHAPE, 0),
            item: "サイズ".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new((MAX_ITEM_VALUE + 400) as f64).expect("有限値"),
            },
        })
        .expect_err("切り詰められた値が成功として返りました");

    // 巻き戻しが実際に発行されていなければ、revision が 1 に留まることは何も
    // 言っていない。
    assert_eq!(
        harness.host.item_value_arguments().len(),
        2,
        "巻き戻しの書き込みが発行されていません"
    );
    assert_eq!(harness.project.revision(), 1, "revision が 2 つ進みました");
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(error.details()["current_project_revision"], json!(1));
}

/// 候補の表だけが主張する値。**ホストが受け付ける値とは重ならない。**
const HINTED_VALUES: [&str; 2] = ["表だけにある形", "表だけにあるもう 1 つの形"];

/// 候補の表を持たせた [`shape_catalog_entry`]。
///
/// 表はカタログの側に持つ。読み取り経路が候補を引く先と同じ場所であり、
/// 書き込みの経路がそこを見るようになれば、この表を変えた結果が成否に現れる。
fn shape_catalog_entry_with_choices(values: &[&str]) -> FakeCatalogEntry {
    shape_catalog_entry_with_facets(ItemFacets {
        choices: Some(ItemChoices {
            values: values.iter().map(|value| (*value).to_string()).collect(),
            source: TableSource::Sidecar,
        }),
        range: None,
    })
}

/// 面の組を全項目へ持たせた [`shape_catalog_entry`]。
fn shape_catalog_entry_with_facets(facets: ItemFacets) -> FakeCatalogEntry {
    let facets = shape_catalog_entry()
        .items
        .into_iter()
        .map(|item| (item.name, facets.clone()))
        .collect();
    FakeCatalogEntry {
        facets,
        ..shape_catalog_entry()
    }
}

/// 候補の表を差し替えたうえで、選択肢の項目へ 1 件書き込む。
///
/// 表の中身を変えても結果が変わらないことを比べられるよう、成否と失敗の種別を
/// 1 つの値へ畳んで返す。
fn write_choice_with_table(
    table: Option<&[&str]>,
    item: &str,
    value: &str,
) -> Result<String, String> {
    let harness = Harness::with(|host| {
        host.catalog.push(match table {
            Some(values) => shape_catalog_entry_with_choices(values),
            None => shape_catalog_entry(),
        });
        host.scene.get_mut().unwrap().layers[1].objects[1]
            .effects
            .push(shape(0));
    });
    harness
        .edit
        .set_object_item(&set_choice_item(&harness, item, value))
        .map(|outcome| raw_item_value(&changed_item(&outcome, item)))
        .map_err(|error| format!("{:?} {}", error.error_code(), error.details()["reason"]))
}

#[test]
fn the_choices_table_never_decides_whether_a_write_goes_through() {
    // **候補はヒントであってゲートではない。** 表に無い値でも書き込みは通し、
    // 表に在る値が必ず通るとも約束しない。可否を決めるのはホストであり、表が
    // 実態から外れたときに事前検証を掛けていれば、正しい値が通らなくなる。
    //
    // 移動方法の一覧とは性質が違う。あちらは一覧に無い名前を書くとホストの
    // プロセスが落ちるため通す選択肢が無いが、候補を外した書き込みは最悪でも
    // ホストが値を無視するだけである。
    //
    // **覆う範囲は [`the_range_table_never_decides_whether_a_write_goes_through`]
    // と同じである。** 表はフェイクのカタログ側にあり、捕まえられるのは
    // [`crate::read::host::ReadHost::effect_facets`] を経由するゲートだけである。
    for value in HINTED_VALUES {
        assert!(
            !CHOICE_VALUES.contains(&value),
            "表だけの値としてホストが受け付ける値を使っています"
        );
    }

    for item in CHOICE_ITEMS {
        for value in [CHOICE_VALUES[1], HINTED_VALUES[0]] {
            // 表を持たない環境での結果を基準に取る。
            let baseline = write_choice_with_table(None, item, value);
            for table in [
                // 表が別の値だけを主張する。
                &HINTED_VALUES[..],
                // 表が 1 件も候補を持たない。
                &[][..],
                // 表が書こうとしている値を含む。
                &[value][..],
            ] {
                assert_eq!(
                    write_choice_with_table(Some(table), item, value),
                    baseline,
                    "{item} へ {value} を書く成否が表の中身で変わりました"
                );
            }
        }
    }

    // 基準そのものはホストの受け付ける値で決まっている。両方が同じ結果になる
    // 表では、表が効いていないことを確かめられない。
    assert!(write_choice_with_table(None, CHOICE_ITEMS[0], CHOICE_VALUES[1]).is_ok());
    assert!(write_choice_with_table(None, CHOICE_ITEMS[0], HINTED_VALUES[0]).is_err());
}

/// 数値の項目。ホストは [`MIN_ITEM_VALUE`]〜[`MAX_ITEM_VALUE`] へ倒す。
const NUMBER_ITEM: &str = "サイズ";

/// 値域の表を差し替えたうえで、数値の項目へ 1 件書き込む。
///
/// 表の中身を変えても結果が変わらないことを比べられるよう、成否と失敗の種別を
/// 1 つの値へ畳んで返す。
fn write_number_with_range(range: Option<(f64, f64)>, value: f64) -> Result<String, String> {
    let harness = Harness::with(|host| {
        host.catalog.push(match range {
            Some((min, max)) => shape_catalog_entry_with_facets(ItemFacets {
                choices: None,
                range: Some(ItemRange {
                    min: FiniteF64::try_new(min),
                    max: FiniteF64::try_new(max),
                    decimals: Some(0),
                    source: TableSource::Sidecar,
                }),
            }),
            None => shape_catalog_entry(),
        });
        host.scene.get_mut().unwrap().layers[1].objects[1]
            .effects
            .push(shape(0));
    });
    harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 300, SHAPE, 0),
            item: NUMBER_ITEM.to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new(value).expect("有限値"),
            },
        })
        .map(|outcome| raw_item_value(&changed_item(&outcome, NUMBER_ITEM)))
        .map_err(|error| format!("{:?} {}", error.error_code(), error.details()["reason"]))
}

#[test]
fn the_range_table_never_decides_whether_a_write_goes_through() {
    // **値域もヒントであってゲートではない。** 表の値域を外れる値でも書き込みは
    // 通し、表の値域に収まる値が必ず通るとも約束しない。可否を決めるのはホスト
    // であり、書き込みの経路は書いた値を読み直して照合する。
    //
    // **値域は候補より外れやすい。** 候補の陳腐化は足りなくなるだけだが、値域の
    // 陳腐化は狭くなる——版が上がって上限が広がったとき、事前検証を掛けて
    // いれば通るはずの値をこちら側が拒む。
    //
    // # この検査が覆う範囲
    //
    // 表はフェイクのカタログ側にあり、読み取り経路が面を引く先と同じ場所で
    // ある。**捕まえられるのは
    // [`crate::read::host::ReadHost::effect_facets`] を経由して面を読むゲート
    // だけである。** [`crate::item_facets::table`] を直に読むゲートはここを
    // 素通りする——あちらは実行ファイルへ埋め込んだ基底とデータディレクトリの
    // サイドカーだけを見ており、フェイクのカタログを見ないためである。
    //
    // **その隙間を塞ぐ手が現状は無い。** 表は要求ごとに解決するものではなく
    // 起動から 1 度きりであり、差し替える口が製品側に無い。検査のために口を
    // 開ければ、塞ごうとしている性質そのものを検査のために曲げることになる。
    let inside = (MAX_ITEM_VALUE / 2) as f64;
    let outside = (MAX_ITEM_VALUE + 400) as f64;

    for value in [inside, outside] {
        // 表を持たない環境での結果を基準に取る。
        let baseline = write_number_with_range(None, value);
        for range in [
            // 表が書こうとしている値より狭い範囲を主張する。
            (0.0, 1.0),
            // 表がホストより広い範囲を主張する。**版が上がった後の状態である。**
            (0.0, f64::from(u16::MAX)),
            // 表が書こうとしている値を含む。
            (value - 1.0, value + 1.0),
        ] {
            assert_eq!(
                write_number_with_range(Some(range), value),
                baseline,
                "{value} を書く成否が表の値域で変わりました"
            );
        }
    }

    // 基準そのものはホストの倒しで決まっている。両方が同じ結果になる値では、
    // 表が効いていないことを確かめられない。
    assert!(write_number_with_range(None, inside).is_ok());
    assert!(write_number_with_range(None, outside).is_err());
}

#[test]
fn a_choice_value_the_host_accepts_succeeds() {
    for item in CHOICE_ITEMS {
        let harness = harness_with_choice_effect();
        harness.host.clear_calls();

        let outcome = harness
            .edit
            .set_object_item(&set_choice_item(&harness, item, CHOICE_VALUES[1]))
            .unwrap_or_else(|error| panic!("{item} で選択肢に在る値が拒否されました: {error}"));

        assert_eq!(
            changed_item(&outcome, item),
            ItemValue::Choice {
                value: CHOICE_VALUES[1].to_string(),
            },
            "{item}"
        );
        let calls = harness.host.calls();
        let first = first_mutation(&calls).expect("変更 API が呼ばれていません");
        assert_eq!(
            count(&calls[first..], ITEM_VALUE),
            1,
            "{item} の照合の読み直しは 1 回だけです: {calls:?}"
        );
    }
}

#[test]
fn a_choice_value_read_from_the_object_can_be_written_straight_back() {
    // 読み取り経路が返した値を組み替えずに書き戻せることを、往復の形で固定
    // する。読み取り口はフェイクが保持する値をそのまま返すため、種別から値へ
    // の写像そのものはここを通らない。写像との突き合わせは写像を直接呼ぶ側が
    // 持ち、ここが確かめるのは書き込み側が同じ値を受理することである。
    for item in CHOICE_ITEMS {
        let harness = harness_with_choice_effect();
        let selector = harness.selector(1, 300);
        let detail = harness
            .read
            .get_object(&selector)
            .expect("対象の詳細を取得できませんでした");
        let value = detail
            .effects
            .iter()
            .find(|effect| effect.name == SHAPE)
            .expect("effect がありません")
            .items
            .iter()
            .find(|entry| entry.name == item)
            .unwrap_or_else(|| panic!("設定項目 {item} がありません"))
            .value
            .clone();
        assert!(
            matches!(value, ItemValue::Choice { .. }),
            "{item} が選択肢として読めません: {value:?}"
        );

        let outcome = harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector: harness.effect_selector(1, 300, SHAPE, 0),
                item: item.to_string(),
                value: value.clone(),
            })
            .unwrap_or_else(|error| panic!("{item} の書き戻しが失敗しました: {error}"));

        assert_eq!(changed_item(&outcome, item), value, "{item}");
    }
}

#[test]
fn a_value_the_host_writes_back_in_another_notation_is_not_a_mismatch() {
    // ホストは受理した値の表記も整える——色は小文字へ、実数は項目の桁へ揃える。
    // テキストは書き込み経路が符号化した表記のまま返る。**比較を種別ごとに
    // 定めたのは、これらを失敗と誤診断しないためである。** 一致の判定をバイト
    // 比較へ倒すと、ここが偽陽性の一覧になる。
    let cases = [
        (
            "色",
            ItemValue::Color {
                value: "FFAA00".to_string(),
            },
            ItemValue::Color {
                value: "ffaa00".to_string(),
            },
        ),
        (
            "サイズ",
            ItemValue::Number {
                value: FiniteF64::try_new(MAX_ITEM_VALUE as f64).expect("有限値"),
            },
            ItemValue::Number {
                value: FiniteF64::try_new(MAX_ITEM_VALUE as f64).expect("有限値"),
            },
        ),
        (
            "メモ",
            ItemValue::Text {
                value: "上\r\n下".to_string(),
            },
            ItemValue::Text {
                value: "上\n下".to_string(),
            },
        ),
    ];
    for (item, requested, stored) in cases {
        let harness = harness_with_choice_effect();
        let outcome = harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector: harness.effect_selector(1, 300, SHAPE, 0),
                item: item.to_string(),
                value: requested.clone(),
            })
            .unwrap_or_else(|error| panic!("{item} の書き込みが失敗として扱われました: {error}"));

        assert_eq!(changed_item(&outcome, item), stored, "{item}");

        // **標本が食い違いを含むことを確かめる。** 要求とホストの間に何の違いも
        // 無い標本は、バイト比較のままの実装でも通ってしまい、種別ごとの比較を
        // 定めた意味を試せていない。違いの現れ方は種別で分かれる——色とテキスト
        // は値の表現が、実数は表記だけが違う。
        let written = harness.host.item_value_arguments();
        assert_eq!(written.len(), 1, "{item} の書き込みが 1 回ではありません");
        assert!(
            requested != stored || raw_item_value(&stored) != written[0],
            "{item} の標本は要求とホストの間に違いが無く、比較の違いを試せていません"
        );
    }
}

#[test]
fn a_value_the_host_rewrote_is_told_apart_from_a_change_that_did_not_apply() {
    // 値域も選択肢も列挙できない以上、当て推量が外れることは常態である。
    // ヘッダーが変更を拒む旨を記していない setter の不一致と畳むと、要求元は
    // 「異常」と「よくある入力誤り」を区別できない。前者は報告する対象であり、
    // 後者は読み直した実値を見て送り直す対象である。
    let choice = harness_with_choice_effect();
    let rejected = choice
        .edit
        .set_object_item(&set_choice(&choice, "存在しない形"))
        .expect_err("選択肢に無い値が成功として返りました");

    let ignored =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreObjectName)));
    let not_applied = ignored
        .edit
        .set_object_name(&SetObjectNameParams {
            selector: ignored.selector(1, 100),
            name: Some("新しい名前".to_string()),
        })
        .expect_err("無視された改名が成功として返りました");

    assert_eq!(rejected.error_code(), not_applied.error_code());
    assert_eq!(
        rejected.details()["reason"],
        json!("item_value_not_applied")
    );
    assert_eq!(not_applied.details()["reason"], json!("change_not_applied"));
    assert_ne!(
        rejected.details()["reason"],
        not_applied.details()["reason"],
        "2 つの失敗が同じ名前を名乗っています"
    );
}

#[test]
fn every_kind_of_rewrite_shares_one_reason() {
    // 「ホストが拒んだ」と「ホストが別の値へ倒した」を区別する材料がこちら側に
    // 無い。どちらも読み直しが要求と違うとしか観測できないため、種別ごとに名前を
    // 割らない。
    let mut reasons: Vec<String> = Vec::new();
    for (item, requested, _) in rewritten_item_cases() {
        let harness = harness_with_choice_effect();
        let error = harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector: harness.effect_selector(1, 300, SHAPE, 0),
                item: item.to_string(),
                value: requested,
            })
            .expect_err("ホストが書き換えた値が成功として返りました");
        reasons.push(error.details()["reason"].to_string());
    }
    let choice = harness_with_choice_effect();
    reasons.push(
        choice
            .edit
            .set_object_item(&set_choice(&choice, "存在しない形"))
            .expect_err("選択肢に無い値が成功として返りました")
            .details()["reason"]
            .to_string(),
    );
    assert!(!reasons.is_empty());
    assert!(
        reasons.iter().all(|reason| *reason == reasons[0]),
        "書き換えの種類ごとに名前が分かれています: {reasons:?}"
    );
}

#[test]
fn a_choice_value_the_host_ignores_is_reported_as_a_failure() {
    // SDK は選択肢を列挙する手段を持たず、選択肢に無い値を渡しても失敗を返さず
    // に無視する。読み直して照合しなければ、当て推量が外れたことを成功として
    // 報告してしまう。
    let rejected = "存在しない形";
    assert!(
        !CHOICE_VALUES.contains(&rejected),
        "ホストが受け付ける値を無効な値として使っています"
    );

    for item in CHOICE_ITEMS {
        let harness = harness_with_choice_effect();
        let error = harness
            .edit
            .set_object_item(&set_choice_item(&harness, item, rejected))
            .expect_err("選択肢に無い値が成功として返りました");

        assert_eq!(
            error.error_code(),
            ErrorCode::UnsupportedOperation,
            "{item}"
        );
        assert_eq!(
            error.details()["reason"],
            json!("item_value_not_applied"),
            "{item}"
        );
        // 書き込みの後に読み直した値が載る。この階級では変更前の値そのもの
        // であり、ホストは何も倒していない。
        assert_eq!(
            error.details()["observed_value"],
            json!(CHOICE_VALUES[0]),
            "{item}"
        );
        assert!(!error.retryable(), "{item} は読み直しても有効になりません");
    }
}
