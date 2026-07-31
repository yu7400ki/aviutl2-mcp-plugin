//! 描画 tool から IPC の render operation への変換と、成果物の引き取りを確認する。

mod support;

use aviutl2_mcp_core::{
    AuthSecret, ErrorCode, ErrorObject, InstanceId, InstanceState, RenderFrameResult,
    RequestEnvelope,
};
use aviutl2_mcp_server::artifact::{ARTIFACT_MEDIA_TYPE, ArtifactStore, base_dir_for_registry};
use aviutl2_mcp_server::mcp::render::{RenderFormatInput, RenderFrameInput};
use aviutl2_mcp_server::mcp::{AviUtl2McpServer, CallLimits};
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use support::{
    MOCK_STARTUP_GRACE, MockPipeServer, OperationResponses, current_process_created_at, err_result,
    ok_result, remove_test_registry, temp_registry_dir,
};

const EPOCH: &str = "78be92d1-c8c9-44c6-ae52-387548971468";
const SCENE_ID: i32 = 3;
const FRAME: u32 = 120;

/// 応答が名乗る引き渡しの識別子。応答にもログにも現れてはならない。
const HANDOFF_TOKEN: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";

/// 引き渡しファイルへ書く画像の中身。
const IMAGE_BYTES: &[u8] = b"\x89PNG\r\n\x1a\nfake-image-body";

/// 生存する mock インスタンスと、成果物の保管庫を持つ MCP サーバー。
///
/// **保管庫を掴んでいるものは、後始末で基底を消す前に全て閉じる必要がある。**
/// 保管庫は自分の session ディレクトリのロックファイルを共有なしで開いたまま
/// 保持するため、閉じないまま基底を消しにいくと走査がそのファイルで打ち切られ、
/// 一時ディレクトリが残る。サーバーも保管庫の参照を持つため、取り出せる形で持つ。
struct Harness {
    server: Option<AviUtl2McpServer>,
    store: Option<Arc<ArtifactStore>>,
    mock: MockPipeServer,
    registry_dir: PathBuf,
}

impl Harness {
    fn start(responses: OperationResponses) -> Self {
        Self::with_limits(responses, CallLimits::default())
    }

    fn with_limits(responses: OperationResponses, limits: CallLimits) -> Self {
        let registry_dir = temp_registry_dir();
        let mock = MockPipeServer::start_with_operations(
            InstanceId::new_v4(),
            AuthSecret::generate(),
            std::process::id(),
            current_process_created_at(),
            InstanceState::Ready,
            responses,
        );
        mock.write_descriptor(&registry_dir);
        std::thread::sleep(MOCK_STARTUP_GRACE);
        let store = Arc::new(
            ArtifactStore::open(base_dir_for_registry(&registry_dir)).expect("保管庫を開ける"),
        );
        Self {
            server: Some(AviUtl2McpServer::with_artifact_store(
                registry_dir.clone(),
                limits,
                Arc::clone(&store),
            )),
            store: Some(store),
            mock,
            registry_dir,
        }
    }

    /// 試験対象のサーバー。
    fn server(&self) -> &AviUtl2McpServer {
        self.server
            .as_ref()
            .expect("サーバーは後始末まで生きています")
    }

    /// 試験対象の保管庫。
    fn store(&self) -> &ArtifactStore {
        self.store.as_ref().expect("保管庫は後始末まで生きています")
    }

    fn instance_id(&self) -> String {
        self.mock.instance_id().to_string()
    }

    /// 引き渡しファイルのパス。
    ///
    /// server と同じ規則で組み立てる。要求経路からは材料が入らないため、
    /// 試験側も自分で組み立てるほかない。
    fn handoff_path(&self, token: &str) -> PathBuf {
        base_dir_for_registry(&self.registry_dir)
            .join("render")
            .join(self.mock.instance_id().to_string())
            .join(format!("{token}.png"))
    }

    /// 引き渡しファイルを書く。
    fn write_handoff(&self, token: &str, bytes: &[u8]) -> PathBuf {
        let path = self.handoff_path(token);
        std::fs::create_dir_all(path.parent().expect("親がある"))
            .expect("引き渡しディレクトリを作れる");
        std::fs::write(&path, bytes).expect("引き渡しファイルを書ける");
        path
    }

