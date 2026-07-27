//! Named pipe server 接続受付と byte stream 読み書き。
//!
//! duplex・byte mode・リモート接続拒否・保護 DACL で named pipe を作成し、
//! 専用スレッドで接続受理 → handshake/ping → 切断 のループを回す。

use crate::lifecycle::Lifecycle;
use crate::security::ProtectedSecurityAttributes;
use crate::session;
use anyhow::{Context, Result};
use aviutl2_mcp_core::identifier::pipe_name_for;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{JoinHandle, spawn};
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{PIPE_ACCESS_DUPLEX, ReadFile, WriteFile};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
};
use windows::core::PCWSTR;

/// 1 本の named pipe 接続に対する読み書きストリーム。
pub struct PipeStream {
    handle: HANDLE,
}

// `HANDLE` は生ポインタだが、`PipeStream` は所有して単一スレッドで使用する。
unsafe impl Send for PipeStream {}

impl Drop for PipeStream {
    fn drop(&mut self) {
        unsafe {
            // クライアント側に read/write が残っている場合、
            // DisconnectNamedPipe や CloseHandle がブロックし得るため、
            // ハンドルを即座に閉じる。
            let _ = CloseHandle(self.handle);
        }
    }
}

impl PipeStream {
    /// `HANDLE` から `PipeStream` を作成する。
    ///
    /// # Safety
    ///
    /// `handle` は有効な named pipe 接続ハンドルである必要がある。
    unsafe fn from_handle(handle: HANDLE) -> Self {
        Self { handle }
    }

    fn read_all(&self, buf: &mut [u8]) -> anyhow::Result<Option<()>> {
        let mut total = 0;
        while total < buf.len() {
            let mut read = 0u32;
            let result = unsafe {
                ReadFile(
                    self.handle,
                    Some(&mut buf[total..]),
                    Some(&mut read as *mut u32),
                    None,
                )
            };
            match result {
                Ok(()) => {
                    if read == 0 {
                        return if total == 0 {
                            Ok(None)
                        } else {
                            Err(anyhow::anyhow!("pipe 接続が途中で閉じられました"))
                        };
                    }
                    total += read as usize;
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("pipe 読み込みに失敗しました: {e}"));
                }
            }
        }
        Ok(Some(()))
    }

    pub fn read_frame(&self) -> anyhow::Result<Option<Vec<u8>>> {
        let mut len_bytes = [0u8; 4];
        match self.read_all(&mut len_bytes)? {
            Some(()) => {}
            None => return Ok(None),
        }
        let len = u32::from_le_bytes(len_bytes) as usize;
        if len == 0 || len > aviutl2_mcp_core::framing::MAX_FRAME_SIZE as usize {
            return Err(anyhow::anyhow!("無効なフレーム長です: {len}"));
        }
        let mut body = vec![0u8; len];
        self.read_all(&mut body)?
            .ok_or_else(|| anyhow::anyhow!("pipe 接続が途中で閉じられました"))?;
        Ok(Some(body))
    }

    pub fn write_all(&self, buf: &[u8]) -> anyhow::Result<()> {
        let mut total = 0;
        while total < buf.len() {
            let mut written = 0u32;
            unsafe {
                WriteFile(
                    self.handle,
                    Some(&buf[total..]),
                    Some(&mut written as *mut u32),
                    None,
                )
            }
            .map_err(|e| anyhow::anyhow!("pipe 書き込みに失敗しました: {e}"))?;
            if written == 0 {
                return Err(anyhow::anyhow!("pipe 書き込みで 0 バイトが返されました"));
            }
            total += written as usize;
        }
        Ok(())
    }

    pub fn write_frame(&self, body: &[u8]) -> anyhow::Result<()> {
        let frame = aviutl2_mcp_core::framing::encode_frame(body)
            .map_err(|e| anyhow::anyhow!("フレームエンコードに失敗しました: {e}"))?;
        self.write_all(&frame)
    }
}

/// named pipe server の制御ハンドル。
pub struct PipeServer {
    stop: AtomicBool,
    pending_handle: AtomicUsize,
    join_handle: Mutex<Option<JoinHandle<()>>>,
    stopped: AtomicBool,
}

impl PipeServer {
    /// 指定したライフサイクルに紐づく named pipe server を起動する。
    pub fn start(lifecycle: Arc<Lifecycle>) -> Result<Arc<Self>> {
        let server = Arc::new(Self {
            stop: AtomicBool::new(false),
            pending_handle: AtomicUsize::new(0),
            join_handle: Mutex::new(None),
            stopped: AtomicBool::new(false),
        });

        let server_for_thread = Arc::clone(&server);
        let handle = spawn(move || {
            if let Err(e) = accept_loop(lifecycle, server_for_thread) {
                tracing::error!("named pipe server ループが異常終了しました: {e:?}");
            }
        });

        *server.join_handle.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        Ok(server)
    }

    /// サーバーを停止する。タイムアウト内に join できなければ切り離してログ化する。
    pub fn stop(&self, timeout: Duration) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stop.store(true, Ordering::Release);

        let pending = self.pending_handle.load(Ordering::Acquire);
        if pending != 0 {
            unsafe {
                let _ = CloseHandle(HANDLE(pending as *mut c_void));
            }
        }

        let join_handle = self
            .join_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(handle) = join_handle {
            match handle.join_timeout(timeout) {
                Ok(()) => {}
                Err(_) => {
                    tracing::error!("named pipe server スレッドの停止がタイムアウトしました");
                }
            }
        }
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        self.stop(Duration::from_secs(5));
    }
}

