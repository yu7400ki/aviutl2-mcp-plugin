//! PID からプロセス作成時刻を取得する adapter。
//!
//! Windows API はこの層に閉じ、core crate には SDK 型を漏らさない。

use chrono::{DateTime, Utc};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// プロセス識別情報。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// プロセス作成時刻（UTC）。
    pub created_at: DateTime<Utc>,
}

/// 指定 PID のプロセス作成時刻を取得する。
///
/// プロセスが存在しないかアクセス不能な場合は `None` を返す。
/// 必要最小権限（`PROCESS_QUERY_LIMITED_INFORMATION`）で開く。
pub fn get_process_identity(pid: u32) -> Option<ProcessIdentity> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let result = get_process_times(handle);
        let _ = CloseHandle(handle);
        result.map(|created_at| ProcessIdentity { created_at })
    }
}

unsafe fn get_process_times(handle: HANDLE) -> Option<DateTime<Utc>> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe {
        GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user).ok()?;
    }
    Some(filetime_to_utc(creation))
}

/// `FILETIME`（100 ナノ秒単位、1601-01-01 UTC 起点）を `DateTime<Utc>` へ変換する。
fn filetime_to_utc(ft: FILETIME) -> DateTime<Utc> {
    // FILETIME 値を 100 ナノ秒間隔の整数として合成。
    let ft100ns = ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64);
    // 1601-01-01 から 1970-01-01 までの秒数。
    const EPOCH_DIFF_100NS: u64 = 11_644_473_600_000_000;
    let unix_100ns = ft100ns.saturating_sub(EPOCH_DIFF_100NS);
    let secs = (unix_100ns / 10_000_000) as i64;
    let nsecs = ((unix_100ns % 10_000_000) * 100) as u32;
    DateTime::from_timestamp(secs, nsecs).unwrap_or(DateTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_has_creation_time() {
        let identity = get_process_identity(std::process::id()).expect("自身の PID は取得可能");
        assert!(identity.created_at > DateTime::UNIX_EPOCH);
    }

    #[test]
    fn nonexistent_process_returns_none() {
        assert!(get_process_identity(0xFFFF_FFFF).is_none());
    }
}
