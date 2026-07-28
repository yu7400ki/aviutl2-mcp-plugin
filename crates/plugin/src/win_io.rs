//! Windows overlapped I/O の RAII ラッパー。
//!
//! イベントハンドルと `OVERLAPPED` の寿命を型で縛り、期限付きの read / write を
//! 提供する。保留 I/O を残したまま `OVERLAPPED` やバッファを破棄すると、後から
//! 完了した I/O が解放済みメモリへ書き込む。`OverlappedOp` は `Drop` で必ず
//! `CancelIoEx` と完了待ちを行うため、期限超過・エラー・panic 巻き戻しの
//! いずれの経路でも保留 I/O がゼロになる。

use std::io;
use std::time::Instant;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING,
    ERROR_NO_DATA, ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED, ERROR_PIPE_NOT_CONNECTED, HANDLE,
    WAIT_ABANDONED_0, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{
    CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};

/// 無期限待機を表すミリ秒値。
pub(crate) const WAIT_INFINITE: u32 = windows::Win32::System::Threading::INFINITE;

/// 手動リセットイベントの所有ハンドル。
pub(crate) struct EventHandle {
    handle: HANDLE,
}

// イベントオブジェクトはスレッド間で共有しても安全であり、`EventHandle` は
// ハンドルの所有権を単一の値に閉じている。
unsafe impl Send for EventHandle {}
unsafe impl Sync for EventHandle {}

impl EventHandle {
    /// 非シグナル状態の手動リセットイベントを作成する。
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: 名前なし・既定のセキュリティ属性でイベントを作成する呼び出しで、
        // ポインタ引数はすべて None を渡している。
        let handle = unsafe { CreateEventW(None, true, false, None) }.map_err(to_io_error)?;
        Ok(Self { handle })
    }

    /// 生ハンドルを取得する。所有権は移動しない。
    pub(crate) fn raw(&self) -> HANDLE {
        self.handle
    }

    /// イベントをシグナル状態にする。
    pub(crate) fn signal(&self) -> io::Result<()> {
        // SAFETY: `self.handle` は本型が所有する有効なイベントハンドル。
        unsafe { SetEvent(self.handle) }.map_err(to_io_error)
    }

    /// 指定ミリ秒だけシグナルを待つ。
    pub(crate) fn wait(&self, timeout_ms: u32) -> WaitOutcome {
        // SAFETY: `self.handle` は本型が所有する有効なイベントハンドル。
        let result = unsafe { WaitForSingleObject(self.handle, timeout_ms) };
        classify_wait(result.0, 1)
    }
}

