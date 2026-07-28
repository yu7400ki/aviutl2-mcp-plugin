//! Windows overlapped I/O の RAII ラッパーと期限付き read/write。
//!
//! named pipe を扱う本番経路とテストの mock server が同一の実装を共有する。
//! 期限超過時は必ず `CancelIoEx` でキャンセルし、`GetOverlappedResult`（bWait = TRUE）で
//! カーネルが I/O を手放したことを確認してから戻る。これにより保留 I/O を残したまま
//! `OVERLAPPED` 構造体や転送バッファが解放される経路を作らない。

use std::io;
use std::time::{Duration, Instant};
use tracing::error;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_IO_INCOMPLETE,
    ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, HANDLE, WAIT_ABANDONED_0, WAIT_FAILED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};

/// 期限付き I/O のエラー。
#[derive(Debug, thiserror::Error)]
pub enum WinIoError {
    /// 期限を超過し、I/O をキャンセルした。
    #[error("I/O が期限を超過しました")]
    TimedOut,
    /// OS レベルの I/O エラー。
    #[error("I/O エラー: {0}")]
    Io(#[from] io::Error),
}

/// 単一オブジェクトの待機結果。
#[derive(Debug)]
pub enum WaitOutcome {
    /// シグナル状態になった。
    Signaled,
    /// 期限を超過した。
    TimedOut,
    /// 待機 API 自体が失敗した（放棄された同期オブジェクトを含む）。
    Failed(io::Error),
}

/// 複数オブジェクトの待機結果。
#[derive(Debug)]
pub enum WaitAnyOutcome {
    /// 指定した配列のうち当該添字のオブジェクトがシグナル状態になった。
    Signaled(usize),
    /// 期限を超過した。
    TimedOut,
    /// 待機 API 自体が失敗した（放棄された同期オブジェクトを含む）。
    Failed(io::Error),
}

/// I/O 発行 API の戻り値の分類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoIssue {
    /// 同期的に完了した。待機は不要。
    Completed,
    /// カーネルに保留された。完了を待つ必要がある。
    Pending,
}

/// 手動リセットイベントの所有ハンドル。
///
/// `Drop` で `CloseHandle` するため、待機経路がどこで打ち切られてもハンドルは漏れない。
pub struct EventHandle {
    handle: HANDLE,
}

// イベントハンドルはカーネルオブジェクトへの参照であり、スレッド親和性を持たない。
// 所有権が移るだけで同時アクセスは生じないため、スレッド間の移動は安全である。
unsafe impl Send for EventHandle {}

impl EventHandle {
    /// 非シグナル状態の手動リセットイベントを作成する。
    pub fn new_manual_reset() -> io::Result<Self> {
        // SAFETY: 引数はいずれも既定値であり、戻り値のハンドルは本型が単独で所有する。
        let handle = unsafe { CreateEventW(None, true, false, None) }.map_err(to_io_error)?;
        Ok(Self { handle })
    }

    /// 生ハンドルを返す。所有権は移動しない。
    pub fn handle(&self) -> HANDLE {
        self.handle
    }

    /// シグナル状態にする。
    pub fn set(&self) -> io::Result<()> {
        // SAFETY: `self.handle` は本型が生存する限り有効なイベントハンドルである。
        unsafe { SetEvent(self.handle) }.map_err(to_io_error)
    }

    /// 非シグナル状態に戻す。
    pub fn reset(&self) -> io::Result<()> {
        // SAFETY: `self.handle` は本型が生存する限り有効なイベントハンドルである。
        unsafe { ResetEvent(self.handle) }.map_err(to_io_error)
    }

    /// `deadline` までシグナル状態を待つ。
    pub fn wait(&self, deadline: Instant) -> WaitOutcome {
        wait_one(self.handle, deadline)
    }
}

