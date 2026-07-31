//! stdio 越しの MCP セッションを実プロセスで確認する。
//!
//! stdout に MCP メッセージ以外が混じらないこと、resource の list / read が
//! 往復することを、ログ出力を最大にした状態で検証する。

mod support;

use aviutl2_mcp_core::{
    AuthSecret, Cursor, DisplayRange, EditInfo, ErrorCode, ErrorObject, Extent, FiniteF64,
    FrameRange, InstanceId, InstanceState, SceneInfo,
};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use support::{
    MOCK_STARTUP_GRACE, MockPipeServer, OperationResponses, current_process_created_at, err_result,
    ok_result, remove_test_registry, temp_registry_dir, write_bare_descriptor,
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
    let mut server = ServerProcess::start(registry_dir, rust_log);
    for request in requests {
        server.send(request);
        if let Some(id) = request.get("id") {
            server.read_until(std::slice::from_ref(id));
        }
    }
    server.finish()
}

/// `first` の実行中に `second` を送り込むセッション。
///
/// サーバーは要求ごとに処理を起こすため、応答を待たずに送れば実行が重なる。
/// `gap` は `first` がインスタンスへ接続し終えるまでの待ちで、これにより
/// `second` は必ず接続済みの pipe に出会う。plugin の pipe は同時 1 接続しか
/// 受け付けないため、実クライアントでも起こり得る競合をそのまま再現する。
fn run_overlapping_session(
    registry_dir: &Path,
    first: &Value,
    gap: std::time::Duration,
    second: &Value,
) -> Session {
    let mut server = ServerProcess::start(registry_dir, Some("trace"));
    for request in initialize_requests() {
        server.send(&request);
        if let Some(id) = request.get("id") {
            server.read_until(std::slice::from_ref(id));
        }
    }

    server.send(first);
    std::thread::sleep(gap);
    server.send(second);

    let ids: Vec<Value> = [first, second]
        .iter()
        .filter_map(|request| request.get("id").cloned())
        .collect();
    server.read_until(&ids);
    server.finish()
}

/// 起動したサーバープロセスと、その stdio。
struct ServerProcess {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    stderr_reader: std::thread::JoinHandle<String>,
    stdout: String,
}

