//! 統合テストが共有する mock pipe server。
//!
//! 複数のテストターゲットから読み込まれるため、片方でのみ使う項目がある。
#![allow(dead_code)]

use aviutl2_mcp_core::{
    AuthSecret, ClientAuth, ClientHello, DescriptorProject, ErrorCode, ErrorObject,
    InstanceDescriptor, InstanceId, InstanceState, Nonce, PongProject, PongResult, ProtocolVersion,
    RequestEnvelope, ResponseEnvelope, ResponseKind, ResponseResult, ServerAuth,
    compute_client_mac, compute_server_mac, encode_frame, format_utc_timestamp, pipe_name_for,
    verify_mac,
};
use aviutl2_mcp_server::win_io::{self, EventHandle, IoIssue, OverlappedOp, WaitAnyOutcome};
use std::collections::HashMap;
use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
};
use windows::core::PCWSTR;

/// mock server が 1 回の read/write に許す時間。
pub const IO_TIMEOUT: Duration = Duration::from_secs(2);

pub fn io_deadline() -> Instant {
    Instant::now() + IO_TIMEOUT
}

pub struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}

/// operation 名から応答内容への対応表。
///
/// テストはここへ応答を注入し、mock server に read operation を演じさせる。
/// `ping` を含む任意の operation を差し替えられる。表に無い operation は、
/// `ping` なら生存応答、それ以外は `unsupported_operation` のエラー応答になる。
pub type OperationResponses = HashMap<String, ResponseResult>;

/// mock server の ping 応答が運ぶプロジェクトの状態。
pub const MOCK_PROJECT_EPOCH: &str = "78be92d1-c8c9-44c6-ae52-387548971468";
pub const MOCK_PROJECT_REVISION: u64 = 42;
pub const MOCK_PROJECT_MODIFIED: bool = true;

/// mock server が ping 応答へ載せるプロジェクトの状態。
pub fn mock_project() -> PongProject {
    PongProject {
        epoch: MOCK_PROJECT_EPOCH.to_string(),
        revision: MOCK_PROJECT_REVISION,
        modified: MOCK_PROJECT_MODIFIED,
    }
}

/// 成功応答を組み立てる。
pub fn ok_result(value: serde_json::Value) -> ResponseResult {
    ResponseResult::Ok { result: value }
}

/// エラー応答を組み立てる。
pub fn err_result(error: ErrorObject) -> ResponseResult {
    ResponseResult::Err { error }
}

/// mock server が受け取った要求の記録。
pub type ReceivedRequests = Arc<Mutex<Vec<RequestEnvelope>>>;

/// mock server がクライアントへ提示する identity と応答。
pub struct MockBehavior {
    instance_id: InstanceId,
    auth_secret: AuthSecret,
    pid: u32,
    process_created_at: String,
    state: InstanceState,
    responses: OperationResponses,
    response_delay: Duration,
    received: ReceivedRequests,
}

pub struct MockPipeServer {
    instance_id: InstanceId,
    auth_secret: AuthSecret,
    pid: u32,
    process_created_at: String,
    state: InstanceState,
    handle: SendHandle,
    stop_event: EventHandle,
    received: ReceivedRequests,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MockPipeServer {
    pub fn start(
        instance_id: InstanceId,
        auth_secret: AuthSecret,
        pid: u32,
        process_created_at: String,
        state: InstanceState,
    ) -> Self {
        Self::start_with_operations(
            instance_id,
            auth_secret,
            pid,
            process_created_at,
            state,
            OperationResponses::new(),
        )
    }

    pub fn start_with_operations(
        instance_id: InstanceId,
        auth_secret: AuthSecret,
        pid: u32,
        process_created_at: String,
        state: InstanceState,
        responses: OperationResponses,
    ) -> Self {
        Self::start_with_delayed_operations(
            instance_id,
            auth_secret,
            pid,
            process_created_at,
            state,
            responses,
            Duration::ZERO,
        )
    }

    /// read operation の応答を `response_delay` だけ遅らせて起動する。
    ///
    /// pipe は同時 1 接続しか受け付けないため、この遅延の間は接続が塞がる。
    /// 実行中の要求と後続の接続が重なる状況を作るために使う。生存確認の `ping`
    /// には適用せず、接続の確立そのものは遅らせない。
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_delayed_operations(
        instance_id: InstanceId,
        auth_secret: AuthSecret,
        pid: u32,
        process_created_at: String,
        state: InstanceState,
        responses: OperationResponses,
        response_delay: Duration,
    ) -> Self {
        let pipe_name = pipe_name_for(&instance_id);
        let wide: Vec<u16> = OsStr::new(&pipe_name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                // 期限付き I/O と停止イベントによる待機打ち切りには overlapped が必須。
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                0,
                None,
            )
        };
        if handle.is_invalid() {
            panic!("mock pipe を作成できませんでした");
        }

        let stop_event = EventHandle::new_manual_reset().unwrap();
        let handle_raw = handle.0 as usize;
        let stop_event_raw = stop_event.handle().0 as usize;
        let received: ReceivedRequests = Arc::new(Mutex::new(Vec::new()));
        let behavior = MockBehavior {
            instance_id,
            auth_secret: auth_secret.clone(),
            pid,
            process_created_at: process_created_at.clone(),
            state: state.clone(),
            responses,
            response_delay,
            received: Arc::clone(&received),
        };

        let thread = std::thread::spawn(move || {
            server_loop(
                HANDLE(handle_raw as *mut c_void),
                behavior,
                HANDLE(stop_event_raw as *mut c_void),
            );
        });

        Self {
            instance_id,
            auth_secret,
            pid,
            process_created_at,
            state,
            handle: SendHandle(handle),
            stop_event,
            received,
            thread: Some(thread),
        }
    }

