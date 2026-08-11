//! カーソル・選択範囲・フォーカスの params の検査。

use super::*;

#[test]
fn set_selection_rejects_omitting_every_change() {
    let params = SetSelectionParams {
        expected_scene_id: 0,
        cursor: None,
        selected_range: None,
        focus: None,
        display: None,
        expected_project_epoch: EPOCH.to_string(),
    };
    let error = params.validate().unwrap_err();
    assert_eq!(
        error,
        EditInputError::NoChangeRequested {
            fields: &["cursor", "selected_range", "focus", "display"],
        }
    );
    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);

    assert_eq!(sample_set_selection().validate(), Ok(()));
    assert_eq!(
        SetSelectionParams {
            cursor: None,
            selected_range: None,
            focus: Some(FocusChange::Clear {}),
            display: None,
            ..sample_set_selection()
        }
        .validate(),
        Ok(())
    );
    // 表示開始位置だけの指定でも変更要求として成立する。
    assert_eq!(
        SetSelectionParams {
            cursor: None,
            selected_range: None,
            focus: None,
            display: Some(DisplayStart {
                layer: 3,
                frame: 90,
            }),
            ..sample_set_selection()
        }
        .validate(),
        Ok(())
    );
}

#[test]
fn set_selection_rejects_a_display_start_outside_the_transferable_range() {
    let over = MAX_POSITION + 1;
    for (display, field) in [
        (
            DisplayStart {
                layer: over,
                frame: 0,
            },
            FIELD_LAYER,
        ),
        (
            DisplayStart {
                layer: 0,
                frame: over,
            },
            FIELD_FRAME,
        ),
    ] {
        assert_eq!(
            SetSelectionParams {
                cursor: None,
                selected_range: None,
                focus: None,
                display: Some(display),
                ..sample_set_selection()
            }
            .validate(),
            Err(EditInputError::PositionOutOfRange {
                field,
                value: over,
                max: MAX_POSITION,
            })
        );
    }
}
