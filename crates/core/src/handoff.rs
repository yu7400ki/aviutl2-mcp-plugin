//! 描画の成果物をプロセスの外へ渡すための取り決め。
//!
//! 引き渡しは、書く側がファイルを置き、読む側が**同じ場所を自分で組み立てて**
//! 引き取る形で成立する。応答はパスを運ばないため、両者が同じ規則を持つことが
//! 唯一の接点である。ディレクトリ名・拡張子・識別子の構文のどれか 1 つでも
//! 食い違えば、読む側はファイルを見つけられない。
//!
//! **組み立ての順序も同じ性質を持つ。** 定数だけを揃えても、`{instance_id}` と
//! `{token}` の順序が入れ替われば同じように壊れる。そのため定数とパスの
//! 組み立てを 1 か所へ置き、両端がここだけを引く。
//!
//! パスの組み立ては OS の API を呼ばない。区切りの表記は動作環境に従う。

use crate::digest::to_hex;
use crate::identifier::InstanceId;
use rand::Rng;
use std::fmt;
use std::path::{Path, PathBuf};

/// 基底の直下に置く引き渡し用ディレクトリの名前。
pub const HANDOFF_DIR: &str = "render";

/// 成果物の拡張子。
pub const ARTIFACT_EXTENSION: &str = "png";

/// 書き込み途中のファイルに付ける拡張子。
pub const TEMP_EXTENSION: &str = "tmp";

/// 成果物の MIME type。
pub const ARTIFACT_MEDIA_TYPE: &str = "image/png";

/// 識別子の長さ（バイト）。
const TOKEN_BYTES: usize = 16;

/// 識別子の長さ（十六進表記の文字数）。
pub const HANDOFF_TOKEN_LEN: usize = TOKEN_BYTES * 2;

/// 構文検証を通した引き渡し用ファイルの識別子。
///
/// 小文字十六進ちょうど [`HANDOFF_TOKEN_LEN`] 文字だけがこの型になる。暗号論的に
/// 安全な乱数から作り、**推測できないことが必要である**——同じ利用者の別プロセスは
/// 信頼境界の内側にあるが、誤って別の成果物を読む事故は境界と無関係に起きる。
///
/// 引き渡し用ファイルのパスを組み立てる関数はこの型しか受け取らない。検証を経て
/// いない文字列が経路長・区切り文字・大小文字の違いを持ち込む余地が無い。
///
/// `Debug` は値を出さない。識別子は応答にもログにも現れてはならず、これを含む
/// 構造体をそのまま記録した場合にも漏れないようにする。
#[derive(Clone, PartialEq, Eq)]
pub struct HandoffToken(String);

/// 引き渡し用ファイルの識別子の書式違反。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("引き渡し用ファイルの識別子は 32 桁の小文字十六進である必要があります")]
pub struct HandoffTokenFormatError;

impl HandoffToken {
    /// 新しい識別子を作る。
    pub fn generate() -> Self {
        let mut bytes = [0u8; TOKEN_BYTES];
        rand::rng().fill_bytes(&mut bytes);
        Self(to_hex(&bytes))
    }

    /// 構文を検証して識別子を復元する。
    ///
    /// 受け付けるのは `0-9` と `a-f` だけからなるちょうど [`HANDOFF_TOKEN_LEN`]
    /// 文字である。長さ違い・大文字・区切り文字・`..`・空文字・十六進でない
    /// Unicode はいずれも拒否する。バイト単位で判定するため、非 ASCII の文字は
    /// 長さの一致にかかわらず十六進でないバイトとして落ちる。
    ///
    /// 復元の口を検証つきにしておくのは、**任意の文字列からファイルの場所を
    /// 組み立てる経路を作らない**ためである。
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

    /// 応答へ載せる文字列表現。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 成果物のファイル名。
    fn artifact_file_name(&self) -> String {
        format!("{}.{ARTIFACT_EXTENSION}", self.0)
    }

    /// 書き込み途中のファイルの名前。
    fn temp_file_name(&self) -> String {
        format!("{}.{ARTIFACT_EXTENSION}.{TEMP_EXTENSION}", self.0)
    }
}

impl fmt::Debug for HandoffToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HandoffToken(<redacted>)")
    }
}

/// 1 つの instance が使う引き渡し用ディレクトリを返す。
///
/// 基底の直下に [`HANDOFF_DIR`]、その下に instance ごとのディレクトリを置く。
pub fn handoff_dir(base: &Path, instance_id: &InstanceId) -> PathBuf {
    base.join(HANDOFF_DIR).join(instance_id.to_string())
}

