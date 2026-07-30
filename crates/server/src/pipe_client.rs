//! named pipe client と handshake client。
//!
//! Windows API（pipe 接続・読み書き）はこの層に閉じ、上位は frame 単位で扱う。

use crate::redact;
use crate::win_io::{self, WinIoError};
use aviutl2_mcp_core::{
    AuthSecret, ClientAuth, ClientHello, ErrorCode, ErrorObject, FrameDecoder, InstanceId, Nonce,
    PLUGIN_HANDSHAKE_TIMEOUT, PLUGIN_WRITE_TIMEOUT, PongResult, ProtocolVersion, RequestEnvelope,
    RequestId, RequestKind, ResponseEnvelope, ResponseResult, SERVER_CONNECT_WAIT_CAP, ServerAuth,
    compute_client_mac, compute_server_mac, deserialize_json, encode_frame, pipe_name_for,
    verify_mac,
};
use chrono::Utc;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::cell::Cell;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, instrument, trace, warn};
use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_BUSY, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING,
};
use windows::Win32::System::Pipes::WaitNamedPipeW;
use windows::core::PCWSTR;

/// `WaitNamedPipeW` へ渡す待機時間の上限（ミリ秒）。
///
/// 解決フェーズの予算は接続待ち・handshake・ping 往復で分け合う。残り時間を
/// そのまま接続待ちへ渡すと、接続できた時点で handshake と ping の持ち時間が
/// 尽き、応答している接続先を期限超過として扱ってしまう。接続待ちが取り分けて
/// よい上限を配分から取り、それ以上は待たない。
const CONNECT_WAIT_CAP_MS: u128 = SERVER_CONNECT_WAIT_CAP.as_millis();

/// 接続待ちが食い潰してはならない、handshake と ping の取り分。
///
/// 接続できた時点でこの取り分が残っていなければ、応答している接続先を期限超過と
/// して扱ってしまう。残り時間がこれを下回った時点で接続待ちを打ち切る。
const CONNECT_RESERVE: Duration = PLUGIN_HANDSHAKE_TIMEOUT.saturating_add(PLUGIN_WRITE_TIMEOUT);

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
    /// フレーム境界を見失った接続に対する要求。
    ///
    /// 期限超過や部分転送のあとは pipe に読み切れなかったバイトが残るため、
    /// 同じ接続で次の要求を送っても境界がずれたまま解釈される。
    #[error("接続はフレーム境界を見失っています")]
    Desynced,
    /// 接続先が返したエラー応答。
    ///
    /// 呼び出し側がそのまま外部へ渡せるよう、受け取った [`ErrorObject`] を
    /// 潰さずに運ぶ。
    #[error("接続先がエラーを返しました: {}", .0.code)]
    Remote(Box<ErrorObject>),
}

impl PipeClientError {
    /// 応答へ載せるエラーコードを返す。
    ///
    /// 接続先が返したエラーはそのコードをそのまま採用する。frame やスキーマの
    /// 破れは相手が契約どおりに応答していない状態であり、接続を張り直す以外に
    /// 回復手段が無いため、接続不能と同じく stale として扱う。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            Self::ConnectFailed
            | Self::Io(_)
            | Self::Framing
            | Self::Json
            | Self::InvalidResponse
            | Self::Desynced
            | Self::InstanceStale => ErrorCode::InstanceStale,
            Self::Timeout => ErrorCode::Timeout,
            Self::AuthenticationFailed => ErrorCode::AuthenticationFailed,
            Self::ProtocolMismatch => ErrorCode::ProtocolMismatch,
            Self::Remote(error) => error.code.clone(),
        }
    }
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
    /// フレーム境界を見失った接続を再利用させないための印。
    ///
    /// 要求は `&self` で送るため内部可変性で持つ。本型は生ハンドルを持ち
    /// `!Sync` であるため、[`Cell`] で足りる。
    desynced: Cell<bool>,
}

