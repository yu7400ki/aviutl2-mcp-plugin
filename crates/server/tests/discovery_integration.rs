//! discovery pipeline と mock pipe server との統合テスト。

use aviutl2_mcp_core::{
    AuthSecret, ClientAuth, ClientHello, DescriptorProject, InstanceDescriptor, InstanceId,
    InstanceState, Nonce, ProtocolVersion, RequestEnvelope, ResponseEnvelope, ServerAuth,
    compute_client_mac, compute_server_mac, encode_frame, negotiate, pipe_name_for, verify_mac,
};
use aviutl2_mcp_server::discovery::{DiscoveryConfig, find_instances};
use aviutl2_mcp_server::win_io::{self, EventHandle, IoIssue, OverlappedOp, WaitAnyOutcome};
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
        let auth_secret_clone = auth_secret.clone();
        let process_created_at_clone = process_created_at.clone();
        let state_clone = state.clone();

        let thread = std::thread::spawn(move || {
            server_loop(
                HANDLE(handle_raw as *mut c_void),
                instance_id,
                auth_secret_clone,
                pid,
                process_created_at_clone,
                state_clone,
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

fn server_loop(
    handle: HANDLE,
    instance_id: InstanceId,
    auth_secret: AuthSecret,
    pid: u32,
    process_created_at: String,
    state: InstanceState,
    stop_event: HANDLE,
) {
    if !accept_connection(handle, stop_event) {
        return;
    }

    // M1 受信。
    let m1_body = match read_frame(handle, io_deadline()) {
        Some(body) => body,
        None => return,
    };
    let m1: ClientHello = serde_json::from_slice(&m1_body).unwrap();
    assert_eq!(m1.instance_id, instance_id);

    let server_nonce = Nonce::generate();
    let negotiated = negotiate(ProtocolVersion::CURRENT, m1.protocol_version).unwrap();
    let server_mac = compute_server_mac(
        auth_secret.as_bytes(),
        &m1.client_nonce,
        &server_nonce,
        &instance_id,
        &negotiated,
    );

    let m2 = ServerAuth {
        protocol_version: negotiated,
        instance_id,
        server_nonce,
        pid,
        process_created_at: process_created_at.clone(),
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
    let expected_client_mac =
        compute_client_mac(auth_secret.as_bytes(), &m2.server_nonce, &m1.client_nonce);
    assert!(verify_mac(&expected_client_mac, &m3.client_mac));

    // ping 受信。
    let ping_body = match read_frame(handle, io_deadline()) {
        Some(body) => body,
        None => return,
    };
    let request: RequestEnvelope = serde_json::from_slice(&ping_body).unwrap();
    assert_eq!(request.operation, "ping");

    let response = ResponseEnvelope::pong(negotiated, request.request_id, instance_id, state);
    let response_body = serde_json::to_vec(&response).unwrap();
    let _ = write_frame(handle, &response_body, io_deadline());
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
        ProcessLookup::Found(identity) => identity.created_at.to_rfc3339(),
        other => panic!("自身の PID は照会できる: {other:?}"),
    }
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
