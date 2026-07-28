//! stdio 越しの MCP セッションを実プロセスで確認する。
//!
//! stdout に MCP メッセージ以外が混じらないこと、resource の list / read が
//! 往復することを、ログ出力を最大にした状態で検証する。

mod support;

use aviutl2_mcp_core::{
    AuthSecret, Cursor, DisplayRange, EditInfo, Extent, FiniteF64, FrameRange, InstanceId,
    InstanceState, SceneInfo,
};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use support::{
    MOCK_STARTUP_GRACE, MockPipeServer, OperationResponses, current_process_created_at, ok_result,
    temp_registry_dir, write_bare_descriptor,
};

/// stdio セッションの結果。
struct Session {
    stdout: String,
    stderr: String,
}

impl Session {
    /// stdout の各行を JSON-RPC メッセージとして解釈する。
    ///
    /// 1 行でも解釈できなければ stdout が汚染されている。
    fn messages(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let message: Value = serde_json::from_str(line).unwrap_or_else(|e| {
                    panic!("stdout に MCP 以外の出力があります: {line:?} ({e})")
                });
                assert_eq!(
                    message["jsonrpc"],
                    json!("2.0"),
                    "JSON-RPC ではない出力があります: {line}"
                );
                message
            })
            .collect()
    }

    /// 指定 id の応答を取り出す。
    fn response(&self, id: u64) -> Value {
        self.messages()
            .into_iter()
            .find(|message| message["id"] == json!(id))
            .unwrap_or_else(|| panic!("id={id} の応答がありません: {}", self.stdout))
    }
}

/// registry を指定してサーバーを起こし、要求を 1 件ずつ往復させて結果を得る。
///
/// ログが stdout へ漏れないことを確かめるため、最も冗長な設定で走らせる。
fn run_session(registry_dir: &Path, requests: &[Value]) -> Session {
    run_session_with_log(registry_dir, requests, Some("trace"))
}

/// ログの絞り込みを指定してセッションを実行する。
///
/// `rust_log` が `None` のときは `RUST_LOG` を渡さず、既定のレベルを確かめる。
/// 要求を一度に流し込むとサーバーが並行に処理し、インスタンスへの接続が重なる。
/// 実際のクライアントと同じく、直前の応答を受け取ってから次を送る。
fn run_session_with_log(
    registry_dir: &Path,
    requests: &[Value],
    rust_log: Option<&str>,
) -> Session {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aviutl2-mcp-server"));
    command.env("AVIUTL2_MCP_REGISTRY_DIR", registry_dir);
    match rust_log {
        Some(filter) => command.env("RUST_LOG", filter),
        None => command.env_remove("RUST_LOG"),
    };

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("MCP サーバーを起動できる");

    let mut stdin = child.stdin.take().expect("stdin を得られる");
    let mut reader = BufReader::new(child.stdout.take().expect("stdout を得られる"));
    // stderr を読み続けないとログでパイプが詰まる。
    let mut stderr_pipe = child.stderr.take().expect("stderr を得られる");
    let stderr_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr_pipe.read_to_string(&mut text);
        text
    });

    let mut stdout = String::new();
    for request in requests {
        let line = serde_json::to_string(request).expect("直列化できる");
        writeln!(stdin, "{line}").expect("要求を書き込める");
        stdin.flush().expect("要求を送出できる");

        let Some(id) = request.get("id") else {
            continue;
        };
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).expect("stdout を読める") == 0 {
                break;
            }
            stdout.push_str(&line);
            let responded = serde_json::from_str::<Value>(line.trim_end())
                .is_ok_and(|message| message.get("id") == Some(id));
            if responded {
                break;
            }
        }
    }

    // stdin を閉じてサーバーの終了を促す。
    drop(stdin);
    reader
        .read_to_string(&mut stdout)
        .expect("残りの stdout を読める");
    child.wait().expect("サーバーの終了を待てる");

    Session {
        stdout,
        stderr: stderr_reader.join().expect("stderr の読み取りが完了する"),
    }
}

fn initialize_requests() -> Vec<Value> {
    vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "1" },
            },
        }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    ]
}

fn sample_edit_info() -> EditInfo {
    EditInfo {
        scene: SceneInfo {
            id: 3,
            name: Some("Scene 1".to_string()),
            width: 1920,
            height: 1080,
            fps: FiniteF64::try_new(60.0),
            fps_rate: 60,
            fps_scale: 1,
            sample_rate: 48_000,
        },
        cursor: Cursor { frame: 5, layer: 2 },
        extent: Extent {
            frame_max: 240,
            layer_max: 4,
        },
        display: DisplayRange {
            frame_start: 0,
            layer_start: 0,
            frame_num: 100,
            layer_num: 10,
        },
        selected_range: Some(FrameRange { start: 0, end: 10 }),
        grid_bpm: vec![FiniteF64::try_new(120.0).expect("有限値")],
        project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
        project_revision: 42,
    }
}