    /// これまでに受け取った要求を古い順に返す。
    pub fn received_requests(&self) -> Vec<RequestEnvelope> {
        self.received
            .lock()
            .expect("received のロックは毒化しない")
            .clone()
    }

    /// この mock の instance_id。
    pub fn instance_id(&self) -> InstanceId {
        self.instance_id
    }

    pub fn descriptor(&self, _registry_dir: PathBuf) -> InstanceDescriptor {
        InstanceDescriptor {
            schema_version: 1,
            protocol_version: ProtocolVersion::CURRENT,
            instance_id: self.instance_id,
            pipe_name: pipe_name_for(&self.instance_id),
            auth_secret: self.auth_secret.clone(),
            pid: self.pid,
            process_created_at: self.process_created_at.clone(),
            hwnd: None,
            started_at: self.process_created_at.clone(),
            state: self.state.clone(),
            project: Some(DescriptorProject {
                display_name: "Mock Project".to_string(),
                path: r"C:\mock.aup".to_string(),
            }),
        }
    }

    pub fn write_descriptor(&self, registry_dir: &std::path::Path) {
        self.write_descriptor_value(registry_dir, self.descriptor(registry_dir.to_path_buf()));
    }

    /// project を持たない descriptor を registry へ書く。
    ///
    /// descriptor へ project が載るのはプロジェクトファイルのパスが確定した
    /// ときだけで、未保存のプロジェクトでは載らない。ping 応答だけが状態を
    /// 運ぶ経路を再現するために使う。
    pub fn write_descriptor_without_project(&self, registry_dir: &std::path::Path) {
        let mut descriptor = self.descriptor(registry_dir.to_path_buf());
        descriptor.project = None;
        self.write_descriptor_value(registry_dir, descriptor);
    }

    fn write_descriptor_value(
        &self,
        registry_dir: &std::path::Path,
        descriptor: InstanceDescriptor,
    ) {
        std::fs::create_dir_all(registry_dir).unwrap();
        let path = registry_dir.join(format!("{}.json", self.instance_id));
        std::fs::write(&path, serde_json::to_string(&descriptor).unwrap()).unwrap();
    }
}

/// pipe server を伴わない descriptor を registry へ書き、その `instance_id` を返す。
///
/// PID は自プロセスを指すためプロセス同一性の確認は通るが、pipe は待ち受けていない。
/// 生存確認を伴わない経路と、伴う経路の違いを分けて確かめるために使う。
pub fn write_bare_descriptor(registry_dir: &std::path::Path) -> InstanceId {
    let instance_id = InstanceId::new_v4();
    let created_at = current_process_created_at();
    let descriptor = InstanceDescriptor {
        schema_version: 1,
        protocol_version: ProtocolVersion::CURRENT,
        instance_id,
        pipe_name: pipe_name_for(&instance_id),
        auth_secret: AuthSecret::generate(),
        pid: std::process::id(),
        process_created_at: created_at.clone(),
        hwnd: None,
        started_at: created_at,
        state: InstanceState::Ready,
        project: None,
    };
    std::fs::create_dir_all(registry_dir).unwrap();
    std::fs::write(
        registry_dir.join(format!("{instance_id}.json")),
        serde_json::to_string(&descriptor).unwrap(),
    )
    .unwrap();
    instance_id
}

impl Drop for MockPipeServer {
    fn drop(&mut self) {
        // 停止を通知してスレッドの終了を待ってから pipe を閉じる。
        // スレッドが保留中の I/O をキャンセルし終えるまでハンドルを有効に保つ。
        let _ = self.stop_event.set();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // SAFETY: スレッド終了後はこのハンドルを参照するものが無く、ここでのみ閉じる。
        unsafe {
            let _ = CloseHandle(self.handle.0);
        }
    }
}

