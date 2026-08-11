//! effect を対象とする params の検査。

use super::*;

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