impl ServerProcess {
    /// registry と `RUST_LOG` を指定してサーバーを起こす。
    fn start(registry_dir: &Path, rust_log: Option<&str>) -> Self {
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

        let stdin = child.stdin.take().expect("stdin を得られる");
        let reader = BufReader::new(child.stdout.take().expect("stdout を得られる"));
        // stderr を読み続けないとログでパイプが詰まる。
        let mut stderr_pipe = child.stderr.take().expect("stderr を得られる");
        let stderr_reader = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr_pipe.read_to_string(&mut text);
            text
        });

        Self {
            child,
            stdin,
            reader,
            stderr_reader,
            stdout: String::new(),
        }
    }

    /// 要求を 1 件送る。応答は待たない。
    fn send(&mut self, request: &Value) {
        let line = serde_json::to_string(request).expect("直列化できる");
        writeln!(self.stdin, "{line}").expect("要求を書き込める");
        self.stdin.flush().expect("要求を送出できる");
    }

    /// 指定した id の応答が揃うまで stdout を読む。
    fn read_until(&mut self, ids: &[Value]) {
        let mut pending: Vec<Value> = ids.to_vec();
        while !pending.is_empty() {
            let mut line = String::new();
            if self.reader.read_line(&mut line).expect("stdout を読める") == 0 {
                break;
            }
            self.stdout.push_str(&line);
            if let Ok(message) = serde_json::from_str::<Value>(line.trim_end())
                && let Some(id) = message.get("id")
            {
                pending.retain(|awaited| awaited != id);
            }
        }
    }

    /// stdin を閉じてサーバーの終了を待ち、記録を返す。
    fn finish(self) -> Session {
        let Self {
            mut child,
            stdin,
            mut reader,
            stderr_reader,
            mut stdout,
        } = self;
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

/// tool 名から期待する `readOnlyHint` / `destructiveHint` / `idempotentHint`。
///
/// 未知の tool は落とす。tool を足したときに annotation の検査から漏れないようにする。
fn expected_annotations(name: &str) -> (bool, bool, bool) {
    match name {
        "aviutl2_list_instances"
        | "aviutl2_get_edit_info"
        | "aviutl2_get_current_scene"
        | "aviutl2_list_layers"
        | "aviutl2_list_objects"
        | "aviutl2_get_object"
        | "aviutl2_list_available_effects"
        // 描画はプロジェクトを変更せず、同じ要求は同じ絵を返す。
        | "aviutl2_render_frame" => (true, false, true),
        // 作成系は再送で重複し得るため冪等と名乗らない。一括適用も、冪等かどうかが
        // 中身に依存する以上、安全である側を主張しない。
        "aviutl2_create_object" | "aviutl2_add_effect" | "aviutl2_apply_batch" => {
            (false, false, false)
        }
        "aviutl2_delete_object" | "aviutl2_delete_effect" => (false, true, true),
        "aviutl2_move_object"
        | "aviutl2_set_object_name"
        | "aviutl2_set_object_item"
        | "aviutl2_set_effect_enabled"
        | "aviutl2_set_layer_state"
        | "aviutl2_set_selection" => (false, false, true),
        other => panic!("{other} の annotation が定義されていません"),
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

    // annotation と outputSchema はワイヤ形式で確かめる。SDK の直列化が変われば
    // Rust の構造体を見るだけでは気づけない。
    let listed = session.response(2);
    let tools = listed["result"]["tools"].as_array().expect("tools は配列");
    assert!(!tools.is_empty());
    for tool in tools {
        let name = tool["name"].as_str().expect("name がある").to_string();
        // 未知の tool 名は [`expected_annotations`] が落とす。tool を足したときに
        // この検査から漏れない。
        let (read_only, destructive, idempotent) = expected_annotations(&name);
        assert_eq!(
            tool["annotations"]["readOnlyHint"],
            json!(read_only),
            "{name}"
        );
        assert_eq!(
            tool["annotations"]["destructiveHint"],
            json!(destructive),
            "{name}"
        );
        assert_eq!(
            tool["annotations"]["idempotentHint"],
            json!(idempotent),
            "{name}"
        );
        assert_eq!(tool["annotations"]["openWorldHint"], json!(false), "{name}");
        assert!(
            tool["outputSchema"]["properties"].is_object(),
            "{name} の outputSchema がありません"
        );
        assert_eq!(
            tool["inputSchema"]["additionalProperties"],
            json!(false),
            "{name}"
        );
    }

    let call = session.response(3);
    assert_eq!(call["result"]["isError"], json!(false));

    remove_test_registry(&registry_dir);
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

    remove_test_registry(&registry_dir);
}

/// `tools/list` の応答から指定 tool の定義を取り出す。
fn listed_tool(response: &Value, name: &str) -> Value {
    response["result"]["tools"]
        .as_array()
        .expect("tools は配列")
        .iter()
        .find(|tool| tool["name"] == json!(name))
        .unwrap_or_else(|| panic!("{name} が登録されていません"))
        .clone()
}

#[test]
fn effect_catalog_paging_is_not_declared_as_revision_checked() {
    let registry_dir = temp_registry_dir();
    std::fs::create_dir_all(&registry_dir).expect("registry を作れる");

    let mut requests = initialize_requests();
    requests.push(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));

    let session = run_session(&registry_dir, &requests);
    let listed = session.response(2);

    // effect カタログはプロジェクトの revision に連動しないため照合されない。
    // 照合されるかのように案内すると、返らない precondition_failed をもって
    // 「ページ間の一貫性が検証された」と読まれてしまう。
    let effects = listed_tool(&listed, "aviutl2_list_available_effects");
    let description = effects["description"].as_str().expect("説明がある");
    assert!(
        !description.contains("先頭ページが返した snapshot_revision を添える"),
        "照合されない値を添えるよう促しています: {description}"
    );
    assert!(
        description.contains("照合には用いない"),
        "照合しないことが説明されていません: {description}"
    );

    let field = &effects["inputSchema"]["properties"]["snapshot_revision"];
    assert!(
        field.is_object(),
        "互換のため snapshot_revision は受理し続ける: {effects}"
    );
    let field_description = field["description"].as_str().expect("説明がある");
    assert!(
        !field_description.contains("precondition_failed"),
        "返らない失敗を宣言しています: {field_description}"
    );
    assert!(
        field_description.contains("照合に用いない"),
        "照合しないことが説明されていません: {field_description}"
    );

    // 照合する列挙 tool の宣言はそのまま残す。
    for name in ["aviutl2_list_layers", "aviutl2_list_objects"] {
        let tool = listed_tool(&listed, name);
        let description = tool["description"].as_str().expect("説明がある");
        assert!(
            description.contains("先頭ページが返した snapshot_revision"),
            "{name}: {description}"
        );
        let field_description =
            tool["inputSchema"]["properties"]["snapshot_revision"]["description"]
                .as_str()
                .expect("説明がある");
        assert!(
            field_description.contains("precondition_failed"),
            "{name}: {field_description}"
        );
    }

    remove_test_registry(&registry_dir);
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
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "aviutl2_list_layers",
            "arguments": {
                "instance_id": instance_id,
                "expected_scene_id": "文字列",
            },
        },
    }));
    // 拒否の説明にはクライアントが送ったキー名がそのまま現れるため、巨大な
    // キーを送れば text content の上限を破れてしまわないかを確かめる。
    let mut huge_arguments = serde_json::Map::new();
    huge_arguments.insert("k".repeat(HUGE_ARGUMENT_KEY_CHARS), json!(1));
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tools/call",
        "params": {
            "name": "aviutl2_list_instances",
            "arguments": Value::Object(huge_arguments),
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
    assert_structured_invalid_argument(&unknown_field);

    // schema の範囲は server 側でも検証し、構造化したエラーを返す。
    let out_of_range = session.response(4);
    assert_eq!(out_of_range["result"]["isError"], json!(true));
    assert_structured_invalid_argument(&out_of_range);

    // 型不一致も引数の復元に失敗する経路であり、同じ形の失敗として返る。
    let type_mismatch = session.response(5);
    assert_eq!(type_mismatch["result"]["isError"], json!(true));
    assert_structured_invalid_argument(&type_mismatch);

    let huge_key = session.response(6);
    assert_eq!(huge_key["result"]["isError"], json!(true));
    assert_structured_invalid_argument(&huge_key);
    let text = huge_key["result"]["content"][0]["text"]
        .as_str()
        .expect("text がある");
    assert!(
        text.chars().count() <= MAX_TOOL_TEXT_CHARS,
        "text content が上限を超えています: {}",
        text.chars().count()
    );

    // この経路を通ったことは stderr の構造化ログから追える。
    let logged: Vec<&str> = session
        .stderr
        .lines()
        .filter(|line| line.contains("tool call rejected before dispatch"))
        .collect();
    for tool in ["aviutl2_list_layers", "aviutl2_list_instances"] {
        let line = logged
            .iter()
            .find(|line| line.contains(tool))
            .unwrap_or_else(|| panic!("{tool} の拒否が記録されていません: {logged:?}"));
        assert!(line.contains("correlation_id"), "{line}");
    }
    // クライアント由来の文字列全文はログへ出さない。
    for line in &logged {
        assert!(
            !line.contains(&"k".repeat(1_000)),
            "クライアントが送った文字列がログに出ています"
        );
    }

    remove_test_registry(&registry_dir);
}