/// 引き渡し用ファイルのパスを返す。
pub fn handoff_file(base: &Path, instance_id: &InstanceId, token: &HandoffToken) -> PathBuf {
    handoff_dir(base, instance_id).join(token.artifact_file_name())
}

/// 書き込み途中の引き渡し用ファイルのパスを返す。
///
/// 成果物と同じディレクトリに置く。名前の差し替えは同一ディレクトリの中でのみ
/// 原子的である。
pub fn handoff_temp_file(base: &Path, instance_id: &InstanceId, token: &HandoffToken) -> PathBuf {
    handoff_dir(base, instance_id).join(token.temp_file_name())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 固定の識別子。
    fn fixed_token() -> HandoffToken {
        HandoffToken::parse(&"5a".repeat(16)).expect("固定の識別子は構文を満たします")
    }

    /// パスを、区切りの表記に依存しない要素の並びとして取り出す。
    fn components(path: &Path) -> Vec<String> {
        path.components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_generated_token_is_thirty_two_lowercase_hex_characters() {
        let token = HandoffToken::generate();
        assert_eq!(token.as_str().len(), HANDOFF_TOKEN_LEN);
        assert!(
            token
                .as_str()
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "{}",
            token.as_str()
        );
    }

    #[test]
    fn a_generated_token_parses_back() {
        let token = HandoffToken::generate();
        assert_eq!(HandoffToken::parse(token.as_str()), Ok(token));
    }

    #[test]
    fn tokens_do_not_repeat() {
        let tokens: HashSet<String> = (0..64)
            .map(|_| HandoffToken::generate().as_str().to_string())
            .collect();
        assert_eq!(tokens.len(), 64, "識別子が重複しました");
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
        // 識別子は応答にもログにも現れてはならない。構造体ごと記録した場合にも
        // 漏れないよう、`Debug` は値を出さない。
        let token = fixed_token();
        let rendered = format!("{token:?}");
        assert!(
            !rendered.contains(token.as_str()),
            "識別子が現れています: {rendered}"
        );
    }

    #[test]
    fn a_handoff_file_sits_at_a_fixed_place_under_the_base() {
        // 組み立ての順序が入れ替われば、書く側と読む側は別の場所を指す。
        // 期待は要素の並びとして書く。順序が変われば落ちる。
        let instance_id = InstanceId::from_bytes([0x11; 16]);
        let token = fixed_token();
        let path = handoff_file(Path::new("base"), &instance_id, &token);

        assert_eq!(
            components(&path),
            vec![
                "base".to_string(),
                "render".to_string(),
                instance_id.to_string(),
                format!("{}.png", token.as_str()),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_handoff_file_is_a_backslash_separated_path() {
        // 区切りの表記は動作環境に従う。他の環境では要素の並びまでしか言えない
        // ため、文字列としての形はここでだけ固定する。
        let instance_id = InstanceId::from_bytes([0x11; 16]);
        let token = fixed_token();
        let path = handoff_file(Path::new("base"), &instance_id, &token);

        assert_eq!(
            path.to_str().expect("パスは UTF-8 で表せます"),
            format!("base\\render\\{}\\{}.png", instance_id, token.as_str())
        );
    }

    #[test]
    fn the_temp_file_sits_beside_the_artifact() {
        // 名前の差し替えが原子的であるのは同一ディレクトリの中だけである。
        let instance_id = InstanceId::from_bytes([0x22; 16]);
        let token = fixed_token();
        let artifact = handoff_file(Path::new("base"), &instance_id, &token);
        let temp = handoff_temp_file(Path::new("base"), &instance_id, &token);

        assert_eq!(artifact.parent(), temp.parent());
        assert_eq!(
            temp.file_name().and_then(|name| name.to_str()),
            Some(format!("{}.png.tmp", token.as_str()).as_str())
        );
    }

    #[test]
    fn the_directory_is_the_parent_of_the_files() {
        let instance_id = InstanceId::from_bytes([0x33; 16]);
        let token = fixed_token();
        let base = Path::new("base");

        assert_eq!(
            handoff_file(base, &instance_id, &token).parent(),
            Some(handoff_dir(base, &instance_id).as_path())
        );
        assert_eq!(
            handoff_temp_file(base, &instance_id, &token).parent(),
            Some(handoff_dir(base, &instance_id).as_path())
        );
    }

    #[test]
    fn each_instance_gets_its_own_directory() {
        let base = Path::new("base");
        assert_ne!(
            handoff_dir(base, &InstanceId::from_bytes([0x01; 16])),
            handoff_dir(base, &InstanceId::from_bytes([0x02; 16]))
        );
    }
}
