//! 一括適用の統合テスト。
//!
//! 相の分離と巻き戻しの順序は型では強制できない。フェイクが記録した呼び出しの
//! 順序と、フェイクが保持する状態そのものを検証対象にする。

use super::*;
use crate::edit::fake::blur;
use aviutl2_mcp_core::{ApplyBatchParams, BatchOperation, EffectItem};

/// 移動の sub-operation を組み立てる。
fn move_op(selector: ObjectSelector, layer: u32, frame: u32) -> BatchOperation {
    BatchOperation::MoveObject {
        selector,
        destination: Destination { layer, frame },
    }
}

/// 設定値の sub-operation を組み立てる。
fn set_item_op(selector: EffectSelector, item: &str, value: i64) -> BatchOperation {
    BatchOperation::SetObjectItem {
        selector,
        item: item.to_string(),
        value: ItemValue::Integer { value },
    }
}

/// 一括適用の要求を組み立てる。
fn batch(operations: Vec<BatchOperation>) -> ApplyBatchParams {
    ApplyBatchParams { operations }
}

/// 識別子で対象の現在の位置を引く。
fn placement_of(harness: &Harness, id: usize) -> (usize, usize) {
    let scene = harness.host.scene();
    let object = scene
        .layers
        .iter()
        .flat_map(|layer| layer.objects.iter())
        .find(|object| object.id == id)
        .unwrap_or_else(|| panic!("識別子 {id} の対象がありません"));
    (object.placement.layer, object.placement.frame_start)
}

/// 識別子と effect の位置で設定項目の値を引く。
fn item_of(harness: &Harness, id: usize, effect: usize, item: &str) -> ItemValue {
    let scene = harness.host.scene();
    let object = scene
        .layers
        .iter()
        .flat_map(|layer| layer.objects.iter())
        .find(|object| object.id == id)
        .unwrap_or_else(|| panic!("識別子 {id} の対象がありません"));
    object.effects[effect]
        .items
        .iter()
        .find(|entry| entry.name == item)
        .unwrap_or_else(|| panic!("設定項目 {item} がありません"))
        .value
        .clone()
}

// -------------------------------------------------------------- 相の分離

#[test]
fn every_target_is_resolved_before_the_first_change_is_issued() {
    // 対象の捕捉は解決処理だけが行う。変更の後に現れれば、適用相か巻き戻し相が
    // 解決し直していることになる。逆操作の材料も許可より前に揃っている必要が
    // ある。**変更より後の値の読み取りは書き込み後の照合であり、材料の読み取り
    // ではない。** 両者は同じ SDK 呼び出しを使うため、位置と件数で切り分ける。
    let harness = Harness::new();
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        set_item_op(harness.effect_selector(1, 300, "ぼかし", 0), "範囲", 40),
    ]);
    harness.host.clear_calls();
    harness.edit.apply_batch(&params).expect("一括適用の失敗");

    let calls = harness.host.calls();
    let first = first_mutation(&calls).expect("変更 API が呼ばれていません");
    for (position, call) in calls.iter().enumerate() {
        if position <= first {
            continue;
        }
        assert!(
            !matches!(*call, "bind_object" | "bind_effect"),
            "変更を発行した後に対象を解決し直しました: {calls:?}"
        );
    }
    assert_eq!(
        count(&calls[..first], ITEM_VALUE),
        1,
        "逆操作の材料が変更より前に読まれていません: {calls:?}"
    );
    assert_eq!(
        count(&calls[first..], ITEM_VALUE),
        1,
        "変更より後の読み取りが照合の 1 回を超えています: {calls:?}"
    );
}