impl Drop for EventHandle {
    fn drop(&mut self) {
        // SAFETY: `self.handle` は本型のみが所有しており、ここでのみ閉じられる。
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// 1 つの overlapped I/O に対応する `OVERLAPPED` と完了通知イベントの組。
///
/// `OVERLAPPED` をヒープへ置くのは、I/O が保留されている間そのアドレスが
/// 変化しないことを保証するためである。`Drop` では保留中の I/O をキャンセルし
/// 完了を待ち合わせるため、カーネルが解放済みメモリへ書き込むことはない。
pub struct OverlappedOp {
    handle: HANDLE,
    overlapped: Box<OVERLAPPED>,
    event: EventHandle,
    pending: bool,
}

impl OverlappedOp {
    /// `handle` に対する I/O 用の `OVERLAPPED` を作成する。
    ///
    /// `handle` の所有権は移動せず、生の値としてコピー保持する。
    ///
    /// # Safety
    ///
    /// `handle` は本 `OverlappedOp` が drop されるまで有効であり続けなければならない。
    /// `Drop` は保留中の I/O をキャンセルするため `handle` に対して `CancelIoEx` と
    /// `GetOverlappedResult` を実行する。先に閉じられていると、OS が同じ値を別の
    /// カーネルオブジェクトへ再割り当てした場合に無関係なオブジェクトを操作し、
    /// さらに保留 I/O を残したまま転送バッファが解放され得る。
    ///
    /// 構造体のフィールドとして保持する場合は、宣言順（drop 順）で
    /// `OverlappedOp` が handle の所有者より先に drop されるようにすること。
    pub unsafe fn new(handle: HANDLE) -> io::Result<Self> {
        let event = EventHandle::new_manual_reset()?;
        let overlapped = Box::new(OVERLAPPED {
            hEvent: event.handle(),
            ..Default::default()
        });
        Ok(Self {
            handle,
            overlapped,
            event,
            pending: false,
        })
    }

    /// 完了通知イベントの生ハンドルを返す。
    pub fn event(&self) -> HANDLE {
        self.event.handle()
    }

    /// I/O 発行 API へ渡す `OVERLAPPED` ポインタを返す。
    ///
    /// 呼び出し直前に [`OverlappedOp::begin`] を呼び、直後に
    /// [`OverlappedOp::classify`] で戻り値を分類すること。
    pub fn as_mut_ptr(&mut self) -> *mut OVERLAPPED {
        self.overlapped.as_mut() as *mut OVERLAPPED
    }

    /// I/O 発行の直前に呼び、イベントと `OVERLAPPED` を初期状態へ戻す。
    pub fn begin(&mut self) -> io::Result<()> {
        debug_assert!(!self.pending, "保留中の I/O がある状態で再発行はできない");
        self.event.reset()?;
        let event = self.event.handle();
        *self.overlapped = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };
        Ok(())
    }

    /// I/O 発行 API の戻り値を分類し、保留状態を記録する。
    ///
    /// `ERROR_IO_PENDING` は [`IoIssue::Pending`]、成功は [`IoIssue::Completed`]、
    /// それ以外はエラーとして返す。`ConnectNamedPipe` の `ERROR_PIPE_CONNECTED` のように
    /// API 固有の「成功扱いのエラー」は呼び出し側で `Ok(())` へ読み替えてから渡す。
    pub fn classify(&mut self, result: windows::core::Result<()>) -> io::Result<IoIssue> {
        match result {
            Ok(()) => Ok(IoIssue::Completed),
            Err(err) if err.code() == ERROR_IO_PENDING.into() => {
                self.pending = true;
                Ok(IoIssue::Pending)
            }
            Err(err) => Err(to_io_error(err)),
        }
    }

    /// 保留中の I/O の完了を `deadline` まで待ち、転送バイト数を返す。
    ///
    /// 期限超過・待機失敗のいずれでも、戻る前に I/O をキャンセルして
    /// カーネルが `OVERLAPPED` と転送バッファを手放したことを確認する。
    pub fn await_completion(&mut self, deadline: Instant) -> Result<u32, WinIoError> {
        match self.event.wait(deadline) {
            WaitOutcome::Signaled => {
                let transferred = self.overlapped_result(false)?;
                self.pending = false;
                Ok(transferred)
            }
            WaitOutcome::TimedOut => {
                self.cancel_and_drain();
                Err(WinIoError::TimedOut)
            }
            WaitOutcome::Failed(err) => {
                self.cancel_and_drain();
                Err(WinIoError::Io(err))
            }
        }
    }

    /// 保留中の I/O をキャンセルし、完了するまで待ち合わせる。
    ///
    /// `GetOverlappedResult` を bWait = TRUE で呼ぶため、戻った時点で
    /// カーネルはこの `OVERLAPPED` と転送バッファを参照しない。
    /// `ERROR_OPERATION_ABORTED` は正常なキャンセル完了であり、相手の切断などによる
    /// エラー完了も「カーネルが I/O を手放した」点では同じく排出成功として扱う。
    ///
    /// `new` の安全性要件どおり `handle` が有効である限り、この排出が
    /// I/O を保留したまま失敗することはない。すなわち失敗は不変条件の違反であり、
    /// 保留 I/O を残したまま転送バッファが解放される直前の状態を意味する。
    /// 復帰手段が無いため、その場合はログを残してプロセスを異常終了させる。
    pub fn cancel_and_drain(&mut self) {
        if !self.pending {
            return;
        }
        // SAFETY: `self.handle` は `new` の安全性要件により有効であり、`overlapped` は
        // この I/O の発行に使ったものと同一アドレスである。
        unsafe {
            let _ = CancelIoEx(
                self.handle,
                Some(self.overlapped.as_ref() as *const OVERLAPPED),
            );
        }
        // キャンセル済み・完了済みのいずれでも完了状態が確定するまで待つ。
        if let Err(err) = self.raw_overlapped_result(true)
            && leaves_io_pending(&err)
        {
            error!(
                handle = ?self.handle.0,
                error = %err,
                "保留中の I/O を排出できませんでした。転送バッファの解放を防ぐためプロセスを終了します"
            );
            std::process::abort();
        }
        self.pending = false;
    }

    fn overlapped_result(&mut self, wait: bool) -> io::Result<u32> {
        match self.raw_overlapped_result(wait) {
            Ok(transferred) => Ok(transferred),
            // キャンセル完了は転送 0 バイトの完了として扱う。
            Err(err) if err.code() == ERROR_OPERATION_ABORTED.into() => Ok(0),
            Err(err) => Err(to_io_error(err)),
        }
    }

    fn raw_overlapped_result(&self, wait: bool) -> windows::core::Result<u32> {
        let mut transferred = 0u32;
        // SAFETY: `self.handle` は `new` の安全性要件により有効であり、`overlapped` は
        // 発行時と同一アドレスの生存中の構造体を指す。
        // `transferred` はスタック上の有効な書き込み先。
        unsafe {
            GetOverlappedResult(
                self.handle,
                self.overlapped.as_ref() as *const OVERLAPPED,
                &mut transferred,
                wait,
            )
        }?;
        Ok(transferred)
    }
}

/// `GetOverlappedResult(bWait = TRUE)` の失敗のうち、I/O がカーネルに
/// 保留されたままである可能性を示すものかどうかを判定する。
///
/// 通常の失敗は I/O 自体の完了状態（相手の切断など）であり、カーネルは既に
/// `OVERLAPPED` と転送バッファを手放している。一方ここで真になるのは
/// ハンドルや引数が無効で完了状態を取得できなかった場合であり、
/// `OverlappedOp::new` の安全性要件が破られたときにのみ起こる。
fn leaves_io_pending(err: &windows::core::Error) -> bool {
    let code = err.code();
    code == ERROR_INVALID_HANDLE.into()
        || code == ERROR_INVALID_PARAMETER.into()
        || code == ERROR_IO_INCOMPLETE.into()
}

impl Drop for OverlappedOp {
    fn drop(&mut self) {
        self.cancel_and_drain();
    }
}

/// `deadline` まで `buf` を満たすまで読み込む。
///
/// 転送バイト数 0 は相手が pipe を閉じたことを示すため `UnexpectedEof` として返し、
/// 進捗のないループに陥らないようにする。
///
/// `buf` は本関数の実行中のみカーネルへ渡される。期限超過時も I/O のキャンセル完了を
/// 待ってから戻るため、戻った後に `buf` が書き換わることはない。
pub fn read_exact(handle: HANDLE, buf: &mut [u8], deadline: Instant) -> Result<(), WinIoError> {
    // SAFETY: `handle` は本関数の呼び出し中ずっと呼び出し側が有効に保つ。
    // `op` は本関数のスコープを出るときに drop されるため handle より長生きしない。
    let mut op = unsafe { OverlappedOp::new(handle) }?;
    let mut total = 0usize;
    while total < buf.len() {
        op.begin()?;
        let mut immediate = 0u32;
        let slice = &mut buf[total..];
        // SAFETY: `slice` は本関数のスコープで生存し、`op` の `Drop` が I/O 完了を
        // 待ち合わせるため、カーネルの書き込み先は常に有効である。
        let result = unsafe {
            ReadFile(
                handle,
                Some(slice),
                Some(&mut immediate),
                Some(op.as_mut_ptr()),
            )
        };
        let transferred = match op.classify(result)? {
            IoIssue::Completed => immediate,
            IoIssue::Pending => op.await_completion(deadline)?,
        };
        if transferred == 0 {
            return Err(WinIoError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "pipe が閉じられました",
            )));
        }
        total += transferred as usize;
    }
    Ok(())
}

