//! ファイル読み取りの共通処理。

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
}
