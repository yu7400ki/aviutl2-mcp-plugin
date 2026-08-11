//! 選択状態の取得の統合テスト。

use super::*;

/// ホストが逆順で返しても、選択がレイヤー・開始フレームの昇順で返ることを
/// 確かめる。
///
/// ページ間で順序が変わると、取りこぼしと重複が同時に起きる。オブジェクトの
/// 列挙と同じ並びであることも併せて確かめる。要求元は 2 つの応答を
/// 突き合わせられる。
#[test]
fn the_selection_is_ordered_by_layer_and_start_frame() {
    let adapter = adapter_with(|_| selecting_host());
    // ホストが既に昇順で返していれば、並べ替えを外した実装でも同じ結果に
    // なる。フェイクの並びが期待と違うことを先に押さえる。
    assert_ne!(
        adapter.host.selected,
        ascending(&adapter.host.selected),
        "フェイクが昇順で返しています"
    );

    let snapshot = adapter.get_selection_page(0).unwrap();

    let positions: Vec<(usize, usize)> = snapshot
        .selected
        .iter()
        .map(|object| (object.layer, object.frame_start))
        .collect();
    assert_eq!(positions, vec![(0, 0), (1, 100), (1, 300)]);

    let enumerated: Vec<(usize, usize)> = adapter
        .list_objects_page(0, None)
        .unwrap()
        .items
        .iter()
        .map(|object| (object.layer, object.frame_start))
        .collect();
    assert_eq!(positions, enumerated, "列挙と並びが違います");
}

/// 選択の alias を読むのがページの窓に入った分だけであることを確かめる。
///
/// 応答へ載せない対象まで読むと、参照区間の保持時間が要求ページではなく
/// 選択の規模で決まってしまう。
#[test]
fn the_selection_reads_aliases_only_within_the_page() {
    let adapter = adapter_with(|_| selecting_host());
    // 窓の位置で並べ替えの有無を見分ける検査である。ホストが既に昇順で
    // 返していれば、並べ替えを外した実装でも同じ対象が窓に入る。
    assert_ne!(
        adapter.host.selected,
        ascending(&adapter.host.selected),
        "フェイクが昇順で返しています"
    );

    let snapshot = adapter
        .get_selection(0, &page_request(0, 1, None))
        .unwrap()
        .unwrap();

    // 総件数は選択全体の件数であり、窓の件数ではない。
    assert_eq!(snapshot.page.total_count, 3);
    assert_eq!(snapshot.page.offset, 0);
    assert!(snapshot.page.has_more);
    assert_eq!(snapshot.selected.len(), 1);
    // 並べ替えた後の先頭である。ホストが返す順序のまま切り出せば、末尾に
    // 居るはずの対象が返る。
    assert_eq!(
        (snapshot.selected[0].layer, snapshot.selected[0].frame_start),
        (0, 0)
    );

    assert_eq!(
        identity_reads(&adapter),
        1,
        "窓の外の対象まで alias を読んでいます: {:?}",
        adapter.host.calls()
    );
}

/// 選択の一覧が配下 effect を読まないことを確かめる。
#[test]
fn the_selection_does_not_read_effects() {
    let adapter = adapter_with(|_| selecting_host());
    let snapshot = adapter.get_selection_page(0).unwrap();

    assert_eq!(snapshot.selected.len(), 3);
    assert!(
        !adapter.host.calls().contains(&EFFECT_LIST),
        "選択の取得が effect を読みました: {:?}",
        adapter.host.calls()
    );
}

/// フォーカス対象とその区間番号が同じ組で返ることを確かめる。
#[test]
fn the_focused_object_carries_its_section_number() {
    let adapter = adapter_with(|_| selecting_host());
    let snapshot = adapter.get_selection_page(0).unwrap();

    let focus = snapshot.focus.expect("フォーカス対象がありません");
    assert_eq!((focus.layer, focus.frame_start), (1, 100));
    assert_eq!(snapshot.focus_section, Some(1));
}

/// フォーカス対象が居るのに区間番号が得られない場合を確かめる。
///
/// ラッパーはホストの `-1` を `None` へ写す。番号だけが落ちても対象は返る。
#[test]
fn a_focused_object_without_a_section_number_still_returns_the_object() {
    let adapter = adapter_with(|_| FakeHost {
        focus_section: None,
        ..selecting_host()
    });
    let snapshot = adapter.get_selection_page(0).unwrap();

    assert!(snapshot.focus.is_some());
    assert_eq!(snapshot.focus_section, None);
}