#[test]
fn rejected_edit_tool_calls_take_the_same_path() {
    // 編集 tool でも引数の復元に失敗する経路は自前の `call_tool` が捕捉する。
    // ここを取りこぼすと、拒否の説明にクライアントが送ったキー名がそのまま
    // 現れたまま、上限も構造化も適用されない応答が返る。
    let registry_dir = temp_registry_dir();
    std::fs::create_dir_all(&registry_dir).expect("registry を作れる");
    let instance_id = InstanceId::new_v4().to_string();
    let epoch = "78be92d1-c8c9-44c6-ae52-387548971468";

    let mut requests = initialize_requests();
    // 未知フィールド。
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "aviutl2_delete_object",
            "arguments": {
                "instance_id": instance_id,
                "selector": {},
                "future": 1,
            },
        },
    }));
    // 前提の epoch を持たない作成。セレクターを持たない要求では必須である。
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "aviutl2_create_object",
            "arguments": {
                "instance_id": instance_id,
                "source": { "type": "object_alias", "alias": "[vo]" },
                "placement": { "scene_id": 0, "layer": 1, "frame": 0 },
            },
        },
    }));
    // 変更内容の全省略は tool 本体の検証で落ちる。
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "aviutl2_set_selection",
            "arguments": {
                "instance_id": instance_id,
                "expected_scene_id": 0,
                "expected_project_epoch": epoch,
            },
        },
    }));
    // 型を取り違えた値。復元に失敗した理由へ値そのものが現れる経路である。
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "aviutl2_create_object",
            "arguments": {
                "instance_id": instance_id,
                "source": { "type": "object_alias", "alias": "[vo]" },
                "placement": { "scene_id": 0, "layer": SECRET_ARGUMENT, "frame": 0 },
                "expected_project_epoch": epoch,
            },
        },
    }));

    let session = run_session(&registry_dir, &requests);
    for id in [2, 3, 4, 5] {
        let response = session.response(id);
        assert_eq!(response["result"]["isError"], json!(true), "{response}");
        assert_structured_invalid_argument(&response);
    }

    let unknown_field = session.response(2);
    let message = unknown_field["result"]["content"][0]["text"]
        .as_str()
        .expect("text がある");
    assert!(
        message.contains("future"),
        "未知フィールドの拒否理由: {message}"
    );

    // 受け取った値そのものは応答へ反響させない。編集 tool の引数は alias・
    // パス・設定値であり、Phase 2 の read tool より漏れたときの損失が大きい。
    let type_mismatch = session.response(5);
    let echoed = serde_json::to_string(&type_mismatch).expect("直列化できる");
    assert!(
        !echoed.contains(SECRET_ARGUMENT),
        "受け取った値が応答に含まれています: {echoed}"
    );

    remove_test_registry(&registry_dir);
}

