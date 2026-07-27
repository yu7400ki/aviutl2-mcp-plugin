//! named pipe client と handshake client。
//!
//! Windows API（pipe 接続・読み書き）はこの層に閉じ、上位は frame 単位で扱う。

use aviutl2_mcp_core::{
    AuthSecret, ClientAuth, ClientHello, InstanceId, Nonce, ProtocolVersion, RequestEnvelope,
    RequestId, ResponseEnvelope, ResponseResult, ServerAuth, compute_client_mac,
    compute_server_mac, encode_frame, negotiate, pipe_name_for, verify_mac,
};
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, instrument, trace, warn};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING, ReadFile, WriteFile,
};
use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::WaitNamedPipeW;
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};
use windows::core::PCWSTR;

/// pipe client のエラー。
#[derive(Debug, Error)]
pub enum PipeClientError {
    /// 接続失敗。
    #[error("pipe 接続に失敗しました")]
    ConnectFailed,
    /// タイムアウト。
    #[error("operation が期限を超過しました")]
    Timeout,
    /// 読み書きエラー。
    #[error("IO エラー: {0}")]
    Io(#[from] io::Error),
    /// フレーミングエラー。
    #[error("フレーミングエラー")]
    Framing,
    /// JSON エラー。
    #[error("JSON エラー")]
    Json,
    /// 認証失敗。
    #[error("handshake 検証に失敗しました")]
    AuthenticationFailed,
    /// プロトコル不一致。
    #[error("プロトコルバージョンが互換ではありません")]
    ProtocolMismatch,
    /// インスタンスが不一致または stale。
    #[error("instance identity が一致しません")]
    InstanceStale,
    /// 無効な応答。
    #[error("無効な応答を受信しました")]
    InvalidResponse,
}

/// 認証済み named pipe 接続。
pub struct PipeClient {
    handle: HANDLE,
    instance_id: InstanceId,
    protocol_version: ProtocolVersion,
}

impl PipeClient {
    /// `pipe_name` へ指定した期限で接続し、handshake を完了する。
    ///
    /// 成功時は認証済みの `PipeClient` を返す。
    #[instrument(skip(auth_secret), fields(instance_id = %descriptor_id))]
    pub fn connect_and_handshake(
        descriptor_id: InstanceId,
        descriptor_pid: u32,
        descriptor_created_at: &str,
        auth_secret: &AuthSecret,
        deadline: Instant,
    ) -> Result<Self, PipeClientError> {
        let pipe_name = pipe_name_for(&descriptor_id);
        let handle = connect_pipe(&pipe_name, duration_until(deadline))?;

        let mut client = Self {
            handle,
            instance_id: descriptor_id,
            protocol_version: ProtocolVersion::CURRENT,
        };

        client.handshake(
            descriptor_id,
            descriptor_pid,
            descriptor_created_at,
            auth_secret,
            deadline,
        )?;
        Ok(client)
    }

    /// ping を送信し、応答を検証する。
    #[instrument(skip(self), fields(instance_id = %self.instance_id))]
    pub fn ping(
        &self,
        deadline: Instant,
    ) -> Result<aviutl2_mcp_core::InstanceState, PipeClientError> {
        let request_id = RequestId::new();
        let request = RequestEnvelope::ping(self.protocol_version, request_id, self.instance_id);
        let request_body = serde_json::to_vec(&request).map_err(|_| PipeClientError::Json)?;
        self.write_frame(&request_body, duration_until(deadline))?;

        let response_body = self.read_frame(duration_until(deadline))?;
        let response: ResponseEnvelope =
            serde_json::from_slice(&response_body).map_err(|_| PipeClientError::Json)?;

        if response.request_id != request_id {
            warn!("request_id mismatch");
            return Err(PipeClientError::InvalidResponse);
        }
        if response.instance_id != self.instance_id {
            warn!("instance_id mismatch in ping response");
            return Err(PipeClientError::InstanceStale);
        }
        if response.protocol_version.major != self.protocol_version.major {
            warn!("protocol major mismatch in ping response");
            return Err(PipeClientError::ProtocolMismatch);
        }

        match response.result {
            ResponseResult::Ok { result } => {
                let state: aviutl2_mcp_core::InstanceState =
                    serde_json::from_value(result["state"].clone())
                        .map_err(|_| PipeClientError::InvalidResponse)?;
                let pong_id: InstanceId = serde_json::from_value(result["instance_id"].clone())
                    .map_err(|_| PipeClientError::InvalidResponse)?;
                if pong_id != self.instance_id {
                    warn!("instance_id in ping result mismatch");
                    return Err(PipeClientError::InstanceStale);
                }
                debug!(state = %state, "ping succeeded");
                Ok(state)
            }
            ResponseResult::Err { error } => {
                warn!(code = %error.code, "ping returned error");
                Err(PipeClientError::InvalidResponse)
            }
        }
    }

