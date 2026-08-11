//! シーン設定変更の統合テスト。

use super::*;

/// シーン設定の変更要求を組み立てる。
///
/// 3 つの軸はいずれも省略した状態から始める。軸ごとの検査は必要なものだけを
/// 立てて行う——全省略は要求の検証が弾くため、ここへは届かない。
fn set_scene_settings(harness: &Harness) -> SetSceneSettingsParams {
    SetSceneSettingsParams {
        expected_scene_id: SCENE_ID,
        name: None,
        size: None,
        sample_rate: None,
        expected_project_epoch: harness.epoch(),
    }
}

/// シーン設定の変更要求を 1 つ組み立てる手続き。
type SceneRequest = fn(&Harness) -> SetSceneSettingsParams;

/// シーン設定の 3 つの setter が呼ばれた回数を、名前・解像度・サンプリング
/// レートの順で数える。
///
/// **成否ではなく回数を数える。** 成否だけを見ると、名前が反映されなかった
/// ときに残る 2 つを発行してしまう実装でも通ってしまう。
fn scene_setter_calls(harness: &Harness) -> [usize; 3] {
    let calls = harness.host.calls();
    ["set_scene_name", "set_scene_size", "set_scene_sample_rate"]
        .map(|setter| calls.iter().filter(|call| **call == setter).count())
}

#[test]
fn changing_every_scene_axis_reports_a_change_that_cannot_be_undone() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_scene_settings(&SetSceneSettingsParams {
            name: Some("本編".to_string()),
            size: Some(SceneSize {
                width: 1280,
                height: 720,
            }),
            sample_rate: Some(44_100),
            ..set_scene_settings(&harness)
        })
        .expect("シーン設定の変更に失敗しました");

    assert_eq!(outcome.scene.id, SCENE_ID);
    assert_eq!(outcome.scene.name.as_deref(), Some("本編"));
    assert_eq!(outcome.scene.width, 1280);
    assert_eq!(outcome.scene.height, 720);
    assert_eq!(outcome.scene.sample_rate, 44_100);
    // 解像度とサンプリングレートは区間を抜けてから観測する。
    assert!(outcome.observed_after_edit);
    // AviUtl2 の取り消し操作ではシーン設定は元へ戻らない。
    assert!(outcome.non_undoable);
    assert_eq!(outcome.project_epoch, harness.epoch());
    assert_eq!(outcome.project_revision, 1);
    assert_eq!(harness.project.revision(), 1);
    assert!(harness.project.modified());
    assert_eq!(scene_setter_calls(&harness), [1, 1, 1]);
}

#[test]
fn each_scene_axis_can_be_set_on_its_own() {
    // 軸ごとに、その軸の setter だけが呼ばれる。まとめて発行する実装では、
    // 要求していない軸が現在値で上書きされる。
    let cases: [(&str, SceneRequest, [usize; 3]); 3] = [
        (
            "name",
            |harness| SetSceneSettingsParams {
                name: Some("本編".to_string()),
                ..set_scene_settings(harness)
            },
            [1, 0, 0],
        ),
        (
            "size",
            |harness| SetSceneSettingsParams {
                size: Some(SceneSize {
                    width: 1280,
                    height: 720,
                }),
                ..set_scene_settings(harness)
            },
            [0, 1, 0],
        ),
        (
            "sample_rate",
            |harness| SetSceneSettingsParams {
                sample_rate: Some(44_100),
                ..set_scene_settings(harness)
            },
            [0, 0, 1],
        ),
    ];

    for (label, build, expected) in cases {
        let harness = Harness::new();
        let outcome = harness
            .edit
            .set_scene_settings(&build(&harness))
            .unwrap_or_else(|error| panic!("{label} だけの変更が失敗しました: {error}"));

        assert_eq!(scene_setter_calls(&harness), expected, "{label}");
        assert_eq!(outcome.project_revision, 1, "{label}");
    }
}

