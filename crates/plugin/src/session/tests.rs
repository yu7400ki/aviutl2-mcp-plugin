use super::*;
use crate::read::{Page, Snapshot};
use aviutl2_mcp_core::{
    AvailableEffect, Cursor, DescribeEffectsParams, DescribeEffectsResult, DisplayRange, EditInfo,
    EffectDescription, EffectFlags, EffectItemDescription, EffectItemType, EffectItemValues,
    EffectSelector, EffectType, EvaluatedItem, Extent, FiniteF64, FrameRange,
    GetEffectItemValuesParams, LayerInfo, ListAvailableEffectsResult, ListObjectAliasesResult,
    ListPalettesResult, MAX_DESCRIBED_EFFECTS, MAX_EVALUATED_FRAMES, MAX_EVALUATED_ITEMS,
    ModuleEntry, ModuleType, ObjectAliasSummary, ObjectDetail, ObjectFilter,
    ObjectFingerprintInput, ObjectSelector, ObjectSummary, PALETTE_COLOR_COUNT, PageWindow,
    PaletteEntry, RequestBudgetKind, Rgba, SceneInfo, SectionRange, SelectionSnapshot,
    ValidatedPageRequest, take_page, take_window,
};
use std::sync::Mutex;

/// 期限判定の基準時刻。壁時計・単調時計いずれの絶対値にも依存しない。
const NOW_UNIX_MS: i64 = 1_785_144_000_000;
const SERVER_LIMIT: Duration = Duration::from_secs(5);

#[test]
fn deadline_shorter_than_server_limit_is_adopted() {
    let now = Instant::now();
    assert_eq!(
        resolve_request_deadline(
            now,
            NOW_UNIX_MS,
            SERVER_LIMIT,
            Some((NOW_UNIX_MS + 500) as u64),
        ),
        RequestDeadline::Within(now + Duration::from_millis(500))
    );
}

#[test]
fn server_limit_is_adopted_when_deadline_is_longer() {
    let now = Instant::now();
    assert_eq!(
        resolve_request_deadline(
            now,
            NOW_UNIX_MS,
            SERVER_LIMIT,
            Some((NOW_UNIX_MS + 60_000) as u64),
        ),
        RequestDeadline::Within(now + SERVER_LIMIT)
    );
}

#[test]
fn absent_deadline_uses_server_limit() {
    let now = Instant::now();
    assert_eq!(
        resolve_request_deadline(now, NOW_UNIX_MS, SERVER_LIMIT, None),
        RequestDeadline::Within(now + SERVER_LIMIT)
    );
}

#[test]
fn passed_deadline_is_exceeded() {
    let now = Instant::now();
    for deadline_unix_ms in [NOW_UNIX_MS - 1, NOW_UNIX_MS] {
        assert_eq!(
            resolve_request_deadline(
                now,
                NOW_UNIX_MS,
                SERVER_LIMIT,
                Some(deadline_unix_ms as u64),
            ),
            RequestDeadline::Exceeded,
            "deadline {deadline_unix_ms} が期限超過として扱われていません"
        );
    }
}

#[test]
fn far_past_deadline_is_exceeded() {
    let now = Instant::now();
    assert_eq!(
        resolve_request_deadline(now, NOW_UNIX_MS, SERVER_LIMIT, Some(0)),
        RequestDeadline::Exceeded
    );
}

#[test]
fn far_future_deadline_is_capped_by_server_limit() {
    let now = Instant::now();
    assert_eq!(
        resolve_request_deadline(now, NOW_UNIX_MS, SERVER_LIMIT, Some(u64::MAX)),
        RequestDeadline::Within(now + SERVER_LIMIT)
    );
}

/// テストで用いるプロジェクトの epoch。
const EPOCH: &str = "9d0a5f4e-2f47-4a13-9a5e-1e2f3a4b5c6d";

/// テストで用いる現在シーンの ID。
const SCENE_ID: i32 = 0;

/// 読み取り口が返す列挙時点の revision。
const REVISION: u64 = 7;

/// 読み取り口の代わりに定型データを返す実装。
///
/// 呼ばれた operation を記録するため、受付判定や params の検証で弾かれた
/// 要求が読み取りへ進んでいないことを確かめられる。
struct FakeAdapter {
    calls: Mutex<Vec<&'static str>>,
    /// 最初の呼び出しで返す失敗。
    failure: Mutex<Option<ReadError>>,
}

