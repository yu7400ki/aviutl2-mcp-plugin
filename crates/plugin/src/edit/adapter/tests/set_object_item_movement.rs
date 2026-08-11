//! 移動(トラックバー)を持つ設定項目の統合テスト。

use super::*;

/// 中間点を持たない対象のトラックバーへ値を書き込む要求を組み立てる。
fn set_track_item_without_midpoints(
    harness: &Harness,
    item: &str,
    value: ItemValue,
) -> SetObjectItemParams {
    SetObjectItemParams {
        selector: harness.effect_selector(1, 300, COORDINATE, 0),
        item: item.to_string(),
        value,
    }
}

#[test]
fn a_movement_the_host_knows_is_written_and_read_back() {
    // 一覧にある移動方法は書ける。ホストが桁を整えて返しても照合は通る。
    let harness = harness_with_track_effect();
    let outcome = harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            movement(&[0.0, 50.0, 100.0], "曲線移動"),
        ))
        .expect("一覧にある移動方法が拒否されました");

    assert_eq!(
        changed_item(&outcome, MOVING_ITEM),
        movement(&[0.0, 50.0, 100.0], "曲線移動")
    );
    assert!(harness.host.fatal_movement_writes().is_empty());
}

/// 対象がいま持っている移動の値を読み取り経路から得る。
fn stored_movement(harness: &Harness) -> ItemValue {
    harness
        .read
        .get_object(&harness.selector(1, 100))
        .expect("対象の詳細")
        .effects
        .into_iter()
        .find(|effect| effect.name == COORDINATE)
        .expect("effect がありません")
        .items
        .into_iter()
        .find(|entry| entry.name == MOVING_ITEM)
        .expect("設定項目がありません")
        .value
}

#[test]
fn a_movement_parameter_the_host_replaces_after_the_write_is_reported_as_a_failure() {
    // **書き込みの直後の読みは要求どおりを返す。** ホストが保存値を既定値へ
    // 差し替えるのはその後の解釈し直しであり、対象を読み直した後の設定値に
    // 現れる。直後の読みだけで照合すると、捨てられたパラメータが成功として返る。
    let harness = harness_with_track_effect();
    let error = harness
        .edit
        .set_object_item(&set_movement(&harness, movement_with_mismatched_params()))
        .expect_err("個数の合わない移動パラメータが成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("item_value_not_applied"));
    // 載るのは差し替えの後の値である。要求した 2 つはどこにも残っていない。
    assert_eq!(
        error.details()["observed_value"],
        json!(replaced_movement_raw())
    );
}

#[test]
fn a_replaced_movement_parameter_names_the_write_as_issued_and_restored() {
    // 書き込みは発行済みであり、失敗は既存の書き込み検証と同じ形を名乗る。
    let harness = harness_with_track_effect();
    let before = stored_movement(&harness);
    let error = harness
        .edit
        .set_object_item(&set_movement(&harness, movement_with_mismatched_params()))
        .expect_err("個数の合わない移動パラメータが成功として返りました");

    let details = error.details();
    assert_eq!(details["reason"], json!("item_value_not_applied"));
    assert_eq!(details["mutation_issued"], json!(true));
    assert_eq!(details["restored"], json!(true));
    assert!(
        details.get("consistency_unknown").is_none(),
        "戻せているのに中途半端な状態を名乗りました: {details}"
    );
    assert_eq!(
        stored_movement(&harness),
        before,
        "書き込み前の移動へ戻っていません"
    );
}

#[test]
fn a_movement_written_without_parameters_survives_the_default_the_host_adds() {
    // **偽の失敗を作らない。** パラメータを省いた書き込みにはホストが既定値を
    // 書き足すため、読み直しは書いた綴りと違う。求めたのはその既定値そのもので
    // あり、失敗ではない。
    let harness = harness_with_track_effect();
    let outcome = harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            movement(&[0.0, 50.0, 100.0], TRACK_DEFAULT_PARAM.0),
        ))
        .expect("既定値の書き足しが失敗として返りました");

    assert_eq!(
        changed_item(&outcome, MOVING_ITEM),
        movement_with_params(
            &[0.0, 50.0, 100.0],
            TRACK_DEFAULT_PARAM.0,
            &[TRACK_DEFAULT_PARAM.1]
        )
    );
}

