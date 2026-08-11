//! 対象を解決する際の検証順序の統合テスト。

use super::*;

#[test]
fn the_selector_epoch_is_checked_first() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    // セレクター・シーン・fingerprint の全てを壊しても、最初の段で落ちる。
    params.selector.project_epoch = "別のプロジェクト".to_string();
    params.selector.scene_id = 9;
    params.selector.fingerprint = tamper(&params.selector.fingerprint);

    let error = harness.edit.move_object(&params).expect_err("epoch 不一致");
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));
    harness.assert_untouched();
}

#[test]
fn an_advanced_revision_is_accepted_when_the_fingerprint_matches() {
    let harness = Harness::new();
    let params = move_params(&harness);
    // 対象は変えずに revision だけを進める。fingerprint は一致したままである。
    harness.project.on_object_updated();

    harness
        .edit
        .move_object(&params)
        .expect("revision が進んだだけで編集が拒否されました");
    assert!(harness.host.mutated());
}

#[test]
fn a_scene_guard_mismatch_is_checked_before_the_resolution() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.scene_id = 9;
    // 解決できない座標を併せて指定しても、シーンの段で落ちる。
    params.selector.frame = 9_999;

    let error = harness.edit.move_object(&params).expect_err("シーン不一致");
    assert_eq!(error.details()["mismatch"], json!("scene_id"));
    assert_eq!(error.details()["expected_scene_id"], json!(9));
    harness.assert_untouched();
}

#[test]
fn a_tampered_fingerprint_is_rejected() {
    // 要求は算出方式を運ばない。対象が変化していれば fingerprint が捕まえる
    // ため、別対象への適用は起きない。
    let harness = Harness::new();
    let params = move_params(&harness);
    harness
        .edit
        .move_object(&params)
        .expect("現在の対象を指す指定が拒否されました");

    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.fingerprint = tamper(&params.selector.fingerprint);

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("fingerprint の食い違いが受理されました");
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["mismatch"], json!("fingerprint"));
    harness.assert_untouched();
}

#[test]
fn an_unresolvable_selector_is_not_found() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.frame = 9_999;

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("解決できない対象");
    assert_eq!(error.error_code(), ErrorCode::NotFound);
    harness.assert_untouched();
}

#[test]
fn an_ambiguous_selector_reports_the_candidate_count() {
    let harness = Harness::with(|host| {
        let mut scene = host.scene.lock().unwrap();
        // 同じ開始フレームに同名の対象を並べる。
        let duplicate = FakeObject {
            id: 42,
            placement: scene.layers[1].objects[0].placement.clone(),
            alias: "[1:100]".to_string(),
            effects: Vec::new(),
            section_points: Vec::new(),
        };
        scene.layers[1].objects.push(duplicate);
        drop(scene);
    });
    let mut params = MoveObjectParams {
        selector: harness.selector(0, 0),
        destination: Destination {
            layer: 1,
            frame: 500,
        },
    };
    params.selector.layer = 1;
    params.selector.frame = 100;
    params.selector.name = Some("立ち絵".to_string());

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("曖昧なセレクター");
    assert_eq!(error.error_code(), ErrorCode::AmbiguousSelector);
    assert_eq!(error.details()["candidate_count"], json!(2));
    harness.assert_untouched();
}

#[test]
fn a_fingerprint_mismatch_is_checked_before_the_operation_preconditions() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.fingerprint = tamper(&params.selector.fingerprint);
    // 宛先も埋めておく。fingerprint の段で落ちるので宛先重複にはならない。
    params.destination.frame = 300;

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("fingerprint 不一致");
    assert_eq!(error.details()["mismatch"], json!("fingerprint"));
    harness.assert_untouched();
}

