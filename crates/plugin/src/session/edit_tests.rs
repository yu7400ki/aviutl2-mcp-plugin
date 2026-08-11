use super::decode::EditRequest;
use super::*;
use crate::edit::error::RollbackOutcome;
use aviutl2_mcp_core::{
    AddEffectParams, CreateObjectParams, CreateObjectSectionParams, DeleteEffectParams,
    DeleteObjectParams, DeleteObjectSectionParams, EditOutcome, FiniteF64, GridBpmOutcome,
    LayerInfo, LayerStateOutcome, MAX_GRID_BPM_ENTRIES, MAX_ITEM_VALUE_BYTES, MAX_PATH_UTF16_UNITS,
    MAX_POSITION, MoveEffectParams, MoveObjectParams, MoveObjectSectionParams,
    ObjectFingerprintInput, ObjectSectionsOutcome, ObjectSummary, RequestBudgetKind, SceneInfo,
    SceneSettingsOutcome, SectionRange, SelectionField, SelectionState, SetEffectEnabledParams,
    SetGridBpmParams, SetLayerStateParams, SetObjectItemParams, SetObjectNameParams,
    SetSceneSettingsParams, SetSelectionParams,
};
use serde_json::json;
use std::sync::Mutex;

const EPOCH: &str = "9d0a5f4e-2f47-4a13-9a5e-1e2f3a4b5c6d";
const SCENE_ID: i32 = 0;

/// 編集口の代わりに定型データを返す実装。
///
/// 呼ばれた operation を記録するため、受付判定や params の検証で弾かれた
/// 要求が編集へ進んでいないことを確かめられる。
struct FakeEditAdapter {
    calls: Mutex<Vec<&'static str>>,
}

impl FakeEditAdapter {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    fn enter(&self, call: &'static str) -> EditOutcome {
        self.calls.lock().unwrap().push(call);
        EditOutcome::object_changed(EPOCH, 1, fake_summary())
    }

    fn enter_sections(&self, call: &'static str) -> ObjectSectionsOutcome {
        self.calls.lock().unwrap().push(call);
        ObjectSectionsOutcome {
            project_epoch: EPOCH.to_string(),
            project_revision: 1,
            object: fake_summary(),
            sections: vec![SectionRange {
                start: 100,
                end: 200,
            }],
        }
    }
}

fn fake_summary() -> ObjectSummary {
    ObjectSummary::new(
        EPOCH,
        ObjectFingerprintInput {
            scene_id: SCENE_ID,
            layer: 1,
            frame_start: 100,
            frame_end: 200,
            name: None,
            alias: "[1:100]",
        },
    )
}

/// 対象を指す effect セレクター。解決はフェイクが行わないため値は任意でよい。
fn fake_effect_selector() -> Value {
    json!({
        "object": fake_summary().selector,
        "effect_name": "動画ファイル",
        "effect_index": 0,
        "fingerprint": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    })
}

impl EditAdapter for FakeEditAdapter {
    fn create_object(&self, _: &CreateObjectParams) -> Result<EditOutcome, EditError> {
        Ok(self.enter("create_object"))
    }

    fn move_object(&self, _: &MoveObjectParams) -> Result<EditOutcome, EditError> {
        Ok(self.enter("move_object"))
    }

    fn delete_object(&self, _: &DeleteObjectParams) -> Result<EditOutcome, EditError> {
        Ok(self.enter("delete_object"))
    }

    fn set_object_name(&self, _: &SetObjectNameParams) -> Result<EditOutcome, EditError> {
        Ok(self.enter("set_object_name"))
    }

    fn create_object_section(
        &self,
        _: &CreateObjectSectionParams,
    ) -> Result<ObjectSectionsOutcome, EditError> {
        Ok(self.enter_sections("create_object_section"))
    }

    fn delete_object_section(
        &self,
        _: &DeleteObjectSectionParams,
    ) -> Result<ObjectSectionsOutcome, EditError> {
        Ok(self.enter_sections("delete_object_section"))
    }

    fn move_object_section(
        &self,
        _: &MoveObjectSectionParams,
    ) -> Result<ObjectSectionsOutcome, EditError> {
        Ok(self.enter_sections("move_object_section"))
    }

    fn set_grid_bpm(&self, _: &SetGridBpmParams) -> Result<GridBpmOutcome, EditError> {
        self.calls.lock().unwrap().push("set_grid_bpm");
        Ok(GridBpmOutcome {
            project_epoch: EPOCH.to_string(),
            project_revision: 1,
            entries: Vec::new(),
        })
    }

    fn set_object_item(&self, _: &SetObjectItemParams) -> Result<EditOutcome, EditError> {
        Ok(self.enter("set_object_item"))
    }

    fn add_effect(&self, _: &AddEffectParams) -> Result<EditOutcome, EditError> {
        Ok(self.enter("add_effect"))
    }

    fn delete_effect(&self, _: &DeleteEffectParams) -> Result<EditOutcome, EditError> {
        Ok(self.enter("delete_effect"))
    }

    fn set_effect_enabled(&self, _: &SetEffectEnabledParams) -> Result<EditOutcome, EditError> {
        Ok(self.enter("set_effect_enabled"))
    }

    fn move_effect(&self, _: &MoveEffectParams) -> Result<EditOutcome, EditError> {
        Ok(self.enter("move_effect"))
    }

    fn set_scene_settings(
        &self,
        _: &SetSceneSettingsParams,
    ) -> Result<SceneSettingsOutcome, EditError> {
        self.calls.lock().unwrap().push("set_scene_settings");
        Ok(SceneSettingsOutcome {
            project_epoch: EPOCH.to_string(),
            project_revision: 1,
            scene: SceneInfo {
                id: SCENE_ID,
                name: Some("本編".to_string()),
                width: 1280,
                height: 720,
                fps: FiniteF64::try_new(30.0),
                fps_rate: 30,
                fps_scale: 1,
                sample_rate: 48000,
            },
            observed_after_edit: true,
            non_undoable: true,
        })
    }

    fn set_layer_state(&self, _: &SetLayerStateParams) -> Result<LayerStateOutcome, EditError> {
        self.calls.lock().unwrap().push("set_layer_state");
        Ok(LayerStateOutcome {
            project_epoch: EPOCH.to_string(),
            project_revision: 1,
            layer: LayerInfo {
                index: 1,
                name: Some("背景".to_string()),
                enabled: true,
                locked: false,
                object_count: 0,
            },
        })
    }

