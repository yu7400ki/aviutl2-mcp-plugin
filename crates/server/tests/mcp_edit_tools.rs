//! 編集 tool から IPC の編集 operation への変換と、失敗時の tool result を確認する。

mod support;

use aviutl2_mcp_core::{
    AuthSecret, Cursor, EditOutcome, EffectFingerprintInput, EffectInfo, EffectItem,
    EffectItemType, ErrorCode, ErrorObject, FrameRange, InstanceId, InstanceState, ItemValue,
    ObjectFingerprintInput, ObjectSummary, RequestEnvelope, SelectionField, SelectionState,
};
use aviutl2_mcp_server::mcp::edit_input::{
    AddEffectInput, CreateObjectInput, CursorPositionInput, DeleteEffectInput, DeleteObjectInput,
    DestinationInput, EffectSelectorInput, ExpectedInput, FocusChangeInput, ItemValueInput,
    MoveObjectInput, ObjectSourceInput, PlacementInput, RangeChangeInput, SetEffectStateInput,
    SetObjectItemInput, SetObjectNameInput, SetSelectionInput,
};
use aviutl2_mcp_server::mcp::input::ObjectSelectorInput;
use aviutl2_mcp_server::mcp::{AviUtl2McpServer, CallLimits};
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;
use support::{
    MOCK_STARTUP_GRACE, MockPipeServer, OperationResponses, current_process_created_at, err_result,
    ok_result, temp_registry_dir,
};

const EPOCH: &str = "78be92d1-c8c9-44c6-ae52-387548971468";
const SCENE_ID: i32 = 3;
const EXPECTED_REVISION: u64 = 42;
const APPLIED_REVISION: u64 = 43;

/// text へ現れてはならない作成元の alias。
const SECRET_ALIAS: &str = "[vo]\n_name=秘密の立ち絵\n";
/// text へ現れてはならないメディアパス。
const SECRET_PATH: &str = r"C:\Users\tester\secret-movie.mp4";
/// text へ現れてはならない設定値。
const SECRET_ITEM_VALUE: &str = "秘密の字幕";

/// 生存する mock インスタンスと、それを見る MCP サーバー。
struct Harness {
    server: AviUtl2McpServer,
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
        Self {
            server: AviUtl2McpServer::with_limits(registry_dir.clone(), limits),
            mock,
            registry_dir,
        }
    }

    /// 応答を遅らせる mock と、実行予算を縮めたサーバーを起こす。
    ///
    /// 遅延は生存確認の `ping` には掛からないため、期限を使い切るのは編集の
    /// 往復だけになる。
    fn with_delay(
        responses: OperationResponses,
        response_delay: Duration,
        limits: CallLimits,
    ) -> Self {
        let registry_dir = temp_registry_dir();
        let mock = MockPipeServer::start_with_delayed_operations(
            InstanceId::new_v4(),
            AuthSecret::generate(),
            std::process::id(),
            current_process_created_at(),
            InstanceState::Ready,
            responses,
            response_delay,
        );
        mock.write_descriptor(&registry_dir);
        std::thread::sleep(MOCK_STARTUP_GRACE);
        Self {
            server: AviUtl2McpServer::with_limits(registry_dir.clone(), limits),
            mock,
            registry_dir,
        }
    }

    fn instance_id(&self) -> String {
        self.mock.instance_id().to_string()
    }

    /// 生存確認の ping を除いた要求。
    fn requests(&self) -> Vec<RequestEnvelope> {
        self.mock
            .received_requests()
            .into_iter()
            .filter(|request| request.operation != "ping")
            .collect()
    }

    /// 送られた要求がちょうど 1 件であることを確かめて返す。
    fn only_request(&self) -> RequestEnvelope {
        let mut requests = self.requests();
        assert_eq!(requests.len(), 1, "{requests:?}");
        requests.remove(0)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.registry_dir);
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

