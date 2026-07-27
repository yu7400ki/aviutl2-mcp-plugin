//! discovery pipeline と mock pipe server との統合テスト。

use aviutl2_mcp_core::{
    AuthSecret, ClientAuth, ClientHello, DescriptorProject, InstanceDescriptor, InstanceId,
    InstanceState, Nonce, ProtocolVersion, RequestEnvelope, ResponseEnvelope, ServerAuth,
    compute_client_mac, compute_server_mac, encode_frame, negotiate, pipe_name_for, verify_mac,
};
use aviutl2_mcp_server::discovery::{DiscoveryConfig, find_instances};
use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{PIPE_ACCESS_DUPLEX, ReadFile, WriteFile};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE,
};
use windows::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::core::PCWSTR;

struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}

struct MockPipeServer {
    instance_id: InstanceId,
    auth_secret: AuthSecret,
    pid: u32,
    process_created_at: String,
    state: InstanceState,
    handle: SendHandle,
    stop_event: HANDLE,
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
                PIPE_ACCESS_DUPLEX,
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

        let stop_event = unsafe { CreateEventW(None, true, false, None).unwrap() };
        let handle_raw = handle.0 as usize;
        let stop_event_raw = stop_event.0 as usize;
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
        unsafe {
            let _ = windows::Win32::System::Threading::SetEvent(self.stop_event);
            // 接続待機中の pending IO をキャンセルし、スレッドを速やかに終了させる。
            let _ = CancelIoEx(self.handle.0, None);
            let _ = CloseHandle(self.handle.0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        unsafe {
            let _ = CloseHandle(self.stop_event);
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
    let connect_event = unsafe { CreateEventW(None, true, false, None).unwrap() };
    let mut connect_overlapped = OVERLAPPED {
        hEvent: connect_event,
        ..Default::default()
    };

    let connect_result = unsafe { ConnectNamedPipe(handle, Some(&mut connect_overlapped)) };
    let pending = match connect_result {
        Ok(()) => false,
        Err(err) => {
            if err.code() == windows::Win32::Foundation::ERROR_IO_PENDING.into() {
                true
            } else if err.code() == windows::Win32::Foundation::ERROR_PIPE_CONNECTED.into() {
                false
            } else {
                return;
            }
        }
    };

    if pending {
        let events = [connect_event, stop_event];
        let result = unsafe { WaitForMultipleObjects(&events, false, INFINITE) };
        if result.0 == WAIT_OBJECT_0.0 + 1 {
            return;
        }
    }

    // M1 受信。
    let m1_body = match read_frame(handle, Duration::from_secs(2)) {
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
    if write_frame(handle, &m2_body, Duration::from_secs(2)).is_err() {
        return;
    }

    // M3 受信。
    let m3_body = match read_frame(handle, Duration::from_secs(2)) {
        Some(body) => body,
        None => return,
    };
    let m3: ClientAuth = serde_json::from_slice(&m3_body).unwrap();
    let expected_client_mac =
        compute_client_mac(auth_secret.as_bytes(), &m2.server_nonce, &m1.client_nonce);
    assert!(verify_mac(&expected_client_mac, &m3.client_mac));

    // ping 受信。
    let ping_body = match read_frame(handle, Duration::from_secs(2)) {
        Some(body) => body,
        None => return,
    };
    let request: RequestEnvelope = serde_json::from_slice(&ping_body).unwrap();
    assert_eq!(request.operation, "ping");

    let response = ResponseEnvelope::pong(negotiated, request.request_id, instance_id, state);
    let response_body = serde_json::to_vec(&response).unwrap();
    let _ = write_frame(handle, &response_body, Duration::from_secs(2));
}

fn read_frame(handle: HANDLE, timeout: Duration) -> Option<Vec<u8>> {
    let mut length_buf = [0u8; 4];
    read_exact(handle, &mut length_buf, timeout).ok()?;
    let length = u32::from_le_bytes(length_buf) as usize;
    if length == 0 || length > 8 * 1024 * 1024 {
        return None;
    }
    let mut body = vec![0u8; length];
    read_exact(handle, &mut body, timeout).ok()?;
    Some(body)
}

fn write_frame(handle: HANDLE, body: &[u8], timeout: Duration) -> std::io::Result<()> {
    let frame = encode_frame(body)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "frame"))?;
    write_all(handle, &frame, timeout)
}

fn read_exact(handle: HANDLE, buf: &mut [u8], timeout: Duration) -> std::io::Result<()> {
    let mut overlapped = new_overlapped()?;
    let mut total = 0;
    while total < buf.len() {
        unsafe {
            ResetEvent(overlapped.hEvent)
                .map_err(|e| std::io::Error::from_raw_os_error(e.code().0))?;
        }
        let mut read = 0u32;
        let slice = &mut buf[total..];
        let result =
            unsafe { ReadFile(handle, Some(slice), Some(&mut read), Some(&mut overlapped)) };
        if result.is_ok() {
            total += read as usize;
            continue;
        }
        let err = result.unwrap_err();
        if err.code() != windows::Win32::Foundation::ERROR_IO_PENDING.into() {
            return Err(std::io::Error::from_raw_os_error(err.code().0));
        }
        wait_io(overlapped.hEvent, timeout)?;
        let transferred = unsafe { get_overlapped_result(handle, &overlapped)? };
        if transferred == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "closed",
            ));
        }
        total += transferred as usize;
    }
    Ok(())
}