    fn apply_batch(
        &self,
        _: &aviutl2_mcp_core::ApplyBatchParams,
    ) -> Result<aviutl2_mcp_core::BatchOutcome, EditError> {
        self.calls.lock().unwrap().push("apply_batch");
        Ok(aviutl2_mcp_core::BatchOutcome {
            project_epoch: EPOCH.to_string(),
            project_revision: 1,
            results: Vec::new(),
        })
    }

    fn set_selection(&self, _: &SetSelectionParams) -> Result<SelectionState, EditError> {
        self.calls.lock().unwrap().push("set_selection");
        Ok(SelectionState::observed(
            EPOCH,
            1,
            aviutl2_mcp_core::ObservedSelection {
                cursor: aviutl2_mcp_core::Cursor { frame: 0, layer: 0 },
                selected_range: None,
                focus: None,
                display: aviutl2_mcp_core::DisplayRange {
                    frame_start: 0,
                    layer_start: 0,
                    frame_num: 0,
                    layer_num: 0,
                },
            },
            vec![SelectionField::Cursor],
            Vec::new(),
        ))
    }
}

/// 有効な選択状態の変更 params。
fn selection_params() -> Value {
    json!({
        "expected_scene_id": SCENE_ID,
        "cursor": { "layer": 0, "frame": 0 },
        "expected_project_epoch": EPOCH,
    })
}

/// operation ごとの、現在の形の要求 params を引く。実行口を持たない
/// operation は `None`。
///
/// **`_` を使わない網羅 match で書く。** 編集 operation を足すとここが
/// コンパイルエラーになるため、要求の形を確かめる一連のテストから漏れる
/// ことがない。手書きの一覧にすると、足し忘れても全て緑のまま通ってしまう。
fn current_request(operation: EditOperation) -> Option<Value> {
    Some(match operation {
        EditOperation::CreateObject => json!({
            "source": { "type": "object_alias", "alias": "[1:100]" },
            "placement": { "scene_id": SCENE_ID, "layer": 1, "frame": 0 },
            "expected_project_epoch": EPOCH,
        }),
        EditOperation::MoveObject => json!({
            "selector": fake_summary().selector,
            "destination": { "layer": 1, "frame": 300 },
        }),
        EditOperation::DeleteObject => json!({ "selector": fake_summary().selector }),
        EditOperation::SetObjectName => json!({
            "selector": fake_summary().selector,
            "name": "名前",
        }),
        EditOperation::SetObjectItem => json!({
            "selector": fake_effect_selector(),
            "item": "X",
            "value": { "type": "integer", "value": 1 },
        }),
        EditOperation::AddEffect => json!({
            "object": fake_summary().selector,
            "effect_name": "ぼかし",
        }),
        EditOperation::DeleteEffect => json!({ "selector": fake_effect_selector() }),
        EditOperation::SetEffectEnabled => json!({
            "selector": fake_effect_selector(),
            "enabled": true,
        }),
        EditOperation::MoveEffect => json!({
            "selector": fake_effect_selector(),
            "position": 1,
        }),
        EditOperation::SetLayerState => json!({
            "expected_scene_id": SCENE_ID,
            "layer": 1,
            "name": { "type": "set", "name": "背景" },
            "expected_project_epoch": EPOCH,
        }),
        EditOperation::SetSelection => selection_params(),
        EditOperation::CreateObjectSection => json!({
            "selector": fake_summary().selector,
            "frame": 150,
        }),
        EditOperation::DeleteObjectSection => json!({
            "selector": fake_summary().selector,
            "section": 1,
        }),
        EditOperation::MoveObjectSection => json!({
            "selector": fake_summary().selector,
            "section": 1,
            "frame": 160,
        }),
        EditOperation::SetGridBpm => json!({
            "expected_scene_id": SCENE_ID,
            "entries": [{ "tempo": 120.0, "beat": 4, "start": 0.0, "offset": 0.0 }],
            "expected_project_epoch": EPOCH,
        }),
        EditOperation::SetSceneSettings => json!({
            "expected_scene_id": SCENE_ID,
            "name": "本編",
            "size": { "width": 1280, "height": 720 },
            "sample_rate": 48000,
            "expected_project_epoch": EPOCH,
        }),
        EditOperation::ApplyBatch => batch_params(),
    })
}

/// 移動 1 件だけの一括適用 params。
fn batch_params() -> Value {
    json!({
        "operations": [{
            "type": "move_object",
            "selector": fake_summary().selector,
            "destination": { "layer": 1, "frame": 300 },
        }],
    })
}

/// 要求を復号し、成功したら params を JSON へ写して返す。
fn decode_request(operation: EditOperation, params: &Value) -> Result<Value, ErrorObject> {
    let request = decode_edit_request(operation, params)?;
    let encoded = match &request {
        EditRequest::CreateObject(params) => serde_json::to_value(params),
        EditRequest::MoveObject(params) => serde_json::to_value(params),
        EditRequest::DeleteObject(params) => serde_json::to_value(params),
        EditRequest::SetObjectName(params) => serde_json::to_value(params),
        EditRequest::SetObjectItem(params) => serde_json::to_value(params),
        EditRequest::AddEffect(params) => serde_json::to_value(params),
        EditRequest::DeleteEffect(params) => serde_json::to_value(params),
        EditRequest::SetEffectEnabled(params) => serde_json::to_value(params),
        EditRequest::MoveEffect(params) => serde_json::to_value(params),
        EditRequest::SetLayerState(params) => serde_json::to_value(params),
        EditRequest::SetSelection(params) => serde_json::to_value(params),
        EditRequest::CreateObjectSection(params) => serde_json::to_value(params),
        EditRequest::DeleteObjectSection(params) => serde_json::to_value(params),
        EditRequest::MoveObjectSection(params) => serde_json::to_value(params),
        EditRequest::SetGridBpm(params) => serde_json::to_value(params),
        EditRequest::SetSceneSettings(params) => serde_json::to_value(params),
        EditRequest::ApplyBatch(params) => serde_json::to_value(params),
    };
    Ok(encoded.expect("params は直列化できる"))
}

