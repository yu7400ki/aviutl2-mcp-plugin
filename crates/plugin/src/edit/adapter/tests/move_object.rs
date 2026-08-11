//! 対象移動の統合テスト。

use super::*;

#[test]
fn a_locked_destination_layer_is_rejected() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.destination.layer = 2;
    params.destination.frame = 500;

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("ロックされたレイヤーへ移動できました");
    assert_eq!(error.details()["reason"], json!("layer_locked"));
    harness.assert_untouched();
}

#[test]
fn an_occupied_destination_is_rejected() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.destination.frame = 350;

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("既存の対象へ重ねて移動できました");
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["reason"], json!("destination_occupied"));
    assert_eq!(error.details()["layer"], json!(1));
    assert_eq!(error.details()["frame"], json!(350));
    // 塞いでいる範囲が返るため、次の宛先を選ぶのに読み直しが要らない。
    assert_eq!(
        error.details()["occupied_by"],
        json!({"frame_start": 300, "frame_end": 400})
    );
    // 塞いでいる対象の名前と fingerprint は載せない。
    let text = error.details().to_string();
    assert!(!text.contains("字幕"), "{text}");
    assert!(!text.contains("fingerprint"), "{text}");
    harness.assert_untouched();
}

#[test]
fn moving_an_object_onto_itself_is_not_treated_as_occupied() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.destination.frame = 100;

    harness
        .edit
        .move_object(&params)
        .expect("自分自身の位置が塞がりとして扱われました");
}

#[test]
fn moving_reports_the_placement_the_host_chose() {
    // ホストが宛先を調整しても移動そのものは成功している。要求値との一致を
    // 求めると、成功した移動が対象の不在として返る。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::AdjustMoveDestination)));
    let params = move_params(&harness);
    let outcome = harness
        .edit
        .move_object(&params)
        .expect("宛先を調整されただけで移動が失敗しました");

    let moved = outcome.object.expect("移動後の対象");
    assert_eq!(
        moved.frame_start,
        500 + MOVE_FRAME_SHIFT,
        "要求した宛先をそのまま応答へ載せています"
    );
    // 応答が返した selector はそのまま次の要求へ渡せる。
    harness
        .read
        .get_object(&moved.selector)
        .expect("応答が返した selector で引けません");
}

#[test]
fn moving_fails_when_the_new_placement_cannot_be_read() {
    // read-back が無くなるわけではない。位置を読めなければ応答を組み立てられず、
    // 変更を発行した後の失敗として返す。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::PositionUnreadable)));
    let params = move_params(&harness);
    let error = harness
        .edit
        .move_object(&params)
        .expect_err("位置を読めないのに成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(
        error.details()["sdk_operation"],
        json!("get_object_layer_frame")
    );
    assert_eq!(error.details()["mutation_issued"], json!(true));
}

#[test]
fn a_read_back_failure_after_a_mutation_keeps_the_revision_and_reports_it() {
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::ReadBack)));
    let params = move_params(&harness);
    let error = harness
        .edit
        .move_object(&params)
        .expect_err("読み直しの失敗が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(error.details()["current_project_revision"], json!(1));
    assert_eq!(
        harness.project.revision(),
        1,
        "変更が入ったのに revision が戻されました"
    );
    assert!(
        harness.project.modified(),
        "変更が入ったのに未保存の変更なしと報告されます"
    );
}

#[test]
fn moving_checks_the_lock_of_both_the_source_and_the_destination() {
    // 移動元だけがロックされている場合。
    let harness = Harness::new();
    let error = harness
        .edit
        .move_object(&MoveObjectParams {
            selector: harness.selector(2, 0),
            destination: Destination {
                layer: 1,
                frame: 500,
            },
        })
        .expect_err("ロックされたレイヤーから移動できました");
    assert_eq!(error.details()["reason"], json!("layer_locked"));
    assert_eq!(error.details()["layer"], json!(2));
    harness.assert_untouched();

    // 移動先だけがロックされている場合。
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.destination.layer = 2;
    params.destination.frame = 500;
    let error = harness
        .edit
        .move_object(&params)
        .expect_err("ロックされたレイヤーへ移動できました");
    assert_eq!(error.details()["reason"], json!("layer_locked"));
    assert_eq!(error.details()["layer"], json!(2));
    harness.assert_untouched();
}

#[test]
fn the_lock_check_reads_only_the_lock_state() {
    // ここで使うのは 1 ビットである。名前と表示まで読むと、応答に現れない値の
    // 読み取り失敗が移動と削除の可否を左右する。
    let harness = Harness::new();
    let params = move_params(&harness);
    harness.host.clear_calls();
    harness
        .edit
        .move_object(&params)
        .expect("移動に失敗しました");

    let calls = harness.host.calls();
    assert!(
        !calls.contains(&LAYER_ATTRIBUTES),
        "ロックの確認がレイヤー属性をまとめて読みました: {calls:?}"
    );
}

#[test]
fn moving_within_one_layer_reads_the_lock_state_once() {
    // 移動元と移動先が同じレイヤーになる移動で 2 回読む理由が無い。
    let harness = Harness::new();
    let params = move_params(&harness);
    harness.host.clear_calls();
    harness
        .edit
        .move_object(&params)
        .expect("移動に失敗しました");
    assert_eq!(
        harness
            .host
            .calls()
            .iter()
            .filter(|call| **call == LAYER_LOCK)
            .count(),
        1,
        "同一レイヤー内の移動でロック状態を 2 回読みました"
    );

    // レイヤーを跨ぐ移動では移動元と移動先の双方を読む。
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.destination.layer = 0;
    params.destination.frame = 500;
    harness.host.clear_calls();
    harness
        .edit
        .move_object(&params)
        .expect("移動に失敗しました");
    assert_eq!(
        harness
            .host
            .calls()
            .iter()
            .filter(|call| **call == LAYER_LOCK)
            .count(),
        2,
        "レイヤーを跨ぐ移動で片方のロックしか確かめていません"
    );
}

#[test]
fn the_response_revision_comes_from_the_increment_not_from_a_reread() {
    // ホストが plugin 発の編集にも対象更新を配送する環境では、加算のあとに
    // 読み直すと別の値を読む。応答が返す revision が非決定になり、要求元の
    // 次の編集が確率的に前提条件で落ちる。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.bump_after_mutation = 3));
    let params = move_params(&harness);
    let outcome = harness
        .edit
        .move_object(&params)
        .expect("移動に失敗しました");

    assert_eq!(
        outcome.project_revision, 1,
        "応答が加算時点の値ではなく読み直した値を返しています"
    );
    assert_eq!(
        harness.project.revision(),
        4,
        "読み直せば別の値になる状況が作れていません"
    );
}
