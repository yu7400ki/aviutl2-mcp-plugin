//! 編集 operation の params / result の検査。

use super::*;
use crate::error::REASON_VALUES;
use crate::fingerprint::{EffectFingerprintInput, ObjectFingerprintInput};
use crate::validation::{MAX_ALIAS_BYTES, MAX_NAME_UTF16_UNITS, MAX_PATH_UTF16_UNITS};
use serde_json::{Value, json};

const EPOCH: &str = "78be92d1-c8c9-44c6-ae52-387548971468";

/// variant を表す名前を返す。
///
/// 網羅 match で書く。variant を足すとここがコンパイルエラーになり、
/// すぐ下の一覧と [`EditInputError::all`] へ足す必要があることが分かる。
fn input_variant_name(error: &EditInputError) -> &'static str {
    match error {
        EditInputError::PositionOutOfRange { .. } => "PositionOutOfRange",
        EditInputError::IndexOutOfRange { .. } => "IndexOutOfRange",
        EditInputError::SectionIndexOutOfRange { .. } => "SectionIndexOutOfRange",
        EditInputError::NoChangeRequested { .. } => "NoChangeRequested",
        EditInputError::SceneValueOutOfRange { .. } => "SceneValueOutOfRange",
        EditInputError::SceneFrameTooLarge { .. } => "SceneFrameTooLarge",
        EditInputError::TooManyEntries { .. } => "TooManyEntries",
        EditInputError::GridBpmOutOfRange { .. } => "GridBpmOutOfRange",
        EditInputError::GridBpmBeatNotRepresentable { .. } => "GridBpmBeatNotRepresentable",
        EditInputError::DuplicateGridBpmStart { .. } => "DuplicateGridBpmStart",
        EditInputError::Text { .. } => "Text",
        EditInputError::Path { .. } => "Path",
        EditInputError::ItemValue(_) => "ItemValue",
    }
}

#[test]
fn all_input_failures_cover_every_variant() {
    const VARIANTS: &[&str] = &[
        "PositionOutOfRange",
        "IndexOutOfRange",
        "SectionIndexOutOfRange",
        "NoChangeRequested",
        "SceneValueOutOfRange",
        "SceneFrameTooLarge",
        "TooManyEntries",
        "GridBpmOutOfRange",
        "GridBpmBeatNotRepresentable",
        "DuplicateGridBpmStart",
        "Text",
        "Path",
        "ItemValue",
    ];
    let covered: Vec<&str> = EditInputError::all()
        .iter()
        .map(input_variant_name)
        .collect();
    for variant in VARIANTS {
        assert!(
            covered.contains(variant),
            "{variant} の代表値が一覧にありません"
        );
    }
    for variant in &covered {
        assert!(
            VARIANTS.contains(variant),
            "{variant} が網羅すべき variant の一覧にありません"
        );
    }
}

#[test]
fn all_input_failures_cover_every_reason() {
    // 名前は包む側の種別で決まる。包む側に種別が増えたとき、一覧が
    // 追随していなければここで落ちる。
    let reasons: Vec<Option<&str>> = EditInputError::all()
        .iter()
        .map(EditInputError::reason)
        .collect();
    for source in TextSyntaxError::ALL {
        assert!(reasons.contains(&Some(source.reason())), "{source}");
    }
    for source in PathSyntaxError::ALL {
        assert!(reasons.contains(&Some(source.reason())), "{source}");
    }
    for source in ItemWriteError::all() {
        assert!(reasons.contains(&source.reason()), "{source}");
    }
}

#[test]
fn input_failures_carry_the_reason_of_the_syntax_error_they_wrap() {
    // 検証の実体は core にあり、失敗の種別名も core が持つ。要求元へ
    // 届けるのは既にある名前であって、経路ごとに付け直す名前ではない。
    for error in PathSyntaxError::ALL {
        let mapped = EditInputError::Path {
            field: FIELD_PATH,
            source: *error,
        };
        assert_eq!(mapped.reason(), Some(error.reason()), "{error}");
        assert!(REASON_VALUES.contains(&error.reason()));
    }
    for error in TextSyntaxError::ALL {
        let mapped = EditInputError::Text {
            field: FIELD_NAME,
            source: *error,
        };
        assert_eq!(mapped.reason(), Some(error.reason()), "{error}");
        assert!(REASON_VALUES.contains(&error.reason()));
    }
    assert_eq!(
        EditInputError::ItemValue(ItemWriteError::Path(PathSyntaxError::UncPath)).reason(),
        Some("unc_path")
    );
}

#[test]
fn position_failures_have_no_reason() {
    // 範囲外の位置は対象フィールド名と上限で説明が尽きる。名前を足しても
    // 要求元が取れる行動は変わらない。
    for error in [
        EditInputError::PositionOutOfRange {
            field: FIELD_LAYER,
            value: 0,
            max: 0,
        },
        EditInputError::IndexOutOfRange {
            field: FIELD_SELECTOR_LAYER,
            value: 0,
            max: 0,
        },
        EditInputError::NoChangeRequested {
            fields: &["enabled"],
        },
    ] {
        assert_eq!(error.reason(), None, "{error}");
    }
}

fn sample_summary() -> ObjectSummary {
    ObjectSummary::new(
        EPOCH,
        ObjectFingerprintInput {
            scene_id: 0,
            layer: 2,
            frame_start: 120,
            frame_end: 240,
            name: Some("立ち絵"),
            alias: "alias",
        },
    )
}

fn sample_object_selector() -> ObjectSelector {
    sample_summary().selector
}

fn sample_effect_info() -> EffectInfo {
    EffectInfo::new(
        sample_object_selector(),
        EffectFingerprintInput {
            effect_name: "動画ファイル",
            effect_index: 0,
            position: 0,
            effect_count: 1,
            enabled: true,
            locked: false,
            items: &[],
        },
    )
}

fn sample_effect_selector() -> EffectSelector {
    sample_effect_info().selector
}

fn sample_create() -> CreateObjectParams {
    CreateObjectParams {
        source: ObjectSource::MediaFile {
            path: r"C:\movie.mp4".to_string(),
        },
        placement: Placement {
            scene_id: 0,
            layer: 2,
            frame: 120,
        },
        expected_project_epoch: EPOCH.to_string(),
    }
}