impl Drop for EventHandle {
    fn drop(&mut self) {
        // SAFETY: `self.handle` は本型が所有する有効なハンドルであり、
        // Drop 以降に参照されることはない。
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// 待機 API の結果分類。
///
/// `WAIT_TIMEOUT` を `WAIT_FAILED` / `WAIT_ABANDONED` と混同しないよう、
/// 呼び出し元へは列挙で返す。
#[derive(Debug)]
pub(crate) enum WaitOutcome {
    /// いずれかのオブジェクトがシグナルされた（待機配列内のインデックス付き）。
    Signaled(usize),
    /// 期限内にシグナルされなかった。
    TimedOut,
    /// 待機自体が失敗した（放棄された同期オブジェクトを含む）。
    Failed(io::Error),
}

/// `WaitForSingleObject` / `WaitForMultipleObjects` の戻り値を分類する。
fn classify_wait(raw: u32, count: usize) -> WaitOutcome {
    if raw == WAIT_TIMEOUT.0 {
        return WaitOutcome::TimedOut;
    }
    if raw == WAIT_FAILED.0 {
        return WaitOutcome::Failed(io::Error::last_os_error());
    }
    let count = count as u32;
    if let Some(index) = raw.checked_sub(WAIT_OBJECT_0.0).filter(|i| *i < count) {
        return WaitOutcome::Signaled(index as usize);
    }
    if let Some(index) = raw.checked_sub(WAIT_ABANDONED_0.0).filter(|i| *i < count) {
        return WaitOutcome::Failed(io::Error::other(format!(
            "待機対象が放棄されました (index={index})"
        )));
    }
    WaitOutcome::Failed(io::Error::other(format!("不明な待機結果です: 0x{raw:08X}")))
}

/// 複数ハンドルのいずれかがシグナルされるまで待つ。
pub(crate) fn wait_any(handles: &[HANDLE], timeout_ms: u32) -> WaitOutcome {
    // SAFETY: `handles` は呼び出し元が所有する有効な待機可能ハンドルの配列で、
    // 呼び出し中を通じて生存する。
    let result = unsafe { WaitForMultipleObjects(handles, false, timeout_ms) };
    classify_wait(result.0, handles.len())
}

/// 保留 I/O 1 件分の `OVERLAPPED`・イベント・発行対象ハンドルを束ねた RAII 型。
///
/// `OVERLAPPED` をヒープに置くのは、カーネルが保留中に参照し続けるアドレスを
/// 安定させるため。スタックに置くと早期 return でフレームが破棄され、
/// カーネルが解放済み領域へ書き込む。
///
/// 保留状態を自身で追跡し、`Drop` で `CancelIoEx` と完了待ちを行う。これにより
/// 「解放時点で保留 I/O が無い」ことが呼び出し規約ではなく型で保証される。
pub(crate) struct OverlappedOp {
    overlapped: Box<OVERLAPPED>,
    event: EventHandle,
    handle: HANDLE,
    pending: bool,
}

impl OverlappedOp {
    /// `handle` に対する保留 I/O 用の `OVERLAPPED` を用意する。
    ///
    /// # Safety
    ///
    /// `handle` は本 `OverlappedOp` が drop されるまで有効であり続けなければ
    /// ならない。`Drop` は `handle` に対して `CancelIoEx` と
    /// `GetOverlappedResult` を発行する。閉じ済みハンドル値は OS が別オブジェクト
    /// へ再割り当てし得るため、先に閉じると無関係なオブジェクトの I/O を
    /// キャンセルすることになる。
    pub(crate) unsafe fn new(handle: HANDLE) -> io::Result<Self> {
        let event = EventHandle::new()?;
        let mut overlapped = Box::new(OVERLAPPED::default());
        overlapped.hEvent = event.raw();
        Ok(Self {
            overlapped,
            event,
            handle,
            pending: false,
        })
    }

    /// I/O 完了通知に使うイベントの生ハンドル。
    pub(crate) fn event_handle(&self) -> HANDLE {
        self.event.raw()
    }

    /// `OVERLAPPED` への可変ポインタ。ヒープ上のため呼び出し間でアドレスは不変。
    fn as_mut_ptr(&mut self) -> *mut OVERLAPPED {
        std::ptr::from_mut(self.overlapped.as_mut())
    }

    /// overlapped I/O を発行する。
    ///
    /// `start` には初期化済みの `OVERLAPPED` ポインタを渡す。発行前に保留状態を
    /// 立てるため、`start` の戻り値にかかわらず `Drop` が確実にキャンセルを試みる。
    pub(crate) fn issue(
        &mut self,
        start: impl FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>,
    ) -> windows::core::Result<()> {
        self.rearm()?;
        let overlapped = self.as_mut_ptr();
        self.pending = true;
        start(overlapped)
    }

    /// 保留 I/O の完了を確認して転送バイト数を得る。
    ///
    /// `wait` が true のときは完了するまでブロックする。完了が確定した時点で
    /// 保留状態を落とす。
    pub(crate) fn result(&mut self, wait: bool) -> io::Result<u32> {
        let mut transferred = 0u32;
        let handle = self.handle;
        let overlapped = self.as_mut_ptr();
        // SAFETY: `handle` は本型が保持する I/O 発行対象で、`new` の前提により
        // 生存している。`overlapped` はその I/O に紐づくヒープ上の `OVERLAPPED`。
        let result = unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, wait) };
        match result {
            Ok(()) => {
                self.pending = false;
                Ok(transferred)
            }
            Err(e) => {
                // `ERROR_IO_INCOMPLETE` のみが「まだ保留中」を意味する。
                // 他のエラーは失敗として完了しているため保留状態を落とす。
                if e.code() != ERROR_IO_INCOMPLETE.into() {
                    self.pending = false;
                }
                Err(to_io_error(e))
            }
        }
    }

    /// 保留 I/O をキャンセルし、カーネルが完了させるまで待つ。
    pub(crate) fn cancel_and_drain(&mut self) {
        if !self.pending {
            return;
        }
        let handle = self.handle;
        let overlapped = self.as_mut_ptr();
        // SAFETY: `handle` は本型が保持する I/O 発行対象で、`new` の前提により
        // 生存している。`overlapped` はその I/O に紐づくヒープ上の `OVERLAPPED`。
        let cancelled = unsafe { CancelIoEx(handle, Some(overlapped)) };
        if let Err(e) = cancelled
            && e.code() != ERROR_NOT_FOUND.into()
        {
            // `ERROR_NOT_FOUND` は既に完了している場合であり正常。
            // それ以外の失敗はキャンセルが届かなかった可能性があり、
            // 直後の完了待ちが長引く要因になるため記録する。
            tracing::warn!("保留 I/O のキャンセル要求に失敗しました: {e}");
        }
        // `ERROR_NOT_FOUND` の場合でも完了を確認しなければ `OVERLAPPED` を
        // 解放できないため、`GetOverlappedResult` は省略しない。bWait = true。
        match self.result(true) {
            Ok(_) => {}
            Err(e) if is_operation_aborted(&e) => {}
            Err(e) => {
                tracing::debug!("保留 I/O のキャンセル完了確認に失敗しました: {e}");
            }
        }
        self.pending = false;
    }

