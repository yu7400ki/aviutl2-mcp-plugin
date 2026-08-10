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
    ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED, ERROR_PIPE_NOT_CONNECTED, HANDLE, WAIT_ABANDONED_0,
    WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{
    CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::core::Owned;

/// 無期限待機を表すミリ秒値。
pub(crate) const WAIT_INFINITE: u32 = windows::Win32::System::Threading::INFINITE;

/// 手動リセットイベントの所有ハンドル。
pub(crate) struct EventHandle {
    handle: Owned<HANDLE>,
}

// イベントオブジェクトはスレッド間で共有しても安全であり、`EventHandle` は
// ハンドルの所有権を単一の値に閉じている。
unsafe impl Send for EventHandle {}
unsafe impl Sync for EventHandle {}

impl EventHandle {
    /// 非シグナル状態の手動リセットイベントを作成する。
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: 名前なし・既定のセキュリティ属性でイベントを作成する呼び出しで、
        // ポインタ引数はすべて None を渡している。返るハンドルは成功時のみ得られ、
        // その所有権はこの値だけが持つ。
        let handle =
            unsafe { Owned::new(CreateEventW(None, true, false, None).map_err(to_io_error)?) };
        Ok(Self { handle })
    }

    /// 生ハンドルを取得する。所有権は移動しない。
    pub(crate) fn raw(&self) -> HANDLE {
        *self.handle
    }

    /// イベントをシグナル状態にする。
    pub(crate) fn signal(&self) -> io::Result<()> {
        // SAFETY: `self.handle` は本型が所有する有効なイベントハンドル。
        unsafe { SetEvent(*self.handle) }.map_err(to_io_error)
    }

    /// 指定ミリ秒だけシグナルを待つ。
    pub(crate) fn wait(&self, timeout_ms: u32) -> WaitOutcome {
        // SAFETY: `self.handle` は本型が所有する有効なイベントハンドル。
        let result = unsafe { WaitForSingleObject(*self.handle, timeout_ms) };
        classify_wait(result.0, 1)
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
    /// 立てるため、`start` が I/O をカーネルへ渡した直後に巻き戻っても
    /// `Drop` が確実にキャンセルを試みる。
    ///
    /// `start` に渡せるのは、`ERROR_IO_PENDING` 以外のエラーで同期的に失敗した
    /// 場合に I/O をカーネルへ渡さない API に限る（`ReadFile` / `WriteFile` /
    /// `ConnectNamedPipe` はこれを満たす）。この前提の下では完了通知イベントが
    /// 後からシグナルされることはないため、同期失敗時に保留状態を落とす。
    /// 保留状態を残すと `Drop` の排出（`GetOverlappedResult` を bWait = TRUE で
    /// 待つ）が永久に戻らなくなる。逆に、同期失敗後も完了通知を出し得る API を
    /// 渡すと、保留 I/O を残したまま `OVERLAPPED` を解放することになる。
    pub(crate) fn issue(
        &mut self,
        start: impl FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>,
    ) -> windows::core::Result<()> {
        self.rearm()?;
        let overlapped = self.as_mut_ptr();
        self.pending = true;
        let result = start(overlapped);
        if let Err(e) = &result
            && e.code() != ERROR_IO_PENDING.into()
        {
            self.pending = false;
        }
        result
    }

    /// 保留 I/O の完了を確認して転送バイト数を得る。
    ///
    /// `wait` が true のときは完了するまでブロックする。完了が確定した時点で
    /// 保留状態を落とす。
    pub(crate) fn result(&mut self, wait: bool) -> windows::core::Result<u32> {
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
                Err(e)
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
    ///
    /// 保留中の I/O がある状態で呼ぶと、カーネルが参照している `OVERLAPPED` を
    /// 上書きしてしまう。
    fn rearm(&mut self) -> windows::core::Result<()> {
        debug_assert!(!self.pending, "保留中の I/O がある状態で再発行はできない");
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
///
/// `windows::core::Error` は Win32 エラーを `HRESULT`（`0x8007_XXXX`）として
/// 保持するため、そのまま `from_raw_os_error` へ渡すと `raw_os_error` が
/// 生の Win32 コードと一致しなくなる。FACILITY_WIN32 の場合は元のコードへ戻す。
fn to_io_error(e: windows::core::Error) -> io::Error {
    let hresult = e.code().0 as u32;
    if hresult & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((hresult & 0xFFFF) as i32)
    } else {
        io::Error::other(e)
    }
}

/// `ERROR_OPERATION_ABORTED` はキャンセルの正常完了として扱う。
fn is_operation_aborted(e: &windows::core::Error) -> bool {
    e.code() == ERROR_OPERATION_ABORTED.into()
}

/// 相手側が接続を閉じたことを示すエラーかどうか。
///
/// 比較は `HRESULT` 同士で行う。`windows::core::Error` が保持するのは
/// `HRESULT` であり、生の Win32 値と直接比較すると常に不一致になる。
fn is_disconnect(e: &windows::core::Error) -> bool {
    let code = e.code();
    code == ERROR_BROKEN_PIPE.into()
        || code == ERROR_PIPE_NOT_CONNECTED.into()
        || code == ERROR_NO_DATA.into()
        || code == ERROR_HANDLE_EOF.into()
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
            Err(e) => return Err(IoError::Os(to_io_error(e))),
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
                    Err(e) => return Err(IoError::Os(to_io_error(e))),
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
            if is_disconnect(&e) {
                return Ok(Transfer::Eof);
            }
            return Err(IoError::Os(to_io_error(e)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::identifier::InstanceId;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::time::Duration;
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, NAMED_PIPE_MODE, PIPE_READMODE_BYTE,
        PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_TYPE_MESSAGE,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
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
    /// `client` 側を本番と同じ経路（`read_exact_deadline` / `write_all_deadline`）で
    /// 駆動し、`server` 側を対向端として使う。
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

        fn create_with(mode: NAMED_PIPE_MODE) -> Self {
            let name = format!(
                r"\\.\pipe\aviutl2-mcp-plugin-win-io-{}",
                InstanceId::new_v4()
            );
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

        /// 対向端から `bytes` を送る。
        fn send(&self, bytes: &[u8]) {
            // SAFETY: `self.server` は本型が所有する有効なハンドルで、
            // `bytes` は本呼び出し中に生存している。
            unsafe { write_all_deadline(self.server, bytes, deadline(5_000), None) }
                .expect("対向端への送信に失敗しました");
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

    /// 現在時刻から `millis` ミリ秒後の期限。
    fn deadline(millis: u64) -> Instant {
        Instant::now() + Duration::from_millis(millis)
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
    fn classify_wait_distinguishes_timeout_from_failure() {
        assert!(matches!(
            classify_wait(WAIT_TIMEOUT.0, 1),
            WaitOutcome::TimedOut
        ));
        assert!(
            matches!(classify_wait(WAIT_FAILED.0, 1), WaitOutcome::Failed(_)),
            "待機 API の失敗を期限超過と混同しない"
        );
        assert!(
            matches!(classify_wait(WAIT_ABANDONED_0.0, 2), WaitOutcome::Failed(_)),
            "放棄された同期オブジェクトを期限超過と混同しない"
        );
        assert!(
            matches!(
                classify_wait(WAIT_ABANDONED_0.0 + 1, 2),
                WaitOutcome::Failed(_)
            ),
            "配列内のどの位置が放棄されても期限超過と混同しない"
        );
        assert!(
            matches!(
                classify_wait(WAIT_OBJECT_0.0 + 1, 2),
                WaitOutcome::Signaled(1)
            ),
            "シグナルされた添字をそのまま返す"
        );
        assert!(
            matches!(
                classify_wait(WAIT_OBJECT_0.0 + 2, 2),
                WaitOutcome::Failed(_)
            ),
            "待機配列の範囲外は失敗として扱う"
        );
    }

    #[test]
    fn issued_read_is_recorded_as_pending() {
        let pipe = PipePair::create();
        let mut buf = [0u8; 4];
        // SAFETY: `pipe` は本テストの終わりまで生存し、`op` はその前に drop される。
        let mut op = unsafe { OverlappedOp::new(pipe.client) }.unwrap();

        let issued = op.issue(|overlapped| {
            // SAFETY: `buf` は本テストのスコープで生存し、`op` は同じスコープ内で
            // キャンセル完了まで待ってから drop される。
            unsafe { ReadFile(pipe.client, Some(&mut buf), None, Some(overlapped)) }
        });
        let error = issued.expect_err("相手が何も送らないため読み取りは保留される");
        assert_eq!(error.code(), ERROR_IO_PENDING.into());
        assert!(op.pending, "保留状態が記録される");

        op.cancel_and_drain();
        assert!(!op.pending, "キャンセル完了を確認してから保留状態を落とす");
        // 保留 I/O が残っていれば `rearm` の表明に掛かる。
        op.rearm().unwrap();
    }

    #[test]
    fn timed_out_transfer_leaves_no_pending_io() {
        let pipe = PipePair::create();
        let mut buf = [0u8; 4];
        // SAFETY: `pipe` は本テストの終わりまで生存し、`op` はその前に drop される。
        let mut op = unsafe { OverlappedOp::new(pipe.client) }.unwrap();

        let result = transfer_once(
            Instant::now() + SHORT_DEADLINE,
            None,
            &mut op,
            |overlapped| {
                // SAFETY: `buf` は本テストのスコープで生存し、`op` は同じスコープ内で
                // キャンセル完了まで待ってから drop される。
                unsafe { ReadFile(pipe.client, Some(&mut buf), None, Some(overlapped)) }
            },
        );
        let error = result.err().expect("相手が何も送らないため期限を超過する");
        assert!(
            matches!(error, IoError::TimedOut),
            "期限超過として報告される: {error:?}"
        );
        assert!(
            !op.pending,
            "期限超過時に I/O をキャンセルし完了を確定させてから戻る"
        );
        // 保留 I/O が残っていれば `rearm` の表明に掛かる。
        op.rearm().unwrap();
    }

    #[test]
    fn cancelled_transfer_leaves_no_pending_io() {
        let pipe = PipePair::create();
        let stop = EventHandle::new().unwrap();
        stop.signal().unwrap();
        let mut buf = [0u8; 4];
        // SAFETY: `pipe` は本テストの終わりまで生存し、`op` はその前に drop される。
        let mut op = unsafe { OverlappedOp::new(pipe.client) }.unwrap();

        let result = transfer_once(
            Instant::now() + Duration::from_secs(5),
            Some(stop.raw()),
            &mut op,
            |overlapped| {
                // SAFETY: `buf` は本テストのスコープで生存し、`op` は同じスコープ内で
                // キャンセル完了まで待ってから drop される。
                unsafe { ReadFile(pipe.client, Some(&mut buf), None, Some(overlapped)) }
            },
        );
        let error = result
            .err()
            .expect("中断イベントがシグナル済みのため読み取りは中断される");
        assert!(
            matches!(error, IoError::Cancelled),
            "中断として報告される: {error:?}"
        );
        assert!(
            !op.pending,
            "中断時に I/O をキャンセルし完了を確定させてから戻る"
        );
        op.rearm().unwrap();
    }

    #[test]
    fn timed_out_read_never_writes_buffer_afterwards() {
        const SENTINEL: [u8; 8] = [0xA5; 8];
        const LATE_DATA: [u8; 8] = [0x5C; 8];

        for attempt in 0..5 {
            let pipe = PipePair::create();
            let mut buf = vec![0u8; SENTINEL.len()];

            // SAFETY: `pipe.client` は本ループの反復中ずっと有効である。
            let error = unsafe {
                read_exact_deadline(pipe.client, &mut buf, Instant::now() + SHORT_DEADLINE, None)
            }
            .expect_err("相手が何も送らないため期限を超過する");
            assert!(
                matches!(error, IoError::TimedOut),
                "{attempt} 回目が期限超過にならない: {error:?}"
            );

            // 期限超過後にバッファを書き換える。キャンセルせずに戻る実装であれば、
            // このあと届くデータで保留中の読み取りが完了し番兵を上書きする。
            buf.copy_from_slice(&SENTINEL);
            pipe.send(&LATE_DATA);
            std::thread::sleep(LATE_COMPLETION_GRACE);

            assert_eq!(
                std::hint::black_box(&buf)[..],
                SENTINEL[..],
                "{attempt} 回目: 期限超過後にカーネルが読み取りバッファを書き換えた"
            );
        }
    }

    #[test]
    fn cancelled_read_never_writes_buffer_afterwards() {
        const SENTINEL: [u8; 8] = [0x3C; 8];
        const LATE_DATA: [u8; 8] = [0xE7; 8];

        for attempt in 0..5 {
            let pipe = PipePair::create();
            let stop = EventHandle::new().unwrap();
            stop.signal().unwrap();
            let mut buf = vec![0u8; SENTINEL.len()];

            // SAFETY: `pipe.client` と `stop` は本ループの反復中ずっと有効である。
            let error = unsafe {
                read_exact_deadline(pipe.client, &mut buf, deadline(5_000), Some(stop.raw()))
            }
            .expect_err("中断イベントがシグナル済みのため読み取りは中断される");
            assert!(
                matches!(error, IoError::Cancelled),
                "{attempt} 回目が中断にならない: {error:?}"
            );

            buf.copy_from_slice(&SENTINEL);
            pipe.send(&LATE_DATA);
            std::thread::sleep(LATE_COMPLETION_GRACE);

            assert_eq!(
                std::hint::black_box(&buf)[..],
                SENTINEL[..],
                "{attempt} 回目: 中断後にカーネルが読み取りバッファを書き換えた"
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
                let mut received = [0u8; 8];
                // SAFETY: 両ハンドルは `pipe` が所有し、本クロージャの実行中は有効。
                unsafe {
                    write_all_deadline(pipe.client, &payload, deadline(5_000), None)
                        .expect("送信に失敗しました");
                    let read =
                        read_exact_deadline(pipe.server, &mut received, deadline(5_000), None)
                            .expect("受信に失敗しました");
                    assert!(read, "相手が切断していない限り読み取りは成立する");
                }
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
        // SAFETY: `pipe.client` は本テストの終わりまで有効である。
        let read = unsafe {
            read_exact_deadline(
                pipe.client,
                &mut buf,
                started + Duration::from_secs(10),
                None,
            )
        }
        .expect("切断は EOF として報告される");
        assert!(!read, "1 バイトも読めないまま切断された");
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
        // SAFETY: `pipe.client` は本テストの終わりまで有効である。
        let error = unsafe {
            write_all_deadline(
                pipe.client,
                b"payload",
                started + Duration::from_secs(10),
                None,
            )
        }
        .expect_err("切断された pipe へは書き込めない");
        assert!(
            matches!(error, IoError::Os(_)),
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
        // SAFETY: `pipe.client` は本テストの終わりまで有効である。
        let read = unsafe {
            read_exact_deadline(
                pipe.client,
                &mut buf,
                started + Duration::from_secs(10),
                None,
            )
        }
        .expect("転送 0 バイトは EOF として打ち切られる");
        assert!(!read, "転送 0 バイトを進捗として数えない");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "転送 0 バイトのまま期限まで回り続けている: {}ms",
            started.elapsed().as_millis()
        );
    }
}
