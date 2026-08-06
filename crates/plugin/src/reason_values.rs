//! `details.reason` の値域と、応答へ実際に載る名前が一致することを固定する。
//!
//! 値域の正本は core にあるが、名前を生む型はこの crate にも居る。core の型
//! だけを見る片側の検査では、plugin 側の名前が一覧から外れても気付けない。
//!
//! 縛るのは 2 方向である。**一覧に無い名前がワイヤへ出ないこと**と、
//! **生成経路の無い名前が一覧に残らないこと**。前者だけでは使われなくなった
//! 名前が積もり、後者だけでは新しい名前が黙って増える。
//!
//! 後者を担うのは、名前を持つ値を並べる一覧ではなく、**その値を実際に返した
//! 呼び出し**である。一覧を回して失敗値を組み立てると、名前を生む呼び出しが
//! 製品に 1 つも無くても検査が通る。したがってここへ集めるのは、検証関数・
//! 編集手順・SDK 失敗の写しが**返した**値だけである。

use crate::edit::error::EditError;
use crate::render::error::RenderError;
use crate::session::{batch_input_error, edit_input_error};
use aviutl2_mcp_core::error::REASON_VALUES;
use aviutl2_mcp_core::{
    BatchInputError, EditInputError, ItemWriteError, MAX_ITEM_VALUE_BYTES, MAX_NAME_UTF16_UNITS,
    MAX_PATH_UTF16_UNITS, PathSyntaxError, TextSyntaxError, validate_alias, validate_item_text,
    validate_multiline_item_text, validate_name, validate_object_alias_name, validate_path,
};
use serde_json::Value;
use std::collections::BTreeSet;
use std::mem::discriminant;

/// variant を実際に起こす検証の呼び出しを並べる。
///
/// [`TextSyntaxError`] に対する網羅 `match` であり `_` を使わない。**variant を
/// 足すとここが落ち、それを起こす入力を書くまでコンパイルできない。** 名前を
/// 数え上げるのは [`TextSyntaxError::ALL`] の役目であり、生成の証明は入力の側が
/// 持つ。
fn text_syntax_case(variant: &TextSyntaxError) -> Vec<TextSyntaxError> {
    let rejected = |result: Result<(), TextSyntaxError>| result.expect_err("受理されました");
    match variant {
        TextSyntaxError::Empty => vec![rejected(validate_object_alias_name(""))],
        TextSyntaxError::ContainsNul => vec![
            rejected(validate_item_text("字幕\0")),
            rejected(validate_name("立ち絵\0")),
            rejected(validate_alias("[vo]\0")),
        ],
        TextSyntaxError::ContainsControl => vec![
            rejected(validate_item_text("字幕\u{1b}")),
            rejected(validate_multiline_item_text("字幕\u{1}")),
        ],
        TextSyntaxError::ForbiddenCharacter => {
            vec![rejected(validate_object_alias_name("立ち絵/通常"))]
        }
        TextSyntaxError::LoneCarriageReturn => {
            vec![rejected(validate_multiline_item_text("1 行目\r2 行目"))]
        }
        TextSyntaxError::TooLongUtf16 { .. } => {
            vec![rejected(validate_name(
                &"a".repeat(MAX_NAME_UTF16_UNITS + 1),
            ))]
        }
        TextSyntaxError::TooLongBytes { .. } => vec![rejected(validate_item_text(
            &"a".repeat(MAX_ITEM_VALUE_BYTES + 1),
        ))],
    }
}