/// JSON の中の全てのオブジェクト selector へ手を入れる。
///
/// 要求の形ごとに selector の位置を知らずに済むよう、木を辿って
/// `project_epoch` を持つオブジェクトを selector と見なす。
fn for_each_object_selector(
    value: &mut Value,
    apply: &impl Fn(&mut serde_json::Map<String, Value>),
) {
    match value {
        Value::Object(map) => {
            if map.contains_key("project_epoch") {
                apply(map);
            }
            for nested in map.values_mut() {
                for_each_object_selector(nested, apply);
            }
        }
        Value::Array(items) => {
            for item in items {
                for_each_object_selector(item, apply);
            }
        }
        _ => {}
    }
}

/// JSON の中の全ての effect selector へ手を入れる。
fn for_each_effect_selector(
    value: &mut Value,
    apply: &impl Fn(&mut serde_json::Map<String, Value>),
) {
    match value {
        Value::Object(map) => {
            if map.contains_key("effect_index") && map.contains_key("object") {
                apply(map);
            }
            for nested in map.values_mut() {
                for_each_effect_selector(nested, apply);
            }
        }
        Value::Array(items) => {
            for item in items {
                for_each_effect_selector(item, apply);
            }
        }
        _ => {}
    }
}

#[test]
fn every_edit_request_follows_the_selector_and_unknown_field_table() {
    // 1 つの operation で通しても、他が違う扱いなら気付けないため、全
    // operation を網羅 match から引いて同じ表に掛ける。
    for operation in EditOperation::ALL {
        let name = operation.as_str();
        let Some(current) = current_request(operation) else {
            continue;
        };
        decode_request(operation, &current)
            .unwrap_or_else(|error| panic!("{name} の現在の形が拒否されました: {error:?}"));

        // セレクターは算出方式を運ばないが、往復型なので名乗る指定も
        // 拒否せず、値を解釈せずに捨てる。
        let mut with_algorithm = current.clone();
        let insert = |map: &mut serde_json::Map<String, Value>| {
            map.insert("fingerprint_algorithm".to_string(), json!("sha256-raw-v1"));
        };
        for_each_object_selector(&mut with_algorithm, &insert);
        for_each_effect_selector(&mut with_algorithm, &insert);
        assert!(
            decode_request(operation, &with_algorithm).is_ok(),
            "{name} がセレクターの算出方式を拒否しました"
        );

        // 未知フィールドは拒否する。
        let mut unknown = current.clone();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("unknown_field".to_string(), json!(1));
        let error = decode_request(operation, &unknown)
            .expect_err(&format!("{name} が未知フィールドを受理しました"));
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{name}");

        // 入れ子の未知フィールドも拒否する。往復型は対象から外す。
        for key in current.as_object().expect("params は object").keys() {
            if is_round_trip_field(key) {
                continue;
            }
            let mut nested = current.clone();
            let Some(inner) = nested[key].as_object_mut() else {
                continue;
            };
            inner.insert("unknown_field".to_string(), json!(1));
            let error = decode_request(operation, &nested)
                .expect_err(&format!("{name} の {key} が未知フィールドを受理しました"));
            assert_eq!(error.code, ErrorCode::InvalidArgument, "{name}.{key}");
        }
    }
}

/// 応答が返した値をそのまま送り返す往復型のフィールドか。
///
/// 往復型は応答へ optional field が増えても往復が壊れないよう、未知
/// フィールドを拒否しない。
fn is_round_trip_field(key: &str) -> bool {
    matches!(key, "selector" | "object" | "value")
}

#[test]
fn only_the_requests_without_a_selector_require_an_expected_epoch() {
    // 前提の epoch を持つのは、対象を指すセレクターを持たない要求だけである。
    // 持つ要求ではその欠落が拒否になり、持たない要求ではフィールド自体が無い。
    let mut carriers = Vec::new();
    for operation in EditOperation::ALL {
        let Some(current) = current_request(operation) else {
            continue;
        };
        let mut without = current.clone();
        if without
            .as_object_mut()
            .unwrap()
            .remove("expected_project_epoch")
            .is_none()
        {
            continue;
        }
        carriers.push(operation.as_str());
        let error = decode_request(operation, &without).expect_err(&format!(
            "{} が前提の epoch なしで受理されました",
            operation.as_str()
        ));
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }

    assert_eq!(
        carriers,
        vec![
            EditOperation::CreateObject.as_str(),
            EditOperation::SetLayerState.as_str(),
            EditOperation::SetSelection.as_str(),
            EditOperation::SetGridBpm.as_str(),
            EditOperation::SetSceneSettings.as_str()
        ]
    );
}

#[test]
fn every_edit_operation_is_routed_from_its_name() {
    for operation in EditOperation::ALL {
        assert_eq!(
            classify_operation(operation.as_str()).unwrap(),
            Operation::Edit(operation),
            "{} が編集へ振り分けられていません",
            operation.as_str()
        );
    }
}

#[test]
fn only_names_outside_every_family_are_unsupported() {
    // 分類できる名前には必ず実行口がある。未対応として返るのは、どの族にも
    // 属さない名前だけである。
    for operation in EditOperation::ALL
        .map(KnownOperation::Edit)
        .into_iter()
        .chain(ReadOperation::ALL.map(KnownOperation::Read))
        .chain(RenderOperation::ALL.map(KnownOperation::Render))
    {
        assert!(
            classify_operation(operation.as_str()).is_ok(),
            "{} が未対応として返りました",
            operation.as_str()
        );
    }

    for name in ["apply_batches", "render_frames", "future_operation"] {
        let error = classify_operation(name).expect_err(&format!("{name} が受理されました"));
        assert_eq!(error.code, ErrorCode::UnsupportedOperation, "{name}");
        assert!(!error.retryable, "{name}");
    }
}

#[test]
fn the_request_table_leaves_out_no_operation() {
    // 網羅 match は operation の追加を止めるが、既存の枝を除外へ書き換えても
    // 止まらない。表から外れているものが 1 つも無いことを固定することで、
    // 除外を増やせばここが落ちる。
    let excluded: Vec<&str> = EditOperation::ALL
        .into_iter()
        .filter(|operation| current_request(*operation).is_none())
        .map(EditOperation::as_str)
        .collect();

    assert!(
        excluded.is_empty(),
        "要求の形の表から外れています: {excluded:?}"
    );
}