/// 型を取り違えて送られた、応答へ反響してはならない値。
const SECRET_ARGUMENT: &str = "秘密のレイヤー値";

/// 引数を解釈できなかった tool call へ送るキー名の長さ。
const HUGE_ARGUMENT_KEY_CHARS: usize = 100_000;

/// 1 応答の text content に許す最大文字数。
const MAX_TOOL_TEXT_CHARS: usize = 25_000;

/// tool result が構造化した `invalid_argument` を運ぶことを確かめる。
fn assert_structured_invalid_argument(response: &Value) {
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["code"], json!("invalid_argument"), "{response}");
    assert_eq!(structured["retryable"], json!(false), "{response}");
    assert!(
        structured["correlation_id"]
            .as_str()
            .is_some_and(|id| id.len() == 36),
        "correlation_id がありません: {structured}"
    );
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
    remove_test_registry(&registry_dir);
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

    remove_test_registry(&registry_dir);
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

    remove_test_registry(&registry_dir);
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

    remove_test_registry(&registry_dir);
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

    remove_test_registry(&registry_dir);
}

/// resource が返す protocol error の JSON-RPC コード。
const RESOURCE_NOT_FOUND_CODE: i32 = rmcp::model::ErrorCode::RESOURCE_NOT_FOUND.0;

