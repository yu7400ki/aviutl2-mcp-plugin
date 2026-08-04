//! ダイジェストの文字列表現。
//!
//! 対象の同一性検証に用いる fingerprint と、成果物の内容ダイジェストは別々の
//! 契約だが、**表現の形は共通である**——いずれも `"sha256:"` に小文字十六進を
//! 続ける。形を複数の場所へ書くと、片方だけが変わっても誰も落ちない。
//!
//! **形を共有することは、両者を同じものとして扱うことではない。** 値の形が
//! 同じであることは同じものであることを意味しないため、型は分けたままにする。

/// SHA-256 のダイジェストであることを示す前置文字列。
pub const SHA256_PREFIX: &str = "sha256:";

/// SHA-256 ダイジェストの十六進表現の桁数。
pub const SHA256_HEX_LEN: usize = 64;

/// ダイジェストを `"sha256:" + 小文字十六進` へ整形する。
pub fn format_sha256(digest: &[u8]) -> String {
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    value.push_str(&to_hex(digest));
    value
}

/// バイト列を小文字十六進へ変換する。
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_hex_is_lowercase_and_padded() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }

    #[test]
    fn a_formatted_digest_carries_the_prefix_and_lowercase_hex() {
        let formatted = format_sha256(&[0xab; 32]);
        assert_eq!(formatted, format!("sha256:{}", "ab".repeat(32)));
        let hex = formatted.strip_prefix(SHA256_PREFIX).unwrap();
        assert_eq!(hex.len(), SHA256_HEX_LEN);
    }
}