    /// 生存確認の ping を除いた要求。
    fn requests(&self) -> Vec<RequestEnvelope> {
        self.mock
            .received_requests()
            .into_iter()
            .filter(|request| request.operation != "ping")
            .collect()
    }

    fn only_request(&self) -> RequestEnvelope {
        let mut requests = self.requests();
        assert_eq!(requests.len(), 1, "{requests:?}");
        requests.remove(0)
    }

    async fn render(&self) -> CallToolResult {
        self.server()
            .aviutl2_render_frame(Parameters(RenderFrameInput {
                instance_id: self.instance_id(),
                expected_scene_id: SCENE_ID,
                frame: FRAME,
                format: RenderFormatInput::Png,
            }))
            .await
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // 保管庫を掴んでいるものを全て閉じてから基底を消す。サーバーも参照を
        // 持つため、片方だけ落としても保管庫は生き残る。
        drop(self.server.take());
        drop(self.store.take());
        remove_test_registry(&self.registry_dir);
    }
}

fn structured(result: &CallToolResult) -> Value {
    result
        .structured_content
        .clone()
        .expect("structuredContent がある")
}

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.clone())
        .expect("text content がある")
}

/// `"sha256:"` と小文字十六進のダイジェスト。
fn sha256_of(bytes: &[u8]) -> String {
    let mut value = "sha256:".to_string();
    for byte in Sha256::digest(bytes) {
        value.push_str(&format!("{byte:02x}"));
    }
    value
}

/// 接続先が返す描画の結果。
fn render_result(byte_length: u64, sha256: String) -> Value {
    serde_json::to_value(RenderFrameResult {
        project_epoch: EPOCH.to_string(),
        project_revision: 42,
        scene_id: SCENE_ID,
        frame: FRAME,
        width: 1920,
        height: 1080,
        media_type: ARTIFACT_MEDIA_TYPE.to_string(),
        byte_length,
        sha256,
        handoff_token: HANDOFF_TOKEN.to_string(),
    })
    .expect("直列化できる")
}

/// 申告が実体と一致する応答。
fn honest_render_result() -> OperationResponses {
    OperationResponses::from([(
        "render_frame".to_string(),
        ok_result(render_result(
            IMAGE_BYTES.len() as u64,
            sha256_of(IMAGE_BYTES),
        )),
    )])
}

#[tokio::test]
async fn render_tool_sends_the_scene_guard_and_takes_over_the_artifact() {
    let harness = Harness::start(honest_render_result());
    let handoff = harness.write_handoff(HANDOFF_TOKEN, IMAGE_BYTES);

    let result = harness.render().await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    let request = harness.only_request();
    assert_eq!(request.operation, "render_frame");
    assert_eq!(
        request.params,
        json!({ "expected_scene_id": SCENE_ID, "frame": FRAME, "format": "png" }),
    );

    let structured = structured(&result);
    // 描画時点のプロジェクトの世代は、そのまま次の要求の前提として使われる。
    assert_eq!(structured["project_epoch"], json!(EPOCH));
    assert_eq!(structured["project_revision"], json!(42));
    assert_eq!(structured["scene_id"], json!(SCENE_ID));
    assert_eq!(structured["frame"], json!(FRAME));
    assert_eq!(structured["width"], json!(1920));
    assert_eq!(structured["height"], json!(1080));
    assert_eq!(structured["artifact"]["media_type"], json!("image/png"));
    assert_eq!(
        structured["artifact"]["byte_length"],
        json!(IMAGE_BYTES.len())
    );
    assert_eq!(
        structured["artifact"]["sha256"],
        json!(sha256_of(IMAGE_BYTES))
    );

    // 識別子は保管庫が採番した値であり、引き渡しの識別子とは別物である。
    let artifact_id = structured["artifact"]["artifact_id"]
        .as_str()
        .expect("識別子がある")
        .to_string();
    assert_ne!(artifact_id, HANDOFF_TOKEN);
    assert_eq!(
        structured["artifact"]["uri"],
        json!(format!("aviutl2://artifacts/{artifact_id}"))
    );

    // 失効時刻は保管庫が定めた値である。作成時刻を返すと、要求元は読める間に
    // 読まず、あるいは読めない成果物を読もうとする。
    let stored = harness
        .store()
        .list()
        .into_iter()
        .find(|artifact| artifact.artifact_id == artifact_id)
        .expect("保管庫に登録されています");
    assert!(stored.expires_at > stored.created_at, "{stored:?}");
    assert_eq!(
        structured["artifact"]["expires_at"],
        json!(stored.expires_at.to_rfc3339())
    );

    // 所有権は 1 か所ずつ移る。引き渡し元は残らない。
    assert!(!handoff.exists(), "引き渡しファイルが残っています");
    assert_eq!(harness.store().len(), 1);
    let content = harness
        .store()
        .read(&artifact_id)
        .expect("成果物を読み出せます");
    assert_eq!(content.bytes, IMAGE_BYTES);
}