#[test]
fn stdout_carries_only_mcp_messages() {
    let registry_dir = temp_registry_dir();
    std::fs::create_dir_all(&registry_dir).expect("registry を作れる");

    let mut requests = initialize_requests();
    requests.push(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": { "name": "aviutl2_list_instances", "arguments": {} },
    }));

    let session = run_session(&registry_dir, &requests);
    let messages = session.messages();
    assert!(!messages.is_empty(), "応答がありません");
    assert!(
        session.stderr.contains("aviutl2-mcp-server started"),
        "ログが stderr に出ていません: {}",
        session.stderr
    );

    let tools = session.response(2);
    assert!(tools["result"]["tools"].is_array());
    let call = session.response(3);
    assert_eq!(call["result"]["isError"], json!(false));

    let _ = std::fs::remove_dir_all(&registry_dir);
}

#[test]
fn tool_call_outcome_is_logged_without_rust_log() {
    let registry_dir = temp_registry_dir();
    std::fs::create_dir_all(&registry_dir).expect("registry を作れる");

    let mut requests = initialize_requests();
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "aviutl2_list_instances", "arguments": {} },
    }));

    let session = run_session_with_log(&registry_dir, &requests, None);
    assert_eq!(session.response(2)["result"]["isError"], json!(false));

    // operation / correlation_id / 所要時間 / 結果コードは既定設定でも記録される。
    let logged = session
        .stderr
        .lines()
        .find(|line| line.contains("tool call succeeded"))
        .unwrap_or_else(|| panic!("tool call の結果が記録されていません: {}", session.stderr));
    for field in [
        "aviutl2_list_instances",
        "correlation_id",
        "duration_ms",
        "result",
    ] {
        assert!(logged.contains(field), "{field} がありません: {logged}");
    }

    let _ = std::fs::remove_dir_all(&registry_dir);
}

#[test]
fn rejected_tool_calls_do_not_pollute_stdout() {
    let registry_dir = temp_registry_dir();
    std::fs::create_dir_all(&registry_dir).expect("registry を作れる");
    let instance_id = InstanceId::new_v4().to_string();

    let mut requests = initialize_requests();
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "no_such_tool", "arguments": {} },
    }));
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "aviutl2_list_layers",
            "arguments": {
                "instance_id": instance_id,
                "expected_scene_id": 0,
                "future": 1,
            },
        },
    }));
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "aviutl2_list_layers",
            "arguments": {
                "instance_id": instance_id,
                "expected_scene_id": 0,
                "limit": 0,
            },
        },
    }));

    let session = run_session(&registry_dir, &requests);
    // 未知 tool は経路が存在しないため protocol error になる。
    assert!(
        session.response(2)["error"].is_object(),
        "未知の tool は protocol error"
    );

    let unknown_field = session.response(3);
    assert_eq!(unknown_field["result"]["isError"], json!(true));
    let message = unknown_field["result"]["content"][0]["text"]
        .as_str()
        .expect("text がある");
    assert!(
        message.contains("future"),
        "未知フィールドの拒否理由: {message}"
    );

    // schema の範囲は server 側でも検証し、構造化したエラーを返す。
    let out_of_range = session.response(4);
    assert_eq!(out_of_range["result"]["isError"], json!(true));
    assert_eq!(
        out_of_range["result"]["structuredContent"]["code"],
        json!("invalid_argument")
    );

    let _ = std::fs::remove_dir_all(&registry_dir);
}

