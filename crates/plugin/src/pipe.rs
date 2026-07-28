//! Named pipe server 接続受付と byte stream 読み書き。
//!
//! duplex・byte mode・リモート接続拒否・保護 DACL で named pipe を作成し、
//! 専用スレッドで接続受理 → handshake/ping → 切断 のループを回す。
//! pipe は overlapped で作成し、読み書きには必ず期限を与える。

use crate::lifecycle::Lifecycle;
use crate::security::ProtectedSecurityAttributes;
use crate::session;
use crate::win_io::{self, EventHandle, IoError, OverlappedOp, WaitOutcome};
use anyhow::{Context, Result};
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

/// 待受の再確立に失敗した際の再試行間隔。
///
/// 一過性の失敗で待受を諦めないために再試行するが、間隔を空けずに回すと
/// 恒久的な失敗時に CPU を占有する。停止イベントを待つ形で間隔を空けることで、
/// 停止要求には即応しつつ再試行の頻度を抑える。
const ACCEPT_RETRY_INTERVAL: Duration = Duration::from_millis(200);

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
    /// フレーム長が契約を満たさない。
    #[error("無効なフレーム長です: {0}")]
    InvalidFrameLength(usize),
    /// フレームのエンコードに失敗した。
    #[error("フレームのエンコードに失敗しました: {0}")]
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
    pub fn read_frame(&self, deadline: Instant) -> Result<Option<Vec<u8>>, PipeError> {
        let mut len_bytes = [0u8; 4];
        if !self.read_exact(&mut len_bytes, deadline, "フレーム長の受信")? {
            return Ok(None);
        }
        let len = u32::from_le_bytes(len_bytes) as usize;
        if len == 0 || len > aviutl2_mcp_core::framing::MAX_FRAME_SIZE as usize {
            return Err(PipeError::InvalidFrameLength(len));
        }
        let mut body = vec![0u8; len];
        if !self.read_exact(&mut body, deadline, "フレーム本体の受信")? {
            return Err(PipeError::UnexpectedEof {
                operation: "フレーム本体の受信",
            });
        }
        Ok(Some(body))
    }

    /// 期限内に 1 フレームを書き込む。
    pub fn write_frame(&self, body: &[u8], deadline: Instant) -> Result<(), PipeError> {
        let frame = aviutl2_mcp_core::framing::encode_frame(body)?;
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
pub(crate) struct StopSignal {
    event: EventHandle,
}

impl StopSignal {
    fn new() -> Result<Self> {
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
    fn is_signaled(&self) -> bool {
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

/// named pipe server の制御ハンドル。
pub struct PipeServer {
    stop_signal: Arc<StopSignal>,
    join_handle: Mutex<Option<JoinHandle<()>>>,
    finished: Mutex<Option<Receiver<()>>>,
    stopped: AtomicBool,
}

impl PipeServer {
    /// 指定したライフサイクルに紐づく named pipe server を起動する。
    pub fn start(lifecycle: Arc<Lifecycle>) -> Result<Arc<Self>> {
        let stop_signal = Arc::new(StopSignal::new()?);
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
            if let Err(e) = accept_loop(lifecycle, stop_for_thread) {
                tracing::error!("named pipe server ループが異常終了しました: {e:?}");
            }
        });

        Ok(Arc::new(Self {
            stop_signal,
            join_handle: Mutex::new(Some(join_handle)),
            finished: Mutex::new(Some(rx)),
            stopped: AtomicBool::new(false),
        }))
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

        let finished = self
            .finished
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let join_handle = self
            .join_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let Some(join_handle) = join_handle else {
            return;
        };

        let ended = match finished {
            Some(rx) => matches!(
                rx.recv_timeout(timeout),
                Err(RecvTimeoutError::Disconnected)
            ),
            None => false,
        };
        if ended {
            if join_handle.join().is_err() {
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
        self.stop(Duration::from_secs(5));
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
fn accept_loop(lifecycle: Arc<Lifecycle>, stop: Arc<StopSignal>) -> Result<()> {
    let pipe_name = pipe_name_for(&lifecycle.instance_id());
    let sa = ProtectedSecurityAttributes::new().context("pipe 用 DACL の作成に失敗しました")?;
    let name_wide = to_wide(&pipe_name);

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
            return Err(anyhow::anyhow!("named pipe の作成に失敗しました"));
        }
        let pipe = OwnedPipeHandle(pipe_handle);

        match await_connection(&pipe, &stop)? {
            Connection::Established => {
                // SAFETY: `pipe` から所有権を移譲した接続確立済みハンドル。
                let stream =
                    unsafe { PipeStream::from_server_handle(pipe.into_raw(), Some(stop.clone())) };
                if stop.is_signaled() {
                    break;
                }
                serve_connection(stream, lifecycle.clone());
            }
            Connection::Stopped => break,
            Connection::Retry => {
                drop(pipe);
                if stop.wait_for(ACCEPT_RETRY_INTERVAL) {
                    break;
                }
            }
        }
    }

    Ok(())
}

/// 接続待ちの結果。
enum Connection {
    /// クライアントとの接続が確立した。
    Established,
    /// 停止要求により待受を終了する。
    Stopped,
    /// 一過性の失敗。待受を作り直して再試行する。
    Retry,
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
                    Err(e) => {
                        tracing::warn!("接続完了の確認に失敗しました: {e}");
                        Ok(Connection::Retry)
                    }
                },
                WaitOutcome::Signaled(_) => {
                    op.cancel_and_drain();
                    Ok(Connection::Stopped)
                }
                WaitOutcome::TimedOut => Ok(Connection::Retry),
                WaitOutcome::Failed(e) => Err(anyhow::anyhow!("接続待ちの待機に失敗しました: {e}")),
            }
        }
        Err(e) => {
            // 待受インスタンスは毎回作り直すため、失敗が一過性であれば
            // 再試行で回復する。恒久的な失敗でも間隔を空けて再試行するため
            // ループが CPU を占有することはない。
            tracing::warn!("接続受理に失敗しました: {e}");
            Ok(Connection::Retry)
        }
    }
}