impl PipeClient {
    /// `pipe_name` へ指定した期限で接続し、handshake を完了する。
    ///
    /// 成功時は認証済みの `PipeClient` を返す。
    #[instrument(skip_all, fields(instance = %redact::instance_id(&descriptor_id), pid = descriptor_pid))]
    pub fn connect_and_handshake(
        descriptor_id: InstanceId,
        descriptor_pid: u32,
        descriptor_created_at: &str,
        auth_secret: &AuthSecret,
        deadline: Instant,
    ) -> Result<Self, PipeClientError> {
        let pipe_name = pipe_name_for(&descriptor_id);
        let handle = connect_pipe(&pipe_name, deadline)?;

        let client = Self {
            handle,
            instance_id: descriptor_id,
            desynced: Cell::new(false),
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

    /// 任意の operation を送信し、成功応答の `result` を返す。
    ///
    /// `request_id` の発番と期限の付与、応答の `request_id` / `instance_id` /
    /// `protocol_version` の整合検証をまとめて行う。接続先がエラー応答を返した場合は
    /// [`PipeClientError::Remote`] として `ErrorObject` をそのまま返す。
    #[instrument(skip_all, fields(instance = %redact::instance_id(&self.instance_id), operation = operation))]
    pub fn request(
        &self,
        operation: &str,
        params: serde_json::Value,
        deadline: Instant,
    ) -> Result<serde_json::Value, PipeClientError> {
        let request = RequestEnvelope {
            kind: RequestKind::Request,
            protocol_version: ProtocolVersion::CURRENT,
            request_id: RequestId::new(),
            instance_id: self.instance_id,
            deadline_unix_ms: deadline_to_unix_ms(deadline),
            operation: operation.to_string(),
            params,
        };
        self.exchange(request, deadline)
    }

    /// 型付きの params を送り、成功応答の `result` を型付きで受け取る。
    pub fn request_typed<P, R>(
        &self,
        operation: &str,
        params: &P,
        deadline: Instant,
    ) -> Result<R, PipeClientError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let params = serde_json::to_value(params).map_err(|_| PipeClientError::Json)?;
        let result = self.request(operation, params, deadline)?;
        self.decode_result(&result)
    }

    /// ping を送信し、応答を検証する。
    ///
    /// 接続先が ping を拒否した場合は [`PipeClientError::Remote`] を返す。拒否理由は
    /// 「起動中で今は応じられない」と「生存確認に失敗した」を区別するために要る。
    ///
    /// 応答が運ぶ内容は既定値で埋めずにそのまま返す。接続先が載せなかった値を
    /// 埋めると、未取得と実測値が区別できなくなる。
    #[instrument(skip_all, fields(instance = %redact::instance_id(&self.instance_id)))]
    pub fn ping(&self, deadline: Instant) -> Result<PongResult, PipeClientError> {
        let request =
            RequestEnvelope::ping(ProtocolVersion::CURRENT, RequestId::new(), self.instance_id)
                .with_deadline(deadline_to_unix_ms(deadline));

        let result = self.exchange(request, deadline)?;
        let pong: PongResult = self.decode_result(&result)?;
        if pong.instance_id != self.instance_id {
            warn!("instance_id in ping result mismatch");
            return Err(PipeClientError::InstanceStale);
        }
        debug!(state = %pong.state, "ping succeeded");
        Ok(pong)
    }

    /// 要求を 1 往復させ、応答の整合を検証して結果を取り出す。
    fn exchange(
        &self,
        request: RequestEnvelope,
        deadline: Instant,
    ) -> Result<serde_json::Value, PipeClientError> {
        if self.desynced.get() {
            return Err(PipeClientError::Desynced);
        }

        let request_id = request.request_id;
        // 呼び出し元の相関 ID を持つ span の中で記録し、MCP の tool call と
        // IPC の要求を後から突き合わせられるようにする。
        debug!(request_id = ?request_id, "sending request");
        // 直列化の失敗はまだ何も送っていないため、接続の境界には影響しない。
        let request_body = serde_json::to_vec(&request).map_err(|_| PipeClientError::Json)?;
        self.write_frame(&request_body, deadline)
            .map_err(|err| self.poison_on_desync(err))?;

        let response_body = self
            .read_frame(deadline)
            .map_err(|err| self.poison_on_desync(err))?;
        let response: ResponseEnvelope = deserialize_json(&response_body)
            .map_err(|_| self.poison_on_desync(PipeClientError::Json))?;

        if response.request_id != request_id {
            // 他の交換に属するフレームを読んだということであり、以降は要求と
            // 応答の対応が 1 つずつずれたまま解釈される。
            warn!("request_id mismatch");
            return Err(self.poison_on_desync(PipeClientError::InvalidResponse));
        }
        if response.instance_id != self.instance_id {
            warn!("instance_id mismatch in response");
            return Err(PipeClientError::InstanceStale);
        }
        if response.protocol_version != ProtocolVersion::CURRENT {
            warn!("protocol version mismatch in response");
            return Err(PipeClientError::ProtocolMismatch);
        }

        match response.result {
            ResponseResult::Ok { result } => Ok(result),
            ResponseResult::Err { error } => {
                warn!(code = %error.code, "request returned error");
                Err(PipeClientError::Remote(Box::new(error)))
            }
        }
    }

    /// 成功応答の `result` を型付きで読み取る。
    ///
    /// 読み取れない `result` は相手が契約から外れていることを意味するため、
    /// envelope の破れと同じく接続を毒化する。判定を [`PipeClient`] のメソッドに
    /// 置くことで、`exchange` の外で読む経路でも毒化が飛ばされないようにしている。
    fn decode_result<R: DeserializeOwned>(
        &self,
        result: &serde_json::Value,
    ) -> Result<R, PipeClientError> {
        decode_result_value(result).map_err(|err| self.poison_on_desync(err))
    }

    /// フレーム境界を疑わせる破れを観測した接続に印を付ける。
    ///
    /// 期限超過・部分転送・切断のあとは、送りかけたフレームや読み残したバイトが
    /// pipe に残る。同じ接続で次の要求を送ると境界がずれたまま解釈され、
    /// 接続が壊れているのに framing や schema の誤りとして報告されてしまう。
    ///
    /// 読めない本文も同じく扱う。envelope・`result` のどちらであっても、本文を
    /// 解釈できないことと境界を取り違えたことは区別が付かない。`request_id` の
    /// 不一致は他の交換に属するフレームを読んだという最も明確な desync である。
    ///
    /// `instance_id` や `protocol_version` の不一致では印を付けない。フレームは 1 つ
    /// 正しく読めており、相手が別のインスタンスであるか互換しない版であることを
    /// 示すだけで、境界は保たれている。
    fn poison_on_desync(&self, err: PipeClientError) -> PipeClientError {
        if matches!(
            err,
            PipeClientError::Timeout
                | PipeClientError::Io(_)
                | PipeClientError::Framing
                | PipeClientError::Json
                | PipeClientError::InvalidResponse
        ) {
            warn!("connection lost frame alignment; refusing further requests");
            self.desynced.set(true);
        }
        err
    }

    /// client 側 handshake を実行する。
    ///
    /// 接続先が名乗るプロトコルバージョンは [`ProtocolVersion::CURRENT`] との
    /// 完全一致を求め、異なる版を名乗る相手は接続ごと拒否する。
    fn handshake(
        &self,
        descriptor_id: InstanceId,
        descriptor_pid: u32,
        descriptor_created_at: &str,
        auth_secret: &AuthSecret,
        deadline: Instant,
    ) -> Result<(), PipeClientError> {
        let client_nonce = Nonce::generate();
        let m1 = ClientHello {
            protocol_version: ProtocolVersion::CURRENT,
            instance_id: descriptor_id,
            client_nonce: client_nonce.clone(),
        };
        let m1_body = serde_json::to_vec(&m1).map_err(|_| PipeClientError::Json)?;
        self.write_frame(&m1_body, deadline)?;

        let m2_body = self.read_frame(deadline)?;
        let m2: ServerAuth = deserialize_json(&m2_body).map_err(|_| PipeClientError::Json)?;

        trace!("received server auth");

        if m2.protocol_version != ProtocolVersion::CURRENT {
            warn!("protocol version mismatch in handshake");
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
            &m2.protocol_version,
        );
        if !verify_mac(&expected_server_mac, &m2.server_mac) {
            warn!("server_mac verification failed");
            return Err(PipeClientError::AuthenticationFailed);
        }

        let client_mac =
            compute_client_mac(auth_secret.as_bytes(), &m2.server_nonce, &client_nonce);
        let m3 = ClientAuth { client_mac };
        let m3_body = serde_json::to_vec(&m3).map_err(|_| PipeClientError::Json)?;
        self.write_frame(&m3_body, deadline)?;

        debug!(protocol_version = %m2.protocol_version.as_str(), "handshake succeeded");
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

/// 成功応答の `result` を型付きで読み取る。
///
/// いったんバイト列へ戻して [`deserialize_json`] を通し、応答の読み取りを
/// 重複 key と非有限数を拒否する経路へ統一する。
///
/// 本関数は接続に触れない。接続の毒化は接続を持つ側、すなわち
/// [`PipeClient::exchange`] と [`PipeClient::decode_result`] が行う。
fn decode_result_value<R: DeserializeOwned>(
    result: &serde_json::Value,
) -> Result<R, PipeClientError> {
    let bytes = serde_json::to_vec(result).map_err(|_| PipeClientError::Json)?;
    deserialize_json(&bytes).map_err(|_| PipeClientError::InvalidResponse)
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
///
/// 接続先の pipe は同時 1 接続しか受け付けないため、先行する要求が処理中の間は
/// 待受インスタンスが無く接続に失敗する。1 回の待ちは [`CONNECT_WAIT_CAP_MS`] で
/// 頭打ちにするので、そこで諦めると解決の予算が余ったまま到達不能な相手として
/// 扱ってしまう。handshake と ping の取り分を残した期限まで待ち直す。
///
/// 再試行するのは pipe が存在して塞がっている場合だけである。pipe そのものが
/// 無い相手は待っても現れないため、即座に失敗として返す。
fn connect_pipe(pipe_name: &str, deadline: Instant) -> Result<HANDLE, PipeClientError> {
    let wide: Vec<u16> = OsStr::new(pipe_name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    loop {
        let remaining = win_io::remaining_until(deadline);
        if remaining <= CONNECT_RESERVE {
            return Err(PipeClientError::Timeout);
        }

        // pipe サーバーが接続可能になるまで短時間待つ。
        // 第 2 引数の 0 は NMPWAIT_NOWAIT ではなく NMPWAIT_USE_DEFAULT_WAIT（pipe 既定の
        // タイムアウト）を意味するため、残り時間が 1 ミリ秒未満でも 0 を渡さない。
        let wait_ms = (remaining - CONNECT_RESERVE)
            .as_millis()
            .clamp(1, CONNECT_WAIT_CAP_MS) as u32;
        // SAFETY: `wide` は NUL 終端した pipe 名であり、呼び出し中は生存している。
        unsafe {
            let _ = WaitNamedPipeW(PCWSTR(wide.as_ptr()), wait_ms);
        }

        // SAFETY: `wide` は NUL 終端した pipe 名であり、呼び出し中は生存している。
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        };

        match handle {
            Ok(h) if h != INVALID_HANDLE_VALUE => return Ok(h),
            Ok(_) => return Err(PipeClientError::ConnectFailed),
            // 待受インスタンスが全て塞がっている。期限まで待ち直す。
            Err(e) if e.code() == ERROR_PIPE_BUSY.into() => continue,
            Err(_) => return Err(PipeClientError::ConnectFailed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::{InstanceId, InstanceState, ResponseKind, SERVER_RESOLVE_BUDGET};
    use serde::Deserialize;
    use std::time::Duration;
    use windows::Win32::Storage::FileSystem::{PIPE_ACCESS_DUPLEX, ReadFile, WriteFile};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
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

    #[test]
    fn connect_fails_immediately_when_the_pipe_does_not_exist() {
        // 待っても現れない相手に予算を使い切らない。
        let id = InstanceId::new_v4();
        let started = Instant::now();
        let result = connect_pipe(&pipe_name_for(&id), started + SERVER_RESOLVE_BUDGET);
        assert!(
            matches!(result, Err(PipeClientError::ConnectFailed)),
            "実際の結果: {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "存在しない pipe を {}ms 待っています",
            started.elapsed().as_millis()
        );
    }

    /// 待受インスタンスを 1 本だけ持つ pipe を作る。
    fn create_single_instance_pipe(name: &str) -> HANDLE {
        let wide: Vec<u16> = OsStr::new(name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` は NUL 終端した pipe 名であり、呼び出し中は生存している。
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4 * 1024,
                4 * 1024,
                0,
                None,
            )
        };
        assert!(!handle.is_invalid(), "テスト用 pipe の作成に失敗しました");
        handle
    }

    /// 接続待ちの上限を超えて塞がっている pipe を表す。
    struct BusyPipe {
        server: HANDLE,
        occupier: HANDLE,
    }

    // SAFETY: pipe ハンドルはスレッドを跨いで使用でき、所有権は移動しない。
    unsafe impl Send for BusyPipe {}

    #[test]
    fn connect_retries_while_every_pipe_instance_is_busy() {
        // 接続先は同時 1 接続しか受け付けない。先行する要求が処理中の間は
        // 待受が無く、1 回の待ちの上限を超えて塞がることが設計上あり得る。
        // そこで諦めると、生きている相手を到達不能として扱ってしまう。
        let name = format!(r"\\.\pipe\aviutl2-mcp-busy-test-{}", InstanceId::new_v4());
        let server = create_single_instance_pipe(&name);

        let wide: Vec<u16> = OsStr::new(&name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` は NUL 終端した pipe 名であり、呼び出し中は生存している。
        let occupier = unsafe {
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
        .expect("唯一の待受インスタンスを占有できる");
        // 接続済みのため `ConnectNamedPipe` は待たずに戻る。
        // SAFETY: `server` は直前に作成した有効な pipe ハンドル。
        let _ = unsafe { ConnectNamedPipe(server, None) };

        // 1 回の待ちの上限を超えて塞ぎ、その後に待受を張り直す。
        let busy_for = SERVER_CONNECT_WAIT_CAP + Duration::from_millis(300);
        let pipe = BusyPipe { server, occupier };
        let releaser = std::thread::spawn(move || {
            let pipe = pipe;
            std::thread::sleep(busy_for);
            // SAFETY: いずれも本テストが所有する有効なハンドル。
            unsafe {
                let _ = CloseHandle(pipe.occupier);
                let _ = DisconnectNamedPipe(pipe.server);
                // 次の接続を受け入れる。接続が来るまで戻らない。
                let _ = ConnectNamedPipe(pipe.server, None);
            }
        });

        let started = Instant::now();
        let handle = connect_pipe(&name, started + SERVER_RESOLVE_BUDGET)
            .expect("塞がっていた pipe へ再試行で接続できる");
        assert!(
            started.elapsed() >= SERVER_CONNECT_WAIT_CAP,
            "1 回の待ちで接続できており、再試行を確かめられていません"
        );

        releaser.join().expect("待受の張り直しが完了する");
        // SAFETY: いずれも本テストが所有する有効なハンドル。
        unsafe {
            let _ = CloseHandle(handle);
            let _ = CloseHandle(server);
        }
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
                desynced: Cell::new(false),
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

    /// 対向端で 1 要求を受け取り、組み立てた応答本文を返すスレッドを起こす。
    ///
    /// join すると受信した要求が得られる。
    fn respond_once(
        peer: &MockPeer,
        make_response: impl FnOnce(&RequestEnvelope) -> Vec<u8> + Send + 'static,
    ) -> std::thread::JoinHandle<RequestEnvelope> {
        let handle = SendHandle(peer.raw());
        std::thread::spawn(move || {
            let handle = handle;
            let body = recv_frame(handle.0);
            let request: RequestEnvelope = serde_json::from_slice(&body).unwrap();
            let response = make_response(&request);
            send_bytes(handle.0, &encode_frame(&response).unwrap());
            request
        })
    }

    /// 要求に対応する応答 Envelope を組み立てる。
    fn response_for(
        request: &RequestEnvelope,
        instance_id: InstanceId,
        result: ResponseResult,
    ) -> ResponseEnvelope {
        ResponseEnvelope {
            kind: ResponseKind::Response,
            protocol_version: request.protocol_version,
            request_id: request.request_id,
            instance_id,
            result,
        }
    }

    fn test_deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn request_sends_operation_and_returns_result() {
        let (peer, client) = MockPeer::connected();
        let instance_id = client.instance_id;
        let result = serde_json::json!({ "items": [1, 2, 3], "page": { "total_count": 3 } });

        let payload = result.clone();
        let responder = respond_once(&peer, move |request| {
            let response =
                response_for(request, instance_id, ResponseResult::Ok { result: payload });
            serde_json::to_vec(&response).unwrap()
        });

        let params = serde_json::json!({ "expected_scene_id": 0, "offset": 0 });
        let received = client
            .request("list_layers", params.clone(), test_deadline())
            .expect("要求が成功する");
        assert_eq!(received, result, "result はそのまま返る");

        let request = responder.join().unwrap();
        assert_eq!(request.operation, "list_layers");
        assert_eq!(request.params, params);
        assert_eq!(request.instance_id, instance_id);
        assert!(request.deadline_unix_ms.is_some(), "要求に期限が設定される");
    }

    #[test]
    fn request_preserves_remote_error_object() {
        let (peer, client) = MockPeer::connected();
        let instance_id = client.instance_id;
        let error = ErrorObject::new(ErrorCode::PreconditionFailed, "scene が変化しました", true)
            .with_details(serde_json::json!({ "current_project_revision": 12 }))
            .with_correlation_id("0190abcd-1234-7def-1234-567890abcdef");

        let payload = error.clone();
        let responder = respond_once(&peer, move |request| {
            let response =
                response_for(request, instance_id, ResponseResult::Err { error: payload });
            serde_json::to_vec(&response).unwrap()
        });

        let failure = client
            .request("get_object", serde_json::json!({}), test_deadline())
            .expect_err("エラー応答は失敗として返る");
        responder.join().unwrap();

        let PipeClientError::Remote(remote) = &failure else {
            panic!("ErrorObject が失われています: {failure:?}");
        };
        assert_eq!(
            **remote, error,
            "code/message/retryable/details/correlation_id が保たれる"
        );
        assert_eq!(failure.error_code(), ErrorCode::PreconditionFailed);
    }

    #[test]
    fn request_rejects_request_id_mismatch() {
        let (peer, client) = MockPeer::connected();
        let instance_id = client.instance_id;

        let responder = respond_once(&peer, move |request| {
            let mut response = response_for(
                request,
                instance_id,
                ResponseResult::Ok {
                    result: serde_json::json!({}),
                },
            );
            response.request_id = RequestId::new();
            serde_json::to_vec(&response).unwrap()
        });

        let failure = client
            .request("get_edit_info", serde_json::json!({}), test_deadline())
            .expect_err("request_id 不一致は拒否される");
        responder.join().unwrap();
        assert!(
            matches!(failure, PipeClientError::InvalidResponse),
            "実際のエラー: {failure:?}"
        );

        // 他の交換に属するフレームを読んでおり、以降の応答は 1 つずつずれる。
        assert_rejects_without_io(&client);
    }

    #[test]
    fn request_rejects_instance_id_mismatch() {
        let (peer, client) = MockPeer::connected();

        let responder = respond_once(&peer, move |request| {
            let response = response_for(
                request,
                InstanceId::new_v4(),
                ResponseResult::Ok {
                    result: serde_json::json!({}),
                },
            );
            serde_json::to_vec(&response).unwrap()
        });

        let failure = client
            .request("get_edit_info", serde_json::json!({}), test_deadline())
            .expect_err("instance_id 不一致は拒否される");
        responder.join().unwrap();
        assert!(
            matches!(failure, PipeClientError::InstanceStale),
            "実際のエラー: {failure:?}"
        );
    }

    /// 応答が名乗る版は現行版との完全一致を求める。MINOR だけの差も拒否する。
    #[test]
    fn request_rejects_protocol_version_mismatch() {
        for version in mismatched_versions() {
            let (peer, client) = MockPeer::connected();
            let instance_id = client.instance_id;

            let responder = respond_once(&peer, move |request| {
                let mut response = response_for(
                    request,
                    instance_id,
                    ResponseResult::Ok {
                        result: serde_json::json!({}),
                    },
                );
                response.protocol_version = version;
                serde_json::to_vec(&response).unwrap()
            });

            let failure = client
                .request("get_edit_info", serde_json::json!({}), test_deadline())
                .expect_err("版が異なる応答は拒否される");
            responder.join().unwrap();
            assert!(
                matches!(failure, PipeClientError::ProtocolMismatch),
                "{} の応答が受理されました: {failure:?}",
                version.as_str()
            );
            assert_eq!(failure.error_code(), ErrorCode::ProtocolMismatch);
        }
    }

    /// 現行版と一致しないプロトコルバージョン。
    fn mismatched_versions() -> [ProtocolVersion; 3] {
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
            ProtocolVersion {
                major: current.major.saturating_sub(1),
                minor: current.minor,
            },
        ]
    }

    /// handshake の M2 を演じ、client 側の版検証の結果を返す。
    fn handshake_against_advertised_version(
        advertised: ProtocolVersion,
    ) -> Result<(), PipeClientError> {
        const PID: u32 = 4321;
        const CREATED_AT: &str = "2026-01-01T00:00:00.0000000Z";

        let (peer, client) = MockPeer::connected();
        let instance_id = client.instance_id;
        let auth_secret = AuthSecret::from_bytes([0x5A; 32]);

        let handle = SendHandle(peer.raw());
        let secret = *auth_secret.as_bytes();
        let responder = std::thread::spawn(move || {
            let handle = handle;
            let hello: ClientHello = serde_json::from_slice(&recv_frame(handle.0)).unwrap();
            let server_nonce = Nonce::generate();
            let server_mac = compute_server_mac(
                &secret,
                &hello.client_nonce,
                &server_nonce,
                &instance_id,
                &advertised,
            );
            let m2 = ServerAuth {
                protocol_version: advertised,
                instance_id,
                server_nonce,
                pid: PID,
                process_created_at: CREATED_AT.to_string(),
                server_mac,
            };
            send_bytes(
                handle.0,
                &encode_frame(&serde_json::to_vec(&m2).unwrap()).unwrap(),
            );
        });

        let result = client.handshake(
            instance_id,
            PID,
            CREATED_AT,
            &auth_secret,
            Instant::now() + Duration::from_secs(5),
        );
        responder.join().unwrap();
        result
    }

    #[test]
    fn handshake_accepts_the_current_version() {
        handshake_against_advertised_version(ProtocolVersion::CURRENT)
            .expect("現行版を名乗る接続先とは handshake が成立する");
    }

    /// handshake は版の交渉を行わず、異なる版を名乗る接続先を拒否する。
    #[test]
    fn handshake_rejects_protocol_version_mismatch() {
        for version in mismatched_versions() {
            let failure = handshake_against_advertised_version(version)
                .expect_err("版が異なる接続先とは handshake が成立しない");
            assert!(
                matches!(failure, PipeClientError::ProtocolMismatch),
                "{} を名乗る接続先が受理されました: {failure:?}",
                version.as_str()
            );
            assert_eq!(failure.error_code(), ErrorCode::ProtocolMismatch);
        }
    }

    #[test]
    fn request_times_out_without_response() {
        let (_peer, client) = MockPeer::connected();

        let started = Instant::now();
        let failure = client
            .request(
                "get_edit_info",
                serde_json::json!({}),
                started + Duration::from_millis(200),
            )
            .expect_err("応答が無ければ期限を超過する");
        assert!(
            matches!(failure, PipeClientError::Timeout),
            "実際のエラー: {failure:?}"
        );
        assert_eq!(failure.error_code(), ErrorCode::Timeout);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "期限を大きく超えて待っています: {}ms",
            started.elapsed().as_millis()
        );
    }

    /// 毒化した接続が I/O を出さずに要求を拒否することを確認する。
    ///
    /// 期限を十分先に置くため、拒否せず送受信していれば期限まで待ち続ける。
    fn assert_rejects_without_io(client: &PipeClient) {
        let started = Instant::now();
        let failure = client
            .request(
                "get_edit_info",
                serde_json::json!({}),
                started + Duration::from_secs(10),
            )
            .expect_err("境界を見失った接続は再利用できない");
        assert!(
            matches!(failure, PipeClientError::Desynced),
            "実際のエラー: {failure:?}"
        );
        assert_eq!(failure.error_code(), ErrorCode::InstanceStale);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "pipe への I/O が発生しています: {}ms",
            started.elapsed().as_millis()
        );
    }

    #[test]
    fn timed_out_request_poisons_connection() {
        let (_peer, client) = MockPeer::connected();

        // 応答が無いまま期限を超過させ、読み残しがある状態を作る。
        let failure = client
            .request(
                "get_edit_info",
                serde_json::json!({}),
                Instant::now() + Duration::from_millis(200),
            )
            .expect_err("応答が無ければ期限を超過する");
        assert!(
            matches!(failure, PipeClientError::Timeout),
            "実際のエラー: {failure:?}"
        );

        assert_rejects_without_io(&client);
    }

    #[test]
    fn framing_error_poisons_connection() {
        let (peer, client) = MockPeer::connected();
        // 本体を伴わない不正なフレーム長を送り、境界を見失わせる。
        peer.send(&0u32.to_le_bytes());

        let failure = client
            .request(
                "get_edit_info",
                serde_json::json!({}),
                Instant::now() + Duration::from_secs(5),
            )
            .expect_err("不正なフレーム長は拒否される");
        assert!(
            matches!(failure, PipeClientError::Framing),
            "実際のエラー: {failure:?}"
        );

        assert_rejects_without_io(&client);
    }

    #[test]
    fn malformed_response_body_poisons_connection() {
        let (peer, client) = MockPeer::connected();
        let instance_id = client.instance_id;

        let responder = respond_once(&peer, move |request| {
            let response = response_for(
                request,
                instance_id,
                ResponseResult::Ok {
                    result: serde_json::json!({}),
                },
            );
            // 末尾の `}` の直前に既出の key を足して重複させる。
            let serialized = serde_json::to_string(&response).unwrap();
            format!("{},\"ok\":true{}", &serialized[..serialized.len() - 1], "}").into_bytes()
        });

        let failure = client
            .request(
                "get_edit_info",
                serde_json::json!({}),
                Instant::now() + Duration::from_secs(5),
            )
            .expect_err("重複 JSON key を含む応答は拒否される");
        responder.join().unwrap();
        assert!(
            matches!(failure, PipeClientError::Json),
            "実際のエラー: {failure:?}"
        );

        assert_rejects_without_io(&client);
    }

    #[test]
    fn remote_error_keeps_connection_usable() {
        let (peer, client) = MockPeer::connected();
        let instance_id = client.instance_id;

        let responder = respond_once(&peer, move |request| {
            let response = response_for(
                request,
                instance_id,
                ResponseResult::Err {
                    error: ErrorObject::new(ErrorCode::NotFound, "見つかりません", false),
                },
            );
            serde_json::to_vec(&response).unwrap()
        });

        let failure = client
            .request("get_object", serde_json::json!({}), test_deadline())
            .expect_err("エラー応答は失敗として返る");
        responder.join().unwrap();
        assert!(matches!(failure, PipeClientError::Remote(_)));

        // 契約どおりのエラー応答は境界を壊さないため、接続は使い続けられる。
        let result = serde_json::json!({ "ok": 1 });
        let payload = result.clone();
        let responder = respond_once(&peer, move |request| {
            let response =
                response_for(request, instance_id, ResponseResult::Ok { result: payload });
            serde_json::to_vec(&response).unwrap()
        });
        assert_eq!(
            client
                .request("get_edit_info", serde_json::json!({}), test_deadline())
                .expect("エラー応答の後も要求を送れる"),
            result
        );
        responder.join().unwrap();
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct SampleResult {
        value: u32,
    }

    #[test]
    fn request_typed_decodes_result() {
        let (peer, client) = MockPeer::connected();
        let instance_id = client.instance_id;

        let responder = respond_once(&peer, move |request| {
            let response = response_for(
                request,
                instance_id,
                ResponseResult::Ok {
                    result: serde_json::json!({ "value": 5, "future": 1 }),
                },
            );
            serde_json::to_vec(&response).unwrap()
        });

        let params = serde_json::json!({ "expected_scene_id": 0 });
        let decoded: SampleResult = client
            .request_typed("get_current_scene", &params, test_deadline())
            .expect("型付きの result を読み取れる");
        assert_eq!(decoded, SampleResult { value: 5 });

        let request = responder.join().unwrap();
        assert_eq!(request.params, params);
    }

    #[test]
    fn request_typed_rejects_result_of_wrong_shape() {
        let (peer, client) = MockPeer::connected();
        let instance_id = client.instance_id;

        let responder = respond_once(&peer, move |request| {
            let response = response_for(
                request,
                instance_id,
                ResponseResult::Ok {
                    result: serde_json::json!({ "value": "not a number" }),
                },
            );
            serde_json::to_vec(&response).unwrap()
        });

        let failure = client
            .request_typed::<_, SampleResult>(
                "get_current_scene",
                &serde_json::json!({}),
                test_deadline(),
            )
            .expect_err("型が合わない result は拒否される");
        responder.join().unwrap();
        assert!(
            matches!(failure, PipeClientError::InvalidResponse),
            "実際のエラー: {failure:?}"
        );

        // envelope が読めても result が契約から外れていれば、相手が契約どおりに
        // 応答していないことに変わりはない。
        assert_rejects_without_io(&client);
    }

    #[test]
    fn ping_result_of_wrong_shape_poisons_connection() {
        let (peer, client) = MockPeer::connected();
        let instance_id = client.instance_id;

        let responder = respond_once(&peer, move |request| {
            let response = response_for(
                request,
                instance_id,
                ResponseResult::Ok {
                    result: serde_json::json!({ "state": 1 }),
                },
            );
            serde_json::to_vec(&response).unwrap()
        });

        let failure = client
            .ping(test_deadline())
            .expect_err("型が合わない ping の result は拒否される");
        responder.join().unwrap();
        assert!(
            matches!(failure, PipeClientError::InvalidResponse),
            "実際のエラー: {failure:?}"
        );

        assert_rejects_without_io(&client);
    }

    #[test]
    fn ping_preserves_remote_error() {
        let (peer, client) = MockPeer::connected();
        let instance_id = client.instance_id;
        let error = ErrorObject::new(ErrorCode::HostBusy, "起動中です", true)
            .with_details(serde_json::json!({ "retry_after_ms": 500 }));

        let payload = error.clone();
        let responder = respond_once(&peer, move |request| {
            let response =
                response_for(request, instance_id, ResponseResult::Err { error: payload });
            serde_json::to_vec(&response).unwrap()
        });

        let failure = client
            .ping(test_deadline())
            .expect_err("エラー応答の ping は失敗する");
        responder.join().unwrap();

        let PipeClientError::Remote(remote) = &failure else {
            panic!("ping の拒否理由が失われています: {failure:?}");
        };
        assert_eq!(**remote, error);
        assert_eq!(failure.error_code(), ErrorCode::HostBusy);
    }

    #[test]
    fn ping_rejects_result_with_other_instance_id() {
        let (peer, client) = MockPeer::connected();
        let instance_id = client.instance_id;

        let responder = respond_once(&peer, move |request| {
            let response = response_for(
                request,
                instance_id,
                ResponseResult::Ok {
                    result: serde_json::json!({
                        "state": "ready",
                        "instance_id": InstanceId::new_v4(),
                    }),
                },
            );
            serde_json::to_vec(&response).unwrap()
        });

        let failure = client
            .ping(test_deadline())
            .expect_err("result の instance_id 不一致は拒否される");
        responder.join().unwrap();
        assert!(
            matches!(failure, PipeClientError::InstanceStale),
            "実際のエラー: {failure:?}"
        );
    }

    #[test]
    fn error_code_matches_failure_kind() {
        let cases = [
            (PipeClientError::ConnectFailed, ErrorCode::InstanceStale),
            (
                PipeClientError::Io(io::Error::from(io::ErrorKind::BrokenPipe)),
                ErrorCode::InstanceStale,
            ),
            (PipeClientError::Framing, ErrorCode::InstanceStale),
            (PipeClientError::Json, ErrorCode::InstanceStale),
            (PipeClientError::InvalidResponse, ErrorCode::InstanceStale),
            (PipeClientError::Desynced, ErrorCode::InstanceStale),
            (PipeClientError::InstanceStale, ErrorCode::InstanceStale),
            (PipeClientError::Timeout, ErrorCode::Timeout),
            (
                PipeClientError::AuthenticationFailed,
                ErrorCode::AuthenticationFailed,
            ),
            (
                PipeClientError::ProtocolMismatch,
                ErrorCode::ProtocolMismatch,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.error_code(), expected, "{error:?} の対応が誤り");
        }

        // 接続先が返したコードはそのまま採用する。
        for code in [
            ErrorCode::EditBlocked,
            ErrorCode::NotFound,
            ErrorCode::UnsupportedOperation,
            ErrorCode::Unknown("future_code".to_string()),
        ] {
            let error = PipeClientError::Remote(Box::new(ErrorObject::new(
                code.clone(),
                "message",
                code.default_retryable(),
            )));
            assert_eq!(error.error_code(), code);
        }
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
                &PongResult::new(instance_id, InstanceState::Ready),
            );
            send_bytes(
                handle.0,
                &encode_frame(&serde_json::to_vec(&response).unwrap()).unwrap(),
            );
            request
        });

        let before = Utc::now().timestamp_millis() as u64;
        let pong = client
            .ping(Instant::now() + Duration::from_secs(5))
            .expect("ping に失敗しました");
        assert_eq!(pong.state, InstanceState::Ready);
        // 接続先が載せなかった値は欠落のままにする。
        assert_eq!(pong.project, None);

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