fn responses(operation: &str, result: Value) -> OperationResponses {
    OperationResponses::from([(operation.to_string(), ok_result(result))])
}

fn sample_summary() -> ObjectSummary {
    ObjectSummary::new(
        EPOCH,
        ObjectFingerprintInput {
            scene_id: SCENE_ID,
            layer: 2,
            frame_start: 120,
            frame_end: 240,
            name: Some("立ち絵"),
            alias: SECRET_ALIAS,
        },
    )
}

/// 秘匿すべき設定値を持つ effect。
fn sample_effect() -> EffectInfo {
    let items = vec![
        EffectItem {
            name: "テキスト".to_string(),
            item_type: EffectItemType::Text,
            value: ItemValue::Text {
                value: SECRET_ITEM_VALUE.to_string(),
            },
            track: None,
        },
        EffectItem {
            name: "ファイル".to_string(),
            item_type: EffectItemType::File,
            value: ItemValue::File {
                path: SECRET_PATH.to_string(),
            },
            track: None,
        },
    ];
    EffectInfo::new(
        sample_summary().selector,
        EffectFingerprintInput {
            effect_name: "テキスト",
            effect_index: 0,
            position: 0,
            effect_count: 1,
            enabled: true,
            locked: false,
            items: &items,
        },
    )
}

fn object_changed() -> Value {
    serde_json::to_value(EditOutcome::object_changed(
        EPOCH,
        APPLIED_REVISION,
        sample_summary(),
    ))
    .expect("直列化できる")
}

fn effect_changed() -> Value {
    serde_json::to_value(EditOutcome::effect_changed(
        EPOCH,
        APPLIED_REVISION,
        sample_summary(),
        sample_effect(),
    ))
    .expect("直列化できる")
}

fn created() -> Value {
    serde_json::to_value(EditOutcome::created(
        EPOCH,
        APPLIED_REVISION,
        vec![sample_summary(), sample_summary()],
    ))
    .expect("直列化できる")
}

fn deleted() -> Value {
    serde_json::to_value(EditOutcome::deleted(EPOCH, APPLIED_REVISION)).expect("直列化できる")
}

fn selection_state() -> Value {
    serde_json::to_value(SelectionState::observed(
        EPOCH,
        EXPECTED_REVISION,
        Cursor {
            frame: 120,
            layer: 2,
        },
        Some(FrameRange { start: 0, end: 10 }),
        Some(sample_summary()),
        vec![SelectionField::Cursor, SelectionField::Focus],
        vec![SelectionField::SelectedRange],
    ))
    .expect("直列化できる")
}

fn selector_input() -> ObjectSelectorInput {
    let selector = sample_summary().selector;
    ObjectSelectorInput {
        project_epoch: selector.project_epoch,
        scene_id: selector.scene_id,
        layer: selector.layer as u32,
        frame: selector.frame as u32,
        name: selector.name,
        fingerprint: selector.fingerprint.as_str().to_string(),
        fingerprint_algorithm: selector.fingerprint_algorithm.as_str().to_string(),
    }
}

fn effect_selector_input() -> EffectSelectorInput {
    let effect = sample_effect();
    EffectSelectorInput {
        object: selector_input(),
        effect_name: effect.selector.effect_name,
        effect_index: effect.selector.effect_index as u32,
        fingerprint: effect.selector.fingerprint.as_str().to_string(),
        fingerprint_algorithm: effect.selector.fingerprint_algorithm.as_str().to_string(),
    }
}

fn expected_input() -> ExpectedInput {
    ExpectedInput {
        project_epoch: EPOCH.to_string(),
        project_revision: EXPECTED_REVISION,
    }
}

fn expected_json() -> Value {
    json!({ "project_epoch": EPOCH, "project_revision": EXPECTED_REVISION })
}

fn selector_json() -> Value {
    serde_json::to_value(sample_summary().selector).expect("直列化できる")
}

fn effect_selector_json() -> Value {
    serde_json::to_value(sample_effect().selector).expect("直列化できる")
}