/// 名前を変えられた対象への編集が、読み直せば作り直せる失敗として返ることを
/// 確かめる。
///
/// 名前で候補を絞ると、この状況は候補 0 件になり「再試行しても解消しない」
/// として返る。要求元は復帰できるのに停止する。
#[test]
fn a_renamed_target_is_rejected_as_a_content_mismatch() {
    let harness = Harness::new();
    let params = move_params(&harness);
    harness.host.scene.lock().unwrap().layers[1].objects[0]
        .placement
        .name = Some("改名後".to_string());

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("改名された対象への編集が受理されました");
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["mismatch"], json!("fingerprint"));
    assert_eq!(error.details()["retry_requires"], json!("refetch"));
    harness.assert_untouched();
}

/// 内容が食い違った応答が返した対象を、そのまま次の要求へ渡せることを確かめる。
///
/// 応答が現在の姿を返さなければ、要求元は列挙まで戻って対象を探し直すほかない。
/// 失敗と再要求の 2 呼び出しで済むことを、呼び出し回数ごと固定する。
#[test]
fn the_current_object_of_a_content_mismatch_is_accepted_as_is() {
    let harness = Harness::new();
    let params = move_params(&harness);
    harness.host.scene.lock().unwrap().layers[1].objects[0]
        .placement
        .name = Some("改名後".to_string());

    // 要求の組み立てに使った読み取りをここまでで数え、以降増えないことを見る。
    let reads_before = read_sections(&harness);

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("改名された対象への編集が受理されました");
    let details = error.details();
    assert_eq!(details["mismatch"], json!("fingerprint"));

    let selector: ObjectSelector =
        serde_json::from_value(details["current_object"]["selector"].clone())
            .expect("応答が返したセレクターを読み取れません");
    let outcome = harness
        .edit
        .move_object(&MoveObjectParams {
            selector,
            destination: params.destination,
        })
        .expect("応答が返したセレクターでの再要求が拒否されました");

    let object = outcome.object.expect("移動の応答が対象を返しませんでした");
    assert_eq!(object.frame_start, params.destination.frame as usize);
    assert_eq!(object.name.as_deref(), Some("改名後"));

    assert_eq!(
        read_sections(&harness),
        reads_before,
        "失敗と再要求の間に読み直しを挟みました"
    );
    assert_eq!(
        harness.host.enter_calls(),
        2,
        "失敗と再要求の 2 呼び出しで済んでいません"
    );
}

/// 名前を名乗らないセレクターでも対象が特定できることを確かめる。
#[test]
fn a_selector_without_a_name_still_resolves_the_target() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.name = None;

    harness
        .edit
        .move_object(&params)
        .expect("名前を持たない指定が拒否されました");
    assert!(harness.host.mutated());
}

#[test]
fn a_revision_change_during_the_resolution_does_not_stop_the_mutation() {
    // 対象の解決と fingerprint の再計算の間に revision が進む状況を作る。
    // 対象の内容は変わっていないので、変更はそのまま発行される。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.bump_on_detail = 1));
    let params = move_params(&harness);

    harness
        .edit
        .move_object(&params)
        .expect("解決中の revision の変化で変更が止まりました");
    assert!(harness.host.mutated());
}

#[test]
fn the_project_boundary_is_matched_only_before_the_resolution() {
    // 境界の照合は区間の先頭で 1 度だけ行う。区間へ入った後に境界が変わっても
    // 変更は止まらない——プロジェクト境界の更新は区間と同じスレッドで走るため、
    // 区間の内側で入れ替わる経路が存在しない。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.renew_on_detail = true));
    let params = move_params(&harness);

    harness
        .edit
        .move_object(&params)
        .expect("区間の内側の境界の変化で変更が止まりました");
    assert!(harness.host.mutated());

    // 区間へ入る前の境界の食い違いは従来どおり止める。
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.project_epoch = "別のプロジェクト".to_string();
    let error = harness
        .edit
        .move_object(&params)
        .expect_err("別プロジェクトのセレクターが受理されました");
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));
    assert!(!harness.host.mutated());
}