/// 接続を受け付け直しながら要求に応答し続ける。
///
/// server は tool call ごとに接続を張り直すため、1 接続で終わらせない。
pub fn server_loop(handle: HANDLE, behavior: MockBehavior, stop_event: HANDLE) {
    loop {
        if !accept_connection(handle, stop_event) {
            return;
        }
        serve_connection(handle, &behavior, stop_event);
        // SAFETY: `handle` は MockPipeServer が所有し、スレッドの join 後にのみ閉じられる。
        unsafe {
            let _ = DisconnectNamedPipe(handle);
        }
        if stop_requested(stop_event) {
            return;
        }
    }
}

/// 停止が要求されているかを待たずに確認する。
fn stop_requested(stop_event: HANDLE) -> bool {
    matches!(
        win_io::wait_any(
            &[stop_event],
            Some(Instant::now() + Duration::from_millis(1))
        ),
        WaitAnyOutcome::Signaled(0)
    )
}

/// 1 接続分の handshake と要求ループを処理する。
fn serve_connection(handle: HANDLE, behavior: &MockBehavior, stop_event: HANDLE) {
    // M1 受信。
    let m1_body = match read_frame(handle, io_deadline()) {
        Some(body) => body,
        None => return,
    };
    let m1: ClientHello = serde_json::from_slice(&m1_body).unwrap();
    assert_eq!(m1.instance_id, behavior.instance_id);

    assert_eq!(m1.protocol_version, ProtocolVersion::CURRENT);

    let server_nonce = Nonce::generate();
    let server_mac = compute_server_mac(
        behavior.auth_secret.as_bytes(),
        &m1.client_nonce,
        &server_nonce,
        &behavior.instance_id,
        &ProtocolVersion::CURRENT,
    );

    let m2 = ServerAuth {
        protocol_version: ProtocolVersion::CURRENT,
        instance_id: behavior.instance_id,
        server_nonce,
        pid: behavior.pid,
        process_created_at: behavior.process_created_at.clone(),
        server_mac,
    };
    let m2_body = serde_json::to_vec(&m2).unwrap();
    if write_frame(handle, &m2_body, io_deadline()).is_err() {
        return;
    }

    // M3 受信。
    let m3_body = match read_frame(handle, io_deadline()) {
        Some(body) => body,
        None => return,
    };
    let m3: ClientAuth = serde_json::from_slice(&m3_body).unwrap();
    let expected_client_mac = compute_client_mac(
        behavior.auth_secret.as_bytes(),
        &m2.server_nonce,
        &m1.client_nonce,
    );
    assert!(verify_mac(&expected_client_mac, &m3.client_mac));

    // 要求ループ。停止要求・切断・EOF・読み取り失敗のいずれでも抜ける。
    loop {
        let Some(body) = read_frame_until_stop(handle, stop_event) else {
            return;
        };
        let Ok(request) = serde_json::from_slice::<RequestEnvelope>(&body) else {
            return;
        };
        behavior
            .received
            .lock()
            .expect("received のロックは毒化しない")
            .push(request.clone());
        if request.operation != "ping" {
            std::thread::sleep(behavior.response_delay);
        }
        let response = build_response(&request, behavior);
        let response_body = serde_json::to_vec(&response).unwrap();
        if write_frame(handle, &response_body, io_deadline()).is_err() {
            return;
        }
    }
}

/// 要求の operation に応じた応答を組み立てる。
pub fn build_response(request: &RequestEnvelope, behavior: &MockBehavior) -> ResponseEnvelope {
    let result = match behavior.responses.get(&request.operation) {
        Some(result) => result.clone(),
        None if request.operation == "ping" => {
            return ResponseEnvelope::pong(
                ProtocolVersion::CURRENT,
                request.request_id,
                &PongResult::new(behavior.instance_id, behavior.state.clone())
                    .with_project(mock_project()),
            );
        }
        None => err_result(ErrorObject::new(
            ErrorCode::UnsupportedOperation,
            "未対応の operation です",
            false,
        )),
    };

    ResponseEnvelope {
        kind: ResponseKind::Response,
        protocol_version: ProtocolVersion::CURRENT,
        request_id: request.request_id,
        instance_id: behavior.instance_id,
        result,
    }
}

