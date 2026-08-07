//! プロセスとモジュールの識別情報の収集。
//!
//! PID とプロセス作成時刻は OS 固有の adapter 層に隔離し、
//! core crate へ SDK/Windows 型を漏らさない。
//!
//! 自身が読み込まれた場所も**ここでしか求めない。** ホストの実行ファイルの隣に
//! 在るもの（同梱の説明）と、自 DLL の隣に在るもの（server の実行体）は別々の
//! 呼び出し元が要るが、パスを受け取る領域を足りるまで広げる手続きは 1 つで
//! 足りる。

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{FILETIME, GetLastError, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows::Win32::System::Threading::GetProcessTimes;
use windows::core::PCWSTR;

/// 実行ファイルのパスを受け取る最初の長さ。
const INITIAL_PATH_LEN: usize = 260;

/// 実行ファイルのパスを受け取る領域の上限。
///
/// Windows のパスの上限を収める長さであり、これでも収まらない応答は解決の
/// 失敗として扱う。
const MAX_PATH_LEN: usize = 32 * 1024;

pub fn current_pid() -> u32 {
    std::process::id()
}

/// 現在のプロセス作成時刻を UTC で返す。
///
/// `FILETIME` は 100 ナノ秒粒度であり、この値は
/// [`aviutl2_mcp_core::format_utc_timestamp`] で情報を失わずに文字列化できる。
pub fn current_process_created_at() -> Result<DateTime<Utc>> {
    unsafe {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let process = windows::Win32::System::Threading::GetCurrentProcess();
        GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user)
            .ok()
            .context("プロセス作成時刻の取得に失敗しました")?;
        filetime_to_datetime(creation)
    }
}

fn filetime_to_datetime(ft: FILETIME) -> Result<DateTime<Utc>> {
    let quad = ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64);
    if quad == 0 {
        let err = unsafe { GetLastError() };
        anyhow::bail!("プロセス作成時刻が無効でした: {err:?}");
    }
    // FILETIME は 1601-01-01 UTC からの 100 ナノ秒単位。
    const HUNDRED_NANOS_PER_SEC: i64 = 10_000_000;
    const EPOCH_DIFF_SECS: i64 = 11_644_473_600; // 1601-01-01 〜 1970-01-01
    let secs = (quad as i64 / HUNDRED_NANOS_PER_SEC) - EPOCH_DIFF_SECS;
    let nanos = ((quad as i64 % HUNDRED_NANOS_PER_SEC) * 100) as u32;
    DateTime::from_timestamp(secs, nanos).context("FILETIME から DateTime への変換に失敗しました")
}

/// モジュールのファイルパスを返す。解決できなければ `None`。
///
/// `module` に `None` を渡すと現在のプロセスの実行ファイルを指す。
///
/// 受け取る領域は足りるまで広げる。**取得は書き込んだ長さしか返さない。**
/// 領域と同じ長さが返った場合は切り詰められた可能性があり、その値をパスとして
/// 扱うと別の場所を指す。
pub(crate) fn module_file_name(module: Option<HMODULE>) -> Option<PathBuf> {
    let mut buffer = vec![0u16; INITIAL_PATH_LEN];
    loop {
        // SAFETY: `buffer` は呼び出し中を通じて生存する書き込み可能な領域であり、
        // 長さは呼び出し先へスライスとして渡る。
        let written = unsafe { GetModuleFileNameW(module, &mut buffer) } as usize;
        if written == 0 {
            return None;
        }
        if written < buffer.len() {
            return Some(PathBuf::from(OsString::from_wide(&buffer[..written])));
        }
        if buffer.len() >= MAX_PATH_LEN {
            return None;
        }
        buffer.resize((buffer.len() * 2).min(MAX_PATH_LEN), 0);
    }
}

/// この DLL が置かれたディレクトリを返す。解決できなければ `None`。
///
/// **プロセスの実行ファイルではない。** ホストは AviUtl2 本体であり、その隣に
/// server の実行体は無い。`aviutl2.toml` は plugin と server の 2 つを同じ
/// `Plugin\` へ置くため、**自 DLL の位置が分かれば server はその兄弟である。**
///
/// 自分自身を指す module handle は、この関数のアドレスから引く。DLL の
/// `HINSTANCE` は SDK の初期化を経ないと手に入らず、設定画面のコールバック
/// からも起動時からも同じ手順で求められる形にしてある。
pub fn plugin_directory() -> Option<PathBuf> {
    let mut module = HMODULE::default();
    // SAFETY: `plugin_directory` 自身のアドレスはこの DLL の内側にあり、
    // `module` はスタック上の有効な書き込み先である。参照数を増やさない指定で
    // あるため、得たハンドルを解放する責任は生じない。
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(plugin_directory as *const u16),
            &mut module,
        )
    }
    .ok()?;
    module_file_name(Some(module))
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_module_path_points_at_the_binary_that_holds_this_code() {
        // 試験では DLL ではなく試験の実行体そのものが持ち主になる。
        // **求めているのは「自分を含むモジュール」であり、ホストの実行ファイル
        // ではない**——両者が一致するのはこの試験の中だけである。
        let directory = plugin_directory().expect("自モジュールの位置を解決できません");
        assert!(directory.is_dir(), "{}", directory.display());

        let own = module_file_name(None).expect("プロセスの実行ファイルを解決できません");
        assert!(own.is_file(), "{}", own.display());
    }

    #[test]
    fn current_pid_matches_std() {
        assert_eq!(current_pid(), std::process::id());
    }

    #[test]
    fn current_process_created_at_is_valid() {
        let dt = current_process_created_at().unwrap();
        let now = Utc::now();
        assert!(dt <= now);
        assert!(now.signed_duration_since(dt).num_seconds() < 60);
    }
}