impl FakeAdapter {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            failure: Mutex::new(None),
        }
    }

    /// 最初の読み取りが指定の失敗を返す読み取り口を作る。
    fn failing(error: ReadError) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            failure: Mutex::new(Some(error)),
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    /// 呼び出しを記録し、設定された失敗があればそれを返す。
    fn enter(&self, call: &'static str) -> Result<(), ReadError> {
        self.calls.lock().unwrap().push(call);
        match self.failure.lock().unwrap().take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl ReadAdapter for FakeAdapter {
    fn project_status(&self) -> crate::read::ProjectStatus {
        self.calls.lock().unwrap().push("project_status");
        crate::read::ProjectStatus {
            epoch: EPOCH.to_string(),
            revision: REVISION,
            modified: true,
        }
    }

    fn get_edit_info(&self) -> Result<EditInfo, ReadError> {
        self.enter("get_edit_info")?;
        Ok(fake_edit_info())
    }

    fn get_current_scene(&self) -> Result<(SceneInfo, u64), ReadError> {
        self.enter("get_current_scene")?;
        Ok((fake_scene(), REVISION))
    }

    fn list_layers(&self, expected_scene_id: i32) -> Result<Snapshot<LayerInfo>, ReadError> {
        self.enter("list_layers")?;
        ensure_scene(expected_scene_id)?;
        Ok(Snapshot {
            items: fake_layers(),
            snapshot_revision: REVISION,
        })
    }

    fn list_objects(
        &self,
        expected_scene_id: i32,
        filter: Option<&ObjectFilter>,
        page: &ValidatedPageRequest,
    ) -> Result<Result<Page<ObjectSummary>, SnapshotRevisionMismatch>, ReadError> {
        self.enter("list_objects")?;
        ensure_scene(expected_scene_id)?;
        let layer_min = filter.and_then(|filter| filter.layer_min).unwrap_or(0);
        let items: Vec<ObjectSummary> = fake_objects()
            .into_iter()
            .filter(|object| object.layer >= layer_min)
            .collect();
        Ok(take_page(&items, page, REVISION).map(|(items, meta)| Page { items, meta }))
    }

    fn get_object(&self, selector: &ObjectSelector) -> Result<ObjectDetail, ReadError> {
        self.enter("get_object")?;
        let summary = fake_object();
        if *selector != summary.selector {
            return Err(ReadError::ObjectNotFound {
                detected_by: "find_object",
            });
        }
        Ok(ObjectDetail {
            alias: "[1:100]".to_string(),
            sections: vec![SectionRange {
                start: 100,
                end: 200,
            }],
            effects: Vec::new(),
            project_revision: REVISION,
            summary,
        })
    }

    fn list_available_effects(
        &self,
        effect_type: Option<&EffectType>,
        page: &PageWindow,
    ) -> Result<ListAvailableEffectsResult, ReadError> {
        self.enter("list_available_effects")?;
        let mut effects = fake_effects();
        if let Some(effect_type) = effect_type {
            effects.retain(|effect| effect.effect_type == *effect_type);
        }
        let (items, page) = take_window(&effects, page, REVISION);
        Ok(ListAvailableEffectsResult { items, page })
    }

    fn describe_effects(
        &self,
        params: &DescribeEffectsParams,
    ) -> Result<DescribeEffectsResult, ReadError> {
        self.enter("describe_effects")?;
        let catalog = fake_effects();
        let mut effects = Vec::new();
        let mut not_found = Vec::new();
        for name in &params.effect_names {
            match catalog.iter().find(|effect| effect.name == *name) {
                Some(effect) => effects.push(fake_effect_description(effect)),
                None => not_found.push(name.clone()),
            }
        }
        Ok(DescribeEffectsResult { effects, not_found })
    }

    fn list_fonts(&self) -> Result<Snapshot<String>, ReadError> {
        self.enter("list_fonts")?;
        Ok(Snapshot {
            items: fake_fonts(),
            snapshot_revision: REVISION,
        })
    }

    fn list_palettes(&self, page: &PageWindow) -> Result<ListPalettesResult, ReadError> {
        self.enter("list_palettes")?;
        let names = fake_palette_names();
        let (window, meta) = take_window(&names, page, REVISION);
        Ok(ListPalettesResult {
            current: Some("[標準.既定]".to_string()),
            items: window
                .into_iter()
                .map(|name| PaletteEntry {
                    name,
                    colors: vec![
                        Rgba {
                            r: 0,
                            g: 0,
                            b: 0,
                            a: 255
                        };
                        PALETTE_COLOR_COUNT
                    ],
                })
                .collect(),
            page: meta,
        })
    }

    fn list_modules(
        &self,
        module_type: Option<&ModuleType>,
    ) -> Result<Snapshot<ModuleEntry>, ReadError> {
        self.enter("list_modules")?;
        let mut items = fake_modules();
        if let Some(module_type) = module_type {
            items.retain(|module| module.module_type == *module_type);
        }
        Ok(Snapshot {
            items,
            snapshot_revision: REVISION,
        })
    }

    fn list_object_aliases(
        &self,
        label: Option<&str>,
        page: &PageWindow,
    ) -> Result<ListObjectAliasesResult, ReadError> {
        self.enter("list_object_aliases")?;
        let mut items = fake_object_aliases();
        if let Some(label) = label {
            items.retain(|item| item.label.as_deref() == Some(label));
        }
        // 呼ぶたびに revision が進む。照合を外したことが実際に効いているか
        // は、進んだ後の 2 ページ目が通るかでしか見えない。
        let calls = self
            .calls()
            .iter()
            .filter(|call| **call == "list_object_aliases")
            .count() as u64;
        let revision = REVISION + calls - 1;
        let (items, page) = take_window(&items, page, revision);
        Ok(ListObjectAliasesResult { items, page })
    }

    fn get_effect_item_values(
        &self,
        params: &GetEffectItemValuesParams,
    ) -> Result<EffectItemValues, ReadError> {
        self.enter("get_effect_item_values")?;
        Ok(EffectItemValues {
            project_revision: REVISION,
            frames: params.frames.clone(),
            items: vec![EvaluatedItem::Track {
                name: "X".to_string(),
                values: params.frames.clone(),
                group: None,
            }],
            truncated: false,
        })
    }

    fn get_selection(
        &self,
        expected_scene_id: i32,
        page: &ValidatedPageRequest,
    ) -> Result<Result<SelectionSnapshot, SnapshotRevisionMismatch>, ReadError> {
        self.enter("get_selection")?;
        ensure_scene(expected_scene_id)?;
        let items = fake_objects();
        Ok(
            take_page(&items, page, REVISION).map(|(selected, meta)| SelectionSnapshot {
                project_revision: REVISION,
                focus: Some(fake_object()),
                focus_section: Some(1),
                selected,
                page: meta,
            }),
        )
    }
}

/// レイヤー 1・フレーム 100 のオブジェクトが持つ effect を指すセレクター。
fn fake_effect_selector() -> EffectSelector {
    let object = fake_object().selector;
    EffectSelector {
        fingerprint: object.fingerprint.clone(),
        object,
        effect_name: "動画ファイル".to_string(),
        effect_index: 0,
    }
}

fn ensure_scene(expected_scene_id: i32) -> Result<(), ReadError> {
    if expected_scene_id == SCENE_ID {
        Ok(())
    } else {
        Err(ReadError::SceneMismatch {
            expected: expected_scene_id,
            current: SCENE_ID,
        })
    }
}

fn fake_scene() -> SceneInfo {
    SceneInfo {
        id: SCENE_ID,
        name: Some("Scene 1".to_string()),
        width: 1920,
        height: 1080,
        fps: FiniteF64::try_new(60.0),
        fps_rate: 60,
        fps_scale: 1,
        sample_rate: 48000,
    }
}

fn fake_edit_info() -> EditInfo {
    EditInfo {
        scene: fake_scene(),
        cursor: Cursor {
            frame: 12,
            layer: 1,
        },
        extent: Extent {
            frame_max: 3600,
            layer_max: 2,
        },
        display: DisplayRange {
            frame_start: 0,
            layer_start: 0,
            frame_num: 600,
            layer_num: 10,
        },
        selected_range: Some(FrameRange { start: 10, end: 20 }),
        grid_bpm: Vec::new(),
        project_epoch: EPOCH.to_string(),
        project_revision: REVISION,
    }
}

fn fake_layers() -> Vec<LayerInfo> {
    (0..3)
        .map(|index| LayerInfo {
            index,
            name: Some(format!("レイヤー {index}")),
            enabled: true,
            locked: false,
            object_count: 1,
        })
        .collect()
}

/// レイヤー 1・フレーム 100 のオブジェクト。
fn fake_object() -> ObjectSummary {
    ObjectSummary::new(
        EPOCH,
        ObjectFingerprintInput {
            scene_id: SCENE_ID,
            layer: 1,
            frame_start: 100,
            frame_end: 200,
            name: Some("立ち絵"),
            alias: "[1:100]",
        },
    )
}

fn fake_objects() -> Vec<ObjectSummary> {
    vec![
        ObjectSummary::new(
            EPOCH,
            ObjectFingerprintInput {
                scene_id: SCENE_ID,
                layer: 0,
                frame_start: 0,
                frame_end: 99,
                name: None,
                alias: "[0:0]",
            },
        ),
        fake_object(),
    ]
}

fn fake_effects() -> Vec<AvailableEffect> {
    vec![
        AvailableEffect {
            name: "ぼかし".to_string(),
            effect_type: EffectType::Filter,
            flags: EffectFlags::from_raw(1),
            item_count: 1,
            description: None,
        },
        AvailableEffect {
            name: "動画ファイル".to_string(),
            effect_type: EffectType::Input,
            flags: EffectFlags::from_raw(3),
            item_count: 0,
            description: None,
        },
    ]
}

/// 一覧の見出しから、中身を引いたときの応答を組み立てる。
///
/// 項目の名前を effect 名から導いてあり、別の effect の項目を返した実装は
/// 結果に現れる。説明を持つのは effect 名が「ぼかし」の場合だけであり、
/// 説明を持つ effect と持たない effect を同じ要求へ並べられる。
fn fake_effect_description(effect: &AvailableEffect) -> EffectDescription {
    let described = effect.name == "ぼかし";
    EffectDescription {
        name: effect.name.clone(),
        description: described.then(|| format!("{} の説明", effect.name)),
        items: (0..effect.item_count)
            .map(|index| EffectItemDescription {
                name: format!("{}の項目{index}", effect.name),
                item_type: EffectItemType::Integer,
                description: described.then(|| format!("{}の項目{index} の説明", effect.name)),
                choices: None,
                range: None,
                group: None,
            })
            .collect(),
    }
}

fn fake_fonts() -> Vec<String> {
    vec![
        "MS UI Gothic".to_string(),
        "游ゴシック".to_string(),
        "Segoe UI".to_string(),
    ]
}

fn fake_palette_names() -> Vec<String> {
    vec!["既定".to_string(), "暖色".to_string(), "寒色".to_string()]
}

fn fake_object_aliases() -> Vec<ObjectAliasSummary> {
    vec![
        ObjectAliasSummary {
            name: "テロップ".to_string(),
            label: Some("テロップ集".to_string()),
            object_count: Some(1),
            effects: vec!["テキスト".to_string(), "標準描画".to_string()],
        },
        ObjectAliasSummary {
            name: "背景".to_string(),
            label: None,
            object_count: Some(2),
            effects: vec!["図形".to_string()],
        },
    ]
}

fn fake_modules() -> Vec<ModuleEntry> {
    vec![
        ModuleEntry {
            module_type: ModuleType::ScriptObject,
            name: "テキスト".to_string(),
            information: "標準搭載".to_string(),
        },
        ModuleEntry {
            module_type: ModuleType::PluginInput,
            name: "入力プラグイン".to_string(),
            information: "動画の読み込み".to_string(),
        },
    ]
}

/// 受付可能な状態・期限内で読み取りを実行する。
fn read(
    adapter: &FakeAdapter,
    operation: ReadOperation,
    params: Value,
) -> Result<Value, ErrorObject> {
    execute_read(
        adapter,
        &InstanceState::Ready,
        operation,
        &params,
        RequestDeadline::Within(Instant::now() + read_timeout()),
    )
}

/// 全 operation と、その operation が受け付ける最小の params。
fn all_operations() -> Vec<(ReadOperation, Value)> {
    vec![
        (ReadOperation::GetEditInfo, json!({})),
        (ReadOperation::GetCurrentScene, json!({})),
        (
            ReadOperation::ListLayers,
            json!({ "expected_scene_id": SCENE_ID }),
        ),
        (
            ReadOperation::ListObjects,
            json!({ "expected_scene_id": SCENE_ID }),
        ),
        (
            ReadOperation::GetObject,
            json!({ "selector": fake_object().selector }),
        ),
        (ReadOperation::ListAvailableEffects, json!({})),
        (
            ReadOperation::DescribeEffects,
            json!({ "effect_names": ["ぼかし"] }),
        ),
        (
            ReadOperation::GetEffectItemValues,
            json!({ "effect": fake_effect_selector(), "frames": [100.0] }),
        ),
        (
            ReadOperation::GetSelection,
            json!({ "expected_scene_id": SCENE_ID }),
        ),
        (ReadOperation::ListFonts, json!({})),
        (ReadOperation::ListPalettes, json!({})),
        (ReadOperation::ListModules, json!({})),
        (ReadOperation::ListObjectAliases, json!({})),
    ]
}

/// [`all_operations`] が全 read operation を含むことを固定する。
///
/// 表は手書きであり、載せ忘れた operation は表を使う検査を全て素通りする。
/// 応答の秘匿・期限超過・状態の検査はいずれもこの表を材料にしている。
#[test]
fn all_operations_covers_every_read_operation() {
    let covered: std::collections::BTreeSet<&str> = all_operations()
        .iter()
        .map(|(operation, _)| operation.as_str())
        .collect();
    let expected: std::collections::BTreeSet<&str> = ReadOperation::ALL
        .iter()
        .map(|operation| operation.as_str())
        .collect();
    assert_eq!(covered, expected);
}

#[test]
fn known_operations_are_routed() {
    assert_eq!(classify_operation("ping").unwrap(), Operation::Ping);
    for (name, operation) in [
        ("get_edit_info", ReadOperation::GetEditInfo),
        ("get_current_scene", ReadOperation::GetCurrentScene),
        ("list_layers", ReadOperation::ListLayers),
        ("list_objects", ReadOperation::ListObjects),
        ("get_object", ReadOperation::GetObject),
        (
            "list_available_effects",
            ReadOperation::ListAvailableEffects,
        ),
        ("describe_effects", ReadOperation::DescribeEffects),
        ("get_effect_item_values", ReadOperation::GetEffectItemValues),
        ("get_selection", ReadOperation::GetSelection),
        ("list_fonts", ReadOperation::ListFonts),
        ("list_palettes", ReadOperation::ListPalettes),
        ("list_modules", ReadOperation::ListModules),
        ("list_object_aliases", ReadOperation::ListObjectAliases),
    ] {
        assert_eq!(
            classify_operation(name).unwrap(),
            Operation::Read(operation),
            "{name} が読み取りへ振り分けられていません"
        );
    }
}

#[test]
fn unknown_operation_is_unsupported() {
    for name in ["", "Ping", "future_operation", "list_layer"] {
        let error = classify_operation(name).unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::UnsupportedOperation,
            "{name} が受理されました"
        );
        assert!(!error.retryable);
    }
}