#[test]
fn busy_instance_resource_reports_not_found_with_retry_after() {
    let registry_dir = temp_registry_dir();
    // 生存はしているが読み取りには応じられないインスタンス。生存確認の ping は
    // 通るため、失敗は resource の読み取り経路そのもので起きる。
    let mock = MockPipeServer::start_with_operations(
        InstanceId::new_v4(),
        AuthSecret::generate(),
        std::process::id(),
        current_process_created_at(),
        InstanceState::Ready,
        OperationResponses::from([(
            "get_edit_info".to_string(),
            err_result(
                ErrorObject::new(ErrorCode::HostBusy, "読み取りキューが飽和しています", true)
                    .with_details(json!({ "retry_after_ms": 500 })),
            ),
        )]),
    );
    mock.write_descriptor(&registry_dir);
    std::thread::sleep(MOCK_STARTUP_GRACE);

    let mut requests = initialize_requests();
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "resources/read",
        "params": { "uri": format!("aviutl2://instances/{}/edit-info", mock.instance_id()) },
    }));

    let session = run_session(&registry_dir, &requests);
    let response = session.response(2);
    let error = &response["error"];
    assert!(error.is_object(), "{response}");
    // 待てば取得し得る失敗を server 自身の不具合として返さない。
    assert_eq!(error["code"], json!(RESOURCE_NOT_FOUND_CODE), "{response}");
    assert_eq!(error["data"]["code"], json!("host_busy"), "{response}");
    assert_eq!(error["data"]["retryable"], json!(true), "{response}");
    assert_eq!(
        error["data"]["details"]["retry_after_ms"],
        json!(500),
        "{response}"
    );
    assert!(error["data"]["correlation_id"].is_string(), "{response}");

    drop(mock);
    remove_test_registry(&registry_dir);
}

/// インスタンス一覧の resource を上限超えにするための表示名の長さ。
const LONG_DISPLAY_NAME_CHARS: usize = 4_000;

/// 上限超過を起こすために登録する生存インスタンスの数。
const CROWDED_INSTANCES: usize = 10;

/// 表示名を差し替えた descriptor を registry へ書く。
fn write_descriptor_with_display_name(
    mock: &MockPipeServer,
    registry_dir: &Path,
    display_name: String,
) {
    let mut descriptor = mock.descriptor(registry_dir.to_path_buf());
    let project = descriptor
        .project
        .as_mut()
        .expect("mock の descriptor は project を持つ");
    project.display_name = display_name;
    std::fs::create_dir_all(registry_dir).expect("registry を作れる");
    std::fs::write(
        registry_dir.join(format!("{}.json", descriptor.instance_id)),
        serde_json::to_string(&descriptor).expect("直列化できる"),
    )
    .expect("descriptor を書ける");
}

#[test]
fn crowded_instances_resource_stays_within_the_text_limit() {
    let registry_dir = temp_registry_dir();
    let mocks: Vec<MockPipeServer> = (0..CROWDED_INSTANCES)
        .map(|_| {
            MockPipeServer::start(
                InstanceId::new_v4(),
                AuthSecret::generate(),
                std::process::id(),
                current_process_created_at(),
                InstanceState::Ready,
            )
        })
        .collect();
    for mock in &mocks {
        write_descriptor_with_display_name(
            mock,
            &registry_dir,
            "名".repeat(LONG_DISPLAY_NAME_CHARS),
        );
    }
    std::thread::sleep(MOCK_STARTUP_GRACE);

    let mut requests = initialize_requests();
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "resources/read",
        "params": { "uri": "aviutl2://instances" },
    }));

    let session = run_session(&registry_dir, &requests);
    let response = session.response(2);
    let text = response["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("resource の内容がありません: {response}"));
    // resource は tool result と別経路であり、tool 側の切り詰めに守られていない。
    assert!(
        text.chars().count() <= MAX_TOOL_TEXT_CHARS,
        "resource の text が上限を超えています: {}",
        text.chars().count()
    );

    let decoded: Value = serde_json::from_str(text).expect("JSON として読める");
    // 上限に収めるために内容ごと捨ててはならない。総件数と続きの位置を示し、
    // 残りは一覧 tool のページ指定で取得できる形にする。
    assert_eq!(
        decoded["total_count"],
        json!(CROWDED_INSTANCES),
        "{decoded}"
    );
    let listed = decoded["instances"].as_array().expect("instances は配列");
    assert!(!listed.is_empty(), "内容が丸ごと落ちています: {decoded}");
    assert!(
        listed.len() < CROWDED_INSTANCES,
        "件数が絞られていません: {}",
        listed.len()
    );
    assert_eq!(decoded["has_more"], json!(true), "{decoded}");
    assert!(decoded["next_offset"].is_number(), "{decoded}");

    drop(mocks);
    remove_test_registry(&registry_dir);
}

