//! 編集 operation の params / result の検査。

use super::*;
use crate::edit_info::{Cursor, DisplayRange, FrameRange, SceneInfo};
use crate::error::REASON_VALUES;
use crate::fingerprint::{EffectFingerprintInput, ObjectFingerprintInput};
use crate::item_value::ItemValue;
use crate::object::{LayerInfo, SectionRange};
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

/// effect を対象とする params の検査。
mod effect;
/// レイヤーを対象とする params / result の検査。
mod layer;
/// オブジェクトを対象とする params の検査。
mod object;
/// シーン設定と BPM グリッドの params / result の検査。
mod scene;
/// 中間点を対象とする params / result の検査。
mod section;
/// カーソル・選択範囲・フォーカスの params の検査。
mod selection;