#[test]
fn params_are_decoded_before_the_lifecycle_state_is_checked() {
    // 起動処理中でも、要求内容の誤りは要求の誤りとして返す。状態由来の
    // 再試行可能なエラーで返すと、解消しない再試行を促してしまう。
    let adapter = FakeEditAdapter::new();
    let error = execute_edit(
        &adapter,
        &InstanceState::Starting,
        EditOperation::SetSelection,
        &json!({ "expected_scene_id": SCENE_ID }),
        within(),
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(adapter.calls().is_empty());
}

/// 期限内の判定。
fn within() -> RequestDeadline {
    RequestDeadline::Within(Instant::now() + Duration::from_secs(1))
}

/// 要求内容の誤りを、状態と期限のどちらへ先に掛けても崩れない組。
///
/// 受付判定を先に通すと、解消しない誤りが再試行を促す `host_busy` として
/// 返る。期限判定を先に通すと、同じ誤りが再試行可能な `timeout` に化ける。
/// **どちらの順序も塞ぐ。** 片方だけを見ていると、もう一方へ入れ替える
/// 変更が素通りする。
fn misordering_cases() -> [(InstanceState, RequestDeadline, &'static str); 2] {
    [
        (InstanceState::Starting, within(), "起動処理中"),
        (InstanceState::Ready, RequestDeadline::Exceeded, "期限超過"),
    ]
}

#[test]
fn invalid_edit_params_are_rejected_before_the_state_and_the_deadline() {
    // 全 operation を網羅 match の表から引く。一括適用も単一の編集も、
    // 要求内容の誤りに対する扱いは同じでなければならない。
    for (state, deadline, order) in misordering_cases() {
        for operation in EditOperation::ALL {
            let mut params = current_request(operation).expect("要求の形が表にありません");
            params
                .as_object_mut()
                .expect("params は object")
                .insert("unknown_field".to_string(), json!(1));

            let adapter = FakeEditAdapter::new();
            let error = execute_edit(&adapter, &state, operation, &params, deadline).unwrap_err();

            assert_eq!(
                error.code,
                ErrorCode::InvalidArgument,
                "{order} の {} が要求内容の誤りとして返りませんでした",
                operation.as_str()
            );
            assert!(
                adapter.calls().is_empty(),
                "{order} の {} が編集口へ届きました",
                operation.as_str()
            );
        }
    }
}

#[test]
fn section_zero_never_reaches_the_edit_adapter() {
    // 区間 0 の開始位置はオブジェクトの開始フレームであって中間点ではない。
    // 対象の状態に依らず常に誤りであるため、編集区間へ入る前に落ちる。
    for (operation, params) in [
        (
            EditOperation::DeleteObjectSection,
            json!({
                "selector": fake_summary().selector,
                "section": 0,
            }),
        ),
        (
            EditOperation::MoveObjectSection,
            json!({
                "selector": fake_summary().selector,
                "section": 0,
                "frame": 160,
            }),
        ),
    ] {
        let adapter = FakeEditAdapter::new();
        let error = execute_edit(
            &adapter,
            &InstanceState::Ready,
            operation,
            &params,
            within(),
        )
        .unwrap_err();

        let name = operation.as_str();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{name}");
        assert_eq!(
            error.details["reason"],
            json!("section_index_out_of_range"),
            "{name}"
        );
        assert!(adapter.calls().is_empty(), "{name} が編集口へ届きました");
    }
}

#[test]
fn a_section_index_of_one_reaches_the_edit_adapter() {
    // 区間の総数との比較は対象の現在の状態を要する。要求内容だけの検証は
    // そこまで見ず、1 以上はそのまま編集口へ届く。
    let adapter = FakeEditAdapter::new();
    execute_edit(
        &adapter,
        &InstanceState::Ready,
        EditOperation::DeleteObjectSection,
        &json!({
            "selector": fake_summary().selector,
            "section": 1,
        }),
        within(),
    )
    .expect("区間番号 1 が編集口へ届きませんでした");
    assert_eq!(adapter.calls(), vec!["delete_object_section"]);
}

#[test]
fn an_unrepresentable_effect_position_never_reaches_the_edit_adapter() {
    // 受け渡せない値は対象の状態に依らず常に誤りである。列の長さとの比較は
    // ここでは見ず、編集口が対象を解決してから行う。
    let adapter = FakeEditAdapter::new();
    let error = execute_edit(
        &adapter,
        &InstanceState::Ready,
        EditOperation::MoveEffect,
        &json!({
            "selector": fake_effect_selector(),
            "position": MAX_POSITION as u64 + 1,
        }),
        within(),
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(error.message.contains("position"), "{}", error.message);
    assert!(adapter.calls().is_empty(), "編集口へ届きました");
}

#[test]
fn a_move_effect_request_reaches_the_edit_adapter() {
    // 列の長さとの比較は対象の現在の状態を要する。要求内容だけの検証は
    // そこまで見ず、受け渡せる位置はそのまま編集口へ届く。
    let adapter = FakeEditAdapter::new();
    execute_edit(
        &adapter,
        &InstanceState::Ready,
        EditOperation::MoveEffect,
        &json!({ "selector": fake_effect_selector(), "position": 3 }),
        within(),
    )
    .expect("移動先 3 が編集口へ届きませんでした");
    assert_eq!(adapter.calls(), vec!["move_effect"]);
}

/// BPM 情報 1 件を要求の形で組み立てる。
fn grid_bpm_json(tempo: f64, beat: i64, start: f64) -> Value {
    json!({ "tempo": tempo, "beat": beat, "start": start, "offset": 0.0 })
}

/// BPM グリッドの置き換え要求を組み立てる。
fn set_grid_bpm_json(entries: Vec<Value>) -> Value {
    json!({
        "expected_scene_id": SCENE_ID,
        "entries": entries,
        "expected_project_epoch": EPOCH,
    })
}

#[test]
fn an_invalid_grid_bpm_list_never_reaches_the_edit_adapter() {
    // 検証は core の純関数にあり、要求の復号がそれを呼ぶ。呼ばなくなると
    // IPC を直接叩く経路が server と違う要求集合を受理するようになる。
    let over_the_limit = (0..=MAX_GRID_BPM_ENTRIES)
        .map(|index| grid_bpm_json(120.0, 4, index as f64))
        .collect::<Vec<_>>();
    for (label, entries, reason) in [
        ("上限超過", over_the_limit, Value::Null),
        (
            "start の重複",
            vec![grid_bpm_json(120.0, 4, 5.0), grid_bpm_json(90.0, 3, 5.0)],
            json!("duplicate_target"),
        ),
        (
            "範囲外の tempo",
            vec![grid_bpm_json(0.0, 4, 0.0)],
            json!("grid_bpm_out_of_range"),
        ),
        (
            "受け渡せない beat",
            vec![grid_bpm_json(120.0, i64::from(i32::MAX) + 1, 0.0)],
            json!("argument_not_representable"),
        ),
    ] {
        let adapter = FakeEditAdapter::new();
        let error = execute_edit(
            &adapter,
            &InstanceState::Ready,
            EditOperation::SetGridBpm,
            &set_grid_bpm_json(entries),
            within(),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidArgument, "{label}");
        assert_eq!(error.details["reason"], reason, "{label}");
        assert!(adapter.calls().is_empty(), "{label} が編集口へ届きました");
    }
}

#[test]
fn a_valid_grid_bpm_list_reaches_the_edit_adapter() {
    // 拒否だけを固定すると、全ての要求を拒む実装でも緑のまま通る。
    let adapter = FakeEditAdapter::new();
    execute_edit(
        &adapter,
        &InstanceState::Ready,
        EditOperation::SetGridBpm,
        &set_grid_bpm_json(vec![
            grid_bpm_json(120.0, 4, 30.0),
            grid_bpm_json(90.0, 3, 10.0),
        ]),
        within(),
    )
    .expect("正常な一覧が編集口へ届きませんでした");
    assert_eq!(adapter.calls(), vec!["set_grid_bpm"]);
}

#[test]
fn a_request_that_changes_nothing_is_an_invalid_argument() {
    let adapter = FakeEditAdapter::new();
    let error = execute_edit(
        &adapter,
        &InstanceState::Ready,
        EditOperation::SetSelection,
        &json!({
            "expected_scene_id": SCENE_ID,
            "expected_project_epoch": EPOCH,
        }),
        RequestDeadline::Within(Instant::now() + Duration::from_secs(1)),
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(adapter.calls().is_empty());
}

/// パスの構文検証が拒否する入力と、返るべき失敗の種別名。
///
/// 実機で観測した入力集合をそのまま用い、長さと NUL を足して 7 種すべてを
/// 覆う。どれも同じ `invalid_argument` で返るため、区別できるのは名前だけ
/// である。
fn rejected_path_cases() -> Vec<(String, &'static str, &'static str)> {
    vec![
        (String::new(), "空文字列", "empty_path"),
        ("C:\\movie\0.mp4".to_string(), "NUL", "contains_nul"),
        (
            format!("C:\\{}", "a".repeat(MAX_PATH_UTF16_UNITS)),
            "長さ超過",
            "path_too_long",
        ),
        (
            r"\\.\pipe\aviutl2".to_string(),
            "device namespace",
            "device_namespace",
        ),
        (
            r"\\?\C:\movie.mp4".to_string(),
            "device namespace の別表記",
            "device_namespace",
        ),
        (
            r"C:\movie.mp4:stream".to_string(),
            "代替データストリーム",
            "alternate_data_stream",
        ),
        (r"..\movie.mp4".to_string(), "相対パス", "not_absolute"),
        (
            r"\\server\share\movie.mp4".to_string(),
            "ネットワークパス",
            "unc_path",
        ),
        (
            "//server/share/movie.mp4".to_string(),
            "区切りを揃えたネットワークパス",
            "unc_path",
        ),
        (r"\\server\share".to_string(), "共有そのもの", "unc_path"),
    ]
}

#[test]
fn rejected_paths_never_reach_the_edit_section() {
    // パスの構文は要求元の側でも検証されるが、そこを通らない要求もある。
    // 実行側で弾けなければ、ネットワーク越しの接続や device namespace への
    // 到達をホストへ任せることになる。
    for (path, label, _) in rejected_path_cases() {
        let path = path.as_str();
        for (operation, params) in [
            (
                EditOperation::CreateObject,
                json!({
                    "source": { "type": "media_file", "path": path },
                    "placement": { "scene_id": SCENE_ID, "layer": 1, "frame": 0 },
                    "expected_project_epoch": EPOCH,
                }),
            ),
            (
                EditOperation::SetObjectItem,
                json!({
                    "selector": fake_effect_selector(),
                    "item": "ファイル",
                    "value": { "type": "file", "path": path },
                }),
            ),
        ] {
            let adapter = FakeEditAdapter::new();
            let error = execute_edit(
                &adapter,
                &InstanceState::Ready,
                operation,
                &params,
                RequestDeadline::Within(Instant::now() + Duration::from_secs(1)),
            )
            .unwrap_err();

            assert_eq!(
                error.code,
                ErrorCode::InvalidArgument,
                "{label} が {operation:?} で拒否されませんでした"
            );
            assert!(
                adapter.calls().is_empty(),
                "{label} が {operation:?} で編集口へ届きました"
            );
        }
    }
}

#[test]
fn rejected_paths_name_the_rule_they_broke() {
    // 7 種はいずれも invalid_argument で返る。名前が無ければ、要求元は
    // 「ローカルへ複製する」「絶対パスにする」「短い場所へ移す」のどれを
    // 取ればよいか説明の文面からしか読めない。
    //
    // メディアファイルからの作成と設定値の書き込みは別の検証を通るが、
    // 同じ入力には同じ名前が返る。
    for (path, label, reason) in rejected_path_cases() {
        let path = path.as_str();
        for (operation, params) in [
            (
                EditOperation::CreateObject,
                json!({
                    "source": { "type": "media_file", "path": path },
                    "placement": { "scene_id": SCENE_ID, "layer": 1, "frame": 0 },
                    "expected_project_epoch": EPOCH,
                }),
            ),
            (
                EditOperation::SetObjectItem,
                json!({
                    "selector": fake_effect_selector(),
                    "item": "ファイル",
                    "value": { "type": "file", "path": path },
                }),
            ),
        ] {
            let error = execute_edit(
                &FakeEditAdapter::new(),
                &InstanceState::Ready,
                operation,
                &params,
                within(),
            )
            .unwrap_err();

            assert_eq!(error.code, ErrorCode::InvalidArgument, "{label}");
            assert_eq!(
                error.details["reason"],
                json!(reason),
                "{label} が {operation:?} で名乗った種別が想定と異なります"
            );
            assert!(
                !error.details.to_string().contains("movie"),
                "{label} の補助情報にパスが現れました: {}",
                error.details
            );
        }
    }
}

#[test]
fn rejected_texts_name_the_rule_they_broke() {
    // 文字列の検証も同じである。空・NUL・制御文字・長さ超過はいずれも
    // invalid_argument であり、要求元が取れる行動だけが異なる。
    let item = |value: String| {
        (
            EditOperation::SetObjectItem,
            json!({
                "selector": fake_effect_selector(),
                "item": "文字",
                "value": { "type": "text", "value": value },
            }),
        )
    };
    let cases = [
        (
            "空文字列",
            "empty",
            (
                EditOperation::SetLayerState,
                json!({
                    "expected_scene_id": SCENE_ID,
                    "layer": 1,
                    "name": { "type": "set", "name": "" },
                    "expected_project_epoch": EPOCH,
                }),
            ),
        ),
        ("NUL", "contains_nul", item("あ\0い".to_string())),
        (
            "制御文字",
            "contains_control",
            item("あ\u{1}い".to_string()),
        ),
        (
            "長さ超過",
            "too_long",
            item("あ".repeat(MAX_ITEM_VALUE_BYTES)),
        ),
    ];
    for (label, reason, (operation, params)) in cases {
        let error = execute_edit(
            &FakeEditAdapter::new(),
            &InstanceState::Ready,
            operation,
            &params,
            within(),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidArgument, "{label}");
        assert_eq!(
            error.details["reason"],
            json!(reason),
            "{label} が名乗った種別が想定と異なります"
        );
        assert!(
            !error.details.to_string().contains('あ'),
            "{label} の補助情報に設定値が現れました: {}",
            error.details
        );
    }
}

#[test]
fn a_batch_gives_the_same_reason_as_the_same_edit_on_its_own() {
    // 一括適用は位置を添えるが、失敗の種別は単独編集と同じ名前で返る。
    // 経路ごとに違う名前を返せば、要求元は一括適用のためだけの分岐を持つ。
    for (path, label, reason) in rejected_path_cases() {
        let operation = json!({
            "type": "set_object_item",
            "selector": fake_effect_selector(),
            "item": "ファイル",
            "value": { "type": "file", "path": path.as_str() },
        });
        let error = execute_edit(
            &FakeEditAdapter::new(),
            &InstanceState::Ready,
            EditOperation::ApplyBatch,
            &json!({ "operations": [operation] }),
            within(),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidArgument, "{label}");
        assert_eq!(error.details["reason"], json!(reason), "{label}");
        assert_eq!(
            error.details["failed_index"],
            json!(0),
            "{label} が落ちた sub-operation の位置を運びませんでした"
        );
    }

    // フォルダも同じパス検証を通る。片方だけを固定すると、種別ごとに
    // 検証を書き分ける形へ戻っても気付けない。
    let error = execute_edit(
        &FakeEditAdapter::new(),
        &InstanceState::Ready,
        EditOperation::ApplyBatch,
        &json!({
            "operations": [{
                "type": "set_object_item",
                "selector": fake_effect_selector(),
                "item": "フォルダ",
                "value": { "type": "folder", "path": r"..\assets" },
            }],
        }),
        within(),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(error.details["reason"], json!("not_absolute"));
    assert_eq!(error.details["failed_index"], json!(0));
}

#[test]
fn both_duplicate_checks_name_the_same_fact() {
    // 同じ状態を書き換える組は 2 層で検出する。手前の層は要求内容だけを
    // 見て、奥の層は解決した結果を見る。**文字列として同一のセレクターを
    // 並べた要求——最も素直な入力——は手前の層で落ちる。** ここで名前が
    // 付かなければ、名前で分岐する要求元は稀な入力だけを拾う。
    let move_op = json!({
        "type": "move_object",
        "selector": fake_summary().selector,
        "destination": { "layer": 1, "frame": 300 },
    });
    let from_request = execute_edit(
        &FakeEditAdapter::new(),
        &InstanceState::Ready,
        EditOperation::ApplyBatch,
        &json!({ "operations": [move_op, move_op] }),
        within(),
    )
    .unwrap_err();

    // 奥の層が同じ事実を検出したときの応答。
    let after_resolution = edit_error(EditError::Batch {
        source: Box::new(EditError::DuplicateTarget),
        failed_index: Some(1),
        rollback: RollbackOutcome::NotAttempted,
    });

    assert_eq!(from_request.code, after_resolution.code);
    assert_eq!(from_request.details["reason"], json!("duplicate_target"));
    assert_eq!(
        from_request.details["reason"], after_resolution.details["reason"],
        "2 層の検査が同じ事実に別の名前を付けました"
    );
    assert_eq!(
        from_request.details["failed_index"],
        after_resolution.details["failed_index"]
    );
}

#[test]
fn a_batch_failure_of_the_request_as_a_whole_has_no_reason() {
    // 件数の誤りに対応する単独編集は無い。名前を持たない失敗へ名前を
    // 与えると、要求元は存在しない種別の分岐を書くことになる。
    let error = execute_edit(
        &FakeEditAdapter::new(),
        &InstanceState::Ready,
        EditOperation::ApplyBatch,
        &json!({ "operations": [] }),
        within(),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(error.details.get("reason"), None, "{:?}", error.details);
}

#[test]
fn a_starting_instance_rejects_a_well_formed_edit() {
    let adapter = FakeEditAdapter::new();
    let error = execute_edit(
        &adapter,
        &InstanceState::Starting,
        EditOperation::SetSelection,
        &selection_params(),
        RequestDeadline::Within(Instant::now() + Duration::from_secs(1)),
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::HostBusy);
    assert!(adapter.calls().is_empty());
}

#[test]
fn an_expired_deadline_stops_the_edit_before_it_starts() {
    let adapter = FakeEditAdapter::new();
    let error = execute_edit(
        &adapter,
        &InstanceState::Ready,
        EditOperation::SetSelection,
        &selection_params(),
        RequestDeadline::Exceeded,
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(error.retryable);
    let details = error.details;
    // 実行前に返す timeout だけが「変更は行われていない」と名乗れる。
    assert_eq!(details["change_applied"], json!("no"));
    assert_eq!(details["mutation_origin"], json!("plugin"));
    assert_eq!(details["retry_requires"], json!("resend"));
    assert!(adapter.calls().is_empty(), "SDK を呼ばずに中止していません");
}

#[test]
fn an_edit_within_the_deadline_reaches_the_adapter() {
    let adapter = FakeEditAdapter::new();
    let result = execute_edit(
        &adapter,
        &InstanceState::Ready,
        EditOperation::SetSelection,
        &selection_params(),
        RequestDeadline::Within(Instant::now() + Duration::from_secs(1)),
    )
    .expect("期限内の編集が拒否されました");

    assert_eq!(adapter.calls(), vec!["set_selection"]);
    assert_eq!(result["applied"], json!(["cursor"]));
}

#[test]
fn the_send_budget_is_never_shortened_by_the_request_deadline() {
    // 編集は結果を破棄しないため、送信には常に送信上限をそのまま充てる。
    // 期限際まで掛かった編集の送信に数ミリ秒しか残らないと、適用済みの
    // 変更が要求元からは無応答に見える。
    let now = Instant::now();
    assert_eq!(retained_send_deadline(now), now + write_timeout());

    // 読み取りは要求の残り時間で縮める。捨ててよい結果と捨ててはいけない
    // 結果の差がここに出る。
    assert_eq!(
        resolve_request_deadline(
            now,
            NOW_UNIX_MS,
            write_timeout(),
            Some((NOW_UNIX_MS + 200) as u64)
        ),
        RequestDeadline::Within(now + Duration::from_millis(200))
    );
}

#[test]
fn a_batch_is_given_its_own_execution_budget() {
    // 一括適用の費用は変更の件数だけでは決まらない。単一の編集と同じ上限に
    // 落ちると、事前解決相だけで尽きる要求が実行前の期限超過になる。
    assert_eq!(
        execution_timeout(Operation::Edit(EditOperation::ApplyBatch)),
        batch_timeout()
    );
    assert_ne!(batch_timeout(), edit_timeout());

    // 一括適用以外の編集は編集の上限のままである。
    for operation in EditOperation::ALL {
        if operation == EditOperation::ApplyBatch {
            continue;
        }
        assert_eq!(
            execution_timeout(Operation::Edit(operation)),
            edit_timeout(),
            "{} が編集の上限から外れました",
            operation.as_str()
        );
    }

    // 一括適用が実行の上限まで走っても、応答送信の持ち時間が一括適用の
    // 要求フェーズ予算の内側に残る。一括適用は結果を破棄しないため、
    // この余地が無いと応答を送り切れないまま接続が切れ得る。
    let batch = batch_timeout();
    let write = write_timeout();
    let server = ScaledBudgets::unscaled();
    let batch_request = server.server_request_phase(RequestBudgetKind::Batch);
    assert!(
        batch + write + server.transport_headroom() <= batch_request,
        "一括適用 {batch:?} と送信 {write:?} が要求フェーズ予算 {batch_request:?} に収まらない"
    );
}

#[test]
fn an_expired_deadline_stops_the_batch_before_it_starts() {
    let adapter = FakeEditAdapter::new();
    let error = execute_edit(
        &adapter,
        &InstanceState::Ready,
        EditOperation::ApplyBatch,
        &batch_params(),
        RequestDeadline::Exceeded,
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(error.retryable);
    // 実行前に返す timeout だけが「変更は行われていない」と名乗れる。
    assert_eq!(error.details["change_applied"], json!("no"));
    assert_eq!(error.details["retry_requires"], json!("resend"));
    assert!(adapter.calls().is_empty(), "SDK を呼ばずに中止していません");
}

#[test]
fn a_batch_within_the_deadline_reaches_the_adapter() {
    let adapter = FakeEditAdapter::new();
    let result = execute_edit(
        &adapter,
        &InstanceState::Ready,
        EditOperation::ApplyBatch,
        &batch_params(),
        RequestDeadline::Within(Instant::now() + Duration::from_secs(1)),
    )
    .expect("期限内の一括適用が拒否されました");

    assert_eq!(adapter.calls(), vec!["apply_batch"]);
    assert_eq!(result["project_epoch"], json!(EPOCH));
    assert_eq!(result["results"], json!([]));
}

#[test]
fn a_batch_result_is_never_discarded_after_its_deadline() {
    // 一括適用が期限を使い切っても結果は捨てない。捨てると、1 要求ぶんの
    // 変更がまとめて要求元からは無応答として観測される。
    let now = Instant::now();
    assert_eq!(retained_send_deadline(now), now + write_timeout());

    // 読み取りは同じ状況で結果を捨てる。捨ててよい結果と、捨ててはいけない
    // 結果の差がここに出る。
    assert_eq!(
        decide_send(
            now,
            NOW_UNIX_MS,
            RequestDeadline::Within(now - Duration::from_millis(1)),
            None,
        ),
        SendDecision::Discard
    );
}

#[test]
fn invalid_batch_params_are_rejected_before_the_lifecycle_state_is_checked() {
    // 起動処理中でも、要求内容の誤りは要求の誤りとして返す。
    let adapter = FakeEditAdapter::new();
    let error = execute_edit(
        &adapter,
        &InstanceState::Starting,
        EditOperation::ApplyBatch,
        &json!({ "operations": [] }),
        RequestDeadline::Within(Instant::now() + Duration::from_secs(1)),
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(adapter.calls().is_empty());
}

#[test]
fn batch_wide_rules_are_checked_before_the_batch_reaches_the_adapter() {
    // 件数・シーンの揃い・同じ状態を書き換える重複は、一括適用で初めて
    // 生じる要求内容の誤りである。実行口へ渡す前にここで落とす。
    let mut other_scene = fake_summary().selector;
    other_scene.scene_id = SCENE_ID + 1;
    let duplicate = json!({
        "type": "move_object",
        "selector": fake_summary().selector,
        "destination": { "layer": 1, "frame": 300 },
    });
    let cases = [
        ("件数 0", json!({ "operations": [] })),
        (
            "シーンの不揃い",
            json!({
                "operations": [
                    duplicate,
                    {
                        "type": "move_object",
                        "selector": other_scene,
                        "destination": { "layer": 1, "frame": 400 },
                    },
                ],
            }),
        ),
        (
            "同じ状態の重複",
            json!({ "operations": [duplicate, duplicate] }),
        ),
    ];

    // 一括適用で初めて生じる規則も、状態にも期限にも先んじて判定する。
    for (state, deadline, order) in misordering_cases() {
        for (label, params) in &cases {
            let adapter = FakeEditAdapter::new();
            let error = execute_edit(
                &adapter,
                &state,
                EditOperation::ApplyBatch,
                params,
                deadline,
            )
            .unwrap_err();

            assert_eq!(
                error.code,
                ErrorCode::InvalidArgument,
                "{order} の {label} が要求内容の誤りとして返りませんでした"
            );
            assert!(
                adapter.calls().is_empty(),
                "{order} の {label} が実行口へ届きました"
            );
        }
    }
}

#[test]
fn batch_validation_failures_name_the_operation_that_failed() {
    // 100 件までを 1 要求で運ぶ operation に対し、位置の分からない
    // invalid_argument は訂正の手掛かりとして足りない。**要求元がこの層へ
    // 届く前に同じ検証を通っているとは限らない。** 検証を備えた口を
    // 経由しない要求でも、位置は同じ形で返る。
    let mut other_scene = fake_summary().selector;
    other_scene.scene_id = SCENE_ID + 1;
    let move_op = json!({
        "type": "move_object",
        "selector": fake_summary().selector,
        "destination": { "layer": 1, "frame": 300 },
    });
    let located = [
        (
            "シーンの不揃い",
            1,
            json!({
                "operations": [
                    move_op,
                    {
                        "type": "move_object",
                        "selector": other_scene,
                        "destination": { "layer": 1, "frame": 400 },
                    },
                ],
            }),
        ),
        (
            "同じ状態の重複",
            1,
            json!({ "operations": [move_op, move_op] }),
        ),
        (
            "sub-operation の内容",
            2,
            json!({
                "operations": [
                    move_op,
                    {
                        "type": "move_object",
                        "selector": fake_summary().selector,
                        "destination": { "layer": 1, "frame": 500 },
                    },
                    {
                        "type": "set_object_item",
                        "selector": fake_effect_selector(),
                        "item": "ファイル",
                        "value": { "type": "file", "path": r"..\movie.mp4" },
                    },
                ],
            }),
        ),
    ];

    for (label, index, params) in located {
        let adapter = FakeEditAdapter::new();
        let error = execute_edit(
            &adapter,
            &InstanceState::Ready,
            EditOperation::ApplyBatch,
            &params,
            within(),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidArgument, "{label}");
        assert_eq!(
            error.details["failed_index"],
            json!(index),
            "{label} が落ちた sub-operation の位置を運びませんでした"
        );
        assert!(adapter.calls().is_empty(), "{label}");
    }
}

#[test]
fn a_batch_failure_without_a_position_does_not_name_one() {
    // 要求全体の誤りは特定の sub-operation に帰せられない。位置を添えると、
    // 要求元は 0 件目を直せば通ると読んでしまう。
    let too_many: Vec<Value> = (0..=aviutl2_mcp_core::MAX_BATCH_OPERATIONS)
        .map(|frame| {
            json!({
                "type": "move_object",
                "selector": fake_summary().selector,
                "destination": { "layer": 1, "frame": frame },
            })
        })
        .collect();
    for (label, params) in [
        ("件数 0", json!({ "operations": [] })),
        ("件数超過", json!({ "operations": too_many })),
    ] {
        let adapter = FakeEditAdapter::new();
        let error = execute_edit(
            &adapter,
            &InstanceState::Ready,
            EditOperation::ApplyBatch,
            &params,
            within(),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidArgument, "{label}");
        assert_eq!(
            error.details.get("failed_index"),
            None,
            "{label} が位置を持たない失敗に位置を添えました: {:?}",
            error.details
        );
        assert!(adapter.calls().is_empty(), "{label}");
    }
}

#[test]
fn batch_validation_failures_only_use_allowed_details_keys() {
    // 検証の失敗が返す補助情報も、実行の失敗と同じ許可キー一覧に従う。
    // 一覧に無いキーが出れば、要求元は解釈できない値を受け取る。
    const ALLOWED: &[&str] = &["failed_index", "reason"];

    let mut other_scene = fake_summary().selector;
    other_scene.scene_id = SCENE_ID + 1;
    let cases = [
        json!({ "operations": [] }),
        json!({
            "operations": [
                {
                    "type": "move_object",
                    "selector": fake_summary().selector,
                    "destination": { "layer": 1, "frame": 300 },
                },
                {
                    "type": "move_object",
                    "selector": other_scene,
                    "destination": { "layer": 1, "frame": 400 },
                },
            ],
        }),
        json!({
            "operations": [{
                "type": "set_object_item",
                "selector": fake_effect_selector(),
                "item": "ファイル",
                "value": { "type": "file", "path": r"\\.\pipe\aviutl2" },
            }],
        }),
    ];

    for params in cases {
        let adapter = FakeEditAdapter::new();
        let error = execute_edit(
            &adapter,
            &InstanceState::Ready,
            EditOperation::ApplyBatch,
            &params,
            within(),
        )
        .unwrap_err();

        for key in error.details.as_object().expect("補助情報は object").keys() {
            assert!(
                ALLOWED.contains(&key.as_str()),
                "検証の失敗の補助情報に未許可のキー {key} が含まれています"
            );
        }

        // 位置は整数だけであり、対象の内容を運ばない。設定値・alias・
        // パスそのものは説明にも補助情報にも現れない。
        let document = format!("{} {}", error.message, error.details);
        for forbidden in [r"\\.", "movie.mp4", "pipe", "[1:100]", "0x"] {
            assert!(
                !document.contains(forbidden),
                "{forbidden} が応答に含まれます: {document}"
            );
        }
    }
}

/// 期限判定の基準時刻。読み取り側のテストと同じ値を用いる。
const NOW_UNIX_MS: i64 = 1_785_144_000_000;
