//! 選択状態変更の統合テスト。

use super::*;

#[test]
fn the_selection_is_applied_in_a_fixed_order() {
    let harness = Harness::new();
    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition {
                layer: 1,
                frame: 5_000,
            }),
            selected_range: Some(RangeChange::Set { start: 10, end: 20 }),
            focus: Some(FocusChange::Set {
                object: harness.selector(1, 100),
            }),
            display: Some(DisplayStart {
                layer: 1,
                frame: 60,
            }),
            expected_project_epoch: harness.epoch(),
        })
        .expect("選択状態の変更に失敗しました");

    let calls: Vec<_> = harness
        .host
        .calls()
        .into_iter()
        .filter(|call| MUTATIONS.contains(call))
        .collect();
    assert_eq!(
        calls,
        vec![
            "set_cursor_layer_frame",
            "set_select_range",
            "set_display_layer_frame",
            "set_focus_object"
        ]
    );
    assert_eq!(
        state.applied,
        vec![
            SelectionField::Cursor,
            SelectionField::SelectedRange,
            SelectionField::Display,
            SelectionField::Focus
        ]
    );
    // ホストが範囲外の値をクランプしても失敗にしない。応答は実際の値を返す。
    assert_eq!(state.cursor.frame, MAX_FRAME);
    assert!(state.cursor.layer <= MAX_LAYER);
    assert_eq!(
        state.focus.expect("フォーカス対象").frame_start,
        100,
        "フォーカスの観測値が返っていません"
    );
}

#[test]
fn a_display_start_can_be_the_only_requested_change() {
    let harness = Harness::new();
    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: None,
            display: Some(DisplayStart {
                layer: 2,
                frame: 30,
            }),
            expected_project_epoch: harness.epoch(),
        })
        .expect("表示開始位置だけの要求が拒否されました");

    assert_eq!(state.applied, vec![SelectionField::Display]);
    assert!(state.not_applied.is_empty());
    assert_eq!(state.display.frame_start, 30);
    assert_eq!(state.display.layer_start, 2);
    assert_eq!(harness.host.scene().display.frame_start, 30);
}

#[test]
fn a_request_without_a_display_start_does_not_touch_the_display() {
    let harness = Harness::new();
    harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition { layer: 1, frame: 5 }),
            selected_range: Some(RangeChange::Clear {}),
            focus: Some(FocusChange::Clear {}),
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect("選択状態の変更に失敗しました");

    let calls = harness
        .host
        .calls()
        .into_iter()
        .filter(|call| *call == "set_display_layer_frame")
        .count();
    assert_eq!(calls, 0, "省略した軸に対して SDK が呼ばれました");
}

#[test]
fn a_clamped_display_start_is_reported_as_not_applied() {
    // ホストは設定できる範囲へ調整する。要求どおりの位置に無い以上、反映された
    // とは言えない。
    let harness = Harness::new();
    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: None,
            display: Some(DisplayStart {
                layer: 0,
                frame: 5_000,
            }),
            expected_project_epoch: harness.epoch(),
        })
        .expect("クランプが失敗として返りました");

    assert!(state.applied.is_empty());
    assert_eq!(state.not_applied, vec![SelectionField::Display]);
    assert_eq!(state.display.frame_start, MAX_FRAME);
}

#[test]
fn the_display_span_does_not_decide_whether_the_start_was_applied() {
    // 表示フレーム数・表示レイヤー数は厳密な値ではない。これらを判定に使うと、
    // 開始位置が要求どおりでも適用できなかったと報告することになる。
    let harness = Harness::new();
    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: None,
            display: Some(DisplayStart {
                layer: 1,
                frame: 60,
            }),
            expected_project_epoch: harness.epoch(),
        })
        .expect("表示開始位置の変更に失敗しました");

    assert_ne!(state.display.frame_num, state.display.frame_start);
    assert_ne!(state.display.layer_num, state.display.layer_start);
    assert_eq!(state.applied, vec![SelectionField::Display]);
    assert!(state.not_applied.is_empty());
}