#[test]
fn get_edit_info_returns_edit_info() {
    let adapter = FakeAdapter::new();
    let result = read(&adapter, ReadOperation::GetEditInfo, json!({})).unwrap();

    assert_eq!(result["scene"]["id"], SCENE_ID);
    assert_eq!(result["scene"]["name"], "Scene 1");
    assert_eq!(result["project_epoch"], EPOCH);
    assert_eq!(result["project_revision"], REVISION);
    assert_eq!(adapter.calls(), vec!["get_edit_info"]);
}

#[test]
fn get_current_scene_returns_scene_and_revision() {
    let adapter = FakeAdapter::new();
    let result = read(&adapter, ReadOperation::GetCurrentScene, json!({})).unwrap();

    assert_eq!(result["scene"]["id"], SCENE_ID);
    assert_eq!(result["project_revision"], REVISION);
    assert_eq!(adapter.calls(), vec!["get_current_scene"]);
}

#[test]
fn list_layers_returns_requested_page() {
    let adapter = FakeAdapter::new();
    let result = read(
        &adapter,
        ReadOperation::ListLayers,
        json!({ "expected_scene_id": SCENE_ID, "offset": 1, "limit": 1 }),
    )
    .unwrap();

    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["index"], 1);
    assert_eq!(result["page"]["total_count"], 3);
    assert_eq!(result["page"]["count"], 1);
    assert_eq!(result["page"]["offset"], 1);
    assert_eq!(result["page"]["has_more"], true);
    assert_eq!(result["page"]["next_offset"], 2);
    assert_eq!(result["page"]["snapshot_revision"], REVISION);
}

#[test]
fn list_objects_passes_filter_to_the_adapter() {
    let adapter = FakeAdapter::new();
    let result = read(
        &adapter,
        ReadOperation::ListObjects,
        json!({ "expected_scene_id": SCENE_ID, "filter": { "layer_min": 1 } }),
    )
    .unwrap();

    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["layer"], 1);
    assert_eq!(result["page"]["total_count"], 1);
    assert_eq!(result["page"]["snapshot_revision"], REVISION);
}

#[test]
fn get_object_passes_selector_to_the_adapter() {
    let adapter = FakeAdapter::new();
    let selector = fake_object().selector;
    let result = read(
        &adapter,
        ReadOperation::GetObject,
        json!({ "selector": selector }),
    )
    .unwrap();

    assert_eq!(result["summary"]["layer"], 1);
    assert_eq!(result["summary"]["frame_start"], 100);
    assert_eq!(result["summary"]["selector"], json!(selector));
    assert_eq!(result["project_revision"], REVISION);
}

#[test]
fn list_available_effects_filters_by_type() {
    let adapter = FakeAdapter::new();
    let result = read(
        &adapter,
        ReadOperation::ListAvailableEffects,
        json!({ "effect_type": "input" }),
    )
    .unwrap();

    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["name"], "動画ファイル");
    assert_eq!(result["page"]["total_count"], 1);
}