fn sample_move() -> MoveObjectParams {
    MoveObjectParams {
        selector: sample_object_selector(),
        destination: Destination {
            layer: 3,
            frame: 240,
        },
    }
}

fn sample_set_object_item() -> SetObjectItemParams {
    SetObjectItemParams {
        selector: sample_effect_selector(),
        item: "X".to_string(),
        value: ItemValue::Number {
            value: FiniteF64::try_new(12.5).unwrap(),
        },
    }
}

fn sample_move_effect() -> MoveEffectParams {
    MoveEffectParams {
        selector: sample_effect_selector(),
        position: 2,
    }
}

fn sample_set_layer_state() -> SetLayerStateParams {
    SetLayerStateParams {
        expected_scene_id: 0,
        layer: 2,
        name: Some(LayerNameChange::Set {
            name: "背景".to_string(),
        }),
        enabled: Some(false),
        locked: Some(true),
        expected_project_epoch: EPOCH.to_string(),
    }
}

fn sample_set_selection() -> SetSelectionParams {
    SetSelectionParams {
        expected_scene_id: 0,
        cursor: Some(CursorPosition {
            layer: 2,
            frame: 120,
        }),
        selected_range: Some(RangeChange::Set { start: 10, end: 20 }),
        focus: Some(FocusChange::Set {
            object: sample_object_selector(),
        }),
        display: Some(DisplayStart {
            layer: 1,
            frame: 60,
        }),
        expected_project_epoch: EPOCH.to_string(),
    }
}

fn sample_display_range() -> DisplayRange {
    DisplayRange {
        frame_start: 60,
        layer_start: 1,
        frame_num: 600,
        layer_num: 10,
    }
}

/// params を JSON へ写し、未知フィールドを足した値を返す。
fn with_unknown_field(value: &impl Serialize) -> Value {
    let mut value = serde_json::to_value(value).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("future".to_string(), json!(1));
    value
}

/// JSON から 1 つのキーを取り除いた値を返す。
fn without_field(value: &impl Serialize, key: &str) -> Value {
    let mut value = serde_json::to_value(value).unwrap();
    assert!(
        value.as_object_mut().unwrap().remove(key).is_some(),
        "{key} が存在しません"
    );
    value
}

/// JSON への往復で値が変わらないことを確かめる。
fn assert_roundtrip<T>(params: T)
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let s = serde_json::to_string(&params).unwrap();
    let restored: T = serde_json::from_str(&s).unwrap();
    assert_eq!(restored, params);
}

#[test]
fn params_roundtrip() {
    assert_roundtrip(sample_create());
    assert_roundtrip(CreateObjectParams {
        source: ObjectSource::ObjectAlias {
            alias: "[vo]\n_name=立ち絵\n".to_string(),
        },
        ..sample_create()
    });
    assert_roundtrip(CreateObjectParams {
        source: ObjectSource::AliasName {
            name: "テストエイリアス".to_string(),
        },
        ..sample_create()
    });
    assert_roundtrip(sample_move());
    assert_roundtrip(DeleteObjectParams {
        selector: sample_object_selector(),
    });
    assert_roundtrip(SetObjectNameParams {
        selector: sample_object_selector(),
        name: Some("立ち絵".to_string()),
    });
    assert_roundtrip(SetObjectNameParams {
        selector: sample_object_selector(),
        name: None,
    });
    assert_roundtrip(sample_set_object_item());
    assert_roundtrip(AddEffectParams {
        object: sample_object_selector(),
        effect_name: "ぼかし".to_string(),
    });
    assert_roundtrip(DeleteEffectParams {
        selector: sample_effect_selector(),
    });
    assert_roundtrip(SetEffectEnabledParams {
        selector: sample_effect_selector(),
        enabled: false,
    });
    assert_roundtrip(sample_move_effect());
    assert_roundtrip(sample_set_selection());
    assert_roundtrip(SetSelectionParams {
        selected_range: Some(RangeChange::Clear {}),
        focus: Some(FocusChange::Clear {}),
        ..sample_set_selection()
    });
    assert_roundtrip(sample_set_layer_state());
    assert_roundtrip(SetLayerStateParams {
        name: Some(LayerNameChange::Reset {}),
        enabled: None,
        locked: None,
        ..sample_set_layer_state()
    });
}

#[test]
fn params_reject_unknown_fields() {
    macro_rules! assert_rejects_unknown {
        ($type:ty, $params:expr) => {
            assert!(
                serde_json::from_value::<$type>(with_unknown_field(&$params)).is_err(),
                "{} が未知フィールドを受理しました",
                stringify!($type)
            );
        };
    }

    assert_rejects_unknown!(CreateObjectParams, sample_create());
    assert_rejects_unknown!(MoveObjectParams, sample_move());
    assert_rejects_unknown!(
        DeleteObjectParams,
        DeleteObjectParams {
            selector: sample_object_selector(),
        }
    );
    assert_rejects_unknown!(
        SetObjectNameParams,
        SetObjectNameParams {
            selector: sample_object_selector(),
            name: None,
        }
    );
    assert_rejects_unknown!(SetObjectItemParams, sample_set_object_item());
    assert_rejects_unknown!(
        AddEffectParams,
        AddEffectParams {
            object: sample_object_selector(),
            effect_name: "ぼかし".to_string(),
        }
    );
    assert_rejects_unknown!(
        DeleteEffectParams,
        DeleteEffectParams {
            selector: sample_effect_selector(),
        }
    );
    assert_rejects_unknown!(
        SetEffectEnabledParams,
        SetEffectEnabledParams {
            selector: sample_effect_selector(),
            enabled: true,
        }
    );
    assert_rejects_unknown!(MoveEffectParams, sample_move_effect());
    assert_rejects_unknown!(SetSelectionParams, sample_set_selection());
    assert_rejects_unknown!(SetLayerStateParams, sample_set_layer_state());
    assert_rejects_unknown!(
        LayerNameChange,
        LayerNameChange::Set {
            name: "背景".to_string(),
        }
    );
    assert_rejects_unknown!(LayerNameChange, LayerNameChange::Reset {});
    assert_rejects_unknown!(
        Placement,
        Placement {
            scene_id: 0,
            layer: 0,
            frame: 0,
        }
    );
    assert_rejects_unknown!(Destination, Destination { layer: 0, frame: 0 });
    assert_rejects_unknown!(CursorPosition, CursorPosition { layer: 0, frame: 0 });
    assert_rejects_unknown!(
        ObjectSource,
        ObjectSource::MediaFile {
            path: r"C:\movie.mp4".to_string(),
        }
    );
    assert_rejects_unknown!(RangeChange, RangeChange::Set { start: 0, end: 1 });
    assert_rejects_unknown!(RangeChange, RangeChange::Clear {});
    assert_rejects_unknown!(
        FocusChange,
        FocusChange::Set {
            object: sample_object_selector(),
        }
    );
}