/// 表示開始位置を含む要求を組み立てる。
fn set_display(
    harness: &Harness,
    layer: u32,
    frame: u32,
    focus: Option<FocusChange>,
) -> SetSelectionParams {
    SetSelectionParams {
        expected_scene_id: SCENE_ID,
        cursor: None,
        selected_range: None,
        focus,
        display: Some(DisplayStart { layer, frame }),
        expected_project_epoch: harness.epoch(),
    }
}

#[test]
fn the_display_start_decides_its_own_membership_by_the_observed_position() {
    // 表示開始位置は「呼び出しが通ったか」ではなく「要求どおりの位置に入ったか」
    // で振り分ける。3 通りを 1 つの表として並べる。
    let harness = Harness::new();
    let focused = harness.selector(1, 100);

    // 範囲を超えた要求はクランプされ、適用できなかった側へ入る。
    let clamped = harness
        .edit
        .set_selection(&set_display(&harness, 30, 3_000, None))
        .expect("クランプが失敗として返りました");
    assert!(clamped.applied.is_empty());
    assert_eq!(clamped.not_applied, vec![SelectionField::Display]);
    assert_ne!(clamped.display.frame_start, 3_000);
    assert_ne!(clamped.display.layer_start, 30);

    // 範囲内の要求はそのまま入る。
    let exact = harness
        .edit
        .set_selection(&set_display(&harness, 0, 0, None))
        .expect("範囲内の表示開始位置が拒否されました");
    assert_eq!(exact.applied, vec![SelectionField::Display]);
    assert!(exact.not_applied.is_empty());
    assert_eq!(exact.display.frame_start, 0);
    assert_eq!(exact.display.layer_start, 0);

    // フォーカスを同時に指定しても、表示開始位置は要求どおりに残る。
    let with_focus = harness
        .edit
        .set_selection(&set_display(
            &harness,
            0,
            5,
            Some(FocusChange::Set { object: focused }),
        ))
        .expect("フォーカスを伴う要求が拒否されました");
    assert_eq!(
        with_focus.applied,
        vec![SelectionField::Display, SelectionField::Focus]
    );
    assert!(with_focus.not_applied.is_empty());
    assert_eq!(with_focus.display.frame_start, 5);
    assert_eq!(with_focus.display.layer_start, 0);
}

#[test]
fn a_clamped_cursor_stays_applied_while_a_clamped_display_start_does_not() {
    // 非対称は軸の性質の違いから来る。カーソルは反映値そのものが応答に載るため
    // 丸められたかを要求元が読める。表示範囲は開始位置以外が概数であり、載せた
    // 値から要求との一致を判定できないため、こちらだけを plugin が振り分ける。
    let harness = Harness::new();
    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition {
                layer: 30,
                frame: 3_000,
            }),
            selected_range: None,
            focus: None,
            display: Some(DisplayStart {
                layer: 30,
                frame: 3_000,
            }),
            expected_project_epoch: harness.epoch(),
        })
        .expect("クランプが失敗として返りました");

    assert_eq!(state.applied, vec![SelectionField::Cursor]);
    assert_eq!(state.not_applied, vec![SelectionField::Display]);
    assert_ne!(state.cursor.frame, 3_000);
    assert_ne!(state.display.frame_start, 3_000);
}

#[test]
fn a_focus_target_is_resolved_before_it_is_set() {
    let harness = Harness::new();
    let mut selector = harness.selector(1, 100);
    selector.fingerprint = tamper(&selector.fingerprint);

    let error = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: Some(FocusChange::Set { object: selector }),
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect_err("照合を経ずにフォーカスが設定されました");

    assert_eq!(error.details()["mismatch"], json!("fingerprint"));
    assert!(!harness.host.mutated());
    assert!(
        harness.host.scene().focus.is_none(),
        "解決できない対象の指定で選択が解除されました"
    );
}