#[test]
fn describe_effects_returns_the_named_effects_and_what_it_could_not_find() {
    // 見つかった分と見つからなかった名前が同じ応答に並ぶ。落とした名前が
    // 応答に無ければ、要求元は「設定項目を持たない effect」と誤読する。
    let adapter = FakeAdapter::new();
    let result = read(
        &adapter,
        ReadOperation::DescribeEffects,
        json!({ "effect_names": ["ぼかし", "存在しない効果", "動画ファイル"] }),
    )
    .unwrap();

    assert_eq!(
        result["effects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|effect| effect["name"].as_str().unwrap())
            .collect::<Vec<&str>>(),
        vec!["ぼかし", "動画ファイル"]
    );
    assert_eq!(result["not_found"], json!(["存在しない効果"]));
    // 説明を持つ effect と持たない effect が混ざる。持たない側は null に
    // なり、推測で埋まらない。
    assert_eq!(result["effects"][0]["description"], "ぼかし の説明");
    assert_eq!(result["effects"][0]["items"][0]["name"], "ぼかしの項目0");
    assert_eq!(
        result["effects"][0]["items"][0]["description"],
        "ぼかしの項目0 の説明"
    );
    assert!(result["effects"][1]["description"].is_null());
    assert_eq!(result["effects"][1]["items"], json!([]));
    assert_eq!(adapter.calls(), vec!["describe_effects"]);
}

#[test]
fn describe_effects_does_not_take_a_page() {
    // ページの続きという概念が無い。他の一覧を真似た指定は受理しない。
    for field in ["offset", "limit", "snapshot_revision"] {
        let adapter = FakeAdapter::new();
        let mut params = json!({ "effect_names": ["ぼかし"] });
        params
            .as_object_mut()
            .unwrap()
            .insert(field.to_string(), json!(1));

        let error = read(&adapter, ReadOperation::DescribeEffects, params).unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::InvalidArgument,
            "{field} が受理されました"
        );
        assert!(adapter.calls().is_empty(), "{field}");
    }
}

#[test]
fn describe_effects_rejects_a_request_over_the_limit_without_reading() {
    // 上限の判定は要求内容だけで決まる。読み取りへ進む前に落とす。
    let names: Vec<String> = (0..=MAX_DESCRIBED_EFFECTS)
        .map(|index| format!("効果{index}"))
        .collect();
    let adapter = FakeAdapter::new();

    let error = read(
        &adapter,
        ReadOperation::DescribeEffects,
        json!({ "effect_names": names }),
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(
        error.details["reason"],
        json!("effect_count_out_of_range"),
        "落ちた規則の名前がありません: {error:?}"
    );
    assert!(
        adapter.calls().is_empty(),
        "上限を超えた要求のまま読み取りへ進みました"
    );
}

#[test]
fn describe_effects_rejects_an_empty_or_repeated_request_without_reading() {
    for (params, reason) in [
        (json!({ "effect_names": [] }), "effect_count_out_of_range"),
        (
            json!({ "effect_names": ["ぼかし", "ぼかし"] }),
            "duplicate_effect_name",
        ),
    ] {
        let adapter = FakeAdapter::new();
        let error = read(&adapter, ReadOperation::DescribeEffects, params.clone()).unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidArgument, "{params}");
        assert_eq!(error.details["reason"], json!(reason), "{params}");
        assert!(adapter.calls().is_empty(), "{params}");
    }
}

#[test]
fn unknown_params_field_is_invalid_argument() {
    for (operation, params) in all_operations() {
        let mut params = params;
        params
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(1));
        let adapter = FakeAdapter::new();

        let error = read(&adapter, operation, params).unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::InvalidArgument,
            "{operation:?} が未知フィールドを受理しました"
        );
        assert!(
            adapter.calls().is_empty(),
            "{operation:?} が未知フィールドのまま読み取りへ進みました"
        );
    }
}

#[test]
fn effect_item_values_bound_the_frame_and_item_counts_before_reading() {
    // 件数は要求内容だけで決まる。読み取りへ進む前に落とす。
    let selector = fake_effect_selector();
    let over_frames: Vec<f64> = (0..=MAX_EVALUATED_FRAMES)
        .map(|index| index as f64)
        .collect();
    let over_items: Vec<String> = (0..=MAX_EVALUATED_ITEMS)
        .map(|index| format!("項目{index}"))
        .collect();
    for params in [
        json!({ "effect": selector, "frames": [] }),
        json!({ "effect": selector, "frames": over_frames }),
        json!({ "effect": selector, "frames": [100.0], "items": [] }),
        json!({ "effect": selector, "frames": [100.0], "items": over_items }),
    ] {
        let adapter = FakeAdapter::new();
        let error = read(&adapter, ReadOperation::GetEffectItemValues, params.clone()).unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::InvalidArgument,
            "{params} が受理されました"
        );
        assert!(
            adapter.calls().is_empty(),
            "{params} が読み取りへ進みました"
        );
    }
}

#[test]
fn effect_item_values_accept_the_counts_at_the_bounds() {
    let selector = fake_effect_selector();
    let frames: Vec<f64> = (0..MAX_EVALUATED_FRAMES)
        .map(|index| index as f64)
        .collect();
    let items: Vec<String> = (0..MAX_EVALUATED_ITEMS)
        .map(|index| format!("項目{index}"))
        .collect();
    let adapter = FakeAdapter::new();
    let result = read(
        &adapter,
        ReadOperation::GetEffectItemValues,
        json!({ "effect": selector, "frames": frames, "items": items }),
    )
    .expect("上限ちょうどが拒否されました");
    assert_eq!(
        result["frames"].as_array().unwrap().len(),
        MAX_EVALUATED_FRAMES
    );
    assert_eq!(adapter.calls(), vec!["get_effect_item_values"]);
}

#[test]
fn effect_item_values_reject_duplicates_before_reading() {
    // 重複も要求内容だけで決まる。同じ値を 2 度評価させず、応答の件数が
    // 要求の件数と対応したままになる。
    let selector = fake_effect_selector();
    for params in [
        json!({ "effect": selector, "frames": [100.0, 100.0] }),
        json!({ "effect": selector, "frames": [100.0], "items": ["範囲", "範囲"] }),
    ] {
        let adapter = FakeAdapter::new();
        let error = read(&adapter, ReadOperation::GetEffectItemValues, params.clone()).unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::InvalidArgument,
            "{params} が受理されました"
        );
        assert!(
            adapter.calls().is_empty(),
            "{params} が読み取りへ進みました"
        );
    }
}

#[test]
fn the_catalog_payloads_carry_no_handle_or_alias() {
    // 登録物の名前と属性しか載せない。対象を指す内部の値は現れない。
    for operation in CATALOG_OPERATIONS {
        let adapter = FakeAdapter::new();
        let payload = read(&adapter, operation, json!({}))
            .unwrap_or_else(|error| panic!("{operation:?}: {error:?}"))
            .to_string();
        for forbidden in ["alias", "handle", "selector", "0x"] {
            assert!(
                !payload.contains(forbidden),
                "{operation:?} の IPC 応答へ {forbidden} が現れました: {payload}"
            );
        }
    }
}

