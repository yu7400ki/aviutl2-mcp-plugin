//! 対象削除の統合テスト。

use super::*;

#[test]
fn a_silently_ignored_deletion_is_not_reported_as_success() {
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreDelete)));
    let error = harness
        .edit
        .delete_object(&DeleteObjectParams {
            selector: harness.selector(1, 100),
        })
        .expect_err("残っている対象が削除済みとして返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("change_not_applied"));
}

#[test]
fn deletion_confirms_that_the_target_is_gone() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .delete_object(&DeleteObjectParams {
            selector: harness.selector(1, 100),
        })
        .expect("削除に失敗しました");

    assert!(outcome.object.is_none());
    assert!(outcome.effect.is_none());
    // 削除の確認は同一区間内の読み直しで行う。
    let calls = harness.host.calls();
    let deleted = calls.iter().position(|call| *call == "delete_object");
    let confirmed = calls.iter().rposition(|call| *call == "object_identity");
    assert!(
        deleted < confirmed,
        "削除後の読み直しが行われていません: {calls:?}"
    );
}
