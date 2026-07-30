//! Named pipe server 接続受付と byte stream 読み書き。
//!
//! duplex・byte mode・リモート接続拒否・保護 DACL で named pipe を作成し、
//! 専用スレッドで接続受理 → handshake/ping → 切断 のループを回す。
//! pipe は overlapped で作成し、読み書きには必ず期限を与える。

use crate::edit::EditAdapter;
use crate::lifecycle::Lifecycle;
use crate::read::ReadAdapter;
use crate::security::ProtectedSecurityAttributes;
use crate::session;
use crate::win_io::{self, EventHandle, IoError, OverlappedOp, WaitOutcome};
use anyhow::{Context, Result};
use aviutl2_mcp_core::framing::{DecoderState, FrameDecoder, encode_frame};
use aviutl2_mcp_core::identifier::pipe_name_for;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::thread::{JoinHandle, spawn};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
};
use windows::core::PCWSTR;

/// pipe の入出力バッファサイズ。
const PIPE_BUFFER_SIZE: u32 = 64 * 1024;

/// 1 回の読み取りで受け取る最大バイト数。
///
/// フレーム本体が大きい場合は複数回に分けて読み取り、デコーダへ逐次投入する。
/// これにより本体長にかかわらず読み取りバッファのサイズが一定に保たれる。
const READ_CHUNK_SIZE: usize = 8 * 1024;

/// 待受の再確立に失敗した際の初回再試行間隔。
///
/// 一過性の失敗で待受を諦めないために再試行するが、間隔を空けずに回すと
/// 恒久的な失敗時に CPU とログを占有する。停止イベントを待つ形で間隔を空ける
/// ことで、停止要求には即応しつつ再試行の頻度を抑える。
const ACCEPT_RETRY_MIN_INTERVAL: Duration = Duration::from_millis(200);

/// 失敗が続いた場合の再試行間隔の上限。
const ACCEPT_RETRY_MAX_INTERVAL: Duration = Duration::from_secs(10);

/// [`PipeServer`] 停止時の bounded join 上限。
///
/// ホスト終了時に無期限で待たないため有限にする。期限内に終了しなければ
/// スレッドを切り離してログ化する。
pub const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// pipe I/O のエラー。
#[derive(Debug, thiserror::Error)]
pub enum PipeError {
    /// 期限超過。どの操作が何ミリ秒待って諦めたかを含む。
    #[error("{operation}が {waited_ms}ms 待って期限を超過しました")]
    TimedOut {
        /// 期限を超過した操作の名前。
        operation: &'static str,
        /// 諦めるまでに待った時間（ミリ秒）。
        waited_ms: u128,
    },
    /// 停止要求により I/O を中断した。
    #[error("{operation}が停止要求により中断されました")]
    Cancelled {
        /// 中断された操作の名前。
        operation: &'static str,
    },
    /// OS レベルの I/O エラー。
    #[error("{operation}に失敗しました: {source}")]
    Io {
        /// 失敗した操作の名前。
        operation: &'static str,
        /// 元のエラー。
        source: std::io::Error,
    },
    /// フレーム途中で接続が閉じられた。
    #[error("{operation}の途中で接続が閉じられました")]
    UnexpectedEof {
        /// 中断された操作の名前。
        operation: &'static str,
    },
    /// フレームの符号化・復号が契約を満たさない。
    #[error("フレームの処理に失敗しました: {0}")]
    Framing(#[from] aviutl2_mcp_core::framing::FrameError),
}

/// `PipeStream` が所有するハンドルの役割。
enum StreamRole {
    /// `CreateNamedPipeW` で作成した server 側の接続ハンドル。
    /// 閉じる際に `DisconnectNamedPipe` を要する。
    Server,
    /// `CreateFileW` で開いた client 側のハンドル。閉じるのみ。
    #[cfg(test)]
    Client,
}

/// 1 本の named pipe 接続に対する読み書きストリーム。
///
/// 全ての読み書きは呼び出し元が与えた期限内で完了するか、期限超過として失敗する。
pub struct PipeStream {
    handle: HANDLE,
    role: StreamRole,
    cancel: Option<Arc<StopSignal>>,
}

// `HANDLE` は生ポインタだが、`PipeStream` は所有して単一スレッドで使用する。
unsafe impl Send for PipeStream {}

impl Drop for PipeStream {
    fn drop(&mut self) {
        // SAFETY: `self.handle` は本型が所有する有効なハンドルであり、
        // Drop 以降に参照されることはない。
        unsafe {
            if matches!(self.role, StreamRole::Server) {
                // `FlushFileBuffers` は呼ばない。クライアントが読み出さない限り
                // 無期限にブロックし、接続受理スレッドが停止要求で抜けられなくなる。
                // 送信済み応答がクライアントに読まれる前に破棄される窓は、
                // 要求ループが期限付き read でクライアント切断（EOF）を待ってから
                // 閉じることで塞いでいる。
                let _ = DisconnectNamedPipe(self.handle);
            }
            let _ = CloseHandle(self.handle);
        }
    }
}

impl PipeStream {
    /// server 側の接続ハンドルから `PipeStream` を作成する。
    ///
    /// # Safety
    ///
    /// `handle` は `CreateNamedPipeW` で作成した有効な接続ハンドルであり、
    /// 所有権を移譲できる必要がある。
    unsafe fn from_server_handle(handle: HANDLE, cancel: Option<Arc<StopSignal>>) -> Self {
        Self {
            handle,
            role: StreamRole::Server,
            cancel,
        }
    }

    /// client 側のハンドルから `PipeStream` を作成する。
    ///
    /// # Safety
    ///
    /// `handle` は `CreateFileW` で開いた有効な pipe ハンドルであり、
    /// 所有権を移譲できる必要がある。
    #[cfg(test)]
    unsafe fn from_client_handle(handle: HANDLE) -> Self {
        Self {
            handle,
            role: StreamRole::Client,
            cancel: None,
        }
    }

    /// 中断イベントの生ハンドル。
    fn cancel_handle(&self) -> Option<HANDLE> {
        self.cancel.as_ref().map(|s| s.raw())
    }

    /// 期限内にバッファ全体を読み取る。EOF なら `Ok(false)`。
    fn read_exact(
        &self,
        buf: &mut [u8],
        deadline: Instant,
        operation: &'static str,
    ) -> Result<bool, PipeError> {
        let started = Instant::now();
        // SAFETY: `self.handle` は本型が所有し `Drop` でのみ閉じるため、
        // 呼び出し中は有効。中断イベントは `Arc<StopSignal>` として本型が
        // 保持しており、同じく呼び出し中は生存する。
        unsafe { win_io::read_exact_deadline(self.handle, buf, deadline, self.cancel_handle()) }
            .map_err(|e| map_io_error(e, operation, started))
    }

    /// 期限内にバッファ全体を書き込む。
    fn write_all(
        &self,
        buf: &[u8],
        deadline: Instant,
        operation: &'static str,
    ) -> Result<(), PipeError> {
        let started = Instant::now();
        // SAFETY: `self.handle` は本型が所有し `Drop` でのみ閉じるため、
        // 呼び出し中は有効。中断イベントは `Arc<StopSignal>` として本型が
        // 保持しており、同じく呼び出し中は生存する。
        unsafe { win_io::write_all_deadline(self.handle, buf, deadline, self.cancel_handle()) }
            .map_err(|e| map_io_error(e, operation, started))
    }

    /// 期限内に 1 フレームを読み取る。相手が切断していれば `Ok(None)`。
    ///
    /// フレーム長の検証と本体の組み立ては [`FrameDecoder`] に委譲する。
    /// 1 回の読み取り量はデコーダが要求する残りバイト数を上限とするため、
    /// フレーム境界を越えて先読みすることはなく、次のフレームのバイトを
    /// 抱え込む必要もない。
    ///
    /// 過大なフレーム長・長さ 0 はデコーダが本体を確保する前に拒否する。
    pub fn read_frame(&self, deadline: Instant) -> Result<Option<Vec<u8>>, PipeError> {
        let mut decoder = FrameDecoder::new();
        let mut chunk = [0u8; READ_CHUNK_SIZE];
        loop {
            if let Some(frame) = decoder.take_frame() {
                return Ok(Some(frame));
            }
            let operation = match decoder.state() {
                DecoderState::ReadingLength => "フレーム長の受信",
                DecoderState::ReadingBody { .. } => "フレーム本体の受信",
            };
            let take = decoder.bytes_needed().min(chunk.len());
            if !self.read_exact(&mut chunk[..take], deadline, operation)? {
                // フレーム境界での切断は接続終了、フレーム途中の切断はエラー。
                return match decoder.end() {
                    Ok(()) => Ok(None),
                    Err(_) => Err(PipeError::UnexpectedEof { operation }),
                };
            }
            decoder.feed(&chunk[..take])?;
        }
    }