/// `deadline` まで `buf` を全て書き込む。
///
/// 転送バイト数 0 は書き込みが進まないことを示すため `WriteZero` として返す。
///
/// `buf` は本関数の実行中のみカーネルへ渡される。期限超過時も I/O のキャンセル完了を
/// 待ってから戻るため、戻った後にカーネルが `buf` を読むことはない。
pub fn write_all(handle: HANDLE, buf: &[u8], deadline: Instant) -> Result<(), WinIoError> {
    // SAFETY: `handle` は本関数の呼び出し中ずっと呼び出し側が有効に保つ。
    // `op` は本関数のスコープを出るときに drop されるため handle より長生きしない。
    let mut op = unsafe { OverlappedOp::new(handle) }?;
    let mut total = 0usize;
    while total < buf.len() {
        op.begin()?;
        let mut immediate = 0u32;
        let slice = &buf[total..];
        // SAFETY: `slice` は本関数のスコープで生存し、`op` の `Drop` が I/O 完了を
        // 待ち合わせるため、カーネルの読み出し元は常に有効である。
        let result = unsafe {
            WriteFile(
                handle,
                Some(slice),
                Some(&mut immediate),
                Some(op.as_mut_ptr()),
            )
        };
        let transferred = match op.classify(result)? {
            IoIssue::Completed => immediate,
            IoIssue::Pending => op.await_completion(deadline)?,
        };
        if transferred == 0 {
            return Err(WinIoError::Io(io::Error::new(
                io::ErrorKind::WriteZero,
                "pipe へ書き込めませんでした",
            )));
        }
        total += transferred as usize;
    }
    Ok(())
}

