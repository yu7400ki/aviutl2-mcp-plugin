//! 区間の作成・削除・移動の統合テスト。

use super::*;

/// 応答の区間を `(start, end)` の列として取り出す。
fn section_pairs(outcome: &ObjectSectionsOutcome) -> Vec<(usize, usize)> {
    outcome
        .sections
        .iter()
        .map(|section| (section.start, section.end))
        .collect()
}

#[test]
fn the_section_index_addresses_the_same_element_of_the_sections_list() {
    // 区間番号 i は sections[i] を指す。i 番目の中間点を sections[i-1] へ写す
    // 実装は、区間 2 の削除で 120 ではなく 150 を消す。
    let harness = harness_with_sections();
    let outcome = harness
        .edit
        .delete_object_section(&DeleteObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 2,
        })
        .expect("区間 2 の削除が拒否されました");

    // 消えたのは sections[2].start = 150 であり、sections[1].start = 120 ではない。
    assert_eq!(harness.host.section_points(1, 100), vec![120, 180]);
    assert_eq!(
        section_pairs(&outcome),
        vec![(100, 119), (120, 179), (180, 200)]
    );
}

#[test]
fn moving_a_section_moves_the_boundary_that_starts_it() {
    // 区間 1 の開始位置は 1 番目の中間点 120 である。1 つずれた実装は 150 を
    // 動かし、応答の sections[1].start が要求したフレームにならない。
    let harness = harness_with_sections();
    let outcome = harness
        .edit
        .move_object_section(&MoveObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 1,
            frame: 110,
        })
        .expect("区間 1 の移動が拒否されました");

    assert_eq!(harness.host.section_points(1, 100), vec![110, 150, 180]);
    assert_eq!(outcome.sections[1].start, 110);
    assert_eq!(
        section_pairs(&outcome),
        vec![(100, 109), (110, 149), (150, 179), (180, 200)]
    );
}

/// フォーカスの区間番号が、対象の詳細が返す区間の列の添字であることを確かめる。
///
/// 2 つの tool にまたがる契約であり、片方の応答だけを見ても崩れに気付けない。
/// 同じ状態に対して両方を呼び、番号が列の範囲に収まること、指した要素が中間点で
/// 始まること、中間点を動かせば両者が揃って追随することを見る。
#[test]
fn the_focused_section_number_indexes_the_sections_of_the_focused_object() {
    let harness = harness_with_sections();
    harness.host.focus_object(Some((1, 100)), Some(2));

    let focused_section = |harness: &Harness| {
        let snapshot = harness
            .read
            .get_selection(SCENE_ID, &default_page_request())
            .expect("選択を取得できます")
            .expect("ページ要求が拒否されました");
        let focus = snapshot.focus.expect("フォーカス対象がありません");
        let section = snapshot.focus_section.expect("区間番号がありません");
        let detail = harness
            .read
            .get_object(&focus.selector)
            .expect("フォーカス対象の詳細を引けません");
        assert!(
            section < detail.sections.len(),
            "区間番号 {section} が区間の列 {:?} の外を指しています",
            detail.sections
        );
        (section, detail.sections[section].start)
    };

    // 区間 2 の開始位置は 2 番目の中間点である。
    assert_eq!(focused_section(&harness), (2, 150));

    harness
        .edit
        .move_object_section(&MoveObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 2,
            frame: 160,
        })
        .expect("区間 2 の移動が拒否されました");

    // 番号は変わらず、指す先だけが動く。
    assert_eq!(focused_section(&harness), (2, 160));
}

#[test]
fn creating_a_section_puts_the_frame_at_the_start_of_a_section() {
    let harness = harness_with_sections();
    let outcome = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 160,
        })
        .expect("中間点の追加が拒否されました");

    assert!(
        outcome.sections.iter().any(|section| section.start == 160),
        "追加したフレームが区間の開始フレームとして現れていません: {:?}",
        outcome.sections
    );
    assert_eq!(
        section_pairs(&outcome),
        vec![(100, 119), (120, 149), (150, 159), (160, 179), (180, 200)]
    );
}

