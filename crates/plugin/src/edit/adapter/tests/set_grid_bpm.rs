//! BPM グリッド置き換えの統合テスト。

use super::*;

/// BPM グリッドの置き換え要求を組み立てる。
fn set_grid_bpm(harness: &Harness, entries: Vec<GridBpm>) -> SetGridBpmParams {
    SetGridBpmParams {
        expected_scene_id: SCENE_ID,
        entries,
        expected_project_epoch: harness.epoch(),
    }
}

#[test]
fn replacing_the_grid_bpm_returns_the_list_read_back() {
    let harness = Harness::new();
    let entries = vec![grid_bpm(140.0, 3, 0.0, 0.25), grid_bpm(90.0, 4, 12.5, 0.0)];
    let outcome = harness
        .edit
        .set_grid_bpm(&set_grid_bpm(&harness, entries.clone()))
        .expect("BPM グリッドの置き換えに失敗しました");

    assert_eq!(outcome.entries, entries);
    assert_eq!(outcome.project_epoch, harness.epoch());
    assert_eq!(outcome.project_revision, 1);
    assert!(harness.project.modified());
}

#[test]
fn an_empty_grid_bpm_list_clears_the_grid() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_grid_bpm(&set_grid_bpm(&harness, Vec::new()))
        .expect("0 件の一覧が拒否されました");
    assert!(outcome.entries.is_empty());
}

#[test]
fn a_descending_grid_bpm_list_is_accepted_by_the_edit_path() {
    // 並べ替えはホストの仕事である。編集口が順序を要求すると、要求元は
    // read-back の順序と要求の順序の食い違いを説明できなくなる。
    let harness = Harness::new();
    let entries = vec![
        grid_bpm(120.0, 4, 30.0, 0.0),
        grid_bpm(120.0, 4, 20.0, 0.0),
        grid_bpm(120.0, 4, 10.0, 0.0),
    ];
    let outcome = harness
        .edit
        .set_grid_bpm(&set_grid_bpm(&harness, entries.clone()))
        .expect("降順の一覧が拒否されました");
    assert_eq!(outcome.entries, entries);
}

#[test]
fn a_grid_bpm_list_at_the_limit_reaches_the_host() {
    let harness = Harness::new();
    let entries = (0..MAX_GRID_BPM_ENTRIES)
        .map(|index| grid_bpm(120.0, 4, index as f64, 0.0))
        .collect::<Vec<_>>();
    let outcome = harness
        .edit
        .set_grid_bpm(&set_grid_bpm(&harness, entries))
        .expect("上限ちょうどの一覧が拒否されました");
    assert_eq!(outcome.entries.len(), MAX_GRID_BPM_ENTRIES);
}

#[test]
fn a_silently_ignored_grid_bpm_replacement_is_not_reported_as_success() {
    // 置き換えの API は戻り値を持たない。件数の照合だけが「送ったのに入って
    // いない」を捕まえる。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreGridBpm)));
    let error = harness
        .edit
        .set_grid_bpm(&set_grid_bpm(
            &harness,
            vec![grid_bpm(140.0, 3, 0.0, 0.0), grid_bpm(90.0, 4, 12.5, 0.0)],
        ))
        .expect_err("無視された置き換えが成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("change_not_applied"));
}

#[test]
fn a_host_that_rewrites_the_grid_bpm_values_is_not_a_failure() {
    // ホストは単精度で受け取り、並べ替えもする。値を照合する実装に戻すと、
    // 正常な正規化を失敗として報告するようになる。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::RewriteGridBpmValues)));
    let entries = vec![grid_bpm(140.0, 3, 0.0, 0.25), grid_bpm(90.0, 4, 12.5, 0.0)];
    let outcome = harness
        .edit
        .set_grid_bpm(&set_grid_bpm(&harness, entries.clone()))
        .expect("値の違いが失敗として返りました");

    assert_eq!(outcome.entries.len(), entries.len());
    assert_ne!(outcome.entries, entries, "フェイクが値を変えていません");
}
