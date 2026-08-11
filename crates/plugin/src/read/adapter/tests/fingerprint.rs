//! 対象と effect の fingerprint の統合テスト。

use super::*;

/// 対象が動くと、読み取り口が返す fingerprint も変わることを確かめる。
///
/// epoch を共有させるため、プロジェクト状態は 2 つの adapter で共用する。
/// これにより差分は対象の位置だけになる。
#[test]
fn fingerprint_from_the_adapter_changes_when_the_target_moves() {
    let project = Arc::new(ProjectState::new());
    let before = HostReadAdapter::new(FakeHost::new(), Arc::clone(&project));
    let after = HostReadAdapter::new(
        FakeHost {
            layers: {
                let mut layers = fake_layers();
                layers[1].objects[0] = object(1, 105, 205, Some("立ち絵"));
                layers
            },
            ..FakeHost::new()
        },
        Arc::clone(&project),
    );

    let fingerprint_of = |adapter: &HostReadAdapter<FakeHost>, frame_start: usize| {
        adapter
            .list_objects_page(0, None)
            .unwrap()
            .items
            .into_iter()
            .find(|item| item.layer == 1 && item.frame_start == frame_start)
            .unwrap_or_else(|| panic!("開始フレーム {frame_start} の対象がありません"))
            .selector
            .fingerprint
    };

    assert_ne!(
        fingerprint_of(&before, 100),
        fingerprint_of(&after, 105),
        "対象が動いても fingerprint が変わりません"
    );
}

/// 一覧から算出した fingerprint と、詳細から算出した fingerprint が一致する
/// ことを確かめる。
///
/// 食い違えば、一覧が返したセレクターで詳細を引けなくなり、対象が事実上
/// 到達不能になる。
#[test]
fn object_fingerprint_agrees_between_listing_and_detail() {
    let adapter = adapter();
    let summaries = adapter.list_objects_page(0, None).unwrap().items;
    assert!(!summaries.is_empty());

    for summary in summaries {
        let detail = adapter
            .get_object(&summary.selector)
            .expect("一覧が返したセレクターで詳細を引けません");
        assert_eq!(
            detail.summary.selector.fingerprint,
            summary.selector.fingerprint
        );
        assert_eq!(detail.summary.selector, summary.selector);
    }
}

/// 配下 effect を持つ対象でも両経路が一致することを確かめる。
///
/// 一方が effect を読み、他方が読まない経路を通るため、材料が食い違えば
/// ここで落ちる。
#[test]
fn object_fingerprint_agrees_for_an_object_that_has_effects() {
    let adapter = adapter();
    let summary = adapter
        .list_objects_page(0, None)
        .unwrap()
        .items
        .into_iter()
        .find(|item| item.layer == 1 && item.frame_start == 100)
        .expect("配下 effect を持つ対象がありません");

    let detail = adapter.get_object(&summary.selector).unwrap();
    assert!(!detail.effects.is_empty());
    assert_eq!(
        detail.summary.selector.fingerprint,
        summary.selector.fingerprint
    );
}

/// effect の設定を変えると、そのオブジェクトの fingerprint も変わることを
/// 確かめる。
///
/// 変わらなければ、effect を書き換えた後も変更前のセレクターが一致し続け、
/// 古いセレクターでの変更を拒否できない。材料に effect の列は無く、alias が
/// 配下 effect の設定値を含むことだけがこの性質を支えている。列挙は effect を
/// 読まないため、ここで変わるのは alias 経由でしかあり得ない。
///
/// epoch を揃えるためプロジェクト状態は共用し、差分を effect の設定だけに
/// する。
#[test]
fn object_fingerprint_changes_when_an_effect_setting_changes() {
    let project = Arc::new(ProjectState::new());
    let fingerprint_of = |path: &'static str| {
        let adapter = HostReadAdapter::new(
            host_with_effects(vec![file_effect("動画ファイル", 0, path)]),
            Arc::clone(&project),
        );
        let fingerprint = listed_sample(&adapter).selector.fingerprint;
        assert!(
            !adapter.host.calls().contains(&EFFECT_LIST),
            "列挙が effect を読みました: {:?}",
            adapter.host.calls()
        );
        fingerprint
    };

    assert_ne!(
        fingerprint_of(r"C:\movie.mp4"),
        fingerprint_of(r"C:\another.mp4"),
        "effect の設定を変えても fingerprint が変わりません"
    );
}