/// 編集情報の resource を上限超えにするための grid_bpm の要素数。
const OVERSIZED_GRID_BPM: usize = 5_000;

#[test]
fn oversized_edit_info_resource_reports_truncation_as_readable_json() {
    let registry_dir = temp_registry_dir();
    let mut edit_info = sample_edit_info();
    edit_info.grid_bpm = (0..OVERSIZED_GRID_BPM)
        .map(|index| FiniteF64::try_new(120.0 + index as f64).expect("有限値"))
        .collect();
    let mock = MockPipeServer::start_with_operations(
        InstanceId::new_v4(),
        AuthSecret::generate(),
        std::process::id(),
        current_process_created_at(),
        InstanceState::Ready,
        OperationResponses::from([(
            "get_edit_info".to_string(),
            ok_result(serde_json::to_value(&edit_info).expect("直列化できる")),
        )]),
    );
    mock.write_descriptor(&registry_dir);
    std::thread::sleep(MOCK_STARTUP_GRACE);

    let mut requests = initialize_requests();
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "resources/read",
        "params": { "uri": format!("aviutl2://instances/{}/edit-info", mock.instance_id()) },
    }));

    let session = run_session(&registry_dir, &requests);
    let response = session.response(2);
    let text = response["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("resource の内容がありません: {response}"));
    assert!(
        text.chars().count() <= MAX_TOOL_TEXT_CHARS,
        "resource の text が上限を超えています: {}",
        text.chars().count()
    );

    // 途中で切ると JSON として読めなくなるため、超過した事実を返す。
    let decoded: Value = serde_json::from_str(text).expect("JSON として読める");
    assert_eq!(decoded["truncated"], json!(true), "{decoded}");
    assert_eq!(
        decoded["max_chars"],
        json!(MAX_TOOL_TEXT_CHARS),
        "{decoded}"
    );

    drop(mock);
    remove_test_registry(&registry_dir);
}

/// インスタンスが read operation を処理している時間。
///
/// この間その pipe は塞がるため、後続の接続は待たされる。1 往復に要する時間より
/// 十分長く、要求の期限（5 秒）より十分短い値を選ぶ。
const BUSY_WHILE_READING: std::time::Duration = std::time::Duration::from_millis(600);

/// 先の要求がインスタンスへ接続し終えるまでの待ち。
///
/// 接続・handshake・ping はミリ秒で終わるため、この待ちの後は必ず pipe が
/// 塞がっている。[`BUSY_WHILE_READING`] より十分短くし、read の実行中に
/// 次の要求が届くようにする。
const CONNECT_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

