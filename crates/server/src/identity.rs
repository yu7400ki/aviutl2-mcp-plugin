//! PID からプロセス作成時刻を取得する adapter。
//!
//! Windows API はこの層に閉じ、core crate には SDK 型を漏らさない。

use chrono::{DateTime, Utc};
use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, HANDLE};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::HRESULT;

/// プロセス識別情報。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// プロセス作成時刻（UTC）。
    pub created_at: DateTime<Utc>,
}

/// PID に対するプロセス照会の結果。
///
/// 「存在しない」と「存在を判定できない」を区別する。両者を混同すると、
/// 権限不足で照会できなかっただけのプロセスを終了済みと誤認する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessLookup {
    /// プロセスが存在し、識別情報を取得できた。
    Found(ProcessIdentity),
    /// PID に対応するプロセスが存在しない。
    Absent,
    /// 権限不足などにより存在を判定できない。
    Undetermined,
}

impl ProcessLookup {
    /// 取得できた識別情報のみを返す。
    ///
    /// [`ProcessLookup::Absent`] と [`ProcessLookup::Undetermined`] は等しく `None`
    /// になるため、両者の区別が必要な判断には使わないこと。
    pub fn found(self) -> Option<ProcessIdentity> {
        match self {
            ProcessLookup::Found(identity) => Some(identity),
            ProcessLookup::Absent | ProcessLookup::Undetermined => None,
        }
    }
}

/// 指定 PID のプロセスを照会する。
///
/// 必要最小権限（`PROCESS_QUERY_LIMITED_INFORMATION`）で開く。
/// プロセスの不在を確定できた場合のみ [`ProcessLookup::Absent`] を返す。
pub fn lookup_process(pid: u32) -> ProcessLookup {
    unsafe {
        let handle = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => handle,
            Err(e) => return classify_open_process_error(e.code()),
        };
        let created_at = get_process_times(handle);
        let _ = CloseHandle(handle);
        match created_at {
            Some(created_at) => ProcessLookup::Found(ProcessIdentity { created_at }),
            // handle を開けた時点でプロセスは存在する。作成時刻を取得できないのは
            // 不在の証拠にならない。
            None => ProcessLookup::Undetermined,
        }
    }
}

/// `OpenProcess` の失敗コードから PID の状態を判定する。
fn classify_open_process_error(code: HRESULT) -> ProcessLookup {
    // 存在しない PID に対する OpenProcess は ERROR_INVALID_PARAMETER を返す。
    // それ以外（アクセス拒否など）は存在の有無を判定できない。
    if code == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) {
        ProcessLookup::Absent
    } else {
        ProcessLookup::Undetermined
    }
}

/// 指定 PID のプロセス作成時刻を取得する。
///
/// プロセスが存在しない場合とアクセス不能な場合をいずれも `None` に畳み込む。
/// 既存の呼び出し元との互換のために残している暫定 API であり、新しい呼び出し元を
/// 作ってはならない。両者を区別する [`lookup_process`] を使うこと。
pub fn get_process_identity(pid: u32) -> Option<ProcessIdentity> {
    lookup_process(pid).found()
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
    let quad = ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64);
    const HUNDRED_NANOS_PER_SEC: i64 = 10_000_000;
    const EPOCH_DIFF_SECS: i64 = 11_644_473_600;
    let secs = (quad as i64 / HUNDRED_NANOS_PER_SEC) - EPOCH_DIFF_SECS;
    let nsecs = ((quad as i64 % HUNDRED_NANOS_PER_SEC) * 100) as u32;
    DateTime::from_timestamp(secs, nsecs).unwrap_or(DateTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_NOT_ENOUGH_MEMORY};

    #[test]
    fn filetime_unix_epoch_converts_to_unix_epoch() {
        let filetime = FILETIME {
            dwLowDateTime: 0xD53E_8000,
            dwHighDateTime: 0x019D_B1DE,
        };
        assert_eq!(filetime_to_utc(filetime), DateTime::UNIX_EPOCH);
    }

    #[test]
    fn current_process_has_creation_time() {
        let ProcessLookup::Found(identity) = lookup_process(std::process::id()) else {
            panic!("自身の PID は取得可能");
        };
        assert!(identity.created_at > DateTime::UNIX_EPOCH);
    }

    #[test]
    fn nonexistent_process_is_absent() {
        assert_eq!(lookup_process(0xFFFF_FFFF), ProcessLookup::Absent);
    }

    #[test]
    fn access_denied_is_undetermined() {
        assert_eq!(
            classify_open_process_error(HRESULT::from_win32(ERROR_ACCESS_DENIED.0)),
            ProcessLookup::Undetermined,
            "アクセス拒否では不在と判定しない"
        );
    }

    #[test]
    fn invalid_parameter_is_absent() {
        assert_eq!(
            classify_open_process_error(HRESULT::from_win32(ERROR_INVALID_PARAMETER.0)),
            ProcessLookup::Absent
        );
    }

    #[test]
    fn unexpected_error_is_undetermined() {
        assert_eq!(
            classify_open_process_error(HRESULT::from_win32(ERROR_NOT_ENOUGH_MEMORY.0)),
            ProcessLookup::Undetermined,
            "分類できない失敗は判定不能として扱う"
        );
    }
}
