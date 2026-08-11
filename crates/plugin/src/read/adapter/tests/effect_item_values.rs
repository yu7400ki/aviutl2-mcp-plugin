//! 補間後の設定項目の値の取得の統合テスト。

use super::*;

#[test]
fn evaluated_values_follow_the_requested_frames_in_order() {
    // 値は位置で対応付ける。並びが崩れると別のフレームの値を読むことになる。
    let adapter = mixed_adapter();
    let frames = [150.0, 120.5, 199.0];
    let values = adapter
        .get_effect_item_values(&item_values_params(
            effect_selector_of(&adapter, "標準描画", 0),
            &frames,
            Some(&["X"]),
        ))
        .expect("評価できます");

    assert_eq!(
        values.frames.iter().map(FiniteF64::get).collect::<Vec<_>>(),
        frames.to_vec(),
        "要求したフレームがそのまま返っていません"
    );
    assert_eq!(
        track_values(&values, "X")
            .iter()
            .map(FiniteF64::get)
            .collect::<Vec<_>>(),
        frames.map(track_value_at).to_vec(),
        "値の並びが要求したフレームの並びと違います"
    );
}

#[test]
fn a_fractional_frame_keeps_its_fraction_for_a_track_and_loses_it_for_a_check() {
    // 小数部はフレーム間の位置を指す。トラックバーへ丸めて渡すと中間点の間を
    // 問えなくなる。チェックボックスは整数フレームしか取らない。
    let adapter = mixed_adapter();
    adapter
        .get_effect_item_values(&item_values_params(
            effect_selector_of(&adapter, "標準描画", 0),
            &[120.5, 130.75],
            Some(&["X", "反転"]),
        ))
        .expect("評価できます");

    assert_eq!(
        adapter.host.evaluations(),
        vec![
            Evaluation::Track {
                items: vec!["X".to_string()],
                frames: vec![120.5, 130.75],
            },
            Evaluation::Check {
                items: vec!["反転".to_string()],
                frames: vec![120, 130],
            },
        ]
    );
}

#[test]
fn the_effect_is_resolved_once_per_kind_not_once_per_item() {
    // 参照区間の内側ではハンドルが有効であり続ける。項目ごとに effect を
    // 引き直すと、対象の解決が項目数に比例する。
    let adapter = mixed_adapter();
    adapter
        .get_effect_item_values(&item_values_params(
            effect_selector_of(&adapter, "標準描画", 0),
            &[120.0],
            Some(&["X", "Y", "拡大率", "反転"]),
        ))
        .expect("評価できます");

    let evaluations = adapter.host.evaluations();
    assert_eq!(evaluations.len(), 2, "{evaluations:?}");
    assert_eq!(
        evaluations[0],
        Evaluation::Track {
            items: vec!["X".to_string(), "Y".to_string(), "拡大率".to_string()],
            frames: vec![120.0],
        }
    );
    assert_eq!(
        evaluations[1],
        Evaluation::Check {
            items: vec!["反転".to_string()],
            frames: vec![120],
        }
    );
}

