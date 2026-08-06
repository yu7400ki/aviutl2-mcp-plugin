//! プロセス識別情報の収集。
//!
//! PID とプロセス作成時刻は OS 固有の adapter 層に隔離し、
//! core crate へ SDK/Windows 型を漏らさない。

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use windows::Win32::Foundation::{FILETIME, GetLastError};
use windows::Win32::System::Threading::GetProcessTimes;

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

#[cfg(test)]
mod tests {
    use super::*;

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