#[test]
fn params_reject_missing_required_fields() {
    for key in ["source", "placement", "expected_project_epoch"] {
        assert!(
            serde_json::from_value::<CreateObjectParams>(without_field(&sample_create(), key))
                .is_err(),
            "{key} の欠落が受理されました"
        );
    }
    for key in ["selector", "destination"] {
        assert!(
            serde_json::from_value::<MoveObjectParams>(without_field(&sample_move(), key)).is_err(),
            "{key} の欠落が受理されました"
        );
    }
    for key in ["selector", "item", "value"] {
        assert!(
            serde_json::from_value::<SetObjectItemParams>(without_field(
                &sample_set_object_item(),
                key
            ))
            .is_err(),
            "{key} の欠落が受理されました"
        );
    }
    let set_effect_enabled = SetEffectEnabledParams {
        selector: sample_effect_selector(),
        enabled: false,
    };
    for key in ["selector", "enabled"] {
        assert!(
            serde_json::from_value::<SetEffectEnabledParams>(without_field(
                &set_effect_enabled,
                key
            ))
            .is_err(),
            "{key} の欠落が受理されました"
        );
    }
    for key in ["expected_scene_id", "expected_project_epoch"] {
        assert!(
            serde_json::from_value::<SetSelectionParams>(without_field(
                &sample_set_selection(),
                key
            ))
            .is_err(),
            "{key} の欠落が受理されました"
        );
    }
    for key in ["expected_scene_id", "layer", "expected_project_epoch"] {
        assert!(
            serde_json::from_value::<SetLayerStateParams>(without_field(
                &sample_set_layer_state(),
                key
            ))
            .is_err(),
            "{key} の欠落が受理されました"
        );
    }
}

#[test]
fn optional_fields_may_be_omitted() {
    // 省略と null の明示はどちらも標準名へ戻すことを意味する。
    let omitted: SetObjectNameParams = serde_json::from_value(json!({
        "selector": serde_json::to_value(sample_object_selector()).unwrap(),
    }))
    .unwrap();
    let explicit: SetObjectNameParams = serde_json::from_value(json!({
        "selector": serde_json::to_value(sample_object_selector()).unwrap(),
        "name": Value::Null,
    }))
    .unwrap();
    assert_eq!(omitted, explicit);
    assert_eq!(omitted.name, None);
}

#[test]
fn nested_selectors_still_accept_unknown_fields() {
    // params が未知フィールドを拒否しても、往復型である selector の
    // 扱いは変わらない。応答へ optional field が増えても往復が壊れない。
    let mut value = serde_json::to_value(sample_move()).unwrap();
    value["selector"]
        .as_object_mut()
        .unwrap()
        .insert("future".to_string(), json!(1));
    let restored: MoveObjectParams = serde_json::from_value(value).unwrap();
    assert_eq!(restored.selector, sample_object_selector());

    let mut value = serde_json::to_value(sample_set_object_item()).unwrap();
    value["selector"]
        .as_object_mut()
        .unwrap()
        .insert("future".to_string(), json!(1));
    value["selector"]["object"]
        .as_object_mut()
        .unwrap()
        .insert("future".to_string(), json!(2));
    let restored: SetObjectItemParams = serde_json::from_value(value).unwrap();
    assert_eq!(restored.selector, sample_effect_selector());

    let mut value = serde_json::to_value(sample_set_selection()).unwrap();
    value["focus"]["object"]
        .as_object_mut()
        .unwrap()
        .insert("future".to_string(), json!(1));
    let restored: SetSelectionParams = serde_json::from_value(value).unwrap();
    assert_eq!(
        restored.focus,
        Some(FocusChange::Set {
            object: sample_object_selector(),
        })
    );
}

#[test]
fn tagged_enums_use_snake_case_tags() {
    assert_eq!(
        serde_json::to_value(ObjectSource::MediaFile {
            path: r"C:\movie.mp4".to_string(),
        })
        .unwrap(),
        json!({"type": "media_file", "path": r"C:\movie.mp4"})
    );
    assert_eq!(
        serde_json::to_value(ObjectSource::ObjectAlias {
            alias: "[vo]".to_string(),
        })
        .unwrap(),
        json!({"type": "object_alias", "alias": "[vo]"})
    );
    assert_eq!(
        serde_json::to_value(ObjectSource::Effect {
            name: "テキスト".to_string(),
        })
        .unwrap(),
        json!({"type": "effect", "name": "テキスト"})
    );
    assert_eq!(
        serde_json::to_value(ObjectSource::AliasName {
            name: "テストエイリアス".to_string(),
        })
        .unwrap(),
        json!({"type": "alias_name", "name": "テストエイリアス"})
    );
    assert_eq!(
        serde_json::to_value(RangeChange::Set { start: 1, end: 2 }).unwrap(),
        json!({"type": "set", "start": 1, "end": 2})
    );
    assert_eq!(
        serde_json::to_value(RangeChange::Clear {}).unwrap(),
        json!({"type": "clear"})
    );
    assert_eq!(
        serde_json::to_value(FocusChange::Clear {}).unwrap(),
        json!({"type": "clear"})
    );
    assert_eq!(
        serde_json::to_value(SelectionField::SelectedRange).unwrap(),
        json!("selected_range")
    );
}