/// 接続受理ループ。
fn accept_loop(lifecycle: Arc<Lifecycle>, server: Arc<PipeServer>) -> Result<()> {
    let pipe_name = pipe_name_for(&lifecycle.instance_id());
    let sa = ProtectedSecurityAttributes::new().context("pipe 用 DACL の作成に失敗しました")?;
    let name_wide = to_wide(&pipe_name);

    loop {
        if server.stop.load(Ordering::Acquire)
            || lifecycle.state() == aviutl2_mcp_core::state::InstanceState::Gone
        {
            break;
        }

        let pipe_handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(name_wide.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                64 * 1024,
                64 * 1024,
                0,
                Some(sa.as_ptr()),
            )
        };
        if pipe_handle.is_invalid() {
            return Err(anyhow::anyhow!("named pipe の作成に失敗しました"));
        }
        server
            .pending_handle
            .store(pipe_handle.0 as usize, Ordering::Release);

        let connected = unsafe { ConnectNamedPipe(pipe_handle, None).is_ok() };

        // pending_handle をクリアしてからハンドルの所有権を移動する。
        server.pending_handle.store(0, Ordering::Release);

        if server.stop.load(Ordering::Acquire) {
            if !connected {
                // stop 側が既にクローズしている可能性があるため、二重クローズを避ける。
            } else {
                unsafe {
                    let _ = DisconnectNamedPipe(pipe_handle);
                    let _ = CloseHandle(pipe_handle);
                }
            }
            break;
        }

        if connected {
            serve_connection(pipe_handle, lifecycle.clone());
            continue;
        }

        // クライアントが既に接続済みの場合もある。
        let err = unsafe { windows::Win32::Foundation::GetLastError() };
        if err.0 == windows::Win32::Foundation::ERROR_PIPE_CONNECTED.0 {
            serve_connection(pipe_handle, lifecycle.clone());
            continue;
        }

        unsafe {
            let _ = CloseHandle(pipe_handle);
        }
    }

    Ok(())
}

/// 接続が確立したらセッション処理に委譲する。
fn serve_connection(handle: HANDLE, lifecycle: Arc<Lifecycle>) {
    let stream = unsafe { PipeStream::from_handle(handle) };
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

/// `JoinHandle` にタイムアウト付き join を提供する trait。
trait JoinHandleTimeout {
    fn join_timeout(self, timeout: Duration) -> std::thread::Result<()>;
}

impl JoinHandleTimeout for JoinHandle<()> {
    fn join_timeout(self, timeout: Duration) -> std::thread::Result<()> {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if self.is_finished() {
                return self.join();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "join timeout",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::{AuthSecret, InstanceId, InstanceState, ProtocolVersion};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        OPEN_EXISTING,
    };

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
        use std::time::{Duration, Instant};
        let wide: Vec<u16> = std::ffi::OsStr::new(pipe_name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let start = Instant::now();
        loop {
            let result = unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
                    windows::Win32::Storage::FileSystem::FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            };
            match result {
                Ok(handle) => return unsafe { PipeStream::from_handle(handle) },
                Err(e) => {
                    if start.elapsed() > Duration::from_secs(5) {
                        panic!("pipe への接続に失敗しました: {e}");
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
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
        client.write_frame(&make_hello(id, &client_nonce)).unwrap();

        let server_auth_body = client.read_frame().unwrap().unwrap();
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

        client
            .write_frame(&make_auth(
                &secret,
                &server_auth.server_nonce,
                &client_nonce,
            ))
            .unwrap();

        let request_id = aviutl2_mcp_core::RequestId::new();
        client
            .write_frame(&make_ping(server_auth.protocol_version, request_id, id))
            .unwrap();

        let response_body = client.read_frame().unwrap().unwrap();
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

        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }

    #[test]
    fn wrong_client_mac_closes_connection() {
        let (lifecycle, dir) = temp_lifecycle();
        let server = PipeServer::start(lifecycle.clone()).unwrap();
        let id = lifecycle.instance_id();
        let secret = *lifecycle.auth_secret().as_bytes();

        let client = connect_client(&pipe_name_for(&id));
        let client_nonce = aviutl2_mcp_core::Nonce::generate();
        client.write_frame(&make_hello(id, &client_nonce)).unwrap();

        let server_auth_body = client.read_frame().unwrap().unwrap();
        let server_auth: aviutl2_mcp_core::ServerAuth =
            serde_json::from_slice(&server_auth_body).unwrap();

        // 改竄された client_mac を送信
        let mut wrong_secret = secret;
        wrong_secret[0] ^= 0xFF;
        client
            .write_frame(&make_auth(
                &wrong_secret,
                &server_auth.server_nonce,
                &client_nonce,
            ))
            .unwrap();

        // サーバーは認証失敗のエラー応答を返してから切断する
        let response_body = client.read_frame().unwrap().unwrap();
        let response: aviutl2_mcp_core::ResponseEnvelope =
            serde_json::from_slice(&response_body).unwrap();
        assert!(matches!(
            response.result,
            aviutl2_mcp_core::ResponseResult::Err { .. }
        ));
        if let aviutl2_mcp_core::ResponseResult::Err { error } = response.result {
            assert_eq!(
                error.code,
                aviutl2_mcp_core::ErrorCode::AuthenticationFailed
            );
        }

        server.stop(Duration::from_secs(5));
        cleanup(dir);
    }
}
