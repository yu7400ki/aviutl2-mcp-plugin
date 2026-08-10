//! ログへ出す値の匿名化。
//!
//! 出力先はホストのログファイルであり、不具合の報告に添えて持ち出される。
//! そのため対象を一意に特定できる値をそのまま残さない。識別子は先頭だけを
//! 残して、同時に稼働するインスタンスを衝突しない範囲で見分けられるようにし、
//! ファイルは名前だけを出して利用者のディレクトリ構成を明かさない。
//!
//! 応答へ載せる値の秘匿は本モジュールの担当ではなく、ログ専用である。

use crate::InstanceId;
use std::path::Path;

/// 匿名化した識別子に残す先頭文字数。
///
/// UUID の先頭 8 桁は同時に稼働するインスタンスを見分けるには十分に長く、
/// 単独では元の識別子を復元できない。
pub const ANONYMIZED_ID_CHARS: usize = 8;

/// ログへ出す `instance_id` の匿名化表現。
pub fn instance_id(instance_id: &InstanceId) -> String {
    prefix(&instance_id.to_string())
}

/// ログへ出す descriptor ファイルの表現。
///
/// descriptor は `{instance_id}.json` として書き込まれ、書き込み途中の一時
/// ファイルも同じ名前で始まる。そのため拡張子を除いた名前の先頭が匿名化した
/// `instance_id` になる。ディレクトリは出さない。
pub fn descriptor_file(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(prefix)
        .unwrap_or_else(|| "-".to_string())
}

/// 先頭 [`ANONYMIZED_ID_CHARS`] 文字だけを残す。
fn prefix(value: &str) -> String {
    value.chars().take(ANONYMIZED_ID_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn instance_id_keeps_only_a_prefix() {
        let id = InstanceId::new_v4();
        let anonymized = instance_id(&id);

        assert_eq!(anonymized.chars().count(), ANONYMIZED_ID_CHARS);
        assert!(
            id.to_string().starts_with(&anonymized),
            "先頭からの部分文字列である"
        );
        assert_ne!(anonymized, id.to_string(), "完全な識別子は残さない");
    }

    #[test]
    fn descriptor_file_drops_directories() {
        let id = InstanceId::new_v4();
        let path = PathBuf::from(r"C:\Users\someone\AppData\Local\AviUtl2Mcp\instances")
            .join(format!("{id}.json"));

        let label = descriptor_file(&path);

        assert_eq!(label, instance_id(&id));
        assert!(!label.contains("Users"), "パスが残っています: {label}");
        assert!(!label.contains(".json"));
    }

    #[test]
    fn descriptor_file_of_a_temporary_file_keeps_the_same_prefix() {
        let id = InstanceId::new_v4();
        let path = PathBuf::from(r"C:\Users\someone\AppData\Local\AviUtl2Mcp\instances")
            .join(format!("{id}.json.{}.tmp", InstanceId::new_v4()));

        assert_eq!(descriptor_file(&path), instance_id(&id));
    }

    #[test]
    fn descriptor_file_without_name_is_placeholder() {
        assert_eq!(descriptor_file(Path::new("")), "-");
    }
}