#[test]
fn a_time_control_movement_passes_even_though_the_host_reports_no_parameters() {
    // **偽の失敗を作らない。** 時間制御の変種はパラメータを保存も評価もするのに、
    // ホストの報告だけが 0 件になる。件数を照合の材料にすれば、正しい綴りが
    // 1 つ残らず失敗になる。
    let harness = harness_with_track_effect();
    let requested = movement_with_params(
        &[0.0, 50.0, 100.0],
        TRACK_TIME_CONTROL_MODE,
        &[0.5, 0.0, 0.0, 0.0],
    );
    let outcome = harness
        .edit
        .set_object_item(&set_movement(&harness, requested.clone()))
        .expect("時間制御の変種の正しい綴りが失敗として返りました");

    assert_eq!(changed_item(&outcome, MOVING_ITEM), requested);
    let reported = outcome
        .effect
        .as_ref()
        .expect("変更後の effect")
        .items
        .iter()
        .find(|entry| entry.name == MOVING_ITEM)
        .expect("設定項目がありません")
        .track
        .as_ref()
        .expect("移動情報");
    assert!(
        reported.params.is_empty(),
        "報告が 0 件になる標本ではありません: {:?}",
        reported.params
    );
}

#[test]
fn a_scalar_that_would_erase_a_movement_is_refused() {
    // ホストは移動を持つ項目へ数値を書くと、移動も加速も中間点無視も捨てて
    // 成功を返す。生の文字列を渡しても同じであり、止められる場所はここしかない。
    let harness = harness_with_track_effect();
    let error = harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            ItemValue::Number {
                value: FiniteF64::try_new(0.0).expect("有限値"),
            },
        ))
        .expect_err("移動を消す書き込みが成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("track_movement_present"));
    // 対象がいま持つ移動を載せる。要求元はこれを読んで書き戻すか消すかを決める。
    assert_eq!(
        error.details()["current_value"],
        json!("0.00,50.00,100.00,直線移動,0|")
    );
    // 書き込みは発行していない。発行してしまえば移動は復元できない。
    harness.assert_untouched();
}

#[test]
fn a_movement_can_be_added_to_an_item_that_has_none() {
    // **アニメーションを作る経路がこれである。** 静的なトラックバーへ移動を
    // 書くと新しく移動が付く。現在値で拒むと、移動は既に移動を持つ項目にしか
    // 書けなくなり、alias から作り直すほかなくなる。
    let harness = harness_with_track_effect();
    let requested = movement(&[0.0, 50.0, 100.0], "直線移動");
    let outcome = harness
        .edit
        .set_object_item(&set_track_item(&harness, STATIC_ITEM, requested.clone()))
        .expect("移動を持たない項目へ移動を書けませんでした");

    assert_eq!(changed_item(&outcome, STATIC_ITEM), requested);

    // 読み直しても移動が付いている。応答だけが移動を名乗る状態と区別する。
    let detail = harness
        .read
        .get_object(&harness.selector(1, 100))
        .expect("対象の詳細");
    let item = detail
        .effects
        .iter()
        .find(|effect| effect.name == COORDINATE)
        .expect("effect がありません")
        .items
        .iter()
        .find(|item| item.name == STATIC_ITEM)
        .expect("設定項目がありません")
        .clone();
    assert_eq!(item.value, requested);
    assert_eq!(
        item.track.expect("移動情報").mode,
        "直線移動",
        "移動情報が付いていません"
    );
}

#[test]
fn a_movement_can_be_added_to_an_object_without_midpoints() {
    // 中間点を持たない対象の静的なトラックバーへ 2 値の移動を書くと成功する
    // （実測）。区間は 1 個であり、値は 2 個でなければならない。
    let harness = harness_with_track_effect();
    let requested = movement(&[0.0, 100.0], "直線移動");
    let outcome = harness
        .edit
        .set_object_item(&set_track_item_without_midpoints(
            &harness,
            STATIC_ITEM,
            requested.clone(),
        ))
        .expect("中間点を持たない対象へ移動を書けませんでした");
    assert_eq!(changed_item(&outcome, STATIC_ITEM), requested);

    // 区間 1 個に対して 3 値は多い。個数の規則は区間の数で決まり、両端で効く。
    let harness = harness_with_track_effect();
    let error = harness
        .edit
        .set_object_item(&set_track_item_without_midpoints(
            &harness,
            STATIC_ITEM,
            movement(&[0.0, 50.0, 100.0], "直線移動"),
        ))
        .expect_err("区間の数と合わない値が受理されました");
    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    assert_eq!(error.details()["reason"], json!("track_value_count"));

    // 同じ 2 値を、中間点を 1 つ持つ対象へ書くと個数が足りない。
    let harness = harness_with_track_effect();
    let error = harness
        .edit
        .set_object_item(&set_track_item(&harness, STATIC_ITEM, requested))
        .expect_err("区間の数と合わない値が受理されました");
    assert_eq!(error.details()["reason"], json!("track_value_count"));
}