#[test]
fn a_missing_effect_a_missing_item_a_wrong_kind_and_a_refused_value_are_four_answers() {
    // effect が無い・項目名が誤っている・種別が違う・値が返らないは、要求元が
    // 次に取る行動がそれぞれ違う。畳むと切り分けられない。
    let adapter = mixed_adapter();
    let selector = effect_selector_of(&adapter, "標準描画", 0);
    let missing_effect = adapter
        .get_effect_item_values(&item_values_params(
            EffectSelector {
                effect_name: "存在しない効果".to_string(),
                ..selector.clone()
            },
            &[120.0],
            Some(&["X"]),
        ))
        .expect_err("存在しない effect が受理されました");
    let missing_item = adapter
        .get_effect_item_values(&item_values_params(
            selector.clone(),
            &[120.0],
            Some(&["存在しない項目"]),
        ))
        .expect_err("存在しない項目名が受理されました");
    let wrong_kind = adapter
        .get_effect_item_values(&item_values_params(
            selector.clone(),
            &[120.0],
            Some(&["説明"]),
        ))
        .expect_err("評価できない種別が受理されました");

    let refusing = adapter_with(|_| FakeHost {
        values_unavailable_for: Some("X".to_string()),
        group_item_names: vec![(
            TRACK_GROUP.to_string(),
            vec!["X".to_string(), "Y".to_string()],
        )],
        ..host_with_effects(vec![mixed_effect()])
    });
    let refused = refusing
        .get_effect_item_values(&item_values_params(
            effect_selector_of(&refusing, "標準描画", 0),
            &[120.0],
            Some(&["X"]),
        ))
        .expect_err("値が返らないのに成功しました");

    let answers: Vec<(ErrorCode, serde_json::Value)> =
        [&missing_effect, &missing_item, &wrong_kind, &refused]
            .into_iter()
            .map(|error| (error.error_code(), error.details()["reason"].clone()))
            .collect();
    assert_eq!(
        answers,
        vec![
            (ErrorCode::NotFound, serde_json::json!("target_missing")),
            (ErrorCode::NotFound, serde_json::json!("item_not_found")),
            (
                ErrorCode::UnsupportedOperation,
                serde_json::json!("item_not_evaluatable")
            ),
            (
                ErrorCode::SdkError,
                serde_json::json!("track_value_unavailable")
            ),
        ]
    );
    let distinct: std::collections::BTreeSet<String> =
        answers.iter().map(|answer| format!("{answer:?}")).collect();
    assert_eq!(
        distinct.len(),
        answers.len(),
        "同じ応答になった失敗があります"
    );
}

#[test]
fn omitting_the_item_names_selects_every_evaluatable_item() {
    // 評価できない種別は現れない。要求元が effect の項目名を知らなくても
    // 「評価できるものを全部」と言えるようにする。
    let adapter = mixed_adapter();
    let values = adapter
        .get_effect_item_values(&item_values_params(
            effect_selector_of(&adapter, "標準描画", 0),
            &[120.0],
            None,
        ))
        .expect("評価できます");

    assert_eq!(evaluated_names(&values), vec!["X", "Y", "拡大率", "反転"]);
    assert!(!values.truncated);
}

#[test]
fn omitting_the_item_names_truncates_at_the_limit() {
    // 項目数が上限を超える effect はあり得る。黙って落とさず、打ち切った
    // ことを伝える。
    for (count, expected, truncated) in [
        (MAX_EVALUATED_ITEMS - 1, MAX_EVALUATED_ITEMS - 1, false),
        (MAX_EVALUATED_ITEMS, MAX_EVALUATED_ITEMS, false),
        (MAX_EVALUATED_ITEMS + 1, MAX_EVALUATED_ITEMS, true),
    ] {
        let effect = HostEffect {
            items: (0..count)
                .map(|i| track_item(&format!("項目{i}"), None))
                .collect(),
            ..mixed_effect()
        };
        let adapter = adapter_with(|_| host_with_effects(vec![effect]));
        let values = adapter
            .get_effect_item_values(&item_values_params(
                effect_selector_of(&adapter, "標準描画", 0),
                &[120.0],
                None,
            ))
            .expect("評価できます");

        assert_eq!(values.items.len(), expected, "{count} 件の effect");
        assert_eq!(values.truncated, truncated, "{count} 件の effect");
    }
}

#[test]
fn a_group_is_returned_with_both_counts_even_when_they_disagree() {
    // グループのトラック数と所属アイテム名の件数が同じであるとは定められて
    // いない。一致を強制せず、両方を返して要求元に見せる。
    let adapter = mixed_adapter();
    let values = adapter
        .get_effect_item_values(&item_values_params(
            effect_selector_of(&adapter, "標準描画", 0),
            &[120.0],
            Some(&["X", "Y", "拡大率"]),
        ))
        .expect("件数が食い違っても失敗しません");

    let group = group_of(&values, "X").expect("グループに属します");
    assert_eq!(group.name, TRACK_GROUP);
    assert_eq!(group.index, 0);
    assert_eq!(group.count, 3);
    assert_eq!(group.item_names, vec!["X".to_string(), "Y".to_string()]);
    assert_ne!(group.count, group.item_names.len());
    assert_eq!(group_of(&values, "Y").expect("グループに属します").index, 1);
    assert_eq!(
        group_of(&values, "拡大率"),
        None,
        "グループに属さない項目がグループを名乗りました"
    );
    assert_eq!(
        adapter
            .host
            .calls()
            .iter()
            .filter(|call| **call == "track_group_item_names")
            .count(),
        1,
        "同じグループの所属アイテム名を引き直しています"
    );
}