    /// client 側 handshake を実行する。
    fn handshake(
        &mut self,
        descriptor_id: InstanceId,
        descriptor_pid: u32,
        descriptor_created_at: &str,
        auth_secret: &AuthSecret,
        deadline: Instant,
    ) -> Result<(), PipeClientError> {
        let client_nonce = Nonce::generate();
        let client_max_version = ProtocolVersion::CURRENT;
        let m1 = ClientHello {
            protocol_version: client_max_version,
            instance_id: descriptor_id,
            client_nonce: client_nonce.clone(),
        };
        let m1_body = serde_json::to_vec(&m1).map_err(|_| PipeClientError::Json)?;
        self.write_frame(&m1_body, duration_until(deadline))?;

        let m2_body = self.read_frame(duration_until(deadline))?;
        let m2: ServerAuth = serde_json::from_slice(&m2_body).map_err(|_| PipeClientError::Json)?;

        trace!("received server auth");

        // version negotiation: M2 の採用版を検証。
        let negotiated = negotiate(client_max_version, m2.protocol_version)
            .map_err(|_| PipeClientError::ProtocolMismatch)?;

        // identity 検証。
        if m2.instance_id != descriptor_id {
            warn!("instance_id mismatch in handshake");
            return Err(PipeClientError::InstanceStale);
        }
        if m2.pid != descriptor_pid {
            warn!("pid mismatch in handshake");
            return Err(PipeClientError::InstanceStale);
        }
        if m2.process_created_at != descriptor_created_at {
            warn!("process_created_at mismatch in handshake");
            return Err(PipeClientError::InstanceStale);
        }

        let expected_server_mac = compute_server_mac(
            auth_secret.as_bytes(),
            &client_nonce,
            &m2.server_nonce,
            &m2.instance_id,
            &negotiated,
        );
        if !verify_mac(&expected_server_mac, &m2.server_mac) {
            warn!("server_mac verification failed");
            return Err(PipeClientError::AuthenticationFailed);
        }

        self.protocol_version = negotiated;

        let client_mac =
            compute_client_mac(auth_secret.as_bytes(), &m2.server_nonce, &client_nonce);
        let m3 = ClientAuth { client_mac };
        let m3_body = serde_json::to_vec(&m3).map_err(|_| PipeClientError::Json)?;
        self.write_frame(&m3_body, duration_until(deadline))?;

        debug!(protocol_version = %self.protocol_version.as_str(), "handshake succeeded");
        Ok(())
    }

    fn write_frame(&self, body: &[u8], timeout: Duration) -> Result<(), PipeClientError> {
        let frame = encode_frame(body).map_err(|_| PipeClientError::Framing)?;
        self.write_all(&frame, timeout)
    }

    fn read_frame(&self, timeout: Duration) -> Result<Vec<u8>, PipeClientError> {
        let mut length_buf = [0u8; 4];
        self.read_exact(&mut length_buf, timeout)?;
        let length = u32::from_le_bytes(length_buf) as usize;
        if length == 0 || length > aviutl2_mcp_core::MAX_FRAME_SIZE as usize {
            return Err(PipeClientError::Framing);
        }
        let mut body = vec![0u8; length];
        self.read_exact(&mut body, timeout)?;
        Ok(body)
    }

