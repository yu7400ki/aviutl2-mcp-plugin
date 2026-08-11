//! レイヤー状態変更の統合テスト。

use super::*;

/// 何も変えないレイヤーの状態変更要求を組み立てる。
fn layer_state_params(harness: &Harness, layer: u32) -> SetLayerStateParams {
    SetLayerStateParams {
        expected_scene_id: SCENE_ID,
        layer,
        name: None,
        enabled: None,
        locked: None,
        expected_project_epoch: harness.epoch(),
    }
}

#[test]
fn the_three_layer_axes_can_be_set_alone_or_together() {
    // 軸ごとに、要求した軸だけが変わり、他の軸は元のままであること。
    let cases: [(&str, SetLayerStateParams, Option<&str>, bool, bool); 4] = {
        let harness = Harness::new();
        [
            (
                "name",
                SetLayerStateParams {
                    name: Some(LayerNameChange::Set {
                        name: "背景".to_string(),
                    }),
                    ..layer_state_params(&harness, 0)
                },
                Some("背景"),
                true,
                false,
            ),
            (
                "enabled",
                SetLayerStateParams {
                    enabled: Some(false),
                    ..layer_state_params(&harness, 0)
                },
                None,
                false,
                false,
            ),
            (
                "locked",
                SetLayerStateParams {
                    locked: Some(true),
                    ..layer_state_params(&harness, 0)
                },
                None,
                true,
                true,
            ),
            (
                "全て",
                SetLayerStateParams {
                    name: Some(LayerNameChange::Set {
                        name: "背景".to_string(),
                    }),
                    enabled: Some(false),
                    locked: Some(true),
                    ..layer_state_params(&harness, 0)
                },
                Some("背景"),
                false,
                true,
            ),
        ]
    };

    for (label, params, name, enabled, locked) in cases {
        let harness = Harness::new();
        let params = SetLayerStateParams {
            expected_project_epoch: harness.epoch(),
            ..params
        };
        let outcome = harness
            .edit
            .set_layer_state(&params)
            .unwrap_or_else(|error| panic!("{label} の変更が失敗しました: {error}"));

        assert_eq!(outcome.layer.index, 0, "{label}");
        assert_eq!(outcome.layer.name.as_deref(), name, "{label}");
        assert_eq!(outcome.layer.enabled, enabled, "{label}");
        assert_eq!(outcome.layer.locked, locked, "{label}");
        // 応答は読み取りの DTO をそのまま返すため、件数も載る。
        assert_eq!(outcome.layer.object_count, 1, "{label}");
        assert_eq!(outcome.project_epoch, harness.epoch(), "{label}");
        assert_eq!(outcome.project_revision, 1, "{label}");
        assert_eq!(harness.project.revision(), 1, "{label}");
        assert!(harness.project.modified(), "{label}");
    }
}

#[test]
fn resetting_the_layer_name_hands_the_sdk_no_name() {
    let harness = Harness::new();
    harness
        .edit
        .set_layer_state(&SetLayerStateParams {
            name: Some(LayerNameChange::Set {
                name: "背景".to_string(),
            }),
            ..layer_state_params(&harness, 0)
        })
        .expect("名前を設定できません");
    assert_eq!(harness.host.scene().layers[0].name.as_deref(), Some("背景"));

    let outcome = harness
        .edit
        .set_layer_state(&SetLayerStateParams {
            name: Some(LayerNameChange::Reset {}),
            ..layer_state_params(&harness, 0)
        })
        .expect("標準名へ戻せません");

    assert_eq!(outcome.layer.name, None, "標準名へ戻っていません");
    assert_eq!(harness.host.scene().layers[0].name, None);
    // 標準名へ戻す指定は、空の名前ではなく「名前を渡さない」ことで表す。
    assert_eq!(
        harness.host.layer_name_arguments(),
        vec![Some("背景".to_string()), None],
        "標準名へ戻す指定が空の名前として渡りました"
    );
}

/// レイヤーの状態のうち、要求できる軸。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayerAxis {
    Name,
    Enabled,
    Locked,
}

impl LayerAxis {
    /// 全軸。
    ///
    /// 要素数と内容は `layer_axes_are_exhaustive` が固定する。
    const ALL: [LayerAxis; 3] = [LayerAxis::Name, LayerAxis::Enabled, LayerAxis::Locked];

    /// 記録に残す軸の名前。
    fn label(self) -> &'static str {
        match self {
            LayerAxis::Name => "name",
            LayerAxis::Enabled => "enabled",
            LayerAxis::Locked => "locked",
        }
    }

    /// この軸だけを、現在と異なる値へ変える要求を組み立てる。
    ///
    /// **網羅 match で書く。** 軸を足すとここがコンパイルエラーになるため、
    /// read-back の確認から漏れることがない。要求値は必ず現在値と異なる——
    /// 同じ値を要求すると、照合が働かなくても一致してしまう。
    fn request(self, params: SetLayerStateParams) -> SetLayerStateParams {
        match self {
            LayerAxis::Name => SetLayerStateParams {
                name: Some(LayerNameChange::Set {
                    name: "背景".to_string(),
                }),
                ..params
            },
            LayerAxis::Enabled => SetLayerStateParams {
                enabled: Some(false),
                ..params
            },
            LayerAxis::Locked => SetLayerStateParams {
                locked: Some(true),
                ..params
            },
        }
    }
}