#[test]
fn a_scene_guard_protects_the_selection() {
    let harness = Harness::new();
    let error = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID + 7,
            cursor: Some(CursorPosition { layer: 1, frame: 5 }),
            selected_range: None,
            focus: None,
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect_err("別シーンの前提が受理されました");

    assert_eq!(error.details()["mismatch"], json!("scene_id"));
    assert!(!harness.host.mutated());
}

#[test]
fn the_selection_change_names_which_epoch_did_not_match() {
    // 前提と focus の双方から epoch を受け取るのは選択状態の変更だけである。
    // どちらで落ちたかを伝えなければ、要求元は直す先を選べない。
    let harness = Harness::new();
    let error = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: Some(FocusChange::Set {
                object: harness.selector(1, 100),
            }),
            display: None,
            expected_project_epoch: "別のプロジェクト".to_string(),
        })
        .expect_err("別プロジェクトの前提が受理されました");
    assert_eq!(error.details()["mismatch"], json!("expected_project_epoch"));

    let harness = Harness::new();
    let mut focus = harness.selector(1, 100);
    focus.project_epoch = "別のプロジェクト".to_string();
    let error = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: Some(FocusChange::Set { object: focus }),
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect_err("別プロジェクトのフォーカス対象が受理されました");
    assert_eq!(error.details()["mismatch"], json!("focus_project_epoch"));

    // focus を省略した要求は epoch を 1 か所からしか受け取らない。出所を名乗る
    // 理由が無い。
    let harness = Harness::new();
    let error = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition { layer: 1, frame: 5 }),
            selected_range: None,
            focus: None,
            display: None,
            expected_project_epoch: "別のプロジェクト".to_string(),
        })
        .expect_err("別プロジェクトの前提が受理されました");
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));
}

#[test]
fn a_partially_applied_selection_reports_both_lists() {
    // フォーカスだけが失敗する状況を作る。
    let harness = Harness::new();
    let focus = harness.selector(1, 100);
    harness
        .host
        .arm(|knobs| knobs.fault = Some(Fault::FocusGone));

    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: Some(FocusChange::Set { object: focus }),
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect("適用できた項目を伝える手段が失われました");

    assert!(state.applied.is_empty());
    assert_eq!(state.not_applied, vec![SelectionField::Focus]);
}

#[test]
fn the_same_selection_failure_does_not_change_success_by_what_else_was_requested() {
    // 同じ失敗が、同時に何を要求したかで成功にも失敗にも分かれてはならない。
    // 要求元から予測できなくなる。
    let harness = Harness::new();
    let focus = harness.selector(1, 100);
    harness
        .host
        .arm(|knobs| knobs.fault = Some(Fault::FocusGone));

    let alone = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: Some(FocusChange::Set {
                object: focus.clone(),
            }),
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect("フォーカスだけの要求");

    let harness = Harness::new();
    let focus = harness.selector(1, 100);
    harness
        .host
        .arm(|knobs| knobs.fault = Some(Fault::FocusGone));
    let combined = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition { layer: 0, frame: 1 }),
            selected_range: None,
            focus: Some(FocusChange::Set { object: focus }),
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect("カーソルを併せた要求");

    assert_eq!(alone.not_applied, vec![SelectionField::Focus]);
    assert_eq!(combined.not_applied, vec![SelectionField::Focus]);
}

#[test]
fn every_requested_selection_field_appears_in_exactly_one_list() {
    let harness = Harness::new();
    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition { layer: 1, frame: 5 }),
            selected_range: Some(RangeChange::Clear {}),
            focus: Some(FocusChange::Clear {}),
            display: Some(DisplayStart {
                layer: 1,
                frame: 60,
            }),
            expected_project_epoch: harness.epoch(),
        })
        .expect("選択状態の変更");

    assert_eq!(
        state.applied,
        vec![
            SelectionField::Cursor,
            SelectionField::SelectedRange,
            SelectionField::Display,
            SelectionField::Focus
        ]
    );
    assert!(state.not_applied.is_empty());
    for field in &state.applied {
        assert!(
            !state.not_applied.contains(field),
            "{field:?} が両方に現れました"
        );
    }
}
