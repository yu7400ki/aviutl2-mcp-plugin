//! シーン設定と BPM グリッドの params / result の検査。

use super::*;

#[test]
fn a_grid_bpm_list_is_accepted_when_every_value_is_in_range() {
    set_grid_bpm(vec![bpm(120.0, 4, 0.0, 0.0), bpm(140.0, 3, 12.5, 0.25)])
        .validate()
        .expect("正常な一覧が拒否されました");
}

#[test]
fn an_empty_grid_bpm_list_is_accepted() {
    // グリッドを消す指定である。ホストが無視するなら read-back の件数照合が
    // 捕まえる。先回りして拒む理由が無い。
    set_grid_bpm(Vec::new())
        .validate()
        .expect("0 件の一覧が拒否されました");
}

#[test]
fn a_grid_bpm_list_at_the_limit_is_accepted_and_one_more_is_not() {
    let entries = |count: usize| {
        (0..count)
            .map(|index| bpm(120.0, 4, index as f64, 0.0))
            .collect::<Vec<_>>()
    };
    set_grid_bpm(entries(MAX_GRID_BPM_ENTRIES))
        .validate()
        .expect("上限ちょうどの一覧が拒否されました");

    let error = set_grid_bpm(entries(MAX_GRID_BPM_ENTRIES + 1))
        .validate()
        .expect_err("上限を超えた一覧が受理されました");
    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    // 件数の上限は対象フィールド名と上限で説明が尽きる。名前を足しても
    // 要求元が取れる行動は変わらない。
    assert_eq!(error.reason(), None);
}

#[test]
fn a_grid_bpm_value_that_merely_rounds_is_accepted() {
    // ホストは単精度で受け取るため、要求元が単精度で表せない値を送れば
    // 読み返した値は要求値と一致しない。それは失敗ではない。拒むのは、
    // 丸めた結果が課した範囲を外れる場合だけである。
    set_grid_bpm(vec![bpm(0.1, 4, 0.3, 0.7)])
        .validate()
        .expect("丸めが起きるだけの値が拒否されました");
    // 単精度の最小の正規化数より小さくても、0 へ潰れなければ通る。
    set_grid_bpm(vec![bpm(f64::from(f32::MIN_POSITIVE), 4, 0.0, 0.0)])
        .validate()
        .expect("単精度で表せる最小の正の値が拒否されました");
}

#[test]
fn a_descending_grid_bpm_list_is_accepted() {
    // 並べ替えはホストの仕事である。求めなかった順序を強制すると、
    // read-back の順序と要求の順序が食い違ったときに説明が要る。
    set_grid_bpm(vec![
        bpm(120.0, 4, 30.0, 0.0),
        bpm(120.0, 4, 20.0, 0.0),
        bpm(120.0, 4, 10.0, 0.0),
    ])
    .validate()
    .expect("降順の一覧が拒否されました");
}

#[test]
fn each_grid_bpm_rejection_names_its_own_reason() {
    // 5 種の検証が別の名前を名乗ることを固定する。畳むと、要求元は
    // 「値の直し方」と「そもそも受け渡せない」と「同じ位置を 2 度指した」を
    // 区別できない。
    let cases: &[(&str, Vec<GridBpm>, &str)] = &[
        (
            "単精度で無限大になる tempo",
            vec![bpm(1.0e300, 4, 0.0, 0.0)],
            "grid_bpm_out_of_range",
        ),
        (
            "単精度で 0 へ潰れる tempo",
            vec![bpm(1.0e-300, 4, 0.0, 0.0)],
            "grid_bpm_out_of_range",
        ),
        (
            "単精度で無限大になる offset",
            vec![bpm(120.0, 4, 0.0, 1.0e300)],
            "grid_bpm_out_of_range",
        ),
        (
            "0 以下の tempo",
            vec![bpm(0.0, 4, 0.0, 0.0)],
            "grid_bpm_out_of_range",
        ),
        (
            "1 未満の beat",
            vec![bpm(120.0, 0, 0.0, 0.0)],
            "grid_bpm_out_of_range",
        ),
        (
            "負の start",
            vec![bpm(120.0, 4, -1.0, 0.0)],
            "grid_bpm_out_of_range",
        ),
        (
            "重複した start",
            vec![bpm(120.0, 4, 5.0, 0.0), bpm(140.0, 3, 5.0, 0.0)],
            "duplicate_target",
        ),
        (
            "i32 に収まらない beat",
            vec![bpm(120.0, i64::from(i32::MAX) + 1, 0.0, 0.0)],
            "argument_not_representable",
        ),
    ];
    for (label, entries, reason) in cases {
        let error = set_grid_bpm(entries.clone()).validate().expect_err(label);
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument, "{label}");
        assert_eq!(error.reason(), Some(*reason), "{label}");
        assert!(REASON_VALUES.contains(reason), "{label}");
    }
}