#[test]
fn a_scene_name_that_did_not_take_effect_stops_before_the_other_axes() {
    // 名前の照合は区間の内側で完結する。反映されていなければ、取り消せない
    // 変更を 1 つも増やさずに戻る。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreSceneName)));
    let error = harness
        .edit
        .set_scene_settings(&SetSceneSettingsParams {
            name: Some("本編".to_string()),
            size: Some(SceneSize {
                width: 1280,
                height: 720,
            }),
            sample_rate: Some(44_100),
            ..set_scene_settings(&harness)
        })
        .expect_err("反映されていない名前が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("change_not_applied"));
    // 名前の setter は SDK へ届いている。届いた以上は変更が入った側へ倒す。
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(
        scene_setter_calls(&harness),
        [1, 0, 0],
        "名前が反映されないまま残りの軸を発行しました"
    );
    // シーンは 3 軸とも元のままである。
    let scene = harness.host.scene();
    assert_eq!(scene.name, SCENE_NAME);
    assert_eq!(scene.width, 1920);
    assert_eq!(scene.height, 1080);
    assert_eq!(scene.sample_rate, 48_000);
}

#[test]
fn a_host_that_adjusts_the_scene_settings_is_not_a_failure() {
    // 反映値は区間を抜けてから観測する。ホストが調整し得るうえ、観測までの間に
    // UI 操作も入り得る。差異を失敗にすると、成功した変更が失敗として返る。
    //
    // **解像度とサンプリングレートの両方で確かめる。** 片方だけを見ると、もう
    // 一方が要求値をそのまま返していても通ってしまう。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::ClampSceneSettings)));
    let requested = SceneSize {
        width: 3840,
        height: 2160,
    };
    let requested_sample_rate = 192_000;
    let outcome = harness
        .edit
        .set_scene_settings(&SetSceneSettingsParams {
            size: Some(requested),
            sample_rate: Some(requested_sample_rate),
            ..set_scene_settings(&harness)
        })
        .expect("要求値との差異が失敗として返りました");

    // 応答が載せるのは観測値である。
    assert_eq!(outcome.scene.width, MAX_SCENE_WIDTH);
    assert_eq!(outcome.scene.height, MAX_SCENE_HEIGHT);
    assert_eq!(outcome.scene.sample_rate, MAX_SCENE_SAMPLE_RATE);
    assert_ne!(
        outcome.scene.width, requested.width,
        "フェイクが解像度を調整していません"
    );
    assert_ne!(
        outcome.scene.height, requested.height,
        "フェイクが解像度を調整していません"
    );
    assert_ne!(
        outcome.scene.sample_rate, requested_sample_rate,
        "フェイクがサンプリングレートを調整していません"
    );
    assert!(outcome.observed_after_edit);
    assert_eq!(outcome.project_revision, 1);
}

#[test]
fn a_scene_renamed_after_the_section_is_reported_as_observed() {
    // 名前の照合は区間の内側で通る。そのうえで、区間を抜けてから観測するまでの
    // 間に UI が名前を付け直す状況を作る。差異は失敗ではなく、応答が載せるのは
    // 観測した名前である——要求値をそのまま返す実装ではここが食い違う。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::RenameSceneAfterSection)));
    let requested = "本編";
    let outcome = harness
        .edit
        .set_scene_settings(&SetSceneSettingsParams {
            name: Some(requested.to_string()),
            ..set_scene_settings(&harness)
        })
        .expect("観測との差異が失敗として返りました");

    assert_eq!(outcome.scene.name.as_deref(), Some(RENAMED_SCENE_NAME));
    assert_ne!(
        outcome.scene.name.as_deref(),
        Some(requested),
        "応答が要求値をそのまま返しました"
    );
    assert!(outcome.observed_after_edit);
    assert_eq!(outcome.project_revision, 1);
    // 観測は区間を抜けた後にある。区間の内側で応答を組み立てていれば、記録の
    // 最後は setter になる。
    assert_eq!(
        harness.host.calls().last(),
        Some(&OBSERVED_SCENE),
        "シーンの観測が区間を抜けた後に行われていません: {:?}",
        harness.host.calls()
    );
}

#[test]
fn a_mismatched_scene_precondition_never_reaches_the_scene_setters() {
    let cases: [(&str, SceneRequest, &str); 2] = [
        (
            "expected_project_epoch",
            |harness| SetSceneSettingsParams {
                expected_project_epoch: "00000000-0000-4000-8000-000000000000".to_string(),
                name: Some("本編".to_string()),
                ..set_scene_settings(harness)
            },
            "project_epoch",
        ),
        (
            "expected_scene_id",
            |harness| SetSceneSettingsParams {
                expected_scene_id: SCENE_ID + 1,
                name: Some("本編".to_string()),
                ..set_scene_settings(harness)
            },
            "scene_id",
        ),
    ];

    for (label, build, mismatch) in cases {
        let harness = Harness::new();
        let error = harness
            .edit
            .set_scene_settings(&build(&harness))
            .err()
            .unwrap_or_else(|| panic!("{label} の不一致が受理されました"));

        assert_eq!(error.error_code(), ErrorCode::PreconditionFailed, "{label}");
        assert_eq!(error.details()["mismatch"], json!(mismatch), "{label}");
        // 取り消せない変更であるため、前提が崩れていれば 1 つも発行しない。
        assert_eq!(scene_setter_calls(&harness), [0, 0, 0], "{label}");
        harness.assert_untouched();
    }
}

#[test]
fn the_scene_settings_response_carries_no_alias_path_or_item_value() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_scene_settings(&SetSceneSettingsParams {
            name: Some("本編".to_string()),
            ..set_scene_settings(&harness)
        })
        .expect("シーン設定の変更に失敗しました");
    let value = serde_json::to_string(&outcome).expect("直列化できる");
    for secret in ["alias", "handle", "[1:100]", "0x", "C:\\"] {
        assert!(!value.contains(secret), "{secret} が応答に現れました");
    }
}