/// 単一のカーネルオブジェクトを `deadline` まで待つ。
///
/// 残り時間が 0 の場合は待機せず [`WaitOutcome::TimedOut`] を返す。
pub fn wait_one(handle: HANDLE, deadline: Instant) -> WaitOutcome {
    let remaining = remaining_until(deadline);
    if remaining.is_zero() {
        return WaitOutcome::TimedOut;
    }
    // SAFETY: `handle` は呼び出し側が有効に保つ待機可能オブジェクトである。
    let result = unsafe { WaitForSingleObject(handle, to_wait_millis(remaining)) };
    match result {
        WAIT_OBJECT_0 => WaitOutcome::Signaled,
        WAIT_TIMEOUT => WaitOutcome::TimedOut,
        WAIT_ABANDONED_0 => WaitOutcome::Failed(io::Error::other(
            "待機対象の同期オブジェクトが放棄されました",
        )),
        WAIT_FAILED => WaitOutcome::Failed(io::Error::last_os_error()),
        other => WaitOutcome::Failed(io::Error::other(format!(
            "予期しない待機結果です: {}",
            other.0
        ))),
    }
}

/// 複数のカーネルオブジェクトのいずれかがシグナル状態になるまで待つ。
///
/// `deadline` が `None` の場合は無期限に待つ。残り時間が 0 の場合は待機せず
/// [`WaitAnyOutcome::TimedOut`] を返す。
pub fn wait_any(handles: &[HANDLE], deadline: Option<Instant>) -> WaitAnyOutcome {
    let millis = match deadline {
        None => INFINITE,
        Some(deadline) => {
            let remaining = remaining_until(deadline);
            if remaining.is_zero() {
                return WaitAnyOutcome::TimedOut;
            }
            to_wait_millis(remaining)
        }
    };
    // SAFETY: `handles` は呼び出し側が有効に保つ待機可能オブジェクトの配列である。
    let result = unsafe { WaitForMultipleObjects(handles, false, millis) };
    if result == WAIT_TIMEOUT {
        return WaitAnyOutcome::TimedOut;
    }
    if result == WAIT_FAILED {
        return WaitAnyOutcome::Failed(io::Error::last_os_error());
    }
    let count = handles.len() as u32;
    let signaled = result.0.wrapping_sub(WAIT_OBJECT_0.0);
    if signaled < count {
        return WaitAnyOutcome::Signaled(signaled as usize);
    }
    let abandoned = result.0.wrapping_sub(WAIT_ABANDONED_0.0);
    if abandoned < count {
        return WaitAnyOutcome::Failed(io::Error::other(
            "待機対象の同期オブジェクトが放棄されました",
        ));
    }
    WaitAnyOutcome::Failed(io::Error::other(format!(
        "予期しない待機結果です: {}",
        result.0
    )))
}

