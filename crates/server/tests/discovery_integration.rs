//! discovery pipeline と mock pipe server との統合テスト。

use aviutl2_mcp_core::{
    AuthSecret, ClientAuth, ClientHello, DescriptorProject, ErrorCode, ErrorObject,
    InstanceDescriptor, InstanceId, InstanceState, Nonce, ProtocolVersion, RequestEnvelope,
    ResponseEnvelope, ResponseKind, ResponseResult, ServerAuth, compute_client_mac,
    compute_server_mac, encode_frame, format_utc_timestamp, negotiate, pipe_name_for, verify_mac,
};
use aviutl2_mcp_server::discovery::{
    DiscoveryConfig, ExclusionReason, ResolveInstanceError, find_instances, resolve_instance,
};
use aviutl2_mcp_server::pipe_client::PipeClientError;
use aviutl2_mcp_server::win_io::{self, EventHandle, IoIssue, OverlappedOp, WaitAnyOutcome};
use std::collections::HashMap;
use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE,
};
use windows::core::PCWSTR;

/// mock server が 1 回の read/write に許す時間。
const IO_TIMEOUT: Duration = Duration::from_secs(2);

fn io_deadline() -> Instant {
    Instant::now() + IO_TIMEOUT
}

struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}

/// operation 名から応答内容への対応表。
///
/// テストはここへ応答を注入し、mock server に read operation を演じさせる。
/// `ping` を含む任意の operation を差し替えられる。表に無い operation は、
/// `ping` なら生存応答、それ以外は `unsupported_operation` のエラー応答になる。
type OperationResponses = HashMap<String, ResponseResult>;

/// 成功応答を組み立てる。
fn ok_result(value: serde_json::Value) -> ResponseResult {
    ResponseResult::Ok { result: value }
}

/// エラー応答を組み立てる。
fn err_result(error: ErrorObject) -> ResponseResult {
    ResponseResult::Err { error }
}

/// mock server がクライアントへ提示する identity と応答。
struct MockBehavior {
    instance_id: InstanceId,
    auth_secret: AuthSecret,
    pid: u32,
    process_created_at: String,
    state: InstanceState,
    responses: OperationResponses,
}