#[tokio::test]
async fn create_object_tool_sends_create_object_operation() {
    let expected = created();
    let harness = Harness::start(responses("create_object", expected.clone()));

    let result = harness
        .server
        .aviutl2_create_object(Parameters(CreateObjectInput {
            instance_id: harness.instance_id(),
            source: ObjectSourceInput::ObjectAlias {
                alias: SECRET_ALIAS.to_string(),
            },
            placement: PlacementInput {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 0,
            },
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "create_object");
    assert_eq!(
        request.params,
        json!({
            "source": { "type": "object_alias", "alias": SECRET_ALIAS },
            "placement": { "scene_id": SCENE_ID, "layer": 1, "frame": 0 },
            "expected": expected_json(),
        }),
    );
    let text = text_of(&result);
    assert!(text.contains("2 件作成"), "{text}");
    assert!(text.contains("project_revision=43"), "{text}");
}

#[tokio::test]
async fn move_object_tool_sends_move_object_operation() {
    let expected = object_changed();
    let harness = Harness::start(responses("move_object", expected.clone()));

    let result = harness
        .server
        .aviutl2_move_object(Parameters(MoveObjectInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            destination: DestinationInput {
                layer: 5,
                frame: 300,
            },
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "move_object");
    assert_eq!(
        request.params,
        json!({
            "selector": selector_json(),
            "destination": { "layer": 5, "frame": 300 },
            "expected": expected_json(),
        }),
    );
}

#[tokio::test]
async fn set_object_name_tool_sends_the_new_name() {
    let expected = object_changed();
    let harness = Harness::start(responses("set_object_name", expected.clone()));

    let result = harness
        .server
        .aviutl2_set_object_name(Parameters(SetObjectNameInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            name: Some("新しい名前".to_string()),
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    let request = harness.only_request();
    assert_eq!(request.operation, "set_object_name");
    assert_eq!(
        request.params,
        json!({
            "selector": selector_json(),
            "name": "新しい名前",
            "expected": expected_json(),
        }),
    );
}

#[tokio::test]
async fn set_object_name_tool_sends_null_to_restore_the_default_name() {
    let harness = Harness::start(responses("set_object_name", object_changed()));

    let result = harness
        .server
        .aviutl2_set_object_name(Parameters(SetObjectNameInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            name: None,
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(harness.only_request().params["name"], Value::Null);
}

#[tokio::test]
async fn set_object_item_tool_sends_the_effect_selector_and_value() {
    let expected = effect_changed();
    let harness = Harness::start(responses("set_object_item", expected.clone()));

    let result = harness
        .server
        .aviutl2_set_object_item(Parameters(SetObjectItemInput {
            instance_id: harness.instance_id(),
            selector: effect_selector_input(),
            item: "テキスト".to_string(),
            value: ItemValueInput::Text {
                value: SECRET_ITEM_VALUE.to_string(),
            },
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "set_object_item");
    assert_eq!(
        request.params,
        json!({
            "selector": effect_selector_json(),
            "item": "テキスト",
            "value": { "type": "text", "value": SECRET_ITEM_VALUE },
            "expected": expected_json(),
        }),
    );
}

#[tokio::test]
async fn set_object_item_tool_forwards_the_choice_value_verbatim() {
    // 読み取りが返した値をそのまま書き戻せるよう、補助情報の index も落とさずに
    // 転送する。選択肢の並びはホスト側の都合で変わるため、index を正として
    // 解釈しないのは実行側の責務である。
    let harness = Harness::start(responses("set_object_item", effect_changed()));

    let result = harness
        .server
        .aviutl2_set_object_item(Parameters(SetObjectItemInput {
            instance_id: harness.instance_id(),
            selector: effect_selector_input(),
            item: "種類".to_string(),
            value: ItemValueInput::Choice {
                value: "通常".to_string(),
                index: Some(3),
            },
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    let request = harness.only_request();
    assert_eq!(request.params["value"]["value"], json!("通常"));
    assert_eq!(request.params["value"]["index"], json!(3));
}

#[tokio::test]
async fn add_effect_tool_sends_the_effect_name() {
    let expected = effect_changed();
    let harness = Harness::start(responses("add_effect", expected.clone()));

    let result = harness
        .server
        .aviutl2_add_effect(Parameters(AddEffectInput {
            instance_id: harness.instance_id(),
            object: selector_input(),
            effect_name: "ぼかし".to_string(),
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "add_effect");
    assert_eq!(
        request.params,
        json!({
            "object": selector_json(),
            "effect_name": "ぼかし",
            "expected": expected_json(),
        }),
    );
}

#[tokio::test]
async fn set_effect_state_tool_sends_only_the_requested_changes() {
    let expected = effect_changed();
    let harness = Harness::start(responses("set_effect_state", expected.clone()));

    let result = harness
        .server
        .aviutl2_set_effect_state(Parameters(SetEffectStateInput {
            instance_id: harness.instance_id(),
            selector: effect_selector_input(),
            enabled: Some(false),
            locked: None,
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    let request = harness.only_request();
    assert_eq!(request.operation, "set_effect_state");
    assert_eq!(
        request.params,
        json!({
            "selector": effect_selector_json(),
            "enabled": false,
            "locked": null,
            "expected": expected_json(),
        }),
    );
}

#[tokio::test]
async fn delete_effect_tool_sends_delete_effect_operation() {
    let expected = object_changed();
    let harness = Harness::start(responses("delete_effect", expected.clone()));

    let result = harness
        .server
        .aviutl2_delete_effect(Parameters(DeleteEffectInput {
            instance_id: harness.instance_id(),
            selector: effect_selector_input(),
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "delete_effect");
    assert_eq!(
        request.params,
        json!({
            "selector": effect_selector_json(),
            "expected": expected_json(),
        }),
    );
}

#[tokio::test]
async fn delete_object_tool_sends_delete_object_operation() {
    let expected = deleted();
    let harness = Harness::start(responses("delete_object", expected.clone()));

    let result = harness
        .server
        .aviutl2_delete_object(Parameters(DeleteObjectInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "delete_object");
    assert_eq!(
        request.params,
        json!({
            "selector": selector_json(),
            "expected": expected_json(),
        }),
    );
    let text = text_of(&result);
    assert!(text.contains("削除"), "{text}");
    assert!(text.contains("project_revision=43"), "{text}");
}

#[tokio::test]
async fn set_selection_tool_sends_the_scene_guard_and_changes() {
    let expected = selection_state();
    let harness = Harness::start(responses("set_selection", expected.clone()));

    let result = harness
        .server
        .aviutl2_set_selection(Parameters(SetSelectionInput {
            instance_id: harness.instance_id(),
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPositionInput {
                layer: 2,
                frame: 120,
            }),
            selected_range: Some(RangeChangeInput::Clear {}),
            focus: Some(FocusChangeInput::Set {
                object: selector_input(),
            }),
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "set_selection");
    assert_eq!(
        request.params,
        json!({
            "expected_scene_id": SCENE_ID,
            "cursor": { "layer": 2, "frame": 120 },
            "selected_range": { "type": "clear" },
            "focus": { "type": "set", "object": selector_json() },
            "expected": expected_json(),
        }),
    );
    let text = text_of(&result);
    assert!(text.contains("適用できた項目: cursor focus"), "{text}");
}

/// 編集要求へ載る期限を確かめるために縮めた予算。
///
/// read 側と桁で離し、取り違えたときに必ず落ちるようにする。
const PROBE_READ_BUDGET: Duration = Duration::from_millis(300);
const PROBE_EDIT_BUDGET: Duration = Duration::from_secs(9);

/// 要求が運ぶ期限が、送信時刻からおよそ `budget` 先であることを確かめる。
fn assert_deadline_from_budget(
    request: &RequestEnvelope,
    before_unix_ms: u64,
    after_unix_ms: u64,
    budget: Duration,
) {
    let deadline = request
        .deadline_unix_ms
        .unwrap_or_else(|| panic!("{} に期限がありません", request.operation));
    let budget_ms = budget.as_millis() as u64;
    // 残り時間はミリ秒未満を切り捨てるため、下限を 1 ミリ秒緩める。
    assert!(
        deadline >= before_unix_ms + budget_ms - 1 && deadline <= after_unix_ms + budget_ms,
        "{} の期限が予算 {budget_ms}ms から算出されていません: deadline={deadline} before={before_unix_ms} after={after_unix_ms}",
        request.operation,
    );
}

#[tokio::test]
async fn edit_requests_carry_a_deadline_derived_from_the_edit_budget() {
    // 編集は read より長くかかるため、read の予算で期限を作ると応答している
    // インスタンスを途中で打ち切ってしまう。
    let harness = Harness::with_limits(
        responses("move_object", object_changed()),
        CallLimits {
            request: PROBE_READ_BUDGET,
            edit_request: PROBE_EDIT_BUDGET,
            ..CallLimits::default()
        },
    );

    let before = Utc::now().timestamp_millis() as u64;
    let result = harness
        .server
        .aviutl2_move_object(Parameters(MoveObjectInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            destination: DestinationInput { layer: 5, frame: 0 },
            expected: expected_input(),
        }))
        .await;
    let after = Utc::now().timestamp_millis() as u64;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_deadline_from_budget(&harness.only_request(), before, after, PROBE_EDIT_BUDGET);
}

#[tokio::test]
async fn read_requests_keep_the_read_budget_after_the_edit_budget_was_added() {
    let harness = Harness::with_limits(
        responses("get_current_scene", json!({ "unexpected": true })),
        CallLimits {
            request: PROBE_READ_BUDGET,
            edit_request: PROBE_EDIT_BUDGET,
            ..CallLimits::default()
        },
    );

    let before = Utc::now().timestamp_millis() as u64;
    let _ = harness
        .server
        .aviutl2_get_current_scene(Parameters(aviutl2_mcp_server::mcp::input::InstanceInput {
            instance_id: harness.instance_id(),
        }))
        .await;
    let after = Utc::now().timestamp_millis() as u64;

    assert_deadline_from_budget(&harness.only_request(), before, after, PROBE_READ_BUDGET);
}

#[tokio::test]
async fn invalid_edit_input_is_rejected_before_any_ipc() {
    let harness = Harness::start(OperationResponses::new());

    let result = harness
        .server
        .aviutl2_create_object(Parameters(CreateObjectInput {
            instance_id: harness.instance_id(),
            source: ObjectSourceInput::MediaFile {
                path: r"..\movie.mp4".to_string(),
            },
            placement: PlacementInput {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 0,
            },
            expected: expected_input(),
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
async fn malformed_instance_id_never_reaches_an_edit_operation() {
    let harness = Harness::start(responses("set_selection", selection_state()));

    let result = harness
        .server
        .aviutl2_set_selection(Parameters(SetSelectionInput {
            instance_id: "not-a-uuid".to_string(),
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPositionInput { layer: 0, frame: 0 }),
            selected_range: None,
            focus: None,
            expected: expected_input(),
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
async fn precondition_failure_reaches_the_tool_result_with_the_current_revision() {
    let error = ErrorObject::new(ErrorCode::PreconditionFailed, "対象が変化しました", true)
        .with_details(json!({
            "current_project_revision": 44,
            "mutation_issued": true,
            "mismatch": "fingerprint",
            "retry_requires": "refetch",
        }));
    let harness = Harness::start(OperationResponses::from([(
        "move_object".to_string(),
        err_result(error),
    )]));

    let result = harness
        .server
        .aviutl2_move_object(Parameters(MoveObjectInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            destination: DestinationInput { layer: 5, frame: 0 },
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("precondition_failed"));
    assert_eq!(structured["retryable"], json!(true));
    assert_eq!(structured["details"]["current_project_revision"], json!(44));
    assert_eq!(structured["details"]["mutation_issued"], json!(true));
    assert_eq!(structured["details"]["mismatch"], json!("fingerprint"));
    assert_eq!(structured["details"]["retry_requires"], json!("refetch"));
    assert!(structured["correlation_id"].is_string());
}

#[tokio::test]
async fn timeout_from_the_instance_keeps_the_change_applied_hint() {
    // timeout は変更が無かったことを意味しない。判断に要る内訳を落とさない。
    let error = ErrorObject::new(ErrorCode::Timeout, "期限内に完了しませんでした", true)
        .with_details(json!({ "change_applied": "unknown", "mutation_origin": "plugin" }));
    let harness = Harness::start(OperationResponses::from([(
        "add_effect".to_string(),
        err_result(error),
    )]));

    let result = harness
        .server
        .aviutl2_add_effect(Parameters(AddEffectInput {
            instance_id: harness.instance_id(),
            object: selector_input(),
            effect_name: "ぼかし".to_string(),
            expected: expected_input(),
        }))
        .await;

    let structured = structured(&result);
    assert_eq!(structured["code"], json!("timeout"));
    assert_eq!(structured["details"]["change_applied"], json!("unknown"));
    assert_eq!(structured["details"]["mutation_origin"], json!("plugin"));
}

/// 応答しないインスタンスを演じる時間。編集の要求予算を確実に超える長さにする。
const SLOW_EDIT: Duration = Duration::from_millis(500);

/// 期限超過を起こすために縮めた編集 operation の予算。
///
/// [`SLOW_EDIT`] より十分短く、接続と生存確認が終わるだけの余裕はある値を選ぶ。
const SHORT_EDIT_BUDGET: Duration = Duration::from_millis(200);

#[tokio::test]
async fn a_timeout_built_by_the_server_reports_an_unknown_change() {
    // 予算切れは、インスタンスが編集区間へ入ったあとにも起きる。そのとき変更は
    // 適用され取り消し履歴にも載っているのに、要求元が受け取るのは timeout で
    // ある。実行前の期限超過だけが未適用を名乗れるので、判別できない側は不明を
    // 名乗り、読み直しを促す。
    let harness = Harness::with_delay(
        responses("create_object", created()),
        SLOW_EDIT,
        CallLimits {
            edit_request: SHORT_EDIT_BUDGET,
            ..CallLimits::default()
        },
    );

    let result = harness
        .server
        .aviutl2_create_object(Parameters(CreateObjectInput {
            instance_id: harness.instance_id(),
            source: ObjectSourceInput::ObjectAlias {
                alias: "[vo]".to_string(),
            },
            placement: PlacementInput {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 0,
            },
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true), "{}", text_of(&result));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("timeout"), "{structured}");
    assert_eq!(
        structured["details"]["change_applied"],
        json!("unknown"),
        "{structured}"
    );
    assert_eq!(
        structured["details"]["mutation_origin"],
        json!("server"),
        "{structured}"
    );
    assert_eq!(
        structured["details"]["retry_requires"],
        json!("refetch"),
        "{structured}"
    );
}

#[tokio::test]
async fn a_read_that_outlasts_its_budget_stays_silent_about_changes() {
    let harness = Harness::with_delay(
        responses("get_current_scene", json!({ "unexpected": true })),
        SLOW_EDIT,
        CallLimits {
            request: SHORT_EDIT_BUDGET,
            ..CallLimits::default()
        },
    );

    let result = harness
        .server
        .aviutl2_get_current_scene(Parameters(aviutl2_mcp_server::mcp::input::InstanceInput {
            instance_id: harness.instance_id(),
        }))
        .await;

    let structured = structured(&result);
    assert_eq!(structured["code"], json!("timeout"), "{structured}");
    assert!(
        structured["details"].get("change_applied").is_none(),
        "読み取りが変更の有無を名乗りました: {structured}"
    );
}

#[tokio::test]
async fn secrets_in_edit_failures_never_reach_the_tool_result() {
    let error = ErrorObject::new(ErrorCode::SdkError, "SDK 呼び出しに失敗しました", false)
        .with_details(json!({
            "auth_secret": "s3cr3t-value",
            "object_handle": 1234,
            "raw_pointer": "0xdeadbeef",
            "pipe_name": r"\\.\pipe\aviutl2-mcp-leaked",
            "object_alias": SECRET_ALIAS,
            "media_path": SECRET_PATH,
            "sdk_operation": "create_object_from_alias",
            "retry_requires": "none",
        }));
    let harness = Harness::start(OperationResponses::from([(
        "create_object".to_string(),
        err_result(error),
    )]));

    let result = harness
        .server
        .aviutl2_create_object(Parameters(CreateObjectInput {
            instance_id: harness.instance_id(),
            source: ObjectSourceInput::ObjectAlias {
                alias: SECRET_ALIAS.to_string(),
            },
            placement: PlacementInput {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 0,
            },
            expected: expected_input(),
        }))
        .await;

    let serialized = serde_json::to_string(&result).expect("直列化できる");
    for forbidden in [
        "s3cr3t-value",
        "0xdeadbeef",
        "aviutl2-mcp-leaked",
        "秘密の立ち絵",
        "tester",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "{forbidden} が tool result に含まれています: {serialized}"
        );
    }

    let structured = structured(&result);
    assert_eq!(structured["code"], json!("sdk_error"));
    assert_eq!(structured["retryable"], json!(false));
    // 訂正に要る内訳は残す。
    assert_eq!(
        structured["details"]["sdk_operation"],
        json!("create_object_from_alias")
    );
    assert_eq!(structured["details"]["retry_requires"], json!("none"));
    assert!(structured["correlation_id"].is_string());
}

#[tokio::test]
async fn edit_text_never_echoes_the_alias_path_or_item_value_that_was_sent() {
    // 要求にも応答にも利用者の内容が現れるが、text へは載せない。
    let harness = Harness::start(responses("set_object_item", effect_changed()));

    let result = harness
        .server
        .aviutl2_set_object_item(Parameters(SetObjectItemInput {
            instance_id: harness.instance_id(),
            selector: effect_selector_input(),
            item: "ファイル".to_string(),
            value: ItemValueInput::File {
                path: SECRET_PATH.to_string(),
            },
            expected: expected_input(),
        }))
        .await;

    let text = text_of(&result);
    for forbidden in [SECRET_ITEM_VALUE, SECRET_PATH, "[vo]", "_name="] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} が text にあります: {text}"
        );
    }
    // 対象を見分けるための位置と名前は残る。
    assert!(text.contains("layer=2"), "{text}");
    assert!(text.contains("立ち絵"), "{text}");
}

#[tokio::test]
async fn edit_tool_reports_an_unsupported_operation_from_the_instance() {
    // 応答を注入しない operation は mock が unsupported_operation で返す。
    let harness = Harness::start(OperationResponses::new());

    let result = harness
        .server
        .aviutl2_set_effect_state(Parameters(SetEffectStateInput {
            instance_id: harness.instance_id(),
            selector: effect_selector_input(),
            enabled: Some(true),
            locked: None,
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)["code"], json!("unsupported_operation"));
}

#[tokio::test]
async fn unknown_instance_id_never_reaches_an_edit_operation() {
    let harness = Harness::start(responses("delete_object", deleted()));

    let result = harness
        .server
        .aviutl2_delete_object(Parameters(DeleteObjectInput {
            instance_id: InstanceId::new_v4().to_string(),
            selector: selector_input(),
            expected: expected_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)["code"], json!("instance_not_found"));
    assert!(harness.requests().is_empty(), "削除要求が送られています");
}