#[test]
fn positions_reject_values_outside_the_representable_range() {
    let over = MAX_POSITION + 1;
    assert_eq!(MAX_POSITION, 2_147_483_647);

    for (field, params) in [
        (
            FIELD_LAYER,
            CreateObjectParams {
                placement: Placement {
                    scene_id: 0,
                    layer: over,
                    frame: 0,
                },
                ..sample_create()
            },
        ),
        (
            FIELD_FRAME,
            CreateObjectParams {
                placement: Placement {
                    scene_id: 0,
                    layer: 0,
                    frame: over,
                },
                ..sample_create()
            },
        ),
    ] {
        assert_eq!(
            params.validate(),
            Err(EditInputError::PositionOutOfRange {
                field,
                value: over,
                max: MAX_POSITION,
            })
        );
    }

    assert_eq!(
        MoveObjectParams {
            destination: Destination {
                layer: over,
                frame: 0,
            },
            ..sample_move()
        }
        .validate(),
        Err(EditInputError::PositionOutOfRange {
            field: FIELD_LAYER,
            value: over,
            max: MAX_POSITION,
        })
    );

    for (cursor, field) in [
        (
            CursorPosition {
                layer: 0,
                frame: over,
            },
            FIELD_FRAME,
        ),
        (
            CursorPosition {
                layer: over,
                frame: 0,
            },
            FIELD_LAYER,
        ),
    ] {
        assert_eq!(
            SetSelectionParams {
                cursor: Some(cursor),
                selected_range: None,
                focus: None,
                display: None,
                ..sample_set_selection()
            }
            .validate(),
            Err(EditInputError::PositionOutOfRange {
                field,
                value: over,
                max: MAX_POSITION,
            })
        );
    }

    assert_eq!(
        SetSelectionParams {
            cursor: None,
            selected_range: Some(RangeChange::Set {
                start: 0,
                end: over,
            }),
            focus: None,
            ..sample_set_selection()
        }
        .validate(),
        Err(EditInputError::PositionOutOfRange {
            field: FIELD_RANGE_END,
            value: over,
            max: MAX_POSITION,
        })
    );
}

#[test]
fn positions_accept_the_upper_bound() {
    // 上限をこれ以上狭めない。
    assert_eq!(
        CreateObjectParams {
            placement: Placement {
                scene_id: 0,
                layer: MAX_POSITION,
                frame: MAX_POSITION,
            },
            ..sample_create()
        }
        .validate(),
        Ok(())
    );
}

#[test]
fn set_selection_rejects_omitting_every_change() {
    let params = SetSelectionParams {
        expected_scene_id: 0,
        cursor: None,
        selected_range: None,
        focus: None,
        display: None,
        expected_project_epoch: EPOCH.to_string(),
    };
    let error = params.validate().unwrap_err();
    assert_eq!(
        error,
        EditInputError::NoChangeRequested {
            fields: &["cursor", "selected_range", "focus", "display"],
        }
    );
    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);

    assert_eq!(sample_set_selection().validate(), Ok(()));
    assert_eq!(
        SetSelectionParams {
            cursor: None,
            selected_range: None,
            focus: Some(FocusChange::Clear {}),
            display: None,
            ..sample_set_selection()
        }
        .validate(),
        Ok(())
    );
    // 表示開始位置だけの指定でも変更要求として成立する。
    assert_eq!(
        SetSelectionParams {
            cursor: None,
            selected_range: None,
            focus: None,
            display: Some(DisplayStart {
                layer: 3,
                frame: 90,
            }),
            ..sample_set_selection()
        }
        .validate(),
        Ok(())
    );
}

#[test]
fn set_selection_rejects_a_display_start_outside_the_transferable_range() {
    let over = MAX_POSITION + 1;
    for (display, field) in [
        (
            DisplayStart {
                layer: over,
                frame: 0,
            },
            FIELD_LAYER,
        ),
        (
            DisplayStart {
                layer: 0,
                frame: over,
            },
            FIELD_FRAME,
        ),
    ] {
        assert_eq!(
            SetSelectionParams {
                cursor: None,
                selected_range: None,
                focus: None,
                display: Some(display),
                ..sample_set_selection()
            }
            .validate(),
            Err(EditInputError::PositionOutOfRange {
                field,
                value: over,
                max: MAX_POSITION,
            })
        );
    }
}

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

#[test]
fn create_validates_the_source() {
    assert_eq!(sample_create().validate(), Ok(()));

    assert_eq!(
        CreateObjectParams {
            source: ObjectSource::MediaFile {
                path: r"..\movie.mp4".to_string(),
            },
            ..sample_create()
        }
        .validate(),
        Err(EditInputError::Path {
            field: FIELD_PATH,
            source: PathSyntaxError::NotAbsolute,
        })
    );

    let path = format!(r"C:\{}", "a".repeat(MAX_PATH_UTF16_UNITS));
    assert!(matches!(
        CreateObjectParams {
            source: ObjectSource::MediaFile { path },
            ..sample_create()
        }
        .validate(),
        Err(EditInputError::Path {
            source: PathSyntaxError::TooLong { .. },
            ..
        })
    ));

    assert_eq!(
        CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "a".repeat(MAX_ALIAS_BYTES + 1),
            },
            ..sample_create()
        }
        .validate(),
        Err(EditInputError::Text {
            field: FIELD_ALIAS,
            source: TextSyntaxError::TooLongBytes {
                bytes: MAX_ALIAS_BYTES + 1,
                max: MAX_ALIAS_BYTES,
            },
        })
    );
    assert_eq!(
        CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "a".repeat(MAX_ALIAS_BYTES),
            },
            ..sample_create()
        }
        .validate(),
        Ok(())
    );
}