#[test]
fn resources_round_trip_over_stdio() {
    let registry_dir = temp_registry_dir();
    let edit_info = serde_json::to_value(sample_edit_info()).expect("直列化できる");
    let mock = MockPipeServer::start_with_operations(
        InstanceId::new_v4(),
        AuthSecret::generate(),
        std::process::id(),
        current_process_created_at(),
        InstanceState::Ready,
        OperationResponses::from([("get_edit_info".to_string(), ok_result(edit_info.clone()))]),
    );
    mock.write_descriptor(&registry_dir);
    std::thread::sleep(MOCK_STARTUP_GRACE);

    let instances_uri = "aviutl2://instances";
    let edit_info_uri = format!("{instances_uri}/{}/edit-info", mock.instance_id());

    let mut requests = initialize_requests();
    requests.push(json!({ "jsonrpc": "2.0", "id": 2, "method": "resources/list" }));
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "resources/read",
        "params": { "uri": instances_uri },
    }));
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "resources/read",
        "params": { "uri": edit_info_uri },
    }));
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "resources/read",
        "params": { "uri": "aviutl2://instances/unknown/edit-info" },
    }));

    let session = run_session(&registry_dir, &requests);

    let listed = session.response(2);
    let uris: Vec<String> = listed["result"]["resources"]
        .as_array()
        .expect("resources は配列")
        .iter()
        .map(|resource| resource["uri"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(uris.contains(&instances_uri.to_string()), "{uris:?}");
    assert!(uris.contains(&edit_info_uri), "{uris:?}");

    let instances = session.response(3);
    let text = instances["result"]["contents"][0]["text"]
        .as_str()
        .expect("text がある");
    let decoded: Value = serde_json::from_str(text).expect("JSON として読める");
    assert_eq!(decoded["total_count"], json!(1));

    let read_edit_info = session.response(4);
    let text = read_edit_info["result"]["contents"][0]["text"]
        .as_str()
        .expect("text がある");
    let decoded: Value = serde_json::from_str(text).expect("JSON として読める");
    assert_eq!(decoded, edit_info);

    assert!(
        session.response(5)["error"].is_object(),
        "未知の resource URI は拒否される"
    );

    drop(mock);
    let _ = std::fs::remove_dir_all(&registry_dir);
}

/// `resources/list` の応答から resource の URI を取り出す。
fn listed_uris(response: &Value) -> Vec<String> {
    response["result"]["resources"]
        .as_array()
        .expect("resources は配列")
        .iter()
        .map(|resource| resource["uri"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn resources_list_does_not_probe_instances() {
    let registry_dir = temp_registry_dir();
    // pipe を待ち受けないインスタンスを登録する。生存確認へ出れば 1 件も並ばない。
    let registered: Vec<String> = (0..3)
        .map(|_| write_bare_descriptor(&registry_dir).to_string())
        .collect();

    let mut requests = initialize_requests();
    requests.push(json!({ "jsonrpc": "2.0", "id": 2, "method": "resources/list" }));

    let session = run_session(&registry_dir, &requests);
    let uris = listed_uris(&session.response(2));
    for instance_id in &registered {
        assert!(
            uris.iter().any(|uri| uri.contains(instance_id)),
            "{instance_id} が列挙されていません: {uris:?}"
        );
    }
    assert!(session.response(2)["result"]["nextCursor"].is_null());

    let _ = std::fs::remove_dir_all(&registry_dir);
}

#[test]
fn resources_list_pages_with_a_cursor() {
    let registry_dir = temp_registry_dir();
    // 1 ページの上限を超える件数を登録し、続きが黙って落ちないことを確かめる。
    let total = 150;
    for _ in 0..total {
        write_bare_descriptor(&registry_dir);
    }

    let mut requests = initialize_requests();
    requests.push(json!({ "jsonrpc": "2.0", "id": 2, "method": "resources/list" }));

    let first_session = run_session(&registry_dir, &requests);
    let first = first_session.response(2);
    let cursor = first["result"]["nextCursor"]
        .as_str()
        .expect("続きがある場合は nextCursor を返す")
        .to_string();
    let first_uris = listed_uris(&first);
    // 先頭ページはインスタンス一覧 1 件と edit-info 100 件。
    assert_eq!(first_uris.len(), 101, "{}", first_uris.len());
    assert!(first_uris.contains(&"aviutl2://instances".to_string()));

    let mut next_requests = initialize_requests();
    next_requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "resources/list",
        "params": { "cursor": cursor },
    }));
    let second_session = run_session(&registry_dir, &next_requests);
    let second = second_session.response(2);
    let second_uris = listed_uris(&second);
    assert_eq!(second_uris.len(), total - 100, "{}", second_uris.len());
    assert!(
        second["result"]["nextCursor"].is_null(),
        "最終ページに cursor は付かない"
    );

    // 2 ページで全件を重複なく覆う。
    let mut all: Vec<&String> = first_uris.iter().chain(second_uris.iter()).collect();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), total + 1);

    let _ = std::fs::remove_dir_all(&registry_dir);
}