/// `resources/read` の内容を JSON として取り出す。
fn read_resource_contents(response: &Value) -> Value {
    let text = response["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("resource の内容がありません: {response}"));
    serde_json::from_str(text).expect("JSON として読める")
}

#[test]
fn instance_listing_and_tool_call_survive_overlapping() {
    let registry_dir = temp_registry_dir();
    let edit_info = serde_json::to_value(sample_edit_info()).expect("直列化できる");
    // 読み取りに時間の掛かるインスタンスを演じさせ、その最中に一覧を要求する。
    let mock = MockPipeServer::start_with_delayed_operations(
        InstanceId::new_v4(),
        AuthSecret::generate(),
        std::process::id(),
        current_process_created_at(),
        InstanceState::Ready,
        OperationResponses::from([("get_edit_info".to_string(), ok_result(edit_info.clone()))]),
        BUSY_WHILE_READING,
    );
    mock.write_descriptor(&registry_dir);
    std::thread::sleep(MOCK_STARTUP_GRACE);

    // 一覧は候補へ接続して生存確認するため、実行中の read 要求と pipe を奪い合う。
    let session = run_overlapping_session(
        &registry_dir,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "aviutl2_get_edit_info",
                "arguments": { "instance_id": mock.instance_id().to_string() },
            },
        }),
        CONNECT_GRACE,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "resources/read",
            "params": { "uri": "aviutl2://instances" },
        }),
    );

    // 一覧が届いた時点で pipe は read 要求に占有されている。
    let operations: Vec<String> = mock
        .received_requests()
        .iter()
        .map(|request| request.operation.clone())
        .collect();
    assert_eq!(
        operations.first().map(String::as_str),
        Some("ping"),
        "{operations:?}"
    );
    assert_eq!(
        operations.get(1).map(String::as_str),
        Some("get_edit_info"),
        "read 要求が先に pipe を占有していません: {operations:?}"
    );

    // 実行中の read 要求は割り込まれても壊れない。
    let call = session.response(2);
    assert_eq!(call["result"]["isError"], json!(false), "{call}");
    assert_eq!(call["result"]["structuredContent"], edit_info);

    // 一覧側も応答を返す。pipe が空くのを待って生存確認できることもあれば、
    // 待ちきれず候補から外れることもあるが、後者でも取り直しへ誘導する
    // retryable な失敗にとどまり、内部エラーにはしない。
    let listed = session.response(3);
    if listed["error"].is_object() {
        assert_eq!(listed["error"]["data"]["code"], json!("instance_stale"));
        assert_eq!(
            listed["error"]["data"]["retryable"],
            json!(true),
            "{listed}"
        );
    } else {
        let contents = read_resource_contents(&listed);
        assert_eq!(contents["total_count"], json!(1), "{contents}");
    }

    // 競合はその 1 回に留まり、登録は失われない。取り直せば一覧にも read にも応じる。
    let mut retry = initialize_requests();
    retry.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "resources/read",
        "params": { "uri": "aviutl2://instances" },
    }));
    retry.push(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "aviutl2_get_edit_info",
            "arguments": { "instance_id": mock.instance_id().to_string() },
        },
    }));
    let after = run_session(&registry_dir, &retry);
    let contents = read_resource_contents(&after.response(2));
    assert_eq!(
        contents["total_count"],
        json!(1),
        "競合の後にインスタンスが失われています: {contents}"
    );
    assert_eq!(
        after.response(3)["result"]["isError"],
        json!(false),
        "競合の後に read が通りません: {}",
        after.response(3)
    );

    drop(mock);
    remove_test_registry(&registry_dir);
}

/// 編集が pipe を占有する時間。
///
/// 編集は read より長く占有するため競合の窓が広い。read の対比
/// （[`BUSY_WHILE_READING`]）より長くし、その差が実際に効くようにする。
const BUSY_WHILE_EDITING: std::time::Duration = std::time::Duration::from_millis(1_200);

/// 競合の窓が read より広いという関係を、コンパイル時に固定する。
///
/// 片方だけを縮めると、編集の競合を read と同じ長さでしか試さなくなる。
const _: () = assert!(BUSY_WHILE_EDITING.as_millis() > BUSY_WHILE_READING.as_millis());

/// どちらの遅延も、接続が確立するまでの待ちより十分長いこと。
///
/// 短いと後続の要求が届く前に pipe が空き、競合そのものが起きない。
const _: () = assert!(BUSY_WHILE_READING.as_millis() > CONNECT_GRACE.as_millis() * 2);

