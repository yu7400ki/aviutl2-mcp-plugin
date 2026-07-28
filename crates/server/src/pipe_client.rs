//! named pipe client と handshake client。
//!
//! Windows API（pipe 接続・読み書き）はこの層に閉じ、上位は frame 単位で扱う。

use crate::win_io::{self, WinIoError};
use aviutl2_mcp_core::{
    AuthSecret, ClientAuth, ClientHello, FrameDecoder, InstanceId, Nonce, ProtocolVersion,
    RequestEnvelope, RequestId, ResponseEnvelope, ResponseResult, ServerAuth, compute_client_mac,
    compute_server_mac, deserialize_json, encode_frame, pipe_name_for, verify_mac,
};
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::time::Instant;
use thiserror::Error;
use tracing::{debug, instrument, trace, warn};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING,
};
use windows::Win32::System::Pipes::WaitNamedPipeW;
use windows::core::PCWSTR;

/// `WaitNamedPipeW` へ渡す待機時間の上限（ミリ秒）。
///
/// discovery の期限が長い場合でも 1 候補の接続待ちに引きずられないよう頭打ちにする。
const CONNECT_WAIT_CAP_MS: u128 = 5_000;

/// 1 回の読み取りで受け取る最大バイト数。
///
/// フレーム本体が大きい場合は複数回に分けて読み取り、デコーダへ逐次投入する。
/// これにより本体長にかかわらず読み取りバッファのサイズが一定に保たれる。
const READ_CHUNK_SIZE: usize = 8 * 1024;

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

impl From<WinIoError> for PipeClientError {
    fn from(err: WinIoError) -> Self {
        match err {
            WinIoError::TimedOut => Self::Timeout,
            WinIoError::Io(err) => Self::Io(err),
        }
    }
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
        let handle = connect_pipe(&pipe_name, deadline)?;

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
        self.write_frame(&request_body, deadline)?;

        let response_body = self.read_frame(deadline)?;
        let response: ResponseEnvelope =
            deserialize_json(&response_body).map_err(|_| PipeClientError::Json)?;

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
        self.write_frame(&m1_body, deadline)?;

        let m2_body = self.read_frame(deadline)?;
        let m2: ServerAuth = deserialize_json(&m2_body).map_err(|_| PipeClientError::Json)?;

        trace!("received server auth");

        // 採用版は M2 の protocol_version そのものであり、client は妥当性のみを検証する。
        // MAJOR 不一致、または client の対応最大 MINOR を超える MINOR は互換性がない。
        let negotiated = m2.protocol_version;
        if negotiated.major != client_max_version.major
            || negotiated.minor > client_max_version.minor
        {
            warn!("negotiated protocol version is not acceptable");
            return Err(PipeClientError::ProtocolMismatch);
        }

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
        self.write_frame(&m3_body, deadline)?;

        debug!(protocol_version = %self.protocol_version.as_str(), "handshake succeeded");
        Ok(())
    }

    fn write_frame(&self, body: &[u8], deadline: Instant) -> Result<(), PipeClientError> {
        let frame = encode_frame(body).map_err(|_| PipeClientError::Framing)?;
        win_io::write_all(self.handle, &frame, deadline)?;
        Ok(())
    }

    /// 期限内に 1 フレームを読み取る。
    ///
    /// フレーム長の検証と本体の組み立ては [`FrameDecoder`] に委譲する。
    /// 1 回の読み取り量はデコーダが要求する残りバイト数を上限とするため、
    /// フレーム境界を越えて先読みすることはなく、次のフレームのバイトを
    /// 抱え込む必要もない。
    ///
    /// 過大なフレーム長・長さ 0 はデコーダが本体を確保する前に拒否する。
    fn read_frame(&self, deadline: Instant) -> Result<Vec<u8>, PipeClientError> {
        let mut decoder = FrameDecoder::new();
        let mut chunk = [0u8; READ_CHUNK_SIZE];
        loop {
            if let Some(frame) = decoder.take_frame() {
                return Ok(frame);
            }
            let take = decoder.bytes_needed().min(chunk.len());
            win_io::read_exact(self.handle, &mut chunk[..take], deadline)?;
            decoder
                .feed(&chunk[..take])
                .map_err(|_| PipeClientError::Framing)?;
        }
    }
}

impl Drop for PipeClient {
    fn drop(&mut self) {
        // read/write は完了・キャンセルのいずれかを確認してから戻るため、
        // ここに到達した時点でこのハンドルに保留中の I/O は存在しない。
        // SAFETY: `self.handle` は本型のみが所有しており、ここでのみ閉じられる。
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// 指定 pipe 名に `deadline` までの範囲で接続する。
fn connect_pipe(pipe_name: &str, deadline: Instant) -> Result<HANDLE, PipeClientError> {
    let wide: Vec<u16> = OsStr::new(pipe_name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let remaining = win_io::remaining_until(deadline);
    if remaining.is_zero() {
        return Err(PipeClientError::Timeout);
    }

    // pipe サーバーが接続可能になるまで短時間待つ。
    // 第 2 引数の 0 は NMPWAIT_NOWAIT ではなく NMPWAIT_USE_DEFAULT_WAIT（pipe 既定の
    // タイムアウト）を意味するため、残り時間が 1 ミリ秒未満でも 0 を渡さない。
    let wait_ms = remaining.as_millis().clamp(1, CONNECT_WAIT_CAP_MS) as u32;
    // SAFETY: `wide` は NUL 終端した pipe 名であり、呼び出し中は生存している。
    unsafe {
        let _ = WaitNamedPipeW(PCWSTR(wide.as_ptr()), wait_ms);
    }

    // SAFETY: `wide` は NUL 終端した pipe 名であり、呼び出し中は生存している。
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

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::InstanceId;
    use std::time::Duration;

    #[test]
    fn pipe_name_generation_matches_descriptor() {
        let id = InstanceId::new_v4();
        assert_eq!(pipe_name_for(&id), pipe_name_for(&id));
    }

    #[test]
    fn connect_fails_immediately_when_deadline_passed() {
        let id = InstanceId::new_v4();
        let deadline = Instant::now() - Duration::from_secs(1);
        let started = Instant::now();
        let result = connect_pipe(&pipe_name_for(&id), deadline);
        assert!(matches!(result, Err(PipeClientError::Timeout)));
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}