#[test]
fn invalid_resources_list_cursor_is_rejected() {
    let registry_dir = temp_registry_dir();
    std::fs::create_dir_all(&registry_dir).expect("registry を作れる");

    let mut requests = initialize_requests();
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "resources/list",
        "params": { "cursor": "not-a-cursor" },
    }));

    let session = run_session(&registry_dir, &requests);
    assert!(session.response(2)["error"].is_object());

    let _ = std::fs::remove_dir_all(&registry_dir);
}

#[test]
fn unreachable_instance_resource_reports_not_found_with_details() {
    let registry_dir = temp_registry_dir();
    // 登録はあるが pipe を待ち受けないため、読み取り時に生存確認で落ちる。
    let instance_id = write_bare_descriptor(&registry_dir);

    let mut requests = initialize_requests();
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "resources/read",
        "params": { "uri": format!("aviutl2://instances/{instance_id}/edit-info") },
    }));

    let session = run_session(&registry_dir, &requests);
    let error = &session.response(2)["error"];
    assert!(error.is_object(), "{}", session.response(2));
    assert_eq!(error["data"]["code"], json!("instance_stale"));
    assert_eq!(error["data"]["retryable"], json!(true));
    assert!(error["data"]["correlation_id"].is_string());

    let _ = std::fs::remove_dir_all(&registry_dir);
}

#[test]
fn tool_call_over_stdio_reaches_the_instance() {
    let registry_dir = temp_registry_dir();
    let edit_info = serde_json::to_value(sample_edit_info()).expect("直列化できる");
    let mock = MockPipeServer::start_with_operations(
        InstanceId::new_v4(),
        AuthSecret::generate(),
        std::process::id(),
        current_process_created_at(),
        InstanceState::Ready,
        OperationResponses::from([("get_edit_info".to_string(), ok_result(edit_info.clone()))]),
    );
    mock.write_descriptor(&registry_dir);
    std::thread::sleep(MOCK_STARTUP_GRACE);

    let mut requests = initialize_requests();
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "aviutl2_get_edit_info",
            "arguments": { "instance_id": mock.instance_id().to_string() },
        },
    }));

    let session = run_session(&registry_dir, &requests);
    let call = session.response(2);
    assert_eq!(call["result"]["isError"], json!(false), "{call}");
    assert_eq!(call["result"]["structuredContent"], edit_info);

    let requests = mock.received_requests();
    assert!(
        requests.iter().any(|r| r.operation == "get_edit_info"),
        "read operation が届いていません: {requests:?}"
    );

    // tool call の相関 ID と IPC の request_id を同じログ行から辿れる。
    let correlated = session
        .stderr
        .lines()
        .find(|line| line.contains("request_id"))
        .unwrap_or_else(|| panic!("request_id のログがありません: {}", session.stderr));
    assert!(
        correlated.contains("correlation_id"),
        "request_id が相関 ID と結び付いていません: {correlated}"
    );

    drop(mock);
    let _ = std::fs::remove_dir_all(&registry_dir);
}

#[test]
fn logs_expose_neither_full_identifiers_nor_absolute_paths() {
    let registry_dir = temp_registry_dir();
    let edit_info = serde_json::to_value(sample_edit_info()).expect("直列化できる");
    let mock = MockPipeServer::start_with_operations(
        InstanceId::new_v4(),
        AuthSecret::generate(),
        std::process::id(),
        current_process_created_at(),
        InstanceState::Ready,
        OperationResponses::from([("get_edit_info".to_string(), ok_result(edit_info))]),
    );
    mock.write_descriptor(&registry_dir);
    std::thread::sleep(MOCK_STARTUP_GRACE);

    let instance_id = mock.instance_id().to_string();
    let mut requests = initialize_requests();
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "aviutl2_get_edit_info",
            "arguments": { "instance_id": instance_id },
        },
    }));

    // 自クレートのログだけを対象にする。SDK が受信メッセージを出す経路は別問題。
    let session = run_session_with_log(&registry_dir, &requests, Some("aviutl2_mcp_server=trace"));

    let anonymized: String = instance_id.chars().take(8).collect();
    assert!(
        session.stderr.contains(&anonymized),
        "匿名化した instance_id が記録されていません: {}",
        session.stderr
    );
    assert!(
        !session.stderr.contains(&instance_id),
        "完全な instance_id がログに出ています: {}",
        session.stderr
    );
    let registry_path = registry_dir.to_string_lossy().to_string();
    assert!(
        !session.stderr.contains(&registry_path),
        "registry の絶対パスがログに出ています: {}",
        session.stderr
    );

    drop(mock);
    let _ = std::fs::remove_dir_all(&registry_dir);
}