#[test]
fn malformed_params_are_invalid_argument() {
    let cases = [
        (ReadOperation::ListLayers, json!({})),
        (
            ReadOperation::ListLayers,
            json!({ "expected_scene_id": "0" }),
        ),
        (
            ReadOperation::ListObjects,
            json!({ "expected_scene_id": SCENE_ID, "filter": { "layer_min": -1 } }),
        ),
        (ReadOperation::GetObject, json!({})),
        (
            ReadOperation::ListAvailableEffects,
            json!({ "effect_type": 1 }),
        ),
    ];

    for (operation, params) in cases {
        let adapter = FakeAdapter::new();
        let error = read(&adapter, operation, params.clone()).unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::InvalidArgument,
            "{operation:?} が {params} を受理しました"
        );
        assert!(adapter.calls().is_empty(), "{operation:?}: {params}");
    }
}

#[test]
fn limit_out_of_range_is_invalid_argument_without_reading() {
    let paged = [
        (
            ReadOperation::ListLayers,
            json!({ "expected_scene_id": SCENE_ID }),
        ),
        (
            ReadOperation::ListObjects,
            json!({ "expected_scene_id": SCENE_ID }),
        ),
        (ReadOperation::ListAvailableEffects, json!({})),
        (
            ReadOperation::GetSelection,
            json!({ "expected_scene_id": SCENE_ID }),
        ),
        (ReadOperation::ListFonts, json!({})),
        (ReadOperation::ListPalettes, json!({})),
        (ReadOperation::ListModules, json!({})),
        (ReadOperation::ListObjectAliases, json!({})),
    ];

    for (operation, params) in paged {
        for limit in [0, 201] {
            let mut params = params.clone();
            params
                .as_object_mut()
                .unwrap()
                .insert("limit".to_string(), json!(limit));
            let adapter = FakeAdapter::new();

            let error = read(&adapter, operation, params).unwrap_err();
            assert_eq!(
                error.code,
                ErrorCode::InvalidArgument,
                "{operation:?} が limit {limit} を受理しました"
            );
            assert!(
                adapter.calls().is_empty(),
                "{operation:?} が limit {limit} のまま読み取りへ進みました"
            );
        }
    }
}

#[test]
fn inverted_layer_filter_is_invalid_argument_without_reading() {
    let adapter = FakeAdapter::new();
    let error = read(
        &adapter,
        ReadOperation::ListObjects,
        json!({
            "expected_scene_id": SCENE_ID,
            "filter": { "layer_min": 2, "layer_max": 1 },
        }),
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(
        adapter.calls().is_empty(),
        "逆転した絞り込み条件のまま読み取りへ進みました"
    );
}

#[test]
fn snapshot_revision_mismatch_is_precondition_failed() {
    // 現在の revision から離れた値を送る。要求元が前ページの値を送り返す
    // 経路と、まったく身に覚えの無い値を送る経路の双方が同じ失敗になる。
    const STALE: u64 = 999;

    let paged = [
        (
            ReadOperation::ListLayers,
            json!({ "expected_scene_id": SCENE_ID, "snapshot_revision": STALE }),
        ),
        (
            ReadOperation::ListObjects,
            json!({ "expected_scene_id": SCENE_ID, "snapshot_revision": STALE }),
        ),
        // 選択はプロジェクトの状態であり revision に連動する。カタログの
        // 一覧と違い、照合の対象になる。
        (
            ReadOperation::GetSelection,
            json!({ "expected_scene_id": SCENE_ID, "snapshot_revision": STALE }),
        ),
    ];

    for (operation, params) in paged {
        let adapter = FakeAdapter::new();
        let error = read(&adapter, operation, params).unwrap_err();

        assert_eq!(
            error.code,
            ErrorCode::PreconditionFailed,
            "{operation:?} が古い snapshot_revision を受理しました"
        );
        // 文言は要求元が次に何をすればよいかを述べる唯一の口である。
        assert_eq!(
            error.message, "一覧が変化したため、先頭のページから取り直してください",
            "{operation:?}"
        );
        assert!(error.retryable);
        assert_eq!(error.details["requested_snapshot_revision"], STALE);
        assert_eq!(error.details["current_snapshot_revision"], REVISION);
    }
}

#[test]
fn get_selection_returns_the_focus_its_section_and_the_selection() {
    let adapter = FakeAdapter::new();
    let result = read(
        &adapter,
        ReadOperation::GetSelection,
        json!({ "expected_scene_id": SCENE_ID }),
    )
    .unwrap();

    assert_eq!(result["project_revision"], REVISION);
    assert_eq!(result["focus"]["layer"], 1);
    assert_eq!(result["focus_section"], 1);
    assert_eq!(result["selected"].as_array().unwrap().len(), 2);
    assert_eq!(result["page"]["total_count"], 2);
    assert_eq!(adapter.calls(), vec!["get_selection"]);
}

#[test]
fn effect_catalog_page_ignores_snapshot_revision() {
    // 登録済み effect の一覧はプロジェクトの編集内容から独立しており、
    // revision の照合対象にしない。無関係な編集で revision が進んでも
    // 後続ページは拒否されない。
    let adapter = FakeAdapter::new();
    let result = read(
        &adapter,
        ReadOperation::ListAvailableEffects,
        json!({ "snapshot_revision": REVISION - 1 }),
    )
    .unwrap();

    assert_eq!(result["items"].as_array().unwrap().len(), 2);
}

#[test]
fn effect_catalog_page_reports_the_revision_of_the_enumeration() {
    // 照合しないことと、ページのメタ情報へ何を載せるかは別である。0 のような
    // 固定値は実在し得る revision と区別が付かず、他の一覧から得た値と混同
    // され得るため、列挙時点の revision をそのまま載せる。
    let adapter = FakeAdapter::new();
    let result = read(&adapter, ReadOperation::ListAvailableEffects, json!({})).unwrap();

    assert_eq!(result["page"]["snapshot_revision"], 7);
    assert_eq!(result["page"]["snapshot_revision"], REVISION);
}

/// ページ間の revision 照合を行わない列挙。
///
/// いずれも登録物の集合であり、プロジェクトの編集内容から独立している。
const CATALOG_OPERATIONS: [ReadOperation; 5] = [
    ReadOperation::ListAvailableEffects,
    ReadOperation::ListFonts,
    ReadOperation::ListPalettes,
    ReadOperation::ListModules,
    ReadOperation::ListObjectAliases,
];

#[test]
fn catalog_pages_ignore_snapshot_revision() {
    // 無関係な編集で revision が進んでも、2 ページ目以降は拒否されない。
    // 照合すると、一覧と関わりの無い編集で先頭からの取り直しを強いる一方、
    // 一覧自身の変化はその値に現れないため取りこぼしも防げない。
    for operation in CATALOG_OPERATIONS {
        let adapter = FakeAdapter::new();
        let result = read(
            &adapter,
            operation,
            json!({ "offset": 1, "limit": 1, "snapshot_revision": REVISION - 1 }),
        )
        .unwrap_or_else(|error| panic!("{operation:?} が拒否されました: {error:?}"));

        assert_eq!(
            result["page"]["offset"], 1,
            "{operation:?} が 2 ページ目を返していません"
        );
    }
}

#[test]
fn catalog_pages_report_the_revision_of_the_enumeration() {
    // 照合しないことと、ページのメタ情報へ何を載せるかは別である。0 のような
    // 固定値は実在し得る revision と区別が付かない。
    for operation in CATALOG_OPERATIONS {
        let adapter = FakeAdapter::new();
        let result = read(&adapter, operation, json!({})).unwrap();
        assert_eq!(
            result["page"]["snapshot_revision"], REVISION,
            "{operation:?}"
        );
    }
}

#[test]
fn the_object_alias_listing_does_not_verify_the_snapshot_revision() {
    // 前ページが返した値をそのまま送り返しても拒否されない。検証済みの
    // 要求をそのまま渡すと落ちる。
    let adapter = FakeAdapter::new();
    let first = read(
        &adapter,
        ReadOperation::ListObjectAliases,
        json!({ "offset": 0, "limit": 1 }),
    )
    .unwrap();
    let returned = first["page"]["snapshot_revision"].clone();

    let second = read(
        &adapter,
        ReadOperation::ListObjectAliases,
        json!({ "offset": 1, "limit": 1, "snapshot_revision": returned }),
    )
    .unwrap();

    assert_eq!(second["page"]["offset"], 1);
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
}

#[test]
fn an_advanced_revision_does_not_reject_the_second_page_of_object_aliases() {
    // 上と対になる。上は「照合しない」を、こちらは「照合しないことが実際に
    // 効く」を見る。フェイクは呼ぶたびに revision を進める。
    let adapter = FakeAdapter::new();
    let first = read(
        &adapter,
        ReadOperation::ListObjectAliases,
        json!({ "offset": 0, "limit": 1 }),
    )
    .unwrap();
    let second = read(
        &adapter,
        ReadOperation::ListObjectAliases,
        json!({ "offset": 1, "limit": 1, "snapshot_revision": REVISION }),
    )
    .unwrap();

    assert_eq!(first["page"]["snapshot_revision"], REVISION);
    assert_eq!(second["page"]["snapshot_revision"], REVISION + 1);
    assert_eq!(second["page"]["offset"], 1);
}

#[test]
fn the_object_alias_listing_filters_by_label() {
    let adapter = FakeAdapter::new();
    let result = read(
        &adapter,
        ReadOperation::ListObjectAliases,
        json!({ "label": "テロップ集" }),
    )
    .unwrap();

    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["name"], "テロップ");
    assert_eq!(result["page"]["total_count"], 1);
}

