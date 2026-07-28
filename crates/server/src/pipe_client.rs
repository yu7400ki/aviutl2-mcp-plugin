//! named pipe client と handshake client。
//!
//! Windows API（pipe 接続・読み書き）はこの層に閉じ、上位は frame 単位で扱う。

use crate::win_io::{self, WinIoError};
use aviutl2_mcp_core::{
    AuthSecret, ClientAuth, ClientHello, FrameDecoder, InstanceId, Nonce, ProtocolVersion,
    RequestEnvelope, RequestId, ResponseEnvelope, ResponseResult, ServerAuth, compute_client_mac,
    compute_server_mac, deserialize_json, encode_frame, pipe_name_for, verify_mac,
};
use chrono::Utc;
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
        let request = RequestEnvelope::ping(self.protocol_version, request_id, self.instance_id)
            .with_deadline(deadline_to_unix_ms(deadline));
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

/// 単調時計上の期限を、Envelope が運ぶ壁時計基準の Unix ミリ秒へ変換する。
///
/// [`Instant`] は epoch を持たない単調時計であり、壁時計時刻へ直接変換できない。
/// そこで「今から期限までの残り時間」を求め、それを現在の壁時計時刻へ加算する。
/// 2 つの時計を読む間に生じる誤差はミリ秒未満で、秒単位の期限には影響しない。
/// 期限を過ぎている場合は残り時間 0、すなわち現在時刻がそのまま期限になる。
///
/// 壁時計が Unix epoch より前を指す場合や加算が溢れる場合は期限を表現できないため、
/// 期限未指定（`None`）として送る。受信側は自身の上限だけを適用する。
fn deadline_to_unix_ms(deadline: Instant) -> Option<u64> {
    let remaining_ms = u64::try_from(win_io::remaining_until(deadline).as_millis()).ok()?;
    let now_unix_ms = u64::try_from(Utc::now().timestamp_millis()).ok()?;
    now_unix_ms.checked_add(remaining_ms)
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
    use aviutl2_mcp_core::{InstanceId, InstanceState};
    use std::time::Duration;
    use windows::Win32::Storage::FileSystem::{PIPE_ACCESS_DUPLEX, ReadFile, WriteFile};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE,
    };

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

    /// フレーム受信を検証するための named pipe 対向端。
    ///
    /// 相手側（plugin 役）のハンドルを保持し、任意のバイト列を送出する。
    /// 読み取り側は本番と同じ [`PipeClient::read_frame`] を通る。
    struct MockPeer {
        handle: HANDLE,
    }

    impl MockPeer {
        /// 対向端と、それに接続済みの `PipeClient` を作る。
        fn connected() -> (Self, PipeClient) {
            let name = format!(r"\\.\pipe\aviutl2-mcp-frame-test-{}", InstanceId::new_v4());
            let wide: Vec<u16> = OsStr::new(&name)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();

            // SAFETY: `wide` は NUL 終端した pipe 名であり、呼び出し中は生存している。
            let peer = unsafe {
                CreateNamedPipeW(
                    PCWSTR(wide.as_ptr()),
                    PIPE_ACCESS_DUPLEX,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    64 * 1024,
                    64 * 1024,
                    0,
                    None,
                )
            };
            assert!(!peer.is_invalid(), "テスト用 pipe の作成に失敗しました");

            // SAFETY: `wide` は NUL 終端した pipe 名であり、呼び出し中は生存している。
            let client_handle = unsafe {
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

            // 接続済みのため `ConnectNamedPipe` は待たずに戻る。
            // SAFETY: `peer` は直前に作成した有効な pipe ハンドル。
            let _ = unsafe { ConnectNamedPipe(peer, None) };

            let client = PipeClient {
                handle: client_handle,
                instance_id: InstanceId::new_v4(),
                protocol_version: ProtocolVersion::CURRENT,
            };
            (Self { handle: peer }, client)
        }

        fn raw(&self) -> HANDLE {
            self.handle
        }

        fn send(&self, bytes: &[u8]) {
            send_bytes(self.handle, bytes);
        }
    }

    /// 対向端から同期的に指定バイト数を読み取る。
    fn recv_bytes(handle: HANDLE, len: usize) -> Vec<u8> {
        let mut buffer = vec![0u8; len];
        let mut filled = 0usize;
        while filled < len {
            let mut read = 0u32;
            // SAFETY: `handle` は生存中の pipe ハンドル、書き込み先は本呼び出し中に生存する。
            unsafe { ReadFile(handle, Some(&mut buffer[filled..]), Some(&mut read), None) }
                .expect("テスト用 pipe からの読み取りに失敗しました");
            assert_ne!(read, 0, "テスト用 pipe が切断されました");
            filled += read as usize;
        }
        buffer
    }

    /// 対向端から 1 フレームを読み取り、本体を返す。
    fn recv_frame(handle: HANDLE) -> Vec<u8> {
        let length = u32::from_le_bytes(recv_bytes(handle, 4).try_into().unwrap()) as usize;
        recv_bytes(handle, length)
    }

    impl Drop for MockPeer {
        fn drop(&mut self) {
            // SAFETY: `self.handle` は本型のみが所有しており、ここでのみ閉じられる。
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }

    /// 対向端へ同期的に全バイトを書き込む。
    fn send_bytes(handle: HANDLE, bytes: &[u8]) {
        let mut written = 0u32;
        // SAFETY: `handle` は生存中の pipe ハンドル、`bytes` は本呼び出し中に生存する。
        unsafe { WriteFile(handle, Some(bytes), Some(&mut written), None) }
            .expect("テスト用 pipe への書き込みに失敗しました");
        assert_eq!(written as usize, bytes.len());
    }

    /// スレッド間で渡すための生ハンドル。
    ///
    /// ハンドルの所有権は `MockPeer` が持ち続け、こちらは値の複製のみを運ぶ。
    struct SendHandle(HANDLE);

    // SAFETY: pipe ハンドルはスレッドを跨いで使用でき、所有権は移動しない。
    unsafe impl Send for SendHandle {}

    #[test]
    fn read_frame_reassembles_frame_split_across_writes() {
        let (peer, client) = MockPeer::connected();
        let body = b"{\"kind\":\"split\"}".to_vec();
        let frame = encode_frame(&body).unwrap();

        // 長さ・本体の双方が複数回の読み取りに跨るよう、間隔を空けて送出する。
        let handle = SendHandle(peer.raw());
        let parts: Vec<Vec<u8>> = vec![
            frame[..2].to_vec(),
            frame[2..4].to_vec(),
            frame[4..6].to_vec(),
            frame[6..].to_vec(),
        ];
        let writer = std::thread::spawn(move || {
            let handle = handle;
            for part in parts {
                std::thread::sleep(Duration::from_millis(20));
                send_bytes(handle.0, &part);
            }
        });

        let received = client
            .read_frame(Instant::now() + Duration::from_secs(10))
            .expect("分割送信されたフレームを受信できません");
        assert_eq!(received, body);
        writer.join().unwrap();
    }

    #[test]
    fn read_frame_reads_batched_frames_in_order() {
        let (peer, client) = MockPeer::connected();
        let first = b"{\"n\":1}".to_vec();
        let second = b"{\"n\":2}".to_vec();

        // 2 フレームを 1 回の書き込みでまとめて送る。
        let mut batched = encode_frame(&first).unwrap();
        batched.extend_from_slice(&encode_frame(&second).unwrap());
        peer.send(&batched);

        let deadline = Instant::now() + Duration::from_secs(10);
        assert_eq!(client.read_frame(deadline).unwrap(), first);
        assert_eq!(client.read_frame(deadline).unwrap(), second);
    }

    #[test]
    fn read_frame_reassembles_body_larger_than_read_chunk() {
        let (peer, client) = MockPeer::connected();
        // 1 回の読み取り上限を超える本体は複数回に分けて読み取られる。
        let body = vec![b'x'; READ_CHUNK_SIZE * 2 + 123];
        peer.send(&encode_frame(&body).unwrap());

        let received = client
            .read_frame(Instant::now() + Duration::from_secs(10))
            .expect("大きな本体を受信できません");
        assert_eq!(received, body);
    }

    #[test]
    fn read_frame_rejects_invalid_length_without_reading_body() {
        for length in [0u32, aviutl2_mcp_core::MAX_FRAME_SIZE + 1] {
            let (peer, client) = MockPeer::connected();
            // 本体を 1 バイトも送らずに長さだけを送る。
            peer.send(&length.to_le_bytes());

            let started = Instant::now();
            let result = client.read_frame(started + Duration::from_secs(5));
            assert!(
                matches!(result, Err(PipeClientError::Framing)),
                "フレーム長 {length} が拒否されませんでした"
            );
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "フレーム長 {length} の拒否に {}ms かかりました（本体を待っています）",
                started.elapsed().as_millis()
            );
        }
    }

    #[test]
    fn deadline_to_unix_ms_reflects_remaining_time() {
        let before = Utc::now().timestamp_millis() as u64;
        let value = deadline_to_unix_ms(Instant::now() + Duration::from_secs(5))
            .expect("期限を Unix ミリ秒へ変換できません");
        let after = Utc::now().timestamp_millis() as u64;

        // 残り時間はミリ秒未満を切り捨てるため、下限は 1 ミリ秒緩める。
        assert!(
            value >= before + 4_999 && value <= after + 5_000,
            "残り時間が反映されていません: value={value}, before={before}, after={after}"
        );
    }

    #[test]
    fn deadline_to_unix_ms_for_passed_deadline_is_now() {
        let before = Utc::now().timestamp_millis() as u64;
        let value = deadline_to_unix_ms(Instant::now() - Duration::from_secs(60))
            .expect("期限を Unix ミリ秒へ変換できません");
        let after = Utc::now().timestamp_millis() as u64;

        // 残り時間は 0 に丸められ、過去へは戻らない。
        assert!(
            value >= before && value <= after,
            "過ぎた期限が現在時刻になっていません: value={value}, before={before}, after={after}"
        );
    }

    #[test]
    fn ping_request_carries_deadline() {
        let (peer, client) = MockPeer::connected();
        let instance_id = client.instance_id;

        // ping は要求送信後に応答を待って戻るため、対向端は別スレッドで応対する。
        let handle = SendHandle(peer.raw());
        let responder = std::thread::spawn(move || {
            let handle = handle;
            let body = recv_frame(handle.0);
            let request: RequestEnvelope = serde_json::from_slice(&body).unwrap();
            let response = ResponseEnvelope::pong(
                request.protocol_version,
                request.request_id,
                instance_id,
                InstanceState::Ready,
            );
            send_bytes(
                handle.0,
                &encode_frame(&serde_json::to_vec(&response).unwrap()).unwrap(),
            );
            request
        });

        let before = Utc::now().timestamp_millis() as u64;
        let state = client
            .ping(Instant::now() + Duration::from_secs(5))
            .expect("ping に失敗しました");
        assert_eq!(state, InstanceState::Ready);

        let request = responder.join().unwrap();
        let deadline_unix_ms = request
            .deadline_unix_ms
            .expect("要求に deadline_unix_ms が設定されていません");
        assert!(
            deadline_unix_ms >= before + 4_000 && deadline_unix_ms <= before + 6_000,
            "deadline_unix_ms が期限を反映していません: {deadline_unix_ms}, before={before}"
        );
    }
}