#[tokio::test]
async fn the_handoff_token_never_leaves_the_server() {
    let harness = Harness::start(honest_render_result());
    harness.write_handoff(HANDOFF_TOKEN, IMAGE_BYTES);

    let result = harness.render().await;

    // 応答全体を文字列として検査する。型を分けていても、直列化の経路が
    // 増えれば戻ってくる余地がある。
    let serialized = serde_json::to_string(&result).expect("直列化できる");
    assert!(
        !serialized.contains(HANDOFF_TOKEN),
        "引き渡しの識別子が応答に含まれています: {serialized}"
    );
    assert!(
        !text_of(&result).contains(HANDOFF_TOKEN),
        "引き渡しの識別子が text に含まれています"
    );
    // 保存先のパスも出さない。
    let base = base_dir_for_registry(&harness.registry_dir);
    assert!(
        !serialized.contains(&base.to_string_lossy().replace('\\', "\\\\")),
        "保存先のパスが応答に含まれています: {serialized}"
    );
}

#[tokio::test]
async fn a_declared_length_that_does_not_match_leaves_no_artifact_behind() {
    // 壊れた画像を成果物として配ると、要求元は原因を特定できない。
    let harness = Harness::start(OperationResponses::from([(
        "render_frame".to_string(),
        ok_result(render_result(
            IMAGE_BYTES.len() as u64 + 1,
            sha256_of(IMAGE_BYTES),
        )),
    )]));
    let handoff = harness.write_handoff(HANDOFF_TOKEN, IMAGE_BYTES);

    let result = harness.render().await;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)["code"], json!("internal_error"));
    assert!(harness.store().is_empty(), "成果物が作られています");
    assert!(!handoff.exists(), "引き渡しファイルが残っています");
}

#[tokio::test]
async fn a_declared_digest_that_does_not_match_leaves_no_artifact_behind() {
    let harness = Harness::start(OperationResponses::from([(
        "render_frame".to_string(),
        ok_result(render_result(
            IMAGE_BYTES.len() as u64,
            sha256_of(b"another image"),
        )),
    )]));
    let handoff = harness.write_handoff(HANDOFF_TOKEN, IMAGE_BYTES);

    let result = harness.render().await;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)["code"], json!("internal_error"));
    assert!(harness.store().is_empty(), "成果物が作られています");
    assert!(!handoff.exists(), "引き渡しファイルが残っています");
}

#[tokio::test]
async fn a_malformed_handoff_token_is_rejected_without_touching_any_file() {
    // 構文の検証はパスの組み立てより先に行われる。`..` を含む識別子で
    // ディレクトリを遡れない。
    let mut result_value = render_result(IMAGE_BYTES.len() as u64, sha256_of(IMAGE_BYTES));
    result_value["handoff_token"] = json!("../../../../windows/system32/config/sam");
    let harness = Harness::start(OperationResponses::from([(
        "render_frame".to_string(),
        ok_result(result_value),
    )]));
    // 構文を満たす場所には実体を置いておく。組み立てが起きていないことを、
    // これが残ることで確かめる。
    let untouched = harness.write_handoff(HANDOFF_TOKEN, IMAGE_BYTES);

    let result = harness.render().await;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)["code"], json!("internal_error"));
    assert!(harness.store().is_empty(), "成果物が作られています");
    assert!(untouched.exists(), "無関係のファイルが消えています");
}

#[tokio::test]
async fn a_missing_handoff_file_fails_without_producing_an_artifact() {
    let harness = Harness::start(honest_render_result());

    let result = harness.render().await;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)["code"], json!("internal_error"));
    assert!(harness.store().is_empty(), "成果物が作られています");
}

