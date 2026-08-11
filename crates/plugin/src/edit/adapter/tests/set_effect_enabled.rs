//! effect 有効・無効変更の統合テスト。

use super::*;

#[test]
fn a_silently_ignored_enable_change_is_not_reported_as_success() {
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreEffectState)));
    let error = harness
        .edit
        .set_effect_enabled(&SetEffectEnabledParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            enabled: false,
        })
        .expect_err("無言で無視された変更が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("effect_state_immutable"));
    assert_eq!(error.details()["mutation_issued"], json!(true));
}

/// 変更を受け付けない状態変更が、SDK を呼ぶ前に弾かれることを確かめる。
///
/// 表で駆動する。無言で拒否される軸が増えたら行を足すだけで、同じ主張
/// （SDK を呼ばない・revision を進めない・成功として返さない）がそのまま掛かる。
#[test]
fn changes_the_host_never_applies_are_refused_before_the_sdk_is_called() {
    /// 変更を受け付けない対象と、それへ要求する状態変更。
    struct Immutable {
        /// 何を確かめているか。
        label: &'static str,
        /// 差し替える effect 名。
        effect_name: &'static str,
        /// 差し替える effect 列の位置。
        position: usize,
        /// 要求する有効・無効。
        enabled: bool,
    }

    let scenarios = [
        // 出力項目の有効・無効。
        Immutable {
            label: "出力項目の enabled",
            effect_name: "標準描画",
            position: 0,
            enabled: false,
        },
    ];

    for Immutable {
        label,
        effect_name,
        position,
        enabled,
    } in scenarios
    {
        let name = effect_name.to_string();
        let harness = Harness::with(move |host| {
            let mut scene = host.scene.lock().unwrap();
            scene.layers[1].objects[0].effects[position].name = name;
            drop(scene);
        });
        let selector = harness.effect_selector(1, 100, effect_name, 0);
        let Err(error) = harness
            .edit
            .set_effect_enabled(&SetEffectEnabledParams { selector, enabled })
        else {
            panic!("{label} が変更できました");
        };

        assert_eq!(
            error.error_code(),
            ErrorCode::UnsupportedOperation,
            "{label}"
        );
        assert_eq!(
            error.details()["reason"],
            json!("effect_state_immutable"),
            "{label}"
        );
        assert_eq!(
            harness.host.enter_calls(),
            0,
            "{label} で編集区間へ入りました"
        );
        harness.assert_untouched();
        assert!(
            !harness.project.modified(),
            "{label} で未保存の変更が記録されました"
        );
    }
}

#[test]
fn disabling_an_input_item_is_reported_with_the_reread_state() {
    // 入力項目は有効・無効を変更できる。応答が返す effect は読み直した値であり、
    // 要求値をそのまま echo したものではない。
    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_effect_enabled(&SetEffectEnabledParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            enabled: false,
        })
        .expect("入力項目の無効化が拒否されました");

    assert!(!outcome.effect.expect("変更後の effect").enabled);
}
