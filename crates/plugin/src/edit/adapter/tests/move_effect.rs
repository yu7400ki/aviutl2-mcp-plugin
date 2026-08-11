//! effect の順序移動の統合テスト。

use super::*;

/// 同名 effect を 3 つ並べた対象を持つフェイクを組む。
///
/// 3 件は名前・有効・ロックが等しく、設定項目の値だけが違う。列の先頭には
/// 別名の effect が居るため、列全体での位置と同名内の順序が 1 つずれる。
///
/// **3 件並べるのは、移動先を列の末尾でも現在位置でもない位置に採るためである。**
/// 末尾を指すと「末尾へ動かす」ホストと、現在位置を指すと「動かさない」ホストと、
/// 正しい移動が区別できない。
fn harness_with_effect_column() -> Harness {
    Harness::with(|host| {
        host.scene.get_mut().unwrap().layers[1].objects[0].effects =
            vec![video_effect(), blur(0, 10), blur(1, 20), blur(2, 30)];
    })
}

/// 列の先頭に置く別名の effect。
fn video_effect() -> HostEffect {
    HostEffect {
        name: "動画ファイル".to_string(),
        index: 0,
        enabled: true,
        locked: false,
        items: Vec::new(),
    }
}

/// 対象の effect を、名前と設定項目の値の組として読み出す。
fn effect_column(harness: &Harness) -> Vec<(String, ItemValue)> {
    let selector = harness.selector(1, 100);
    harness
        .healthy(|| harness.read.get_object(&selector))
        .expect("対象の詳細")
        .effects
        .into_iter()
        .map(|effect| {
            let value = effect
                .items
                .first()
                .map(|item| item.value.clone())
                .unwrap_or(ItemValue::Unknown { raw: String::new() });
            (effect.name, value)
        })
        .collect()
}

/// 設定項目 `範囲` の値を持つ組を作る。[`effect_column`] の期待値に使う。
fn blur_entry(range: i64) -> (String, ItemValue) {
    ("ぼかし".to_string(), ItemValue::Integer { value: range })
}

/// [`video_effect`] を [`effect_column`] の期待値として書く形。
fn video_entry() -> (String, ItemValue) {
    (
        "動画ファイル".to_string(),
        ItemValue::Unknown { raw: String::new() },
    )
}

/// 同名 effect のうち `effect_index` 番目を移動する要求を組み立てる。
fn move_blur(harness: &Harness, effect_index: usize, position: usize) -> MoveEffectParams {
    MoveEffectParams {
        selector: harness.effect_selector(1, 100, "ぼかし", effect_index),
        position,
    }
}

#[test]
fn moving_an_effect_backwards_puts_the_column_in_that_order() {
    // 列は [動画ファイル, ぼかし10, ぼかし20, ぼかし30]。位置 1 の 1 件を
    // 位置 2 へ動かす。**移動先は末尾ではない**——末尾を指すと、末尾へ動かす
    // ホストと正しい移動が区別できない。
    let harness = harness_with_effect_column();
    let outcome = harness
        .edit
        .move_effect(&move_blur(&harness, 0, 2))
        .expect("後方への移動に失敗しました");

    assert_eq!(
        effect_column(&harness),
        vec![
            video_entry(),
            blur_entry(20),
            blur_entry(10),
            blur_entry(30)
        ]
    );
    assert_eq!(
        changed_item(&outcome, "範囲"),
        ItemValue::Integer { value: 10 }
    );
    let effect = outcome.effect.expect("移動後の effect");
    assert_eq!(effect.position, 2);
    assert_eq!(effect.name, "ぼかし");
}

#[test]
fn moving_an_effect_forwards_puts_the_column_in_that_order() {
    // 抜いてから挿す順序を取り違えると、後方への移動だけが 1 つずれる。前方を
    // 試さなければ、その取り違えは片側でしか現れない。
    let harness = harness_with_effect_column();
    let outcome = harness
        .edit
        .move_effect(&move_blur(&harness, 2, 1))
        .expect("前方への移動に失敗しました");

    assert_eq!(
        effect_column(&harness),
        vec![
            video_entry(),
            blur_entry(30),
            blur_entry(10),
            blur_entry(20)
        ]
    );
    assert_eq!(
        changed_item(&outcome, "範囲"),
        ItemValue::Integer { value: 30 }
    );
    let effect = outcome.effect.expect("移動後の effect");
    assert_eq!(effect.position, 1);
}