#[test]
fn create_validates_the_effect_name_by_the_same_rule_as_add_effect() {
    // 名前の規則が作成元と effect の付与で食い違うと、同じ名前が片方でだけ
    // 通る。上限は UTF-16 code unit で数える。
    let over = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2 + 1);
    let at_limit = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2);
    for name in [over.clone(), at_limit.clone(), "図形\0".to_string()] {
        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::Effect { name: name.clone() },
                ..sample_create()
            }
            .validate()
            .map_err(|error| match error {
                EditInputError::Text { source, .. } => source,
                other => panic!("{other:?}"),
            }),
            AddEffectParams {
                object: sample_object_selector(),
                effect_name: name.clone(),
            }
            .validate()
            .map_err(|error| match error {
                EditInputError::Text { source, .. } => source,
                other => panic!("{other:?}"),
            }),
            "{name:?}"
        );
    }

    assert_eq!(
        CreateObjectParams {
            source: ObjectSource::Effect {
                name: "図形\0".to_string(),
            },
            ..sample_create()
        }
        .validate(),
        Err(EditInputError::Text {
            field: FIELD_NAME,
            source: TextSyntaxError::ContainsNul,
        })
    );
    assert!(matches!(
        CreateObjectParams {
            source: ObjectSource::Effect { name: over },
            ..sample_create()
        }
        .validate(),
        Err(EditInputError::Text {
            field: FIELD_NAME,
            source: TextSyntaxError::TooLongUtf16 { .. },
        })
    ));
    assert_eq!(
        CreateObjectParams {
            source: ObjectSource::Effect { name: at_limit },
            ..sample_create()
        }
        .validate(),
        Ok(())
    );
}

#[test]
fn the_alias_name_source_goes_through_the_alias_name_rules() {
    // 名前はファイル名の一部になる。禁止文字を拒めばディレクトリの外を指す
    // 名前は残らないが、規則は連結より先に掛かっていなければならない。
    for (name, expected) in [
        ("テストエイリアス", None),
        ("", Some(TextSyntaxError::Empty)),
        ("..", Some(TextSyntaxError::ForbiddenCharacter)),
        (r"..\..\x", Some(TextSyntaxError::ForbiddenCharacter)),
        ("a/b", Some(TextSyntaxError::ForbiddenCharacter)),
        (r"C:\x", Some(TextSyntaxError::ForbiddenCharacter)),
        ("図形\0", Some(TextSyntaxError::ContainsNul)),
        ("図形\u{1}", Some(TextSyntaxError::ContainsControl)),
    ] {
        let result = CreateObjectParams {
            source: ObjectSource::AliasName {
                name: name.to_string(),
            },
            ..sample_create()
        }
        .validate();
        match expected {
            None => assert_eq!(result, Ok(()), "{name:?}"),
            Some(source) => assert_eq!(
                result,
                Err(EditInputError::Text {
                    field: FIELD_NAME,
                    source,
                }),
                "{name:?}"
            ),
        }
    }

    // effect 名は 1,024 UTF-16 code units を上限とする。エイリアス名も同じ
    // 上限を共有する。
    assert!(matches!(
        CreateObjectParams {
            source: ObjectSource::AliasName {
                name: "あ".repeat(MAX_NAME_UTF16_UNITS + 1),
            },
            ..sample_create()
        }
        .validate(),
        Err(EditInputError::Text {
            field: FIELD_NAME,
            source: TextSyntaxError::TooLongUtf16 { .. },
        })
    ));
}

#[test]
fn the_alias_name_source_is_stricter_than_the_effect_name_source() {
    // 生テキストと effect 名は禁止文字を持たない。エイリアス名だけが追加の
    // 規則を負う。片方だけに規則が掛かっていることを 1 つの比較で残す。
    for name in [r"..\図形", r"C:\図形:1", "図形.1"] {
        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::Effect {
                    name: name.to_string(),
                },
                ..sample_create()
            }
            .validate(),
            Ok(()),
            "{name}"
        );
        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::AliasName {
                    name: name.to_string(),
                },
                ..sample_create()
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_NAME,
                source: TextSyntaxError::ForbiddenCharacter,
            }),
            "{name}"
        );
    }
}

#[test]
fn the_effect_source_is_not_subject_to_the_path_rules() {
    // 作成元がパスを運ばない以上、パスの規則は掛からない。掛かると、
    // パスとしては不正な文字列を名前に持つ effect を作成元にできなくなる。
    for name in [
        r"..\図形",
        r"\\.\図形",
        r"C:\図形:1",
        r"\\server\share\図形",
        "図形",
    ] {
        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::Effect {
                    name: name.to_string(),
                },
                ..sample_create()
            }
            .validate(),
            Ok(()),
            "{name}"
        );
    }
}

#[test]
fn path_rules_apply_to_every_field_that_carries_a_path() {
    // 作成元のパスと設定値のパスは別の型を通るため、規則が片方だけに
    // 掛かっていても個別のテストでは気付けない。
    for (path, expected) in [
        ("", PathSyntaxError::Empty),
        (r"..\movie.mp4", PathSyntaxError::NotAbsolute),
        (r"\\.\pipe\aviutl2", PathSyntaxError::DeviceNamespace),
        (r"C:\movie.mp4:stream", PathSyntaxError::AlternateDataStream),
        (r"\\server\share\movie.mp4", PathSyntaxError::UncPath),
        ("//server/share/movie.mp4", PathSyntaxError::UncPath),
    ] {
        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::MediaFile {
                    path: path.to_string(),
                },
                ..sample_create()
            }
            .validate(),
            Err(EditInputError::Path {
                field: FIELD_PATH,
                source: expected,
            }),
            "作成元の {path}"
        );

        for value in [
            ItemValue::File {
                path: path.to_string(),
            },
            ItemValue::Folder {
                path: path.to_string(),
            },
        ] {
            let kind = value.kind();
            assert_eq!(
                SetObjectItemParams {
                    value,
                    ..sample_set_object_item()
                }
                .validate(),
                Err(EditInputError::ItemValue(ItemWriteError::Path(expected))),
                "{kind} の {path}"
            );
        }
    }
}