/// variant を実際に起こすパス検証の呼び出しを並べる。網羅 `match` の理由は同上。
fn path_syntax_case(variant: &PathSyntaxError) -> Vec<PathSyntaxError> {
    let rejected = |path: &str| validate_path(path).expect_err("受理されました");
    match variant {
        PathSyntaxError::Empty => vec![rejected("")],
        PathSyntaxError::ContainsNul => vec![rejected("C:\\movie\0.mp4")],
        PathSyntaxError::TooLong { .. } => {
            vec![rejected(&format!(
                r"C:\{}",
                "a".repeat(MAX_PATH_UTF16_UNITS)
            ))]
        }
        PathSyntaxError::DeviceNamespace => vec![rejected(r"\\.\PhysicalDrive0")],
        PathSyntaxError::AlternateDataStream => vec![rejected(r"C:\movie.mp4:stream")],
        PathSyntaxError::NotAbsolute => vec![rejected("movie.mp4")],
        PathSyntaxError::UncPath => vec![rejected(r"\\server\share")],
    }
}

/// 一覧の各 variant について、それを実際に返した呼び出しの結果を集める。
///
/// 起こす入力を持たない variant と、別の variant を返した入力をその場で落とす。
/// 値を持つ variant では payload が入力ごとに変わるため、突き合わせは
/// 判別子で行う。
fn produced_variants<T>(
    all: &[T],
    case: impl Fn(&T) -> Vec<T>,
    name: impl Fn(&T) -> &'static str,
) -> Vec<T> {
    let mut produced = Vec::new();
    for variant in all {
        let failures = case(variant);
        assert!(
            !failures.is_empty(),
            "{} を起こす入力がありません",
            name(variant)
        );
        for failure in &failures {
            assert_eq!(
                discriminant(failure),
                discriminant(variant),
                "{} を起こすはずの入力が別の失敗を返しました",
                name(variant)
            );
        }
        produced.extend(failures);
    }
    produced
}

/// 検証関数が実際に返した構文の失敗を、設定値の書き込みの失敗として並べる。
fn syntax_failures() -> Vec<ItemWriteError> {
    produced_variants(
        TextSyntaxError::ALL,
        text_syntax_case,
        TextSyntaxError::reason,
    )
    .into_iter()
    .map(ItemWriteError::Text)
    .chain(
        produced_variants(
            PathSyntaxError::ALL,
            path_syntax_case,
            PathSyntaxError::reason,
        )
        .into_iter()
        .map(ItemWriteError::Path),
    )
    .collect()
}

/// 名前を持ち得る編集の失敗を、種別ごとに 1 つずつ作る。
///
/// 代表値の一覧に加え、名前が値から決まる失敗を全通り並べる。並べなければ、
/// 名前を生む型に variant を足しても補助情報の側では気付けない。
fn edit_failures() -> Vec<EditError> {
    crate::edit::error::tests::all_errors()
        .into_iter()
        .chain(crate::edit::adapter::tests::unsupported_target_failures())
        .chain(crate::edit::adapter::tests::produced_item_value_mismatch_failures())
        .chain(crate::edit::adapter::tests::produced_movement_mismatch_failures())
        .chain(crate::edit::sdk::tests::failures_that_never_reached_the_sdk())
        .chain(crate::edit::adapter::tests::produced_section_precondition_failures())
        .chain(syntax_failures().into_iter().map(EditError::ItemWrite))
        .chain(ItemWriteError::all().into_iter().map(EditError::ItemWrite))
        // 受け入れ規則の失敗は一覧では捨てられ、作成でだけ応答へ載る。載る側の
        // 経路をここへ通さないと、名前は「生成経路の無い名前」として残る。
        .chain(
            crate::alias::tests::all_rejections()
                .into_iter()
                .map(EditError::AliasRejected),
        )
        .collect()
}

/// 名前を持ち得るレンダリングの失敗を、種別ごとに 1 つずつ作る。
fn render_failures() -> Vec<RenderError> {
    crate::render::error::tests::all_errors()
        .into_iter()
        .chain(
            crate::render::buffer::tests::broken_buffer_rules()
                .into_iter()
                .map(|rule| RenderError::InvalidBuffer { rule }),
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
fn every_syntax_variant_has_an_input_that_produces_it() {
    // 入力を書かないまま variant を足すと、応答に現れない名前が一覧へ残る。
    assert!(!syntax_failures().is_empty());
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