#[test]
fn moving_one_of_two_effects_with_the_same_name_swaps_their_effect_index() {
    // 列全体の位置と同名内の順序は別の数である。移動先に前者を渡すべきところへ
    // 後者を渡すと、この列では移動先が 2 ではなく 0 になる。
    let harness = harness_with_effect_column();
    let outcome = harness
        .edit
        .move_effect(&move_blur(&harness, 0, 2))
        .expect("同名 effect の移動に失敗しました");

    let effect = outcome.effect.expect("移動後の effect");
    assert_eq!(effect.position, 2);
    assert_eq!(effect.index, 1, "同名内の順序が入れ替わっていません");
    assert_eq!(effect.selector.effect_index, 1);

    // 追い越された側は前へ出て、同名内の順序を受け取る。
    let selector = harness.selector(1, 100);
    let detail = harness
        .healthy(|| harness.read.get_object(&selector))
        .expect("対象の詳細");
    let overtaken = detail
        .effects
        .iter()
        .find(|effect| effect.position == 1)
        .expect("位置 1 の effect");
    assert_eq!(overtaken.index, 0);
    assert_eq!(
        overtaken.items[0].value,
        ItemValue::Integer { value: 20 },
        "追い越された側が入れ替わっていません"
    );
}

#[test]
fn a_different_effect_at_the_destination_is_not_taken_for_the_moved_one() {
    // 移動先へ来たのは名前も有効もロックも等しく、設定項目の値だけが違う別の
    // 1 件である。名前だけを比べる照合は、これを移動の成功として通す。
    let harness = Harness::with(|host| {
        host.arm(|knobs| knobs.fault = Some(Fault::AppendMovedEffect));
        host.scene.get_mut().unwrap().layers[1].objects[0].effects =
            vec![video_effect(), blur(0, 10), blur(1, 20), blur(2, 30)];
    });
    let error = harness
        .edit
        .move_effect(&move_blur(&harness, 0, 2))
        .expect_err("別の effect が移動先に居る状態が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("change_not_applied"));
    // 動いてしまった列は移動前の並びへ戻る。
    assert_eq!(
        effect_column(&harness),
        vec![
            video_entry(),
            blur_entry(10),
            blur_entry(20),
            blur_entry(30)
        ]
    );
}

#[test]
fn an_effect_whose_type_is_not_a_filter_is_refused_before_the_section() {
    // 種別はカタログが述べている。入力 item を動かせないと分かるのは呼ぶ前で
    // あり、呼んでしまえば届いた以上は変更が入った側へ倒すほかない。
    let harness = Harness::new();
    let error = harness
        .edit
        .move_effect(&MoveEffectParams {
            selector: harness.effect_selector(1, 100, "動画ファイル", 0),
            position: 1,
        })
        .expect_err("フィルタでない effect を動かせました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("effect_not_movable"));
    assert_eq!(harness.host.enter_calls(), 0, "編集区間へ入りました");
    harness.assert_untouched();
}

#[test]
fn an_effect_the_catalog_does_not_describe_is_let_through() {
    // カタログに無い effect と、種別が未知の effect は事前確認を通す。我々が
    // モデル化していない種別を我々の都合で拒むと、上流が種別を足すたびに
    // 動かせない対象が増える。
    let unlisted = |name: &str| HostEffect {
        name: name.to_string(),
        index: 0,
        enabled: true,
        locked: false,
        items: Vec::new(),
    };
    for name in ["カタログに無い効果", "未知種別の効果"] {
        let harness = Harness::with(|host| {
            host.catalog.push(FakeCatalogEntry {
                name: "未知種別の効果".to_string(),
                effect_type: EffectType::Unknown(99),
                flags: EffectFlags::from_raw(1),
                items: Vec::new(),
                facets: HashMap::new(),
            });
            host.scene.get_mut().unwrap().layers[1].objects[0].effects =
                vec![blur(0, 10), unlisted(name)];
        });
        let outcome = harness
            .edit
            .move_effect(&MoveEffectParams {
                selector: harness.effect_selector(1, 100, name, 0),
                position: 0,
            })
            .unwrap_or_else(|error| panic!("{name} の移動が拒否されました: {error}"));

        let effect = outcome.effect.expect("移動後の effect");
        assert_eq!(effect.name, name);
        assert_eq!(effect.position, 0);
    }
}

#[test]
fn a_destination_past_the_end_of_the_column_is_refused_before_the_write() {
    // 列の長さは対象を解決した時点で手元に在る。ホストが切り詰めるかどうかを
    // 問わずに落とす。
    let harness = harness_with_effect_column();
    let error = harness
        .edit
        .move_effect(&move_blur(&harness, 0, 4))
        .expect_err("列の長さちょうどの移動先が受理されました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(
        error.details()["reason"],
        json!("effect_position_out_of_range")
    );
    assert_eq!(error.details()["retry_requires"], json!("refetch"));
    harness.assert_untouched();
}

#[test]
fn a_column_that_did_not_move_keeps_the_selector_it_was_asked_with() {
    // 発行の後に「ホストが拒んだ」と「別の位置へ倒した」を我々の側から区別
    // できない。名乗る名前は 1 つであり、列が動いたかどうかは巻き戻しの結末が
    // 運ぶ。
    //
    // 移動先は現在位置（1）でも末尾（3）でもない 2 を指す。現在位置を指せば
    // 無視するホストが、末尾を指せば末尾へ動かすホストが、それぞれ正しい移動と
    // 区別できなくなる。
    let harness = harness_with_faulty_move(Fault::IgnoreEffectMove);
    let request = move_blur(&harness, 0, 2);
    let error = harness
        .edit
        .move_effect(&request)
        .expect_err("動かなかった移動が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("change_not_applied"));
    // 発行の後に落ちた失敗である。
    assert_eq!(error.details()["mutation_issued"], json!(true));
    // 列は書き込み前の並びを持つ。要求元から見た状態は戻した場合と区別がつかず、
    // 読み直した先には要求の前と同じ列が在る。
    assert_eq!(error.details()["restored"], json!(true));
    assert_eq!(error.details()["retry_requires"], json!("none"));
    // fingerprint の材料が 1 つも変わっていないため、同じ selector がそのまま
    // 通る。前提条件の食い違いにはならない。
    let again = harness
        .edit
        .move_effect(&request)
        .expect_err("動かなかった移動が成功として返りました");
    assert_eq!(again.details()["reason"], json!("change_not_applied"));
}

#[test]
fn a_column_that_changed_length_is_not_a_move() {
    // 移動は effect を増やしも減らしもしない。**動かした 1 件は要求どおりの
    // 位置に居り、消えたのは別の 1 件である**——移動先を見ても元の位置を見ても
    // 食い違いは現れず、長さだけが変化を示す。
    let harness = Harness::with(|host| {
        host.arm(|knobs| knobs.fault = Some(Fault::DropAnotherEffect));
        host.scene.get_mut().unwrap().layers[1].objects[0].effects =
            vec![video_effect(), blur(0, 10), blur(1, 20), blur(2, 30)];
    });
    let error = harness
        .edit
        .move_effect(&move_blur(&harness, 0, 2))
        .expect_err("列が短くなった移動が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("change_not_applied"));
    // 1 度の移動では移動前の並びへ戻せない。戻せていないことを名乗る。
    assert_eq!(error.details()["restored"], json!(false));
    assert_eq!(error.details()["consistency_unknown"], json!(true));
    // 移動そのものは要求どおりに入っている。列の末尾の 1 件だけが消えた。
    assert_eq!(
        effect_column(&harness),
        vec![video_entry(), blur_entry(20), blur_entry(10)]
    );
}

#[test]
fn the_position_the_host_reports_does_not_decide_the_outcome() {
    // 戻り値が何を数えているかを SDK は述べていない。列全体の位置なのか、
    // フィルタ効果だけを数えた位置なのかが書かれておらず、後者なら正しく動いた
    // 移動を我々が失敗と判定する。可否は読み直した列だけが決める。
    let harness = Harness::with(|host| {
        host.arm(|knobs| knobs.fault = Some(Fault::MisreportEffectPosition));
        host.scene.get_mut().unwrap().layers[1].objects[0].effects =
            vec![video_effect(), blur(0, 10), blur(1, 20), blur(2, 30)];
    });
    let outcome = harness
        .edit
        .move_effect(&move_blur(&harness, 0, 2))
        .expect("戻り値だけが違う移動が失敗として返りました");

    assert_eq!(outcome.effect.expect("移動後の effect").position, 2);
    assert_eq!(
        effect_column(&harness),
        vec![
            video_entry(),
            blur_entry(20),
            blur_entry(10),
            blur_entry(30)
        ]
    );
}

#[test]
fn a_failed_move_reports_the_position_the_host_named() {
    // 照合が食い違ったとき、どちらの解釈だったかを応答から読めるようにする。
    let harness = Harness::with(|host| {
        host.arm(|knobs| knobs.fault = Some(Fault::AppendMovedEffect));
        host.scene.get_mut().unwrap().layers[1].objects[0].effects =
            vec![video_effect(), blur(0, 10), blur(1, 20), blur(2, 30)];
    });
    let error = harness
        .edit
        .move_effect(&move_blur(&harness, 0, 2))
        .expect_err("別の位置へ動いた移動が成功として返りました");

    // 要求した位置は 2、ホストが動かした先は末尾の 3 である。
    assert_eq!(error.details()["reported_position"], json!(3));
}

#[test]
fn moving_an_effect_to_where_it_already_is_still_reaches_the_host() {
    // 短絡すると、ホストが同意していない成功が 1 つできる。他の編集と性質が
    // 変わるため、同じ位置でも発行する。
    let harness = harness_with_effect_column();
    let before = effect_column(&harness);
    let outcome = harness
        .edit
        .move_effect(&move_blur(&harness, 0, 1))
        .expect("現在位置への移動が拒否されました");

    assert!(
        harness.host.calls().contains(&"move_effect"),
        "移動 API を呼んでいません: {:?}",
        harness.host.calls()
    );
    assert_eq!(effect_column(&harness), before, "列が動きました");
    assert_eq!(outcome.effect.expect("移動後の effect").position, 1);
}

/// [`harness_with_effect_column`] と同じ列を、失敗を仕込んだホストで用意する。
fn harness_with_faulty_move(fault: Fault) -> Harness {
    Harness::with(|host| {
        host.arm(|knobs| knobs.fault = Some(fault));
        host.scene.get_mut().unwrap().layers[1].objects[0].effects =
            vec![video_effect(), blur(0, 10), blur(1, 20), blur(2, 30)];
    })
}

/// 移動前の列。巻き戻しの照合に使う。
fn column_before_the_move() -> Vec<(String, ItemValue)> {
    vec![
        video_entry(),
        blur_entry(10),
        blur_entry(20),
        blur_entry(30),
    ]
}

/// 変更 API のうち、effect の順序を動かした回数。
fn move_calls(harness: &Harness) -> usize {
    harness
        .host
        .calls()
        .iter()
        .filter(|call| **call == "move_effect")
        .count()
}

#[test]
fn a_move_that_landed_elsewhere_puts_the_column_back() {
    // 失敗が状態を変える経路を残さない。列が動いたままだと、要求元が要求に
    // 使った selector も一緒に無効になる。
    let harness = harness_with_faulty_move(Fault::AppendMovedEffect);
    let error = harness
        .edit
        .move_effect(&move_blur(&harness, 0, 2))
        .expect_err("別の位置へ動いた移動が成功として返りました");

    assert_eq!(error.details()["reason"], json!("change_not_applied"));
    assert_eq!(error.details()["restored"], json!(true));
    assert!(
        error.details().get("consistency_unknown").is_none(),
        "戻せているのに中途半端な状態を名乗りました: {}",
        error.details()
    );
    // 動いた 1 件だけでなく、間に在った effect の並びまで戻っている。
    assert_eq!(effect_column(&harness), column_before_the_move());
}

#[test]
fn a_move_the_host_ignored_reports_the_column_restored_without_issuing_a_restore() {
    // ホストが動かさなかった列に戻すものは無い。戻す移動を発行すると、要らない
    // 書き込みが 1 つプロジェクトへ届く。それでも列は書き込み前の並びを持つ。
    let harness = harness_with_faulty_move(Fault::IgnoreEffectMove);
    let error = harness
        .edit
        .move_effect(&move_blur(&harness, 0, 2))
        .expect_err("動かなかった移動が成功として返りました");

    assert_eq!(error.details()["reason"], json!("change_not_applied"));
    // 戻す移動を発行していないことと、列が書き込み前の並びを持つことは両立する。
    assert_eq!(error.details()["restored"], json!(true));
    assert_eq!(move_calls(&harness), 1, "戻す移動が発行されました");
    assert_eq!(effect_column(&harness), column_before_the_move());
}

#[test]
fn a_restore_that_does_not_take_names_the_state_unknown() {
    // ホストは移動の成否を返さない。戻す移動が通ったことは、列を読み直して
    // 移動前の並びと比べるまで確かめられない。
    let harness = harness_with_faulty_move(Fault::IgnoreEffectMoveRestore);
    let error = harness
        .edit
        .move_effect(&move_blur(&harness, 0, 2))
        .expect_err("別の位置へ動いた移動が成功として返りました");

    assert_eq!(error.details()["reason"], json!("change_not_applied"));
    assert_eq!(error.details()["restored"], json!(false));
    assert_eq!(error.details()["consistency_unknown"], json!(true));
    // 戻す移動そのものは発行されている。発行していなければ、戻せなかったことは
    // 何も言っていない。
    assert_eq!(move_calls(&harness), 2, "戻す移動が発行されていません");
    assert_eq!(
        effect_column(&harness),
        vec![
            video_entry(),
            blur_entry(20),
            blur_entry(30),
            blur_entry(10)
        ],
        "列が動いたままであることを名乗れていません"
    );
}

#[test]
fn a_restored_move_advances_the_revision_at_most_once() {
    // 巻き戻しは同じ許可で発行する。許可は最初の発行で確定した revision を
    // 保つため、移動が 2 回でも revision は 1 つしか進まない。
    let harness = harness_with_faulty_move(Fault::AppendMovedEffect);
    let error = harness
        .edit
        .move_effect(&move_blur(&harness, 0, 2))
        .expect_err("別の位置へ動いた移動が成功として返りました");

    assert_eq!(move_calls(&harness), 2, "戻す移動が発行されていません");
    assert_eq!(harness.project.revision(), 1, "revision が 2 つ進みました");
    assert_eq!(error.details()["current_project_revision"], json!(1));
}

/// 名前・有効・ロック・設定項目の値がすべて等しい effect を並べた列を作る。
fn harness_with_twin_effects(effects: Vec<HostEffect>) -> Harness {
    Harness::with(|host| {
        host.arm(|knobs| knobs.fault = Some(Fault::AppendMovedEffect));
        host.scene.get_mut().unwrap().layers[1].objects[0].effects = effects;
    })
}

#[test]
fn a_twin_sliding_into_the_old_position_is_not_read_as_a_column_that_did_not_move() {
    // 列は [動画ファイル, ぼかし10, ぼかし10, ぼかし10 の双子, ぼかし30] ではなく
    // 双子 2 件である。先頭のぼかしを動かすと、もう 1 件が移動前の位置へずれ
    // 込む。同一性の材料は双子を区別しないため、その 1 件だけを見る判定は
    // 「ホストは何も動かしていない」を真にする——列は現に動いている。
    let harness =
        harness_with_twin_effects(vec![video_effect(), blur(0, 10), blur(1, 10), blur(2, 30)]);
    let error = harness
        .edit
        .move_effect(&move_blur(&harness, 0, 2))
        .expect_err("別の位置へ動いた移動が成功として返りました");

    assert_eq!(error.details()["reason"], json!("change_not_applied"));
    assert_eq!(error.details()["restored"], json!(true));
    assert_eq!(
        effect_column(&harness),
        vec![
            video_entry(),
            blur_entry(10),
            blur_entry(10),
            blur_entry(30)
        ]
    );
}

#[test]
fn the_restore_moves_the_effect_that_puts_the_column_back() {
    // 列は [動画ファイル, ぼかし10, ぼかし20, ぼかし10, ぼかし30]。先頭の
    // ぼかし10 を 3 へ動かすと、ホストは末尾へ倒す。読み直した列を先頭から
    // 探すと、動いていない方のぼかし10 を掴む——戻せたはずの列が戻らない。
    let harness = harness_with_twin_effects(vec![
        video_effect(),
        blur(0, 10),
        blur(1, 20),
        blur(2, 10),
        blur(3, 30),
    ]);
    let error = harness
        .edit
        .move_effect(&move_blur(&harness, 0, 3))
        .expect_err("別の位置へ動いた移動が成功として返りました");

    assert_eq!(error.details()["reason"], json!("change_not_applied"));
    assert_eq!(error.details()["restored"], json!(true));
    assert!(
        error.details().get("consistency_unknown").is_none(),
        "戻せているのに中途半端な状態を名乗りました: {}",
        error.details()
    );
    assert_eq!(
        effect_column(&harness),
        vec![
            video_entry(),
            blur_entry(10),
            blur_entry(20),
            blur_entry(10),
            blur_entry(30)
        ]
    );
}