#[test]
fn a_non_finite_grid_bpm_value_never_becomes_a_dto() {
    // 有限であることは型が担保する。JSON が非有限数を運べる唯一の経路は
    // 指数が範囲を超える表記であり、そこで拒否される。
    let json = format!(
        r#"{{"expected_scene_id":0,"entries":[{{"tempo":1e999,"beat":4,"start":0.0,"offset":0.0}}],"expected_project_epoch":"{EPOCH}"}}"#
    );
    assert!(serde_json::from_str::<SetGridBpmParams>(&json).is_err());
}

#[test]
fn set_grid_bpm_params_roundtrip() {
    assert_roundtrip(set_grid_bpm(vec![bpm(120.0, 4, 1.5, 0.25)]));
}

#[test]
fn set_grid_bpm_params_reject_unknown_fields() {
    let value = with_unknown_field(&set_grid_bpm(Vec::new()));
    assert!(serde_json::from_value::<SetGridBpmParams>(value).is_err());
}

#[test]
fn grid_bpm_outcome_roundtrip() {
    let outcome = GridBpmOutcome {
        project_epoch: EPOCH.to_string(),
        project_revision: 43,
        entries: vec![bpm(120.0, 4, 0.0, 0.0)],
    };
    let s = serde_json::to_string(&outcome).unwrap();
    let restored: GridBpmOutcome = serde_json::from_str(&s).unwrap();
    assert_eq!(restored, outcome);
}

#[test]
fn set_scene_settings_rejects_omitting_every_change() {
    let params = SetSceneSettingsParams {
        name: None,
        size: None,
        sample_rate: None,
        ..sample_set_scene_settings()
    };
    let error = params.validate().unwrap_err();
    assert_eq!(
        error,
        EditInputError::NoChangeRequested {
            fields: &["name", "size", "sample_rate"],
        }
    );
    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    assert_eq!(error.reason(), None);

    // 3 つの軸は個別にも組み合わせても指定できる。
    for params in [
        SetSceneSettingsParams {
            size: None,
            sample_rate: None,
            ..sample_set_scene_settings()
        },
        SetSceneSettingsParams {
            name: None,
            sample_rate: None,
            ..sample_set_scene_settings()
        },
        SetSceneSettingsParams {
            name: None,
            size: None,
            ..sample_set_scene_settings()
        },
        SetSceneSettingsParams {
            sample_rate: None,
            ..sample_set_scene_settings()
        },
        sample_set_scene_settings(),
    ] {
        assert_eq!(params.validate(), Ok(()));
    }
}

#[test]
fn set_scene_settings_rejects_an_empty_scene_name() {
    // ホストは空文字を「変更しない」として無視する。受け付ければ、何も
    // 起きなかった要求を成功として返すことになる。
    let error = SetSceneSettingsParams {
        name: Some(String::new()),
        ..sample_set_scene_settings()
    }
    .validate()
    .expect_err("空のシーン名が受理されました");
    assert_eq!(
        error,
        EditInputError::Text {
            field: FIELD_NAME,
            source: TextSyntaxError::Empty,
        }
    );
    assert_eq!(error.reason(), Some("empty"));
    assert!(REASON_VALUES.contains(&"empty"));

    // オブジェクト名は空を標準名へ戻す指定として受け付け続ける。シーン名に
    // その意味が無いのは、戻す先が存在しないためである。
    assert_eq!(
        SetObjectNameParams {
            selector: sample_object_selector(),
            name: Some(String::new()),
        }
        .validate(),
        Ok(())
    );
}