#[test]
fn the_section_response_carries_the_state_after_the_change() {
    // 応答の sections は read-back そのものである。変更前の複製を返す実装では
    // 件数が増えない。
    let harness = harness_with_sections();
    let before = harness
        .read
        .get_object(&harness.selector(1, 100))
        .expect("対象の詳細を取得できませんでした")
        .sections
        .len();
    let outcome = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 160,
        })
        .expect("中間点の追加が拒否されました");

    assert_eq!(outcome.sections.len(), before + 1);
}

#[test]
fn the_section_response_carries_the_selector_after_the_change() {
    // 応答の selector と fingerprint は変更後に読み直した値である。要求で
    // 受け取った selector をそのまま返す実装では、対象の現在の姿が分からない。
    let harness = harness_with_sections();
    let selector = harness.selector(1, 100);
    let outcome = harness
        .edit
        .delete_object_section(&DeleteObjectSectionParams {
            selector: selector.clone(),
            section: 1,
        })
        .expect("中間点の削除が拒否されました");

    assert_eq!(outcome.object.selector.layer, selector.layer);
    assert_eq!(outcome.object.selector.frame, selector.frame);
    assert_eq!(outcome.object.selector.project_epoch, harness.epoch());
    // 読み直した対象をそのまま次の編集へ渡せる。
    harness
        .edit
        .delete_object_section(&DeleteObjectSectionParams {
            selector: outcome.object.selector.clone(),
            section: 1,
        })
        .expect("応答が返した selector で続けて編集できませんでした");
}

#[test]
fn a_failed_section_precondition_leaves_the_project_untouched() {
    let harness = harness_with_sections();
    let failures = section_precondition_failures(&harness);
    assert!(!failures.is_empty());
    harness.assert_untouched();
}

#[test]
fn creating_at_the_end_frame_of_the_object_is_accepted() {
    // 受け付ける範囲は閉区間である。終了フレームちょうどを外すと、最後の
    // 1 フレームだけ中間点を置けない穴ができる。
    let harness = harness_with_sections();
    let outcome = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 200,
        })
        .expect("終了フレームへの追加が拒否されました");
    assert_eq!(outcome.sections.last().expect("区間がある").start, 200);

    let error = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 201,
        })
        .expect_err("終了フレームより後への追加が受理されました");
    assert_eq!(error.details()["reason"], json!("frame_outside_object"));
}

#[test]
fn creating_at_the_start_frame_of_the_object_reports_an_existing_boundary() {
    // 開始フレームは範囲の内側であり、範囲外ではない。既に区間の開始位置で
    // あることが理由であり、要求元が直すべき点が違う。
    let harness = harness_with_sections();
    let error = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 100,
        })
        .expect_err("開始フレームへの追加が受理されました");
    assert_eq!(error.details()["reason"], json!("section_boundary_exists"));

    let error = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 99,
        })
        .expect_err("開始フレームより前への追加が受理されました");
    assert_eq!(error.details()["reason"], json!("frame_outside_object"));
}

#[test]
fn a_section_that_cannot_be_reread_is_reported_as_a_change_that_went_through() {
    // 変更は発行済みである。読み直せなかったことを「適用されなかった」として
    // 返すと、要求元は入った変更を無かったものとして次の要求を組み立てる。
    let harness = harness_with_sections();
    let selector = harness.selector(1, 100);
    harness.host.arm(|knobs| {
        knobs.fault = Some(Fault::SectionsUnreadable);
    });

    let error = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector,
            frame: 160,
        })
        .expect_err("読み直せないのに成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(error.details()["current_project_revision"], json!(1));
    assert_eq!(error.details()["retry_requires"], json!("refetch"));
    // 事前確認は通っている。変更そのものはホストへ届いた。
    assert!(harness.host.mutated());
    assert_eq!(
        harness.host.section_points(1, 100),
        vec![120, 150, 160, 180]
    );
    assert_eq!(harness.project.revision(), 1);
}

#[test]
fn a_move_that_stops_short_of_the_neighbours_is_accepted() {
    // 事前確認が広すぎないことを確かめる。隣の中間点の直前・直後は通る。
    let harness = harness_with_sections();
    let outcome = harness
        .edit
        .move_object_section(&MoveObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 2,
            frame: 179,
        })
        .expect("隣の中間点を越えない移動が拒否されました");
    assert_eq!(outcome.sections[2].start, 179);
}