#[test]
fn an_unusable_label_is_invalid_argument_without_reading() {
    // 種別まで固定する。コードだけを見ると、NUL と長さ超過が同じ応答に
    // 畳まれても気付けない。
    for (label, reason) in [
        (json!("\u{0}"), "contains_nul"),
        (json!("あ".repeat(1025)), "too_long"),
    ] {
        let adapter = FakeAdapter::new();
        let error = read(
            &adapter,
            ReadOperation::ListObjectAliases,
            json!({ "label": label }),
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidArgument, "{label}");
        assert_eq!(error.details["reason"], json!(reason), "{label}");
        assert!(!error.retryable, "{label}");
        assert!(adapter.calls().is_empty(), "{label}");
    }
}

#[test]
fn list_fonts_returns_the_registered_names() {
    let adapter = FakeAdapter::new();
    let result = read(&adapter, ReadOperation::ListFonts, json!({})).unwrap();

    assert_eq!(result["items"], json!(fake_fonts()));
    assert_eq!(result["page"]["total_count"], fake_fonts().len());
}

#[test]
fn list_palettes_returns_the_current_name_and_the_colors() {
    let adapter = FakeAdapter::new();
    let result = read(&adapter, ReadOperation::ListPalettes, json!({})).unwrap();

    assert_eq!(result["current"], "[標準.既定]");
    assert_eq!(
        result["items"][0]["colors"].as_array().unwrap().len(),
        PALETTE_COLOR_COUNT
    );
    assert_eq!(result["items"][0]["colors"][0]["a"], 255);
}

#[test]
fn list_modules_filters_by_type() {
    let adapter = FakeAdapter::new();
    let result = read(
        &adapter,
        ReadOperation::ListModules,
        json!({ "module_type": "plugin_input" }),
    )
    .unwrap();

    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["name"], "入力プラグイン");
    assert_eq!(result["items"][0]["module_type"], "plugin_input");
    assert_eq!(result["page"]["total_count"], 1);
}

