//! Win32 へ渡す文字列の形。

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

/// パスを NUL 終端の UTF-16 列へ直す。
///
/// `PCWSTR` を作る前段であり、戻り値は呼び出しの間だけ生存していればよい。
pub fn to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
