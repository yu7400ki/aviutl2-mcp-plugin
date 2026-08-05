//! read tool から IPC operation への変換と、失敗時の tool result を確認する。

mod support;

use aviutl2_mcp_core::AuthSecret;
use aviutl2_mcp_core::{
    AvailableEffect, AvailableEffectItem, Cursor, DisplayRange, EditInfo, EffectFlags,
    EffectItemValues, EffectType, ErrorCode, ErrorObject, EvaluatedItem, Extent, FiniteF64,
    FrameRange, GetCurrentSceneResult, GridBpm, InstanceId, InstanceState, LayerInfo,
    ListAvailableEffectsResult, ListFontsResult, ListLayersResult, ListModulesResult,
    ListObjectAliasesResult, ListObjectsResult, ListPalettesResult, ModuleEntry, ModuleType,
    ObjectAliasSummary, ObjectDetail, ObjectFingerprintInput, ObjectSummary, PALETTE_COLOR_COUNT,
    PageMeta, PaletteEntry, RequestEnvelope, Rgba, SceneInfo, SectionRange, SelectionSnapshot,
    TrackGroup,
};

use aviutl2_mcp_server::mcp::input::{
    CatalogPageInput, EffectSelectorInput, GetEffectItemValuesInput, GetObjectInput,
    GetSelectionInput, InstanceInput, ListAvailableEffectsInput, ListFontsInput,
    ListInstancesInput, ListLayersInput, ListModulesInput, ListObjectAliasesInput,
    ListObjectsInput, ListPalettesInput, ModuleTypeInput, ObjectFilterInput, ObjectSelectorInput,
    PageInput,
};
use aviutl2_mcp_server::mcp::{AviUtl2McpServer, CallLimits};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;
use support::{
    MOCK_STARTUP_GRACE, MockPipeServer, OperationResponses, current_process_created_at, err_result,
    ok_result, remove_test_registry, temp_registry_dir,
};

/// 生存する mock インスタンスと、それを見る MCP サーバー。
struct Harness {
    server: AviUtl2McpServer,
    mock: MockPipeServer,
    registry_dir: PathBuf,
}

impl Harness {
    /// 指定の operation 応答を返す mock を起こす。
    fn start(responses: OperationResponses) -> Self {
        Self::start_with_delay(responses, Duration::ZERO, CallLimits::default())
    }