#[test]
fn edit_tool_call_and_resource_read_survive_overlapping() {
    let registry_dir = temp_registry_dir();
    let outcome = json!({
        "project_epoch": "78be92d1-c8c9-44c6-ae52-387548971468",
        "project_revision": 43,
        "object": null,
        "effect": null,
        "created": [],
    });
    // 編集に時間の掛かるインスタンスを演じさせ、その最中に resource を読む。
    let mock = MockPipeServer::start_with_delayed_operations(
        InstanceId::new_v4(),
        AuthSecret::generate(),
        std::process::id(),
        current_process_created_at(),
        InstanceState::Ready,
        OperationResponses::from([("delete_object".to_string(), ok_result(outcome.clone()))]),
        BUSY_WHILE_EDITING,
    );
    mock.write_descriptor(&registry_dir);
    std::thread::sleep(MOCK_STARTUP_GRACE);

    let session = run_overlapping_session(
        &registry_dir,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "aviutl2_delete_object",
                "arguments": {
                    "instance_id": mock.instance_id().to_string(),
                    "selector": {
                        "project_epoch": "78be92d1-c8c9-44c6-ae52-387548971468",
                        "scene_id": 3,
                        "layer": 2,
                        "frame": 120,
                        "name": null,
                        "fingerprint": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                        // セレクターは往復型であり、要求の一部ではない算出方式を
                        // 名乗っても入口で拒否されない。
                        "fingerprint_algorithm": "sha256-raw-v1",
                    },
                },
            },
        }),
        CONNECT_GRACE,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "resources/read",
            "params": { "uri": "aviutl2://instances" },
        }),
    );

    let operations: Vec<String> = mock
        .received_requests()
        .iter()
        .map(|request| request.operation.clone())
        .collect();
    assert_eq!(
        operations.get(1).map(String::as_str),
        Some("delete_object"),
        "編集要求が先に pipe を占有していません: {operations:?}"
    );

    // 実行中の編集は割り込まれても壊れず、結果を落とさない。適用済みの変更を
    // 隠すと、要求元は自分の変更が入ったのかを知れないままになる。
    let call = session.response(2);
    assert_eq!(call["result"]["isError"], json!(false), "{call}");
    assert_eq!(call["result"]["structuredContent"], outcome);

    // resource 側も応答を返す。pipe が空くのを待って生存確認できることも、
    // 待ちきれず候補から外れることもあるが、後者でも取り直しへ誘導する
    // retryable な失敗にとどまる。
    let listed = session.response(3);
    if listed["error"].is_object() {
        assert_eq!(listed["error"]["data"]["code"], json!("instance_stale"));
        assert_eq!(
            listed["error"]["data"]["retryable"],
            json!(true),
            "{listed}"
        );
    } else {
        let contents = read_resource_contents(&listed);
        assert_eq!(contents["total_count"], json!(1), "{contents}");
    }

    // 競合はその 1 回に留まり、登録も編集経路も失われない。
    let mut retry = initialize_requests();
    retry.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "resources/read",
        "params": { "uri": "aviutl2://instances" },
    }));
    let after = run_session(&registry_dir, &retry);
    let contents = read_resource_contents(&after.response(2));
    assert_eq!(
        contents["total_count"],
        json!(1),
        "競合の後にインスタンスが失われています: {contents}"
    );

    drop(mock);
    remove_test_registry(&registry_dir);
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
    remove_test_registry(&registry_dir);
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
    remove_test_registry(&registry_dir);
}

#[test]
fn verbose_logging_does_not_leak_full_identifiers_from_any_crate() {
    // 不具合の報告時に最も自然な操作は `RUST_LOG=debug` である。要求本文を
    // そのまま出す依存 crate があると、匿名化した識別子だけを記録している
    // 自クレートの努力が迂回される。対象を絞らずに stderr 全体を検査する。
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

    let session = run_session_with_log(&registry_dir, &requests, Some("debug"));

    // 応答は届く。ログの絞り込みが処理そのものを妨げていないことを確かめる。
    assert!(
        session.response(2)["result"]["isError"] != json!(true),
        "tool call が失敗しています: {}",
        session.stdout
    );
    assert!(
        !session.stderr.contains(&instance_id),
        "完全な instance_id がログに出ています: {}",
        session.stderr
    );

    drop(mock);
    remove_test_registry(&registry_dir);
}
