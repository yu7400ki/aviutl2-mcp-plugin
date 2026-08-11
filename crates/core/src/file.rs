//! ファイルの読み取り。
//!
//! 上限を掛けて読む共通処理と、パスで指定されたエイリアスファイルを作成元の
//! 本文として読む経路を持つ。後者は上限も失敗の名前もエイリアス固有である。

use crate::error::ErrorCode;
use crate::validation::MAX_ALIAS_BYTES;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// 上限を超えないことを確かめながらファイルを読む。
///
/// 上限は開いた直後の大きさで判定し、読み取り自体にも同じ上限を掛ける。判定と
/// 読み取りの間にファイルが伸びても、上限を超えて読むことはない。上限を超えて
/// いれば `Ok(None)` を返す。
pub fn read_bounded(path: &Path, limit: u64) -> std::io::Result<Option<Vec<u8>>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > limit {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Ok(None);
    }
    Ok(Some(bytes))
}

/// パスで指定されたエイリアスファイルを読めない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AliasFileError {
    /// 指定されたパスにファイルが無い。
    #[error("指定されたパスにエイリアスファイルがありません")]
    NotFound,
    /// ファイルはあるが読み取れない。
    #[error("エイリアスファイルを読み取れません")]
    Unreadable,
    /// 大きさが上限を超えている。
    #[error("エイリアスファイルが大きすぎます (上限 {MAX_ALIAS_BYTES} バイト)")]
    TooLarge,
    /// UTF-8 として解釈できない。
    #[error("エイリアスファイルを UTF-8 として解釈できません")]
    NotUtf8,
}

impl AliasFileError {
    /// 全 variant の代表値。
    ///
    /// [`AliasFileError::reason`] が返し得る名前を数え上げるために用いる。
    pub const ALL: &'static [AliasFileError] = &[
        AliasFileError::NotFound,
        AliasFileError::Unreadable,
        AliasFileError::TooLarge,
        AliasFileError::NotUtf8,
    ];

    /// 失敗の種別を表す機械可読な名前を返す。
    ///
    /// 名前はパスもファイルの内容も含まない。
    ///
    /// 上限超過と UTF-8 として読めない本文は、既にある名前を名乗る。前者は要求
    /// へ直接書かれた alias が上限を超えたときと同じ事実であり、後者は本文を
    /// 解釈できないという同じ事実である。
    ///
    /// 不在と読み取り不能は 1 つの種別へ畳まない。綴りの誤りと、権限や掴まれて
    /// いるファイルは、要求元にとって別の対処になる。**両者を分けるのは
    /// [`AliasFileError::error_code`] と、読み取り不能だけが名乗る名前の組で
    /// ある。** 不在は名前を持たない。
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            AliasFileError::NotFound => None,
            AliasFileError::Unreadable => Some("alias_file_unreadable"),
            AliasFileError::TooLarge => Some("too_long"),
            AliasFileError::NotUtf8 => Some("alias_not_parsable"),
        }
    }

    /// 応答へ載せるエラーコードを返す。
    ///
    /// 不在だけが [`ErrorCode::NotFound`] であり、残りは指定されたパスが作成元
    /// として使えないファイルを指していることを述べる。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            AliasFileError::NotFound => ErrorCode::NotFound,
            AliasFileError::Unreadable | AliasFileError::TooLarge | AliasFileError::NotUtf8 => {
                ErrorCode::InvalidArgument
            }
        }
    }
}