#[test]
fn a_move_to_the_end_of_the_object_is_accepted() {
    // 最後の区間の移動先はオブジェクトの終了フレームまで許す。
    let harness = harness_with_sections();
    let outcome = harness
        .edit
        .move_object_section(&MoveObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 3,
            frame: 200,
        })
        .expect("終了フレームへの移動が拒否されました");
    assert_eq!(outcome.sections[3].start, 200);

    let error = harness
        .edit
        .move_object_section(&MoveObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 3,
            frame: 201,
        })
        .expect_err("終了フレームより後への移動が受理されました");
    assert_eq!(
        error.details()["reason"],
        json!("section_move_crosses_boundary")
    );
}

#[test]
fn a_rejected_section_change_that_passed_the_precheck_names_the_sdk_function() {
    // 事前確認を通ったのに false が返る経路。要求元に直せることが無いため、
    // 要求の誤りではなく SDK の失敗として返す。
    let harness = harness_with_sections();
    let selector = harness.selector(1, 100);
    harness.host.arm(|knobs| {
        knobs.fault = Some(Fault::RejectSectionChange);
    });

    for (operation, error) in [
        (
            "create_object_section",
            harness
                .edit
                .create_object_section(&CreateObjectSectionParams {
                    selector: selector.clone(),
                    frame: 160,
                })
                .expect_err("拒否された追加が成功として返りました"),
        ),
        (
            "delete_object_section",
            harness
                .edit
                .delete_object_section(&DeleteObjectSectionParams {
                    selector: selector.clone(),
                    section: 1,
                })
                .expect_err("拒否された削除が成功として返りました"),
        ),
        (
            "move_object_section",
            harness
                .edit
                .move_object_section(&MoveObjectSectionParams {
                    selector: selector.clone(),
                    section: 1,
                    frame: 110,
                })
                .expect_err("拒否された移動が成功として返りました"),
        ),
    ] {
        assert_eq!(error.error_code(), ErrorCode::SdkError, "{operation}");
        assert_eq!(
            error.details()["reason"],
            json!("section_change_rejected"),
            "{operation}"
        );
        assert_eq!(
            error.details()["sdk_operation"],
            json!(operation),
            "{operation}"
        );
    }
}

#[test]
fn the_precheck_reads_the_sections_inside_the_edit_section() {
    // 事前確認は区間の内側で読み直した実態に対して行う。区間の外の複製で
    // 判定する実装では、この記録が変更の前に現れない。
    let harness = harness_with_sections();
    let selector = harness.selector(1, 100);
    harness.host.clear_calls();
    harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector,
            frame: 160,
        })
        .expect("中間点の追加が拒否されました");

    let calls = harness.host.calls();
    let mutation = calls
        .iter()
        .position(|call| *call == "create_object_section")
        .expect("変更 API が呼ばれていません");
    let first_read = calls
        .iter()
        .position(|call| *call == SECTION_RANGES)
        .expect("区間を読み直していません");
    assert!(
        first_read < mutation,
        "事前確認の読み直しが変更より後です: {calls:?}"
    );
    // 読み直しは事前確認と read-back の 2 回だけである。
    assert_eq!(
        calls.iter().filter(|call| **call == SECTION_RANGES).count(),
        2,
        "{calls:?}"
    );
}

/// 中間点を変える SDK の関数名。
///
/// フェイクが記録する名前であり、変更が発行されたかを名前で数えられる。成否だけを
/// 見ると、判定が変更の後に置かれた実装でも通ってしまう。
const SECTION_MUTATIONS: [&str; 3] = [
    "create_object_section",
    "delete_object_section",
    "move_object_section",
];

