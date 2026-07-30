//! レンダリング成果物の受け渡しと保管。
//!
//! 成果物の画像は AviUtl2 のプロセスで生まれ、ファイルとして引き渡される。
//! 画像には利用者のプロジェクトの内容が写るため、経路に置くディレクトリは
//! いずれも現在のユーザーへ限定した DACL を持つ。
//!
//! 引き渡しの応答が運ぶのは handoff token だけであり、パスもディレクトリも
//! 運ばれない。受け取る側は自分が持つ基底と解決済みの識別子からパスを
//! 組み立てる。
//!
//! **パスを組み立てる材料は要求経路から入らない。** token は
//! [`HandoffToken`] へ通した場合にのみパスの組み立てへ渡せるため、構文検証を
//! 経ていない文字列がファイル名になる経路が無い。

pub mod protected_dir;

use std::fmt;

/// handoff token の文字数（128 bit を小文字十六進で表した長さ）。
const HANDOFF_TOKEN_LEN: usize = 32;

/// 構文検証を通した handoff token。
///
/// 小文字十六進ちょうど [`HANDOFF_TOKEN_LEN`] 文字だけがこの型になる。
/// handoff ファイルのパスを組み立てる経路はこの型しか受け取らないため、
/// 検証を経ていない値が経路長・区切り文字・大小文字の違いを持ち込めない。
///
/// `Debug` は値を出さない。token は応答にもログにも現れてはならず、
/// これを含む構造体をそのまま記録した場合にも漏れないようにする。
#[derive(Clone, PartialEq, Eq)]
pub struct HandoffToken(String);

/// handoff token の書式違反。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("handoff token は 32 桁の小文字十六進である必要があります")]
pub struct HandoffTokenFormatError;

impl HandoffToken {
    /// 構文を検証して token を作る。
    ///
    /// 受け付けるのは `0-9` と `a-f` だけからなるちょうど 32 文字である。
    /// 長さ違い・大文字・区切り文字・`..`・空文字・十六進でない Unicode は
    /// いずれも拒否する。バイト単位で判定するため、非 ASCII の文字は
    /// 長さの一致にかかわらず十六進でないバイトとして落ちる。
    pub fn parse(value: &str) -> Result<Self, HandoffTokenFormatError> {
        let is_lower_hex = value.len() == HANDOFF_TOKEN_LEN
            && value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if is_lower_hex {
            Ok(Self(value.to_owned()))
        } else {
            Err(HandoffTokenFormatError)
        }
    }
}

impl fmt::Debug for HandoffToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HandoffToken(<redacted>)")
    }
}

#[cfg(test)]
mod tests;