/// エイリアスファイルを読み、大きさと符号化を確かめる。
///
/// 確かめるのは [`MAX_ALIAS_BYTES`] を超えないことと UTF-8 として解釈できること
/// までであり、本文はそのまま返る。本文の構文は、読んだ文字列を作成元として
/// 組んだ後に [`ObjectSource::validate`](crate::edit::ObjectSource::validate) が
/// 掛ける。
pub fn read_alias_file(path: &Path) -> Result<String, AliasFileError> {
    let bytes = match read_bounded(path, MAX_ALIAS_BYTES as u64) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Err(AliasFileError::TooLarge),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AliasFileError::NotFound);
        }
        Err(_) => return Err(AliasFileError::Unreadable),
    };
    String::from_utf8(bytes).map_err(|_| AliasFileError::NotUtf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "aviutl2-mcp-file-test-{name}-{}",
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn a_file_within_the_limit_is_read_in_full() {
        let path = temp_path("within-limit");
        std::fs::write(&path, b"hello").unwrap();

        assert_eq!(read_bounded(&path, 5).unwrap(), Some(b"hello".to_vec()));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_file_over_the_limit_yields_none() {
        let path = temp_path("over-limit");
        std::fs::write(&path, b"hello").unwrap();

        assert_eq!(read_bounded(&path, 4).unwrap(), None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_path_yields_an_error() {
        let path = temp_path("missing");

        assert!(read_bounded(&path, 4).is_err());
    }

    #[test]
    fn an_alias_file_is_returned_verbatim() {
        let path = temp_path("alias-verbatim");
        let text = "[Object]\r\nframe=0,80\r\n[Object.0]\r\neffect.name=図形\r\n";
        std::fs::write(&path, text.as_bytes()).unwrap();

        assert_eq!(read_alias_file(&path).unwrap(), text);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_syntax_of_the_body_is_left_to_the_caller() {
        // 構文の検証はここより後ろに居る。読めた本文はそのまま返る。
        let path = temp_path("alias-unvalidated");
        std::fs::write(&path, "[Object]\u{0}\r\n".as_bytes()).unwrap();

        assert_eq!(read_alias_file(&path).unwrap(), "[Object]\u{0}\r\n");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_alias_file_at_the_limit_is_read_in_full() {
        let path = temp_path("alias-at-limit");
        std::fs::write(&path, vec![b'a'; MAX_ALIAS_BYTES]).unwrap();

        assert_eq!(read_alias_file(&path).unwrap().len(), MAX_ALIAS_BYTES);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_alias_file_one_byte_over_the_limit_is_too_large() {
        let path = temp_path("alias-over-limit");
        std::fs::write(&path, vec![b'a'; MAX_ALIAS_BYTES + 1]).unwrap();

        assert_eq!(read_alias_file(&path), Err(AliasFileError::TooLarge));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_alias_file_is_not_found() {
        let path = temp_path("alias-missing");

        assert_eq!(read_alias_file(&path), Err(AliasFileError::NotFound));
    }

    #[test]
    fn a_directory_is_not_a_readable_alias_file() {
        // ディレクトリを開けるかどうかは OS で分かれ、開ければ読み取りの側が
        // 落ちる。どちらの経路でも不在とは別の種別へ落ちる。
        let path = temp_path("alias-directory");
        std::fs::create_dir(&path).unwrap();

        assert_eq!(read_alias_file(&path), Err(AliasFileError::Unreadable));

        let _ = std::fs::remove_dir(&path);
    }

    #[test]
    fn an_alias_file_that_is_not_utf8_is_not_parsable() {
        let path = temp_path("alias-not-utf8");
        std::fs::write(&path, b"[Object]\r\nname=\xff\xfe\r\n").unwrap();

        assert_eq!(read_alias_file(&path), Err(AliasFileError::NotUtf8));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn alias_file_reasons_belong_to_the_shared_value_set() {
        // 一覧に無い名前は、誰にも気付かれないままワイヤへ出る。
        for reason in AliasFileError::ALL
            .iter()
            .filter_map(AliasFileError::reason)
        {
            assert!(
                crate::error::REASON_VALUES.contains(&reason),
                "{reason} が reason の値域にありません"
            );
        }
    }

    #[test]
    fn absence_and_unreadability_stay_distinguishable() {
        // 不在は名前を持たない。それでも読み取り不能とは、エラーコードと
        // 名前の有無の組で分かれる。
        assert_eq!(AliasFileError::NotFound.reason(), None);
        assert_eq!(AliasFileError::NotFound.error_code(), ErrorCode::NotFound);
        assert_eq!(
            AliasFileError::Unreadable.reason(),
            Some("alias_file_unreadable")
        );
        assert_eq!(
            AliasFileError::Unreadable.error_code(),
            ErrorCode::InvalidArgument
        );
    }
}