#[test]
fn a_movement_written_to_an_item_that_cannot_hold_one_is_refused() {
    // ホストは移動を持ち得ない種別へ多値の文字列を渡しても先頭の値だけを使う。
    // 拒むのは種別と値の形の照合であり、移動の有無を見る判定ではない。
    let harness = harness_with_choice_effect();
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 300, SHAPE, 0),
            item: "メモ".to_string(),
            value: movement(&[0.0, 100.0], "直線移動"),
        })
        .expect_err("移動を持ち得ない種別への移動が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    harness.assert_untouched();
}

#[test]
fn a_movement_is_removed_by_writing_a_value_without_a_mode() {
    // 移動を消す手段はこれだけである。**表せるのだから、数値の書き込みが黙って
    // 消すことを成功として返す理由が無い。**
    let harness = harness_with_track_effect();
    let outcome = harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            ItemValue::Track(aviutl2_mcp_core::TrackValue {
                values: vec![FiniteF64::try_new(50.0).expect("有限値")],
                mode: None,
                params: Vec::new(),
                accelerate: false,
                decelerate: false,
                twopoint: false,
                reserved_flags: 0,
            }),
        ))
        .expect("移動を消す書き込みが拒否されました");

    assert_eq!(
        changed_item(&outcome, MOVING_ITEM),
        ItemValue::Number {
            value: FiniteF64::try_new(50.0).expect("有限値"),
        }
    );
    // 消した後は移動を持たない項目になる。数値で書き換えられる。
    harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            ItemValue::Number {
                value: FiniteF64::try_new(10.0).expect("有限値"),
            },
        ))
        .expect("移動を消した後の数値の書き込みが拒否されました");
}

#[test]
fn a_write_stops_when_the_current_value_cannot_be_read() {
    // 現在値を読めなければ移動の有無が分からない。読めないまま書き込むと、
    // 判定を迂回して移動が消え得る。**読めないことは、通してよい理由にならない。**
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::ItemValueUnreadable)));
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 40 },
        })
        .expect_err("現在値を読めないまま書き込みました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    harness.assert_untouched();
}

#[test]
fn a_movement_check_without_its_material_is_refused() {
    // 材料を読む条件と、移動の判定が要る条件は別の述語である。今日は前者が
    // 後者を含むが、片方だけを変えれば包含は破れる。そのとき「読んでいないから
    // 判定しない」と倒すと、移動を黙って消す書き込みが素通りする。**到達不能で
    // あることを根拠に分岐を消さず、到達したら落ちる形にする。**
    let items = vec![AvailableEffectItem {
        name: MOVING_ITEM.to_string(),
        item_type: EffectItemType::Number,
    }];
    let scalar = ItemValue::Number {
        value: FiniteF64::try_new(0.0).expect("有限値"),
    };
    let error = ensure_movement_write_with_origin(&items, MOVING_ITEM, &scalar, None)
        .expect_err("材料が無いまま移動の判定が素通りしました");
    assert_eq!(error.details()["reason"], json!("inverse_unavailable"));

    // 判定が要らない組み合わせは、材料が無くても通る。判定の対象そのものが無い。
    ensure_movement_write_with_origin(
        &items,
        MOVING_ITEM,
        &movement(&[0.0, 100.0], "直線移動"),
        None,
    )
    .expect("移動を書く要求まで拒否されました");

    // 材料があれば普段どおり判定する。拒否の向きは現在値が決める。
    ensure_movement_write_with_origin(
        &items,
        MOVING_ITEM,
        &scalar,
        Some("0.00,100.00,直線移動,0|"),
    )
    .expect_err("移動を消す書き込みが通りました");
    ensure_movement_write_with_origin(&items, MOVING_ITEM, &scalar, Some("50.00"))
        .expect("移動を持たない項目への数値が拒否されました");
}

