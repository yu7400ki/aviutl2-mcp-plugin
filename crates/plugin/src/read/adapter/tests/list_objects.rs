//! オブジェクト列挙の統合テスト。

use super::*;

#[test]
fn list_objects_enumerates_every_layer_by_default() {
    let adapter = adapter();
    let snapshot = adapter.list_objects_page(0, None).unwrap();
    assert_eq!(snapshot.items.len(), 3);
    assert_eq!(snapshot.items[0].layer, 0);
    assert_eq!(snapshot.items[1].frame_start, 100);
    assert_eq!(snapshot.items[1].frame_end, 200);
    assert_eq!(snapshot.items[1].name.as_deref(), Some("立ち絵"));
}

#[test]
fn list_objects_applies_layer_filter() {
    let adapter = adapter();
    let filter = ObjectFilter {
        layer_min: Some(1),
        layer_max: Some(1),
    };
    let snapshot = adapter.list_objects_page(0, Some(&filter)).unwrap();
    assert_eq!(snapshot.items.len(), 2);
    assert!(snapshot.items.iter().all(|item| item.layer == 1));
}

#[test]
fn list_objects_clamps_filter_to_existing_layers() {
    let adapter = adapter();
    let filter = ObjectFilter {
        layer_min: None,
        layer_max: Some(999),
    };
    assert_eq!(
        adapter
            .list_objects_page(0, Some(&filter))
            .unwrap()
            .items
            .len(),
        3
    );
}

#[test]
fn list_objects_treats_the_filter_as_already_validated() {
    // 絞り込み条件の妥当性は要求の復号と同じ場所で判定するため、逆転した
    // 範囲はここへ届かない。届いた場合も空の範囲として扱われるだけで、
    // 矛盾した指定がホストへ渡ることはない。
    let adapter = adapter();
    let filter = ObjectFilter {
        layer_min: Some(2),
        layer_max: Some(1),
    };
    let snapshot = adapter
        .list_objects_page(0, Some(&filter))
        .expect("検証は呼び出し側の責務であり、ここでは失敗させない");
    assert!(snapshot.items.is_empty());
}

#[test]
fn list_objects_selector_can_be_resolved() {
    let adapter = adapter();
    let snapshot = adapter.list_objects_page(0, None).unwrap();
    for summary in snapshot.items {
        let detail = adapter.get_object(&summary.selector).unwrap();
        assert_eq!(
            detail.summary.selector.fingerprint,
            summary.selector.fingerprint
        );
    }
}

/// ページ窓の外にある対象を読まないことを確かめる。
#[test]
fn list_objects_reads_details_only_within_the_page() {
    let adapter = adapter();
    let page = adapter
        .list_objects(0, None, &page_request(1, 1, None))
        .unwrap()
        .unwrap();

    // 総件数は列挙全体の件数であり、窓の件数ではない。並び順も変わらない。
    assert_eq!(page.meta.total_count, 3);
    assert_eq!(page.meta.offset, 1);
    assert!(page.meta.has_more);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].layer, 1);
    assert_eq!(page.items[0].frame_start, 100);

    assert_eq!(identity_reads(&adapter), 1, "窓の外の対象まで読んでいます");
}

/// 重い読み取りの回数が、プロジェクトの規模ではなく要求ページの件数で
/// 決まることを確かめる。
#[test]
fn list_objects_bounds_detail_reads_by_the_page_size() {
    const TOTAL: usize = 200;
    const LIMIT: u32 = 5;

    let adapter = adapter_with(|_| FakeHost {
        layers: vec![FakeLayer {
            name: None,
            enabled: true,
            locked: false,
            objects: (0..TOTAL)
                .map(|index| object(0, index * 10, index * 10 + 5, None))
                .collect(),
        }],
        info: HostEditInfo {
            layer_max: 0,
            ..fake_edit_info()
        },
        ..FakeHost::new()
    });

    let page = adapter
        .list_objects(0, None, &page_request(0, LIMIT, None))
        .unwrap()
        .unwrap();

    assert_eq!(page.meta.total_count, TOTAL as u32);
    assert_eq!(page.items.len(), LIMIT as usize);
    assert_eq!(identity_reads(&adapter), LIMIT as usize);
}

/// 列挙が「対象が見つからない」を返さないことを確かめる。
///
/// 対象を 1 つも指定しない列挙で不在を返しても、要求元は何が見つからな
/// かったのかを特定できない。窓を確定してから対象が消えたのは列挙の失敗
/// である。
#[test]
fn list_objects_does_not_report_not_found() {
    let adapter = adapter_with(|_| FakeHost {
        object_missing_at: Some(100),
        ..FakeHost::new()
    });

    let error = adapter.list_objects_page(0, None).unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::SdkError);
    // 畳んだ後も、実際に不在を検出した呼び出しを指す。
    assert_eq!(error.details()["sdk_operation"], "find_object");
}

/// スナップショット revision が一致しない要求で、重い読み取りへ進まない
/// ことを確かめる。
#[test]
fn list_objects_rejects_a_stale_snapshot_revision_before_reading_details() {
    let adapter = adapter();
    let error = adapter
        .list_objects(0, None, &page_request(0, 50, Some(99)))
        .unwrap()
        .unwrap_err();

    assert_eq!(
        error,
        SnapshotRevisionMismatch {
            requested: 99,
            current: 0,
        }
    );
    assert_eq!(identity_reads(&adapter), 0);
}

/// 列挙が配下 effect を読まないことを確かめる。
///
/// 読めば 1 ページあたりの SDK 呼び出しが effect 数と設定項目数に比例して
/// 増え、窓内の 1 件の effect が読めないだけでページ全体が失敗する。
#[test]
fn list_objects_does_not_read_effects() {
    let adapter = adapter();
    let page = adapter.list_objects_page(0, None).unwrap();

    assert_eq!(page.items.len(), 3);
    assert!(
        !adapter.host.calls().contains(&EFFECT_LIST),
        "列挙が effect を読みました: {:?}",
        adapter.host.calls()
    );
    assert_eq!(detail_reads(&adapter), 0);
}

/// 配下 effect が読めなくても列挙が成功することを確かめる。
///
/// 列挙が effect を読んでいれば、窓に入った 1 件の失敗がページ全体を SDK の
/// 失敗へ落とす。対象を 1 つも指定していない要求が、応答に現れない値の
/// 読み取り失敗で丸ごと失敗する経路である。
#[test]
fn list_objects_survives_a_failing_effect_read() {
    let adapter = adapter_with(|_| FakeHost {
        effects_fail_at: Some(100),
        ..FakeHost::new()
    });

    let page = adapter
        .list_objects_page(0, None)
        .expect("effect の読み取り失敗が列挙を巻き込みました");
    assert_eq!(page.items.len(), 3);
}

/// 列挙の失敗へ畳んだ後も、不在を検出した呼び出しを指すことを確かめる。
///
/// 検出元を決め打ちすると、切り分けが誤った系統へ向かう。
#[test]
fn enumeration_failure_keeps_the_detecting_call() {
    for detected_by in ["find_object", "get_effect_list"] {
        let folded = enumeration_failure(ReadError::ObjectNotFound { detected_by });
        assert_eq!(folded.error_code(), ErrorCode::SdkError);
        assert_eq!(folded.details()["sdk_operation"], detected_by);
    }
    // 不在以外の失敗は分類を変えない。
    let untouched = enumeration_failure(ReadError::FingerprintMismatch {
        current_object: Box::new(crate::test_support::sample_object_summary()),
    });
    assert_eq!(untouched.error_code(), ErrorCode::PreconditionFailed);
}