#[test]
fn layer_axes_are_exhaustive() {
    // 網羅 match は軸の追加を止めるが、`ALL` は手書きである。両方を突き合わせる。
    fn assert_listed(axis: LayerAxis) {
        match axis {
            LayerAxis::Name | LayerAxis::Enabled | LayerAxis::Locked => {}
        }
        assert!(
            LayerAxis::ALL.contains(&axis),
            "{} が LayerAxis::ALL に含まれていません",
            axis.label()
        );
    }

    assert_listed(LayerAxis::Name);
    assert_listed(LayerAxis::Enabled);
    assert_listed(LayerAxis::Locked);
    assert_eq!(LayerAxis::ALL.len(), 3);
}

#[test]
fn a_layer_state_that_did_not_take_effect_is_not_reported_as_a_success() {
    // 3 つの setter は戻り値を持たない。無言で無視されたことは読み直しでしか
    // 分からず、read-back が唯一の防波堤である。**軸ごとに確かめる。** 1 つの
    // 軸で通しても、他の軸の照合が抜けていれば、その軸の無言の拒否は成功と
    // して返る。
    for axis in LayerAxis::ALL {
        let name = axis.label();
        let harness =
            Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreLayerState)));
        let params = axis.request(layer_state_params(&harness, 0));
        let error = harness
            .edit
            .set_layer_state(&params)
            .err()
            .unwrap_or_else(|| panic!("{name} の反映されていない変更が成功として返りました"));

        assert_eq!(
            error.error_code(),
            ErrorCode::UnsupportedOperation,
            "{name}"
        );
        assert_eq!(
            error.details()["reason"],
            json!("change_not_applied"),
            "{name}"
        );
        // SDK へは届いている。届いた以上は変更が入った側へ倒す。
        assert_eq!(error.details()["mutation_issued"], json!(true), "{name}");
    }
}

#[test]
fn every_layer_axis_is_applied_when_the_host_accepts_it() {
    // 上の確認の対になる。要求が通る状態で失敗するなら、read-back の照合が
    // 厳しすぎることになる。
    for axis in LayerAxis::ALL {
        let name = axis.label();
        let harness = Harness::new();
        let params = axis.request(layer_state_params(&harness, 0));
        harness
            .edit
            .set_layer_state(&params)
            .unwrap_or_else(|error| panic!("{name} の変更が拒否されました: {error}"));
    }
}

#[test]
fn the_layer_state_read_back_takes_the_three_attributes_at_once() {
    let harness = Harness::new();
    let params = SetLayerStateParams {
        name: Some(LayerNameChange::Set {
            name: "背景".to_string(),
        }),
        enabled: Some(false),
        locked: Some(true),
        ..layer_state_params(&harness, 0)
    };
    harness.host.clear_calls();
    harness
        .edit
        .set_layer_state(&params)
        .expect("変更が失敗しました");

    let calls = harness.host.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| **call == LAYER_ATTRIBUTES)
            .count(),
        1,
        "読み直しが属性ごとに分かれています: {calls:?}"
    );
}

#[test]
fn changing_the_layer_state_is_not_stopped_by_the_layer_lock() {
    // ロックを外すこの operation にロックのガードを掛けると、ロックされた
    // レイヤーの行き止まりが解けなくなる。
    let harness = Harness::new();
    assert!(
        harness.host.scene().layers[2].locked,
        "レイヤー 2 がロックされていません"
    );
    harness.host.clear_calls();

    // ロックは 3 軸のいずれも止めない。ロックを外す軸だけを確かめると、名前や
    // 表示にだけガードが掛かった実装が素通りする。
    harness
        .edit
        .set_layer_state(&SetLayerStateParams {
            name: Some(LayerNameChange::Set {
                name: "背景".to_string(),
            }),
            ..layer_state_params(&harness, 2)
        })
        .expect("ロックされたレイヤーの名前を変えられません");
    harness
        .edit
        .set_layer_state(&SetLayerStateParams {
            enabled: Some(false),
            ..layer_state_params(&harness, 2)
        })
        .expect("ロックされたレイヤーの表示を変えられません");
    let outcome = harness
        .edit
        .set_layer_state(&SetLayerStateParams {
            locked: Some(false),
            ..layer_state_params(&harness, 2)
        })
        .expect("ロックされたレイヤーのロックを外せません");

    assert!(!outcome.layer.locked, "ロックが外れていません");
    assert!(!harness.host.scene().layers[2].locked);
    assert!(
        !harness.host.calls().contains(&LAYER_LOCK),
        "ロックの確認を行いました: {:?}",
        harness.host.calls()
    );
}

#[test]
fn changing_the_layer_state_advances_the_revision_once_for_all_three_axes() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_layer_state(&SetLayerStateParams {
            name: Some(LayerNameChange::Set {
                name: "背景".to_string(),
            }),
            enabled: Some(false),
            locked: Some(true),
            ..layer_state_params(&harness, 0)
        })
        .expect("変更が失敗しました");

    assert_eq!(outcome.project_revision, 1);
    assert_eq!(
        harness.project.revision(),
        1,
        "軸ごとに revision が進みました"
    );
}
