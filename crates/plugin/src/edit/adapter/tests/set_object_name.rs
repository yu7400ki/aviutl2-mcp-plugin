//! 対象名変更の統合テスト。

use super::*;

#[test]
fn a_silently_ignored_rename_is_not_reported_as_success() {
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreObjectName)));
    let error = harness
        .edit
        .set_object_name(&SetObjectNameParams {
            selector: harness.selector(1, 100),
            name: Some("新しい名前".to_string()),
        })
        .expect_err("無言で無視された改名が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("change_not_applied"));
}