    /// 次の I/O 発行に備えてイベントと `OVERLAPPED` を初期状態へ戻す。
    fn rearm(&mut self) -> windows::core::Result<()> {
        // SAFETY: `event` は本型が所有する有効なイベントハンドル。
        unsafe { ResetEvent(self.event.raw()) }?;
        let event = self.event.raw();
        *self.overlapped = OVERLAPPED::default();
        self.overlapped.hEvent = event;
        Ok(())
    }
}

impl Drop for OverlappedOp {
    fn drop(&mut self) {
        // 期限超過・エラー・panic 巻き戻しのいずれで解放されても、
        // カーネルが参照している `OVERLAPPED` とイベントを保留中のまま
        // 手放さない。
        self.cancel_and_drain();
    }
}

/// 期限付き I/O のエラー。
#[derive(Debug)]
pub(crate) enum IoError {
    /// 期限を超過した。
    TimedOut,
    /// 中断イベントがシグナルされた。
    Cancelled,
    /// OS レベルのエラー。
    Os(io::Error),
}

impl std::fmt::Display for IoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => write!(f, "期限を超過しました"),
            Self::Cancelled => write!(f, "中断されました"),
            Self::Os(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for IoError {}

/// `windows` crate のエラーを `std::io::Error` へ変換する。
fn to_io_error(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0)
}

/// `ERROR_OPERATION_ABORTED` はキャンセルの正常完了として扱う。
fn is_operation_aborted(err: &io::Error) -> bool {
    err.raw_os_error() == Some(ERROR_OPERATION_ABORTED.0 as i32)
}

/// 相手側が接続を閉じたことを示すエラーかどうか。
fn is_disconnect(err: &io::Error) -> bool {
    matches!(
        err.raw_os_error(),
        Some(code)
            if code == ERROR_BROKEN_PIPE.0 as i32
                || code == ERROR_PIPE_NOT_CONNECTED.0 as i32
                || code == ERROR_NO_DATA.0 as i32
                || code == ERROR_HANDLE_EOF.0 as i32
    )
}

/// 期限までの残り時間をミリ秒へ変換する。期限に達していれば `None`。
fn remaining_ms(deadline: Instant) -> Option<u32> {
    let now = Instant::now();
    if now >= deadline {
        return None;
    }
    let remaining = deadline.duration_since(now).as_millis();
    // 0ms を返すと即時タイムアウトになるため、残りがある限り最低 1ms は待つ。
    Some(remaining.clamp(1, u128::from(WAIT_INFINITE - 1)) as u32)
}

/// 1 回分の overlapped 転送の結果。
enum Transfer {
    /// 転送されたバイト数（1 以上）。
    Bytes(u32),
    /// 相手が接続を閉じた。
    Eof,
}

