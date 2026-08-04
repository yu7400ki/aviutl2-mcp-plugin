//! `details.reason` の値域と、応答へ実際に載る名前が一致することを固定する。
//!
//! 値域の正本は core にあるが、名前を生む型はこの crate にも居る。core の型
//! だけを見る片側の検査では、plugin 側の名前が一覧から外れても気付けない。
//!
//! 縛るのは 2 方向である。**一覧に無い名前がワイヤへ出ないこと**と、
//! **生成経路の無い名前が一覧に残らないこと**。前者だけでは使われなくなった
//! 名前が積もり、後者だけでは新しい名前が黙って増える。

use crate::edit::error::{
    EditError, NotIssuedReason, SectionPreconditionReason, UnsupportedReason,
};
use crate::render::error::{BufferRule, RenderError};
use crate::session::{batch_input_error, edit_input_error};
use aviutl2_mcp_core::error::REASON_VALUES;
use aviutl2_mcp_core::{
    BatchInputError, EditInputError, ItemWriteError, PathSyntaxError, TextSyntaxError,
};
use serde_json::Value;
use std::collections::BTreeSet;

/// 名前を持ち得る編集の失敗を、種別ごとに 1 つずつ作る。
///
/// 代表値の一覧に加え、名前が値から決まる失敗を全通り並べる。並べなければ、
/// 名前を生む型に variant を足しても補助情報の側では気付けない。
fn edit_failures() -> Vec<EditError> {
    crate::edit::error::tests::all_errors()
        .into_iter()
        .chain(
            UnsupportedReason::ALL
                .iter()
                .map(|reason| EditError::UnsupportedTarget { reason: *reason }),
        )
        .chain(
            NotIssuedReason::ALL
                .iter()
                .map(|reason| EditError::NotIssued { reason: *reason }),
        )
        .chain(
            SectionPreconditionReason::ALL
                .iter()
                .map(|reason| EditError::SectionPrecondition { reason: *reason }),
        )
        .chain(
            PathSyntaxError::ALL
                .iter()
                .map(|source| EditError::ItemWrite(ItemWriteError::Path(*source))),
        )
        .chain(
            TextSyntaxError::ALL
                .iter()
                .map(|source| EditError::ItemWrite(ItemWriteError::Text(*source))),
        )
        .chain(ItemWriteError::all().into_iter().map(EditError::ItemWrite))
        .collect()
}

/// 名前を持ち得るレンダリングの失敗を、種別ごとに 1 つずつ作る。
fn render_failures() -> Vec<RenderError> {
    crate::render::error::tests::all_errors()
        .into_iter()
        .chain(
            BufferRule::ALL
                .iter()
                .map(|rule| RenderError::InvalidBuffer { rule: *rule }),
        )
        .collect()
}

/// 応答へ実際に載る名前を、生成経路をすべて通して集める。
fn produced_reasons() -> BTreeSet<String> {
    fn reason_of(details: &Value) -> Option<String> {
        details
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    let mut reasons = BTreeSet::new();
    reasons.extend(
        edit_failures()
            .iter()
            .filter_map(|e| reason_of(&e.details())),
    );
    reasons.extend(
        render_failures()
            .iter()
            .filter_map(|e| reason_of(&e.details())),
    );
    reasons.extend(
        crate::read::error::tests::all_errors()
            .iter()
            .filter_map(|e| reason_of(&e.details())),
    );
    // 要求内容だけで決まる検証は実行口へ届く前に落ちるため、応答の組み立ても
    // 別の経路を通る。名前が届いているかは、その経路そのものを通して見る。
    reasons.extend(
        EditInputError::all()
            .into_iter()
            .filter_map(|error| reason_of(&edit_input_error(error).details)),
    );
    reasons.extend(
        BatchInputError::all()
            .into_iter()
            .filter_map(|error| reason_of(&batch_input_error(error).details)),
    );
    reasons
}

#[test]
fn every_produced_reason_belongs_to_the_shared_value_set() {
    // 一覧に無い名前は、誰にも気付かれないままワイヤへ出る。
    for reason in produced_reasons() {
        assert!(
            REASON_VALUES.contains(&reason.as_str()),
            "{reason} が reason の値域にありません"
        );
    }
}

#[test]
fn every_value_in_the_shared_set_is_actually_produced() {
    // 生成経路の無い名前が一覧に残ると、要求元は現れ得ない分岐を書き続ける。
    let produced = produced_reasons();
    for reason in REASON_VALUES {
        assert!(
            produced.contains(*reason),
            "{reason} を生む経路がありません"
        );
    }
}

#[test]
fn the_shared_value_set_is_exactly_what_is_produced() {
    // 上の 2 つを合わせた形を 1 つの比較で残す。落ちたときに差分が読める。
    let produced = produced_reasons();
    let produced: Vec<&str> = produced.iter().map(String::as_str).collect();
    let declared: Vec<&str> = REASON_VALUES.to_vec();
    assert_eq!(produced, declared);
}
