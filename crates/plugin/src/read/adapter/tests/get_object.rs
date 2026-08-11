//! オブジェクト取得の統合テスト。

use super::*;

/// 対象を指定する取得では、不在がそのまま不在として返ることを確かめる。
///
/// 列挙側の畳み込みが、対象を指定する経路まで巻き込んではならない。
#[test]
fn get_object_reports_not_found_when_the_target_vanished() {
    let adapter = adapter_with(|_| FakeHost {
        object_missing_at: Some(100),
        ..FakeHost::new()
    });
    let selector = sample_selector(&adapter);

    assert_eq!(
        adapter.get_object(&selector).unwrap_err().error_code(),
        ErrorCode::NotFound
    );
}

#[test]
fn get_object_returns_detail_for_matching_selector() {
    let adapter = adapter();
    let selector = sample_selector(&adapter);
    let detail = adapter.get_object(&selector).unwrap();

    assert_eq!(detail.summary.layer, 1);
    assert_eq!(detail.summary.frame_start, 100);
    // alias は配下 effect の設定値を含むため、位置だけの表記では終わらない。
    assert!(detail.alias.starts_with("[1:100]"), "{}", detail.alias);
    assert!(
        detail.alias.contains("動画ファイル"),
        "alias に配下 effect が現れません: {}",
        detail.alias
    );
    assert_eq!(detail.sections.len(), 1);
    assert_eq!(detail.effects.len(), 1);
    assert_eq!(detail.effects[0].name, "動画ファイル");
    assert_eq!(detail.effects[0].selector.object, detail.summary.selector);
}

#[test]
fn get_object_gives_each_effect_its_position_in_the_column() {
    // 3 件のうち 2 件を同名にする。同名内の順序と列の位置が食い違う要素が
    // 無ければ、両者を取り違えた実装が通る。
    let adapter = adapter_with(|_| {
        host_with_effects(vec![
            file_effect("動画ファイル", 0, r"C:\movie.mp4"),
            file_effect("ぼかし", 0, r"C:\mask.png"),
            file_effect("ぼかし", 1, r"C:\mask2.png"),
        ])
    });
    let selector = listed_sample(&adapter).selector;
    let detail = adapter.get_object(&selector).expect("対象の詳細");

    let positions: Vec<usize> = detail
        .effects
        .iter()
        .map(|effect| effect.position)
        .collect();
    assert_eq!(positions, vec![0, 1, 2]);
    let indices: Vec<usize> = detail.effects.iter().map(|effect| effect.index).collect();
    assert_eq!(indices, vec![0, 0, 1]);
}

#[test]
fn get_object_returns_a_movement_as_a_movement() {
    // 移動を持つ項目は区間ごとの値と移動方法を運び、移動を持たない項目は
    // 1 つの数値のままである。同じ種別の中で分かれるため、応答の形を
    // 種別だけで決めていれば片方が食い違う。
    let adapter = mixed_adapter();
    let selector = listed_sample(&adapter).selector;
    let detail = adapter.get_object(&selector).expect("対象の詳細");
    let items = &detail.effects[0].items;
    let value = |name: &str| {
        items
            .iter()
            .find(|item| item.name == name)
            .unwrap_or_else(|| panic!("設定項目 {name} がありません"))
            .value
            .clone()
    };

    let ItemValue::Track(track) = value("X") else {
        panic!("移動を持つ項目が移動として返りません: {:?}", value("X"));
    };
    assert_eq!(track.mode.as_deref(), Some(MOVEMENT_MODE));
    assert_eq!(track.values.len(), 2);
    assert!(
        matches!(value("拡大率"), ItemValue::Number { .. }),
        "移動を持たない項目まで移動として返りました: {:?}",
        value("拡大率")
    );
}

/// 候補の絞り込みが、候補以外の詳細を読まずに済むことを確かめる。
#[test]
fn get_object_reads_the_detail_of_the_candidate_only() {
    let adapter = adapter();
    let selector = sample_selector(&adapter);
    adapter.get_object(&selector).unwrap();

    assert_eq!(detail_reads(&adapter), 1, "候補以外の詳細まで読んでいます");
    assert_eq!(
        identity_reads(&adapter),
        0,
        "詳細と同一性の材料を二重に読んでいます"
    );
}

/// 同じレイヤーにある無関係な対象が読めなくても、対象の取得が成功することを
/// 確かめる。
///
/// 候補の絞り込みでレイヤー内の全対象の alias を読むと、無関係な対象の不調が
/// 対象の取得を巻き込んで失敗させる。
#[test]
fn get_object_is_unaffected_by_a_failing_sibling() {
    // レイヤー 1 には開始フレーム 100 と 300 の対象がある。300 の読み取りだけを
    // 失敗させ、100 を取得する。
    let adapter = adapter_with(|_| FakeHost {
        object_read_fails_at: Some(300),
        ..FakeHost::new()
    });
    let selector = sample_selector(&adapter);

    let detail = adapter
        .get_object(&selector)
        .expect("同じレイヤーの別対象の失敗に巻き込まれました");
    assert_eq!(detail.summary.frame_start, 100);
}

