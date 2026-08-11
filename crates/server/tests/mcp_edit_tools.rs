//! 編集 tool から IPC の編集 operation への変換と、失敗時の tool result を確認する。

mod support;

use aviutl2_mcp_core::{
    AuthSecret, BatchOutcome, BatchStepOutcome, Cursor, DisplayRange, EditOutcome,
    EffectFingerprintInput, EffectInfo, EffectItem, EffectItemType, ErrorCode, ErrorObject,
    FiniteF64, FrameRange, GridBpm, GridBpmOutcome, InstanceId, InstanceState, ItemValue,
    LayerInfo, LayerStateOutcome, MAX_GRID_BPM_ENTRIES, ObjectFingerprintInput,
    ObjectSectionsOutcome, ObjectSelector, ObjectSummary, ObservedSelection, RequestEnvelope,
    SceneInfo, SceneSettingsOutcome, SectionRange, SelectionField, SelectionState,
};
use aviutl2_mcp_server::mcp::edit_input::{
    AddEffectInput, ApplyBatchInput, BatchOperationInput, CreateObjectInput,
    CreateObjectSectionInput, CursorPositionInput, DeleteEffectInput, DeleteObjectInput,
    DeleteObjectSectionInput, DestinationInput, DisplayStartInput, FocusChangeInput, GridBpmInput,
    ItemValueInput, LayerNameChangeInput, MoveEffectInput, MoveObjectInput, MoveObjectSectionInput,
    ObjectSourceInput, PlacementInput, RangeChangeInput, SceneSizeInput, SetEffectEnabledInput,
    SetGridBpmInput, SetLayerStateInput, SetObjectItemInput, SetObjectNameInput,
    SetSceneSettingsInput, SetSelectionInput,
};
use aviutl2_mcp_server::mcp::input::{EffectSelectorInput, ObjectSelectorInput};
use aviutl2_mcp_server::mcp::{AviUtl2McpServer, CallLimits};
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;
use support::{
    MOCK_STARTUP_GRACE, MockPipeServer, OperationResponses, current_process_created_at, err_result,
    ok_result, remove_test_registry, temp_registry_dir,
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
            server: AviUtl2McpServer::without_artifact_store(registry_dir.clone(), limits),
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
            server: AviUtl2McpServer::without_artifact_store(registry_dir.clone(), limits),
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
        ObservedSelection {
            cursor: Cursor {
                frame: 120,
                layer: 2,
            },
            selected_range: Some(FrameRange { start: 0, end: 10 }),
            focus: Some(sample_summary()),
            display: DisplayRange {
                frame_start: 60,
                layer_start: 1,
                frame_num: 600,
                layer_num: 10,
            },
        },
        vec![SelectionField::Cursor, SelectionField::Focus],
        vec![SelectionField::SelectedRange, SelectionField::Display],
    ))
    .expect("直列化できる")
}

fn layer_state() -> Value {
    serde_json::to_value(LayerStateOutcome {
        project_epoch: EPOCH.to_string(),
        project_revision: APPLIED_REVISION,
        layer: LayerInfo {
            index: 2,
            name: Some("背景".to_string()),
            enabled: true,
            locked: false,
            object_count: 3,
        },
    })
    .expect("直列化できる")
}

/// 3 軸を変更したあとに観測したシーンの状態。
///
/// 観測値は要求値と異なる。ホストが調整し得るため、差異は失敗ではない。
fn scene_settings() -> Value {
    serde_json::to_value(SceneSettingsOutcome {
        project_epoch: EPOCH.to_string(),
        project_revision: APPLIED_REVISION,
        scene: SceneInfo {
            id: SCENE_ID,
            name: Some("本編".to_string()),
            width: 1920,
            height: 1080,
            fps: FiniteF64::try_new(60.0),
            fps_rate: 60,
            fps_scale: 1,
            sample_rate: 48_000,
        },
        observed_after_edit: true,
        non_undoable: true,
    })
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
    }
}

fn effect_selector_input() -> EffectSelectorInput {
    let effect = sample_effect();
    EffectSelectorInput {
        object: selector_input(),
        effect_name: effect.selector.effect_name,
        effect_index: effect.selector.effect_index as u32,
        fingerprint: effect.selector.fingerprint.as_str().to_string(),
    }
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
        .create_object(Parameters(CreateObjectInput {
            instance_id: harness.instance_id(),
            source: ObjectSourceInput::ObjectAlias {
                alias: SECRET_ALIAS.to_string(),
            },
            placement: PlacementInput {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 0,
            },
            expected_project_epoch: EPOCH.to_string(),
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
            "expected_project_epoch": EPOCH,
        }),
    );
    let text = text_of(&result);
    assert!(text.contains("2 件作成"), "{text}");
    assert!(text.contains("project_revision=43"), "{text}");
}

