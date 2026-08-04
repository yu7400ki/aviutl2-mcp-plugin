//! 保護された DACL を持つディレクトリとファイルの用意。

use crate::ProtectedDirError;
use crate::dacl::{ProtectedSecurityAttributes, to_io_error, to_wide};
use crate::verify::verify_protected_dacl;
use std::path::Path;
use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE,
    FILE_SHARE_MODE,
};
use windows::core::PCWSTR;

/// 保護された DACL を持つディレクトリを用意する。
///
/// 手順は次のとおりである。
///
/// 1. セキュリティ属性を組み立てて `CreateDirectoryW` を呼ぶ
/// 2. 成功したら終わり。我々が作ったものであり、DACL は与えたとおりである
/// 3. 既に存在していた場合は DACL を読んで検証する。想定と異なれば
///    [`ProtectedDirError::NotProtected`] を返す。**書き換えない**
/// 4. それ以外の失敗はそのまま返す
///
/// 存在の有無は作成を試みた結果だけで判定する。先に問い合わせてから作ると、
/// その間に別のプロセスが同じディレクトリを作った場合に失敗してしまう。
/// 基底とその直下は複数のプロセスが共有するため、ほぼ同時の起動で必ず踏む。
pub fn create_protected_directory(path: &Path) -> Result<(), ProtectedDirError> {
    let wide = to_wide(path);
    let attributes = ProtectedSecurityAttributes::new()?;
    // SAFETY: `wide` は NUL 終端したパスであり、`attributes` は本呼び出しの間
    // 生存する。
    match unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(attributes.as_ptr())) } {
        Ok(()) => Ok(()),
        Err(e) if e.code() == ERROR_ALREADY_EXISTS.into() => verify_protected_dacl(path),
        Err(e) => Err(to_io_error(e).into()),
    }
}

/// 保護された DACL を持つ新規ファイルを作り、書き込み用の `File` を返す。
///
/// 既に存在する場合は失敗する。上書きの経路を持たないことが、書き込み先を
/// 取り違えたときに既存の内容を失わない根拠になる。
pub fn create_protected_file(path: &Path) -> Result<std::fs::File, ProtectedDirError> {
    let wide = to_wide(path);
    let attributes = ProtectedSecurityAttributes::new()?;
    // SAFETY: `wide` は NUL 終端したパスであり、`attributes` は本呼び出しの間
    // 生存する。返されたハンドルはそのまま `File` の所有へ移す。
    unsafe {
        let handle = CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_MODE(0),
            Some(attributes.as_ptr()),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
        .map_err(to_io_error)?;

        use std::os::windows::io::FromRawHandle;
        Ok(std::fs::File::from_raw_handle(handle.0))
    }
}