#[test]
fn get_object_matches_start_frame_exactly() {
    // 開始フレーム以降の探索を流用していると、範囲内のフレームでも
    // 同じオブジェクトが解決されてしまう。
    let adapter = adapter();
    let mut selector = sample_selector(&adapter);
    selector.frame = 150;

    let error = adapter.get_object(&selector).unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::NotFound);
}

#[test]
fn get_object_reports_not_found_when_no_candidate() {
    let adapter = adapter();
    let mut selector = sample_selector(&adapter);
    selector.frame = 1000;
    assert_eq!(
        adapter.get_object(&selector).unwrap_err().error_code(),
        ErrorCode::NotFound
    );
}

/// 名前が変わった対象が、一致する対象なしではなく内容の食い違いとして
/// 返ることを確かめる。
///
/// 名前で候補を絞ると、この状況は候補 0 件になり「再試行しても解消しない」
/// として返る。実際には読み直せば要求を作り直せる。
#[test]
fn get_object_reports_precondition_failed_after_the_target_is_renamed() {
    let project = Arc::new(ProjectState::new());
    let before = HostReadAdapter::new(FakeHost::new(), Arc::clone(&project));
    let selector = sample_selector(&before);

    let renamed = HostReadAdapter::new(
        FakeHost {
            layers: {
                let mut layers = fake_layers();
                layers[1].objects[0] =
                    object_with_effects(1, 100, 200, Some("改名後"), fake_effects());
                layers
            },
            ..FakeHost::new()
        },
        Arc::clone(&project),
    );

    let error = renamed.get_object(&selector).unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert!(
        matches!(error, ReadError::FingerprintMismatch { .. }),
        "{error} が内容の食い違いとして返っていません"
    );
}

/// 食い違いの応答が返したセレクターで読み直せることを確かめる。
///
/// 現在の姿を返さなければ、要求元は列挙まで戻って対象を探し直すほかない。
#[test]
fn a_content_mismatch_returns_a_selector_that_resolves() {
    let project = Arc::new(ProjectState::new());
    let stale = HostReadAdapter::new(FakeHost::new(), Arc::clone(&project));
    let selector = sample_selector(&stale);

    let adapter = HostReadAdapter::new(
        FakeHost {
            layers: {
                let mut layers = fake_layers();
                layers[1].objects[0] =
                    object_with_effects(1, 100, 200, Some("改名後"), fake_effects());
                layers
            },
            ..FakeHost::new()
        },
        Arc::clone(&project),
    );

    let ReadError::FingerprintMismatch { current_object } =
        adapter.get_object(&selector).unwrap_err()
    else {
        panic!("内容の食い違いとして返っていません");
    };
    assert_eq!(current_object.name.as_deref(), Some("改名後"));

    let detail = adapter
        .get_object(&current_object.selector)
        .expect("失敗が返したセレクターで読み直せません");
    assert_eq!(detail.summary, *current_object);
}

/// 名前を名乗らないセレクターでも対象が特定できることを確かめる。
#[test]
fn get_object_resolves_a_selector_without_a_name() {
    let adapter = adapter();
    let mut selector = sample_selector(&adapter);
    selector.name = None;

    let detail = adapter
        .get_object(&selector)
        .expect("名前を持たない指定が拒否されました");
    assert_eq!(detail.summary.frame_start, 100);
}

/// 名前だけが食い違うセレクターが、位置と内容で解決されることを確かめる。
///
/// 名前は fingerprint の材料であり、対象が実際に改名されていれば
/// fingerprint が捕まえる。セレクターの名前欄そのものは絞り込みに使わない。
#[test]
fn get_object_ignores_the_name_carried_by_the_selector() {
    let adapter = adapter();
    let mut selector = sample_selector(&adapter);
    selector.name = Some("別の名前".to_string());

    let detail = adapter
        .get_object(&selector)
        .expect("名前の食い違いで対象を見失いました");
    assert_eq!(detail.summary.name.as_deref(), Some("立ち絵"));
}

#[test]
fn get_object_reports_ambiguous_selector_for_multiple_candidates() {
    let adapter = adapter_with(|_| {
        let mut layers = fake_layers();
        // 同じ開始フレームの候補を 2 件にする。
        layers[1].objects = vec![
            object(1, 100, 200, Some("立ち絵")),
            object(1, 100, 250, Some("立ち絵")),
        ];
        FakeHost {
            layers,
            ..FakeHost::new()
        }
    });
    let selector = sample_selector(&adapter);

    let error = adapter.get_object(&selector).unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::AmbiguousSelector);
    assert_eq!(error.details()["candidate_count"], 2);
}