#[test]
fn paths_reject_control_characters_on_top_of_the_syntax_rules() {
    // ファイル名に現れ得ない文字は、構文の規則を通っても渡さない。
    for control in ['\u{1}', '\u{1b}', '\n', '\t'] {
        let path = format!(r"C:\movie{control}.mp4");
        assert_eq!(
            validate_path(&path),
            Ok(()),
            "{control:?} が構文の規則で落ちています"
        );

        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::MediaFile { path: path.clone() },
                ..sample_create()
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_PATH,
                source: TextSyntaxError::ContainsControl,
            }),
            "作成元の {control:?}"
        );

        assert_eq!(
            SetObjectItemParams {
                value: ItemValue::File { path },
                ..sample_set_object_item()
            }
            .validate(),
            Err(EditInputError::ItemValue(ItemWriteError::Text(
                TextSyntaxError::ContainsControl
            ))),
            "設定値の {control:?}"
        );
    }
}

#[test]
fn media_file_path_is_bounded_only_by_the_path_limit() {
    // 作成元のパスは設定項目の値ではないため、値としての上限は掛からない。
    let path = format!(r"C:\{}", "a".repeat(MAX_PATH_UTF16_UNITS - 3));
    assert_eq!(path.encode_utf16().count(), MAX_PATH_UTF16_UNITS);
    assert_eq!(
        CreateObjectParams {
            source: ObjectSource::MediaFile { path },
            ..sample_create()
        }
        .validate(),
        Ok(())
    );
}

#[test]
fn names_are_limited_in_utf16_code_units() {
    let name = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2 + 1);
    assert!(matches!(
        SetObjectNameParams {
            selector: sample_object_selector(),
            name: Some(name.clone()),
        }
        .validate(),
        Err(EditInputError::Text {
            field: FIELD_NAME,
            source: TextSyntaxError::TooLongUtf16 { .. },
        })
    ));
    assert!(matches!(
        AddEffectParams {
            object: sample_object_selector(),
            effect_name: name.clone(),
        }
        .validate(),
        Err(EditInputError::Text {
            field: FIELD_EFFECT_NAME,
            source: TextSyntaxError::TooLongUtf16 { .. },
        })
    ));
    assert!(matches!(
        SetObjectItemParams {
            item: name,
            ..sample_set_object_item()
        }
        .validate(),
        Err(EditInputError::Text {
            field: FIELD_ITEM,
            source: TextSyntaxError::TooLongUtf16 { .. },
        })
    ));

    let name = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2);
    assert_eq!(
        SetObjectNameParams {
            selector: sample_object_selector(),
            name: Some(name),
        }
        .validate(),
        Ok(())
    );
}

#[test]
fn set_object_item_rejects_unknown_values() {
    let error = SetObjectItemParams {
        value: ItemValue::Unknown {
            raw: "future=1".to_string(),
        },
        ..sample_set_object_item()
    }
    .validate()
    .unwrap_err();
    assert_eq!(
        error,
        EditInputError::ItemValue(ItemWriteError::UnknownValue)
    );
    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
}

/// 各コンストラクタが埋めるフィールドの組み合わせを固定する。
///
/// 固定するのは**コンストラクタの契約だけ**である。どの operation が
/// どのコンストラクタを呼ぶかは応答を組み立てる側にあり、ここでは
/// 検証できない。表の operation 名は、どの契約がどの用途に対応するかを
/// 読み手へ示すための注記である。
#[test]
fn edit_outcome_matches_the_operation_table() {
    let created = vec![sample_summary(), sample_summary()];
    // operation ごとの object / effect / created の設定内容。
    let cases: Vec<(&str, EditOutcome, bool, bool, usize)> = vec![
        (
            "create_object",
            EditOutcome::created(EPOCH, 43, created.clone()),
            true,
            false,
            2,
        ),
        (
            "move_object",
            EditOutcome::object_changed(EPOCH, 43, sample_summary()),
            true,
            false,
            0,
        ),
        (
            "delete_object",
            EditOutcome::deleted(EPOCH, 43),
            false,
            false,
            0,
        ),
        (
            "set_object_name",
            EditOutcome::object_changed(EPOCH, 43, sample_summary()),
            true,
            false,
            0,
        ),
        (
            "set_object_item",
            EditOutcome::effect_changed(EPOCH, 43, sample_summary(), sample_effect_info()),
            true,
            true,
            0,
        ),
        (
            "add_effect",
            EditOutcome::effect_changed(EPOCH, 43, sample_summary(), sample_effect_info()),
            true,
            true,
            0,
        ),
        (
            "delete_effect",
            EditOutcome::object_changed(EPOCH, 43, sample_summary()),
            true,
            false,
            0,
        ),
        (
            "set_effect_enabled",
            EditOutcome::effect_changed(EPOCH, 43, sample_summary(), sample_effect_info()),
            true,
            true,
            0,
        ),
    ];

    for (operation, outcome, has_object, has_effect, created_count) in cases {
        assert_eq!(outcome.object.is_some(), has_object, "{operation}: object");
        assert_eq!(outcome.effect.is_some(), has_effect, "{operation}: effect");
        assert_eq!(outcome.created.len(), created_count, "{operation}: created");
        assert_eq!(outcome.project_epoch, EPOCH, "{operation}");
        assert_eq!(outcome.project_revision, 43, "{operation}");
    }
}

#[test]
fn created_outcome_points_at_the_first_object() {
    let created = vec![sample_summary(), sample_summary()];
    let outcome = EditOutcome::created(EPOCH, 43, created.clone());
    assert_eq!(outcome.object.as_ref(), created.first());
    assert_eq!(outcome.created, created);

    // 作成された件数が 0 の場合は対象を名乗らない。
    let empty = EditOutcome::created(EPOCH, 43, Vec::new());
    assert_eq!(empty.object, None);
    assert!(empty.created.is_empty());
}

#[test]
fn results_keep_reporting_the_project_generation() {
    // 要求から外した値でも、応答が返し続けるものはある。要求のフィールドは
    // 要求元へ組み立てを強いるが、応答のフィールドは強いない。revision は
    // `modified` の状態と変更の発生を要求元が観測する唯一の手段である。
    let outcome = serde_json::to_value(EditOutcome::effect_changed(
        EPOCH,
        43,
        sample_summary(),
        sample_effect_info(),
    ))
    .unwrap();
    assert_eq!(outcome["project_epoch"], json!(EPOCH));
    assert_eq!(outcome["project_revision"], json!(43));

    let state = serde_json::to_value(SelectionState::observed(
        EPOCH,
        42,
        ObservedSelection {
            cursor: Cursor { frame: 0, layer: 0 },
            selected_range: None,
            focus: Some(sample_summary()),
            display: sample_display_range(),
        },
        Vec::new(),
        Vec::new(),
    ))
    .unwrap();
    assert_eq!(state["project_epoch"], json!(EPOCH));
    assert_eq!(state["project_revision"], json!(42));
}