/// overlapped I/O を 1 回発行し、期限内の完了を待つ。
///
/// `start` は `OVERLAPPED` ポインタを受け取り `ReadFile` / `WriteFile` を発行する。
/// `cancel` を渡すと、そのイベントがシグナルされた時点で I/O を打ち切る。
fn transfer_once(
    deadline: Instant,
    cancel: Option<HANDLE>,
    op: &mut OverlappedOp,
    start: impl FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>,
) -> Result<Transfer, IoError> {
    // 同期完了が続く場合でも期限を超えて回り続けないよう、発行前に確認する。
    if remaining_ms(deadline).is_none() {
        return Err(IoError::TimedOut);
    }

    let transferred = match op.issue(start) {
        // 同期完了。転送バイト数は `GetOverlappedResult` で確定させる。
        Ok(()) => match op.result(true) {
            Ok(n) => n,
            Err(e) if is_disconnect(&e) => return Ok(Transfer::Eof),
            Err(e) => return Err(IoError::Os(e)),
        },
        Err(e) if e.code() == ERROR_IO_PENDING.into() => {
            let Some(timeout_ms) = remaining_ms(deadline) else {
                op.cancel_and_drain();
                return Err(IoError::TimedOut);
            };
            let outcome = match cancel {
                Some(cancel) => wait_any(&[op.event_handle(), cancel], timeout_ms),
                None => op.event.wait(timeout_ms),
            };
            match outcome {
                WaitOutcome::Signaled(0) => match op.result(false) {
                    Ok(n) => n,
                    Err(e) if is_disconnect(&e) => return Ok(Transfer::Eof),
                    Err(e) => return Err(IoError::Os(e)),
                },
                WaitOutcome::Signaled(_) => {
                    op.cancel_and_drain();
                    return Err(IoError::Cancelled);
                }
                WaitOutcome::TimedOut => {
                    op.cancel_and_drain();
                    return Err(IoError::TimedOut);
                }
                WaitOutcome::Failed(e) => {
                    op.cancel_and_drain();
                    return Err(IoError::Os(e));
                }
            }
        }
        Err(e) => {
            let err = to_io_error(e);
            if is_disconnect(&err) {
                return Ok(Transfer::Eof);
            }
            return Err(IoError::Os(err));
        }
    };

    // 転送 0 バイトで進捗しないままループを回すと無限ループになるため、
    // ここで EOF として打ち切る。
    if transferred == 0 {
        Ok(Transfer::Eof)
    } else {
        Ok(Transfer::Bytes(transferred))
    }
}

/// 期限内にバッファ全体を読み取る。
///
/// 1 バイトも読めないまま相手が切断した場合は `Ok(false)` を返す。
/// `cancel` がシグナルされた場合は `IoError::Cancelled` を返す。
///
/// # Safety
///
/// `handle` と `cancel` は本呼び出しが戻るまで有効であり続けなければならない。
pub(crate) unsafe fn read_exact_deadline(
    handle: HANDLE,
    buf: &mut [u8],
    deadline: Instant,
    cancel: Option<HANDLE>,
) -> Result<bool, IoError> {
    // SAFETY: 呼び出し元の前提により `handle` は本呼び出し中を通じて有効であり、
    // `op` は本関数の内側で drop される。
    let mut op = unsafe { OverlappedOp::new(handle) }.map_err(IoError::Os)?;
    let mut total = 0usize;
    while total < buf.len() {
        let chunk = &mut buf[total..];
        let transfer = transfer_once(deadline, cancel, &mut op, |overlapped| {
            // SAFETY: `chunk` は `buf` の生存中の部分領域、`overlapped` は
            // `op` が保持するヒープ上の `OVERLAPPED`。`op` の `Drop` が
            // 保留 I/O の完了を待つため、`chunk` が先に無効化されることはない。
            unsafe { ReadFile(handle, Some(chunk), None, Some(overlapped)) }
        })?;
        match transfer {
            Transfer::Eof => {
                return if total == 0 {
                    Ok(false)
                } else {
                    Err(IoError::Os(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "フレーム途中で接続が閉じられました",
                    )))
                };
            }
            Transfer::Bytes(n) => total += n as usize,
        }
    }
    Ok(true)
}

/// 期限内にバッファ全体を書き込む。
///
/// `cancel` がシグナルされた場合は `IoError::Cancelled` を返す。
///
/// # Safety
///
/// `handle` と `cancel` は本呼び出しが戻るまで有効であり続けなければならない。
pub(crate) unsafe fn write_all_deadline(
    handle: HANDLE,
    buf: &[u8],
    deadline: Instant,
    cancel: Option<HANDLE>,
) -> Result<(), IoError> {
    // SAFETY: 呼び出し元の前提により `handle` は本呼び出し中を通じて有効であり、
    // `op` は本関数の内側で drop される。
    let mut op = unsafe { OverlappedOp::new(handle) }.map_err(IoError::Os)?;
    let mut total = 0usize;
    while total < buf.len() {
        let chunk = &buf[total..];
        let transfer = transfer_once(deadline, cancel, &mut op, |overlapped| {
            // SAFETY: `chunk` は `buf` の生存中の部分領域、`overlapped` は
            // `op` が保持するヒープ上の `OVERLAPPED`。`op` の `Drop` が
            // 保留 I/O の完了を待つため、`chunk` が先に無効化されることはない。
            unsafe { WriteFile(handle, Some(chunk), None, Some(overlapped)) }
        })?;
        match transfer {
            Transfer::Eof => {
                return Err(IoError::Os(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "書き込み中に接続が閉じられました",
                )));
            }
            Transfer::Bytes(n) => total += n as usize,
        }
    }
    Ok(())
}
