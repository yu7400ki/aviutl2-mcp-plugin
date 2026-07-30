//! handoff token の単体テスト。

use super::*;
use proptest::prelude::*;

/// 有効な token を `seed` から作る。
fn token(seed: u8) -> String {
    format!("{seed:02x}").repeat(16)
}

#[test]
fn accepts_exactly_thirty_two_lowercase_hex_digits() {
    for value in [
        "0123456789abcdef0123456789abcdef",
        &"f".repeat(HANDOFF_TOKEN_LEN),
        &"0".repeat(HANDOFF_TOKEN_LEN),
    ] {
        assert!(
            HandoffToken::parse(value).is_ok(),
            "小文字十六進 32 文字は受け付ける: {value}"
        );
    }
}

#[test]
fn rejects_everything_but_thirty_two_lowercase_hex_digits() {
    let cases = [
        ("", "空文字"),
        ("0123456789abcdef0123456789abcde", "31 文字"),
        ("0123456789abcdef0123456789abcdef0", "33 文字"),
        ("0123456789ABCDEF0123456789abcdef", "大文字"),
        ("0123456789abcdef0123456789abcdeg", "十六進でない ASCII"),
        ("..", ".. だけ"),
        ("..\\..\\0123456789abcdef01234567", "区切りを含む相対経路"),
        ("../../0123456789abcdef0123456789", "スラッシュ区切り"),
        ("0123456789abcdef0123456789abcd:1", "ドライブ区切り"),
        ("0123456789abcdef0123456789abcd\0f", "NUL"),
        ("0123456789abcdef0123456789abcde\n", "改行"),
        ("０１２３４５６７８９abcdef0123456789", "全角数字"),
        ("0123456789abcdef0123456789abcde\u{0301}", "結合文字"),
        ("0123456789abcdef0123456789abcd\u{1F600}", "補助面の文字"),
    ];
    for (value, label) in cases {
        assert_eq!(
            HandoffToken::parse(value),
            Err(HandoffTokenFormatError),
            "{label} は拒否する"
        );
    }
}

#[test]
fn token_does_not_appear_in_its_debug_output() {
    // token は応答にもログにも現れてはならない。構造体ごと記録した場合にも
    // 漏れないよう、`Debug` は値を出さない。
    let value = token(0xab);
    let parsed = HandoffToken::parse(&value).unwrap();
    let rendered = format!("{parsed:?}");
    assert!(
        !rendered.contains(&value),
        "token が現れています: {rendered}"
    );
}

proptest! {
    /// 任意の文字列に対して panic せず、必ず可否を返す。
    #[test]
    fn token_parse_never_panics(value in ".*") {
        let parsed = HandoffToken::parse(&value);
        if parsed.is_ok() {
            prop_assert_eq!(value.len(), HANDOFF_TOKEN_LEN);
            prop_assert!(value.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        }
    }

    /// 十六進でない文字を混ぜた 32 文字は必ず拒否される。
    #[test]
    fn token_parse_rejects_non_hex_characters(
        prefix in "[0-9a-f]{0,31}",
        intruder in "[^0-9a-f]",
    ) {
        let mut value = prefix;
        value.push_str(&intruder);
        while value.chars().count() < HANDOFF_TOKEN_LEN {
            value.push('0');
        }
        prop_assert_eq!(HandoffToken::parse(&value), Err(HandoffTokenFormatError));
    }
}
