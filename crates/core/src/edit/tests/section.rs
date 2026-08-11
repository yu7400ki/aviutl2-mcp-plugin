//! 中間点を対象とする params / result の検査。

use super::*;

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