    fn read_exact(&self, buf: &mut [u8], timeout: Duration) -> Result<(), PipeClientError> {
        let mut overlapped = new_overlapped()?;
        let mut total = 0;
        while total < buf.len() {
            unsafe {
                ResetEvent(overlapped.hEvent).map_err(into_io_error)?;
            }
            let mut read = 0u32;
            let slice = &mut buf[total..];
            let result = unsafe {
                ReadFile(
                    self.handle,
                    Some(slice),
                    Some(&mut read),
                    Some(&mut overlapped),
                )
            };
            if result.is_ok() {
                total += read as usize;
                continue;
            }
            let err = result.unwrap_err();
            if err.code() != windows::Win32::Foundation::ERROR_IO_PENDING.into() {
                return Err(into_io_error(err).into());
            }
            wait_io(overlapped.hEvent, timeout)?;
            let bytes_transferred = unsafe { get_overlapped_result(self.handle, &overlapped)? };
            if bytes_transferred == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "pipe closed").into());
            }
            total += bytes_transferred as usize;
        }
        Ok(())
    }

    fn write_all(&self, buf: &[u8], timeout: Duration) -> Result<(), PipeClientError> {
        let mut overlapped = new_overlapped()?;
        let mut total = 0;
        while total < buf.len() {
            unsafe {
                ResetEvent(overlapped.hEvent).map_err(into_io_error)?;
            }
            let mut written = 0u32;
            let result = unsafe {
                WriteFile(
                    self.handle,
                    Some(&buf[total..]),
                    Some(&mut written),
                    Some(&mut overlapped),
                )
            };
            if result.is_ok() {
                total += written as usize;
                continue;
            }
            let err = result.unwrap_err();
            if err.code() != windows::Win32::Foundation::ERROR_IO_PENDING.into() {
                return Err(into_io_error(err).into());
            }
            wait_io(overlapped.hEvent, timeout)?;
            let bytes_transferred = unsafe { get_overlapped_result(self.handle, &overlapped)? };
            total += bytes_transferred as usize;
        }
        Ok(())
    }
}

impl Drop for PipeClient {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// 指定 pipe 名に接続する。
fn connect_pipe(pipe_name: &str, timeout: Duration) -> Result<HANDLE, PipeClientError> {
    let wide: Vec<u16> = OsStr::new(pipe_name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;

    // pipe サーバーが接続可能になるまで短時間待つ。
    unsafe {
        let _ = WaitNamedPipeW(PCWSTR(wide.as_ptr()), timeout_ms.min(5000));
    }

    unsafe {
        let handle = CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            None,
        );

        match handle {
            Ok(h) if h != INVALID_HANDLE_VALUE => Ok(h),
            Ok(_) => Err(PipeClientError::ConnectFailed),
            Err(_) => Err(PipeClientError::ConnectFailed),
        }
    }
}

/// 新しい OVERLAPPED と手動リセットイベントを作成する。
fn new_overlapped() -> io::Result<OVERLAPPED> {
    unsafe {
        let event = CreateEventW(None, true, false, None)?;
        let mut overlapped = std::mem::zeroed::<OVERLAPPED>();
        overlapped.hEvent = event;
        Ok(overlapped)
    }
}

/// IO 完了を指定時間待つ。
fn wait_io(event: HANDLE, timeout: Duration) -> Result<(), PipeClientError> {
    let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
    let result = unsafe { WaitForSingleObject(event, ms) };
    if result.0 == windows::Win32::Foundation::WAIT_OBJECT_0.0 {
        Ok(())
    } else {
        Err(PipeClientError::Timeout)
    }
}

/// OVERLAPPED 結果を取得する。
unsafe fn get_overlapped_result(handle: HANDLE, overlapped: &OVERLAPPED) -> io::Result<u32> {
    let mut transferred = 0u32;
    unsafe {
        GetOverlappedResult(handle, overlapped, &mut transferred, false).map_err(into_io_error)?;
    }
    Ok(transferred)
}

/// `windows::core::Error` を `io::Error` へ変換する。
fn into_io_error(err: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(err.code().0)
}

/// 現在時刻から `deadline` までの残り時間を返す。
fn duration_until(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::InstanceId;

    #[test]
    fn timeout_duration_is_nonnegative() {
        let now = Instant::now();
        assert_eq!(duration_until(now), Duration::ZERO);
        let future = now + Duration::from_secs(1);
        assert!(!duration_until(future).is_zero());
    }

    #[test]
    fn pipe_name_generation_matches_descriptor() {
        let id = InstanceId::new_v4();
        assert_eq!(pipe_name_for(&id), pipe_name_for(&id));
    }
}