fn write_all(handle: HANDLE, buf: &[u8], timeout: Duration) -> std::io::Result<()> {
    let mut overlapped = new_overlapped()?;
    let mut total = 0;
    while total < buf.len() {
        unsafe {
            ResetEvent(overlapped.hEvent)
                .map_err(|e| std::io::Error::from_raw_os_error(e.code().0))?;
        }
        let mut written = 0u32;
        let result = unsafe {
            WriteFile(
                handle,
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
            return Err(std::io::Error::from_raw_os_error(err.code().0));
        }
        wait_io(overlapped.hEvent, timeout)?;
        let transferred = unsafe { get_overlapped_result(handle, &overlapped)? };
        total += transferred as usize;
    }
    Ok(())
}

fn new_overlapped() -> std::io::Result<OVERLAPPED> {
    unsafe {
        let event = CreateEventW(None, true, false, None)?;
        let mut overlapped = std::mem::zeroed::<OVERLAPPED>();
        overlapped.hEvent = event;
        Ok(overlapped)
    }
}

fn wait_io(event: HANDLE, timeout: Duration) -> std::io::Result<()> {
    let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
    let result = unsafe { WaitForSingleObject(event, ms) };
    if result.0 == windows::Win32::Foundation::WAIT_OBJECT_0.0 {
        Ok(())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout"))
    }
}

unsafe fn get_overlapped_result(handle: HANDLE, overlapped: &OVERLAPPED) -> std::io::Result<u32> {
    let mut transferred = 0u32;
    unsafe {
        GetOverlappedResult(handle, overlapped, &mut transferred, false)
            .map_err(|e| std::io::Error::from_raw_os_error(e.code().0))?;
    }
    Ok(transferred)
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
    use aviutl2_mcp_server::identity::get_process_identity;
    let identity = get_process_identity(std::process::id()).unwrap();
    identity.created_at.to_rfc3339()
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

    let instances = find_instances(&dir, DiscoveryConfig::default(), true);
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

    let instances = find_instances(&dir, DiscoveryConfig::default(), true);
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

    let instances = find_instances(&dir, DiscoveryConfig::default(), true);
    assert!(instances.is_empty(), "auth_secret 不一致は除外される");

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

    let instances = find_instances(&dir, DiscoveryConfig::default(), true);
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].instance_id, id1);

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

    let instances = find_instances(&dir, DiscoveryConfig::default(), true);
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

    let instances = find_instances(&dir, DiscoveryConfig::default(), true);
    assert_eq!(instances.len(), 1, "終了したインスタンスは一覧に含まれない");
    assert_eq!(instances[0].instance_id, id2);

    let _ = std::fs::remove_dir_all(&dir);
}