#[test]
fn get_object_reports_precondition_failed_for_fingerprint_mismatch() {
    let adapter = adapter_with(|_| {
        let mut layers = fake_layers();
        // 位置と名前は同じまま alias だけ変える。
        layers[1].objects[0].identity.alias = "[changed]".to_string();
        FakeHost {
            layers,
            ..FakeHost::new()
        }
    });
    let selector = sample_selector(&adapter);

    assert_eq!(
        adapter.get_object(&selector).unwrap_err().error_code(),
        ErrorCode::PreconditionFailed
    );
}

#[test]
fn get_object_reports_precondition_failed_for_scene_mismatch() {
    let adapter = adapter();
    let mut selector = sample_selector(&adapter);
    selector.scene_id = 5;

    let error = adapter.get_object(&selector).unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["expected_scene_id"], 5);
}

#[test]
fn get_object_reports_precondition_failed_for_epoch_mismatch() {
    let adapter = adapter();
    let mut selector = sample_selector(&adapter);
    selector.project_epoch = "00000000-0000-0000-0000-000000000000".to_string();

    let error = adapter.get_object(&selector).unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert!(
        !adapter.host.calls().contains(&"enter_read_section"),
        "epoch 不一致で参照区間へ入りました"
    );
}

#[test]
fn a_selector_carrying_a_tampered_fingerprint_is_rejected() {
    // 要求は算出方式を運ばない。方式が変われば digest も変わるため、対象の
    // 同一性は fingerprint の照合だけで守られる。
    let adapter = adapter();
    let mut selector = sample_selector(&adapter);
    selector.fingerprint = format!("sha256:{}", "0".repeat(64))
        .parse()
        .expect("差し替えた fingerprint の書式");

    let error = adapter.get_object(&selector).unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert!(
        matches!(error, ReadError::FingerprintMismatch { .. }),
        "{error} が fingerprint の食い違いとして返っていません"
    );
}

/// 対象が動いた後のセレクターが、一致する対象なしとして拒否されることを確かめる。
#[test]
fn get_object_reports_not_found_after_the_target_moved() {
    let adapter = adapter_with(|_| {
        // レイヤー 1 の対象が開始フレーム 100 から 105 へ動く。
        let mut layers = fake_layers();
        layers[1].objects[0] = object(1, 105, 205, Some("立ち絵"));
        FakeHost {
            layers,
            ..FakeHost::new()
        }
    });
    let selector = sample_selector(&adapter);

    assert_eq!(
        adapter.get_object(&selector).unwrap_err().error_code(),
        ErrorCode::NotFound
    );
}

/// 移動先へ別の対象が居座った場合に、fingerprint の照合で拒否されることを
/// 確かめる。
///
/// 位置だけで対象を決めていると、旧セレクターが別の対象へ解決されてしまう。
#[test]
fn get_object_reports_precondition_failed_when_another_object_took_the_place() {
    let adapter = adapter_with(|_| {
        let mut layers = fake_layers();
        // 元の対象は動き、空いた位置に同名で別内容の対象が入る。
        layers[1].objects[0] = object(1, 105, 205, Some("立ち絵"));
        let mut intruder = object(1, 100, 150, Some("立ち絵"));
        intruder.identity.alias = "[1:100]#2".to_string();
        layers[1].objects.push(intruder);
        FakeHost {
            layers,
            ..FakeHost::new()
        }
    });
    let selector = sample_selector(&adapter);

    assert_eq!(
        adapter.get_object(&selector).unwrap_err().error_code(),
        ErrorCode::PreconditionFailed
    );
}

/// プロジェクトを開き直すと、旧セレクターが拒否されることを確かめる。
///
/// epoch の再発行はプロジェクト境界そのものであり、それ以前に得た
/// セレクターは参照区間へ入る前に拒否される。
#[test]
fn get_object_is_rejected_after_the_project_is_reopened() {
    let adapter = adapter();
    let selector = sample_selector(&adapter);
    adapter
        .get_object(&selector)
        .expect("開き直す前のセレクターが解決できません");
    let entered = section_entries(&adapter);

    adapter
        .project
        .on_project_load(Some(r"C:\projects\reopened.aup2"));

    let error = adapter.get_object(&selector).unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(
        section_entries(&adapter),
        entered,
        "epoch を再発行した後のセレクターで参照区間へ入りました"
    );
}

/// 対象を指定する取得は配下 effect を読むことを確かめる。
///
/// 応答が effect の一覧を返すため、読まなければ組み立てられない。
#[test]
fn get_object_reads_effects() {
    let adapter = adapter();
    let selector = sample_selector(&adapter);
    let detail = adapter.get_object(&selector).unwrap();

    assert!(!detail.effects.is_empty());
    assert!(
        adapter.host.calls().contains(&EFFECT_LIST),
        "effect を読まずに一覧を返しました: {:?}",
        adapter.host.calls()
    );
}