#[test]
fn set_scene_settings_applies_the_shared_name_rule() {
    // 名前の規則はオブジェクト名・レイヤー名と共有する。別の規則を書き
    // 起こすと、同じ名前が経路によって受理されたり拒否されたりする。
    assert_eq!(
        SetSceneSettingsParams {
            name: Some("本\0編".to_string()),
            ..sample_set_scene_settings()
        }
        .validate(),
        Err(EditInputError::Text {
            field: FIELD_NAME,
            source: TextSyntaxError::ContainsNul,
        })
    );

    let over = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2 + 1);
    assert!(matches!(
        SetSceneSettingsParams {
            name: Some(over),
            ..sample_set_scene_settings()
        }
        .validate(),
        Err(EditInputError::Text {
            field: FIELD_NAME,
            source: TextSyntaxError::TooLongUtf16 { .. },
        })
    ));

    // 制御文字は見ない。名前の規則が経路ごとに分かれる。
    assert_eq!(
        SetSceneSettingsParams {
            name: Some("本\t編".to_string()),
            ..sample_set_scene_settings()
        }
        .validate(),
        Ok(())
    );
}

#[test]
fn the_scene_size_bound_comes_from_the_render_limit() {
    let scene_size = |width, height| SetSceneSettingsParams {
        name: None,
        size: Some(SceneSize { width, height }),
        sample_rate: None,
        ..sample_set_scene_settings()
    };
    let max_pixels = (MAX_RENDER_FRAME_BYTES / 4) as u32;

    // ちょうど上限の組は通る。形が違っても境界は画素数だけで決まる。
    for (width, height) in [(8192, 8192), (1, max_pixels)] {
        scene_size(width, height)
            .validate()
            .expect("上限ちょうどの解像度が拒否されました");
    }

    // 1 画素超えると落ちる。
    let error = scene_size(1, max_pixels + 1)
        .validate()
        .expect_err("上限を 1 画素超えた解像度が受理されました");
    assert_eq!(
        error,
        EditInputError::SceneFrameTooLarge {
            bytes: (u64::from(max_pixels) + 1) * 4,
            max: MAX_RENDER_FRAME_BYTES,
        }
    );
    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);

    // 積は 64bit で取る。`u32` の掛け算では 0 へ折り返し、上限を下回る値
    // として通ってしまう組である。
    assert!(matches!(
        scene_size(65536, 65536).validate(),
        Err(EditInputError::SceneFrameTooLarge { .. })
    ));
}

#[test]
fn set_scene_settings_rejects_values_outside_the_receivable_range() {
    let over = MAX_POSITION + 1;
    let cases: &[(&str, SetSceneSettingsParams, &'static str, u32)] = &[
        (
            "横幅 0",
            SetSceneSettingsParams {
                size: Some(SceneSize {
                    width: 0,
                    height: 1080,
                }),
                ..sample_set_scene_settings()
            },
            FIELD_SIZE_WIDTH,
            0,
        ),
        (
            "高さ 0",
            SetSceneSettingsParams {
                size: Some(SceneSize {
                    width: 1920,
                    height: 0,
                }),
                ..sample_set_scene_settings()
            },
            FIELD_SIZE_HEIGHT,
            0,
        ),
        (
            "i32 に収まらない横幅",
            SetSceneSettingsParams {
                size: Some(SceneSize {
                    width: over,
                    height: 1,
                }),
                ..sample_set_scene_settings()
            },
            FIELD_SIZE_WIDTH,
            over,
        ),
        (
            "i32 に収まらない高さ",
            SetSceneSettingsParams {
                size: Some(SceneSize {
                    width: 1,
                    height: over,
                }),
                ..sample_set_scene_settings()
            },
            FIELD_SIZE_HEIGHT,
            over,
        ),
        (
            "サンプリングレート 0",
            SetSceneSettingsParams {
                name: None,
                size: None,
                sample_rate: Some(0),
                ..sample_set_scene_settings()
            },
            FIELD_SAMPLE_RATE,
            0,
        ),
        (
            "i32 に収まらないサンプリングレート",
            SetSceneSettingsParams {
                name: None,
                size: None,
                sample_rate: Some(over),
                ..sample_set_scene_settings()
            },
            FIELD_SAMPLE_RATE,
            over,
        ),
    ];
    for (label, params, field, value) in cases {
        let error = params.clone().validate().expect_err(label);
        assert_eq!(
            error,
            EditInputError::SceneValueOutOfRange {
                field,
                value: *value,
                max: MAX_POSITION,
            },
            "{label}"
        );
    }

    // 上限ちょうどのサンプリングレートは通る。受理値の一覧は我々の側に
    // 無く、受け渡せる範囲だけを課している。
    assert_eq!(
        SetSceneSettingsParams {
            name: None,
            size: None,
            sample_rate: Some(MAX_POSITION),
            ..sample_set_scene_settings()
        }
        .validate(),
        Ok(())
    );
}

#[test]
fn scene_setting_range_failures_have_no_reason() {
    // 範囲外はフィールド名と上限の文面で説明が尽きる。機械可読な種別名を
    // 足しても要求元が取れる行動は変わらず、値域を広げる理由が無い。
    for error in [
        EditInputError::SceneValueOutOfRange {
            field: FIELD_SIZE_WIDTH,
            value: 0,
            max: MAX_POSITION,
        },
        EditInputError::SceneValueOutOfRange {
            field: FIELD_SAMPLE_RATE,
            value: 0,
            max: MAX_POSITION,
        },
        EditInputError::SceneFrameTooLarge {
            bytes: MAX_RENDER_FRAME_BYTES + 4,
            max: MAX_RENDER_FRAME_BYTES,
        },
    ] {
        assert_eq!(error.reason(), None, "{error}");
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument, "{error}");
    }
}