#[test]
fn results_roundtrip() {
    let outcome = EditOutcome::effect_changed(EPOCH, 43, sample_summary(), sample_effect_info());
    let s = serde_json::to_string(&outcome).unwrap();
    assert_eq!(serde_json::from_str::<EditOutcome>(&s).unwrap(), outcome);

    let state = SelectionState::observed(
        EPOCH,
        42,
        ObservedSelection {
            cursor: Cursor {
                frame: 120,
                layer: 2,
            },
            selected_range: Some(FrameRange { start: 10, end: 20 }),
            focus: Some(sample_summary()),
            display: sample_display_range(),
        },
        vec![SelectionField::Cursor, SelectionField::Focus],
        Vec::new(),
    );
    let s = serde_json::to_string(&state).unwrap();
    assert_eq!(serde_json::from_str::<SelectionState>(&s).unwrap(), state);
}

#[test]
fn results_allow_unknown_optional_fields() {
    // 応答型は将来の optional field を受け入れる。
    let outcome = EditOutcome::deleted(EPOCH, 43);
    let restored: EditOutcome = serde_json::from_value(with_unknown_field(&outcome)).unwrap();
    assert_eq!(restored, outcome);

    let state = SelectionState::observed(
        EPOCH,
        42,
        ObservedSelection {
            cursor: Cursor { frame: 0, layer: 0 },
            selected_range: None,
            focus: None,
            display: sample_display_range(),
        },
        Vec::new(),
        Vec::new(),
    );
    let restored: SelectionState = serde_json::from_value(with_unknown_field(&state)).unwrap();
    assert_eq!(restored, state);
}

#[test]
fn results_do_not_expose_handles() {
    let documents = [
        serde_json::to_string(&EditOutcome::created(EPOCH, 43, vec![sample_summary()])).unwrap(),
        serde_json::to_string(&EditOutcome::effect_changed(
            EPOCH,
            43,
            sample_summary(),
            sample_effect_info(),
        ))
        .unwrap(),
        serde_json::to_string(&EditOutcome::deleted(EPOCH, 43)).unwrap(),
        serde_json::to_string(&SelectionState::observed(
            EPOCH,
            42,
            ObservedSelection {
                cursor: Cursor { frame: 0, layer: 0 },
                selected_range: Some(FrameRange { start: 0, end: 1 }),
                focus: Some(sample_summary()),
                display: sample_display_range(),
            },
            vec![SelectionField::Focus],
            Vec::new(),
        ))
        .unwrap(),
    ];

    for document in documents {
        let lowered = document.to_lowercase();
        for forbidden in ["handle", "pointer", "0x", "secret", "nonce"] {
            assert!(
                !lowered.contains(forbidden),
                "{forbidden} が応答に含まれます: {document}"
            );
        }
    }
}

#[test]
fn a_selector_position_out_of_range_is_an_invalid_argument() {
    // 往復型だから正常な値は範囲内に収まる、というのは信頼の前提であって
    // 検証ではない。範囲外をそのまま解決へ渡すと、対象の探索が整数変換で
    // 落ちて SDK の失敗として返り、呼ばれてもいない関数が名指しされる。
    let out_of_range = MAX_POSITION as usize + 1;

    let mut selector = sample_object_selector();
    selector.layer = out_of_range;
    let error = MoveObjectParams {
        selector: selector.clone(),
        destination: Destination {
            layer: 1,
            frame: 10,
        },
    }
    .validate()
    .expect_err("範囲外のレイヤー番号が受理されました");
    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);

    let mut selector = sample_object_selector();
    selector.frame = out_of_range;
    let error = DeleteObjectParams { selector }
        .validate()
        .expect_err("範囲外の開始フレーム番号が受理されました");
    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
}

#[test]
fn every_edit_input_checks_the_selectors_it_carries() {
    // ネストしたセレクターだけが検証を免れると、そこから範囲外の値が
    // 解決へ抜ける。
    let out_of_range = MAX_POSITION as usize + 1;
    let mut object = sample_object_selector();
    object.layer = out_of_range;
    let mut effect = sample_effect_selector();
    effect.object.layer = out_of_range;

    let failures: Vec<Result<(), EditInputError>> = vec![
        SetObjectNameParams {
            selector: object.clone(),
            name: None,
        }
        .validate(),
        SetObjectItemParams {
            selector: effect.clone(),
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 1 },
        }
        .validate(),
        AddEffectParams {
            object: object.clone(),
            effect_name: "ぼかし".to_string(),
        }
        .validate(),
        DeleteEffectParams {
            selector: effect.clone(),
        }
        .validate(),
        SetEffectEnabledParams {
            selector: effect.clone(),
            enabled: true,
        }
        .validate(),
        MoveEffectParams {
            selector: effect,
            position: 0,
        }
        .validate(),
        SetSelectionParams {
            expected_scene_id: 0,
            cursor: None,
            selected_range: None,
            focus: Some(FocusChange::Set { object }),
            display: None,
            expected_project_epoch: EPOCH.to_string(),
        }
        .validate(),
    ];

    for failure in failures {
        let error = failure.expect_err("範囲外のセレクターが受理されました");
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    }
}