#[test]
fn list_modules_rejects_a_type_it_cannot_name() {
    // 絞り込みは閉じた集合に対する等値判定である。名乗れない値を受けると、
    // 0 件が「そういう種別が無い」のか「綴りを間違えた」のか区別できない。
    let adapter = FakeAdapter::new();
    let error = read(
        &adapter,
        ReadOperation::ListModules,
        json!({ "module_type": "script_unknown" }),
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(adapter.calls().is_empty());
}

#[test]
fn the_module_information_never_reaches_the_log() {
    // 説明文は秘匿の対象ではないが、ローカルのログへは残さない。応答の
    // 組み立てを記録へ写す変更が入れば、ここで現れる。
    let logs = crate::test_support::capture_logs(|| {
        let adapter = FakeAdapter::new();
        let result = read(&adapter, ReadOperation::ListModules, json!({})).unwrap();
        assert_eq!(
            result["items"][0]["information"], "標準搭載",
            "説明文が応答へ載っていません"
        );
    });

    for module in fake_modules() {
        assert!(
            !logs.contains(&module.information),
            "説明文がログへ出ています: {logs}"
        );
    }
}

#[test]
fn starting_rejects_read_without_touching_the_adapter() {
    for (operation, params) in all_operations() {
        let adapter = FakeAdapter::new();
        let error = execute_read(
            &adapter,
            &InstanceState::Starting,
            operation,
            &params,
            RequestDeadline::Within(Instant::now() + read_timeout()),
        )
        .unwrap_err();

        assert_eq!(
            error.code,
            ErrorCode::HostBusy,
            "{operation:?} が起動処理中に受理されました"
        );
        assert!(error.retryable);
        assert_eq!(error.details["retry_after_ms"], 500);
        assert!(
            adapter.calls().is_empty(),
            "{operation:?} が起動処理中に読み取り口を呼びました"
        );
    }
}

/// 各 operation が受理しない params。
fn malformed_params_of_all_operations() -> Vec<(ReadOperation, Value)> {
    all_operations()
        .into_iter()
        .map(|(operation, params)| {
            let mut params = params;
            params
                .as_object_mut()
                .unwrap()
                .insert("future".to_string(), json!(1));
            (operation, params)
        })
        .collect()
}

#[test]
fn invalid_params_are_rejected_regardless_of_the_lifecycle_state() {
    // 要求内容の誤りは状態に依存しない。受付判定を先に通すと、解消しない
    // 誤りが再試行を促す host_busy として返ってしまう。
    for state in [
        InstanceState::Starting,
        InstanceState::Draining,
        InstanceState::Gone,
    ] {
        for (operation, params) in malformed_params_of_all_operations() {
            let adapter = FakeAdapter::new();
            let error = execute_read(
                &adapter,
                &state,
                operation,
                &params,
                RequestDeadline::Within(Instant::now() + read_timeout()),
            )
            .unwrap_err();

            assert_eq!(
                error.code,
                ErrorCode::InvalidArgument,
                "{state} の {operation:?} が状態由来のエラーで返りました"
            );
            assert!(adapter.calls().is_empty(), "{state}: {operation:?}");
        }
    }
}

#[test]
fn invalid_params_are_rejected_before_the_deadline_is_evaluated() {
    // 期限超過は再試行可能として返る。要求内容の誤りをその後ろに置くと、
    // 解消しない誤りが再試行可能なエラーに化ける。
    for (operation, params) in malformed_params_of_all_operations() {
        let adapter = FakeAdapter::new();
        let error = execute_read(
            &adapter,
            &InstanceState::Ready,
            operation,
            &params,
            RequestDeadline::Exceeded,
        )
        .unwrap_err();

        assert_eq!(
            error.code,
            ErrorCode::InvalidArgument,
            "{operation:?} が期限超過として返りました"
        );
    }
}

#[test]
fn page_and_filter_violations_are_rejected_regardless_of_the_state() {
    let cases = [
        (
            ReadOperation::ListLayers,
            json!({ "expected_scene_id": SCENE_ID, "limit": 0 }),
        ),
        (
            ReadOperation::ListObjects,
            json!({
                "expected_scene_id": SCENE_ID,
                "filter": { "layer_min": 2, "layer_max": 1 },
            }),
        ),
        (ReadOperation::ListAvailableEffects, json!({ "limit": 201 })),
    ];

    for (operation, params) in cases {
        let adapter = FakeAdapter::new();
        let error = execute_read(
            &adapter,
            &InstanceState::Starting,
            operation,
            &params,
            RequestDeadline::Within(Instant::now() + read_timeout()),
        )
        .unwrap_err();

        assert_eq!(
            error.code,
            ErrorCode::InvalidArgument,
            "{operation:?} が起動処理中に状態由来のエラーで返りました"
        );
        assert!(adapter.calls().is_empty(), "{operation:?}");
    }
}

#[test]
fn pong_carries_the_project_state_in_every_state() {
    // 生存確認は状態を問わず受け付ける。プロジェクトの状態は SDK に触れずに
    // 読めるため、受付できない状態でも載せられる。
    let instance_id = InstanceId::new_v4();
    for state in [
        InstanceState::Starting,
        InstanceState::Ready,
        InstanceState::Busy,
        InstanceState::Draining,
        InstanceState::Gone,
    ] {
        let adapter = FakeAdapter::new();
        let result = pong_result(instance_id, state, &adapter);

        assert_eq!(result.instance_id, instance_id);
        assert_eq!(result.state, state);
        assert_eq!(result.project.epoch, EPOCH);
        assert_eq!(result.project.revision, REVISION);
        assert!(result.project.modified);
        assert_eq!(adapter.calls(), vec!["project_status"]);
    }
}

#[test]
fn pong_does_not_report_a_scene() {
    // シーンは編集ハンドルを介してしか読めず、生存確認を受け付ける全ての
    // 状態でそれを呼べるとは限らない。読み取り口へも問い合わせない。
    let adapter = FakeAdapter::new();
    let result = pong_result(InstanceId::new_v4(), InstanceState::Ready, &adapter);

    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(value.get("scene"), None);
    assert_eq!(
        adapter.calls(),
        vec!["project_status"],
        "生存確認が読み取りを行いました"
    );
}

#[test]
fn timeouts_match_the_intended_budget() {
    // 読み取りが実行の上限まで走っても、応答送信の持ち時間が要求元の
    // 要求フェーズ予算の内側に残る。ここが崩れると、完了した読み取りを
    // 誰も待っていない窓へ送ることになる。
    let read = read_timeout();
    let edit = edit_timeout();
    let write = write_timeout();
    let handshake = handshake_timeout();
    // 要求元の予算は倍率を掛けない一式から採る。
    let server = ScaledBudgets::unscaled();
    let headroom = server.transport_headroom();
    let read_request = server.server_request_phase(RequestBudgetKind::Read);
    let edit_request = server.server_request_phase(RequestBudgetKind::Edit);
    let resolve = server.server_resolve();
    assert!(
        read + write + headroom <= read_request,
        "読み取り {read:?} と送信 {write:?} が要求フェーズ予算 {read_request:?} に収まらない"
    );

    // 編集が実行の上限まで走っても、応答送信の持ち時間が編集要求フェーズ
    // 予算の内側に残る。編集は結果を破棄しないため、この余地が無いと
    // 応答を送り切れないまま接続が切れ得る。
    assert!(
        edit + write + headroom <= edit_request,
        "編集 {edit:?} と送信 {write:?} が編集要求フェーズ予算 {edit_request:?} に収まらない"
    );

    // handshake が解決フェーズの予算を使い切ると、続く ping の往復に
    // 持ち時間が残らず、応答している接続が期限超過として扱われる。
    assert!(
        handshake + write + headroom <= resolve,
        "handshake {handshake:?} と ping 応答 {write:?} が解決フェーズ予算 {resolve:?} に収まらない"
    );

    // 接続を保持する上限（REQUEST_IDLE_TIMEOUT）はここで主張しない。掛かる
    // のは要求フレームの到着待ちだけであり、要求の処理時間を含まないため、
    // 要求フェーズの予算と比べる量ではない。比べると、長い予算を持つ
    // operation を足すたびに、無関係なこの値を引き上げる圧力が生まれる。
    // この値はそのまま「沈黙したクライアントが待受を占有できる時間」であり、
    // 引き上げてよい理由が要求の処理時間の側から来ることはない。

    // 再試行案内の設計値。変えると要求元との取り決めが変わるため、
    // 値そのものを主張する。
    assert_eq!(HOST_BUSY_RETRY_AFTER_MS, 500);
}

#[test]
fn every_operation_draws_the_execution_budget_of_its_kind() {
    // 上限を引く経路は要求処理に 1 つしかない。ここが operation ごとに
    // 意図どおりの値を返すことが、全 operation の期限判定の根拠になる。
    //
    // 引いた上限が期限の判定へ実際に効いていることまで見る。要求が期限を
    // 運ばなければ、採用される期限は operation ごとの上限そのものになる。
    let now = Instant::now();
    let deadline = |operation| resolve_execution_deadline(now, NOW_UNIX_MS, operation, None);

    assert_eq!(execution_timeout(Operation::Ping), write_timeout());
    assert_eq!(
        deadline(Operation::Ping),
        RequestDeadline::Within(now + write_timeout())
    );

    for operation in ReadOperation::ALL {
        assert_eq!(
            deadline(Operation::Read(operation)),
            RequestDeadline::Within(now + read_timeout()),
            "{} が読み取りの上限から外れました",
            operation.as_str()
        );
    }

    for operation in EditOperation::ALL {
        let expected = if operation == EditOperation::ApplyBatch {
            batch_timeout()
        } else {
            edit_timeout()
        };
        assert_eq!(
            deadline(Operation::Edit(operation)),
            RequestDeadline::Within(now + expected),
            "{} が編集の上限から外れました",
            operation.as_str()
        );
    }

    for operation in aviutl2_mcp_core::RenderOperation::ALL {
        assert_eq!(
            deadline(Operation::Render(operation)),
            RequestDeadline::Within(now + render_timeout()),
            "{} がレンダリングの上限から外れました",
            operation.as_str()
        );
    }
}

#[test]
fn admit_request_accepts_only_serviceable_states() {
    for state in [InstanceState::Ready, InstanceState::Busy] {
        assert_eq!(admit_request(&state), Ok(()), "{state} が拒否されました");
    }

    for state in [InstanceState::Starting, InstanceState::Draining] {
        let error = admit_request(&state).unwrap_err();
        assert_eq!(error.code, ErrorCode::HostBusy, "{state} が受理されました");
        assert!(error.retryable);
        assert_eq!(error.details["retry_after_ms"], 500);
    }
}

#[test]
fn gone_instance_is_not_advised_to_retry() {
    // 終了済みのインスタンスは同じ相手として戻らない。再試行の間隔を案内すると
    // 待てば復活するかのように読める。
    let error = admit_request(&InstanceState::Gone).unwrap_err();
    assert_eq!(error.code, ErrorCode::InstanceStale);
    assert_eq!(error.details.get("retry_after_ms"), None);
}

/// 読み取りの失敗の全 variant。新しい variant を足したらここへも足す。
fn read_error_variants() -> Vec<fn() -> ReadError> {
    vec![
        || ReadError::NotReady,
        || ReadError::EditBlocked {
            state: crate::read::EditState::Preview,
        },
        || ReadError::EditBlocked {
            state: crate::read::EditState::Save,
        },
        || ReadError::SceneMismatch {
            expected: 3,
            current: SCENE_ID,
        },
        || ReadError::EpochMismatch,
        || ReadError::FingerprintMismatch {
            current_object: Box::new(crate::test_support::sample_object_summary()),
        },
        || ReadError::ObjectNotFound {
            detected_by: "find_object",
        },
        || ReadError::AmbiguousObject { candidate_count: 2 },
        || ReadError::Sdk {
            operation: "call_read_section",
        },
        || ReadError::Panicked,
    ]
}

#[test]
fn read_failures_keep_their_code_and_details() {
    for make in read_error_variants() {
        let expected = make();
        let adapter = FakeAdapter::failing(make());

        let error = read(&adapter, ReadOperation::GetEditInfo, json!({})).unwrap_err();

        assert_eq!(error.code, expected.error_code(), "{expected}");
        assert_eq!(error.retryable, expected.retryable(), "{expected}");
        assert_eq!(error.message, expected.to_string());
        // 再試行間隔は補助情報の中だけに現れ、重ねて載せない。
        assert_eq!(error.details, expected.details(), "{expected}");
        assert_eq!(
            error.details.get("retry_after_ms").and_then(Value::as_u64),
            expected.retry_after_ms(),
            "{expected}"
        );
    }
}

#[test]
fn scene_mismatch_from_the_adapter_is_precondition_failed() {
    let adapter = FakeAdapter::new();
    let error = read(
        &adapter,
        ReadOperation::ListLayers,
        json!({ "expected_scene_id": SCENE_ID + 1 }),
    )
    .unwrap_err();

    assert_eq!(error.code, ErrorCode::PreconditionFailed);
    assert_eq!(error.details["expected_scene_id"], SCENE_ID + 1);
    assert_eq!(error.details["current_scene_id"], SCENE_ID);
}

#[test]
fn responses_do_not_expose_handles() {
    let mut documents = Vec::new();
    for (operation, params) in all_operations() {
        let adapter = FakeAdapter::new();
        let result = read(&adapter, operation, params).unwrap();
        documents.push(serde_json::to_string(&result).unwrap());
    }
    for make in read_error_variants() {
        let adapter = FakeAdapter::failing(make());
        let error = read(&adapter, ReadOperation::GetEditInfo, json!({})).unwrap_err();
        documents.push(serde_json::to_string(&error).unwrap());
    }

    for document in documents {
        let lowered = document.to_lowercase();
        for forbidden in ["handle", "pointer", "0x", "secret", "nonce"] {
            assert!(
                !lowered.contains(forbidden),
                "{forbidden} が応答に含まれます: {document}"
            );
        }
    }
}

#[test]
fn exceeded_deadline_skips_the_read() {
    for (operation, params) in all_operations() {
        let adapter = FakeAdapter::new();
        let error = execute_read(
            &adapter,
            &InstanceState::Ready,
            operation,
            &params,
            RequestDeadline::Exceeded,
        )
        .unwrap_err();

        assert_eq!(
            error.code,
            ErrorCode::Timeout,
            "{operation:?} が期限超過後に実行されました"
        );
        assert!(error.retryable);
        assert!(
            adapter.calls().is_empty(),
            "{operation:?} が期限超過後に読み取り口を呼びました"
        );
    }
}

#[test]
fn send_uses_the_remaining_budget_after_the_read() {
    let now = Instant::now();
    // 読み取りは期限内で終わり、要求の残りは 500 ミリ秒。送信上限より短いので
    // 残りを採る。
    assert_eq!(
        decide_send(
            now,
            NOW_UNIX_MS,
            RequestDeadline::Within(now + Duration::from_secs(4)),
            Some((NOW_UNIX_MS + 500) as u64),
        ),
        SendDecision::Send(now + Duration::from_millis(500))
    );
}

#[test]
fn send_is_capped_by_the_write_limit() {
    let now = Instant::now();
    assert_eq!(
        decide_send(
            now,
            NOW_UNIX_MS,
            RequestDeadline::Within(now + Duration::from_secs(4)),
            None,
        ),
        SendDecision::Send(now + write_timeout())
    );
    assert_eq!(
        decide_send(
            now,
            NOW_UNIX_MS,
            RequestDeadline::Within(now + Duration::from_secs(4)),
            Some((NOW_UNIX_MS + 60_000) as u64),
        ),
        SendDecision::Send(now + write_timeout())
    );
}

#[test]
fn result_is_discarded_when_the_read_used_up_its_deadline() {
    let now = Instant::now();
    for read_deadline in [now, now - Duration::from_millis(1)] {
        assert_eq!(
            decide_send(
                now,
                NOW_UNIX_MS,
                RequestDeadline::Within(read_deadline),
                None,
            ),
            SendDecision::Discard
        );
    }
}

#[test]
fn result_is_discarded_when_the_request_deadline_passed_during_the_read() {
    let now = Instant::now();
    assert_eq!(
        decide_send(
            now,
            NOW_UNIX_MS,
            RequestDeadline::Within(now + Duration::from_secs(4)),
            Some(NOW_UNIX_MS as u64),
        ),
        SendDecision::Discard
    );
}

#[test]
fn unstarted_read_still_gets_a_send_budget() {
    // 実行前に期限を超過していた要求は捨てる結果を持たない。理由を返せるよう
    // 送信上限だけで送る。
    let now = Instant::now();
    assert_eq!(
        decide_send(now, NOW_UNIX_MS, RequestDeadline::Exceeded, Some(0)),
        SendDecision::Send(now + write_timeout())
    );
}

/// 読み取りの期限を使い切った状態。結果は捨てる判定になる。
fn spent_read_deadline(now: Instant) -> RequestDeadline {
    RequestDeadline::Within(now - Duration::from_millis(1))
}

#[test]
fn successful_result_is_replaced_by_timeout_when_discarded() {
    let now = Instant::now();
    let response = resolve_read_response(
        now,
        NOW_UNIX_MS,
        spent_read_deadline(now),
        None,
        Ok(json!({ "scene": { "id": SCENE_ID } })),
    );

    let error = response.outcome.unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(error.retryable);
    assert!(response.discarded);
    assert_eq!(response.deadline, now + write_timeout());
}

#[test]
fn failure_keeps_its_reason_when_the_deadline_passed() {
    // 読み取りが失敗していれば捨てる結果は無い。期限超過で上書きすると、
    // 再試行しても解消しない理由が再試行可能な timeout に化ける。
    let now = Instant::now();
    for original in [
        error_object(ErrorCode::InvalidArgument, "params の解釈に失敗しました"),
        error_object(ErrorCode::HostBusy, "起動処理中です"),
        error_object(ErrorCode::EditBlocked, "再生中です"),
    ] {
        let response = resolve_read_response(
            now,
            NOW_UNIX_MS,
            spent_read_deadline(now),
            None,
            Err(original.clone()),
        );

        let error = response.outcome.unwrap_err();
        assert_eq!(error, original, "失敗の理由が書き換わりました");
        assert!(
            !response.discarded,
            "捨てる結果が無いのに破棄として扱われました"
        );
        assert_eq!(response.deadline, now + write_timeout());
    }
}

#[test]
fn outcome_is_kept_within_the_deadline() {
    let now = Instant::now();
    let result = json!({ "items": [] });
    let response = resolve_read_response(
        now,
        NOW_UNIX_MS,
        RequestDeadline::Within(now + Duration::from_secs(4)),
        Some((NOW_UNIX_MS + 500) as u64),
        Ok(result.clone()),
    );

    assert_eq!(response.outcome.unwrap(), result);
    assert!(!response.discarded);
    assert_eq!(response.deadline, now + Duration::from_millis(500));
}
