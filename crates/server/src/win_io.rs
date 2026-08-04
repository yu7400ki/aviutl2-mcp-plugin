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
enum WaitOutcome {
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

// SAFETY: イベントオブジェクトはスレッドを跨いで待機・シグナルしてよく、
// `EventHandle` はハンドルの所有権を単一の値に閉じている。閉じるのは `Drop`
// だけであり、複製されたハンドルは存在しない。
unsafe impl Send for EventHandle {}
// SAFETY: 共有参照から呼べるのは `SetEvent` と待機だけであり、いずれも
// カーネル側で直列化される。
unsafe impl Sync for EventHandle {}

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
    fn reset(&self) -> io::Result<()> {
        // SAFETY: `self.handle` は本型が生存する限り有効なイベントハンドルである。
        unsafe { ResetEvent(self.handle) }.map_err(to_io_error)
    }

    /// `deadline` までシグナル状態を待つ。
    fn wait(&self, deadline: Instant) -> WaitOutcome {
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

    /// 保留状態かどうか。
    ///
    /// 保留中の I/O を残したまま転送バッファを解放しないことが本型の役目であり、
    /// その記録が実際に付いていることを試験がここで確かめる。
    #[cfg(test)]
    pub(crate) fn is_pending(&self) -> bool {
        self.pending
    }

    /// 発行の成功が「完了」ではなく「登録」を意味する API の I/O を発行する。
    ///
    /// **`ReadDirectoryChangesW` は overlapped ハンドルに対して成功を返しても、
    /// それは要求が登録されたことしか意味しない。** [`OverlappedOp::classify`]
    /// で完了として扱うと保留状態が記録されず、`Drop` がキャンセルを飛ばして
    /// 転送バッファを解放する——カーネルはその後もそこへ書き込む。
    ///
    /// **分類の仕方を呼び出し側に選ばせない。** [`OverlappedOp::begin`] から
    /// 発行・分類までをこの 1 つの口に閉じ、成功も `ERROR_IO_PENDING` も
    /// どちらも保留として記録する。呼び出し側は必ず完了を待つことになる。
    ///
    /// `issue` は `OVERLAPPED` のポインタを受け取り、API の戻り値を返す。
    ///
    /// # Safety
    ///
    /// `issue` がカーネルへ渡す転送バッファは、本 `OverlappedOp` が drop される
    /// まで有効であり続けなければならない。`Drop` は保留中の I/O をキャンセル
    /// して完了を待ち合わせるため、その順序さえ守れば解放済みメモリへの書き込み
    /// は起きない。
    pub unsafe fn issue_queued(
        &mut self,
        issue: impl FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>,
    ) -> io::Result<()> {
        self.begin()?;
        match issue(self.as_mut_ptr()) {
            Ok(()) => {
                self.pending = true;
                Ok(())
            }
            Err(err) if err.code() == ERROR_IO_PENDING.into() => {
                self.pending = true;
                Ok(())
            }
            Err(err) => Err(to_io_error(err)),
        }
    }

    /// 保留中の I/O の完了を `deadline` まで待ち、転送バイト数を返す。
    ///
    /// 期限超過・待機失敗のいずれでも、戻る前に I/O をキャンセルして
    /// カーネルが `OVERLAPPED` と転送バッファを手放したことを確認する。
    ///
    /// **完了として届いた I/O は、結果が失敗であっても保留ではない。** 相手の
    /// 切断などで失敗した I/O もカーネルは既に `OVERLAPPED` と転送バッファを
    /// 手放している。失敗を理由に保留のまま残すと、呼び出し元が次の発行へ
    /// 進んだときに [`OverlappedOp::begin`] の表明へ掛かり、`Drop` は不要な
    /// キャンセルの排出に失敗して [`std::process::abort`] へ落ちる。
    ///
    /// **完了状態そのものを取得できなかった場合だけは排出する。** それは
    /// [`leaves_io_pending`] が真になる失敗であり、カーネルがまだ手放していない
    /// 可能性を残す。
    pub fn await_completion(&mut self, deadline: Instant) -> Result<u32, WinIoError> {
        match self.event.wait(deadline) {
            WaitOutcome::Signaled => match self.raw_overlapped_result(false) {
                Ok(transferred) => {
                    self.pending = false;
                    Ok(transferred)
                }
                // キャンセル完了は転送 0 バイトの完了として扱う。
                Err(err) if err.code() == ERROR_OPERATION_ABORTED.into() => {
                    self.pending = false;
                    Ok(0)
                }
                Err(err) if leaves_io_pending(&err) => {
                    self.cancel_and_drain();
                    Err(WinIoError::Io(to_io_error(err)))
                }
                Err(err) => {
                    self.pending = false;
                    Err(WinIoError::Io(to_io_error(err)))
                }
            },
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
    fn cancel_and_drain(&mut self) {
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
            // ハンドルの値はログへ出さない。どの I/O かは呼び出し元の span が示す。
            error!(
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
        if remaining_until(deadline).is_zero() {
            return Err(WinIoError::TimedOut);
        }
        op.begin()?;
        let slice = &mut buf[total..];
        // SAFETY: `slice` は本関数のスコープで生存し、`op` の `Drop` が I/O 完了を
        // 待ち合わせるため、カーネルの書き込み先は常に有効である。
        // 転送バイト数は同期完了時も `OVERLAPPED` から取得するため NULL を渡す。
        let result = unsafe { ReadFile(handle, Some(slice), None, Some(op.as_mut_ptr())) };
        let transferred = match op.classify(result)? {
            IoIssue::Completed => op.overlapped_result(false)?,
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
        if remaining_until(deadline).is_zero() {
            return Err(WinIoError::TimedOut);
        }
        op.begin()?;
        let slice = &buf[total..];
        // SAFETY: `slice` は本関数のスコープで生存し、`op` の `Drop` が I/O 完了を
        // 待ち合わせるため、カーネルの読み出し元は常に有効である。
        // 転送バイト数は同期完了時も `OVERLAPPED` から取得するため NULL を渡す。
        let result = unsafe { WriteFile(handle, Some(slice), None, Some(op.as_mut_ptr())) };
        let transferred = match op.classify(result)? {
            IoIssue::Completed => op.overlapped_result(false)?,
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
fn wait_one(handle: HANDLE, deadline: Instant) -> WaitOutcome {
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
///
/// `windows::core::Error` は Win32 エラーを `HRESULT`（`0x8007_XXXX`）として
/// 保持するため、そのまま `from_raw_os_error` へ渡すと `io::Error::raw_os_error`
/// が生の Win32 コードと一致せず、`io::Error::kind` も常に分類不能になる。
/// FACILITY_WIN32 の場合は元の Win32 コードへ戻し、それ以外の HRESULT は
/// 生の OS エラーではないため `io::Error::other` として包む。
fn to_io_error(err: windows::core::Error) -> io::Error {
    let hresult = err.code().0 as u32;
    if hresult & 0xFFFF_0000 == FACILITY_WIN32_MASK {
        io::Error::from_raw_os_error((hresult & 0xFFFF) as i32)
    } else {
        io::Error::other(err)
    }
}

/// `HRESULT` の上位 16bit が示す「Win32 エラーを包んだ HRESULT」の印。
///
/// 失敗ビットと FACILITY_WIN32 の組で、`HRESULT_FROM_WIN32` が生成する値に対応する。
const FACILITY_WIN32_MASK: u32 = 0x8007_0000;

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
    use aviutl2_mcp_core::InstanceId;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{
        ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, GENERIC_READ, GENERIC_WRITE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_READMODE_MESSAGE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_TYPE_MESSAGE,
    };
    use windows::Win32::System::Threading::{
        CreateMutexW, GetCurrentProcess, GetProcessHandleCount,
    };
    use windows::core::PCWSTR;

    /// 期限超過を確実に起こすための待機時間。
    const SHORT_DEADLINE: Duration = Duration::from_millis(80);

    /// 期限超過後に「遅れて完了した I/O」が観測されるまでの猶予。
    ///
    /// キャンセルせずに戻る実装であれば、この間にカーネルが読み取りバッファへ
    /// 書き込む。短すぎると見逃すため、期限の数倍を取る。
    const LATE_COMPLETION_GRACE: Duration = Duration::from_millis(250);

    /// 両端とも overlapped で開いた named pipe の対。
    ///
    /// `client` 側を本番と同じ経路（`read_exact` / `write_all`）で駆動し、
    /// `server` 側を対向端として使う。
    struct PipePair {
        server: HANDLE,
        client: HANDLE,
    }

    impl PipePair {
        fn create() -> Self {
            Self::create_with(PIPE_TYPE_BYTE | PIPE_READMODE_BYTE)
        }

        /// メッセージモードの pipe 対を作る。
        ///
        /// 長さ 0 のメッセージを送ると読み取りが「成功したが転送 0 バイト」で
        /// 完了するため、バイトモードでは作れない転送 0 バイトを再現できる。
        fn create_message_mode() -> Self {
            Self::create_with(PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE)
        }

        fn create_with(mode: windows::Win32::System::Pipes::NAMED_PIPE_MODE) -> Self {
            let name = format!(r"\\.\pipe\aviutl2-mcp-win-io-{}", InstanceId::new_v4());
            let wide: Vec<u16> = OsStr::new(&name)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();

            // SAFETY: `wide` は NUL 終端した pipe 名であり、呼び出し中は生存している。
            let server = unsafe {
                CreateNamedPipeW(
                    PCWSTR(wide.as_ptr()),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                    mode | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    64 * 1024,
                    64 * 1024,
                    0,
                    None,
                )
            };
            assert!(!server.is_invalid(), "テスト用 pipe の作成に失敗しました");

            // SAFETY: `wide` は NUL 終端した pipe 名であり、呼び出し中は生存している。
            let client = unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    None,
                )
            }
            .expect("テスト用 pipe への接続に失敗しました");

            // client が先に接続済みのため、この呼び出しは待たずに戻る。
            // SAFETY: `server` は直前に作成した有効な pipe ハンドル。
            let _ = unsafe { ConnectNamedPipe(server, None) };

            Self { server, client }
        }

        /// 対向端だけを閉じ、client 側から見て切断された状態にする。
        fn close_server(&mut self) {
            close_handle(&mut self.server);
        }
    }

    impl Drop for PipePair {
        fn drop(&mut self) {
            close_handle(&mut self.client);
            close_handle(&mut self.server);
        }
    }

    /// 二重クローズを避けつつハンドルを閉じる。
    fn close_handle(handle: &mut HANDLE) {
        if handle.is_invalid() {
            return;
        }
        // SAFETY: 呼び出し元が単独で所有するハンドルであり、閉じた後は無効値で
        // 上書きするため再度閉じられることはない。
        unsafe {
            let _ = CloseHandle(*handle);
        }
        *handle = HANDLE::default();
    }

    /// 自プロセスが開いているカーネルハンドルの総数。
    fn process_handle_count() -> u32 {
        let mut count = 0u32;
        // SAFETY: `GetCurrentProcess` が返す擬似ハンドルは常に有効であり、
        // `count` はスタック上の有効な書き込み先。
        unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) }
            .expect("プロセスのハンドル数を取得できません");
        count
    }

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
    fn win32_error_keeps_raw_os_error() {
        for code in [
            ERROR_FILE_NOT_FOUND,
            ERROR_BROKEN_PIPE,
            ERROR_INVALID_HANDLE,
            ERROR_OPERATION_ABORTED,
        ] {
            let converted = to_io_error(windows::core::Error::from_hresult(code.to_hresult()));
            assert_eq!(
                converted.raw_os_error(),
                Some(code.0 as i32),
                "HRESULT ではなく生の Win32 コードを保持する"
            );
        }

        let broken_pipe = to_io_error(windows::core::Error::from_hresult(
            ERROR_BROKEN_PIPE.to_hresult(),
        ));
        assert_eq!(
            broken_pipe.kind(),
            io::ErrorKind::BrokenPipe,
            "生の Win32 コードから種別が分類できる"
        );
    }

    #[test]
    fn non_win32_hresult_has_no_raw_os_error() {
        // E_NOTIMPL。FACILITY_WIN32 ではないため Win32 コードへは還元できない。
        let converted = to_io_error(windows::core::Error::from_hresult(windows::core::HRESULT(
            0x8000_4001_u32 as i32,
        )));
        assert_eq!(
            converted.raw_os_error(),
            None,
            "Win32 由来でない HRESULT を生の OS エラーとして扱わない"
        );
    }

    #[test]
    fn failed_io_reports_raw_win32_error() {
        let mut buf = [0u8; 1];
        let deadline = Instant::now() + Duration::from_millis(200);
        // 無効なハンドルへの読み取りは同期的に失敗し、保留 I/O を残さない。
        let error = read_exact(HANDLE::default(), &mut buf, deadline)
            .expect_err("無効なハンドルへの読み取りは失敗する");
        let WinIoError::Io(error) = error else {
            panic!("期限超過ではなく I/O エラーになる: {error:?}");
        };
        assert_eq!(error.raw_os_error(), Some(ERROR_INVALID_HANDLE.0 as i32));
    }

    #[test]
    fn a_queued_issue_is_recorded_as_pending_even_when_it_succeeds() {
        // `ReadDirectoryChangesW` は overlapped ハンドルに対して成功を返しても、
        // それは要求が登録されたことしか意味しない。完了として扱うと保留状態が
        // 記録されず、`Drop` がキャンセルを飛ばして転送バッファを解放する。
        let pipe = PipePair::create();
        // SAFETY: `pipe` は本テストの終わりまで生存し、`op` はその前に drop される。
        let mut op = unsafe { OverlappedOp::new(pipe.client) }.unwrap();

        // SAFETY: 発行しないため、カーネルへ渡す転送バッファも存在しない。
        unsafe { op.issue_queued(|_| Ok(())) }.unwrap();
        assert!(
            op.is_pending(),
            "成功した発行が保留として記録されていません"
        );

        // 期限超過の経路でキャンセルと完了待ちが走る。走らなければ保留のまま
        // 次の発行の表明に掛かる。
        let error = op
            .await_completion(Instant::now() + SHORT_DEADLINE)
            .expect_err("何も届かないため期限を超過する");
        assert!(matches!(error, WinIoError::TimedOut), "{error:?}");
        assert!(!op.is_pending());
        op.begin().unwrap();
    }

    #[test]
    fn a_queued_issue_reports_a_real_failure() {
        let pipe = PipePair::create();
        // SAFETY: `pipe` は本テストの終わりまで生存し、`op` はその前に drop される。
        let mut op = unsafe { OverlappedOp::new(pipe.client) }.unwrap();

        // SAFETY: 発行が失敗を返すため、カーネルへ渡す転送バッファは無い。
        let error = unsafe {
            op.issue_queued(|_| {
                Err(windows::core::Error::from_hresult(
                    ERROR_INVALID_HANDLE.to_hresult(),
                ))
            })
        }
        .expect_err("発行の失敗はそのまま伝わる");
        assert_eq!(error.raw_os_error(), Some(ERROR_INVALID_HANDLE.0 as i32));
        assert!(!op.is_pending(), "失敗した発行を保留として記録しています");
    }

    #[test]
    fn a_completion_that_carries_an_error_still_clears_the_pending_state() {
        // 失敗として完了した I/O も、カーネルは既に `OVERLAPPED` と転送バッファ
        // を手放している。保留のまま残すと次の発行が `begin` の表明に掛かり、
        // `Drop` は不要なキャンセルの排出に失敗してプロセスを異常終了させる。
        let mut pipe = PipePair::create();
        let mut buf = [0u8; 8];
        // SAFETY: `pipe` は本テストの終わりまで生存し、`op` はその前に drop される。
        let mut op = unsafe { OverlappedOp::new(pipe.client) }.unwrap();

        op.begin().unwrap();
        // SAFETY: `buf` は本テストのスコープで生存し、`op` は同じスコープ内で
        // 完了まで待ってから drop される。
        let issued = unsafe { ReadFile(pipe.client, Some(&mut buf), None, Some(op.as_mut_ptr())) };
        assert_eq!(op.classify(issued).unwrap(), IoIssue::Pending);

        // 対向端を閉じると、保留中の読み取りは失敗として完了する。
        pipe.close_server();

        let error = op
            .await_completion(Instant::now() + Duration::from_secs(5))
            .expect_err("切断された pipe からは読み取れない");
        assert!(
            matches!(error, WinIoError::Io(_)),
            "期限超過ではなく I/O エラーとして報告する: {error:?}"
        );
        assert!(
            !op.is_pending(),
            "失敗として完了した I/O が保留のまま残りました"
        );
        // 保留が残っていればここで表明に掛かる。
        op.begin().unwrap();
    }

    #[test]
    fn leaves_io_pending_matches_hresult_form() {
        for code in [
            ERROR_INVALID_HANDLE,
            ERROR_INVALID_PARAMETER,
            ERROR_IO_INCOMPLETE,
        ] {
            assert!(
                leaves_io_pending(&windows::core::Error::from_hresult(code.to_hresult())),
                "完了状態を取得できない失敗は I/O が保留されたままであり得る"
            );
        }

        for code in [ERROR_BROKEN_PIPE, ERROR_OPERATION_ABORTED] {
            assert!(
                !leaves_io_pending(&windows::core::Error::from_hresult(code.to_hresult())),
                "I/O 自体の完了状態を示す失敗ではカーネルは既に手放している"
            );
        }
    }

    #[test]
    fn remaining_until_is_zero_for_past_deadline() {
        assert_eq!(
            remaining_until(Instant::now() - Duration::from_secs(1)),
            Duration::ZERO
        );
        assert!(!remaining_until(Instant::now() + Duration::from_secs(1)).is_zero());
    }

    #[test]
    fn timed_out_read_leaves_no_pending_io() {
        let pipe = PipePair::create();
        let mut buf = [0u8; 4];
        // SAFETY: `pipe` は本テストの終わりまで生存し、`op` はその前に drop される。
        let mut op = unsafe { OverlappedOp::new(pipe.client) }.unwrap();

        op.begin().unwrap();
        // SAFETY: `buf` は本テストのスコープで生存し、`op` は同じスコープ内で
        // キャンセル完了まで待ってから drop される。
        let issued = unsafe { ReadFile(pipe.client, Some(&mut buf), None, Some(op.as_mut_ptr())) };
        assert_eq!(
            op.classify(issued).unwrap(),
            IoIssue::Pending,
            "相手が何も送らないため読み取りはカーネルに保留される"
        );
        assert!(op.pending, "保留状態が記録される");

        let error = op
            .await_completion(Instant::now() + SHORT_DEADLINE)
            .expect_err("相手が何も送らないため期限を超過する");
        assert!(
            matches!(error, WinIoError::TimedOut),
            "期限超過として報告される: {error:?}"
        );
        assert!(
            !op.pending,
            "期限超過時に I/O をキャンセルし完了を確定させてから戻る"
        );
        // 保留 I/O が残っていれば `begin` の表明に掛かる。
        op.begin().unwrap();
    }

    #[test]
    fn timed_out_read_never_writes_buffer_afterwards() {
        const SENTINEL: [u8; 8] = [0xA5; 8];
        const LATE_DATA: [u8; 8] = [0x5C; 8];

        for attempt in 0..5 {
            let pipe = PipePair::create();
            let mut buf = vec![0u8; SENTINEL.len()];

            let error = read_exact(pipe.client, &mut buf, Instant::now() + SHORT_DEADLINE)
                .expect_err("相手が何も送らないため期限を超過する");
            assert!(
                matches!(error, WinIoError::TimedOut),
                "{attempt} 回目が期限超過にならない: {error:?}"
            );

            // 期限超過後にバッファを書き換える。キャンセルせずに戻る実装であれば、
            // このあと届くデータで保留中の読み取りが完了し番兵を上書きする。
            buf.copy_from_slice(&SENTINEL);
            write_all(
                pipe.server,
                &LATE_DATA,
                Instant::now() + Duration::from_secs(5),
            )
            .expect("対向端への送信に失敗しました");
            std::thread::sleep(LATE_COMPLETION_GRACE);

            assert_eq!(
                std::hint::black_box(&buf)[..],
                SENTINEL[..],
                "{attempt} 回目: 期限超過後にカーネルが読み取りバッファを書き換えた"
            );
        }
    }

    #[test]
    fn repeated_transfers_do_not_leak_handles() {
        /// 1 回のリークが 1 ハンドルに対応するため、測定誤差と明確に差が付く回数。
        const ITERATIONS: usize = 200;
        /// 同一プロセスで並行するテストがハンドルを開閉する分の許容幅。
        /// リーク時の増分（`ITERATIONS` × 2 = 400）より十分小さい。
        const TOLERANCE: u32 = 32;
        /// 一過性のノイズと単調増加を切り分けるための測定回数。
        const ATTEMPTS: usize = 3;

        let pipe = PipePair::create();
        let payload = [0xC3u8; 8];
        let transfer = |count: usize| {
            for _ in 0..count {
                let deadline = Instant::now() + Duration::from_secs(5);
                write_all(pipe.client, &payload, deadline).expect("送信に失敗しました");
                let mut received = [0u8; 8];
                read_exact(pipe.server, &mut received, deadline).expect("受信に失敗しました");
                assert_eq!(received, payload);
            }
        };

        // 遅延初期化で確保されるハンドルを測定前に確定させる。
        transfer(20);

        let mut deltas = Vec::with_capacity(ATTEMPTS);
        for _ in 0..ATTEMPTS {
            let before = process_handle_count();
            transfer(ITERATIONS);
            let delta = process_handle_count().saturating_sub(before);
            if delta <= TOLERANCE {
                return;
            }
            deltas.push(delta);
        }

        panic!(
            "{ITERATIONS} 回の読み書きでハンドル数が {deltas:?} 増加しました（許容 {TOLERANCE}）"
        );
    }

    #[test]
    fn read_reports_disconnect_without_spinning_until_deadline() {
        let mut pipe = PipePair::create();
        pipe.close_server();

        let started = Instant::now();
        let mut buf = [0u8; 4];
        let error = read_exact(pipe.client, &mut buf, started + Duration::from_secs(10))
            .expect_err("切断された pipe からは読み取れない");
        assert!(
            matches!(error, WinIoError::Io(_)),
            "期限超過ではなく I/O エラーとして報告する: {error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "進捗のないまま期限まで回り続けている: {}ms",
            started.elapsed().as_millis()
        );
    }

    #[test]
    fn write_reports_disconnect_without_spinning_until_deadline() {
        let mut pipe = PipePair::create();
        pipe.close_server();

        let started = Instant::now();
        let error = write_all(pipe.client, b"payload", started + Duration::from_secs(10))
            .expect_err("切断された pipe へは書き込めない");
        assert!(
            matches!(error, WinIoError::Io(_)),
            "期限超過ではなく I/O エラーとして報告する: {error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "進捗のないまま期限まで回り続けている: {}ms",
            started.elapsed().as_millis()
        );
    }

    #[test]
    fn zero_byte_transfer_stops_instead_of_looping() {
        let pipe = PipePair::create_message_mode();
        // 長さ 0 のメッセージを送ると、読み取りは成功しつつ転送 0 バイトで完了する。
        // 進捗が無いまま再発行を繰り返すと期限まで回り続ける。
        let mut written = 0u32;
        // SAFETY: `pipe.server` は本テストが所有する有効なハンドルで、
        // 長さ 0 の書き込みはバッファを参照しない。
        unsafe { WriteFile(pipe.server, Some(&[]), Some(&mut written), None) }
            .expect("長さ 0 のメッセージ送信に失敗しました");

        let started = Instant::now();
        let mut buf = [0u8; 4];
        let error = read_exact(pipe.client, &mut buf, started + Duration::from_secs(10))
            .expect_err("転送 0 バイトは進捗が無いため打ち切られる");
        assert!(
            matches!(error, WinIoError::Io(_)),
            "転送 0 バイトを期限超過ではなく打ち切りとして報告する: {error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "転送 0 バイトのまま期限まで回り続けている: {}ms",
            started.elapsed().as_millis()
        );
    }

    #[test]
    fn wait_failure_is_distinguished_from_timeout() {
        // 無効なハンドルへの待機は `WAIT_FAILED` になる。期限は十分先に置くため、
        // 期限超過と取り違えていればここで待ち続けることになる。
        let deadline = Instant::now() + Duration::from_secs(30);
        let started = Instant::now();
        let outcome = wait_one(HANDLE::default(), deadline);
        let WaitOutcome::Failed(error) = outcome else {
            panic!("待機失敗が期限超過と混同されています: {outcome:?}");
        };
        assert_eq!(error.raw_os_error(), Some(ERROR_INVALID_HANDLE.0 as i32));
        assert!(started.elapsed() < Duration::from_secs(1));

        let outcome = wait_any(&[HANDLE::default()], Some(deadline));
        assert!(
            matches!(outcome, WaitAnyOutcome::Failed(_)),
            "複数待機でも待機失敗を期限超過と混同しない: {outcome:?}"
        );
    }

    #[test]
    fn abandoned_object_is_distinguished_from_timeout() {
        /// 所有スレッドへ生ハンドルを持ち込むための包み。所有権は移動しない。
        struct RawHandle(HANDLE);
        // SAFETY: ミューテックスハンドルはスレッドを跨いで使用でき、
        // 所有権は呼び出し元に残る。
        unsafe impl Send for RawHandle {}

        /// 取得したまま所有スレッドが終了した（放棄された）ミューテックスを作る。
        ///
        /// 放棄されたミューテックスは待機に成功した側が所有権を得るため、
        /// 検証のたびに作り直す。
        fn abandoned_mutex() -> HANDLE {
            // SAFETY: 名前なし・既定のセキュリティ属性でミューテックスを作成する。
            let mutex = unsafe { CreateMutexW(None, false, None) }
                .expect("テスト用ミューテックスの作成に失敗しました");
            let owned = RawHandle(mutex);
            std::thread::spawn(move || {
                let owned = owned;
                // SAFETY: `mutex` はこのスレッドの終了まで呼び出し元が保持している。
                let result = unsafe { WaitForSingleObject(owned.0, 5_000) };
                assert_eq!(result, WAIT_OBJECT_0, "ミューテックスを取得できません");
                // 解放せずに終了し、ミューテックスを放棄状態にする。
            })
            .join()
            .expect("所有スレッドが異常終了しました");
            mutex
        }

        let mut mutex = abandoned_mutex();
        let outcome = wait_one(mutex, Instant::now() + Duration::from_secs(30));
        assert!(
            matches!(outcome, WaitOutcome::Failed(_)),
            "放棄された同期オブジェクトを期限超過と混同しない: {outcome:?}"
        );
        close_handle(&mut mutex);

        let mut mutex = abandoned_mutex();
        let outcome = wait_any(&[mutex], Some(Instant::now() + Duration::from_secs(30)));
        assert!(
            matches!(outcome, WaitAnyOutcome::Failed(_)),
            "複数待機でも放棄を期限超過と混同しない: {outcome:?}"
        );
        close_handle(&mut mutex);
    }
}