#[tokio::test]
async fn a_render_failure_from_the_instance_reaches_the_caller() {
    let error = ErrorObject::new(ErrorCode::EditBlocked, "出力中です", true)
        .with_details(json!({ "render_stage": "wait", "retry_requires": "resend" }));
    let harness = Harness::start(OperationResponses::from([(
        "render_frame".to_string(),
        err_result(error),
    )]));

    let result = harness.render().await;

    assert_eq!(result.is_error, Some(true));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("edit_blocked"));
    assert_eq!(structured["retryable"], json!(true));
    assert_eq!(structured["details"]["render_stage"], json!("wait"));
    // 描画は変更を起こさない。編集と同じ警戒を要すると誤解させない。
    assert!(
        structured["details"].get("change_applied").is_none(),
        "{structured}"
    );
    assert!(harness.store().is_empty(), "成果物が作られています");
}

#[tokio::test]
async fn invalid_render_input_is_rejected_before_any_ipc() {
    let harness = Harness::start(honest_render_result());

    let result = harness
        .server()
        .aviutl2_render_frame(Parameters(RenderFrameInput {
            instance_id: harness.instance_id(),
            expected_scene_id: SCENE_ID,
            frame: i32::MAX as u32 + 1,
            format: RenderFormatInput::Png,
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)["code"], json!("invalid_argument"));
    assert!(
        harness.mock.received_requests().is_empty(),
        "検証前に IPC を発生させない"
    );
}

#[tokio::test]
async fn a_server_without_a_store_never_asks_the_host_to_render() {
    // 保管庫が無いまま要求を送ると、ホストの計算資源を使って作らせた成果物を
    // 受け取れずに捨てることになる。
    let harness = Harness::start(honest_render_result());
    let server = AviUtl2McpServer::with_limits(harness.registry_dir.clone(), CallLimits::default());

    let result = server
        .aviutl2_render_frame(Parameters(RenderFrameInput {
            instance_id: harness.instance_id(),
            expected_scene_id: SCENE_ID,
            frame: FRAME,
            format: RenderFormatInput::Png,
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)["code"], json!("internal_error"));
    assert!(
        harness.requests().is_empty(),
        "保管庫が無いまま描画を要求しています"
    );
}

/// 要求へ載る期限を確かめるために縮めた予算。
///
/// 引き取りの取り分と桁で離し、差し引きを忘れたときに必ず落ちるようにする。
const PROBE_RENDER_BUDGET: Duration = Duration::from_secs(29);
const PROBE_INGEST_BUDGET: Duration = Duration::from_secs(7);

#[tokio::test]
async fn render_requests_reserve_the_time_the_takeover_needs() {
    // 予算をそのまま渡すと、接続先が期限いっぱいまで使った直後に引き取りが
    // 始まり、どの層の期限にも捕まらないまま予算を超える。
    let harness = Harness::with_limits(
        honest_render_result(),
        CallLimits {
            render_request: PROBE_RENDER_BUDGET,
            artifact_ingest: PROBE_INGEST_BUDGET,
            ..CallLimits::default()
        },
    );
    harness.write_handoff(HANDOFF_TOKEN, IMAGE_BYTES);

    let before = Utc::now().timestamp_millis() as u64;
    let result = harness.render().await;
    let after = Utc::now().timestamp_millis() as u64;
    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));

    let request = harness.only_request();
    let deadline = request.deadline_unix_ms.expect("期限がある");
    let expected = (PROBE_RENDER_BUDGET - PROBE_INGEST_BUDGET).as_millis() as u64;
    // 残り時間はミリ秒未満を切り捨てるため、下限を 1 ミリ秒緩める。
    assert!(
        deadline >= before + expected - 1 && deadline <= after + expected,
        "期限が引き取りの取り分を残していません: deadline={deadline} before={before} after={after}"
    );

    // 要求フェーズの予算をそのまま渡していないことを、対比で固定する。
    let whole = PROBE_RENDER_BUDGET.as_millis() as u64;
    assert!(
        deadline < before + whole,
        "要求フェーズの予算をそのまま渡しています: deadline={deadline} before={before}"
    );
}