/// ロックされたレイヤーの対象に対して 3 operation を 1 度ずつ実行する。
fn locked_layer_section_changes(harness: &Harness) -> Vec<(&'static str, EditError)> {
    let selector = || harness.selector(1, 100);
    vec![
        (
            "create_object_section",
            harness
                .edit
                .create_object_section(&CreateObjectSectionParams {
                    selector: selector(),
                    frame: 160,
                })
                .expect_err("ロックされたレイヤーへ中間点を追加できました"),
        ),
        (
            "delete_object_section",
            harness
                .edit
                .delete_object_section(&DeleteObjectSectionParams {
                    selector: selector(),
                    section: 1,
                })
                .expect_err("ロックされたレイヤーの中間点を削除できました"),
        ),
        (
            "move_object_section",
            harness
                .edit
                .move_object_section(&MoveObjectSectionParams {
                    selector: selector(),
                    section: 1,
                    frame: 110,
                })
                .expect_err("ロックされたレイヤーの中間点を移動できました"),
        ),
    ]
}

#[test]
fn every_section_change_is_refused_on_a_locked_layer() {
    let harness = harness_with_sections();
    harness.host.lock_layer(1, true);

    for (operation, error) in locked_layer_section_changes(&harness) {
        assert_eq!(
            error.error_code(),
            ErrorCode::PreconditionFailed,
            "{operation}"
        );
        assert_eq!(
            error.details()["reason"],
            json!("layer_locked"),
            "{operation}"
        );
        assert_eq!(error.details()["layer"], json!(1), "{operation}");
    }

    // 数えるのは変更 API だけである。対象の解決とロック状態の読み取りは判定に
    // 要るため、読み取りが起きないことは求めない。
    let calls = harness.host.calls();
    for mutation in SECTION_MUTATIONS {
        assert!(
            !calls.contains(&mutation),
            "{mutation} が呼ばれました: {calls:?}"
        );
    }
    harness.assert_untouched();
}

#[test]
fn a_locked_layer_is_reported_before_the_section_precheck() {
    // 事前確認にも掛かる要求を送る。ロックの判定が事前確認より後にある実装は、
    // 要求元が直しても解けない理由を名乗り、要求元は往復を繰り返す。
    let harness = harness_with_sections();
    harness.host.lock_layer(1, true);
    let selector = || harness.selector(1, 100);

    let failures = [
        (
            "範囲外のフレームへの追加",
            harness
                .edit
                .create_object_section(&CreateObjectSectionParams {
                    selector: selector(),
                    frame: 400,
                })
                .expect_err("範囲外への追加が受理されました"),
        ),
        (
            "区間数以上の番号での削除",
            harness
                .edit
                .delete_object_section(&DeleteObjectSectionParams {
                    selector: selector(),
                    section: 4,
                })
                .expect_err("区間数以上の番号での削除が受理されました"),
        ),
        (
            "区間数以上の番号での移動",
            harness
                .edit
                .move_object_section(&MoveObjectSectionParams {
                    selector: selector(),
                    section: 4,
                    frame: 190,
                })
                .expect_err("区間数以上の番号での移動が受理されました"),
        ),
    ];

    for (label, error) in failures {
        assert_eq!(error.details()["reason"], json!("layer_locked"), "{label}");
    }
}

#[test]
fn every_section_change_passes_on_an_unlocked_layer() {
    // ガードが広すぎないこと。ロックしていないレイヤーでは 3 つとも通る。
    let harness = harness_with_sections();
    harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 160,
        })
        .expect("中間点の追加が拒否されました");
    harness
        .edit
        .move_object_section(&MoveObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 1,
            frame: 110,
        })
        .expect("中間点の移動が拒否されました");
    harness
        .edit
        .delete_object_section(&DeleteObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 1,
        })
        .expect("中間点の削除が拒否されました");
}

#[test]
fn section_changes_do_not_read_the_effect_list() {
    // 応答は effect を含まない。読めば、無関係な読み取り失敗が反映済みの変更を
    // 失敗として報告させる。
    let harness = harness_with_sections();
    let selector = harness.selector(1, 100);
    harness.host.clear_calls();
    harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector,
            frame: 160,
        })
        .expect("中間点の追加が拒否されました");

    assert!(
        !harness.host.calls().contains(&EFFECT_LIST),
        "配下 effect を読んでいます: {:?}",
        harness.host.calls()
    );
}