#[test]
fn set_scene_settings_params_roundtrip() {
    assert_roundtrip(sample_set_scene_settings());
    assert_roundtrip(SetSceneSettingsParams {
        name: None,
        size: None,
        ..sample_set_scene_settings()
    });
}

#[test]
fn set_scene_settings_params_reject_unknown_fields() {
    assert!(
        serde_json::from_value::<SetSceneSettingsParams>(with_unknown_field(
            &sample_set_scene_settings()
        ))
        .is_err()
    );
    let mut value = serde_json::to_value(sample_set_scene_settings()).unwrap();
    value["size"] = json!({"width": 1920, "height": 1080, "future": 1});
    assert!(serde_json::from_value::<SetSceneSettingsParams>(value).is_err());
}

#[test]
fn set_scene_settings_params_require_the_guards_and_the_whole_size() {
    for key in ["expected_scene_id", "expected_project_epoch"] {
        assert!(
            serde_json::from_value::<SetSceneSettingsParams>(without_field(
                &sample_set_scene_settings(),
                key
            ))
            .is_err(),
            "{key} の欠落が受理されました"
        );
    }

    // 3 つの軸は省略できる。
    let omitted: SetSceneSettingsParams = serde_json::from_value(json!({
        "expected_scene_id": 0,
        "sample_rate": 48_000,
        "expected_project_epoch": EPOCH,
    }))
    .unwrap();
    assert_eq!(omitted.name, None);
    assert_eq!(omitted.size, None);

    // 解像度は組であり、片方だけの指定は綴れない。ホストは片方だけを
    // 変える手段を持たない。
    let mut value = serde_json::to_value(sample_set_scene_settings()).unwrap();
    value["size"] = json!({"width": 1920});
    assert!(serde_json::from_value::<SetSceneSettingsParams>(value).is_err());
}

#[test]
fn the_scene_settings_outcome_reuses_the_read_dto() {
    let outcome = SceneSettingsOutcome {
        project_epoch: EPOCH.to_string(),
        project_revision: 43,
        scene: SceneInfo {
            id: 0,
            name: Some("本編".to_string()),
            width: 1920,
            height: 1080,
            fps: Some(finite(60.0)),
            fps_rate: 60,
            fps_scale: 1,
            sample_rate: 48_000,
        },
        observed_after_edit: true,
        non_undoable: true,
    };
    let value = serde_json::to_value(&outcome).unwrap();
    assert_eq!(value["project_epoch"], json!(EPOCH));
    assert_eq!(value["project_revision"], json!(43));
    assert_eq!(
        value["scene"],
        serde_json::to_value(&outcome.scene).unwrap()
    );
    // 取り消せないことと、観測が編集と原子的でないことは、応答だけを見る
    // 経路が拾える唯一の口である。
    assert_eq!(value["non_undoable"], json!(true));
    assert_eq!(value["observed_after_edit"], json!(true));

    let s = serde_json::to_string(&outcome).unwrap();
    assert_eq!(
        serde_json::from_str::<SceneSettingsOutcome>(&s).unwrap(),
        outcome
    );
    // 応答型は将来の optional field を受け入れる。
    let restored: SceneSettingsOutcome =
        serde_json::from_value(with_unknown_field(&outcome)).unwrap();
    assert_eq!(restored, outcome);
}