/// 現在時刻から `deadline` までの残り時間を返す。既に過ぎている場合は 0。
pub fn remaining_until(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// `windows::core::Error` を `io::Error` へ変換する。
pub fn to_io_error(err: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(err.code().0)
}

/// 待機 API へ渡すミリ秒値へ変換する。
///
/// 1 ミリ秒未満の残り時間が 0 へ丸められて即時タイムアウトになるのを避けるため切り上げる。
/// `INFINITE` と衝突しないよう上限を 1 つ下で飽和させる。
fn to_wait_millis(remaining: Duration) -> u32 {
    let millis = remaining.as_nanos().div_ceil(1_000_000);
    millis.min(INFINITE as u128 - 1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_reset_event_signals_and_resets() {
        let event = EventHandle::new_manual_reset().unwrap();
        let deadline = Instant::now() + Duration::from_millis(50);
        assert!(matches!(event.wait(deadline), WaitOutcome::TimedOut));

        event.set().unwrap();
        let deadline = Instant::now() + Duration::from_millis(50);
        assert!(matches!(event.wait(deadline), WaitOutcome::Signaled));

        event.reset().unwrap();
        let deadline = Instant::now() + Duration::from_millis(50);
        assert!(matches!(event.wait(deadline), WaitOutcome::TimedOut));
    }

    #[test]
    fn expired_deadline_does_not_wait() {
        let event = EventHandle::new_manual_reset().unwrap();
        let deadline = Instant::now() - Duration::from_secs(1);
        let started = Instant::now();
        assert!(matches!(event.wait(deadline), WaitOutcome::TimedOut));
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn wait_any_reports_signaled_index() {
        let first = EventHandle::new_manual_reset().unwrap();
        let second = EventHandle::new_manual_reset().unwrap();
        second.set().unwrap();

        let handles = [first.handle(), second.handle()];
        let outcome = wait_any(&handles, Some(Instant::now() + Duration::from_millis(200)));
        assert!(matches!(outcome, WaitAnyOutcome::Signaled(1)));
    }

    #[test]
    fn wait_any_times_out_when_nothing_signaled() {
        let event = EventHandle::new_manual_reset().unwrap();
        let handles = [event.handle()];
        let outcome = wait_any(&handles, Some(Instant::now() + Duration::from_millis(20)));
        assert!(matches!(outcome, WaitAnyOutcome::TimedOut));
    }

    #[test]
    fn sub_millisecond_remaining_rounds_up() {
        assert_eq!(to_wait_millis(Duration::from_nanos(1)), 1);
        assert_eq!(to_wait_millis(Duration::from_micros(1500)), 2);
        assert_eq!(to_wait_millis(Duration::from_millis(3)), 3);
        assert_eq!(to_wait_millis(Duration::from_secs(u64::MAX)), INFINITE - 1);
    }

    #[test]
    fn remaining_until_is_zero_for_past_deadline() {
        assert_eq!(
            remaining_until(Instant::now() - Duration::from_secs(1)),
            Duration::ZERO
        );
        assert!(!remaining_until(Instant::now() + Duration::from_secs(1)).is_zero());
    }
}
