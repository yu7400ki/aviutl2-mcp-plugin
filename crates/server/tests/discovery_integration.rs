//! discovery pipeline と mock pipe server との統合テスト。

mod support;

use aviutl2_mcp_core::{
    AuthSecret, ErrorCode, ErrorObject, InstanceDescriptor, InstanceId, InstanceState,
    ProtocolVersion, pipe_name_for,
};
use aviutl2_mcp_server::discovery::{
    DiscoveryConfig, ExclusionReason, ResolveInstanceError, find_instances, resolve_instance,
};
use aviutl2_mcp_server::pipe_client::PipeClientError;
use std::time::{Duration, Instant};
use support::{
    IO_TIMEOUT, MOCK_STARTUP_GRACE, MockPipeServer, OperationResponses, current_process_created_at,
    err_result, ok_result, request_deadline, temp_registry_dir,
};

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

#[test]
fn mock_server_stops_while_client_is_connected() {
    let dir = temp_registry_dir();
    let id = InstanceId::new_v4();
    let server = MockPipeServer::start(
        id,
        AuthSecret::generate(),
        std::process::id(),
        current_process_created_at(),
        InstanceState::Ready,
    );
    server.write_descriptor(&dir);
    std::thread::sleep(MOCK_STARTUP_GRACE);

    // 接続を保持したまま server を止める。要求待ちが停止要求で打ち切られなければ
    // 読み取りの期限まで join がブロックする。
    let _resolved = resolve_instance(&dir, id, DiscoveryConfig::default())
        .expect("生存中のインスタンスは解決できる");

    let started = Instant::now();
    drop(server);
    assert!(
        started.elapsed() < IO_TIMEOUT / 2,
        "停止に {}ms かかりました",
        started.elapsed().as_millis()
    );

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

    // project の epoch / revision / modified は descriptor に無く、ping 応答から
    // 得る。descriptor 由来の表示名とパスは維持される。
    let project = instances[0]
        .project
        .as_ref()
        .expect("project が失われています");
    assert!(project.path.is_some());
    assert_eq!(project.epoch.as_deref(), Some(support::MOCK_PROJECT_EPOCH));
    assert_eq!(project.revision, Some(support::MOCK_PROJECT_REVISION));
    assert_eq!(project.modified, Some(support::MOCK_PROJECT_MODIFIED));

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