    /// 期限内に 1 フレームを書き込む。
    pub fn write_frame(&self, body: &[u8], deadline: Instant) -> Result<(), PipeError> {
        let frame = encode_frame(body)?;
        self.write_all(&frame, deadline, "フレームの送信")
    }
}

/// `win_io` のエラーを操作名と待機時間つきの `PipeError` へ変換する。
fn map_io_error(error: IoError, operation: &'static str, started: Instant) -> PipeError {
    match error {
        IoError::TimedOut => PipeError::TimedOut {
            operation,
            waited_ms: started.elapsed().as_millis(),
        },
        IoError::Cancelled => PipeError::Cancelled { operation },
        IoError::Os(source) if source.kind() == std::io::ErrorKind::UnexpectedEof => {
            PipeError::UnexpectedEof { operation }
        }
        IoError::Os(source) => PipeError::Io { operation, source },
    }
}

/// 接続受理ループへの停止通知。
///
/// 生ハンドル値を共有せず、Windows イベントオブジェクトのシグナルのみで
/// 停止を伝える。待機中の overlapped I/O もこのイベントで中断できる。
///
/// **接続受理ループの外からも観測される。** 要求処理は接続を受けるスレッド上で
/// 同期実行されるため、長く待つ処理はこの合図を見て早く抜ける必要がある。
/// 別の合図を用意すると、片方が立ってからもう片方が立つまでの間その処理が
/// 居座り、停止手順は待ちきれずにスレッドを切り離すことになる。合図を立てる
/// 手段は公開せず、観測だけを外から行えるようにしてある。
pub struct StopSignal {
    event: EventHandle,
}

impl StopSignal {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            event: EventHandle::new().context("停止イベントの作成に失敗しました")?,
        })
    }

    fn raw(&self) -> HANDLE {
        self.event.raw()
    }

    fn signal(&self) -> std::io::Result<()> {
        self.event.signal()
    }

    /// 既にシグナル済みかを待機なしで確認する。
    pub(crate) fn is_signaled(&self) -> bool {
        matches!(self.event.wait(0), WaitOutcome::Signaled(_))
    }

    /// 指定時間だけシグナルを待つ。シグナルされたら true。
    fn wait_for(&self, timeout: Duration) -> bool {
        let ms = timeout
            .as_millis()
            .min(u128::from(win_io::WAIT_INFINITE - 1)) as u32;
        matches!(self.event.wait(ms), WaitOutcome::Signaled(_))
    }
}

/// 起動済み accept スレッドの終了検知に必要な組。
struct ServerThread {
    join_handle: JoinHandle<()>,
    /// スレッド終了時に送信端が drop され `Disconnected` になる受信端。
    finished: Receiver<()>,
}

/// named pipe server の制御ハンドル。
pub struct PipeServer {
    stop_signal: Arc<StopSignal>,
    thread: Mutex<Option<ServerThread>>,
    stopped: AtomicBool,
}

impl PipeServer {
    /// 指定したライフサイクルに紐づく named pipe server を起動する。
    ///
    /// 戻り値は制御ハンドルであり、accept スレッドとは共有しない。スレッドへ
    /// 渡すのは停止イベントとライフサイクル・読み取り口・編集口のみで、スレッドから
    /// 制御ハンドルを触る経路は存在しない。
    ///
    /// 停止イベントを引数に取るのは、**接続受理ループの外にもこの合図を観測する
    /// 処理があるため**である。観測する側は起動より前に組み立てられるため、
    /// 合図の作成をここへ閉じ込めると渡す先が無くなる。
    pub fn start(
        lifecycle: Arc<Lifecycle>,
        read_adapter: Arc<dyn ReadAdapter>,
        edit_adapter: Arc<dyn EditAdapter>,
        stop_signal: Arc<StopSignal>,
    ) -> Result<Self> {
        // 送信は行わない。スレッド終了時に `tx` が drop され、受信側が
        // `Disconnected` を得ることでスレッド終了を検知する。
        let (tx, rx) = channel::<()>();

        let stop_for_thread = Arc::clone(&stop_signal);
        let join_handle = spawn(move || {
            // 不変条件: このスレッドは plugin singleton（`with_instance_mut`
            // 経由の API）へ一切触れてはならない。ホストの `UninitializePlugin`
            // は singleton の write lock を保持したまま plugin を Drop し、
            // その Drop がこのスレッドを join する。ここで同じ write lock を
            // 要求すると確実にデッドロックする。
            let _finished = tx;
            if let Err(e) = accept_loop(lifecycle, read_adapter, edit_adapter, stop_for_thread) {
                tracing::error!("named pipe server ループが異常終了しました: {e:?}");
            }
        });

        Ok(Self {
            stop_signal,
            thread: Mutex::new(Some(ServerThread {
                join_handle,
                finished: rx,
            })),
            stopped: AtomicBool::new(false),
        })
    }

    /// サーバーを停止する。
    ///
    /// 冪等であり、二度目以降の呼び出しは何もしない。タイムアウト内に
    /// スレッドが終了しなければ切り離してログ化する。
    pub fn stop(&self, timeout: Duration) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Err(e) = self.stop_signal.signal() {
            tracing::error!("停止イベントのシグナルに失敗しました: {e}");
        }

        let thread = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(thread) = thread else {
            return;
        };

        let ended = matches!(
            thread.finished.recv_timeout(timeout),
            Err(RecvTimeoutError::Disconnected)
        );
        if ended {
            if thread.join_handle.join().is_err() {
                tracing::error!("named pipe server スレッドが panic で終了しました");
            }
        } else {
            tracing::error!(
                "named pipe server スレッドの停止が {}ms でタイムアウトしました",
                timeout.as_millis()
            );
        }
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        // 明示的な `stop` が呼ばれなかった場合の保険。`stop` は冪等。
        self.stop(STOP_TIMEOUT);
    }
}

/// 所有権つきの pipe ハンドル。早期 return でも確実に閉じる。
struct OwnedPipeHandle(HANDLE);

impl OwnedPipeHandle {
    fn raw(&self) -> HANDLE {
        self.0
    }

    /// 所有権を呼び出し元へ移す。以後 `Drop` では閉じない。
    fn into_raw(self) -> HANDLE {
        let handle = self.0;
        std::mem::forget(self);
        handle
    }
}