/// クライアントの接続を待つ。停止要求で待機を打ち切った場合は `false` を返す。
pub fn accept_connection(handle: HANDLE, stop_event: HANDLE) -> bool {
    // SAFETY: `handle` は MockPipeServer が所有し、スレッドの join 後にのみ閉じられる。
    // `op` は本関数のスコープを出るときに drop されるため handle より長生きしない。
    let mut op = unsafe { OverlappedOp::new(handle) }.unwrap();
    if op.begin().is_err() {
        return false;
    }
    // SAFETY: `handle` は overlapped 用に作成した有効な pipe であり、`op` は
    // 接続完了まで生存して保留 I/O の後始末を行う。
    let result = unsafe { ConnectNamedPipe(handle, Some(op.as_mut_ptr())) };
    // ConnectNamedPipe 発行前にクライアントが接続していた場合は接続済みとして扱う。
    let result = match result {
        Err(err) if err.code() == ERROR_PIPE_CONNECTED.into() => Ok(()),
        other => other,
    };
    match op.classify(result) {
        Ok(IoIssue::Completed) => true,
        Ok(IoIssue::Pending) => {
            match win_io::wait_any(&[op.event(), stop_event], None) {
                WaitAnyOutcome::Signaled(0) => op.await_completion(io_deadline()).is_ok(),
                // 停止要求または待機失敗。保留中の接続待ちは op の Drop がキャンセルする。
                _ => false,
            }
        }
        Err(_) => false,
    }
}

/// 次の要求が届くまで待ち、1 フレームを読み取る。
///
/// 要求はいつ来るとも限らないため、待ちには期限を置かず停止要求で打ち切る。
/// これにより、クライアントが接続したままでも server を即座に停止できる。
/// フレームの先頭が届いた後は残りが続けて届くため、通常の期限で読み取る。
pub fn read_frame_until_stop(handle: HANDLE, stop_event: HANDLE) -> Option<Vec<u8>> {
    let mut length_buf = [0u8; 4];
    if !read_exact_until_stop(handle, &mut length_buf, stop_event) {
        return None;
    }
    let length = u32::from_le_bytes(length_buf) as usize;
    if length == 0 || length > aviutl2_mcp_core::MAX_FRAME_SIZE as usize {
        return None;
    }
    let mut body = vec![0u8; length];
    win_io::read_exact(handle, &mut body, io_deadline()).ok()?;
    Some(body)
}

/// 停止要求を監視しながら `buf` を満たすまで読み取る。
///
/// 停止要求・切断・読み取り失敗のいずれでも `false` を返す。
pub fn read_exact_until_stop(handle: HANDLE, buf: &mut [u8], stop_event: HANDLE) -> bool {
    let mut total = 0usize;
    while total < buf.len() {
        // SAFETY: `handle` は MockPipeServer が所有し、スレッドの join 後にのみ閉じられる。
        // `op` はループ本体を出るときに drop されるため handle より長生きしない。
        let Ok(mut op) = (unsafe { OverlappedOp::new(handle) }) else {
            return false;
        };
        if op.begin().is_err() {
            return false;
        }
        let slice = &mut buf[total..];
        // SAFETY: `slice` は本関数のスコープで生存し、`op` の Drop が I/O 完了を
        // 待ち合わせるため、カーネルの書き込み先は常に有効である。
        let issued = unsafe { ReadFile(handle, Some(slice), None, Some(op.as_mut_ptr())) };
        let issue = match op.classify(issued) {
            Ok(issue) => issue,
            Err(_) => return false,
        };
        if issue == IoIssue::Pending
            && !matches!(
                win_io::wait_any(&[op.event(), stop_event], None),
                WaitAnyOutcome::Signaled(0)
            )
        {
            // 停止要求または待機失敗。保留中の読み取りは op の Drop がキャンセルする。
            return false;
        }
        let Ok(transferred) = op.await_completion(io_deadline()) else {
            return false;
        };
        if transferred == 0 {
            return false;
        }
        total += transferred as usize;
    }
    true
}

pub fn read_frame(handle: HANDLE, deadline: Instant) -> Option<Vec<u8>> {
    let mut length_buf = [0u8; 4];
    win_io::read_exact(handle, &mut length_buf, deadline).ok()?;
    let length = u32::from_le_bytes(length_buf) as usize;
    if length == 0 || length > aviutl2_mcp_core::MAX_FRAME_SIZE as usize {
        return None;
    }
    let mut body = vec![0u8; length];
    win_io::read_exact(handle, &mut body, deadline).ok()?;
    Some(body)
}

pub fn write_frame(
    handle: HANDLE,
    body: &[u8],
    deadline: Instant,
) -> Result<(), win_io::WinIoError> {
    let frame = encode_frame(body).map_err(|_| {
        win_io::WinIoError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame をエンコードできませんでした",
        ))
    })?;
    win_io::write_all(handle, &frame, deadline)
}

pub fn temp_registry_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aviutl2-mcp-integration-test-{}",
        InstanceId::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

pub fn current_process_created_at() -> String {
    use aviutl2_mcp_server::identity::{ProcessLookup, lookup_process};
    match lookup_process(std::process::id()) {
        ProcessLookup::Found(identity) => format_utc_timestamp(identity.created_at),
        other => panic!("自身の PID は照会できる: {other:?}"),
    }
}

/// mock server の準備が整うまで待つ余裕。
pub const MOCK_STARTUP_GRACE: Duration = Duration::from_millis(100);

pub fn request_deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
