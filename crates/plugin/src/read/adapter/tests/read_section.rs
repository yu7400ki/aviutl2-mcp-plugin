//! 受付可否の判定と参照区間の使い方の統合テスト。

use super::*;

#[test]
fn not_ready_rejects_every_operation_without_touching_sdk() {
    let adapter = adapter_with(|_| FakeHost {
        ready: false,
        ..FakeHost::new()
    });

    for code in error_codes_of_all_operations(&adapter) {
        assert_eq!(code, ErrorCode::HostBusy);
    }
    assert!(
        adapter.host.calls().is_empty(),
        "準備前に SDK を呼び出しました: {:?}",
        adapter.host.calls()
    );
}

#[test]
fn not_ready_advises_retry() {
    let adapter = adapter_with(|_| FakeHost {
        ready: false,
        ..FakeHost::new()
    });
    let error = adapter.get_edit_info().unwrap_err();
    assert!(error.retryable());
    assert!(error.retry_after_ms().is_some());
}

#[test]
fn preview_and_save_are_edit_blocked_without_entering_read_section() {
    for state in [EditState::Preview, EditState::Save] {
        let adapter = adapter_with(|_| FakeHost {
            state,
            ..FakeHost::new()
        });

        for code in error_codes_of_all_operations(&adapter) {
            assert_eq!(
                code,
                ErrorCode::EditBlocked,
                "{state} で拒否されませんでした"
            );
        }
        assert!(
            !adapter.host.calls().contains(&"enter_read_section"),
            "{state} で参照区間へ入りました: {:?}",
            adapter.host.calls()
        );
        assert!(
            !adapter.host.calls().contains(&"edit_info"),
            "{state} で編集情報を取得しました"
        );
    }
}

#[test]
fn edit_blocked_reports_current_state() {
    let adapter = adapter_with(|_| FakeHost {
        state: EditState::Save,
        ..FakeHost::new()
    });
    let error = adapter.get_edit_info().unwrap_err();
    assert_eq!(error.details()["edit_state"], "save");
    assert!(error.retryable());
}

#[test]
fn guard_converts_panic_into_internal_error() {
    let error = with_silent_panic_hook(|| {
        guard::<()>(|| panic!("参照区間の内側で panic させます")).unwrap_err()
    });
    assert_eq!(error.error_code(), ErrorCode::InternalError);
}

#[test]
fn guard_passes_through_success_and_failure() {
    assert_eq!(guard(|| Ok(7)).unwrap(), 7);
    let error = guard::<()>(|| {
        Err(ReadError::ObjectNotFound {
            detected_by: "find_object",
        })
    })
    .unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::NotFound);
}

#[test]
fn panic_inside_read_section_becomes_internal_error() {
    let adapter = adapter_with(|_| FakeHost {
        panic_at: Some(PanicPoint::SceneName),
        ..FakeHost::new()
    });

    let error = with_silent_panic_hook(|| adapter.get_edit_info().unwrap_err());
    assert_eq!(error.error_code(), ErrorCode::InternalError);
    assert!(adapter.host.calls().contains(&"enter_read_section"));
}

#[test]
fn panic_inside_object_lookup_becomes_internal_error() {
    let adapter = adapter_with(|_| FakeHost {
        panic_at: Some(PanicPoint::ObjectPlacements),
        ..FakeHost::new()
    });
    let selector = sample_selector(&adapter);

    let error = with_silent_panic_hook(|| adapter.get_object(&selector).unwrap_err());
    assert_eq!(error.error_code(), ErrorCode::InternalError);
}

#[test]
fn panic_entering_the_read_section_becomes_internal_error() {
    // 参照区間へ入る呼び出しは、渡すクロージャを包んでも捕捉できない位置で
    // 落ち得る。捕捉しなければ接続の境界まで巻き戻り、要求元は応答ではなく
    // 切断を観測する。
    let adapter = adapter_with(|_| FakeHost {
        panic_at: Some(PanicPoint::EnterSection),
        ..FakeHost::new()
    });
    let selector = sample_selector(&adapter);

    with_silent_panic_hook(|| {
        for error in [
            adapter.get_edit_info().unwrap_err(),
            adapter.get_current_scene().unwrap_err(),
            adapter.list_layers(0).unwrap_err(),
            adapter.list_objects_page(0, None).unwrap_err(),
            adapter.get_object(&selector).unwrap_err(),
        ] {
            assert_eq!(error.error_code(), ErrorCode::InternalError);
        }
    });
}

