//! effect 付与の統合テスト。

use super::*;

#[test]
fn an_unregistered_effect_name_is_rejected_without_entering_the_section() {
    let harness = Harness::new();
    let error = harness
        .edit
        .add_effect(&AddEffectParams {
            object: harness.selector(1, 100),
            effect_name: "存在しないエフェクト".to_string(),
        })
        .expect_err("未登録の effect が付与されました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("effect_not_registered"));
    assert_eq!(harness.host.enter_calls(), 0);
}

#[test]
fn an_added_effect_is_located_by_the_difference_in_the_name_list() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .add_effect(&AddEffectParams {
            object: harness.selector(1, 100),
            effect_name: "ぼかし".to_string(),
        })
        .expect("effect の付与に失敗しました");

    let effect = outcome.effect.expect("付与された effect");
    assert_eq!(effect.name, "ぼかし");
    // 既に同名が 1 つあるため、同名内の順序は 1 になる。
    assert_eq!(effect.index, 1);
    assert_eq!(effect.selector.effect_index, 1);
}

#[test]
fn an_added_effect_reports_where_it_landed_in_the_column() {
    // 既定の対象は `動画ファイル` と `ぼかし` を持つ。末尾へ `ぼかし` が入ると
    // 同名内の順序は 1、列の位置は 2 になり、2 つの数が食い違う。
    let harness = Harness::new();
    let outcome = harness
        .edit
        .add_effect(&AddEffectParams {
            object: harness.selector(1, 100),
            effect_name: "ぼかし".to_string(),
        })
        .expect("effect の付与に失敗しました");

    let effect = outcome.effect.expect("付与された effect");
    let scene = harness.host.scene();
    let effects = &scene.layers[1].objects[0].effects;
    assert_eq!(effect.position, effects.len() - 1);
    assert_eq!(effects[effect.position].name, effect.name);
    assert_eq!(effects[effect.position].index, effect.index);
    assert_ne!(effect.position, effect.index);
}

#[test]
fn every_effect_changing_response_reports_the_column_position() {
    // 既定の対象は `動画ファイル` と `ぼかし` を持つ。先頭の `ぼかし` は同名内で
    // 0 番目・列では 1 番目であり、2 つの数が食い違う。位置は対象を解決した時点の
    // 列から求め、変更後に読み直した列へ当てる。どちらの operation も列の構成を
    // 変えないため、2 つの列で同じ effect を指す。
    let harness = Harness::new();
    let outcomes = [
        (
            "set_object_item",
            harness
                .edit
                .set_object_item(&SetObjectItemParams {
                    selector: harness.effect_selector(1, 100, "ぼかし", 0),
                    item: "範囲".to_string(),
                    value: ItemValue::Integer { value: 30 },
                })
                .expect("set_object_item"),
        ),
        (
            "set_effect_enabled",
            harness
                .edit
                .set_effect_enabled(&SetEffectEnabledParams {
                    selector: harness.effect_selector(1, 100, "ぼかし", 0),
                    enabled: false,
                })
                .expect("set_effect_enabled"),
        ),
    ];
    for (tool, outcome) in outcomes {
        let effect = outcome.effect.expect("変更後の effect");
        assert_eq!(effect.index, 0, "{tool}");
        assert_eq!(effect.position, 1, "{tool}");
        let scene = harness.host.scene();
        let effects = &scene.layers[1].objects[0].effects;
        assert_eq!(effects[effect.position].name, effect.name, "{tool}");
        assert_eq!(effects[effect.position].index, effect.index, "{tool}");
    }
}

#[test]
fn an_added_effect_is_located_even_when_the_host_does_not_append_it() {
    // 付与位置が末尾だと決めつけると、先頭へ挿入するホストで別の effect を
    // 指す selector を返してしまう。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::PrependEffect)));
    let outcome = harness
        .edit
        .add_effect(&AddEffectParams {
            object: harness.selector(1, 100),
            effect_name: "ぼかし".to_string(),
        })
        .expect("effect の付与に失敗しました");

    let effect = outcome.effect.expect("付与された effect");
    assert_eq!(effect.name, "ぼかし");
    // 先頭へ挿入されたため、同名内の順序は 0 になり既存の方が 1 へ繰り上がる。
    assert_eq!(effect.index, 0);
    // 列の位置も先頭である。末尾を決め打つと、ここで別の要素を指す。
    assert_eq!(effect.position, 0);
    let scene = harness.host.scene();
    let effects = &scene.layers[1].objects[0].effects;
    assert_eq!(effects[0].name, "ぼかし");
    assert_eq!(effects[0].index, 0);
}

#[test]
fn an_ambiguous_effect_difference_is_reported_instead_of_being_guessed() {
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::AddTwoEffects)));
    let error = harness
        .edit
        .add_effect(&AddEffectParams {
            object: harness.selector(1, 100),
            effect_name: "ぼかし".to_string(),
        })
        .expect_err("位置を特定できないのに selector が返りました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(error.details()["sdk_operation"], json!("create_effect"));
    assert_eq!(error.details()["mutation_issued"], json!(true));
}

#[test]
fn the_added_position_comes_from_the_difference_in_the_name_list() {
    let names = |list: &[&str]| -> Vec<String> { list.iter().map(|s| s.to_string()).collect() };

    // 末尾・中間・先頭のいずれへ挿入されても位置が求まる。
    assert_eq!(
        added_effect_position(&names(&["a", "b"]), &names(&["a", "b", "c"])),
        Some(2)
    );
    assert_eq!(
        added_effect_position(&names(&["a", "b"]), &names(&["a", "c", "b"])),
        Some(1)
    );
    assert_eq!(
        added_effect_position(&names(&["a", "b"]), &names(&["c", "a", "b"])),
        Some(0)
    );
    // 同名が並んでいても件数が 1 つ増えていれば位置が定まる。
    assert_eq!(
        added_effect_position(&names(&["a", "a"]), &names(&["a", "a", "a"])),
        Some(2)
    );

    // 増減が 1 件でない、あるいは並びが入れ替わった場合は位置を名乗らない。
    assert_eq!(added_effect_position(&names(&["a"]), &names(&["a"])), None);
    assert_eq!(
        added_effect_position(&names(&["a"]), &names(&["a", "b", "c"])),
        None
    );
    assert_eq!(
        added_effect_position(&names(&["a", "b"]), &names(&["b", "a", "c"])),
        None
    );
}