/// フォーカス対象が無ければ区間番号も無いことを確かめる。
///
/// 区間番号は対象の性質である。ホストが対象を返さないまま番号だけを返しても、
/// 対象と番号の食い違った組を応答へ載せない。
#[test]
fn an_unfocused_selection_carries_no_section_number() {
    let adapter = adapter_with(|_| FakeHost {
        focus: None,
        focus_section: Some(3),
        ..selecting_host()
    });
    let snapshot = adapter.get_selection_page(0).unwrap();

    assert_eq!(snapshot.focus, None);
    assert_eq!(snapshot.focus_section, None);
}

/// フォーカス対象と区間番号を同じ参照区間の内側で読むことを確かめる。
///
/// 別の区間に分けると、間に利用者の操作が入って両者が食い違った組を返し得る。
#[test]
fn the_focus_and_its_section_are_read_in_the_same_section() {
    let adapter = adapter_with(|_| selecting_host());
    adapter.get_selection_page(0).unwrap();

    let calls = adapter.host.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| **call == "enter_read_section")
            .count(),
        1,
        "参照区間へ複数回入りました: {calls:?}"
    );
    let entered = calls
        .iter()
        .position(|call| *call == "enter_read_section")
        .expect("参照区間へ入っていません");
    for call in ["focused_object", "focus_section"] {
        let at = calls
            .iter()
            .position(|recorded| *recorded == call)
            .unwrap_or_else(|| panic!("{call} が呼ばれていません: {calls:?}"));
        assert!(
            at > entered,
            "{call} が参照区間の外で呼ばれました: {calls:?}"
        );
    }
}

/// シーンの guard が対象を読む前に効くことを確かめる。
#[test]
fn get_selection_rejects_a_different_scene_before_reading_objects() {
    let adapter = adapter_with(|_| selecting_host());
    let error = adapter.get_selection_page(7).unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["expected_scene_id"], 7);
    assert_eq!(identity_reads(&adapter), 0);
}

/// 選択の取得がページ間の revision 照合を行うことを確かめる。
///
/// 選択はプロジェクトの状態であり、revision に連動する。
#[test]
fn get_selection_rejects_a_stale_snapshot_revision() {
    let adapter = adapter_with(|_| selecting_host());
    let error = adapter
        .get_selection(0, &page_request(0, 50, Some(99)))
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

/// 選択が 0 件でもフォーカス対象は返ることを確かめる。
///
/// タイムライン上の選択とオブジェクト設定ウィンドウの選択は別物である。
#[test]
fn an_empty_selection_still_carries_the_focus() {
    let adapter = adapter_with(|_| FakeHost {
        selected: Vec::new(),
        ..selecting_host()
    });
    let snapshot = adapter.get_selection_page(0).unwrap();

    assert!(snapshot.selected.is_empty());
    assert_eq!(snapshot.page.total_count, 0);
    assert!(snapshot.focus.is_some());
}

/// 選択の応答が持つ項目を表として固定する。
///
/// ハンドルは参照区間の内側で位置と同一性の材料へ写し切る。名前で探す検査は
/// `handle` を含まない名前を付けた項目を見逃すため、項目の集合そのものを
/// 固定して、区間の外へ持ち出す値が増えたことをここで落とす。
#[test]
fn the_selection_response_carries_only_position_and_identity() {
    let adapter = adapter_with(|_| selecting_host());
    let snapshot = adapter.get_selection_page(0).unwrap();
    let value = serde_json::to_value(&snapshot).expect("直列化できます");

    let expected: std::collections::BTreeSet<String> = [
        "project_revision",
        "focus",
        "focus_section",
        "selected",
        "page",
        "layer",
        "frame_start",
        "frame_end",
        "name",
        "selector",
        "fingerprint",
        "project_epoch",
        "scene_id",
        "frame",
        "total_count",
        "count",
        "offset",
        "has_more",
        "next_offset",
        "snapshot_revision",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    assert_eq!(field_names(&value), expected);
}

/// 選択が返したセレクターで対象を引けることを確かめる。
///
/// 「対話利用の起点」の中身である。返ってきた対象をそのまま編集へ渡せる。
#[test]
fn the_selection_returns_usable_selectors() {
    let adapter = adapter_with(|_| selecting_host());
    let snapshot = adapter.get_selection_page(0).unwrap();

    let focus = snapshot.focus.expect("フォーカス対象がありません");
    let detail = adapter
        .get_object(&focus.selector)
        .expect("フォーカス対象のセレクターで引けません");
    assert_eq!(detail.summary, focus);

    for object in &snapshot.selected {
        adapter
            .get_object(&object.selector)
            .expect("選択のセレクターで引けません");
    }
}
