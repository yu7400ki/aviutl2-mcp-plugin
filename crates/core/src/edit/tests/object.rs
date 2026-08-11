//! オブジェクトを対象とする params の検査。

use super::*;

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

#[test]
fn set_object_item_bounds_integers_to_a_signed_32bit_integer() {
    for value in [i64::from(i32::MIN), i64::from(i32::MAX)] {
        assert_eq!(
            SetObjectItemParams {
                value: ItemValue::Integer { value },
                ..sample_set_object_item()
            }
            .validate(),
            Ok(()),
            "{value}"
        );
    }

    for value in [i64::from(i32::MIN) - 1, i64::from(i32::MAX) + 1] {
        let error = SetObjectItemParams {
            value: ItemValue::Integer { value },
            ..sample_set_object_item()
        }
        .validate()
        .expect_err("幅を外れた整数が受理されました");
        assert_eq!(
            error,
            EditInputError::ItemValue(ItemWriteError::IntegerNotRepresentable),
            "{value}"
        );
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument, "{value}");
        assert_eq!(
            error.reason(),
            Some("argument_not_representable"),
            "{value}"
        );
    }
}