#[test]
fn a_movement_read_from_the_object_can_be_written_straight_back() {
    // 読み取りが返した移動をそのまま書き戻せる。ホストが桁を整えても往復は
    // 成立し、対象の同一性も動かない。
    let harness = harness_with_track_effect();
    let selector = harness.selector(1, 100);
    let detail = harness.read.get_object(&selector).expect("対象の詳細");
    let effect = detail
        .effects
        .iter()
        .find(|effect| effect.name == COORDINATE)
        .expect("effect がありません")
        .clone();
    let value = effect
        .items
        .iter()
        .find(|item| item.name == MOVING_ITEM)
        .expect("設定項目がありません")
        .value
        .clone();
    assert!(
        matches!(value, ItemValue::Track(_)),
        "移動が移動として読めません: {value:?}"
    );

    let outcome = harness
        .edit
        .set_object_item(&set_movement(&harness, value.clone()))
        .expect("読み取った移動を書き戻せませんでした");

    assert_eq!(changed_item(&outcome, MOVING_ITEM), value);
    // 書き戻しても対象は変わっていない。fingerprint は設定値まで含めて算出
    // されるため、値が動けばここが食い違う。
    assert_eq!(outcome.object.expect("対象の概要").selector, selector);
    assert_eq!(outcome.effect.expect("effect"), effect);
}

#[test]
fn a_movement_with_an_unknown_mode_never_reaches_the_host() {
    // 一覧に無い移動方法を書くと実機はプロセスごと落ちる。**止められるのは
    // 書き込みの手前だけである。** 記録が空でなければ、検証を通り抜けた入力が
    // ホストへ届いている。panic は編集の入口が捕捉して失敗の応答へ畳むため、
    // 応答の形だけを見ても届いたことは分からない。
    let harness = harness_with_track_effect();
    let error = harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            movement(&[0.0, 50.0, 100.0], "存在しない移動"),
        ))
        .expect_err("存在しない移動方法が受理されました");

    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    assert_eq!(error.details()["reason"], json!("track_mode_unknown"));
    assert_eq!(
        harness.host.fatal_movement_writes(),
        Vec::<String>::new(),
        "落ちる移動方法がホストへ届きました"
    );
}

#[test]
fn a_rejected_movement_name_never_comes_back_in_the_failure() {
    // known_movements が運ぶのはホストの一覧であり、拒否された要求の名前では
    // ない。一覧に無いからこそ拒否されているのだから、要求の名前が紛れ込めば
    // それ自体が矛盾になる。
    //
    // **要求を通して組み立てた失敗で確かめる。** 手で組み立てた失敗では、
    // 要求の名前が応答へ入り込む経路そのものを通らない。
    let harness = harness_with_track_effect();
    let requested = "存在しない移動";
    let error = harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            movement(&[0.0, 50.0, 100.0], requested),
        ))
        .expect_err("存在しない移動方法が受理されました");

    let details = error.details();
    assert!(
        !details.to_string().contains(requested),
        "拒否された要求の名前が応答に現れました: {details}"
    );
    // 一覧そのものは運ぶ。運ばなければ要求元は選び直す材料を持たない。
    assert_eq!(
        details["known_movements"]
            .as_array()
            .expect("配列です")
            .len(),
        TRACK_MODES.len()
    );
}

#[test]
fn what_the_list_calls_unwritable_is_what_a_raw_alias_refuses() {
    // **移動を書く経路は 2 本あり、どちらも同じ 1 つの表を読む。** 片方にだけ
    // 条件を足せば、生テキストで作れるオブジェクトと設定項目として書ける値が
    // 食い違う。一覧を渡し損ねた実装も、可否を落とした一覧を渡す実装も、
    // ここで落ちる。
    let movements = vec![
        Movement {
            name: "直線移動".to_string(),
            writable: true,
        },
        Movement {
            name: "移動無し".to_string(),
            writable: false,
        },
    ];
    for movement_entry in &movements {
        let harness = Harness::new();
        harness.host.set_movements(movements.clone());
        let alias = format!(
            "[Object]\r\nframe=0,80\r\n[Object.0]\r\neffect.name=標準描画\r\nX=0.00,100.00,{},0\r\n",
            movement_entry.name
        );
        let result = harness
            .edit
            .create_object(&create_from_raw_alias_params(&harness, &alias));
        if movement_entry.writable {
            result.unwrap_or_else(|error| {
                panic!("{} が拒否されました: {error}", movement_entry.name)
            });
        } else {
            let error = result.expect_err("書けない移動方法が受理されました");
            assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
            assert_eq!(
                error.details()["reason"],
                json!("track_mode_not_writable"),
                "{} の拒否の理由",
                movement_entry.name
            );
            // 名前を選び直す手は通らない。一覧に無い名前とは別の失敗である。
            assert_ne!(error.details()["reason"], json!("track_mode_unknown"));
            harness.assert_untouched();
        }
    }
}