struct MockPipeServer {
    instance_id: InstanceId,
    auth_secret: AuthSecret,
    pid: u32,
    process_created_at: String,
    state: InstanceState,
    handle: SendHandle,
    stop_event: EventHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MockPipeServer {
    fn start(
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

    fn start_with_operations(
        instance_id: InstanceId,
        auth_secret: AuthSecret,
        pid: u32,
        process_created_at: String,
        state: InstanceState,
        responses: OperationResponses,
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
        let behavior = MockBehavior {
            instance_id,
            auth_secret: auth_secret.clone(),
            pid,
            process_created_at: process_created_at.clone(),
            state: state.clone(),
            responses,
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
            thread: Some(thread),
        }
    }

    fn descriptor(&self, _registry_dir: PathBuf) -> InstanceDescriptor {
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

    fn write_descriptor(&self, registry_dir: &std::path::Path) {
        std::fs::create_dir_all(registry_dir).unwrap();
        let path = registry_dir.join(format!("{}.json", self.instance_id));
        std::fs::write(
            &path,
            serde_json::to_string(&self.descriptor(registry_dir.to_path_buf())).unwrap(),
        )
        .unwrap();
    }
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

fn server_loop(handle: HANDLE, behavior: MockBehavior, stop_event: HANDLE) {
    if !accept_connection(handle, stop_event) {
        return;
    }

    // M1 受信。
    let m1_body = match read_frame(handle, io_deadline()) {
        Some(body) => body,
        None => return,
    };
    let m1: ClientHello = serde_json::from_slice(&m1_body).unwrap();
    assert_eq!(m1.instance_id, behavior.instance_id);

    let server_nonce = Nonce::generate();
    let negotiated = negotiate(ProtocolVersion::CURRENT, m1.protocol_version).unwrap();
    let server_mac = compute_server_mac(
        behavior.auth_secret.as_bytes(),
        &m1.client_nonce,
        &server_nonce,
        &behavior.instance_id,
        &negotiated,
    );

    let m2 = ServerAuth {
        protocol_version: negotiated,
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

    // 要求ループ。切断・EOF・読み取り失敗のいずれでも抜ける。
    loop {
        let Some(body) = read_frame(handle, io_deadline()) else {
            return;
        };
        let Ok(request) = serde_json::from_slice::<RequestEnvelope>(&body) else {
            return;
        };
        let response = build_response(&request, &behavior, negotiated);
        let response_body = serde_json::to_vec(&response).unwrap();
        if write_frame(handle, &response_body, io_deadline()).is_err() {
            return;
        }
    }
}

/// 要求の operation に応じた応答を組み立てる。
fn build_response(
    request: &RequestEnvelope,
    behavior: &MockBehavior,
    negotiated: ProtocolVersion,
) -> ResponseEnvelope {
    let result = match behavior.responses.get(&request.operation) {
        Some(result) => result.clone(),
        None if request.operation == "ping" => {
            return ResponseEnvelope::pong(
                negotiated,
                request.request_id,
                behavior.instance_id,
                behavior.state.clone(),
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
        protocol_version: negotiated,
        request_id: request.request_id,
        instance_id: behavior.instance_id,
        result,
    }
}

/// クライアントの接続を待つ。停止要求で待機を打ち切った場合は `false` を返す。
fn accept_connection(handle: HANDLE, stop_event: HANDLE) -> bool {
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

fn read_frame(handle: HANDLE, deadline: Instant) -> Option<Vec<u8>> {
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

fn write_frame(handle: HANDLE, body: &[u8], deadline: Instant) -> Result<(), win_io::WinIoError> {
    let frame = encode_frame(body).map_err(|_| {
        win_io::WinIoError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame をエンコードできませんでした",
        ))
    })?;
    win_io::write_all(handle, &frame, deadline)
}

fn temp_registry_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aviutl2-mcp-integration-test-{}",
        InstanceId::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn current_process_created_at() -> String {
    use aviutl2_mcp_server::identity::{ProcessLookup, lookup_process};
    match lookup_process(std::process::id()) {
        ProcessLookup::Found(identity) => format_utc_timestamp(identity.created_at),
        other => panic!("自身の PID は照会できる: {other:?}"),
    }
}

/// mock server の準備が整うまで待つ余裕。
const MOCK_STARTUP_GRACE: Duration = Duration::from_millis(100);

fn request_deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[test]
fn resolved_client_serves_multiple_requests() {
    let dir = temp_registry_dir();
    let id = InstanceId::new_v4();
    let created_at = current_process_created_at();

    let edit_info = serde_json::json!({ "project_revision": 7 });
    let layers = serde_json::json!({ "items": [], "page": { "total_count": 0 } });
    let responses = OperationResponses::from([
        ("get_edit_info".to_string(), ok_result(edit_info.clone())),
        ("list_layers".to_string(), ok_result(layers.clone())),
    ]);

    let server = MockPipeServer::start_with_operations(
        id,
        AuthSecret::generate(),
        std::process::id(),
        created_at,
        InstanceState::Ready,
        responses,
    );
    server.write_descriptor(&dir);
    std::thread::sleep(MOCK_STARTUP_GRACE);

    let resolved = resolve_instance(&dir, id, DiscoveryConfig::default())
        .expect("生存中のインスタンスは解決できる");
    assert_eq!(resolved.info.instance_id, id);
    assert_eq!(resolved.info.state, InstanceState::Ready);

    // handshake と ping に続けて複数の要求を同じ接続で処理できる。
    assert_eq!(
        resolved
            .client
            .request("get_edit_info", serde_json::json!({}), request_deadline())
            .expect("注入した応答を受け取れる"),
        edit_info
    );
    assert_eq!(
        resolved
            .client
            .request("list_layers", serde_json::json!({}), request_deadline())
            .expect("2 件目の要求も処理される"),
        layers
    );

    let error = resolved
        .client
        .request(
            "no_such_operation",
            serde_json::json!({}),
            request_deadline(),
        )
        .expect_err("未知 operation は拒否される");
    let PipeClientError::Remote(remote) = &error else {
        panic!("エラー応答が保たれていません: {error:?}");
    };
    assert_eq!(remote.code, ErrorCode::UnsupportedOperation);
    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);

    // 未知 operation の拒否後も接続は継続する。
    assert_eq!(
        resolved
            .client
            .request("get_edit_info", serde_json::json!({}), request_deadline())
            .expect("エラー応答の後も要求を処理できる"),
        edit_info
    );

    drop(resolved);
    drop(server);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_instance_reports_authentication_failed_for_wrong_secret() {
    let dir = temp_registry_dir();
    let id = InstanceId::new_v4();
    let created_at = current_process_created_at();
    let server = MockPipeServer::start(
        id,
        AuthSecret::generate(),
        std::process::id(),
        created_at,
        InstanceState::Ready,
    );

    // descriptor には別の auth_secret を書き、handshake を失敗させる。
    let mut descriptor = server.descriptor(dir.clone());
    descriptor.auth_secret = AuthSecret::generate();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}.json", id));
    std::fs::write(&path, serde_json::to_string(&descriptor).unwrap()).unwrap();
    std::thread::sleep(MOCK_STARTUP_GRACE);

    let error = resolve_instance(&dir, id, DiscoveryConfig::default())
        .err()
        .expect("auth_secret 不一致は解決に失敗する");
    assert!(
        matches!(error, ResolveInstanceError::Excluded(_)),
        "登録済みだが検証に落ちた扱いになる: {error:?}"
    );
    assert_eq!(error.error_code(), ErrorCode::AuthenticationFailed);

    let _ = std::fs::remove_dir_all(&dir);
}

/// ping が指定のエラーを返す mock を起こし、descriptor を書く。
fn start_server_with_ping_error(
    dir: &std::path::Path,
    id: InstanceId,
    error: ErrorObject,
) -> MockPipeServer {
    let server = MockPipeServer::start_with_operations(
        id,
        AuthSecret::generate(),
        std::process::id(),
        current_process_created_at(),
        InstanceState::Ready,
        OperationResponses::from([("ping".to_string(), err_result(error))]),
    );
    server.write_descriptor(dir);
    std::thread::sleep(MOCK_STARTUP_GRACE);
    server
}

#[test]
fn resolve_instance_surfaces_host_busy_from_ping() {
    let dir = temp_registry_dir();
    let id = InstanceId::new_v4();
    let error = ErrorObject::new(ErrorCode::HostBusy, "起動処理中です", true)
        .with_details(serde_json::json!({ "retry_after_ms": 500 }));
    let _server = start_server_with_ping_error(&dir, id, error.clone());

    let failure = resolve_instance(&dir, id, DiscoveryConfig::default())
        .err()
        .expect("host_busy を返すインスタンスは解決できない");

    // 一覧を取り直しても同じ ID が返るだけなので、待ち直しへ誘導する。
    assert_eq!(failure.error_code(), ErrorCode::HostBusy);
    assert_eq!(
        failure.remote_error(),
        Some(&error),
        "retry_after_ms を含むエラー応答がそのまま届く"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_instance_hides_other_ping_errors() {
    let dir = temp_registry_dir();
    let id = InstanceId::new_v4();
    let _server = start_server_with_ping_error(
        &dir,
        id,
        ErrorObject::new(ErrorCode::InternalError, "想定外の失敗", false),
    );

    let failure = resolve_instance(&dir, id, DiscoveryConfig::default())
        .err()
        .expect("ping を拒否するインスタンスは解決できない");

    // 使えることを確認できていないため、生存確認の失敗と同じ扱いにする。
    assert!(
        matches!(
            failure,
            ResolveInstanceError::Excluded(ExclusionReason::PingFailed)
        ),
        "実際のエラー: {failure:?}"
    );
    assert_eq!(failure.error_code(), ErrorCode::InstanceStale);
    assert_eq!(failure.remote_error(), None);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn discovery_excludes_instance_whose_ping_is_rejected() {
    let dir = temp_registry_dir();
    let id = InstanceId::new_v4();
    let _server = start_server_with_ping_error(
        &dir,
        id,
        ErrorObject::new(ErrorCode::HostBusy, "起動処理中です", true),
    );

    // 一覧は生存確認済みの候補だけを返す。host_busy でも一覧には出さない。
    let instances = find_instances(&dir, DiscoveryConfig::default(), true)
        .expect("registry ディレクトリを列挙できる");
    assert!(
        instances.is_empty(),
        "ping を拒否した候補は一覧に含まれない"
    );
    assert!(
        dir.join(format!("{}.json", id)).exists(),
        "生存中の descriptor は削除されない"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_instance_excludes_draining_instance() {
    let dir = temp_registry_dir();
    let id = InstanceId::new_v4();
    let created_at = current_process_created_at();
    let server = MockPipeServer::start(
        id,
        AuthSecret::generate(),
        std::process::id(),
        created_at,
        InstanceState::Draining,
    );
    server.write_descriptor(&dir);
    std::thread::sleep(MOCK_STARTUP_GRACE);

    let error = resolve_instance(&dir, id, DiscoveryConfig::default())
        .err()
        .expect("draining のインスタンスは解決できない");
    assert_eq!(error.error_code(), ErrorCode::InstanceStale);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn discovery_finds_live_mock_instance() {
    let dir = temp_registry_dir();
    let id = InstanceId::new_v4();
    let secret = AuthSecret::generate();
    let created_at = current_process_created_at();
    let server = MockPipeServer::start(
        id,
        secret.clone(),
        std::process::id(),
        created_at.clone(),
        InstanceState::Ready,
    );
    server.write_descriptor(&dir);

    // pipe server の準備を待つ。
    std::thread::sleep(Duration::from_millis(100));

    let instances = find_instances(&dir, DiscoveryConfig::default(), true)
        .expect("registry ディレクトリを列挙できる");
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].instance_id, id);
    assert_eq!(instances[0].state, InstanceState::Ready);
    assert_eq!(instances[0].pid, std::process::id());
    assert!(instances[0].project.is_some());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn discovery_excludes_draining_instance() {
    let dir = temp_registry_dir();
    let id = InstanceId::new_v4();
    let secret = AuthSecret::generate();
    let created_at = current_process_created_at();
    let server = MockPipeServer::start(
        id,
        secret.clone(),
        std::process::id(),
        created_at.clone(),
        InstanceState::Draining,
    );
    server.write_descriptor(&dir);

    std::thread::sleep(Duration::from_millis(100));

    let instances = find_instances(&dir, DiscoveryConfig::default(), true)
        .expect("registry ディレクトリを列挙できる");
    assert!(instances.is_empty(), "draining instance は一覧に含まれない");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn discovery_excludes_authentication_failed_instance() {
    let dir = temp_registry_dir();
    let id = InstanceId::new_v4();
    let wrong_secret = AuthSecret::generate();
    let created_at = current_process_created_at();
    let server = MockPipeServer::start(
        id,
        AuthSecret::generate(),
        std::process::id(),
        created_at.clone(),
        InstanceState::Ready,
    );

    // descriptor には異なる auth_secret を書く。
    let mut descriptor = server.descriptor(dir.clone());
    descriptor.auth_secret = wrong_secret;
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}.json", id));
    std::fs::write(&path, serde_json::to_string(&descriptor).unwrap()).unwrap();

    std::thread::sleep(Duration::from_millis(100));

    let instances = find_instances(&dir, DiscoveryConfig::default(), true)
        .expect("registry ディレクトリを列挙できる");
    assert!(instances.is_empty(), "auth_secret 不一致は除外される");
    assert!(path.exists(), "生存中の descriptor は削除されない");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cleanup_preserves_live_but_unreachable_instance() {
    let dir = temp_registry_dir();
    let id = InstanceId::new_v4();
    let created_at = current_process_created_at();
    let descriptor = InstanceDescriptor {
        schema_version: 1,
        protocol_version: ProtocolVersion::CURRENT,
        instance_id: id,
        pipe_name: pipe_name_for(&id),
        auth_secret: AuthSecret::generate(),
        pid: std::process::id(),
        process_created_at: created_at.clone(),
        hwnd: None,
        started_at: created_at,
        state: InstanceState::Ready,
        project: None,
    };
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}.json", id));
    std::fs::write(&path, serde_json::to_string(&descriptor).unwrap()).unwrap();

    let instances = find_instances(&dir, DiscoveryConfig::default(), true)
        .expect("registry ディレクトリを列挙できる");
    assert!(
        instances.is_empty(),
        "pipe に接続できない instance は除外される"
    );
    assert!(path.exists(), "生存中の descriptor は削除されない");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn discovery_isolates_broken_candidate() {
    let dir = temp_registry_dir();
    let id1 = InstanceId::new_v4();
    let id2 = InstanceId::new_v4();
    let created_at = current_process_created_at();

    let server1 = MockPipeServer::start(
        id1,
        AuthSecret::generate(),
        std::process::id(),
        created_at.clone(),
        InstanceState::Ready,
    );

    server1.write_descriptor(&dir);

    // id2 は pipe server を起動せず descriptor だけ残す（生存確認に失敗）。
    let descriptor2 = InstanceDescriptor {
        schema_version: 1,
        protocol_version: ProtocolVersion::CURRENT,
        instance_id: id2,
        pipe_name: pipe_name_for(&id2),
        auth_secret: AuthSecret::generate(),
        pid: std::process::id(),
        process_created_at: created_at.clone(),
        hwnd: None,
        started_at: created_at.clone(),
        state: InstanceState::Ready,
        project: None,
    };
    std::fs::create_dir_all(&dir).unwrap();
    let path2 = dir.join(format!("{}.json", id2));
    std::fs::write(&path2, serde_json::to_string(&descriptor2).unwrap()).unwrap();

    std::thread::sleep(Duration::from_millis(100));

    let instances = find_instances(&dir, DiscoveryConfig::default(), true)
        .expect("registry ディレクトリを列挙できる");
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].instance_id, id1);
    assert!(path2.exists(), "生存中の descriptor は削除されない");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn discovery_lists_three_distinct_instances() {
    let dir = temp_registry_dir();
    let created_at = current_process_created_at();

    let server1 = MockPipeServer::start(
        InstanceId::new_v4(),
        AuthSecret::generate(),
        std::process::id(),
        created_at.clone(),
        InstanceState::Ready,
    );
    let server2 = MockPipeServer::start(
        InstanceId::new_v4(),
        AuthSecret::generate(),
        std::process::id(),
        created_at.clone(),
        InstanceState::Ready,
    );
    let server3 = MockPipeServer::start(
        InstanceId::new_v4(),
        AuthSecret::generate(),
        std::process::id(),
        created_at.clone(),
        InstanceState::Busy,
    );

    server1.write_descriptor(&dir);
    server2.write_descriptor(&dir);
    server3.write_descriptor(&dir);

    std::thread::sleep(Duration::from_millis(200));

    let instances = find_instances(&dir, DiscoveryConfig::default(), true)
        .expect("registry ディレクトリを列挙できる");
    assert_eq!(instances.len(), 3, "3 件の生存インスタンスが列挙される");

    let ids: std::collections::HashSet<_> = instances.iter().map(|info| info.instance_id).collect();
    assert_eq!(ids.len(), 3, "各 instance_id は互いに異なる");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn discovery_excludes_stopped_instance_even_if_descriptor_remains() {
    let dir = temp_registry_dir();
    let created_at = current_process_created_at();

    let id1 = InstanceId::new_v4();
    let id2 = InstanceId::new_v4();

    let server1 = MockPipeServer::start(
        id1,
        AuthSecret::generate(),
        std::process::id(),
        created_at.clone(),
        InstanceState::Ready,
    );
    let server2 = MockPipeServer::start(
        id2,
        AuthSecret::generate(),
        std::process::id(),
        created_at.clone(),
        InstanceState::Ready,
    );

    server1.write_descriptor(&dir);
    server2.write_descriptor(&dir);

    std::thread::sleep(Duration::from_millis(200));

    // 最初に server1 のみ終了し、descriptor は意図的に残す（crash 模擬）。
    drop(server1);

    let instances = find_instances(&dir, DiscoveryConfig::default(), true)
        .expect("registry ディレクトリを列挙できる");
    assert_eq!(instances.len(), 1, "終了したインスタンスは一覧に含まれない");
    assert_eq!(instances[0].instance_id, id2);
    assert!(
        dir.join(format!("{}.json", id1)).exists(),
        "生存中の descriptor は削除されない"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