/// effect のロック状態を変えると、そのオブジェクトの fingerprint も変わる
/// ことを確かめる。
///
/// ロックは alias の節へ書き出される。設定値と同じく、列挙が effect を
/// 読まないままオブジェクトの同一性へ伝わる。
#[test]
fn object_fingerprint_changes_when_an_effect_lock_changes() {
    let project = Arc::new(ProjectState::new());
    let fingerprint_of = |locked: bool| {
        let effect = HostEffect {
            locked,
            ..file_effect("動画ファイル", 0, r"C:\movie.mp4")
        };
        let adapter = HostReadAdapter::new(host_with_effects(vec![effect]), Arc::clone(&project));
        let fingerprint = listed_sample(&adapter).selector.fingerprint;
        assert!(
            !adapter.host.calls().contains(&EFFECT_LIST),
            "列挙が effect を読みました: {:?}",
            adapter.host.calls()
        );
        fingerprint
    };

    assert_ne!(
        fingerprint_of(false),
        fingerprint_of(true),
        "effect のロックを変えても fingerprint が変わりません"
    );
}

/// effect の有効状態を変えると、そのオブジェクトの fingerprint も変わる
/// ことを確かめる。
///
/// 有効状態は alias の節へ書き出される。設定値やロックと同じく、列挙が
/// effect を読まないままオブジェクトの同一性へ伝わる。
#[test]
fn object_fingerprint_changes_when_an_effect_enabled_changes() {
    let project = Arc::new(ProjectState::new());
    let fingerprint_of = |enabled: bool| {
        let effect = HostEffect {
            enabled,
            ..file_effect("動画ファイル", 0, r"C:\movie.mp4")
        };
        let adapter = HostReadAdapter::new(host_with_effects(vec![effect]), Arc::clone(&project));
        let fingerprint = listed_sample(&adapter).selector.fingerprint;
        assert!(
            !adapter.host.calls().contains(&EFFECT_LIST),
            "列挙が effect を読みました: {:?}",
            adapter.host.calls()
        );
        fingerprint
    };

    assert_ne!(
        fingerprint_of(true),
        fingerprint_of(false),
        "effect の有効状態を変えても fingerprint が変わりません"
    );
}

/// 配下 effect が読めなくても対象の fingerprint が変わらないことを確かめる。
///
/// effect の一覧は 0 件と取得失敗を区別しない。推定が同一性の材料に入って
/// いれば、一過性の失敗で fingerprint が揺れ、直前に返したセレクターが
/// 拒否される。
#[test]
fn a_failing_effect_read_does_not_shift_the_object_fingerprint() {
    let project = Arc::new(ProjectState::new());
    let healthy = HostReadAdapter::new(FakeHost::new(), Arc::clone(&project));
    let failing = HostReadAdapter::new(
        FakeHost {
            effects_fail_at: Some(100),
            ..FakeHost::new()
        },
        Arc::clone(&project),
    );

    assert_eq!(
        listed_sample(&healthy).selector.fingerprint,
        listed_sample(&failing).selector.fingerprint,
        "effect の読み取り失敗で fingerprint が揺れました"
    );
}

/// 同名 effect が繰り上がった場合に、残った effect が別物として扱われる
/// ことを確かめる。
///
/// 名前と同名内の番号だけを材料にすると、繰り上がった側が取り除く前の
/// 先頭と同じ fingerprint になり、別のインスタンスへ変更が当たる。
#[test]
fn effect_fingerprint_changes_when_the_preceding_effect_is_removed() {
    let adapter_for = |effects: Vec<HostEffect>| adapter_with(|_| host_with_effects(effects));
    let fingerprints_of = |adapter: &HostReadAdapter<FakeHost>| {
        let summary = listed_sample(adapter);
        adapter
            .get_object(&summary.selector)
            .unwrap()
            .effects
            .into_iter()
            .map(|effect| effect.selector.fingerprint)
            .collect::<Vec<_>>()
    };

    // 同じ設定の同名 effect が 2 つ並ぶ。
    let before = adapter_for(vec![
        file_effect("ぼかし", 0, r"C:\a.png"),
        file_effect("ぼかし", 1, r"C:\a.png"),
    ]);
    // 前方の 1 つが取り除かれ、残った側の番号が 0 へ繰り上がる。
    let after = adapter_for(vec![file_effect("ぼかし", 0, r"C:\a.png")]);

    assert_ne!(
        fingerprints_of(&before)[0],
        fingerprints_of(&after)[0],
        "繰り上がった effect が取り除く前の先頭と同じ値になりました"
    );
}

#[test]
fn selector_fingerprint_is_canonical() {
    let adapter = adapter();
    let selector = sample_selector(&adapter);
    let parsed: Fingerprint = selector.fingerprint.as_str().parse().unwrap();
    assert_eq!(parsed, selector.fingerprint);
}
