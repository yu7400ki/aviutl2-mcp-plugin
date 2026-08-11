//! 編集情報の取得の統合テスト。

use super::*;

#[test]
fn a_failed_edit_info_call_is_told_apart_from_an_out_of_range_value() {
    // 読み取り経路にも同じ切り分けが要る。片方の経路だけを直すと、同じ
    // 壊れ方が呼び出し口によって別の応答になる。
    let call_failed = adapter_with(|_| FakeHost {
        edit_info_failure: Some(EditInfoFailure::CallFailed),
        ..FakeHost::new()
    });
    let call_error = call_failed.get_edit_info().unwrap_err();
    let call_details = call_error.details();
    assert_eq!(call_error.error_code(), ErrorCode::SdkError);
    assert_eq!(call_details["sdk_operation"], "get_edit_info");
    assert!(
        call_details.get("reason").is_none(),
        "呼び出しの失敗に名前が付きました: {call_details}"
    );

    let out_of_range = adapter_with(|_| FakeHost {
        edit_info_failure: Some(EditInfoFailure::OutOfRange),
        ..FakeHost::new()
    });
    let value_error = out_of_range.get_edit_info().unwrap_err();
    let value_details = value_error.details();
    assert_eq!(value_error.error_code(), call_error.error_code());
    assert_eq!(
        value_details["sdk_operation"],
        call_details["sdk_operation"]
    );
    assert_eq!(value_details["reason"], "edit_info_out_of_range");
}

#[test]
fn get_edit_info_maps_host_values() {
    let adapter = adapter();
    let info = adapter.get_edit_info().unwrap();

    assert_eq!(info.scene.id, 0);
    assert_eq!(info.scene.name.as_deref(), Some("Scene 1"));
    assert_eq!(info.scene.width, 1920);
    assert_eq!(info.scene.fps_rate, 30000);
    assert_eq!(info.scene.fps_scale, 1001);
    assert_eq!(info.scene.fps.map(|fps| fps.get()), Some(30000.0 / 1001.0));
    assert_eq!(
        info.cursor,
        Cursor {
            frame: 12,
            layer: 1
        }
    );
    assert_eq!(
        info.extent,
        Extent {
            frame_max: 3600,
            layer_max: 2
        }
    );
    assert_eq!(info.selected_range, Some(FrameRange { start: 10, end: 20 }));
    // 一覧は 4 つのフィールドを揃えて返る。tempo だけを運ぶと、読み取った
    // 一覧をそのまま書き戻す経路で残りの 3 つが失われる。
    assert_eq!(info.grid_bpm, vec![sample_grid_bpm()]);
    assert_eq!(info.project_epoch, adapter.project.epoch());
}

#[test]
fn fps_is_absent_when_denominator_is_zero() {
    let adapter = adapter_with(|_| FakeHost {
        info: HostEditInfo {
            fps_scale: 0,
            ..fake_edit_info()
        },
        ..FakeHost::new()
    });
    let info = adapter.get_edit_info().unwrap();
    assert_eq!(info.scene.fps, None);
    assert_eq!(info.scene.fps_rate, 30000);
    assert_eq!(info.scene.fps_scale, 0);
}

#[test]
fn unselected_range_is_absent() {
    let adapter = adapter_with(|_| FakeHost {
        info: HostEditInfo {
            selected_range: None,
            ..fake_edit_info()
        },
        ..FakeHost::new()
    });
    assert_eq!(adapter.get_edit_info().unwrap().selected_range, None);
}
