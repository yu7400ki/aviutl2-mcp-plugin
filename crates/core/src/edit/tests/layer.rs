//! レイヤーを対象とする params / result の検査。

use super::*;

#[test]
fn set_layer_state_rejects_omitting_every_change() {
    let params = SetLayerStateParams {
        name: None,
        enabled: None,
        locked: None,
        ..sample_set_layer_state()
    };
    let error = params.validate().unwrap_err();
    assert_eq!(
        error,
        EditInputError::NoChangeRequested {
            fields: &["name", "enabled", "locked"],
        }
    );
    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);

    // 3 つの軸は個別にも組み合わせても指定できる。
    for params in [
        SetLayerStateParams {
            name: Some(LayerNameChange::Reset {}),
            enabled: None,
            locked: None,
            ..sample_set_layer_state()
        },
        SetLayerStateParams {
            name: None,
            enabled: Some(true),
            locked: None,
            ..sample_set_layer_state()
        },
        SetLayerStateParams {
            name: None,
            enabled: None,
            locked: Some(false),
            ..sample_set_layer_state()
        },
        sample_set_layer_state(),
    ] {
        assert_eq!(params.validate(), Ok(()));
    }
}

#[test]
fn the_layer_name_change_is_a_struct_variant() {
    // internally tagged 表現では unit variant が deny_unknown_fields を
    // 無視し、未知フィールドを黙って読み飛ばす。
    assert_eq!(
        serde_json::to_value(LayerNameChange::Reset {}).unwrap(),
        json!({"type": "reset"})
    );
    assert_eq!(
        serde_json::to_value(LayerNameChange::Set {
            name: "背景".to_string(),
        })
        .unwrap(),
        json!({"type": "set", "name": "背景"})
    );
    assert!(
        serde_json::from_value::<LayerNameChange>(json!({"type": "reset", "name": "x"})).is_err(),
        "標準名へ戻す指定が名前を読み飛ばしました"
    );
    assert!(
        serde_json::from_value::<LayerNameChange>(json!({"type": "set"})).is_err(),
        "名前を持たない設定が受理されました"
    );

    // params の内側でも同じ扱いになる。
    let mut value = serde_json::to_value(sample_set_layer_state()).unwrap();
    value["name"] = json!({"type": "reset", "name": "x"});
    assert!(serde_json::from_value::<SetLayerStateParams>(value).is_err());
}

#[test]
fn set_layer_state_validates_the_layer_and_the_name() {
    let over = MAX_POSITION + 1;
    assert_eq!(
        SetLayerStateParams {
            layer: over,
            ..sample_set_layer_state()
        }
        .validate(),
        Err(EditInputError::PositionOutOfRange {
            field: FIELD_LAYER,
            value: over,
            max: MAX_POSITION,
        })
    );

    // 空の名前は標準名へ戻す指定と同じ結果になるため受け付けない。要求元が
    // 言っていない変更を、成功として返すことになる。
    assert_eq!(
        SetLayerStateParams {
            name: Some(LayerNameChange::Set {
                name: String::new(),
            }),
            ..sample_set_layer_state()
        }
        .validate(),
        Err(EditInputError::Text {
            field: FIELD_NAME,
            source: TextSyntaxError::Empty,
        })
    );
    // オブジェクト名は空を標準名へ戻す指定として受け付け続ける。取り消しを
    // 表す別の指定を持たないためである。
    assert_eq!(
        SetObjectNameParams {
            selector: sample_object_selector(),
            name: Some(String::new()),
        }
        .validate(),
        Ok(())
    );

    // 名前の規則はオブジェクト名と共有する。
    assert_eq!(
        SetLayerStateParams {
            name: Some(LayerNameChange::Set {
                name: "名\0前".to_string(),
            }),
            ..sample_set_layer_state()
        }
        .validate(),
        Err(EditInputError::Text {
            field: FIELD_NAME,
            source: TextSyntaxError::ContainsNul,
        })
    );

    let over = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2 + 1);
    assert!(matches!(
        SetLayerStateParams {
            name: Some(LayerNameChange::Set { name: over }),
            ..sample_set_layer_state()
        }
        .validate(),
        Err(EditInputError::Text {
            field: FIELD_NAME,
            source: TextSyntaxError::TooLongUtf16 { .. },
        })
    ));
}

#[test]
fn the_layer_state_outcome_reuses_the_read_dto() {
    let outcome = LayerStateOutcome {
        project_epoch: EPOCH.to_string(),
        project_revision: 43,
        layer: LayerInfo {
            index: 2,
            name: Some("背景".to_string()),
            enabled: false,
            locked: true,
            object_count: 3,
        },
    };
    let value = serde_json::to_value(&outcome).unwrap();
    assert_eq!(value["project_epoch"], json!(EPOCH));
    assert_eq!(value["project_revision"], json!(43));
    assert_eq!(
        value["layer"],
        serde_json::to_value(&outcome.layer).unwrap()
    );

    let s = serde_json::to_string(&outcome).unwrap();
    assert_eq!(
        serde_json::from_str::<LayerStateOutcome>(&s).unwrap(),
        outcome
    );
    // 応答型は将来の optional field を受け入れる。
    let restored: LayerStateOutcome = serde_json::from_value(with_unknown_field(&outcome)).unwrap();
    assert_eq!(restored, outcome);
}