#[test]
fn what_the_list_calls_unwritable_is_what_set_object_item_refuses() {
    // **一覧と拒否が同じ表を読む。** 一覧が返した 1 件ずつについて、書けないと
    // 名乗ったものは書き込みが拒み、書けると名乗ったものは名前を理由に拒まれ
    // ない。名前を書き並べた検査は、一覧が変わったときにこの規律を守らない。
    let harness = harness_with_track_effect();
    let movements = vec![
        Movement {
            name: "直線移動".to_string(),
            writable: true,
        },
        Movement {
            name: "移動無し".to_string(),
            writable: false,
        },
    ];
    harness.host.set_movements(movements.clone());

    for movement_entry in &movements {
        let result = harness.edit.set_object_item(&set_movement(
            &harness,
            movement(&[0.0, 50.0, 100.0], &movement_entry.name),
        ));
        if movement_entry.writable {
            result.unwrap_or_else(|error| {
                panic!("{} が拒否されました: {error}", movement_entry.name)
            });
        } else {
            let error = result.expect_err("書けない移動方法が受理されました");
            assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
            assert_eq!(
                error.details()["reason"],
                json!("track_mode_not_writable"),
                "{} の拒否の理由",
                movement_entry.name
            );
            // 名前を選び直す手は通らない。一覧に無い名前とは別の失敗である。
            assert_ne!(error.details()["reason"], json!("track_mode_unknown"));
        }
    }

    // 拒否は書き込みを発行する手前で起きる。記録が空でなければ、検証を通り
    // 抜けた入力がホストへ届いている。
    assert_eq!(
        harness.host.fatal_movement_writes(),
        Vec::<String>::new(),
        "書けない移動方法がホストへ届きました"
    );
}

#[test]
fn a_movement_that_reaches_the_host_with_an_unknown_mode_is_recorded() {
    // **記録に入る経路があることを確かめる。** 空であることしか見ない検査は、
    // 記録そのものが壊れていても緑のまま通り、検証を外した変更を捕まえられない。
    // ホストが本当に知っている名前と、検証へ渡す一覧は別の出所を持つ。食い違わ
    // せれば、検証を通り抜けた書き込みがホストへ届く。
    let harness = harness_with_track_effect();
    let unknown = "存在しない移動";
    assert!(
        !TRACK_MODES.contains(&unknown),
        "ホストが知っている名前を未知の名前として使っています"
    );
    harness.host.set_movements(vec![Movement {
        name: unknown.to_string(),
        writable: true,
    }]);

    let error = with_silent_panic_hook(|| {
        harness
            .edit
            .set_object_item(&set_movement(
                &harness,
                movement(&[0.0, 50.0, 100.0], unknown),
            ))
            .expect_err("実機ならプロセスが落ちる書き込みが成功として返りました")
    });

    // 実機は落ちる。フェイクの panic は編集の入口が捕捉するため、応答からは
    // 内部の失敗としか見えない。
    assert_eq!(error.error_code(), ErrorCode::InternalError);
    assert_eq!(
        harness.host.fatal_movement_writes(),
        vec![unknown.to_string()],
        "落ちる移動方法がホストへ届いたのに記録されていません"
    );
}

#[test]
fn a_movement_whose_value_count_does_not_match_the_sections_is_refused() {
    // ホストは個数の不一致を拒否せず、余った値を評価せずに保存する。要求した
    // 区間の値が入らないことに気付く手段が要求元に無い。
    // 標本の対象は中間点を 1 つ持つ。区間 2 個に対して値は 3 個である。
    let harness = harness_with_track_effect();
    harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            movement(&[0.0, 50.0, 100.0], "直線移動"),
        ))
        .expect("区間の数と合う値が拒否されました");

    let error = harness
        .edit
        .set_object_item(&set_movement(&harness, movement(&[0.0, 100.0], "直線移動")))
        .expect_err("区間の数と合わない値が受理されました");

    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    assert_eq!(error.details()["reason"], json!("track_value_count"));
    assert!(harness.host.fatal_movement_writes().is_empty());
}

#[test]
fn no_movement_can_be_written_when_the_list_is_unavailable() {
    // 一覧を引けない環境では移動を 1 つも書けない。検証できないまま通すと、
    // その場でホストのプロセスが落ちる。
    let harness = harness_with_track_effect();
    harness.host.set_movements(Vec::new());
    let error = harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            movement(&[0.0, 50.0, 100.0], "直線移動"),
        ))
        .expect_err("一覧が空でも移動が受理されました");

    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    assert_eq!(error.details()["reason"], json!("track_mode_unknown"));
    assert!(harness.host.fatal_movement_writes().is_empty());

    // 一覧を要さない書き込みは影響を受けない。
    harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 30 },
        })
        .expect("移動を含まない書き込みまで拒否されました");
}