#[test]
fn move_effect_params_only_bound_the_destination() {
    // 列の長さとの比較は対象の現在の状態を要する。要求内容だけの検証は、
    // 移動先が受け渡せる範囲に収まることまでしか見ない。
    sample_move_effect()
        .validate()
        .expect("移動先の位置が拒否されました");

    let error = MoveEffectParams {
        position: MAX_POSITION as usize + 1,
        ..sample_move_effect()
    }
    .validate()
    .expect_err("i32 に収まらない移動先が受理されました");
    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    assert!(
        matches!(
            error,
            EditInputError::IndexOutOfRange {
                field: FIELD_POSITION,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn move_effect_params_reject_a_negative_destination() {
    // 負値は usize へ復号できない。実行口へ届く前に落ちる。
    let mut value = serde_json::to_value(sample_move_effect()).unwrap();
    value["position"] = json!(-1);
    assert!(serde_json::from_value::<MoveEffectParams>(value).is_err());
}

fn sample_create_section() -> CreateObjectSectionParams {
    CreateObjectSectionParams {
        selector: sample_object_selector(),
        frame: 180,
    }
}

fn sample_delete_section() -> DeleteObjectSectionParams {
    DeleteObjectSectionParams {
        selector: sample_object_selector(),
        section: 1,
    }
}

fn sample_move_section() -> MoveObjectSectionParams {
    MoveObjectSectionParams {
        selector: sample_object_selector(),
        section: 1,
        frame: 200,
    }
}

#[test]
fn object_section_params_roundtrip() {
    assert_roundtrip(sample_create_section());
    assert_roundtrip(sample_delete_section());
    assert_roundtrip(sample_move_section());
}

#[test]
fn object_section_params_reject_unknown_fields() {
    assert!(
        serde_json::from_value::<CreateObjectSectionParams>(with_unknown_field(
            &sample_create_section()
        ))
        .is_err()
    );
    assert!(
        serde_json::from_value::<DeleteObjectSectionParams>(with_unknown_field(
            &sample_delete_section()
        ))
        .is_err()
    );
    assert!(
        serde_json::from_value::<MoveObjectSectionParams>(with_unknown_field(
            &sample_move_section()
        ))
        .is_err()
    );
}

#[test]
fn object_section_params_reject_a_negative_number() {
    // 負値は u32 へ復号できない。実行口へ届く前に落ちる。
    let mut value = serde_json::to_value(sample_move_section()).unwrap();
    value["frame"] = json!(-1);
    assert!(serde_json::from_value::<MoveObjectSectionParams>(value).is_err());

    let mut value = serde_json::to_value(sample_delete_section()).unwrap();
    value["section"] = json!(-1);
    assert!(serde_json::from_value::<DeleteObjectSectionParams>(value).is_err());
}

#[test]
fn section_zero_is_rejected_as_an_invalid_argument() {
    // 区間 0 の開始位置はオブジェクトの開始フレームであって中間点ではない。
    // 対象を読み直しても 0 が有効になることはないため、前提条件の不整合では
    // なく要求の誤りとして返す。
    for error in [
        DeleteObjectSectionParams {
            section: 0,
            ..sample_delete_section()
        }
        .validate()
        .expect_err("区間番号 0 の削除が受理されました"),
        MoveObjectSectionParams {
            section: 0,
            ..sample_move_section()
        }
        .validate()
        .expect_err("区間番号 0 の移動が受理されました"),
    ] {
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
        assert_eq!(error.reason(), Some("section_index_out_of_range"));
        assert!(REASON_VALUES.contains(&"section_index_out_of_range"));
    }
}

#[test]
fn section_one_is_accepted_without_knowing_the_object() {
    // 区間の総数との比較は対象の現在の状態を要する。要求内容だけの検証は
    // そこまで見ない。
    sample_delete_section()
        .validate()
        .expect("区間番号 1 の削除が拒否されました");
    sample_move_section()
        .validate()
        .expect("区間番号 1 の移動が拒否されました");
    sample_create_section()
        .validate()
        .expect("中間点の追加が拒否されました");
}

#[test]
fn object_section_params_reject_values_beyond_i32() {
    for error in [
        CreateObjectSectionParams {
            frame: MAX_POSITION + 1,
            ..sample_create_section()
        }
        .validate()
        .expect_err("i32 に収まらないフレームが受理されました"),
        DeleteObjectSectionParams {
            section: MAX_POSITION + 1,
            ..sample_delete_section()
        }
        .validate()
        .expect_err("i32 に収まらない区間番号が受理されました"),
        MoveObjectSectionParams {
            frame: MAX_POSITION + 1,
            ..sample_move_section()
        }
        .validate()
        .expect_err("i32 に収まらないフレームが受理されました"),
    ] {
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    }
}

#[test]
fn object_sections_outcome_roundtrip() {
    let outcome = ObjectSectionsOutcome {
        project_epoch: EPOCH.to_string(),
        project_revision: 43,
        object: sample_summary(),
        sections: vec![
            SectionRange {
                start: 120,
                end: 179,
            },
            SectionRange {
                start: 180,
                end: 240,
            },
        ],
    };
    let s = serde_json::to_string(&outcome).unwrap();
    let restored: ObjectSectionsOutcome = serde_json::from_str(&s).unwrap();
    assert_eq!(restored, outcome);
}

#[test]
fn object_sections_outcome_carries_no_alias() {
    // 応答が返すのは概要であり詳細ではない。alias も設定値も載らない。
    let value = serde_json::to_value(ObjectSectionsOutcome {
        project_epoch: EPOCH.to_string(),
        project_revision: 43,
        object: sample_summary(),
        sections: Vec::new(),
    })
    .unwrap();
    assert!(value.get("alias").is_none());
    assert!(value["object"].get("alias").is_none());
}

fn finite(value: f64) -> FiniteF64 {
    FiniteF64::try_new(value).expect("有限値")
}

fn bpm(tempo: f64, beat: i64, start: f64, offset: f64) -> GridBpm {
    GridBpm {
        tempo: finite(tempo),
        beat,
        start: finite(start),
        offset: finite(offset),
    }
}

fn set_grid_bpm(entries: Vec<GridBpm>) -> SetGridBpmParams {
    SetGridBpmParams {
        expected_scene_id: 0,
        entries,
        expected_project_epoch: EPOCH.to_string(),
    }
}

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

fn sample_set_scene_settings() -> SetSceneSettingsParams {
    SetSceneSettingsParams {
        expected_scene_id: 0,
        name: Some("本編".to_string()),
        size: Some(SceneSize {
            width: 1920,
            height: 1080,
        }),
        sample_rate: Some(48_000),
        expected_project_epoch: EPOCH.to_string(),
    }
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