#[test]
fn catch_returns_the_value_without_flattening() {
    assert_eq!(catch(|| 7).unwrap(), 7);
    let error = with_silent_panic_hook(|| {
        catch::<()>(|| panic!("参照区間へ入る呼び出しで panic させます")).unwrap_err()
    });
    assert_eq!(error.error_code(), ErrorCode::InternalError);
}

#[test]
fn panic_while_asking_readiness_becomes_internal_error() {
    // 受付判定の最初の一手も捕捉層の内側に置く。ここだけ素通しにすると、
    // 準備状態の問い合わせが落ちた場合に限って接続の境界まで巻き戻る。
    let adapter = adapter_with(|_| FakeHost {
        panic_at: Some(PanicPoint::IsReady),
        ..FakeHost::new()
    });

    with_silent_panic_hook(|| {
        for code in error_codes_of_all_operations(&adapter) {
            assert_eq!(code, ErrorCode::InternalError);
        }
    });
}

#[test]
fn panic_outside_the_read_section_becomes_internal_error() {
    // 編集情報の取得はフレームレートの分母が 0 のとき panic する。参照区間の
    // 外で起きるため、捕捉しなければ接続の境界まで巻き戻り、応答を返さない
    // まま切断される。
    let adapter = adapter_with(|_| FakeHost {
        panic_at: Some(PanicPoint::EditInfo),
        ..FakeHost::new()
    });

    with_silent_panic_hook(|| {
        for error in [
            adapter.get_edit_info().unwrap_err(),
            adapter.get_current_scene().unwrap_err(),
            adapter.list_layers(0).unwrap_err(),
            adapter.list_objects_page(0, None).unwrap_err(),
        ] {
            assert_eq!(error.error_code(), ErrorCode::InternalError);
        }
    });
    assert!(
        !adapter.host.calls().contains(&"enter_read_section"),
        "編集情報を取得できないまま参照区間へ入りました"
    );
}

#[test]
fn section_failure_during_playback_is_reported_as_edit_blocked() {
    // 受付判定と参照の確保の間に再生が始まると、参照の確保だけが失敗する。
    // 編集状態を読み直して、時間を置けば解消する失敗として返す。
    let adapter = adapter_with(|_| FakeHost {
        section_fails: true,
        later_state: Some(EditState::Preview),
        ..FakeHost::new()
    });

    let error = adapter.get_edit_info().unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::EditBlocked);
    assert_eq!(error.details()["edit_state"], "preview");
    assert!(error.retryable());
    assert!(error.retry_after_ms().is_some());
}

#[test]
fn section_failure_while_editing_remains_sdk_error() {
    // 再生・出力に由来しない失敗は分類を変えない。
    let adapter = adapter_with(|_| FakeHost {
        section_fails: true,
        ..FakeHost::new()
    });

    let error = adapter.get_edit_info().unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(error.details()["sdk_operation"], "call_read_section");
}

#[test]
fn errors_from_inside_the_section_are_not_reclassified() {
    // 参照区間へは入れており、内側の失敗は編集状態と無関係である。
    let adapter = adapter_with(|_| FakeHost {
        later_state: Some(EditState::Save),
        ..FakeHost::new()
    });
    let mut selector = sample_selector(&adapter);
    selector.frame = 1000;

    assert_eq!(
        adapter.get_object(&selector).unwrap_err().error_code(),
        ErrorCode::NotFound
    );
}

/// 参照区間の内側からプロジェクト境界へ触れても読み取りが完了することを
/// 確かめる。
///
/// 境界は非再入の Mutex で守られている。読み取りが区間を跨いでそれを保持して
/// いれば、同じスレッドからの更新で待ち合わせが解けなくなる。epoch を区間の
/// 外で採ることが、この経路を成立させている。
#[test]
fn reading_completes_when_the_project_boundary_changes_inside_the_section() {
    let project = Arc::new(ProjectState::new());
    let epoch = project.epoch();
    let adapter = HostReadAdapter::new(
        FakeHost {
            renew_boundary_on_enter: true,
            project: Some(Arc::clone(&project)),
            ..FakeHost::new()
        },
        Arc::clone(&project),
    );

    let (grid_bpm, object_count) = complete_within(Duration::from_secs(10), move || {
        let info = adapter.get_edit_info().unwrap();
        let objects = adapter.list_objects_page(0, None).unwrap();
        (info.grid_bpm.len(), objects.items.len())
    });

    assert_eq!(grid_bpm, 1);
    assert_eq!(object_count, 3);
    assert_ne!(
        project.epoch(),
        epoch,
        "参照区間の内側で境界が更新されていません"
    );
}
