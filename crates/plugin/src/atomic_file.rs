//! 保護されたファイルの原子的な書き込み。
//!
//! registry の descriptor も共有設定も、読み手が途中の状態を観測してはならない
//! 点で同じである。同一ディレクトリの一時ファイルへ全量を書き、`FlushFileBuffers`
//! でディスクへ落としてから置換する。置換は `ReplaceFileW`、対象が無ければ
//! `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)` で行う。
//!
//! 一時ファイルも本体も [`create_protected_file`] で作るため、置換後のファイルは
//! 保護 DACL を持つ。
//!
//! 失敗の説明に絶対パスを含めない。どの対象で失敗したかは呼び出し元が匿名化した
//! 呼び名で添える。

use anyhow::{Context, Result};
use aviutl2_mcp_win::create_protected_file;
use std::io::Write;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    DeleteFileW, FlushFileBuffers, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    ReplaceFileW,
};
use windows::core::PCWSTR;

/// 一時ファイルを消し残さないための番人。
///
/// 置換が成功すれば一時ファイルは既に無く、削除は何もしない。
pub(crate) struct TempFileGuard<'a>(pub(crate) &'a Path);

impl Drop for TempFileGuard<'_> {
    fn drop(&mut self) {
        if self.0.exists() {
            let wide = to_wide(self.0);
            // SAFETY: `wide` は NUL 終端したパスであり、呼び出しの間だけ参照される。
            unsafe {
                let _ = DeleteFileW(PCWSTR(wide.as_ptr()));
            }
        }
    }
}

/// 保護された一時ファイルへ全量を書き、`target_path` へ原子的に置換する。
///
/// 呼び出し元は `temp_path` を `target_path` と同一ディレクトリに採ること。
/// ボリュームをまたぐと置換が原子的でなくなる。
pub(crate) fn write_protected_atomic(
    temp_path: &Path,
    target_path: &Path,
    contents: &[u8],
) -> Result<()> {
    let _guard = TempFileGuard(temp_path);

    let mut file =
        create_protected_file(temp_path).context("一時ファイルを作成できませんでした")?;
    file.write_all(contents)
        .context("一時ファイルへの書き込みに失敗しました")?;

    // SAFETY: `file` は生存中のファイルハンドルを所有しており、生ハンドルは
    // この呼び出しの間だけ使う。
    unsafe {
        let raw_handle = file.as_raw_handle();
        FlushFileBuffers(HANDLE(raw_handle))
            .ok()
            .context("ファイルバッファの flush に失敗しました")?;
    }
    mem::drop(file);

    atomic_replace(temp_path, target_path)
}

/// 書き終えた一時ファイルで対象を置き換える。
pub(crate) fn atomic_replace(temp_path: &Path, target_path: &Path) -> Result<()> {
    let temp_wide = to_wide(temp_path);
    let target_wide = to_wide(target_path);

    // SAFETY: いずれのパスも NUL 終端しており、呼び出しの間だけ参照される。
    unsafe {
        if target_path.exists() {
            ReplaceFileW(
                PCWSTR(target_wide.as_ptr()),
                PCWSTR(temp_wide.as_ptr()),
                None,
                windows::Win32::Storage::FileSystem::REPLACE_FILE_FLAGS(0),
                None,
                None,
            )
            .ok()
            .context("ReplaceFileW に失敗しました")?;
        } else {
            MoveFileExW(
                PCWSTR(temp_wide.as_ptr()),
                PCWSTR(target_wide.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
            .ok()
            .context("MoveFileExW に失敗しました")?;
        }
    }
    Ok(())
}

/// パスを NUL 終端の UTF-16 列へ写す。
pub(crate) fn to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