    /// read operation の応答を遅らせる mock と、実行予算を縮めたサーバーを起こす。
    ///
    /// 遅延は生存確認の `ping` には掛からないため、期限を使い切るのは read の
    /// 往復だけになる。
    fn start_with_delay(
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

    /// 生存確認の ping を除いた read 要求。
    fn read_requests(&self) -> Vec<RequestEnvelope> {
        self.mock
            .received_requests()
            .into_iter()
            .filter(|request| request.operation != "ping")
            .collect()
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

fn responses(operation: &str, result: serde_json::Value) -> OperationResponses {
    OperationResponses::from([(operation.to_string(), ok_result(result))])
}

fn sample_scene_info() -> SceneInfo {
    SceneInfo {
        id: 3,
        name: Some("Scene 1".to_string()),
        width: 1920,
        height: 1080,
        fps: FiniteF64::try_new(60.0),
        fps_rate: 60,
        fps_scale: 1,
        sample_rate: 48_000,
    }
}

fn sample_page_meta() -> PageMeta {
    PageMeta {
        total_count: 1,
        count: 1,
        offset: 0,
        has_more: false,
        next_offset: None,
        snapshot_revision: 42,
    }
}

fn sample_edit_info() -> EditInfo {
    EditInfo {
        scene: sample_scene_info(),
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
        grid_bpm: vec![GridBpm {
            tempo: FiniteF64::try_new(120.0).expect("有限値"),
            beat: 4,
            start: FiniteF64::try_new(1.5).expect("有限値"),
            offset: FiniteF64::try_new(0.25).expect("有限値"),
        }],
        project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
        project_revision: 42,
    }
}

fn sample_object_summary() -> ObjectSummary {
    ObjectSummary::new(
        "78be92d1-c8c9-44c6-ae52-387548971468",
        ObjectFingerprintInput {
            scene_id: 3,
            layer: 2,
            frame_start: 120,
            frame_end: 240,
            name: Some("立ち絵"),
            alias: "alias",
        },
    )
}

fn selector_input() -> ObjectSelectorInput {
    let selector = sample_object_summary().selector;
    ObjectSelectorInput {
        project_epoch: selector.project_epoch,
        scene_id: selector.scene_id,
        layer: selector.layer as u32,
        frame: selector.frame as u32,
        name: selector.name,
        fingerprint: selector.fingerprint.as_str().to_string(),
    }
}

fn page_input(offset: u32, limit: u32, snapshot_revision: Option<u64>) -> PageInput {
    PageInput {
        offset,
        limit,
        snapshot_revision,
    }
}

fn effects_page_input(offset: u32, limit: u32, snapshot_revision: Option<u64>) -> CatalogPageInput {
    CatalogPageInput {
        offset,
        limit,
        snapshot_revision,
    }
}

#[tokio::test]
async fn get_edit_info_tool_sends_get_edit_info_operation() {
    let expected = serde_json::to_value(sample_edit_info()).expect("直列化できる");
    let harness = Harness::start(responses("get_edit_info", expected.clone()));

    let result = harness
        .server
        .get_edit_info(Parameters(InstanceInput {
            instance_id: harness.instance_id(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);

    let requests = harness.read_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].operation, "get_edit_info");
    assert_eq!(requests[0].params, json!({}));
    assert_eq!(requests[0].instance_id.to_string(), harness.instance_id());
    assert!(
        requests[0].deadline_unix_ms.is_some(),
        "要求へ期限が設定される"
    );
    assert!(text_of(&result).contains("project_revision=42"));
}

#[tokio::test]
async fn get_current_scene_tool_sends_get_current_scene_operation() {
    let expected = serde_json::to_value(GetCurrentSceneResult {
        scene: sample_scene_info(),
        project_revision: 7,
    })
    .expect("直列化できる");
    let harness = Harness::start(responses("get_current_scene", expected.clone()));

    let result = harness
        .server
        .get_current_scene(Parameters(InstanceInput {
            instance_id: harness.instance_id(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);
    let requests = harness.read_requests();
    assert_eq!(requests[0].operation, "get_current_scene");
    assert_eq!(requests[0].params, json!({}));
}

#[tokio::test]
async fn list_layers_tool_sends_flat_page_params() {
    let expected = serde_json::to_value(ListLayersResult {
        items: vec![LayerInfo {
            index: 0,
            name: Some("背景".to_string()),
            enabled: true,
            locked: false,
            object_count: 2,
        }],
        page: sample_page_meta(),
    })
    .expect("直列化できる");
    let harness = Harness::start(responses("list_layers", expected.clone()));

    let result = harness
        .server
        .list_layers(Parameters(ListLayersInput {
            instance_id: harness.instance_id(),
            expected_scene_id: 3,
            page: page_input(5, 10, Some(42)),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);
    let requests = harness.read_requests();
    assert_eq!(requests[0].operation, "list_layers");
    assert_eq!(
        requests[0].params,
        json!({
            "expected_scene_id": 3,
            "offset": 5,
            "limit": 10,
            "snapshot_revision": 42,
        }),
    );
}

#[tokio::test]
async fn list_objects_tool_sends_filter_and_page() {
    let expected = serde_json::to_value(ListObjectsResult {
        items: vec![sample_object_summary()],
        page: sample_page_meta(),
    })
    .expect("直列化できる");
    let harness = Harness::start(responses("list_objects", expected.clone()));

    let result = harness
        .server
        .list_objects(Parameters(ListObjectsInput {
            instance_id: harness.instance_id(),
            expected_scene_id: 3,
            filter: Some(ObjectFilterInput {
                layer_min: Some(1),
                layer_max: Some(8),
            }),
            page: page_input(0, 50, None),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);
    let requests = harness.read_requests();
    assert_eq!(requests[0].operation, "list_objects");
    assert_eq!(
        requests[0].params,
        json!({
            "expected_scene_id": 3,
            "filter": { "layer_min": 1, "layer_max": 8 },
            "offset": 0,
            "limit": 50,
            "snapshot_revision": null,
        }),
    );
}

#[tokio::test]
async fn get_object_tool_sends_selector() {
    let summary = sample_object_summary();
    let expected = serde_json::to_value(ObjectDetail {
        summary: summary.clone(),
        alias: "[vo]\n_name=立ち絵\n".to_string(),
        sections: vec![SectionRange {
            start: 120,
            end: 240,
        }],
        effects: Vec::new(),
        project_revision: 42,
    })
    .expect("直列化できる");
    let harness = Harness::start(responses("get_object", expected.clone()));

    let result = harness
        .server
        .get_object(Parameters(GetObjectInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);
    let requests = harness.read_requests();
    assert_eq!(requests[0].operation, "get_object");
    assert_eq!(
        requests[0].params,
        json!({ "selector": serde_json::to_value(&summary.selector).expect("直列化できる") }),
    );
}

/// フォーカスと選択を持つ選択状態。
fn sample_selection_snapshot() -> SelectionSnapshot {
    SelectionSnapshot {
        project_revision: 42,
        focus: Some(sample_object_summary()),
        focus_section: Some(1),
        selected: vec![sample_object_summary()],
        page: sample_page_meta(),
    }
}

#[tokio::test]
async fn get_selection_tool_sends_the_scene_guard_and_the_page() {
    let expected = serde_json::to_value(sample_selection_snapshot()).expect("直列化できる");
    let harness = Harness::start(responses("get_selection", expected.clone()));

    let result = harness
        .server
        .get_selection(Parameters(GetSelectionInput {
            instance_id: harness.instance_id(),
            expected_scene_id: 3,
            page: page_input(5, 10, Some(42)),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);
    let requests = harness.read_requests();
    assert_eq!(requests[0].operation, "get_selection");
    assert_eq!(
        requests[0].params,
        json!({
            "expected_scene_id": 3,
            "offset": 5,
            "limit": 10,
            "snapshot_revision": 42,
        }),
    );
}

#[tokio::test]
async fn the_selection_text_separates_the_focus_from_the_timeline_selection() {
    // 2 つは別の概念である。同じ応答に並べる以上、並んでいることが「同じもの」と
    // 読まれないようにする。
    let expected = serde_json::to_value(sample_selection_snapshot()).expect("直列化できる");
    let harness = Harness::start(responses("get_selection", expected));

    let result = harness
        .server
        .get_selection(Parameters(GetSelectionInput {
            instance_id: harness.instance_id(),
            expected_scene_id: 3,
            page: page_input(0, 50, None),
        }))
        .await;

    let text = text_of(&result);
    assert!(
        text.contains("オブジェクト設定ウィンドウ"),
        "フォーカスの意味が示されていません: {text}"
    );
    assert!(
        text.contains("タイムライン"),
        "選択の意味が示されていません: {text}"
    );
    assert!(
        text.contains("区間番号 1"),
        "区間番号が示されていません: {text}"
    );
    // 秘匿値は text へ載せない。
    for forbidden in ["alias", "fingerprint", "handle"] {
        assert!(
            !text.contains(forbidden),
            "text content に秘匿値が載りました: {text}"
        );
    }
}

#[tokio::test]
async fn the_selection_carries_neither_the_cursor_nor_the_selected_range() {
    // どちらも get_edit_info が既に返している。同じ値を 2 つの読み取りが返すと、
    // 要求元は「どちらが新しいか」を判断する規則を持つことになる。
    let expected = serde_json::to_value(sample_selection_snapshot()).expect("直列化できる");
    let harness = Harness::start(responses("get_selection", expected));

    let result = harness
        .server
        .get_selection(Parameters(GetSelectionInput {
            instance_id: harness.instance_id(),
            expected_scene_id: 3,
            page: page_input(0, 50, None),
        }))
        .await;

    let structured = structured(&result);
    let fields = structured.as_object().expect("オブジェクト");
    for forbidden in ["cursor", "selected_range", "display"] {
        assert!(
            !fields.contains_key(forbidden),
            "{forbidden} が応答に現れました: {structured}"
        );
    }
    let text = text_of(&result);
    for forbidden in ["cursor", "選択範囲"] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} が text content に現れました: {text}"
        );
    }
}

#[tokio::test]
async fn an_empty_selection_still_reports_the_absence_of_a_focus() {
    let expected = serde_json::to_value(SelectionSnapshot {
        project_revision: 42,
        focus: None,
        focus_section: None,
        selected: Vec::new(),
        page: PageMeta {
            total_count: 0,
            count: 0,
            offset: 0,
            has_more: false,
            next_offset: None,
            snapshot_revision: 42,
        },
    })
    .expect("直列化できる");
    let harness = Harness::start(responses("get_selection", expected.clone()));

    let result = harness
        .server
        .get_selection(Parameters(GetSelectionInput {
            instance_id: harness.instance_id(),
            expected_scene_id: 3,
            page: page_input(0, 50, None),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);
    let text = text_of(&result);
    assert!(text.contains("フォーカス"), "{text}");
    assert!(text.contains("区間番号なし"), "{text}");
}

#[tokio::test]
async fn list_available_effects_tool_sends_effect_type() {
    let expected = serde_json::to_value(ListAvailableEffectsResult {
        items: vec![AvailableEffect {
            name: "ぼかし".to_string(),
            effect_type: EffectType::Filter,
            flags: EffectFlags::from_raw(9),
            items: vec![AvailableEffectItem {
                name: "範囲".to_string(),
                item_type: aviutl2_mcp_core::EffectItemType::Integer,
            }],
        }],
        page: sample_page_meta(),
    })
    .expect("直列化できる");
    let harness = Harness::start(responses("list_available_effects", expected.clone()));

    let result = harness
        .server
        .list_available_effects(Parameters(ListAvailableEffectsInput {
            instance_id: harness.instance_id(),
            effect_type: Some(aviutl2_mcp_server::mcp::input::EffectTypeInput::Filter),
            page: effects_page_input(0, 50, None),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);
    let requests = harness.read_requests();
    assert_eq!(requests[0].operation, "list_available_effects");
    assert_eq!(
        requests[0].params,
        json!({
            "effect_type": "filter",
            "offset": 0,
            "limit": 50,
            "snapshot_revision": null,
        }),
    );
}

#[tokio::test]
async fn list_fonts_tool_sends_only_the_page() {
    let expected = serde_json::to_value(ListFontsResult {
        items: vec!["MS UI Gothic".to_string()],
        page: sample_page_meta(),
    })
    .expect("直列化できる");
    let harness = Harness::start(responses("list_fonts", expected.clone()));

    let result = harness
        .server
        .list_fonts(Parameters(ListFontsInput {
            instance_id: harness.instance_id(),
            page: effects_page_input(0, 50, None),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);
    let requests = harness.read_requests();
    assert_eq!(requests[0].operation, "list_fonts");
    assert_eq!(
        requests[0].params,
        json!({ "offset": 0, "limit": 50, "snapshot_revision": null }),
    );
}

#[tokio::test]
async fn list_palettes_tool_carries_the_current_name_and_every_colour() {
    let expected = serde_json::to_value(ListPalettesResult {
        current: Some("[標準.既定]".to_string()),
        items: vec![PaletteEntry {
            name: "既定".to_string(),
            colors: vec![
                Rgba {
                    r: 1,
                    g: 2,
                    b: 3,
                    a: 255
                };
                PALETTE_COLOR_COUNT
            ],
        }],
        page: sample_page_meta(),
    })
    .expect("直列化できる");
    let harness = Harness::start(responses("list_palettes", expected.clone()));

    let result = harness
        .server
        .list_palettes(Parameters(ListPalettesInput {
            instance_id: harness.instance_id(),
            page: effects_page_input(0, 50, None),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);
    let structured = structured(&result);
    assert_eq!(
        structured["items"][0]["colors"].as_array().unwrap().len(),
        64
    );
    assert_eq!(structured["items"][0]["colors"][0]["a"], 255);

    let requests = harness.read_requests();
    assert_eq!(requests[0].operation, "list_palettes");
    assert_eq!(
        requests[0].params,
        json!({ "offset": 0, "limit": 50, "snapshot_revision": null }),
    );
}

#[tokio::test]
async fn list_modules_tool_sends_the_type_filter() {
    let expected = serde_json::to_value(ListModulesResult {
        items: vec![ModuleEntry {
            module_type: ModuleType::PluginInput,
            name: "入力プラグイン".to_string(),
            information: "動画の読み込み".to_string(),
        }],
        page: sample_page_meta(),
    })
    .expect("直列化できる");
    let harness = Harness::start(responses("list_modules", expected.clone()));

    let result = harness
        .server
        .list_modules(Parameters(ListModulesInput {
            instance_id: harness.instance_id(),
            module_type: Some(ModuleTypeInput::PluginInput),
            page: effects_page_input(0, 50, None),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);
    // 説明文は structuredContent が運び、text へは載せない。
    let text = text_of(&result);
    assert!(text.contains("入力プラグイン"), "{text}");
    assert!(!text.contains("動画の読み込み"), "{text}");

    let requests = harness.read_requests();
    assert_eq!(requests[0].operation, "list_modules");
    assert_eq!(
        requests[0].params,
        json!({
            "module_type": "plugin_input",
            "offset": 0,
            "limit": 50,
            "snapshot_revision": null,
        }),
    );
}

/// エイリアスの生テキスト。応答のどこにも現れてはならない。
const SECRET_ALIAS_TEXT: &str = "[vo]\n_name=秘密の立ち絵\n";

fn sample_object_aliases() -> ListObjectAliasesResult {
    ListObjectAliasesResult {
        items: vec![ObjectAliasSummary {
            name: "立ち絵".to_string(),
            label: Some("キャラ".to_string()),
            object_count: Some(2),
            effects: vec!["テキスト".to_string(), "標準描画".to_string()],
        }],
        page: sample_page_meta(),
    }
}

#[tokio::test]
async fn list_object_aliases_tool_sends_the_label_and_the_flat_page() {
    let expected = serde_json::to_value(sample_object_aliases()).expect("直列化できる");
    let harness = Harness::start(responses("list_object_aliases", expected.clone()));

    let result = harness
        .server
        .list_object_aliases(Parameters(ListObjectAliasesInput {
            instance_id: harness.instance_id(),
            label: Some("キャラ".to_string()),
            page: effects_page_input(0, 50, Some(42)),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);
    let requests = harness.read_requests();
    assert_eq!(requests[0].operation, "list_object_aliases");
    assert_eq!(
        requests[0].params,
        json!({
            "label": "キャラ",
            "offset": 0,
            "limit": 50,
            "snapshot_revision": 42,
        }),
    );
}

#[tokio::test]
async fn an_object_alias_listing_carries_neither_the_alias_text_nor_a_path() {
    // 要約に生テキストの置き場は無い。接続先が余分な欄で送ってきても、
    // text にも structuredContent にも現れない。
    let mut expected = serde_json::to_value(sample_object_aliases()).expect("直列化できる");
    expected["items"][0]["raw"] = json!(SECRET_ALIAS_TEXT);
    expected["items"][0]["path"] = json!(r"C:\Users\tester\Alias\立ち絵.object");
    let harness = Harness::start(responses("list_object_aliases", expected));

    let result = harness
        .server
        .list_object_aliases(Parameters(ListObjectAliasesInput {
            instance_id: harness.instance_id(),
            label: None,
            page: effects_page_input(0, 50, None),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    let text = text_of(&result);
    let structured = structured(&result).to_string();
    // 利用者が付けた名前は返す。
    assert!(text.contains("立ち絵"), "{text}");
    assert!(text.contains("label=キャラ"), "{text}");
    for forbidden in [SECRET_ALIAS_TEXT, "[vo]", "_name=", "tester", ".object"] {
        assert!(!text.contains(forbidden), "text: {text}");
        assert!(!structured.contains(forbidden), "structured: {structured}");
    }
}

#[tokio::test]
async fn a_label_that_breaks_the_name_rule_never_reaches_the_instance() {
    // label の構文は要求内容だけで決まる。接続前に落とさなければ、要求の誤りが
    // 転送の失敗として報告される。
    for label in ["ラベル\u{0}".to_string(), "あ".repeat(1_025)] {
        let harness = Harness::start(OperationResponses::new());

        let result = harness
            .server
            .list_object_aliases(Parameters(ListObjectAliasesInput {
                instance_id: harness.instance_id(),
                label: Some(label),
                page: effects_page_input(0, 50, None),
            }))
            .await;

        assert_eq!(result.is_error, Some(true), "{}", text_of(&result));
        let structured = structured(&result);
        assert_eq!(structured["code"], json!("invalid_argument"));
        // どの規則で落ちたかを機械可読な形で添える。
        assert!(
            structured["details"]["reason"].is_string(),
            "落ちた規則の名前がありません: {structured}"
        );
        assert!(harness.read_requests().is_empty());
    }
}

/// 立ち絵オブジェクトの effect を指すセレクター。
fn effect_selector_input() -> EffectSelectorInput {
    let selector = selector_input();
    let fingerprint = selector.fingerprint.clone();
    EffectSelectorInput {
        object: selector,
        effect_name: "標準描画".to_string(),
        effect_index: 0,
        fingerprint,
    }
}

/// 評価した値を含む応答。
fn sample_effect_item_values() -> EffectItemValues {
    EffectItemValues {
        project_revision: 42,
        frames: vec![
            FiniteF64::try_new(120.0).expect("有限値"),
            FiniteF64::try_new(120.5).expect("有限値"),
        ],
        items: vec![
            EvaluatedItem::Track {
                name: "X".to_string(),
                values: vec![
                    FiniteF64::try_new(640.0).expect("有限値"),
                    FiniteF64::try_new(645.25).expect("有限値"),
                ],
                group: Some(TrackGroup {
                    name: "座標".to_string(),
                    index: 0,
                    count: 3,
                    item_names: vec!["X".to_string(), "Y".to_string()],
                }),
            },
            EvaluatedItem::Check {
                name: "反転".to_string(),
                values: vec![true, false],
            },
        ],
        truncated: false,
    }
}

#[tokio::test]
async fn get_effect_item_values_tool_sends_the_effect_and_the_frames() {
    let expected = serde_json::to_value(sample_effect_item_values()).expect("直列化できる");
    let harness = Harness::start(responses("get_effect_item_values", expected.clone()));

    let result = harness
        .server
        .get_effect_item_values(Parameters(GetEffectItemValuesInput {
            instance_id: harness.instance_id(),
            effect: effect_selector_input(),
            frames: vec![120.0, 120.5],
            items: Some(vec!["X".to_string(), "反転".to_string()]),
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(structured(&result), expected);
    let requests = harness.read_requests();
    assert_eq!(requests[0].operation, "get_effect_item_values");
    assert_eq!(requests[0].params["frames"], json!([120.0, 120.5]));
    assert_eq!(requests[0].params["items"], json!(["X", "反転"]));
}

#[tokio::test]
async fn the_evaluated_values_reach_the_structured_content_but_not_the_text() {
    // 値そのものを返すのがこの tool の目的であり、structuredContent には載せる。
    // text content は既存の規則どおり値を載せない。
    let expected = serde_json::to_value(sample_effect_item_values()).expect("直列化できる");
    let harness = Harness::start(responses("get_effect_item_values", expected.clone()));

    let result = harness
        .server
        .get_effect_item_values(Parameters(GetEffectItemValuesInput {
            instance_id: harness.instance_id(),
            effect: effect_selector_input(),
            frames: vec![120.0, 120.5],
            items: None,
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    assert_eq!(
        structured(&result)["items"][0]["values"],
        json!([640.0, 645.25])
    );
    let text = text_of(&result);
    for value in ["640", "645.25", "true", "false"] {
        assert!(
            !text.contains(value),
            "text content に値が載りました: {text}"
        );
    }
    for forbidden in ["alias", "fingerprint", "handle"] {
        assert!(
            !text.contains(forbidden),
            "text content に秘匿値が載りました: {text}"
        );
    }
}

#[tokio::test]
async fn out_of_range_frame_and_item_counts_never_reach_the_instance() {
    // 件数は要求内容だけで決まる。接続前に落とさなければ、要求の誤りが転送の
    // 失敗として報告される。
    let over_frames: Vec<f64> = vec![120.0; 17];
    let over_items: Vec<String> = (0..33).map(|index| format!("項目{index}")).collect();
    for (frames, items) in [
        (Vec::new(), None),
        (over_frames, None),
        (vec![120.0], Some(Vec::new())),
        (vec![120.0], Some(over_items)),
    ] {
        let harness = Harness::start(OperationResponses::new());
        let result = harness
            .server
            .get_effect_item_values(Parameters(GetEffectItemValuesInput {
                instance_id: harness.instance_id(),
                effect: effect_selector_input(),
                frames,
                items,
            }))
            .await;

        assert_eq!(result.is_error, Some(true), "{}", text_of(&result));
        assert!(
            harness.read_requests().is_empty(),
            "件数の誤りが接続先へ送られました"
        );
    }
}

#[tokio::test]
async fn list_instances_tool_lists_live_mock() {
    let harness = Harness::start(OperationResponses::new());

    let result = harness
        .server
        .list_instances(Parameters(ListInstancesInput {
            offset: 0,
            limit: 50,
        }))
        .await;

    assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    let structured = structured(&result);
    assert_eq!(structured["total_count"], json!(1));
    assert_eq!(
        structured["instances"][0]["instance_id"],
        json!(harness.instance_id())
    );
    assert!(text_of(&result).contains(&harness.instance_id()));
}

#[tokio::test]
async fn unknown_instance_id_becomes_instance_not_found() {
    let harness = Harness::start(OperationResponses::new());

    let result = harness
        .server
        .get_edit_info(Parameters(InstanceInput {
            instance_id: InstanceId::new_v4().to_string(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("instance_not_found"));
    assert_eq!(structured["retryable"], json!(false));
    assert!(structured["correlation_id"].is_string());
    assert!(harness.read_requests().is_empty());
}

#[tokio::test]
async fn dead_instance_becomes_instance_stale() {
    let registry_dir = temp_registry_dir();
    let mock = MockPipeServer::start(
        InstanceId::new_v4(),
        AuthSecret::generate(),
        std::process::id(),
        current_process_created_at(),
        InstanceState::Ready,
    );
    // 生存しない PID を指す descriptor を書き、生存確認を落とす。
    let mut descriptor = mock.descriptor(registry_dir.clone());
    descriptor.pid = 0xFFFF_FFFF;
    std::fs::create_dir_all(&registry_dir).expect("registry を作れる");
    std::fs::write(
        registry_dir.join(format!("{}.json", descriptor.instance_id)),
        serde_json::to_string(&descriptor).expect("直列化できる"),
    )
    .expect("descriptor を書ける");

    let server =
        AviUtl2McpServer::without_artifact_store(registry_dir.clone(), CallLimits::default());
    let result = server
        .get_edit_info(Parameters(InstanceInput {
            instance_id: descriptor.instance_id.to_string(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("instance_stale"));
    assert_eq!(structured["retryable"], json!(true));

    drop(mock);
    remove_test_registry(&registry_dir);
}

#[tokio::test]
async fn malformed_instance_id_becomes_invalid_argument() {
    let harness = Harness::start(OperationResponses::new());

    let result = harness
        .server
        .get_edit_info(Parameters(InstanceInput {
            instance_id: "not-a-uuid".to_string(),
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
async fn out_of_range_limit_becomes_invalid_argument() {
    let harness = Harness::start(OperationResponses::new());

    let result = harness
        .server
        .list_layers(Parameters(ListLayersInput {
            instance_id: harness.instance_id(),
            expected_scene_id: 0,
            page: page_input(0, 201, None),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)["code"], json!("invalid_argument"));
    assert!(harness.read_requests().is_empty());
}

#[tokio::test]
async fn remote_error_is_returned_as_tool_error_with_retry_after() {
    let error = ErrorObject::new(ErrorCode::HostBusy, "読み取りキューが飽和しています", true)
        .with_details(json!({ "retry_after_ms": 500 }));
    let harness = Harness::start(OperationResponses::from([(
        "get_edit_info".to_string(),
        err_result(error),
    )]));

    let result = harness
        .server
        .get_edit_info(Parameters(InstanceInput {
            instance_id: harness.instance_id(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("host_busy"));
    assert_eq!(structured["retryable"], json!(true));
    assert_eq!(structured["details"]["retry_after_ms"], json!(500));
    assert!(structured["correlation_id"].is_string());
}

#[tokio::test]
async fn unsupported_operation_from_instance_is_reported() {
    let harness = Harness::start(OperationResponses::new());

    let result = harness
        .server
        .get_object(Parameters(GetObjectInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)["code"], json!("unsupported_operation"));
}

#[tokio::test]
async fn response_of_wrong_shape_is_rejected() {
    let harness = Harness::start(responses("get_edit_info", json!({ "unexpected": true })));

    let result = harness
        .server
        .get_edit_info(Parameters(InstanceInput {
            instance_id: harness.instance_id(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)["code"], json!("instance_stale"));
}

#[tokio::test]
async fn each_tool_call_uses_a_fresh_connection() {
    let expected = serde_json::to_value(sample_edit_info()).expect("直列化できる");
    let harness = Harness::start(responses("get_edit_info", expected.clone()));

    for _ in 0..3 {
        let result = harness
            .server
            .get_edit_info(Parameters(InstanceInput {
                instance_id: harness.instance_id(),
            }))
            .await;
        assert_eq!(result.is_error, Some(false), "{}", text_of(&result));
    }

    // 接続ごとに生存確認の ping が 1 回入るため、read 要求と同数になる。
    let all = harness.mock.received_requests();
    let pings = all.iter().filter(|r| r.operation == "ping").count();
    assert_eq!(pings, 3, "tool call ごとに接続を張り直す");
    assert_eq!(harness.read_requests().len(), 3);
}

/// 応答しないインスタンスを演じる時間。要求予算を確実に超える長さにする。
const SLOW_READ: Duration = Duration::from_millis(500);

/// 期限超過を起こすために縮めた read operation の予算。
///
/// [`SLOW_READ`] より十分短く、接続と生存確認が終わるだけの余裕はある値を選ぶ。
const SHORT_REQUEST_BUDGET: Duration = Duration::from_millis(200);

#[tokio::test]
async fn read_that_outlasts_the_request_budget_becomes_timeout() {
    // 期限超過は接続先の応答ではなく server 側の打ち切りで起きるため、
    // 正常な結果を返す mock を遅らせるだけで再現できる。
    let expected = serde_json::to_value(sample_edit_info()).expect("直列化できる");
    let harness = Harness::start_with_delay(
        responses("get_edit_info", expected),
        SLOW_READ,
        CallLimits {
            request: SHORT_REQUEST_BUDGET,
            ..CallLimits::default()
        },
    );

    let result = harness
        .server
        .get_edit_info(Parameters(InstanceInput {
            instance_id: harness.instance_id(),
        }))
        .await;

    assert_eq!(result.is_error, Some(true), "{}", text_of(&result));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("timeout"), "{structured}");
    assert_eq!(structured["retryable"], json!(true), "{structured}");
    assert!(structured["correlation_id"].is_string(), "{structured}");
    assert!(text_of(&result).contains("timeout"), "{}", text_of(&result));

    // 要求自体は届いており、接続先も期限を知らされている。
    let requests = harness.read_requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(
        requests[0].deadline_unix_ms.is_some(),
        "打ち切る側の期限が要求へ載っていません"
    );
}

/// 注入したエラーを `get_object` の tool result として受け取る。
async fn get_object_failure(error: ErrorObject) -> CallToolResult {
    let harness = Harness::start(OperationResponses::from([(
        "get_object".to_string(),
        err_result(error),
    )]));
    harness
        .server
        .get_object(Parameters(GetObjectInput {
            instance_id: harness.instance_id(),
            selector: selector_input(),
        }))
        .await
}

#[tokio::test]
async fn ambiguous_selector_reaches_the_tool_result_with_candidate_count() {
    let result = get_object_failure(
        ErrorObject::new(
            ErrorCode::AmbiguousSelector,
            "セレクターが複数のオブジェクトに一致しました",
            false,
        )
        .with_details(json!({ "candidate_count": 3 })),
    )
    .await;

    assert_eq!(result.is_error, Some(true));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("ambiguous_selector"));
    assert_eq!(structured["retryable"], json!(false));
    assert_eq!(structured["details"]["candidate_count"], json!(3));
    let text = text_of(&result);
    assert!(text.contains("ambiguous_selector"), "{text}");
    assert!(text.contains("複数のオブジェクト"), "{text}");
}

#[tokio::test]
async fn edit_blocked_reaches_the_tool_result_with_edit_state() {
    let result = get_object_failure(
        ErrorObject::new(
            ErrorCode::EditBlocked,
            "プレビュー中のため読み取れません",
            true,
        )
        .with_details(json!({ "edit_state": "preview", "retry_after_ms": 250 })),
    )
    .await;

    assert_eq!(result.is_error, Some(true));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("edit_blocked"));
    assert_eq!(structured["retryable"], json!(true));
    // 待ち直しの判断に要る内訳は落とさない。
    assert_eq!(structured["details"]["edit_state"], json!("preview"));
    assert_eq!(structured["details"]["retry_after_ms"], json!(250));
    let text = text_of(&result);
    assert!(text.contains("edit_blocked"), "{text}");
    assert!(text.contains("リトライ可能"), "{text}");
}

#[tokio::test]
async fn not_found_selector_reaches_the_tool_result() {
    let result = get_object_failure(ErrorObject::new(
        ErrorCode::NotFound,
        "セレクターに一致するオブジェクトがありません",
        false,
    ))
    .await;

    assert_eq!(result.is_error, Some(true));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("not_found"));
    assert_eq!(structured["retryable"], json!(false));
    let text = text_of(&result);
    assert!(text.contains("not_found"), "{text}");
    assert!(text.contains("一致するオブジェクト"), "{text}");
}

#[tokio::test]
async fn precondition_failed_reaches_the_tool_result_with_current_revision() {
    let result = get_object_failure(
        ErrorObject::new(
            ErrorCode::PreconditionFailed,
            "プロジェクトが変化しました",
            true,
        )
        .with_details(json!({
            "current_project_revision": 12,
            "expected_project_revision": 7,
        })),
    )
    .await;

    assert_eq!(result.is_error, Some(true));
    let structured = structured(&result);
    assert_eq!(structured["code"], json!("precondition_failed"));
    assert_eq!(structured["retryable"], json!(true));
    // 取り直しの基準になる revision を落とさない。
    assert_eq!(structured["details"]["current_project_revision"], json!(12));
    assert_eq!(structured["details"]["expected_project_revision"], json!(7));
    assert!(text_of(&result).contains("precondition_failed"));
}

#[tokio::test]
async fn secrets_in_remote_details_never_reach_the_tool_result() {
    // 接続先が秘匿すべき値を details に載せてきても、tool result へは出さない。
    let result = get_object_failure(
        ErrorObject::new(
            ErrorCode::EditBlocked,
            "プレビュー中のため読み取れません",
            true,
        )
        .with_details(json!({
            "auth_secret": "s3cr3t-value",
            "raw_pointer": "0xdeadbeef",
            "pipe_name": r"\\.\pipe\aviutl2-mcp-leaked",
            "object_alias": "[vo]\n_name=leaked-alias",
            "project_path": r"C:\Users\tester\leaked-project.aup2",
            "edit_state": "preview",
            "retry_after_ms": 250,
            "candidate_count": 2,
        })),
    )
    .await;

    let structured = structured(&result);
    let text = text_of(&result);
    let serialized = serde_json::to_string(&result).expect("直列化できる");
    for forbidden in [
        "s3cr3t-value",
        "0xdeadbeef",
        "aviutl2-mcp-leaked",
        "leaked-alias",
        "leaked-project",
        "tester",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "{forbidden} が tool result に含まれています: {serialized}"
        );
        assert!(!text.contains(forbidden), "{forbidden} が text にあります");
    }

    // 秘匿が効きすぎて、待ち直しと再解決に要る内訳まで落ちてはならない。
    assert_eq!(structured["code"], json!("edit_blocked"));
    assert_eq!(structured["retryable"], json!(true));
    assert_eq!(structured["details"]["edit_state"], json!("preview"));
    assert_eq!(structured["details"]["retry_after_ms"], json!(250));
    assert_eq!(structured["details"]["candidate_count"], json!(2));
}