/// 接続が確立したらセッション処理に委譲する。
fn serve_connection(stream: PipeStream, lifecycle: Arc<Lifecycle>) {
    session::handle_connection(stream, lifecycle);
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
    use aviutl2_mcp_core::{AuthSecret, InstanceId, InstanceState, ProtocolVersion};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, OPEN_EXISTING,
    };

    /// テストクライアントが 1 回の読み書きに許す上限。
    const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(10);

    fn temp_lifecycle() -> (Arc<Lifecycle>, std::path::PathBuf) {
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
            "2026-01-01T00:00:00Z".to_string(),
            Some("0x0".to_string()),
            "2026-01-01T00:00:00Z".to_string(),
            writer,
        )
        .unwrap();
        lifecycle.transition_to(InstanceState::Ready).unwrap();
        (Arc::new(lifecycle), dir)
    }

    fn cleanup(dir: std::path::PathBuf) {
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn connect_client(pipe_name: &str) -> PipeStream {
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
                    if start.elapsed() > Duration::from_secs(5) {
                        panic!("pipe への接続に失敗しました: {e}");
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
        let hello = aviutl2_mcp_core::ClientHello {
            protocol_version: ProtocolVersion::CURRENT,
            instance_id,
            client_nonce: client_nonce.clone(),
        };
        serde_json::to_vec(&hello).unwrap()
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

    #[test]
    fn handshake_and_ping() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = PipeServer::start(lifecycle.clone()).unwrap();
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let client_nonce = aviutl2_mcp_core::Nonce::generate();
        send(&client, &make_hello(id, &client_nonce), "ClientHello");

        let server_auth_body = recv(&client, "ServerAuth").expect("ServerAuth が受信できません");
        let server_auth: aviutl2_mcp_core::ServerAuth =
            serde_json::from_slice(&server_auth_body).unwrap();
        assert_eq!(server_auth.instance_id, id);
        assert_eq!(server_auth.protocol_version, ProtocolVersion::CURRENT);

        let server_mac = aviutl2_mcp_core::compute_server_mac(
            &secret,
            &client_nonce,
            &server_auth.server_nonce,
            &id,
            &server_auth.protocol_version,
        );
        assert_eq!(server_mac.as_bytes(), server_auth.server_mac.as_bytes());

        send(
            &client,
            &make_auth(&secret, &server_auth.server_nonce, &client_nonce),
            "ClientAuth",
        );

        let request_id = aviutl2_mcp_core::RequestId::new();
        send(
            &client,
            &make_ping(server_auth.protocol_version, request_id, id),
            "ping 要求",
        );

        let response_body = recv(&client, "ping 応答").expect("ping 応答が受信できません");
        let response: aviutl2_mcp_core::ResponseEnvelope =
            serde_json::from_slice(&response_body).unwrap();
        assert_eq!(response.request_id, request_id);
        assert_eq!(response.instance_id, id);
        assert!(matches!(
            response.result,
            aviutl2_mcp_core::ResponseResult::Ok { .. }
        ));
        if let aviutl2_mcp_core::ResponseResult::Ok { result } = response.result {
            assert_eq!(result["state"], "ready");
            assert_eq!(result["instance_id"], serde_json::to_value(id).unwrap());
        }

        drop(client);
        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    #[test]
    fn wrong_client_mac_disconnects_without_response() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = PipeServer::start(lifecycle.clone()).unwrap();
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
    fn silent_client_does_not_block_accept_loop() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = PipeServer::start(lifecycle.clone()).unwrap();
        let id = lifecycle.instance_id();

        // 何も送らないクライアントを接続したまま放置しても、停止要求で
        // 待受スレッドが終了できることを確認する。
        let silent = connect_client(&pipe_name_for(&id));

        let started = Instant::now();
        server.stop(Duration::from_secs(10));
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(10),
            "停止に {}ms かかりました",
            elapsed.as_millis()
        );

        drop(silent);
        cleanup(dir);
    }
}