#[tokio::test]
async fn create_object_tool_sends_an_effect_name_as_its_own_source() {
    let expected = created();
    let harness = Harness::start(responses("create_object", expected.clone()));

    let result = harness
        .server
        .create_object(Parameters(CreateObjectInput {
            instance_id: harness.instance_id(),
            source: ObjectSourceInput::Effect {
                name: "テキスト".to_string(),
            },
            placement: PlacementInput {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 0,
            },
            expected_project_epoch: EPOCH.to_string(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "create_object");
    assert_eq!(
        request.params,
        json!({
            "source": { "type": "effect", "name": "テキスト" },
            "placement": { "scene_id": SCENE_ID, "layer": 1, "frame": 0 },
            "expected_project_epoch": EPOCH,
        }),
    );

    // 作成元の種別が変わっても、応答が運ぶものは変わらない。
    let serialized = serde_json::to_string(&result).expect("直列化できる");
    for forbidden in ["秘密の立ち絵", "object_handle", "alias"] {
        assert!(
            !serialized.contains(forbidden),
            "{forbidden} が tool result に含まれています: {serialized}"
        );
    }
}

#[tokio::test]
async fn create_object_tool_sends_a_registered_alias_name_as_its_own_source() {
    let expected = created();
    let harness = Harness::start(responses("create_object", expected.clone()));

    let result = harness
        .server
        .create_object(Parameters(CreateObjectInput {
            instance_id: harness.instance_id(),
            source: ObjectSourceInput::AliasName {
                name: "テストエイリアス".to_string(),
            },
            placement: PlacementInput {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 0,
            },
            expected_project_epoch: EPOCH.to_string(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "create_object");
    assert_eq!(
        request.params,
        json!({
            "source": { "type": "alias_name", "name": "テストエイリアス" },
            "placement": { "scene_id": SCENE_ID, "layer": 1, "frame": 0 },
            "expected_project_epoch": EPOCH,
        }),
    );
}

#[tokio::test]
async fn create_object_tool_checks_the_alias_name_syntax_without_reaching_the_plugin() {
    // ファイルの存在も中身も server からは見えない。構文だけを両側で見て、
    // 内容は plugin が判定する。
    for (name, reason) in [(r"..\エイリアス", "forbidden_character"), ("", "empty")] {
        let harness = Harness::start(responses("create_object", created()));
        let result = harness
            .server
            .create_object(Parameters(CreateObjectInput {
                instance_id: harness.instance_id(),
                source: ObjectSourceInput::AliasName {
                    name: name.to_string(),
                },
                placement: PlacementInput {
                    scene_id: SCENE_ID,
                    layer: 1,
                    frame: 0,
                },
                expected_project_epoch: EPOCH.to_string(),
            }))
            .await;

        assert_eq!(result.is_error, Some(true), "{name:?}");
        let structured = structured(&result);
        assert_eq!(structured["code"], json!("invalid_argument"), "{name:?}");
        assert_eq!(structured["details"]["reason"], json!(reason), "{name:?}");
        assert!(
            harness.requests().is_empty(),
            "{name:?} が plugin へ届きました"
        );
    }
}

#[tokio::test]
async fn move_object_tool_sends_move_object_operation() {
    let expected = object_changed();
    let harness = Harness::start(responses("move_object", expected.clone()));

    let result = harness
        .server
        .move_object(Parameters(MoveObjectInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            destination: DestinationInput {
                layer: 5,
                frame: 300,
            },
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
        }),
    );
}

#[tokio::test]
async fn set_object_name_tool_sends_the_new_name() {
    let expected = object_changed();
    let harness = Harness::start(responses("set_object_name", expected.clone()));

    let result = harness
        .server
        .set_object_name(Parameters(SetObjectNameInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            name: Some("新しい名前".to_string()),
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
        }),
    );
}

#[tokio::test]
async fn set_object_name_tool_sends_null_to_restore_the_default_name() {
    let harness = Harness::start(responses("set_object_name", object_changed()));

    let result = harness
        .server
        .set_object_name(Parameters(SetObjectNameInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            name: None,
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
        .set_object_item(Parameters(SetObjectItemInput {
            instance_id: harness.instance_id(),
            selector: effect_selector_input(),
            item: "テキスト".to_string(),
            value: ItemValueInput::Text {
                value: SECRET_ITEM_VALUE.to_string(),
            },
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
        }),
    );
}

#[tokio::test]
async fn set_object_item_tool_forwards_the_choice_value_verbatim() {
    let harness = Harness::start(responses("set_object_item", effect_changed()));

    let result = harness
        .server
        .set_object_item(Parameters(SetObjectItemInput {
            instance_id: harness.instance_id(),
            selector: effect_selector_input(),
            item: "種類".to_string(),
            value: ItemValueInput::Choice {
                value: "通常".to_string(),
            },
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    let request = harness.only_request();
    assert_eq!(
        request.params["value"],
        json!({ "type": "choice", "value": "通常" }),
    );
}

#[tokio::test]
async fn add_effect_tool_sends_the_effect_name() {
    let expected = effect_changed();
    let harness = Harness::start(responses("add_effect", expected.clone()));

    let result = harness
        .server
        .add_effect(Parameters(AddEffectInput {
            instance_id: harness.instance_id(),
            object: selector_input(),
            effect_name: "ぼかし".to_string(),
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
        }),
    );
}

#[tokio::test]
async fn set_effect_enabled_tool_sends_set_effect_enabled_operation() {
    let expected = effect_changed();
    let harness = Harness::start(responses("set_effect_enabled", expected.clone()));

    let result = harness
        .server
        .set_effect_enabled(Parameters(SetEffectEnabledInput {
            instance_id: harness.instance_id(),
            selector: effect_selector_input(),
            enabled: false,
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    let request = harness.only_request();
    assert_eq!(request.operation, "set_effect_enabled");
    assert_eq!(
        request.params,
        json!({
            "selector": effect_selector_json(),
            "enabled": false,
        }),
    );
}

#[tokio::test]
async fn move_effect_tool_sends_the_destination_position() {
    // 移動先は selector の外の引数である。selector が運ぶ effect_index は同名
    // effect の順序であり、列全体での位置とは別の値であるため、position を
    // selector へ畳んだ実装も、別の名前で送る実装もここで落ちる。
    let expected = effect_changed();
    let harness = Harness::start(responses("move_effect", expected.clone()));

    let result = harness
        .server
        .move_effect(Parameters(MoveEffectInput {
            instance_id: harness.instance_id(),
            selector: effect_selector_input(),
            position: 2,
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "move_effect");
    assert_eq!(
        request.params,
        json!({
            "selector": effect_selector_json(),
            "position": 2,
        }),
    );
}

#[tokio::test]
async fn delete_effect_tool_sends_delete_effect_operation() {
    let expected = object_changed();
    let harness = Harness::start(responses("delete_effect", expected.clone()));

    let result = harness
        .server
        .delete_effect(Parameters(DeleteEffectInput {
            instance_id: harness.instance_id(),
            selector: effect_selector_input(),
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
        }),
    );
}

#[tokio::test]
async fn delete_object_tool_sends_delete_object_operation() {
    let expected = deleted();
    let harness = Harness::start(responses("delete_object", expected.clone()));

    let result = harness
        .server
        .delete_object(Parameters(DeleteObjectInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
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
        .set_selection(Parameters(SetSelectionInput {
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
            display: Some(DisplayStartInput {
                layer: 1,
                frame: 60,
            }),
            expected_project_epoch: EPOCH.to_string(),
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
            "display": { "layer": 1, "frame": 60 },
            "expected_project_epoch": EPOCH,
        }),
    );
    let text = text_of(&result);
    assert!(text.contains("適用できた項目: cursor focus"), "{text}");
}

#[tokio::test]
async fn set_layer_state_tool_sends_the_three_axes_and_the_scene_guard() {
    let expected = layer_state();
    let harness = Harness::start(responses("set_layer_state", expected.clone()));

    let result = harness
        .server
        .set_layer_state(Parameters(SetLayerStateInput {
            instance_id: harness.instance_id(),
            expected_scene_id: SCENE_ID,
            layer: 2,
            name: Some(LayerNameChangeInput::Reset {}),
            enabled: Some(true),
            locked: Some(false),
            expected_project_epoch: EPOCH.to_string(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "set_layer_state");
    assert_eq!(
        request.params,
        json!({
            "expected_scene_id": SCENE_ID,
            "layer": 2,
            "name": { "type": "reset" },
            "enabled": true,
            "locked": false,
            "expected_project_epoch": EPOCH,
        }),
    );
    let text = text_of(&result);
    assert!(text.contains("layer=2"), "{text}");
    assert!(text.contains("project_revision=43"), "{text}");
    // レイヤーは fingerprint を持たない。応答の値で確認するよう案内する。
    assert!(text.contains("fingerprint"), "{text}");
}

#[tokio::test]
async fn set_scene_settings_tool_sends_the_three_axes_and_the_scene_guard() {
    let expected = scene_settings();
    let harness = Harness::start(responses("set_scene_settings", expected.clone()));

    let result = harness
        .server
        .set_scene_settings(Parameters(SetSceneSettingsInput {
            instance_id: harness.instance_id(),
            expected_scene_id: SCENE_ID,
            name: Some("本編".to_string()),
            size: Some(SceneSizeInput {
                width: 1280,
                height: 720,
            }),
            sample_rate: Some(44_100),
            expected_project_epoch: EPOCH.to_string(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "set_scene_settings");
    assert_eq!(
        request.params,
        json!({
            "expected_scene_id": SCENE_ID,
            "name": "本編",
            "size": { "width": 1280, "height": 720 },
            "sample_rate": 44_100,
            "expected_project_epoch": EPOCH,
        }),
    );

    // 観測値は要求値と異なるが失敗ではない。応答が運ぶのは観測した値である。
    assert_eq!(structured(&result)["scene"]["width"], json!(1920));
    let text = text_of(&result);
    assert!(text.contains("1920x1080"), "{text}");
    assert!(text.contains("project_revision=43"), "{text}");
}

#[tokio::test]
async fn set_scene_settings_tool_reports_a_change_that_cannot_be_undone() {
    // 取り消せないことを要求のあとから読める唯一の口である。説明と annotation は
    // 要求を出す前にしか効かず、応答だけを見る経路はそこから性質を拾えない。
    // 観測が編集と原子的でないことも同じ場所が運ぶ。
    let harness = Harness::start(responses("set_scene_settings", scene_settings()));

    let result = harness
        .server
        .set_scene_settings(Parameters(SetSceneSettingsInput {
            instance_id: harness.instance_id(),
            expected_scene_id: SCENE_ID,
            name: None,
            size: None,
            sample_rate: Some(48_000),
            expected_project_epoch: EPOCH.to_string(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    let structured = structured(&result);
    assert_eq!(structured["non_undoable"], json!(true));
    assert_eq!(structured["observed_after_edit"], json!(true));
    // 省略した軸は null として運ばれる。値を持つ軸だけが要求に現れる形ではない。
    assert_eq!(
        harness.only_request().params,
        json!({
            "expected_scene_id": SCENE_ID,
            "name": null,
            "size": null,
            "sample_rate": 48_000,
            "expected_project_epoch": EPOCH,
        }),
    );

    let text = text_of(&result);
    assert!(text.contains("この変更は取り消せません"), "{text}");
}

/// 編集要求へ載る期限を確かめるために縮めた予算。
///
/// read 側と桁で離し、取り違えたときに必ず落ちるようにする。
const PROBE_READ_BUDGET: Duration = Duration::from_millis(300);
const PROBE_EDIT_BUDGET: Duration = Duration::from_secs(9);
const PROBE_BATCH_BUDGET: Duration = Duration::from_secs(19);

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
        .move_object(Parameters(MoveObjectInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            destination: DestinationInput { layer: 5, frame: 0 },
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
        .get_current_scene(Parameters(aviutl2_mcp_server::mcp::input::InstanceInput {
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
        .create_object(Parameters(CreateObjectInput {
            instance_id: harness.instance_id(),
            source: ObjectSourceInput::MediaFile {
                path: r"..\movie.mp4".to_string(),
            },
            placement: PlacementInput {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 0,
            },
            expected_project_epoch: EPOCH.to_string(),
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
async fn a_rejected_path_names_the_rule_it_broke() {
    // 7 種のパス構文の失敗はいずれも invalid_argument で返る。要求元が
    // 「ローカルへ複製する」「絶対パスにする」「短い場所へ移す」のどれを
    // 取ればよいかは、名前が無ければ説明の文面からしか読めない。
    let long_path = format!(r"C:\{}", "a".repeat(32_767));
    let cases = [
        ("", "empty_path"),
        ("C:\\movie\0.mp4", "contains_nul"),
        (long_path.as_str(), "path_too_long"),
        (r"\\.\pipe\aviutl2", "device_namespace"),
        (r"\\?\C:\movie.mp4", "device_namespace"),
        (r"C:\movie.mp4:stream", "alternate_data_stream"),
        (r"..\movie.mp4", "not_absolute"),
        (r"\\server\share\movie.mp4", "unc_path"),
        ("//server/share/movie.mp4", "unc_path"),
        (r"\\server\share", "unc_path"),
    ];
    let harness = Harness::start(OperationResponses::new());

    for (path, reason) in cases {
        let result = harness
            .server
            .create_object(Parameters(CreateObjectInput {
                instance_id: harness.instance_id(),
                source: ObjectSourceInput::MediaFile {
                    path: path.to_string(),
                },
                placement: PlacementInput {
                    scene_id: SCENE_ID,
                    layer: 1,
                    frame: 0,
                },
                expected_project_epoch: EPOCH.to_string(),
            }))
            .await;

        assert_eq!(result.is_error, Some(true), "{reason}");
        let structured = structured(&result);
        assert_eq!(structured["code"], json!("invalid_argument"), "{reason}");
        assert_eq!(
            structured["details"]["reason"],
            json!(reason),
            "{path:?} が名乗った種別が想定と異なります"
        );
        // 名前は種別だけを表す。渡したパスは応答のどこにも現れない。
        let serialized = serde_json::to_string(&result).expect("直列化できる");
        assert!(
            !serialized.contains("movie"),
            "{reason} の応答にパスが現れました: {serialized}"
        );
    }
    assert!(
        harness.mock.received_requests().is_empty(),
        "検証前に IPC を発生させない"
    );
}

#[tokio::test]
async fn a_rejected_item_value_names_the_rule_it_broke() {
    // 設定値の文字列検証も同じである。値そのものは応答へ載せない。
    let harness = Harness::start(OperationResponses::new());
    let cases = [
        ("秘密\0の字幕".to_string(), "contains_nul"),
        ("秘密\u{1}の字幕".to_string(), "contains_control"),
        ("秘".repeat(8_192), "too_long"),
    ];

    for (value, reason) in cases {
        let result = harness
            .server
            .set_object_item(Parameters(SetObjectItemInput {
                instance_id: harness.instance_id(),
                selector: effect_selector_input(),
                item: "テキスト".to_string(),
                value: ItemValueInput::Text {
                    value: value.clone(),
                },
            }))
            .await;

        assert_eq!(result.is_error, Some(true), "{reason}");
        let structured = structured(&result);
        assert_eq!(structured["code"], json!("invalid_argument"), "{reason}");
        assert_eq!(structured["details"]["reason"], json!(reason));
        let serialized = serde_json::to_string(&result).expect("直列化できる");
        assert!(
            !serialized.contains("秘密"),
            "{reason} の応答に設定値が現れました"
        );
    }

    // 空文字列は文字列値ではなくレイヤー名の指定で拒否される。名前を消す
    // 指定は別に用意してあるため、空を「消す」意味へ黙って読み替えない。
    let result = harness
        .server
        .set_layer_state(Parameters(SetLayerStateInput {
            instance_id: harness.instance_id(),
            expected_scene_id: SCENE_ID,
            layer: 2,
            name: Some(LayerNameChangeInput::Set {
                name: String::new(),
            }),
            enabled: None,
            locked: None,
            expected_project_epoch: EPOCH.to_string(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("invalid_argument"));
    assert_eq!(structured["details"]["reason"], json!("empty"));
}

#[tokio::test]
async fn a_batch_names_the_same_rule_as_the_same_edit_on_its_own() {
    // 同じ入力が単独編集と一括適用で違う応答になれば、要求元は一括適用の
    // ためだけの分岐を持つことになる。一括適用は位置も併せて返す。
    let harness = Harness::start(OperationResponses::new());

    let alone = harness
        .server
        .set_object_item(Parameters(SetObjectItemInput {
            instance_id: harness.instance_id(),
            selector: effect_selector_input(),
            item: "ファイル".to_string(),
            value: ItemValueInput::File {
                path: r"\\server\share\movie.mp4".to_string(),
            },
        }))
        .await;

    let batched = harness
        .server
        .apply_batch(Parameters(ApplyBatchInput {
            instance_id: harness.instance_id(),
            operations: vec![
                move_operation(5),
                BatchOperationInput::SetObjectItem {
                    selector: effect_selector_input(),
                    item: "ファイル".to_string(),
                    value: ItemValueInput::File {
                        path: r"\\server\share\movie.mp4".to_string(),
                    },
                },
            ],
        }))
        .await;

    assert_eq!(alone.is_error, Some(true));
    assert_eq!(batched.is_error, Some(true));
    let alone = structured(&alone);
    let batched = structured(&batched);
    assert_eq!(alone["code"], batched["code"]);
    assert_eq!(alone["details"]["reason"], json!("unc_path"));
    assert_eq!(batched["details"]["reason"], alone["details"]["reason"]);
    assert_eq!(batched["details"]["failed_index"], json!(1));
    assert!(alone["details"].get("failed_index").is_none());

    // フォルダも同じパス検証を通る。片方だけを固定すると、種別ごとに
    // 検証を書き分ける形へ戻っても気付けない。
    let folder = harness
        .server
        .apply_batch(Parameters(ApplyBatchInput {
            instance_id: harness.instance_id(),
            operations: vec![
                move_operation(5),
                BatchOperationInput::SetObjectItem {
                    selector: effect_selector_input(),
                    item: "フォルダ".to_string(),
                    value: ItemValueInput::Folder {
                        path: r"..\assets".to_string(),
                    },
                },
            ],
        }))
        .await;

    assert_eq!(folder.is_error, Some(true));
    let folder = structured(&folder);
    assert_eq!(folder["details"]["reason"], json!("not_absolute"));
    assert_eq!(folder["details"]["failed_index"], json!(1));

    assert!(
        harness.mock.received_requests().is_empty(),
        "検証前に IPC を発生させない"
    );
}

#[tokio::test]
async fn an_item_integer_past_the_representable_width_never_reaches_the_instance() {
    // schema の上下界は宣言であり、要求がそれを満たすかを rmcp は検証しない。
    // 宣言した幅は server 側で実際に確かめ、書き込みを発行せずに落とす。
    for value in [i64::from(i32::MAX) + 1, i64::from(i32::MIN) - 1] {
        let harness = Harness::start(OperationResponses::new());

        let result = harness
            .server
            .set_object_item(Parameters(SetObjectItemInput {
                instance_id: harness.instance_id(),
                selector: effect_selector_input(),
                item: "対象レイヤー数".to_string(),
                value: ItemValueInput::Integer { value },
            }))
            .await;

        assert_eq!(result.is_error, Some(true), "{value}");
        let structured = structured(&result);
        assert_eq!(structured["code"], json!("invalid_argument"), "{value}");
        assert_eq!(
            structured["details"]["reason"],
            json!("argument_not_representable"),
            "{value}"
        );
        assert!(harness.requests().is_empty(), "{value} が IPC へ届きました");
    }
}

#[tokio::test]
async fn a_batch_holding_one_over_wide_integer_applies_none_of_its_operations() {
    // 幅を外した 1 件は、並んだ他の sub-operation ごと発行前に落とす。
    for value in [i64::from(i32::MAX) + 1, i64::from(i32::MIN) - 1] {
        let harness = Harness::start(OperationResponses::new());

        let result = harness
            .server
            .apply_batch(Parameters(ApplyBatchInput {
                instance_id: harness.instance_id(),
                operations: vec![
                    distinct_move_operation(4),
                    BatchOperationInput::SetObjectItem {
                        selector: effect_selector_input(),
                        item: "対象レイヤー数".to_string(),
                        value: ItemValueInput::Integer { value },
                    },
                    distinct_move_operation(6),
                ],
            }))
            .await;

        assert_eq!(result.is_error, Some(true), "{value}");
        let structured = structured(&result);
        assert_eq!(structured["code"], json!("invalid_argument"), "{value}");
        assert_eq!(
            structured["details"]["reason"],
            json!("argument_not_representable"),
            "{value}"
        );
        assert_eq!(structured["details"]["failed_index"], json!(1), "{value}");
        assert!(harness.requests().is_empty(), "{value} が IPC へ届きました");
    }
}

#[tokio::test]
async fn malformed_instance_id_never_reaches_an_edit_operation() {
    let harness = Harness::start(responses("set_selection", selection_state()));

    let result = harness
        .server
        .set_selection(Parameters(SetSelectionInput {
            instance_id: "not-a-uuid".to_string(),
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPositionInput { layer: 0, frame: 0 }),
            selected_range: None,
            focus: None,
            display: None,
            expected_project_epoch: EPOCH.to_string(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)["code"], json!("invalid_argument"));
    assert!(
        harness.mock.received_requests().is_empty(),
        "検証前に IPC を発生させない"
    );
}

/// instance_id と params の両方が不正な要求は、instance_id の誤りとして返る。
///
/// cursor と selected_range と focus と display を全て省略した要求は params の
/// 検証で落ちるため、返った誤りがどちらの検証から来たかを message が見分ける。
#[tokio::test]
async fn a_malformed_instance_id_outranks_invalid_parameters() {
    let harness = Harness::start(responses("set_selection", selection_state()));

    let result = harness
        .server
        .set_selection(Parameters(SetSelectionInput {
            instance_id: "not-a-uuid".to_string(),
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: None,
            display: None,
            expected_project_epoch: EPOCH.to_string(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("invalid_argument"));
    assert_eq!(
        structured["message"],
        json!("instance_id はハイフン区切りの UUID である必要があります")
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
        .move_object(Parameters(MoveObjectInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            destination: DestinationInput { layer: 5, frame: 0 },
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
async fn a_content_mismatch_delivers_the_current_object_whole() {
    // 概要はセレクターを内包するため入れ子が深い。秘匿の選別・深さ・文字数の
    // どれかに掛かると、要求元はそのまま送り返せる値を失い、列挙まで戻ることに
    // なる。
    let summary = sample_summary();
    let error = ErrorObject::new(ErrorCode::PreconditionFailed, "対象が変化しました", true)
        .with_details(json!({
            "mismatch": "fingerprint",
            "retry_requires": "refetch",
            "current_object": summary,
        }));
    let harness = Harness::start(OperationResponses::from([(
        "move_object".to_string(),
        err_result(error),
    )]));

    let result = harness
        .server
        .move_object(Parameters(MoveObjectInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            destination: DestinationInput { layer: 5, frame: 0 },
        }))
        .await;

    let structured = structured(&result);
    let current = &structured["details"]["current_object"];
    assert_eq!(*current, serde_json::to_value(&summary).unwrap());

    // 応答が返した値がそのまま次の要求の入力になる。
    let input: ObjectSelectorInput = serde_json::from_value(current["selector"].clone())
        .expect("tool の入力としてそのまま受け取れます");
    assert_eq!(input.frame, summary.selector.frame as u32);
    let selector: ObjectSelector =
        serde_json::from_value(current["selector"].clone()).expect("セレクターを読み取れます");
    assert_eq!(selector, summary.selector);

    // 概要は alias も設定値もパスも持たない。
    let text = serde_json::to_string(&structured).unwrap();
    assert!(!text.contains("秘密の立ち絵"), "{text}");
    assert!(!text.contains(SECRET_PATH), "{text}");
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
        .add_effect(Parameters(AddEffectInput {
            instance_id: harness.instance_id(),
            object: selector_input(),
            effect_name: "ぼかし".to_string(),
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
        .create_object(Parameters(CreateObjectInput {
            instance_id: harness.instance_id(),
            source: ObjectSourceInput::ObjectAlias {
                alias: "[vo]".to_string(),
            },
            placement: PlacementInput {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 0,
            },
            expected_project_epoch: EPOCH.to_string(),
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
        .get_current_scene(Parameters(aviutl2_mcp_server::mcp::input::InstanceInput {
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
        .create_object(Parameters(CreateObjectInput {
            instance_id: harness.instance_id(),
            source: ObjectSourceInput::ObjectAlias {
                alias: SECRET_ALIAS.to_string(),
            },
            placement: PlacementInput {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 0,
            },
            expected_project_epoch: EPOCH.to_string(),
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
        .set_object_item(Parameters(SetObjectItemInput {
            instance_id: harness.instance_id(),
            selector: effect_selector_input(),
            item: "ファイル".to_string(),
            value: ItemValueInput::File {
                path: SECRET_PATH.to_string(),
            },
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
        .set_effect_enabled(Parameters(SetEffectEnabledInput {
            instance_id: harness.instance_id(),
            selector: effect_selector_input(),
            enabled: true,
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
        .delete_object(Parameters(DeleteObjectInput {
            instance_id: InstanceId::new_v4().to_string(),
            selector: selector_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)["code"], json!("instance_not_found"));
    assert!(harness.requests().is_empty(), "削除要求が送られています");
}

/// 一括適用の結果を、移動 1 件と設定変更 1 件で組み立てる。
fn batch_outcome(count: usize) -> Value {
    let results: Vec<BatchStepOutcome> = (0..count)
        .map(|index| BatchStepOutcome {
            object: sample_summary(),
            effect: (index % 2 == 1).then(sample_effect),
        })
        .collect();
    serde_json::to_value(BatchOutcome {
        project_epoch: EPOCH.to_string(),
        project_revision: APPLIED_REVISION,
        results,
    })
    .expect("直列化できる")
}

/// 移動 1 件の sub-operation。
fn move_operation(layer: u32) -> BatchOperationInput {
    BatchOperationInput::MoveObject {
        selector: selector_input(),
        destination: DestinationInput { layer, frame: 0 },
    }
}

/// 互いに別の対象を指す移動の sub-operation。
///
/// 同じ状態を 2 回書き換える要求は検証で落ちるため、件数を並べるには対象を
/// 変えなければならない。
fn distinct_move_operation(index: u32) -> BatchOperationInput {
    BatchOperationInput::MoveObject {
        selector: ObjectSelectorInput {
            layer: index,
            ..selector_input()
        },
        destination: DestinationInput {
            layer: index,
            frame: 0,
        },
    }
}

#[tokio::test]
async fn apply_batch_tool_sends_the_operations_in_order() {
    let expected = batch_outcome(2);
    let harness = Harness::start(responses("apply_batch", expected.clone()));

    let result = harness
        .server
        .apply_batch(Parameters(ApplyBatchInput {
            instance_id: harness.instance_id(),
            operations: vec![
                move_operation(5),
                BatchOperationInput::SetObjectItem {
                    selector: effect_selector_input(),
                    item: "テキスト".to_string(),
                    value: ItemValueInput::Text {
                        value: SECRET_ITEM_VALUE.to_string(),
                    },
                },
            ],
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "apply_batch");
    assert_eq!(
        request.params,
        json!({
            "operations": [
                {
                    "type": "move_object",
                    "selector": selector_json(),
                    "destination": { "layer": 5, "frame": 0 },
                },
                {
                    "type": "set_object_item",
                    "selector": effect_selector_json(),
                    "item": "テキスト",
                    "value": { "type": "text", "value": SECRET_ITEM_VALUE },
                },
            ],
        }),
    );

    let text = text_of(&result);
    assert!(text.contains("2 件の操作"), "{text}");
    assert!(text.contains("project_revision=43"), "{text}");
}

#[tokio::test]
async fn a_hundred_step_batch_stays_within_the_text_limit() {
    let harness = Harness::start(responses("apply_batch", batch_outcome(100)));

    let result = harness
        .server
        .apply_batch(Parameters(ApplyBatchInput {
            instance_id: harness.instance_id(),
            operations: (0..100).map(distinct_move_operation).collect(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    let text = text_of(&result);
    assert!(
        text.chars().count() <= 25_000,
        "text が上限を超えています: {}",
        text.chars().count()
    );
    assert!(text.contains("- [9] "), "{text}");
    assert!(!text.contains("- [10] "), "{text}");
    assert!(text.contains("他 90 件"), "{text}");
}

#[tokio::test]
async fn batch_text_never_echoes_aliases_values_or_paths() {
    let harness = Harness::start(responses("apply_batch", batch_outcome(2)));

    let result = harness
        .server
        .apply_batch(Parameters(ApplyBatchInput {
            instance_id: harness.instance_id(),
            operations: vec![
                move_operation(5),
                BatchOperationInput::SetObjectItem {
                    selector: effect_selector_input(),
                    item: "ファイル".to_string(),
                    value: ItemValueInput::File {
                        path: SECRET_PATH.to_string(),
                    },
                },
            ],
        }))
        .await;

    let text = text_of(&result);
    for forbidden in [SECRET_ITEM_VALUE, SECRET_PATH, "[vo]", "_name="] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} が text にあります: {text}"
        );
    }
    assert!(text.contains("layer=2"), "{text}");
    assert!(text.contains("立ち絵"), "{text}");
}

#[tokio::test]
async fn batch_requests_carry_a_deadline_derived_from_the_batch_budget() {
    // 一括適用は単独編集より長くかかる。編集の予算で期限を作ると、応答して
    // いるインスタンスを途中で打ち切ってしまう。
    let harness = Harness::with_limits(
        responses("apply_batch", batch_outcome(1)),
        CallLimits {
            edit_request: PROBE_EDIT_BUDGET,
            batch_request: PROBE_BATCH_BUDGET,
            ..CallLimits::default()
        },
    );

    let before = Utc::now().timestamp_millis() as u64;
    let result = harness
        .server
        .apply_batch(Parameters(ApplyBatchInput {
            instance_id: harness.instance_id(),
            operations: vec![move_operation(5)],
        }))
        .await;
    let after = Utc::now().timestamp_millis() as u64;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_deadline_from_budget(&harness.only_request(), before, after, PROBE_BATCH_BUDGET);
}

#[tokio::test]
async fn invalid_batch_input_is_rejected_before_any_ipc_and_names_the_operation() {
    let harness = Harness::start(OperationResponses::new());

    let result = harness
        .server
        .apply_batch(Parameters(ApplyBatchInput {
            instance_id: harness.instance_id(),
            // 同じオブジェクトを 2 回動かす要求は、2 つ目の逆操作が 1 つ目の
            // 結果を指すため事前に組み立てられない。
            operations: vec![move_operation(5), move_operation(6)],
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("invalid_argument"));
    assert_eq!(structured["details"]["failed_index"], json!(1));
    assert!(
        harness.mock.received_requests().is_empty(),
        "検証前に IPC を発生させない"
    );
}

#[tokio::test]
async fn a_batch_that_could_not_be_rolled_back_reaches_the_caller_intact() {
    // 巻き戻しに失敗したことを隠さない。要求元が読み直さなければ、次の編集は
    // 壊れた前提の上に積み上がる。
    let error = ErrorObject::new(ErrorCode::SdkError, "巻き戻しに失敗しました", false)
        .with_details(json!({
            "failed_index": 1,
            "rolled_back": false,
            "rolled_back_count": 0,
            "consistency_unknown": true,
            "retry_requires": "refetch",
        }));
    let harness = Harness::start(OperationResponses::from([(
        "apply_batch".to_string(),
        err_result(error),
    )]));

    let result = harness
        .server
        .apply_batch(Parameters(ApplyBatchInput {
            instance_id: harness.instance_id(),
            operations: vec![move_operation(5)],
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("sdk_error"));
    assert_eq!(structured["details"]["failed_index"], json!(1));
    assert_eq!(structured["details"]["rolled_back"], json!(false));
    assert_eq!(structured["details"]["rolled_back_count"], json!(0));
    assert_eq!(structured["details"]["consistency_unknown"], json!(true));

    let text = text_of(&result);
    assert!(text.contains("operations[1]"), "{text}");
    assert!(text.contains("必ず対象を読み直して"), "{text}");
}

/// 中間点の変更が返す応答。
fn object_sections() -> Value {
    serde_json::to_value(ObjectSectionsOutcome {
        project_epoch: EPOCH.to_string(),
        project_revision: APPLIED_REVISION,
        object: sample_summary(),
        sections: vec![
            SectionRange {
                start: 120,
                end: 179,
            },
            SectionRange {
                start: 180,
                end: 240,
            },
        ],
    })
    .expect("直列化できる")
}

#[tokio::test]
async fn create_object_section_tool_sends_create_object_section_operation() {
    let expected = object_sections();
    let harness = Harness::start(responses("create_object_section", expected.clone()));

    let result = harness
        .server
        .create_object_section(Parameters(CreateObjectSectionInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            frame: 180,
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "create_object_section");
    assert_eq!(
        request.params,
        json!({
            "selector": selector_json(),
            "frame": 180,
        }),
    );
}

#[tokio::test]
async fn delete_object_section_tool_sends_delete_object_section_operation() {
    let expected = object_sections();
    let harness = Harness::start(responses("delete_object_section", expected.clone()));

    let result = harness
        .server
        .delete_object_section(Parameters(DeleteObjectSectionInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            section: 1,
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "delete_object_section");
    assert_eq!(
        request.params,
        json!({
            "selector": selector_json(),
            "section": 1,
        }),
    );
}

#[tokio::test]
async fn move_object_section_tool_sends_move_object_section_operation() {
    let expected = object_sections();
    let harness = Harness::start(responses("move_object_section", expected.clone()));

    let result = harness
        .server
        .move_object_section(Parameters(MoveObjectSectionInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            section: 1,
            frame: 200,
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "move_object_section");
    assert_eq!(
        request.params,
        json!({
            "selector": selector_json(),
            "section": 1,
            "frame": 200,
        }),
    );
}

/// 区間番号 0 の要求が IPC へ届かないことを確かめる。
fn assert_section_zero_rejected(harness: &Harness, name: &str, result: &CallToolResult) {
    assert_eq!(result.is_error, Some(true), "{name}");
    let structured = structured(result);
    assert_eq!(structured["code"], json!("invalid_argument"), "{name}");
    assert_eq!(
        structured["details"]["reason"],
        json!("section_index_out_of_range"),
        "{name}"
    );
    assert!(harness.requests().is_empty(), "{name} が IPC へ届きました");
}

#[tokio::test]
async fn deleting_section_zero_is_rejected_before_any_ipc() {
    // schema の minimum は宣言であり、要求がそれを満たすかを rmcp は検証しない。
    // 宣言した制約は server 側で実際に確かめる。
    let harness = Harness::start(responses("delete_object_section", object_sections()));
    let result = harness
        .server
        .delete_object_section(Parameters(DeleteObjectSectionInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            section: 0,
        }))
        .await;
    assert_section_zero_rejected(&harness, "delete_object_section", &result);
}

#[tokio::test]
async fn moving_section_zero_is_rejected_before_any_ipc() {
    let harness = Harness::start(responses("move_object_section", object_sections()));
    let result = harness
        .server
        .move_object_section(Parameters(MoveObjectSectionInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            section: 0,
            frame: 200,
        }))
        .await;
    assert_section_zero_rejected(&harness, "move_object_section", &result);
}

#[tokio::test]
async fn section_responses_carry_neither_the_alias_nor_a_handle() {
    let harness = Harness::start(responses("create_object_section", object_sections()));

    let result = harness
        .server
        .create_object_section(Parameters(CreateObjectSectionInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
            frame: 180,
        }))
        .await;

    let text = text_of(&result);
    let structured = structured(&result).to_string();
    let request = harness.only_request().params.to_string();
    for forbidden in [SECRET_ALIAS, "[vo]", "_name=", "handle"] {
        assert!(!text.contains(forbidden), "text: {text}");
        assert!(!structured.contains(forbidden), "structured: {structured}");
        assert!(!request.contains(forbidden), "params: {request}");
    }
}

#[tokio::test]
async fn a_display_start_request_carries_neither_the_alias_nor_a_handle() {
    let harness = Harness::start(responses("set_selection", selection_state()));

    let result = harness
        .server
        .set_selection(Parameters(SetSelectionInput {
            instance_id: harness.instance_id(),
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: Some(FocusChangeInput::Set {
                object: selector_input(),
            }),
            display: Some(DisplayStartInput {
                layer: 1,
                frame: 60,
            }),
            expected_project_epoch: EPOCH.to_string(),
        }))
        .await;

    let text = text_of(&result);
    let structured = structured(&result).to_string();
    let request = harness.only_request().params.to_string();
    assert!(text.contains("表示開始 frame=60 layer=1"), "text: {text}");
    for forbidden in [SECRET_ALIAS, "[vo]", "_name=", "handle"] {
        assert!(!text.contains(forbidden), "text: {text}");
        assert!(!structured.contains(forbidden), "structured: {structured}");
        assert!(!request.contains(forbidden), "params: {request}");
    }
}

/// BPM グリッドの置き換えが返す応答。
fn grid_bpm_outcome(entries: Vec<GridBpm>) -> Value {
    serde_json::to_value(GridBpmOutcome {
        project_epoch: EPOCH.to_string(),
        project_revision: APPLIED_REVISION,
        entries,
    })
    .expect("直列化できる")
}

/// BPM 情報 1 件の入力。
fn grid_bpm_input(tempo: f64, beat: i64, start: f64, offset: f64) -> GridBpmInput {
    GridBpmInput {
        tempo,
        beat,
        start,
        offset,
    }
}

/// BPM 情報 1 件の DTO。
fn grid_bpm_entry(tempo: f64, beat: i64, start: f64, offset: f64) -> GridBpm {
    GridBpm {
        tempo: FiniteF64::try_new(tempo).expect("有限値"),
        beat,
        start: FiniteF64::try_new(start).expect("有限値"),
        offset: FiniteF64::try_new(offset).expect("有限値"),
    }
}

/// BPM グリッドの置き換えを 1 度だけ呼ぶ。
async fn call_set_grid_bpm(harness: &Harness, entries: Vec<GridBpmInput>) -> CallToolResult {
    harness
        .server
        .set_grid_bpm(Parameters(SetGridBpmInput {
            instance_id: harness.instance_id(),
            expected_scene_id: SCENE_ID,
            entries,
            expected_project_epoch: EPOCH.to_string(),
        }))
        .await
}

#[tokio::test]
async fn set_grid_bpm_tool_sends_set_grid_bpm_operation() {
    let expected = grid_bpm_outcome(vec![grid_bpm_entry(120.0, 4, 0.0, 0.25)]);
    let harness = Harness::start(responses("set_grid_bpm", expected.clone()));

    let result = call_set_grid_bpm(
        &harness,
        vec![
            grid_bpm_input(120.0, 4, 0.0, 0.25),
            grid_bpm_input(90.0, 3, 12.5, 0.0),
        ],
    )
    .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let request = harness.only_request();
    assert_eq!(request.operation, "set_grid_bpm");
    assert_eq!(
        request.params,
        json!({
            "expected_scene_id": SCENE_ID,
            "entries": [
                { "tempo": 120.0, "beat": 4, "start": 0.0, "offset": 0.25 },
                { "tempo": 90.0, "beat": 3, "start": 12.5, "offset": 0.0 },
            ],
            "expected_project_epoch": EPOCH,
        }),
    );
}

#[tokio::test]
async fn an_empty_grid_bpm_request_reaches_the_instance() {
    // グリッドを消す指定である。server が先回りして拒むと手段が無くなる。
    let expected = grid_bpm_outcome(Vec::new());
    let harness = Harness::start(responses("set_grid_bpm", expected.clone()));

    let result = call_set_grid_bpm(&harness, Vec::new()).await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(harness.only_request().params["entries"], json!([]));
}

#[tokio::test]
async fn a_grid_bpm_request_at_the_limit_reaches_the_instance() {
    let expected = grid_bpm_outcome(Vec::new());
    let harness = Harness::start(responses("set_grid_bpm", expected));
    let entries = (0..MAX_GRID_BPM_ENTRIES)
        .map(|index| grid_bpm_input(120.0, 4, index as f64, 0.0))
        .collect::<Vec<_>>();

    let result = call_set_grid_bpm(&harness, entries).await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(
        harness.only_request().params["entries"]
            .as_array()
            .expect("配列")
            .len(),
        MAX_GRID_BPM_ENTRIES
    );
}

#[tokio::test]
async fn a_grid_bpm_request_past_the_limit_never_reaches_the_instance() {
    // schema の maxItems は宣言であり、要求がそれを満たすかを rmcp は検証しない。
    // 宣言した制約は server 側で実際に確かめる。
    let harness = Harness::start(responses("set_grid_bpm", grid_bpm_outcome(Vec::new())));
    let entries = (0..=MAX_GRID_BPM_ENTRIES)
        .map(|index| grid_bpm_input(120.0, 4, index as f64, 0.0))
        .collect::<Vec<_>>();

    let result = call_set_grid_bpm(&harness, entries).await;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)["code"], json!("invalid_argument"));
    assert!(harness.requests().is_empty(), "IPC へ届きました");
}

#[tokio::test]
async fn each_invalid_grid_bpm_request_names_its_own_reason_before_any_ipc() {
    // 検証は core の純関数にあり、server も plugin も同じものを呼ぶ。ここで
    // 確かめるのは、server の経路がその名前をそのまま要求元へ届けることである。
    let cases: &[(&str, Vec<GridBpmInput>, &str)] = &[
        (
            "0 以下の tempo",
            vec![grid_bpm_input(0.0, 4, 0.0, 0.0)],
            "grid_bpm_out_of_range",
        ),
        (
            "1 未満の beat",
            vec![grid_bpm_input(120.0, 0, 0.0, 0.0)],
            "grid_bpm_out_of_range",
        ),
        (
            "負の start",
            vec![grid_bpm_input(120.0, 4, -1.0, 0.0)],
            "grid_bpm_out_of_range",
        ),
        (
            "単精度で無限大になる tempo",
            vec![grid_bpm_input(1.0e300, 4, 0.0, 0.0)],
            "grid_bpm_out_of_range",
        ),
        (
            "単精度で 0 へ潰れる tempo",
            vec![grid_bpm_input(1.0e-300, 4, 0.0, 0.0)],
            "grid_bpm_out_of_range",
        ),
        (
            "重複した start",
            vec![
                grid_bpm_input(120.0, 4, 5.0, 0.0),
                grid_bpm_input(90.0, 3, 5.0, 0.0),
            ],
            "duplicate_target",
        ),
        (
            "i32 に収まらない beat",
            vec![grid_bpm_input(120.0, i64::from(i32::MAX) + 1, 0.0, 0.0)],
            "argument_not_representable",
        ),
    ];
    for (label, entries, reason) in cases {
        let harness = Harness::start(responses("set_grid_bpm", grid_bpm_outcome(Vec::new())));
        let result = call_set_grid_bpm(&harness, entries.clone()).await;

        assert_eq!(result.is_error, Some(true), "{label}");
        let structured = structured(&result);
        assert_eq!(structured["code"], json!("invalid_argument"), "{label}");
        assert_eq!(structured["details"]["reason"], json!(reason), "{label}");
        assert!(harness.requests().is_empty(), "{label} が IPC へ届きました");
    }
}

#[tokio::test]
async fn a_descending_grid_bpm_request_reaches_the_instance() {
    // 並べ替えはホストの仕事である。server が昇順を求めると、要求元は
    // read-back の順序と要求の順序の食い違いを説明できなくなる。
    let harness = Harness::start(responses("set_grid_bpm", grid_bpm_outcome(Vec::new())));

    let result = call_set_grid_bpm(
        &harness,
        vec![
            grid_bpm_input(120.0, 4, 30.0, 0.0),
            grid_bpm_input(120.0, 4, 20.0, 0.0),
            grid_bpm_input(120.0, 4, 10.0, 0.0),
        ],
    )
    .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(
        harness.only_request().params["entries"][0]["start"],
        json!(30.0)
    );
}

#[tokio::test]
async fn a_grid_bpm_response_carries_no_handle_or_alias() {
    let expected = grid_bpm_outcome(vec![grid_bpm_entry(120.0, 4, 0.0, 0.25)]);
    let harness = Harness::start(responses("set_grid_bpm", expected));

    let result = call_set_grid_bpm(&harness, vec![grid_bpm_input(120.0, 4, 0.0, 0.25)]).await;

    let text = text_of(&result);
    let structured = structured(&result).to_string();
    let request = harness.only_request().params.to_string();
    assert!(text.contains("1 件の一覧"), "text: {text}");
    for forbidden in [SECRET_ALIAS, "[vo]", "_name=", "handle"] {
        assert!(!text.contains(forbidden), "text: {text}");
        assert!(!structured.contains(forbidden), "structured: {structured}");
        assert!(!request.contains(forbidden), "params: {request}");
    }
}
