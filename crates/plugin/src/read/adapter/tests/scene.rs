//! 現在シーンとレイヤー一覧の取得の統合テスト。

use super::*;

#[test]
fn get_current_scene_returns_scene_and_revision() {
    let adapter = adapter();
    adapter.project.on_object_updated();
    let (scene, revision) = adapter.get_current_scene().unwrap();
    assert_eq!(scene.id, 0);
    assert_eq!(revision, 1);
}

#[test]
fn snapshot_revision_is_taken_inside_read_section() {
    // 参照区間へ入った時点の revision を採る。区間へ入る前の値を採っていると
    // ここで 0 が返り、テストが落ちる。
    let adapter = adapter_with(|project| FakeHost {
        bump_on_enter: 3,
        project: Some(Arc::clone(project)),
        ..FakeHost::new()
    });

    assert_eq!(adapter.list_layers(0).unwrap().snapshot_revision, 3);
}

#[test]
fn list_layers_enumerates_up_to_layer_max() {
    let adapter = adapter();
    let snapshot = adapter.list_layers(0).unwrap();

    assert_eq!(snapshot.items.len(), 3);
    assert_eq!(snapshot.items[0].index, 0);
    assert_eq!(snapshot.items[0].name.as_deref(), Some("背景"));
    assert_eq!(snapshot.items[0].object_count, 1);
    assert_eq!(snapshot.items[1].name, None);
    assert!(snapshot.items[1].locked);
    assert_eq!(snapshot.items[1].object_count, 2);
    assert!(!snapshot.items[2].enabled);
    assert_eq!(snapshot.items[2].object_count, 0);
}

#[test]
fn list_layers_counts_objects_without_reading_them() {
    // 件数のために名前や alias まで読むと、参照ロックを保持する時間が
    // オブジェクト数に比例して伸びる。
    let adapter = adapter();
    adapter.list_layers(0).unwrap();

    let calls = adapter.host.calls();
    assert!(calls.contains(&"object_count"), "{calls:?}");
    for forbidden in ["object_placements", "object_identity", "object_detail"] {
        assert!(
            !calls.contains(&forbidden),
            "件数のために {forbidden} を呼んでいます: {calls:?}"
        );
    }
}

#[test]
fn scene_guard_rejects_other_scene() {
    let adapter = adapter();
    for error in [
        adapter.list_layers(7).unwrap_err(),
        adapter.list_objects_page(7, None).unwrap_err(),
        adapter.get_selection_page(7).unwrap_err(),
    ] {
        assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
        assert_eq!(error.details()["expected_scene_id"], 7);
        assert_eq!(error.details()["current_scene_id"], 0);
    }
}

#[test]
fn layer_range_is_clamped_to_existing_layers() {
    assert_eq!(layer_range(None, 5), 0..=5);
    let filter = ObjectFilter {
        layer_min: Some(2),
        layer_max: Some(9),
    };
    assert_eq!(layer_range(Some(&filter), 5), 2..=5);
}