impl Drop for OwnedPipeHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` は本型が所有する有効なハンドル。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// 接続受理ループ。
///
/// pipe 接続ハンドルの所有権はこのループのみが持つ。`PipeServer::stop` は
/// 停止イベントを立てるだけでハンドルに触れないため、二重クローズと
/// クローズ済みハンドルの使用が構造的に発生しない。
fn accept_loop(
    lifecycle: Arc<Lifecycle>,
    read_adapter: Arc<dyn ReadAdapter>,
    edit_adapter: Arc<dyn EditAdapter>,
    stop: Arc<StopSignal>,
) -> Result<()> {
    let pipe_name = pipe_name_for(&lifecycle.instance_id());
    let sa = ProtectedSecurityAttributes::new().context("pipe 用 DACL の作成に失敗しました")?;
    let name_wide = to_wide(&pipe_name);
    let mut backoff = RetryBackoff::default();

    loop {
        if stop.is_signaled() || lifecycle.state() == aviutl2_mcp_core::state::InstanceState::Gone {
            break;
        }

        // SAFETY: `name_wide` は NUL 終端の UTF-16 文字列、`sa` は生存中の
        // `SECURITY_ATTRIBUTES` を指す。
        let pipe_handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(name_wide.as_ptr()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                PIPE_BUFFER_SIZE,
                PIPE_BUFFER_SIZE,
                0,
                Some(sa.as_ptr()),
            )
        };
        if pipe_handle.is_invalid() {
            // 直前の API 呼び出しは `CreateNamedPipeW` のみであり、
            // last error は当該失敗のもの。pipe 名には完全な instance_id が
            // 含まれるため、失敗の説明には載せない。
            let reason = windows::core::Error::from_thread();
            return Err(anyhow::anyhow!("named pipe の作成に失敗しました: {reason}"));
        }
        let pipe = OwnedPipeHandle(pipe_handle);

        match await_connection(&pipe, &stop)? {
            Connection::Established => {
                backoff.reset();
                // SAFETY: `pipe` から所有権を移譲した接続確立済みハンドル。
                let stream =
                    unsafe { PipeStream::from_server_handle(pipe.into_raw(), Some(stop.clone())) };
                if stop.is_signaled() {
                    break;
                }
                session::handle_connection(
                    stream,
                    lifecycle.clone(),
                    read_adapter.clone(),
                    edit_adapter.clone(),
                );
            }
            Connection::Stopped => break,
            Connection::Retry { reason } => {
                drop(pipe);
                backoff.report(&reason);
                if stop.wait_for(backoff.next_interval()) {
                    break;
                }
            }
        }
    }

    Ok(())
}

/// 待受再確立の失敗が続いた場合の間隔とログ量を抑える。
///
/// 恒久的な失敗でも `Connection::Retry` を返し続けるため、間隔を空けずに
/// 回すとログを埋め尽くす。初回のみ warn で通知し、以降は debug へ落として
/// 間隔を指数的に伸ばす。
struct RetryBackoff {
    failures: u32,
    interval: Duration,
}

impl Default for RetryBackoff {
    fn default() -> Self {
        Self {
            failures: 0,
            interval: ACCEPT_RETRY_MIN_INTERVAL,
        }
    }
}

impl RetryBackoff {
    fn reset(&mut self) {
        self.failures = 0;
        self.interval = ACCEPT_RETRY_MIN_INTERVAL;
    }

    fn report(&mut self, reason: &str) {
        self.failures += 1;
        if self.failures == 1 {
            tracing::warn!("待受の再確立に失敗しました: {reason}");
        } else {
            tracing::debug!(
                "待受の再確立に失敗しました（連続 {} 回目）: {reason}",
                self.failures
            );
        }
    }

    fn next_interval(&mut self) -> Duration {
        let current = self.interval;
        self.interval = (self.interval * 2).min(ACCEPT_RETRY_MAX_INTERVAL);
        current
    }
}

/// 接続待ちの結果。
enum Connection {
    /// クライアントとの接続が確立した。
    Established,
    /// 停止要求により待受を終了する。
    Stopped,
    /// 待受の確立に失敗した。作り直して再試行する。
    Retry {
        /// ログに残す失敗理由。
        reason: String,
    },
}

/// クライアントの接続を待つ。停止イベントがシグナルされたら待機を打ち切る。
fn await_connection(pipe: &OwnedPipeHandle, stop: &StopSignal) -> Result<Connection> {
    // SAFETY: `pipe` は呼び出し元が所有しており、`op` は本関数の内側で
    // drop されるため、`op` の `Drop` が動く時点で必ず生存している。
    let mut op = unsafe { OverlappedOp::new(pipe.raw()) }
        .context("接続待ち用 OVERLAPPED の作成に失敗しました")?;
    let result = op.issue(|overlapped| {
        // SAFETY: `pipe` は生存中の overlapped named pipe、`overlapped` は
        // 接続完了まで生存する `op` の内部を指す。
        unsafe { ConnectNamedPipe(pipe.raw(), Some(overlapped)) }
    });

    // `GetLastError` の遅延読み取りは他の API 呼び出しで上書きされ得るため、
    // 戻り値そのものが持つエラーコードで分岐する。
    match result {
        Ok(()) => Ok(Connection::Established),
        // `CreateNamedPipeW` と `ConnectNamedPipe` の間にクライアントが
        // 接続した正常ケース。
        Err(e) if e.code() == ERROR_PIPE_CONNECTED.into() => Ok(Connection::Established),
        Err(e) if e.code() == ERROR_IO_PENDING.into() => {
            match win_io::wait_any(&[op.event_handle(), stop.raw()], win_io::WAIT_INFINITE) {
                WaitOutcome::Signaled(0) => match op.result(false) {
                    Ok(_) => Ok(Connection::Established),
                    Err(e) => Ok(Connection::Retry {
                        reason: format!("接続完了の確認に失敗しました: {e}"),
                    }),
                },
                WaitOutcome::Signaled(_) => {
                    op.cancel_and_drain();
                    Ok(Connection::Stopped)
                }
                // 無期限待機を指定しているため到達しない。待機結果の分類が
                // 変わっても待受を落とさないための防御的な分岐。
                WaitOutcome::TimedOut => Ok(Connection::Retry {
                    reason: "接続待ちが無期限待機で期限切れになりました".to_string(),
                }),
                WaitOutcome::Failed(e) => Err(anyhow::anyhow!("接続待ちの待機に失敗しました: {e}")),
            }
        }
        // 待受インスタンスは毎回作り直すため、失敗が一過性であれば再試行で
        // 回復する。恒久的な失敗でも間隔を空けて再試行するため CPU を
        // 占有しない。
        Err(e) => Ok(Connection::Retry {
            reason: format!("接続受理に失敗しました: {e}"),
        }),
    }
}

/// UTF-16 文字列（NUL 終端）を作成する。
fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::sdk_edit_adapter;
    use crate::project::ProjectState;
    use crate::test_support::with_silent_panic_hook;
    use aviutl2_mcp_core::{AuthSecret, InstanceId, InstanceState, ProtocolVersion};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, OPEN_EXISTING,
    };

    /// テストクライアントが 1 回の読み書きに許す上限。
    const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(10);

    fn temp_lifecycle() -> (Arc<Lifecycle>, std::path::PathBuf) {
        let (lifecycle, dir) = temp_lifecycle_starting();
        lifecycle.transition_to(InstanceState::Ready).unwrap();
        (lifecycle, dir)
    }

    /// 起動処理中のまま、`ready` へ遷移させないライフサイクルを作る。
    fn temp_lifecycle_starting() -> (Arc<Lifecycle>, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("aviutl2-mcp-pipe-test-{}", InstanceId::new_v4()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let writer = crate::registry::RegistryWriter::for_dir(dir.clone());
        let id = InstanceId::new_v4();
        let lifecycle = Lifecycle::new(
            id,
            AuthSecret::generate(),
            std::process::id(),
            "2026-01-01T00:00:00.0000000Z".to_string(),
            Some("0x0".to_string()),
            "2026-01-01T00:00:00.0000000Z".to_string(),
            writer,
        )
        .unwrap();
        (Arc::new(lifecycle), dir)
    }

    /// テスト用の読み取り口を添えて server を起動する。
    ///
    /// 編集ハンドルは初期化されないため、読み取り口は SDK を呼ばない。
    fn start_server(lifecycle: &Arc<Lifecycle>) -> PipeServer {
        let project = Arc::new(ProjectState::new());
        let read_adapter = crate::read::sdk_read_adapter(project.clone());
        PipeServer::start(
            lifecycle.clone(),
            read_adapter,
            sdk_edit_adapter(project),
            new_stop_signal(),
        )
        .unwrap()
    }

    /// 接続受理ループを止める合図を 1 つ作る。
    fn new_stop_signal() -> Arc<StopSignal> {
        Arc::new(StopSignal::new().unwrap())
    }

    /// 指定した読み取り口を添えて server を起動する。
    ///
    /// 編集口は SDK 版を用いる。編集ハンドルが初期化されないため、編集要求は
    /// SDK を呼ばずに受付できない状態として失敗する。
    fn start_server_with(
        lifecycle: &Arc<Lifecycle>,
        read_adapter: Arc<dyn ReadAdapter>,
    ) -> PipeServer {
        let edit_adapter = sdk_edit_adapter(Arc::new(ProjectState::new()));
        PipeServer::start(
            lifecycle.clone(),
            read_adapter,
            edit_adapter,
            new_stop_signal(),
        )
        .unwrap()
    }

    /// 応答が要求へ届くことだけを確かめるための読み取り口。
    ///
    /// 用いるのは編集情報の取得だけである。他の読み取りは呼ばれないが、呼ばれても
    /// 接続が落ちないよう受付前と同じ失敗を返す。
    struct StubReadAdapter;

    /// 実在の値と取り違えないよう、応答に現れる値へ目印を付ける。
    const STUB_SCENE_ID: i32 = 3;
    const STUB_REVISION: u64 = 11;
    const STUB_EPOCH: &str = "0c9b1f2e-6d3a-4c85-9f10-2b7c4d5e6f70";

    impl ReadAdapter for StubReadAdapter {
        fn project_status(&self) -> crate::read::ProjectStatus {
            crate::read::ProjectStatus {
                epoch: STUB_EPOCH.to_string(),
                revision: STUB_REVISION,
                modified: false,
            }
        }

        fn get_edit_info(&self) -> Result<aviutl2_mcp_core::EditInfo, crate::read::ReadError> {
            Ok(aviutl2_mcp_core::EditInfo {
                scene: stub_scene(),
                cursor: aviutl2_mcp_core::Cursor { frame: 0, layer: 0 },
                extent: aviutl2_mcp_core::Extent {
                    frame_max: 100,
                    layer_max: 1,
                },
                display: aviutl2_mcp_core::DisplayRange {
                    frame_start: 0,
                    layer_start: 0,
                    frame_num: 100,
                    layer_num: 2,
                },
                selected_range: None,
                grid_bpm: Vec::new(),
                project_epoch: STUB_EPOCH.to_string(),
                project_revision: STUB_REVISION,
            })
        }

        fn get_current_scene(
            &self,
        ) -> Result<(aviutl2_mcp_core::SceneInfo, u64), crate::read::ReadError> {
            Err(crate::read::ReadError::NotReady)
        }

        fn list_layers(
            &self,
            _expected_scene_id: i32,
        ) -> Result<crate::read::Snapshot<aviutl2_mcp_core::LayerInfo>, crate::read::ReadError>
        {
            Err(crate::read::ReadError::NotReady)
        }

        fn list_objects(
            &self,
            _expected_scene_id: i32,
            _filter: Option<&aviutl2_mcp_core::ObjectFilter>,
            _page: &aviutl2_mcp_core::PageRequest,
        ) -> Result<
            Result<crate::read::Page<aviutl2_mcp_core::ObjectSummary>, aviutl2_mcp_core::PageError>,
            crate::read::ReadError,
        > {
            Err(crate::read::ReadError::NotReady)
        }

        fn get_object(
            &self,
            _selector: &aviutl2_mcp_core::ObjectSelector,
        ) -> Result<aviutl2_mcp_core::ObjectDetail, crate::read::ReadError> {
            Err(crate::read::ReadError::NotReady)
        }

        fn list_available_effects(
            &self,
            _effect_type: Option<&aviutl2_mcp_core::EffectType>,
        ) -> Result<crate::read::Snapshot<aviutl2_mcp_core::AvailableEffect>, crate::read::ReadError>
        {
            Err(crate::read::ReadError::NotReady)
        }
    }

    fn stub_scene() -> aviutl2_mcp_core::SceneInfo {
        aviutl2_mcp_core::SceneInfo {
            id: STUB_SCENE_ID,
            name: Some("Scene 1".to_string()),
            width: 1920,
            height: 1080,
            fps: aviutl2_mcp_core::FiniteF64::try_new(60.0),
            fps_rate: 60,
            fps_scale: 1,
            sample_rate: 48000,
        }
    }

    /// 参照区間の内側で panic するホスト。
    ///
    /// panic を応答へ変換するのは読み取り口の責務であり、要求処理側には捕捉層が
    /// 無い。層を繋いだ経路を確かめるため、実際の読み取り口を SDK の代わりに
    /// このホストへ被せる。
    struct PanickingReadHost;

    impl crate::read::host::ReadHost for PanickingReadHost {
        fn is_ready(&self) -> bool {
            true
        }

        fn edit_state(&self) -> Result<crate::read::EditState, crate::read::ReadError> {
            Ok(crate::read::EditState::Edit)
        }

        fn edit_info(&self) -> Result<crate::read::host::HostEditInfo, crate::read::ReadError> {
            Ok(crate::read::host::HostEditInfo {
                scene_id: STUB_SCENE_ID,
                width: 1920,
                height: 1080,
                fps_rate: 60,
                fps_scale: 1,
                sample_rate: 48000,
                cursor_frame: 0,
                cursor_layer: 0,
                frame_max: 100,
                layer_max: 0,
                display_frame_start: 0,
                display_layer_start: 0,
                display_frame_num: 100,
                display_layer_num: 1,
                select_range_start: None,
                select_range_end: None,
            })
        }

        fn effect_catalog(
            &self,
        ) -> Result<Vec<aviutl2_mcp_core::AvailableEffect>, crate::read::ReadError> {
            Ok(Vec::new())
        }

        fn enter_read_section<T, F>(&self, f: F) -> Result<T, crate::read::ReadError>
        where
            T: Send + 'static,
            F: FnOnce(&dyn crate::read::host::SceneReader) -> T + Send,
        {
            Ok(f(&PanickingScene))
        }
    }

    /// 参照区間の内側で必ず panic するプロジェクトデータ。
    struct PanickingScene;

    impl crate::read::host::SceneReader for PanickingScene {
        fn scene_name(&self) -> Option<String> {
            panic!("参照区間の内側で panic させます")
        }

        fn grid_bpm(&self) -> Result<Vec<aviutl2_mcp_core::FiniteF64>, crate::read::ReadError> {
            Ok(Vec::new())
        }

        fn layer(
            &self,
            _layer: usize,
        ) -> Result<crate::read::host::HostLayer, crate::read::ReadError> {
            panic!("参照区間の内側で panic させます")
        }

        fn layer_locked(&self, _layer: usize) -> Result<bool, crate::read::ReadError> {
            panic!("参照区間の内側で panic させます")
        }

        fn object_count(&self, _layer: usize) -> Result<usize, crate::read::ReadError> {
            panic!("参照区間の内側で panic させます")
        }

        fn object_placements(
            &self,
            _layer: usize,
        ) -> Result<Vec<crate::read::host::HostObjectPlacement>, crate::read::ReadError> {
            panic!("参照区間の内側で panic させます")
        }

        fn object_identity(
            &self,
            _layer: usize,
            _frame_start: usize,
        ) -> Result<crate::read::host::HostObject, crate::read::ReadError> {
            panic!("参照区間の内側で panic させます")
        }

        fn object_detail(
            &self,
            _layer: usize,
            _frame_start: usize,
        ) -> Result<crate::read::host::HostObjectDetail, crate::read::ReadError> {
            panic!("参照区間の内側で panic させます")
        }
    }

    /// 参照区間で panic する読み取り口を作る。
    fn panicking_read_adapter() -> Arc<dyn ReadAdapter> {
        Arc::new(crate::read::HostReadAdapter::new(
            PanickingReadHost,
            Arc::new(ProjectState::new()),
        ))
    }

    fn cleanup(dir: std::path::PathBuf) {
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn connect_client(pipe_name: &str) -> PipeStream {
        connect_client_within(pipe_name, Duration::from_secs(5))
    }

    /// 指定した猶予内で pipe への接続を繰り返し試みる。
    ///
    /// 待受インスタンスは 1 本のため、他の接続が処理中の間は
    /// `ERROR_PIPE_BUSY` で失敗する。待受が再確立されるまで再試行する。
    fn connect_client_within(pipe_name: &str, budget: Duration) -> PipeStream {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = std::ffi::OsStr::new(pipe_name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let start = Instant::now();
        loop {
            // SAFETY: `wide` は NUL 終端の UTF-16 文字列。
            // overlapped I/O を使うため `FILE_FLAG_OVERLAPPED` を指定する。
            let result = unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
                    windows::Win32::Storage::FileSystem::FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    None,
                )
            };
            match result {
                Ok(handle) => return unsafe { PipeStream::from_client_handle(handle) },
                Err(e) => {
                    if start.elapsed() > budget {
                        panic!(
                            "pipe への接続を {}ms 待って諦めました: {e}",
                            start.elapsed().as_millis()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    /// 期限を切ってフレームを送信する。失敗時は待った時間つきで panic する。
    fn send(client: &PipeStream, body: &[u8], stage: &str) {
        let started = Instant::now();
        if let Err(e) = client.write_frame(body, started + CLIENT_IO_TIMEOUT) {
            panic!(
                "{stage}の送信に失敗しました（{}ms 経過）: {e}",
                started.elapsed().as_millis()
            );
        }
    }

    /// 1 フレームを 4 回の書き込みに分割して送信する。
    ///
    /// 長さの前半・後半・本体の前半・後半に分けるため、受信側では長さと本体の
    /// 双方が複数回の読み取りに跨る。
    fn send_split(client: &PipeStream, body: &[u8], stage: &str) {
        let frame = encode_frame(body).unwrap();
        let body_middle = 4 + (frame.len() - 4) / 2;
        let parts = [
            &frame[..2],
            &frame[2..4],
            &frame[4..body_middle],
            &frame[body_middle..],
        ];
        for part in parts {
            let started = Instant::now();
            if let Err(e) = client.write_all(part, started + CLIENT_IO_TIMEOUT, "分割送信") {
                panic!("{stage}の分割送信に失敗しました: {e}");
            }
            // 受信側の 1 回の読み取りに複数の断片がまとまらないよう間隔を空ける。
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// 複数フレームを 1 回の書き込みでまとめて送信する。
    fn send_batched(client: &PipeStream, bodies: &[&[u8]], stage: &str) {
        let mut batched = Vec::new();
        for body in bodies {
            batched.extend_from_slice(&encode_frame(body).unwrap());
        }
        let started = Instant::now();
        if let Err(e) = client.write_all(&batched, started + CLIENT_IO_TIMEOUT, "まとめ送信") {
            panic!("{stage}のまとめ送信に失敗しました: {e}");
        }
    }

    /// 期限を切ってフレームを受信する。無期限には待たない。
    fn recv(client: &PipeStream, stage: &str) -> Option<Vec<u8>> {
        let started = Instant::now();
        match client.read_frame(started + CLIENT_IO_TIMEOUT) {
            Ok(body) => body,
            Err(PipeError::TimedOut { waited_ms, .. }) => {
                panic!("{stage}の受信を {waited_ms}ms 待って諦めました")
            }
            Err(e) => panic!(
                "{stage}の受信に失敗しました（{}ms 経過）: {e}",
                started.elapsed().as_millis()
            ),
        }
    }

    /// 期限を切って受信を試み、切断された場合を許容する。
    ///
    /// 切断は `Ok(None)`（フレーム先頭での EOF）か `UnexpectedEof`
    /// （フレーム途中での EOF）としてのみ許容する。ここで `PipeError::Io` を
    /// 許容すると、EOF 判定の取り違えが切断として通ってしまう。
    fn recv_or_disconnected(client: &PipeStream, stage: &str) -> Option<Vec<u8>> {
        match client.read_frame(Instant::now() + CLIENT_IO_TIMEOUT) {
            Ok(body) => body,
            Err(PipeError::TimedOut { waited_ms, .. }) => {
                panic!("{stage}を {waited_ms}ms 待って諦めました")
            }
            Err(PipeError::UnexpectedEof { .. }) => None,
            Err(e) => panic!("{stage}で想定外のエラーが発生しました: {e}"),
        }
    }

    fn make_hello(instance_id: InstanceId, client_nonce: &aviutl2_mcp_core::Nonce) -> Vec<u8> {
        make_hello_with_version(ProtocolVersion::CURRENT, instance_id, client_nonce)
    }

    fn make_hello_with_version(
        version: ProtocolVersion,
        instance_id: InstanceId,
        client_nonce: &aviutl2_mcp_core::Nonce,
    ) -> Vec<u8> {
        let hello = aviutl2_mcp_core::ClientHello {
            protocol_version: version,
            instance_id,
            client_nonce: client_nonce.clone(),
        };
        serde_json::to_vec(&hello).unwrap()
    }

    /// 現行版と一致しないプロトコルバージョン。
    fn mismatched_versions() -> [ProtocolVersion; 2] {
        let current = ProtocolVersion::CURRENT;
        [
            ProtocolVersion {
                major: current.major + 1,
                minor: current.minor,
            },
            ProtocolVersion {
                major: current.major,
                minor: current.minor + 1,
            },
        ]
    }

    fn make_auth(
        auth_secret: &[u8; 32],
        server_nonce: &aviutl2_mcp_core::Nonce,
        client_nonce: &aviutl2_mcp_core::Nonce,
    ) -> Vec<u8> {
        let mac = aviutl2_mcp_core::compute_client_mac(auth_secret, server_nonce, client_nonce);
        let auth = aviutl2_mcp_core::ClientAuth { client_mac: mac };
        serde_json::to_vec(&auth).unwrap()
    }

    fn make_ping(
        version: ProtocolVersion,
        request_id: aviutl2_mcp_core::RequestId,
        instance_id: InstanceId,
    ) -> Vec<u8> {
        serde_json::to_vec(&aviutl2_mcp_core::RequestEnvelope::ping(
            version,
            request_id,
            instance_id,
        ))
        .unwrap()
    }

    /// 要求フレームを組み立てる。`deadline_unix_ms` は指定しなければ無期限。
    fn make_request(
        version: ProtocolVersion,
        request_id: aviutl2_mcp_core::RequestId,
        instance_id: InstanceId,
        operation: &str,
        deadline_unix_ms: Option<u64>,
    ) -> Vec<u8> {
        serde_json::to_vec(&aviutl2_mcp_core::RequestEnvelope {
            kind: aviutl2_mcp_core::RequestKind::Request,
            protocol_version: version,
            request_id,
            instance_id,
            deadline_unix_ms,
            operation: operation.to_string(),
            params: serde_json::json!({}),
        })
        .unwrap()
    }

    /// handshake を完走し、採用バージョンを返す。
    fn complete_handshake(
        client: &PipeStream,
        id: InstanceId,
        secret: &[u8; 32],
    ) -> ProtocolVersion {
        let client_nonce = aviutl2_mcp_core::Nonce::generate();
        send(client, &make_hello(id, &client_nonce), "ClientHello");

        let server_auth_body = recv(client, "ServerAuth").expect("ServerAuth が受信できません");
        let server_auth: aviutl2_mcp_core::ServerAuth =
            serde_json::from_slice(&server_auth_body).unwrap();
        assert_eq!(server_auth.instance_id, id);
        assert_eq!(server_auth.protocol_version, ProtocolVersion::CURRENT);

        let server_mac = aviutl2_mcp_core::compute_server_mac(
            secret,
            &client_nonce,
            &server_auth.server_nonce,
            &id,
            &server_auth.protocol_version,
        );
        assert_eq!(server_mac.as_bytes(), server_auth.server_mac.as_bytes());

        send(
            client,
            &make_auth(secret, &server_auth.server_nonce, &client_nonce),
            "ClientAuth",
        );
        server_auth.protocol_version
    }

    /// ping を 1 往復し、応答内容を検証する。
    fn exchange_ping(client: &PipeStream, id: InstanceId, version: ProtocolVersion) {
        let request_id = aviutl2_mcp_core::RequestId::new();
        send(client, &make_ping(version, request_id, id), "ping 要求");

        let response_body = recv(client, "ping 応答").expect("ping 応答が受信できません");
        let response: aviutl2_mcp_core::ResponseEnvelope =
            serde_json::from_slice(&response_body).unwrap();
        assert_eq!(response.request_id, request_id);
        assert_eq!(response.instance_id, id);
        match response.result {
            aviutl2_mcp_core::ResponseResult::Ok { result } => {
                assert_eq!(result["state"], "ready");
                assert_eq!(result["instance_id"], serde_json::to_value(id).unwrap());
            }
            aviutl2_mcp_core::ResponseResult::Err { error } => {
                panic!("ping がエラー応答になりました: {error:?}")
            }
        }
    }

    #[test]
    fn handshake_and_ping() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server(&lifecycle);
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let version = complete_handshake(&client, id, &secret);
        exchange_ping(&client, id, version);

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    /// 期限を指定せずに読み取り要求を 1 往復する。
    fn exchange_read(
        client: &PipeStream,
        id: InstanceId,
        version: ProtocolVersion,
        operation: &str,
    ) -> aviutl2_mcp_core::ResponseEnvelope {
        exchange_read_within(client, id, version, operation, None)
    }

    /// 期限つきで読み取り要求を 1 往復し、応答 Envelope を返す。
    fn exchange_read_within(
        client: &PipeStream,
        id: InstanceId,
        version: ProtocolVersion,
        operation: &str,
        deadline_unix_ms: Option<u64>,
    ) -> aviutl2_mcp_core::ResponseEnvelope {
        let request_id = aviutl2_mcp_core::RequestId::new();
        send(
            client,
            &make_request(version, request_id, id, operation, deadline_unix_ms),
            operation,
        );

        let body =
            recv(client, operation).unwrap_or_else(|| panic!("{operation} の応答がありません"));
        let response: aviutl2_mcp_core::ResponseEnvelope = serde_json::from_slice(&body).unwrap();
        assert_eq!(response.request_id, request_id);
        assert_eq!(response.instance_id, id);
        response
    }

    /// 読み取りの結果が要求元まで届くことを確かめる。
    ///
    /// 判定そのものは要求処理側の単体テストで確かめている。ここで確かめるのは、
    /// その結果を成功応答として送り出す経路が繋がっていることである。
    #[test]
    fn read_request_receives_its_result() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server_with(&lifecycle, Arc::new(StubReadAdapter));
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let version = complete_handshake(&client, id, &secret);

        match exchange_read(&client, id, version, "get_edit_info").result {
            aviutl2_mcp_core::ResponseResult::Ok { result } => {
                assert_eq!(result["scene"]["id"], STUB_SCENE_ID);
                assert_eq!(result["project_revision"], STUB_REVISION);
            }
            aviutl2_mcp_core::ResponseResult::Err { error } => {
                panic!("読み取りがエラー応答になりました: {error:?}")
            }
        }

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    /// 未対応の operation が切断ではなくエラー応答として返ることを確かめる。
    #[test]
    fn unknown_operation_receives_an_error_response() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server_with(&lifecycle, Arc::new(StubReadAdapter));
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let version = complete_handshake(&client, id, &secret);

        match exchange_read(&client, id, version, "apply_batch").result {
            aviutl2_mcp_core::ResponseResult::Err { error } => {
                assert_eq!(
                    error.code,
                    aviutl2_mcp_core::ErrorCode::UnsupportedOperation
                );
                assert!(!error.retryable);
            }
            aviutl2_mcp_core::ResponseResult::Ok { result } => {
                panic!("未対応の operation が受理されました: {result:?}")
            }
        }

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    /// 応答からエラーを取り出す。成功応答は誤りとして落とす。
    fn expect_error(
        response: aviutl2_mcp_core::ResponseEnvelope,
        stage: &str,
    ) -> aviutl2_mcp_core::ErrorObject {
        match response.result {
            aviutl2_mcp_core::ResponseResult::Err { error } => error,
            aviutl2_mcp_core::ResponseResult::Ok { result } => {
                panic!("{stage}が成功応答になりました: {result:?}")
            }
        }
    }

    /// 読み取りの panic が切断ではなくエラー応答として要求元へ届くことを確かめる。
    ///
    /// panic の変換は読み取り口の内側で完結し、要求処理側には捕捉層が無い。
    /// 変換が失われると接続の境界まで巻き戻り、要求元は応答ではなく切断を
    /// 観測する。両者は要求元にとって全く異なるため、層を繋いだ経路で確かめる。
    #[test]
    fn panicking_read_receives_an_internal_error_without_closing_the_connection() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server_with(&lifecycle, panicking_read_adapter());
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let version = complete_handshake(&client, id, &secret);

        let error = with_silent_panic_hook(|| {
            expect_error(
                exchange_read(&client, id, version, "get_edit_info"),
                "panic した読み取り",
            )
        });
        assert_eq!(error.code, aviutl2_mcp_core::ErrorCode::InternalError);
        assert!(!error.retryable);

        // 応答を返した後も接続は生きており、続く要求を処理できる。
        exchange_ping(&client, id, version);

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    /// 起動処理中の読み取りが、再試行の案内つきで拒否されることを確かめる。
    #[test]
    fn read_while_starting_receives_host_busy_with_retry_advice() {
        let (lifecycle, dir) = temp_lifecycle_starting();
        let server = start_server_with(&lifecycle, Arc::new(StubReadAdapter));
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let version = complete_handshake(&client, id, &secret);

        let error = expect_error(
            exchange_read(&client, id, version, "get_edit_info"),
            "起動処理中の読み取り",
        );
        assert_eq!(error.code, aviutl2_mcp_core::ErrorCode::HostBusy);
        assert!(error.retryable);
        assert_eq!(error.details["retry_after_ms"], 500);

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    /// 起動処理中に拒否された読み取りが、初回のプロジェクトロード後に同じ
    /// 読み取り口で成功することを確かめる。
    #[test]
    fn read_succeeds_after_the_first_project_load() {
        let (lifecycle, dir) = temp_lifecycle_starting();
        let project_state = ProjectState::new();
        let server = start_server_with(&lifecycle, Arc::new(StubReadAdapter));
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let version = complete_handshake(&client, id, &secret);

        let error = expect_error(
            exchange_read(&client, id, version, "get_edit_info"),
            "起動処理中の読み取り",
        );
        assert_eq!(error.code, aviutl2_mcp_core::ErrorCode::HostBusy);

        crate::apply_project_load(
            Some(&lifecycle),
            Some(&project_state),
            Some(std::path::Path::new(r"C:\projects\sample.aup2")),
        );

        match exchange_read(&client, id, version, "get_edit_info").result {
            aviutl2_mcp_core::ResponseResult::Ok { result } => {
                assert_eq!(result["scene"]["id"], STUB_SCENE_ID);
            }
            aviutl2_mcp_core::ResponseResult::Err { error } => {
                panic!("プロジェクトロード後も読み取りが拒否されました: {error:?}")
            }
        }

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    /// 終了処理中は要求を読む前に接続が閉じられることを確かめる。
    ///
    /// 要求元が観測するのはエラー応答ではなく切断であり、インスタンスを
    /// 探し直す契機になる。
    #[test]
    fn draining_closes_the_connection_without_reading_a_request() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server_with(&lifecycle, Arc::new(StubReadAdapter));
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        lifecycle.transition_to(InstanceState::Draining).unwrap();

        let client = connect_client(&pipe_name_for(&id));
        complete_handshake(&client, id, &secret);

        let body = recv_or_disconnected(&client, "終了処理中の接続");
        assert!(
            body.is_none(),
            "終了処理中に応答が返されました: {body:?}（切断されるべきです）"
        );

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    /// 既に過ぎた期限を指定した要求が、実行されずに期限超過として返ることを確かめる。
    #[test]
    fn read_with_a_passed_deadline_receives_timeout() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server_with(&lifecycle, Arc::new(StubReadAdapter));
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let version = complete_handshake(&client, id, &secret);

        // 過ぎた期限は待たずに判定できる。実時間の経過を待つ必要はない。
        let passed = (chrono::Utc::now().timestamp_millis() - 1) as u64;
        let error = expect_error(
            exchange_read_within(&client, id, version, "get_edit_info", Some(passed)),
            "期限を過ぎた読み取り",
        );
        assert_eq!(error.code, aviutl2_mcp_core::ErrorCode::Timeout);
        assert!(error.retryable);

        // 期限を指定しない要求は同じ接続でそのまま成功する。
        assert!(matches!(
            exchange_read(&client, id, version, "get_edit_info").result,
            aviutl2_mcp_core::ResponseResult::Ok { .. }
        ));

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    #[test]
    fn frame_split_across_writes_is_reassembled() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server(&lifecycle);
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let client_nonce = aviutl2_mcp_core::Nonce::generate();
        // 長さ・本体の双方が複数回の読み取りに跨っても 1 フレームへ復元される。
        send_split(&client, &make_hello(id, &client_nonce), "ClientHello");

        let server_auth_body = recv(&client, "ServerAuth").expect("ServerAuth が受信できません");
        let server_auth: aviutl2_mcp_core::ServerAuth =
            serde_json::from_slice(&server_auth_body).unwrap();
        send_split(
            &client,
            &make_auth(&secret, &server_auth.server_nonce, &client_nonce),
            "ClientAuth",
        );
        exchange_ping(&client, id, server_auth.protocol_version);

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    #[test]
    fn frames_batched_in_single_write_are_processed_in_order() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server(&lifecycle);
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let client_nonce = aviutl2_mcp_core::Nonce::generate();
        send(&client, &make_hello(id, &client_nonce), "ClientHello");

        let server_auth_body = recv(&client, "ServerAuth").expect("ServerAuth が受信できません");
        let server_auth: aviutl2_mcp_core::ServerAuth =
            serde_json::from_slice(&server_auth_body).unwrap();

        // ClientAuth と ping 要求を 1 回の書き込みへ詰めて送る。受信側は
        // フレーム境界を越えて読まないため、2 フレームが順に処理される。
        let request_id = aviutl2_mcp_core::RequestId::new();
        let client_auth = make_auth(&secret, &server_auth.server_nonce, &client_nonce);
        let ping = make_ping(server_auth.protocol_version, request_id, id);
        send_batched(&client, &[&client_auth, &ping], "ClientAuth と ping 要求");

        let response_body = recv(&client, "ping 応答").expect("ping 応答が受信できません");
        let response: aviutl2_mcp_core::ResponseEnvelope =
            serde_json::from_slice(&response_body).unwrap();
        assert_eq!(response.request_id, request_id);
        assert_eq!(response.instance_id, id);
        assert!(matches!(
            response.result,
            aviutl2_mcp_core::ResponseResult::Ok { .. }
        ));

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    /// 契約を満たさないフレーム長は本体を待たずに拒否される。
    ///
    /// 過大な長さで本体の到着を待つと、その分だけ待受が占有された上で
    /// 過大なバッファが確保される。長さの検証は本体を読む前に行われる。
    fn assert_invalid_frame_length_disconnects(length: u32) {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server(&lifecycle);
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        complete_handshake(&client, id, &secret);

        // 本体を 1 バイトも送らずに長さだけを送る。
        let started = Instant::now();
        client
            .write_all(
                &length.to_le_bytes(),
                started + CLIENT_IO_TIMEOUT,
                "不正なフレーム長の送信",
            )
            .unwrap();

        let body = recv_or_disconnected(&client, "不正なフレーム長の送信後");
        assert!(
            body.is_none(),
            "フレーム長 {length} に応答が返されました: {body:?}（切断されるべきです）"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "フレーム長 {length} の拒否に {}ms かかりました",
            started.elapsed().as_millis()
        );

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    #[test]
    fn zero_frame_length_disconnects() {
        assert_invalid_frame_length_disconnects(0);
    }

    #[test]
    fn oversized_frame_length_disconnects_without_reading_body() {
        assert_invalid_frame_length_disconnects(aviutl2_mcp_core::MAX_FRAME_SIZE + 1);
    }

    #[test]
    fn listener_is_reestablished_after_rejected_frame() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server(&lifecycle);
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();
        let name = pipe_name_for(&id);

        // server 側から切断した直後の再接続では、`CreateNamedPipeW` と
        // `ConnectNamedPipe` の間にクライアントが接続する経路を通りやすい。
        // その経路でも待受が止まらないことを確かめる。
        for round in 0..3 {
            let client = connect_client_within(&name, Duration::from_secs(10));
            complete_handshake(&client, id, &secret);
            client
                .write_all(
                    &0u32.to_le_bytes(),
                    Instant::now() + CLIENT_IO_TIMEOUT,
                    "不正なフレーム長の送信",
                )
                .unwrap();
            let body = recv_or_disconnected(&client, "不正なフレーム長の送信後");
            assert!(body.is_none(), "{round} 回目に応答が返されました: {body:?}");
            drop(client);
        }

        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    /// 版が一致しない要求には、切断ではなく `protocol_mismatch` 応答が返る。
    ///
    /// 応答送信の直後にハンドルを破棄すると pipe バッファの未読データが
    /// 捨てられ、クライアントは応答を読めないまま切断だけを観測する。
    fn assert_version_mismatch_request_is_rejected(mismatched: ProtocolVersion) {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server(&lifecycle);
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let version = complete_handshake(&client, id, &secret);
        assert_eq!(version, ProtocolVersion::CURRENT);

        let request_id = aviutl2_mcp_core::RequestId::new();
        send(
            &client,
            &make_ping(mismatched, request_id, id),
            "版が一致しない要求",
        );

        let response_body =
            recv(&client, "版が一致しない応答").expect("版が一致しない要求の応答が受信できません");
        let response: aviutl2_mcp_core::ResponseEnvelope =
            serde_json::from_slice(&response_body).unwrap();
        assert_eq!(response.request_id, request_id);
        match response.result {
            aviutl2_mcp_core::ResponseResult::Err { error } => {
                assert_eq!(error.code, aviutl2_mcp_core::ErrorCode::ProtocolMismatch);
            }
            aviutl2_mcp_core::ResponseResult::Ok { result } => {
                panic!("{} の要求が受理されました: {result:?}", mismatched.as_str())
            }
        }

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    #[test]
    fn major_mismatch_request_receives_protocol_mismatch_response() {
        assert_version_mismatch_request_is_rejected(ProtocolVersion {
            major: ProtocolVersion::CURRENT.major + 1,
            minor: ProtocolVersion::CURRENT.minor,
        });
    }

    /// 版の交渉を行わないため、MINOR だけの差も要求として受理しない。
    #[test]
    fn minor_mismatch_request_receives_protocol_mismatch_response() {
        assert_version_mismatch_request_is_rejected(ProtocolVersion {
            major: ProtocolVersion::CURRENT.major,
            minor: ProtocolVersion::CURRENT.minor + 1,
        });
    }

    /// 版が一致しない ClientHello は、理由を返さずに切断される。
    #[test]
    fn handshake_with_a_mismatched_version_disconnects_without_response() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server(&lifecycle);
        let id = lifecycle.instance_id();
        let name = pipe_name_for(&id);

        for version in mismatched_versions() {
            let client = connect_client_within(&name, Duration::from_secs(10));
            let client_nonce = aviutl2_mcp_core::Nonce::generate();
            send(
                &client,
                &make_hello_with_version(version, id, &client_nonce),
                "版が一致しない ClientHello",
            );

            let body = recv_or_disconnected(&client, "版が一致しない ClientHello の送信後");
            assert!(
                body.is_none(),
                "{} を名乗る ClientHello に応答が返されました: {body:?}",
                version.as_str()
            );
            drop(client);
        }

        // 拒否した接続は待受を止めない。現行版の相手はそのまま handshake できる。
        let client = connect_client_within(&name, Duration::from_secs(10));
        let secret = *lifecycle.auth_secret().as_bytes();
        let version = complete_handshake(&client, id, &secret);
        exchange_ping(&client, id, version);

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    #[test]
    fn wrong_client_mac_disconnects_without_response() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server(&lifecycle);
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let client_nonce = aviutl2_mcp_core::Nonce::generate();
        send(&client, &make_hello(id, &client_nonce), "ClientHello");

        let server_auth_body = recv(&client, "ServerAuth").expect("ServerAuth が受信できません");
        let server_auth: aviutl2_mcp_core::ServerAuth =
            serde_json::from_slice(&server_auth_body).unwrap();

        // 改竄された client_mac を送信
        let mut wrong_secret = secret;
        wrong_secret[0] ^= 0xFF;
        send(
            &client,
            &make_auth(&wrong_secret, &server_auth.server_nonce, &client_nonce),
            "改竄した ClientAuth",
        );

        // 失敗理由を開示せず、応答を返さずに接続が切断される。
        let body = recv_or_disconnected(&client, "認証失敗後の切断");
        assert!(
            body.is_none(),
            "認証失敗時に応答が返されました: {body:?}（切断されるべきです）"
        );

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    #[test]
    fn silent_client_does_not_occupy_listener() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server(&lifecycle);
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();
        let name = pipe_name_for(&id);

        // ClientHello を送らないクライアント。待受インスタンスは 1 本のため、
        // handshake 期限が切れるまでこの接続が待受を占有する。
        let silent = connect_client(&name);

        // 期限超過で接続が破棄され待受が再確立されるので、黙ったクライアントを
        // 保持したままでも 2 本目が handshake から ping まで完走できる。
        let budget = crate::session::HANDSHAKE_TIMEOUT + Duration::from_secs(15);
        let client = connect_client_within(&name, budget);
        let version = complete_handshake(&client, id, &secret);
        exchange_ping(&client, id, version);

        drop(client);
        drop(silent);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    #[test]
    fn stop_returns_promptly_while_client_is_connected() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server(&lifecycle);
        let id = lifecycle.instance_id();

        // 何も送らないクライアントを接続したまま停止要求を出しても、
        // 待受スレッドは handshake 期限を待たずに終了する。
        let silent = connect_client(&pipe_name_for(&id));

        let started = Instant::now();
        server.stop(Duration::from_secs(10));
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "停止に {}ms かかりました",
            elapsed.as_millis()
        );

        drop(silent);
        cleanup(dir);
    }

    /// 応答が要求の期限を確実に超えるまで待つ時間。
    const SLOW_WORK: Duration = Duration::from_millis(300);

    /// 要求が指定する期限。[`SLOW_WORK`] より短く採る。
    const SLOW_DEADLINE_MS: i64 = 50;

    /// 期限を超えてから結果を返す読み取り口。
    struct SlowReadAdapter;

    impl ReadAdapter for SlowReadAdapter {
        fn project_status(&self) -> crate::read::ProjectStatus {
            StubReadAdapter.project_status()
        }

        fn get_edit_info(&self) -> Result<aviutl2_mcp_core::EditInfo, crate::read::ReadError> {
            std::thread::sleep(SLOW_WORK);
            StubReadAdapter.get_edit_info()
        }

        fn get_current_scene(
            &self,
        ) -> Result<(aviutl2_mcp_core::SceneInfo, u64), crate::read::ReadError> {
            StubReadAdapter.get_current_scene()
        }

        fn list_layers(
            &self,
            expected_scene_id: i32,
        ) -> Result<crate::read::Snapshot<aviutl2_mcp_core::LayerInfo>, crate::read::ReadError>
        {
            StubReadAdapter.list_layers(expected_scene_id)
        }

        fn list_objects(
            &self,
            expected_scene_id: i32,
            filter: Option<&aviutl2_mcp_core::ObjectFilter>,
            page: &aviutl2_mcp_core::PageRequest,
        ) -> Result<
            Result<crate::read::Page<aviutl2_mcp_core::ObjectSummary>, aviutl2_mcp_core::PageError>,
            crate::read::ReadError,
        > {
            StubReadAdapter.list_objects(expected_scene_id, filter, page)
        }

        fn get_object(
            &self,
            selector: &aviutl2_mcp_core::ObjectSelector,
        ) -> Result<aviutl2_mcp_core::ObjectDetail, crate::read::ReadError> {
            StubReadAdapter.get_object(selector)
        }

        fn list_available_effects(
            &self,
            effect_type: Option<&aviutl2_mcp_core::EffectType>,
        ) -> Result<crate::read::Snapshot<aviutl2_mcp_core::AvailableEffect>, crate::read::ReadError>
        {
            StubReadAdapter.list_available_effects(effect_type)
        }
    }

    /// 期限を超えてから結果を返す編集口。
    ///
    /// 用いるのは選択状態の変更だけである。他の編集は呼ばれないが、呼ばれても
    /// 接続が落ちないよう受付前と同じ失敗を返す。
    struct SlowEditAdapter;

    impl SlowEditAdapter {
        fn unavailable() -> crate::edit::EditError {
            crate::read::ReadError::NotReady.into()
        }
    }

    impl EditAdapter for SlowEditAdapter {
        fn create_object(
            &self,
            _: &aviutl2_mcp_core::CreateObjectParams,
        ) -> Result<aviutl2_mcp_core::EditOutcome, crate::edit::EditError> {
            Err(Self::unavailable())
        }

        fn move_object(
            &self,
            _: &aviutl2_mcp_core::MoveObjectParams,
        ) -> Result<aviutl2_mcp_core::EditOutcome, crate::edit::EditError> {
            Err(Self::unavailable())
        }

        fn delete_object(
            &self,
            _: &aviutl2_mcp_core::DeleteObjectParams,
        ) -> Result<aviutl2_mcp_core::EditOutcome, crate::edit::EditError> {
            Err(Self::unavailable())
        }

        fn set_object_name(
            &self,
            _: &aviutl2_mcp_core::SetObjectNameParams,
        ) -> Result<aviutl2_mcp_core::EditOutcome, crate::edit::EditError> {
            Err(Self::unavailable())
        }

        fn set_object_item(
            &self,
            _: &aviutl2_mcp_core::SetObjectItemParams,
        ) -> Result<aviutl2_mcp_core::EditOutcome, crate::edit::EditError> {
            Err(Self::unavailable())
        }

        fn add_effect(
            &self,
            _: &aviutl2_mcp_core::AddEffectParams,
        ) -> Result<aviutl2_mcp_core::EditOutcome, crate::edit::EditError> {
            Err(Self::unavailable())
        }

        fn delete_effect(
            &self,
            _: &aviutl2_mcp_core::DeleteEffectParams,
        ) -> Result<aviutl2_mcp_core::EditOutcome, crate::edit::EditError> {
            Err(Self::unavailable())
        }

        fn set_effect_enabled(
            &self,
            _: &aviutl2_mcp_core::SetEffectEnabledParams,
        ) -> Result<aviutl2_mcp_core::EditOutcome, crate::edit::EditError> {
            Err(Self::unavailable())
        }

        fn set_layer_state(
            &self,
            _: &aviutl2_mcp_core::SetLayerStateParams,
        ) -> Result<aviutl2_mcp_core::LayerStateOutcome, crate::edit::EditError> {
            Err(Self::unavailable())
        }

        fn set_selection(
            &self,
            _: &aviutl2_mcp_core::SetSelectionParams,
        ) -> Result<aviutl2_mcp_core::SelectionState, crate::edit::EditError> {
            std::thread::sleep(SLOW_WORK);
            Ok(aviutl2_mcp_core::SelectionState::observed(
                STUB_EPOCH,
                STUB_REVISION,
                aviutl2_mcp_core::Cursor { frame: 0, layer: 0 },
                None,
                None,
                vec![aviutl2_mcp_core::SelectionField::Cursor],
                Vec::new(),
            ))
        }
    }

    /// 指定した読み取り口と編集口を添えて server を起動する。
    fn start_server_with_adapters(
        lifecycle: &Arc<Lifecycle>,
        read_adapter: Arc<dyn ReadAdapter>,
        edit_adapter: Arc<dyn EditAdapter>,
    ) -> PipeServer {
        PipeServer::start(
            lifecycle.clone(),
            read_adapter,
            edit_adapter,
            new_stop_signal(),
        )
        .unwrap()
    }

    /// 編集区間のクロージャの内側で panic する編集口と、その前提を作る。
    ///
    /// 差し込んだホストも返す。巻き戻しがクロージャの外へ漏れなかったことを
    /// 呼び出し側が確かめられるようにするためである。
    fn panicking_edit_adapter() -> (
        Arc<dyn EditAdapter>,
        Arc<crate::edit::fake::FakeEditHost>,
        String,
    ) {
        use crate::edit::fake::{FakeEditHost, PanicPoint};

        let project = Arc::new(ProjectState::new());
        let epoch = project.epoch();
        let mut host = FakeEditHost::new();
        host.project = Some(project.clone());
        let host = Arc::new(host);
        host.arm(|knobs| knobs.panic_at = Some(PanicPoint::InClosure));
        (
            Arc::new(crate::edit::HostEditAdapter::new(host.clone(), project)),
            host,
            epoch,
        )
    }

    /// 選択状態の変更要求を組み立てる。対象を指すセレクターを持たないため、
    /// 前提の epoch だけで組める。
    fn selection_params(epoch: &str) -> serde_json::Value {
        serde_json::json!({
            "expected_scene_id": 0,
            "cursor": { "layer": 0, "frame": 0 },
            "expected_project_epoch": epoch,
        })
    }

    /// params つきで要求を 1 往復する。
    fn exchange_with_params(
        client: &PipeStream,
        id: InstanceId,
        version: ProtocolVersion,
        operation: &str,
        params: serde_json::Value,
        deadline_unix_ms: Option<u64>,
    ) -> aviutl2_mcp_core::ResponseEnvelope {
        let request_id = aviutl2_mcp_core::RequestId::new();
        let body = serde_json::to_vec(&aviutl2_mcp_core::RequestEnvelope {
            kind: aviutl2_mcp_core::RequestKind::Request,
            protocol_version: version,
            request_id,
            instance_id: id,
            deadline_unix_ms,
            operation: operation.to_string(),
            params,
        })
        .unwrap();
        send(client, &body, operation);
        let response =
            recv(client, operation).unwrap_or_else(|| panic!("{operation} の応答がありません"));
        let response: aviutl2_mcp_core::ResponseEnvelope =
            serde_json::from_slice(&response).unwrap();
        assert_eq!(response.request_id, request_id);
        response
    }

    /// 編集区間の内側の panic が、切断ではなく応答として要求元へ届くことを確かめる。
    ///
    /// panic の変換は編集口の内側で完結し、要求処理側には捕捉層が無い。変換が
    /// 失われると接続の境界まで巻き戻り、要求元は応答ではなく切断を観測する。
    /// 両者は要求元にとって全く異なるため、層を繋いだ経路で確かめる。
    #[test]
    fn panicking_edit_receives_an_internal_error_without_closing_the_connection() {
        let (lifecycle, dir) = temp_lifecycle();
        let (edit_adapter, edit_host, epoch) = panicking_edit_adapter();
        let server =
            start_server_with_adapters(&lifecycle, Arc::new(StubReadAdapter), edit_adapter);
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let version = complete_handshake(&client, id, &secret);

        let error = with_silent_panic_hook(|| {
            expect_error(
                exchange_with_params(
                    &client,
                    id,
                    version,
                    "set_selection",
                    selection_params(&epoch),
                    None,
                ),
                "panic した編集",
            )
        });
        assert_eq!(error.code, aviutl2_mcp_core::ErrorCode::InternalError);
        assert!(!error.retryable);
        assert!(
            !edit_host
                .calls()
                .contains(&crate::edit::fake::CLOSURE_ESCAPED),
            "巻き戻しがクロージャの外へ漏れました。実機ではホストが落ちます"
        );

        // 応答を返した後も接続は生きており、続く要求を処理できる。
        exchange_ping(&client, id, version);

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    /// 期限を過ぎた読み取りの結果は破棄される。
    ///
    /// 直後の編集のテストと対にしてある。破棄の規則を取り違えると、どちらか
    /// 一方が必ず落ちる。
    #[test]
    fn a_read_that_outlives_its_deadline_is_discarded() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server_with_adapters(
            &lifecycle,
            Arc::new(SlowReadAdapter),
            Arc::new(SlowEditAdapter),
        );
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let version = complete_handshake(&client, id, &secret);

        let deadline = (chrono::Utc::now().timestamp_millis() + SLOW_DEADLINE_MS) as u64;
        let error = expect_error(
            exchange_with_params(
                &client,
                id,
                version,
                "get_edit_info",
                serde_json::json!({}),
                Some(deadline),
            ),
            "期限を過ぎた読み取り",
        );
        assert_eq!(error.code, aviutl2_mcp_core::ErrorCode::Timeout);

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    /// 期限を過ぎても編集の結果は破棄されない。
    ///
    /// 編集が完了していればプロジェクトは変わり、取り消し単位にも登録されて
    /// いる。破棄すると、適用済みの変更が要求元からは失敗として観測される。
    #[test]
    fn an_edit_that_outlives_its_deadline_still_delivers_its_result() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = start_server_with_adapters(
            &lifecycle,
            Arc::new(SlowReadAdapter),
            Arc::new(SlowEditAdapter),
        );
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let version = complete_handshake(&client, id, &secret);

        let deadline = (chrono::Utc::now().timestamp_millis() + SLOW_DEADLINE_MS) as u64;
        let response = exchange_with_params(
            &client,
            id,
            version,
            "set_selection",
            selection_params(STUB_EPOCH),
            Some(deadline),
        );
        match response.result {
            aviutl2_mcp_core::ResponseResult::Ok { result } => {
                assert_eq!(result["applied"], serde_json::json!(["cursor"]));
            }
            aviutl2_mcp_core::ResponseResult::Err { error } => {
                panic!("適用済みの編集結果が破棄されました: {error:?}")
            }
        }

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }
}