#[test]
fn a_failure_in_the_planning_phase_issues_no_change_at_all() {
    let harness = Harness::new();
    let mut stale = harness.selector(1, 100);
    stale.fingerprint = tamper(&stale.fingerprint);
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        move_op(stale, 1, 700),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("食い違った対象を含む一括適用が成功しました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    harness.assert_untouched();
}

// ------------------------------------------------------ 事前解決相の各段

#[test]
fn a_stale_epoch_names_the_sub_operation_that_carries_it() {
    let harness = Harness::new();
    let mut stale = harness.selector(1, 100);
    stale.project_epoch = "別のプロジェクト".to_string();
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        move_op(stale, 1, 700),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("別プロジェクトのセレクターが受理されました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));
    assert_eq!(error.details()["failed_index"], json!(1));
    harness.assert_untouched();
}

#[test]
fn a_stale_scene_names_the_sub_operation_that_carries_it() {
    let harness = Harness::new();
    let mut stale = harness.selector(1, 100);
    stale.scene_id = 9;
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        move_op(stale, 1, 700),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("別シーンのセレクターが受理されました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["mismatch"], json!("scene_id"));
    assert_eq!(error.details()["failed_index"], json!(1));
    harness.assert_untouched();
}

#[test]
fn a_target_that_cannot_be_resolved_names_its_position() {
    let harness = Harness::new();
    let mut missing = harness.selector(1, 100);
    missing.frame = 999;
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        move_op(missing, 1, 700),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("存在しない対象が受理されました");

    assert_eq!(error.error_code(), ErrorCode::NotFound);
    assert_eq!(error.details()["failed_index"], json!(1));
    // 解決できなかった対象は載せる姿を持たない。
    assert!(error.details().get("failed_object").is_none());
    harness.assert_untouched();
}

#[test]
fn an_object_mismatch_names_the_position_and_the_current_object() {
    // 要求元は落ちた 1 件だけを差し替えて送り直せる。全件を読み直す必要は無い。
    let harness = Harness::new();
    let mut stale = harness.selector(1, 100);
    stale.fingerprint = tamper(&stale.fingerprint);
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        move_op(stale, 1, 700),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("食い違った対象が受理されました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["mismatch"], json!("fingerprint"));
    assert_eq!(error.details()["failed_index"], json!(1));
    let failed = &error.details()["failed_object"];
    assert_eq!(failed["layer"], json!(1));
    assert_eq!(failed["frame_start"], json!(100));

    // 返された姿でその 1 件だけを差し替えれば通る。
    let repaired: ObjectSelector =
        serde_json::from_value(failed["selector"].clone()).expect("セレクターの書式");
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        move_op(repaired, 1, 700),
    ]);
    harness
        .edit
        .apply_batch(&params)
        .expect("差し替えた要求が拒否されました");
}

#[test]
fn an_effect_mismatch_names_only_the_position() {
    // effect の食い違いで返せる対象の姿は、要求元が既に持っている値と同じに
    // なる。差し替えの材料にならないものを載せると、要求元は同じ失敗を繰り返す。
    let harness = Harness::new();
    let mut stale = harness.effect_selector(1, 100, "ぼかし", 0);
    stale.fingerprint = tamper(&stale.fingerprint);
    let params = batch(vec![
        set_item_op(harness.effect_selector(1, 300, "ぼかし", 0), "範囲", 40),
        set_item_op(stale, "範囲", 50),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("食い違った effect が受理されました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["mismatch"], json!("fingerprint"));
    assert_eq!(error.details()["failed_index"], json!(1));
    assert!(
        error.details().get("failed_object").is_none(),
        "effect の食い違いで対象の姿が載りました"
    );
    harness.assert_untouched();
}

#[test]
fn a_locked_layer_stops_a_move_but_not_a_value_change() {
    // 一括適用が運ぶ 2 種のうち、レイヤーのロックが止めるのは時間軸上の移動で
    // ある。設定値の変更は設定パネルから行えるため止めない。
    let harness = Harness::new();
    let effect = harness.effect_selector(1, 100, "ぼかし", 0);
    let selector = harness.selector(1, 100);
    harness.host.lock_layer(1, true);

    let error = harness
        .edit
        .apply_batch(&batch(vec![move_op(selector, 1, 500)]))
        .expect_err("ロックされたレイヤーの対象が移動できました");
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["reason"], json!("layer_locked"));
    assert_eq!(error.details()["failed_index"], json!(0));

    // 拒否された移動は何も変えていないため、同じ読み取りから得た effect の
    // 指定はそのまま通る。
    harness
        .edit
        .apply_batch(&batch(vec![set_item_op(effect, "範囲", 40)]))
        .expect("ロックされたレイヤー上の設定値変更が拒否されました");
}

#[test]
fn an_unknown_item_name_names_its_position() {
    let harness = Harness::new();
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        set_item_op(
            harness.effect_selector(1, 100, "ぼかし", 0),
            "存在しない項目",
            40,
        ),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("存在しない設定項目が受理されました");

    assert_eq!(error.error_code(), ErrorCode::NotFound);
    assert_eq!(error.details()["item"], json!("存在しない項目"));
    assert_eq!(error.details()["failed_index"], json!(1));
    harness.assert_untouched();
}

#[test]
fn an_item_whose_type_is_not_writable_names_its_position() {
    // 設定項目の列挙は未知種別の項目を落とす。落ちた項目への書き込みを「項目が
    // 見つからない」として返すと、要求元は存在しない問題を指す失敗を受け取る。
    // 判定は単独編集と同じ実装を呼ぶが、事前解決相の段として個別に固定する。
    let harness = harness_with_unlisted_item();
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        set_item_op(
            harness.effect_selector(1, 100, "ぼかし", 0),
            "未知種別の項目",
            40,
        ),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("書き込みを公開しない種別の項目が受理されました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("item_type_not_writable"));
    assert_eq!(error.details()["failed_index"], json!(1));
    harness.assert_untouched();
}

#[test]
fn a_value_of_the_wrong_shape_names_its_position() {
    let harness = Harness::new();
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        BatchOperation::SetObjectItem {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            item: "範囲".to_string(),
            value: ItemValue::Text {
                value: "広め".to_string(),
            },
        },
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("種別の合わない値が受理されました");

    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    assert_eq!(error.details()["failed_index"], json!(1));
    harness.assert_untouched();
}

#[test]
fn an_unreadable_original_value_is_refused_before_anything_changes() {
    // 逆操作を組み立てられない変更は発行しない。実行してから組み立てられないと
    // 分かる経路を作らない。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::ItemValueUnreadable)));
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        set_item_op(harness.effect_selector(1, 100, "ぼかし", 0), "範囲", 40),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("逆操作を組み立てられない要求が受理されました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("inverse_unavailable"));
    assert_eq!(error.details()["failed_index"], json!(1));
    harness.assert_untouched();
}

#[test]
fn two_selectors_that_resolve_to_the_same_object_are_refused() {
    // 名前を指定したセレクターと指定しないセレクターは文字列としては別物で
    // ある。要求内容だけの検証はここを取りこぼすため、解決してからもう一度見る。
    let harness = Harness::new();
    let named = harness.selector(1, 100);
    let mut unnamed = named.clone();
    unnamed.name = None;
    assert_ne!(named.name, unnamed.name, "同じ文字列のセレクターです");

    let params = batch(vec![move_op(named, 1, 500), move_op(unnamed, 1, 700)]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("同じ対象を 2 度移動する要求が受理されました");

    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    assert_eq!(error.details()["reason"], json!("duplicate_target"));
    assert_eq!(error.details()["failed_index"], json!(1));
    harness.assert_untouched();
}

// ------------------------------------------------------------ キャッシュ

#[test]
fn two_sub_operations_on_one_object_read_its_detail_once() {
    // 同一区間内の 2 回の読み取りが違う値を返せば、同じ対象を指す 2 つの
    // sub-operation の一方だけが前提条件の不一致になる。要求元は何が起きたか
    // 理解できない。
    let harness = Harness::with(|host| {
        host.scene.get_mut().unwrap().layers[1].objects[0]
            .effects
            .push(blur(1, 30));
    });
    let params = batch(vec![
        set_item_op(harness.effect_selector(1, 100, "ぼかし", 0), "範囲", 40),
        set_item_op(harness.effect_selector(1, 100, "ぼかし", 1), "範囲", 50),
    ]);
    harness.host.clear_calls();
    harness.edit.apply_batch(&params).expect("一括適用の失敗");

    let calls = harness.host.calls();
    let first = first_mutation(&calls).expect("変更 API が呼ばれていません");
    assert_eq!(
        count(&calls[..first], "object_detail"),
        1,
        "同じ対象の詳細を 2 度読みました: {calls:?}"
    );
}

#[test]
fn sub_operations_in_different_layers_scan_each_layer() {
    // 設定値の変更だけの要求は宛先を確かめないため、記録された走査は事前解決相の
    // ものだけである。同じレイヤーなら 1 度、別のレイヤーならレイヤーごとに走る。
    let harness = Harness::with(|host| {
        host.scene.get_mut().unwrap().layers[0].objects[0]
            .effects
            .push(blur(0, 20));
    });

    let same_layer = batch(vec![
        set_item_op(harness.effect_selector(1, 100, "ぼかし", 0), "範囲", 40),
        set_item_op(harness.effect_selector(1, 300, "ぼかし", 0), "範囲", 50),
    ]);
    harness.host.clear_calls();
    harness
        .edit
        .apply_batch(&same_layer)
        .expect("一括適用の失敗");
    assert_eq!(
        count(&harness.host.calls(), "object_placements"),
        1,
        "同じレイヤーを 2 度走査しました: {:?}",
        harness.host.calls()
    );

    let across_layers = batch(vec![
        set_item_op(harness.effect_selector(0, 0, "ぼかし", 0), "範囲", 60),
        set_item_op(harness.effect_selector(1, 100, "ぼかし", 0), "範囲", 70),
    ]);
    harness.host.clear_calls();
    harness
        .edit
        .apply_batch(&across_layers)
        .expect("一括適用の失敗");
    assert_eq!(
        count(&harness.host.calls(), "object_placements"),
        2,
        "レイヤーごとの走査が分かれていません: {:?}",
        harness.host.calls()
    );
}

// ---------------------------------------------------------- 宛先の判定時点

#[test]
fn a_destination_freed_by_an_earlier_sub_operation_can_be_moved_into() {
    // 事前解決相で宛先を一括判定すると、先行 sub-operation が空けた宛先へ移動
    // する要求が必ず失敗する。**適用時点で判定することの価値はこの連鎖にある。**
    // ここでは 1 件目が (0,0) を空け、2 件目がそこへ入る。
    //
    // 2 つの対象が互いの位置を交換する形は、判定時点をどれだけ遅らせても
    // 成立しない。1 件目を発行する時点で相手はまだ宛先に居るためである。
    // 交換には空き位置を経由する 3 件目が要る。
    let harness = Harness::new();
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        move_op(harness.selector(1, 100), 0, 0),
    ]);
    let outcome = harness
        .edit
        .apply_batch(&params)
        .expect("先行が空けた宛先への移動が拒否されました");

    assert_eq!(placement_of(&harness, 1), (1, 500));
    assert_eq!(placement_of(&harness, 2), (0, 0));
    assert_eq!(outcome.results[0].object.layer, 1);
    assert_eq!(outcome.results[0].object.frame_start, 500);
    assert_eq!(outcome.results[1].object.layer, 0);
    assert_eq!(outcome.results[1].object.frame_start, 0);
}

#[test]
fn an_occupied_destination_is_detected_with_the_effects_of_earlier_sub_operations() {
    let harness = Harness::new();
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        move_op(harness.selector(1, 300), 1, 500),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("塞がった宛先への移動が成功しました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["reason"], json!("destination_occupied"));
    assert_eq!(error.details()["failed_index"], json!(1));
}

// ---------------------------------------------------------------- 巻き戻し

/// 3 件目で宛先が塞がる一括適用を組み立てる。
///
/// 先の 2 件は成功し、3 件目は SDK を呼ばずに落ちる。巻き戻しの対象は先行
/// 2 件だけである。
fn rollback_params(harness: &Harness) -> ApplyBatchParams {
    batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        move_op(harness.selector(1, 100), 0, 0),
        move_op(harness.selector(1, 300), 1, 500),
    ])
}

#[test]
fn a_rolled_back_batch_restores_every_target_in_reverse_order() {
    // 逆順は慣習ではない。移動を元位置へ戻すには元位置が空いている必要があり、
    // その位置を塞ぎ得るのは後から移動してきた sub-operation だけである。順方向
    // に戻すと、先に戻した対象が後続の元位置を塞いだままになり失敗する。
    let harness = Harness::new();
    let params = rollback_params(&harness);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("塞がった宛先への移動が成功しました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["reason"], json!("destination_occupied"));
    assert_eq!(error.details()["failed_index"], json!(2));
    assert_eq!(error.details()["rolled_back"], json!(true));
    assert_eq!(error.details()["rolled_back_count"], json!(2));
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(error.details()["retry_requires"], json!("refetch"));
    assert!(
        error.details().get("consistency_unknown").is_none(),
        "全件戻せたのに不整合を名乗りました"
    );

    assert_eq!(placement_of(&harness, 1), (0, 0));
    assert_eq!(placement_of(&harness, 2), (1, 100));
    assert_eq!(placement_of(&harness, 3), (1, 300));
}

#[test]
fn a_rollback_continues_after_one_inverse_fails() {
    // 1 件失敗しても止めない。止めると、戻せたはずのものまで戻さないことに
    // なる。
    let harness = Harness::with(|host| {
        // 発行の順序は「移動・設定値・設定値の巻き戻し・移動の巻き戻し」で
        // ある。3 件目だけを失敗させる。
        host.arm(|knobs| knobs.fault_at = Some((2, Fault::Mutation)));
    });
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        set_item_op(harness.effect_selector(1, 300, "ぼかし", 0), "範囲", 40),
        move_op(harness.selector(1, 300), 1, 500),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("塞がった宛先への移動が成功しました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert!(!error.retryable());
    assert_eq!(error.details()["consistency_unknown"], json!(true));
    assert_eq!(error.details()["rolled_back"], json!(false));
    assert_eq!(error.details()["rolled_back_count"], json!(1));
    assert_eq!(error.details()["failed_index"], json!(2));
    assert_eq!(error.details()["retry_requires"], json!("refetch"));

    // 戻せなかったのは設定値だけであり、移動は続けて戻されている。
    assert_eq!(placement_of(&harness, 1), (0, 0));
    assert_eq!(
        item_of(&harness, 3, 0, "範囲"),
        ItemValue::Integer { value: 40 }
    );
}

#[test]
fn a_move_that_does_not_land_back_on_its_origin_is_not_counted_as_restored() {
    // 発行しただけでは戻せたと言えない。ホストが位置を調整すれば、元へ戻した
    // つもりの対象が別の場所に居る。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::AdjustMoveDestination)));
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        move_op(harness.selector(1, 100), 1, 700),
        move_op(harness.selector(1, 300), 1, 500 + MOVE_FRAME_SHIFT as u32),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("塞がった宛先への移動が成功しました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(error.details()["consistency_unknown"], json!(true));
    assert_eq!(error.details()["rolled_back_count"], json!(0));
    assert_eq!(placement_of(&harness, 1), (0, MOVE_FRAME_SHIFT));
}

/// 書き戻した値がそのままでは読み直せない対象を持つ一式を組む。
///
/// ホストは上限を超える値を丸める。元値が上限を超えていれば、元の文字列を
/// 書き戻しても同じ文字列は読めない。
fn harness_with_unnormalized_item() -> Harness {
    Harness::with(|host| {
        host.scene.get_mut().unwrap().layers[1].objects[1].effects[0].items = vec![EffectItem {
            name: "範囲".to_string(),
            item_type: EffectItemType::Integer,
            value: ItemValue::Unknown {
                raw: format!("{:04}", MAX_ITEM_VALUE + 150),
            },
            track: None,
        }];
    })
}

#[test]
fn the_original_value_is_written_back_as_the_raw_string_the_host_returned() {
    // 読み取り経路が解釈した値を組み立て直すと、その往復の破れが巻き戻しの
    // 正しさへ持ち込まれる。前向きの変更なら要求元が正規化値から気付けるが、
    // 巻き戻しでは失敗の応答しか返らず、誰も値を検分しない。
    let harness = harness_with_unnormalized_item();
    let params = batch(vec![
        set_item_op(harness.effect_selector(1, 300, "ぼかし", 0), "範囲", 40),
        move_op(harness.selector(1, 300), 1, 100),
    ]);
    harness
        .edit
        .apply_batch(&params)
        .expect_err("塞がった宛先への移動が成功しました");

    assert_eq!(
        harness.host.item_value_arguments(),
        vec!["40".to_string(), format!("{:04}", MAX_ITEM_VALUE + 150)],
        "元値が生の文字列のまま書き戻されていません"
    );
}

#[test]
fn a_value_that_cannot_be_read_back_unchanged_is_not_counted_as_restored() {
    // 正規化が冪等でなければ、戻せていたのに不整合を名乗ることになる。それで
    // よい。逆へ倒すと、戻っていない値を戻ったと報告する。
    let harness = harness_with_unnormalized_item();
    let params = batch(vec![
        set_item_op(harness.effect_selector(1, 300, "ぼかし", 0), "範囲", 40),
        move_op(harness.selector(1, 300), 1, 100),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("塞がった宛先への移動が成功しました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(error.details()["consistency_unknown"], json!(true));
    assert_eq!(error.details()["rolled_back_count"], json!(0));
}

#[test]
fn a_failure_while_building_the_response_does_not_roll_anything_back() {
    // 応答を組み立てる時点で全変更は成功している。失敗したのは読み直しだけで
    // あり、巻き戻す理由が無い。変更が起きたという事実は、その後の処理が成功
    // したかとは独立している。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::ReadBack)));
    let params = batch(vec![move_op(harness.selector(0, 0), 1, 500)]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("読み直しの失敗が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(error.details()["retry_requires"], json!("refetch"));
    // 巻き戻しは起きていない。起きたと名乗ると、要求元は元へ戻ったと読む。
    assert!(error.details().get("rolled_back").is_none());
    assert!(error.details().get("consistency_unknown").is_none());
    // 変更はそのまま残っている。
    assert_eq!(placement_of(&harness, 1), (1, 500));
    assert_eq!(harness.project.revision(), 1);
}

#[test]
fn a_change_that_never_reached_the_sdk_is_not_rolled_back() {
    // SDK へ届かなかった失敗はプロジェクトを一切変えていない。巻き戻しの対象に
    // 数えると、起きていない変更を戻そうとすることになる。
    let harness = Harness::with(|host| {
        host.arm(|knobs| knobs.fault_at = Some((1, Fault::TargetGone)));
    });
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        move_op(harness.selector(1, 100), 1, 700),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("対象が失われた移動が成功しました");

    assert_eq!(error.error_code(), ErrorCode::NotFound);
    assert_eq!(error.details()["reason"], json!("target_missing"));
    assert_eq!(error.details()["failed_index"], json!(1));
    assert_eq!(error.details()["rolled_back"], json!(true));
    assert_eq!(error.details()["rolled_back_count"], json!(1));
    assert_eq!(placement_of(&harness, 1), (0, 0));
    assert_eq!(placement_of(&harness, 2), (1, 100));
}

/// 選択肢を持つ項目を変える sub-operation を組み立てる。
fn set_choice_op(harness: &Harness, value: &str) -> BatchOperation {
    BatchOperation::SetObjectItem {
        selector: harness.effect_selector(1, 300, SHAPE, 0),
        item: "図形の種類".to_string(),
        value: ItemValue::Choice {
            value: value.to_string(),
        },
    }
}

#[test]
fn a_choice_value_the_host_ignores_fails_the_batch_and_rolls_it_back() {
    // 照合を一括適用で省くと、単独では失敗する入力が一括適用では成功する経路が
    // できる。落ちた sub-operation 自身は変更を発行し終えているため、巻き戻しの
    // 対象に含める。
    let harness = harness_with_choice_effect();
    let params = batch(vec![
        set_item_op(harness.effect_selector(1, 300, "ぼかし", 0), "範囲", 40),
        set_choice_op(&harness, "存在しない形"),
    ]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("選択肢に無い値が一括適用で受理されました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("item_value_not_applied"));
    assert_eq!(error.details()["current_value"], json!(CHOICE_VALUES[0]));
    assert_eq!(error.details()["failed_index"], json!(1));
    assert_eq!(error.details()["rolled_back"], json!(true));
    assert_eq!(error.details()["rolled_back_count"], json!(2));

    // 先行 sub-operation も、落ちた sub-operation 自身も元へ戻っている。
    assert_eq!(
        item_of(&harness, 3, 0, "範囲"),
        ItemValue::Integer { value: 20 }
    );
    assert_eq!(
        item_of(&harness, 3, 1, "図形の種類"),
        ItemValue::Choice {
            value: CHOICE_VALUES[0].to_string(),
        }
    );
}

#[test]
fn a_choice_value_the_host_accepts_passes_through_the_batch() {
    let harness = harness_with_choice_effect();
    let params = batch(vec![
        set_item_op(harness.effect_selector(1, 300, "ぼかし", 0), "範囲", 40),
        set_choice_op(&harness, CHOICE_VALUES[1]),
    ]);
    harness
        .edit
        .apply_batch(&params)
        .expect("選択肢に在る値が一括適用で拒否されました");

    assert_eq!(
        item_of(&harness, 3, 1, "図形の種類"),
        ItemValue::Choice {
            value: CHOICE_VALUES[1].to_string(),
        }
    );
}

#[test]
fn the_plan_phase_reads_the_item_value_once_per_item_sub_operation() {
    // 移動の有無の照合は、逆操作の材料として既に読んだ値を見る。**読み直さない**
    // ため、sub-operation あたりの読み取りは 1 回のままである。
    let harness = Harness::new();
    let params = batch(vec![
        set_item_op(harness.effect_selector(1, 300, "ぼかし", 0), "範囲", 40),
        set_item_op(harness.effect_selector(1, 100, "ぼかし", 0), "範囲", 40),
    ]);
    harness.host.clear_calls();
    harness.edit.apply_batch(&params).expect("一括適用の失敗");

    let calls = harness.host.calls();
    let first = first_mutation(&calls).expect("変更 API が呼ばれていません");
    assert_eq!(
        count(&calls[..first], ITEM_VALUE),
        2,
        "事前解決相の読み取りが設定項目を変える sub-operation の件数と違います: {calls:?}"
    );
}

#[test]
fn the_same_movement_write_is_judged_the_same_way_alone_and_in_a_batch() {
    // 受理する集合が単独編集と一括適用で違ってはならない。**通る入力も並べる**
    // ——拒否だけを見ていると、両方が同じように拒みすぎていても気付けない。
    let cases: [(&str, ItemValue); 3] = [
        // 移動を消す数値の書き込み。どちらも拒否する。
        (
            MOVING_ITEM,
            ItemValue::Number {
                value: FiniteF64::try_new(0.0).expect("有限値"),
            },
        ),
        // 移動を消す明示的な指定。どちらも通す。
        (
            MOVING_ITEM,
            ItemValue::Track(aviutl2_mcp_core::TrackValue {
                values: vec![FiniteF64::try_new(50.0).expect("有限値")],
                mode: None,
                params: Vec::new(),
                accelerate: false,
                decelerate: false,
                twopoint: false,
            }),
        ),
        // 移動を持たない項目へ移動を付ける。どちらも通す。
        (STATIC_ITEM, movement(&[0.0, 50.0, 100.0], "直線移動")),
    ];
    for (item, value) in cases {
        let alone = harness_with_track_effect();
        let single = alone
            .edit
            .set_object_item(&set_track_item(&alone, item, value.clone()));

        let together = harness_with_track_effect();
        let batched = together
            .edit
            .apply_batch(&batch(vec![BatchOperation::SetObjectItem {
                selector: together.effect_selector(1, 100, COORDINATE, 0),
                item: item.to_string(),
                value: value.clone(),
            }]));

        match (single, batched) {
            (Ok(_), Ok(_)) => {}
            (Err(single), Err(batched)) => {
                assert_eq!(single.error_code(), batched.error_code(), "{item}");
                assert_eq!(
                    single.details()["reason"],
                    batched.details()["reason"],
                    "{item}"
                );
                assert_eq!(
                    single.details()["current_value"],
                    batched.details()["current_value"],
                    "{item}"
                );
                assert_eq!(batched.details()["failed_index"], json!(0), "{item}");
                // 事前解決相で落ちる。変更は 1 つも発行されていない。
                assert!(
                    batched.details().get("mutation_issued").is_none(),
                    "{item} の拒否が変更の発行後に起きました"
                );
                together.assert_untouched();
            }
            (single, batched) => panic!(
                "{item} へ {} を書いた結果が単独と一括で分かれました: {single:?} / {batched:?}",
                value.kind()
            ),
        }
    }
}

#[test]
fn the_apply_phase_reads_back_once_per_item_sub_operation() {
    // 照合の費用は sub-operation 1 件あたり 1 回に留まる。逆操作の材料を読む
    // 事前解決相と数を混ぜないため、最初の変更より後だけを数える。**照合が
    // 全種別へ掛かるため、設定項目を変える sub-operation はすべて 1 回ずつ
    // 数える。**
    let harness = Harness::with(|host| {
        host.catalog.push(shape_catalog_entry());
        let effects = &mut host.scene.get_mut().unwrap().layers[1].objects[1].effects;
        effects.push(shape(0));
        effects.push(shape(1));
    });
    let params = batch(vec![
        set_item_op(harness.effect_selector(1, 300, "ぼかし", 0), "範囲", 40),
        BatchOperation::SetObjectItem {
            selector: harness.effect_selector(1, 300, SHAPE, 0),
            item: "図形の種類".to_string(),
            value: ItemValue::Choice {
                value: CHOICE_VALUES[1].to_string(),
            },
        },
        BatchOperation::SetObjectItem {
            selector: harness.effect_selector(1, 300, SHAPE, 1),
            item: "図形の種類".to_string(),
            value: ItemValue::Choice {
                value: CHOICE_VALUES[1].to_string(),
            },
        },
        // 設定項目を変えない sub-operation は 1 回も足さない。
        move_op(harness.selector(0, 0), 0, 500),
    ]);
    harness.host.clear_calls();
    harness.edit.apply_batch(&params).expect("一括適用の失敗");

    let calls = harness.host.calls();
    let first = first_mutation(&calls).expect("変更 API が呼ばれていません");
    assert_eq!(
        count(&calls[first..], ITEM_VALUE),
        3,
        "適用相の読み直しが設定項目を変える sub-operation の件数と違います: {calls:?}"
    );
}

/// 設定項目を変える sub-operation を組み立てる。
fn set_shape_item_op(harness: &Harness, item: &str, value: ItemValue) -> BatchOperation {
    BatchOperation::SetObjectItem {
        selector: harness.effect_selector(1, 300, SHAPE, 0),
        item: item.to_string(),
        value,
    }
}

#[test]
fn the_same_item_value_fails_the_same_way_alone_and_in_a_batch() {
    // 受理する集合が単独編集と一括適用で違ってはならない。同じ入力に対して
    // 同じ code と同じ名前と同じ実値を返すことで固定する。**照合を広げた種別
    // すべてについて見る。** 1 種別だけを見ていると、判定が 2 か所へ分かれた
    // ときに残りの種別が一括適用でだけ通り抜ける。
    let mut cases: Vec<(&str, ItemValue)> = vec![(
        "図形の種類",
        ItemValue::Choice {
            value: "存在しない形".to_string(),
        },
    )];
    cases.extend(
        rewritten_item_cases()
            .into_iter()
            .map(|(item, requested, _)| (item, requested)),
    );

    for (item, requested) in cases {
        let alone = harness_with_choice_effect();
        let single = alone
            .edit
            .set_object_item(&SetObjectItemParams {
                selector: alone.effect_selector(1, 300, SHAPE, 0),
                item: item.to_string(),
                value: requested.clone(),
            })
            .expect_err("ホストが書き換えた値が単独で成功として返りました");

        let together = harness_with_choice_effect();
        let batched = together
            .edit
            .apply_batch(&batch(vec![set_shape_item_op(
                &together,
                item,
                requested.clone(),
            )]))
            .expect_err("ホストが書き換えた値が一括適用で受理されました");

        assert_eq!(single.error_code(), batched.error_code(), "{item}");
        assert_eq!(
            single.details()["reason"],
            batched.details()["reason"],
            "{item}"
        );
        assert_eq!(
            single.details()["current_value"],
            batched.details()["current_value"],
            "{item}"
        );
        assert_eq!(batched.details()["failed_index"], json!(0), "{item}");
    }
}

#[test]
fn a_batch_that_issued_nothing_does_not_advance_the_revision() {
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault_at = Some((0, Fault::TargetGone))));
    let params = batch(vec![move_op(harness.selector(0, 0), 1, 500)]);
    let error = harness
        .edit
        .apply_batch(&params)
        .expect_err("対象が失われた移動が成功しました");

    assert_eq!(error.error_code(), ErrorCode::NotFound);
    assert!(error.details().get("mutation_issued").is_none());
    assert_eq!(harness.project.revision(), 0);
}

// ------------------------------------------------------ 取り消し単位と許可

#[test]
fn one_batch_enters_the_edit_section_once_and_advances_the_revision_once() {
    // 許可はそれぞれ独立に発行を数える。2 つ取れば revision が 2 度進み、応答が
    // 返す値がどちらのものか定まらない。件数によらず 1 だけ進むことが、許可を
    // 1 つしか取っていないことの観測できる形である。
    let harness = Harness::new();
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        move_op(harness.selector(1, 100), 0, 0),
        set_item_op(harness.effect_selector(1, 300, "ぼかし", 0), "範囲", 40),
    ]);
    let outcome = harness.edit.apply_batch(&params).expect("一括適用の失敗");

    assert_eq!(harness.host.enter_calls(), 1);
    assert_eq!(harness.project.revision(), 1);
    assert_eq!(outcome.project_revision, 1);
    assert_eq!(
        count(&harness.host.calls(), "move_object")
            + count(&harness.host.calls(), "set_effect_item_value"),
        3,
        "発行された変更の件数が想定と異なります"
    );
}

// ---------------------------------------------------------------- 結果 DTO

#[test]
fn the_results_are_read_back_after_every_change_has_been_applied() {
    // sub-operation ごとに直後の状態で組み立てると、後続の変更で先に組み立てた
    // セレクターと fingerprint が無効になる。応答は「次の要求へそのまま使える
    // こと」を目的としており、無効なセレクターを返すことはその目的を裏切る。
    let harness = Harness::new();
    let params = batch(vec![
        move_op(harness.selector(1, 100), 1, 500),
        set_item_op(harness.effect_selector(1, 100, "ぼかし", 0), "範囲", 40),
    ]);
    let outcome = harness.edit.apply_batch(&params).expect("一括適用の失敗");

    assert_eq!(
        outcome.results[0].object, outcome.results[1].object,
        "同じ対象を指す sub-operation が別々の要約を返しました"
    );
    assert_eq!(outcome.results[0].object.frame_start, 500);
    // 移動は effect を返さない。設定値の変更は正規化後の値ごと返す。
    assert!(outcome.results[0].effect.is_none());
    let effect = outcome.results[1].effect.as_ref().expect("変更後の effect");
    assert_eq!(effect.name, "ぼかし");

    // 返したセレクターでそのまま次の要求を組み立てられる。
    harness
        .edit
        .apply_batch(&batch(vec![move_op(
            outcome.results[0].object.selector.clone(),
            1,
            700,
        )]))
        .expect("応答が返したセレクターが拒否されました");
}

#[test]
fn the_result_reports_where_the_host_actually_placed_the_object() {
    // 要求した宛先ではなく実際の配置を返す。要求値との一致を求めると、成功した
    // 移動が対象の不在として返る。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::AdjustMoveDestination)));
    let params = batch(vec![move_op(harness.selector(0, 0), 1, 500)]);
    let outcome = harness.edit.apply_batch(&params).expect("一括適用の失敗");

    assert_eq!(
        outcome.results[0].object.frame_start,
        500 + MOVE_FRAME_SHIFT
    );
}

// -------------------------------------------------------------------- panic

#[test]
fn a_panic_after_a_change_reports_that_the_project_may_be_half_applied() {
    // 捕捉は逆操作を保持する計画ごと巻き戻す。どこまで適用したかも、巻き戻しの
    // 途中だったかも分からない。1 つの変更しか持たない編集と違い、一括適用では
    // 中途半端な状態が実際に起こり得る。
    let harness = Harness::with(|host| {
        host.arm(|knobs| knobs.panic_at = Some(PanicPoint::AfterMutationScan));
    });
    let params = batch(vec![
        move_op(harness.selector(0, 0), 1, 500),
        move_op(harness.selector(1, 100), 0, 0),
    ]);
    let error = with_silent_panic_hook(|| {
        harness
            .edit
            .apply_batch(&params)
            .expect_err("panic が伝播しました")
    });

    assert_eq!(error.error_code(), ErrorCode::InternalError);
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(error.details()["consistency_unknown"], json!(true));
    assert_eq!(error.details()["retry_requires"], json!("refetch"));
    assert!(
        !harness.host.calls().contains(&CLOSURE_ESCAPED),
        "巻き戻しがクロージャの外へ漏れました"
    );
}

#[test]
fn a_single_edit_that_panics_after_a_change_does_not_claim_an_unknown_state() {
    // 単独の編集は 1 つの変更が入ったか入らないかしかない。中途半端な状態を
    // 名乗ると、要求元に無用の読み直しを強いる。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.panic_at = Some(PanicPoint::AfterMutation)));
    let params = move_params(&harness);
    let error = with_silent_panic_hook(|| {
        harness
            .edit
            .move_object(&params)
            .expect_err("panic が伝播しました")
    });

    assert_eq!(error.error_code(), ErrorCode::InternalError);
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert!(error.details().get("consistency_unknown").is_none());
}

#[test]
fn a_panic_before_any_change_does_not_claim_an_unknown_state() {
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.panic_at = Some(PanicPoint::InClosure)));
    let params = batch(vec![move_op(harness.selector(0, 0), 1, 500)]);
    let error = with_silent_panic_hook(|| {
        harness
            .edit
            .apply_batch(&params)
            .expect_err("panic が伝播しました")
    });

    assert_eq!(error.error_code(), ErrorCode::InternalError);
    assert!(error.details().get("mutation_issued").is_none());
    assert!(error.details().get("consistency_unknown").is_none());
    harness.assert_untouched();
}