#[test]
fn a_group_that_the_host_does_not_know_is_not_a_failure() {
    // 所属アイテム名が 0 件で返るのは「指定グループが無い」であって失敗
    // ではない。
    let adapter = adapter_with(|_| host_with_effects(vec![mixed_effect()]));
    let values = adapter
        .get_effect_item_values(&item_values_params(
            effect_selector_of(&adapter, "標準描画", 0),
            &[120.0],
            Some(&["X"]),
        ))
        .expect("0 件でも失敗しません");
    assert!(
        group_of(&values, "X")
            .expect("グループに属します")
            .item_names
            .is_empty()
    );
}

#[test]
fn a_frame_outside_the_object_is_a_precondition_failure() {
    // フレームはシーンの絶対フレーム番号である。対象の外を指す要求には
    // 補間する対象そのものが無い。
    let adapter = mixed_adapter();
    for frame in [99.0, 200.5, 300.0] {
        let error = adapter
            .get_effect_item_values(&item_values_params(
                effect_selector_of(&adapter, "標準描画", 0),
                &[120.0, frame],
                Some(&["X"]),
            ))
            .unwrap_err();
        assert_eq!(
            error.error_code(),
            ErrorCode::PreconditionFailed,
            "フレーム {frame}"
        );
        assert_eq!(error.details()["reason"], "frame_out_of_range");
    }
    assert!(
        adapter.host.evaluations().is_empty(),
        "範囲外のまま値を読みに行きました"
    );
    // 端は含む。オブジェクトが占めるフレームは開始から終了までである。
    for frame in [100.0, 200.0] {
        assert!(
            adapter
                .get_effect_item_values(&item_values_params(
                    effect_selector_of(&adapter, "標準描画", 0),
                    &[frame],
                    Some(&["X"]),
                ))
                .is_ok(),
            "端のフレーム {frame} が拒否されました"
        );
    }
}

#[test]
fn an_unknown_effect_and_a_stale_effect_fingerprint_are_told_apart() {
    let adapter = mixed_adapter();
    let selector = effect_selector_of(&adapter, "標準描画", 0);

    let unknown = adapter
        .get_effect_item_values(&item_values_params(
            EffectSelector {
                effect_name: "存在しない効果".to_string(),
                ..selector.clone()
            },
            &[120.0],
            Some(&["X"]),
        ))
        .expect_err("存在しない effect が受理されました");
    assert_eq!(unknown.error_code(), ErrorCode::NotFound);
    assert_eq!(unknown.details()["reason"], "target_missing");

    let stale = adapter
        .get_effect_item_values(&item_values_params(
            EffectSelector {
                fingerprint: sample_selector(&adapter).fingerprint,
                ..selector
            },
            &[120.0],
            Some(&["X"]),
        ))
        .expect_err("古い fingerprint が受理されました");
    assert_eq!(stale.error_code(), ErrorCode::PreconditionFailed);
}

#[test]
fn the_response_carries_neither_a_handle_nor_an_alias() {
    // 値そのものは載せるが、対象を指す内部の値と alias は載せない。
    let adapter = mixed_adapter();
    let values = adapter
        .get_effect_item_values(&item_values_params(
            effect_selector_of(&adapter, "標準描画", 0),
            &[120.0],
            None,
        ))
        .expect("評価できます");
    let json = serde_json::to_string(&values).expect("直列化できます");
    for forbidden in ["alias", "handle", "selector", "0x"] {
        assert!(
            !json.contains(forbidden),
            "{forbidden} が現れました: {json}"
        );
    }
}
