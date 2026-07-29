//! AviUtl2 実機を用いた編集の受け入れ確認。
//!
//! # 警告: 本ターゲットは破壊的である
//!
//! 実行するとオブジェクトの作成・移動・改名・削除、effect の付与・削除、設定値の
//! 書き換えを実際に行う。**必ず固定サンプルプロジェクトの複製に対して実行し、
//! 原本は開かないこと。** 本ターゲットは開かれているプロジェクトが複製であることを
//! 実行者へ確認し、複製であると答えられない限り実行しない。終了後は保存せずに
//! AviUtl2 を閉じること。
//!
//! # 準備
//!
//! 1. `au2 develop` で plugin と server を配置し、AviUtl2 から plugin が読み込まれる
//!    状態にする。
//! 2. 固定サンプルプロジェクトの**複製を 2 つ**用意する。2 つは同一内容の複製で
//!    なければならない。同位置オブジェクトの fingerprint が一致する構成でなければ、
//!    別インスタンスの selector を拒否する確認が fingerprint の確認にすり替わる。
//! 3. AviUtl2 を **2 プロセス**起動し、それぞれで別の複製を開く。
//! 4. サンプルプロジェクトは次を満たすこと。
//!    - 現在シーンにオブジェクトが 1 つ以上ある。
//!    - オブジェクトを 1 つも置いていない空きレイヤーが 3 つ以上ある。
//!
//! # 実行方法
//!
//! ```text
//! cargo run -p aviutl2-mcp-server --example live_edit_acceptance
//! ```
//!
//! 実行者は表示される指示に従って AviUtl2 を操作する。自動で判定できる項目は本
//! ターゲットが判定し、実行者の操作を要する項目は指示を出して入力を待つ。判定と
//! 観測値は実行の最後に一覧で出力する。不合格が 1 件でもあれば終了コード 1 で
//! 終了する。
//!
//! # 環境変数
//!
//! | 変数 | 用途 | 省略時 |
//! |---|---|---|
//! | `AVIUTL2_MCP_REGISTRY_DIR` | インスタンス登録ディレクトリ | 既定の場所 |
//! | `AVIUTL2_MCP_LIVE_MEDIA_FILE` | メディアファイルからの作成に使う絶対パス | 該当項目を未実施にする |
//! | `AVIUTL2_MCP_LIVE_UNC_MEDIA_FILE` | ネットワークパス（UNC）の確認に使う絶対パス | 該当項目を未実施にする |
//! | `AVIUTL2_MCP_LIVE_MULTI_ALIAS_FILE` | 複数オブジェクトを含む alias ファイルのパス | 該当項目を未実施にする |
//! | `AVIUTL2_MCP_LIVE_EFFECT_NAME` | 付与に用いる effect 名 | カタログから自動で選ぶ |
//!
//! # 分離方式
//!
//! 実機を要するため、テストターゲットではなく example ターゲットとして定義する。
//! example は `cargo test` ではビルドのみ行われ実行されない。したがって
//! `cargo test --workspace --all-features` が AviUtl2 を起動することはなく、
//! 一方で `cargo clippy --workspace --all-targets --all-features` の検査対象には
//! 含まれるため、型検査と lint は常に働く。
//!
//! 本ターゲットは MCP server ではないため、対話用の出力は stdout へ書く。

use aviutl2_mcp_core::{
    AvailableEffect, EditInfo, EditOutcome, EffectInfo, EffectItem, EffectItemType, EffectSelector,
    EffectType, ErrorCode, ErrorObject, ItemValue, LayerInfo, ObjectDetail, ObjectSelector,
    ObjectSummary, SelectionState,
};
use aviutl2_mcp_server::api::ListInstancesResponse;
use aviutl2_mcp_server::discovery::default_registry_dir;
use aviutl2_mcp_server::mcp::edit_input::{
    AddEffectInput, CreateObjectInput, CursorPositionInput, DeleteEffectInput, DeleteObjectInput,
    DestinationInput, EffectSelectorInput, ExpectedInput, FocusChangeInput, ItemValueInput,
    MoveObjectInput, ObjectSourceInput, PlacementInput, RangeChangeInput, SetEffectStateInput,
    SetObjectItemInput, SetObjectNameInput, SetSelectionInput,
};
use aviutl2_mcp_server::mcp::input::{
    AvailableEffectsPageInput, GetObjectInput, InstanceInput, ListAvailableEffectsInput,
    ListInstancesInput, ListLayersInput, ListObjectsInput, ObjectFilterInput, ObjectSelectorInput,
    PageInput,
};
use aviutl2_mcp_server::mcp::{AviUtl2McpServer, REGISTRY_DIR_ENV};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use serde::de::DeserializeOwned;
use std::cell::RefCell;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// メディアファイルからの作成に使う絶対パスを与える環境変数。
const MEDIA_FILE_ENV: &str = "AVIUTL2_MCP_LIVE_MEDIA_FILE";
/// ネットワークパス（UNC）の確認に使う絶対パスを与える環境変数。
const UNC_MEDIA_FILE_ENV: &str = "AVIUTL2_MCP_LIVE_UNC_MEDIA_FILE";
/// 複数オブジェクトを含む alias ファイルのパスを与える環境変数。
const MULTI_ALIAS_FILE_ENV: &str = "AVIUTL2_MCP_LIVE_MULTI_ALIAS_FILE";
/// 付与に用いる effect 名を与える環境変数。
const EFFECT_NAME_ENV: &str = "AVIUTL2_MCP_LIVE_EFFECT_NAME";

/// 完了条件の検証に要する同時起動プロセス数。
const REQUIRED_INSTANCES: usize = 2;
/// インスタンスが揃うまで待つ上限。
const READY_TIMEOUT: Duration = Duration::from_secs(180);
/// 一覧の再取得間隔。
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// 一覧取得の 1 ページあたり件数。
const PAGE_LIMIT: u32 = 200;
/// 一覧取得で辿るページ数の上限。
const MAX_PAGES: usize = 20;
/// fingerprint の安定性を確かめる読み取り回数。
const STABILITY_READS: usize = 10;
/// revision の収束を確かめる連続編集の回数。
const CONVERGENCE_STEPS: usize = 10;
/// revision の訂正を含めて許す試行回数。
const MAX_ATTEMPTS: u32 = 2;

fn main() {
    let mut report = Report::new();
    if let Err(message) = run(&mut report) {
        println!();
        println!("実行を中断しました: {message}");
        report.abort(&message);
    }
    report.print_summary();
    if report.has_failure() {
        std::process::exit(1);
    }
}

fn run(report: &mut Report) -> Result<(), String> {
    let registry_dir = registry_dir()?;
    println!("registry ディレクトリ: {}", registry_dir.display());
    print_destructive_warning();

    let harness = Harness::new(registry_dir)?;
    let (a, b) = prepare(&harness, report)?;
    let context = Context::new(&harness, &a)?;

    let convergence = section_fingerprint_premises(&harness, report, &a, &context)?;
    section_basic_edits(&harness, report, &a, &context)?;
    section_undo(&harness, report, &a, &context)?;
    section_silent_rejection(&harness, report, &a, &context)?;
    section_item_round_trip(&harness, report, &a, &context)?;
    section_revision(report, &convergence);
    section_blocked(&harness, report, &a, &context)?;
    section_target_confusion(&harness, report, &a, &context)?;
    section_misc(&harness, report, &a, &context)?;
    section_completion(&harness, report, &a, &b)?;

    prompt("すべての確認が終わりました。AviUtl2 を保存せずに閉じてから Enter を押してください。");
    Ok(())
}

/// 破壊的であることを実行前に告げる。
fn print_destructive_warning() {
    println!();
    println!("警告: 本ターゲットは対象プロジェクトを実際に書き換えます。");
    println!("      固定サンプルプロジェクトの複製に対してのみ実行してください。");
    println!("      終了後は保存せずに AviUtl2 を閉じてください。");
}

/// registry ディレクトリを決定する。
fn registry_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os(REGISTRY_DIR_ENV) {
        return Ok(PathBuf::from(dir));
    }
    default_registry_dir().ok_or_else(|| {
        format!("registry ディレクトリを決定できません。{REGISTRY_DIR_ENV} を設定してください。")
    })
}

// ---------------------------------------------------------------------------
// 記録簿
// ---------------------------------------------------------------------------

/// 判定を誰が行うか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// 本ターゲットが自動で判定する。
    Auto,
    /// 実行者の操作または確認を要する。
    Operator,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::Auto => "自動",
            Mode::Operator => "要操作",
        }
    }
}

/// 1 項目の判定結果。
enum Verdict {
    Pass,
    Fail(String),
    Skipped(String),
}

impl Verdict {
    fn label(&self) -> &'static str {
        match self {
            Verdict::Pass => "合格",
            Verdict::Fail(_) => "不合格",
            Verdict::Skipped(_) => "未実施",
        }
    }
}

/// 確認 1 件の記録。
struct Check {
    section: &'static str,
    title: String,
    verified: String,
    mode: Mode,
    verdict: Verdict,
    notes: Vec<String>,
}

/// 実機でしか決着しない事項の観測結果。
struct Observation {
    key: &'static str,
    question: &'static str,
    finding: String,
}

/// 確認の成否。`Ok` の要素は記録に残す観測値。
type CheckResult = Result<Vec<String>, String>;

/// 観測値を 1 つ伴う合格。
fn passed_with(note: impl Into<String>) -> CheckResult {
    Ok(vec![note.into()])
}

/// 判定と観測値をまとめる記録簿。
struct Report {
    checks: Vec<Check>,
    observations: Vec<Observation>,
}

impl Report {
    fn new() -> Self {
        Self {
            checks: Vec::new(),
            observations: Vec::new(),
        }
    }

    /// 確認 1 件を記録し、その場で表示する。
    fn record(
        &mut self,
        section: &'static str,
        title: impl Into<String>,
        verified: impl Into<String>,
        mode: Mode,
        outcome: CheckResult,
    ) {
        let (verdict, notes) = match outcome {
            Ok(notes) => (Verdict::Pass, notes),
            Err(reason) => (Verdict::Fail(reason), Vec::new()),
        };
        self.push(Check {
            section,
            title: title.into(),
            verified: verified.into(),
            mode,
            verdict,
            notes,
        });
    }

    /// 前提が揃わず実施できなかった確認を記録する。
    fn skip(
        &mut self,
        section: &'static str,
        title: impl Into<String>,
        verified: impl Into<String>,
        mode: Mode,
        reason: impl Into<String>,
    ) {
        self.push(Check {
            section,
            title: title.into(),
            verified: verified.into(),
            mode,
            verdict: Verdict::Skipped(reason.into()),
            notes: Vec::new(),
        });
    }

    /// 実機でのみ決着する事項の観測を記録する。
    fn observe(&mut self, key: &'static str, question: &'static str, finding: impl Into<String>) {
        let finding = finding.into();
        println!("  観測 [{key}] {finding}");
        self.observations.push(Observation {
            key,
            question,
            finding,
        });
    }

    /// 実行を中断した事実を記録に残す。
    fn abort(&mut self, message: &str) {
        self.push(Check {
            section: "-",
            title: "実行の完了".to_string(),
            verified: "全項目を最後まで実行できること".to_string(),
            mode: Mode::Auto,
            verdict: Verdict::Fail(message.to_string()),
            notes: Vec::new(),
        });
    }

    fn push(&mut self, check: Check) {
        println!();
        println!(
            "[{}] {} ({}) {}",
            check.section,
            check.verdict.label(),
            check.mode.label(),
            check.title
        );
        println!("  確認: {}", check.verified);
        match &check.verdict {
            Verdict::Pass => {}
            Verdict::Fail(reason) => println!("  理由: {reason}"),
            Verdict::Skipped(reason) => println!("  理由: {reason}"),
        }
        for note in &check.notes {
            println!("  観測値: {note}");
        }
        self.checks.push(check);
    }

    fn has_failure(&self) -> bool {
        self.checks
            .iter()
            .any(|check| matches!(check.verdict, Verdict::Fail(_)))
    }

    /// 判定一覧と観測結果を出力する。
    fn print_summary(&self) {
        println!();
        println!("================ 判定一覧 ================");
        for check in &self.checks {
            println!(
                "[{}] {} ({}) {}",
                check.section,
                check.verdict.label(),
                check.mode.label(),
                check.title
            );
            println!("      確認: {}", check.verified);
            match &check.verdict {
                Verdict::Pass => {}
                Verdict::Fail(reason) | Verdict::Skipped(reason) => {
                    println!("      理由: {reason}");
                }
            }
            for note in &check.notes {
                println!("      観測値: {note}");
            }
        }

        println!();
        println!("========= 実機でのみ決着する事項 =========");
        if self.observations.is_empty() {
            println!("（観測なし）");
        }
        for observation in &self.observations {
            println!("- {} : {}", observation.key, observation.question);
            println!("  観測: {}", observation.finding);
        }

        let passed = self
            .checks
            .iter()
            .filter(|check| matches!(check.verdict, Verdict::Pass))
            .count();
        let failed = self
            .checks
            .iter()
            .filter(|check| matches!(check.verdict, Verdict::Fail(_)))
            .count();
        let skipped = self
            .checks
            .iter()
            .filter(|check| matches!(check.verdict, Verdict::Skipped(_)))
            .count();
        println!();
        println!("================== 集計 ==================");
        println!("合格 {passed} / 不合格 {failed} / 未実施 {skipped}");
    }
}

// ---------------------------------------------------------------------------
// 対話
// ---------------------------------------------------------------------------

/// 実行者へ操作を指示し、Enter を待つ。
fn prompt(message: &str) {
    println!();
    println!("{message}");
    print!("> ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

/// 実行者へ可否を尋ねる。
fn confirm(message: &str) -> bool {
    println!();
    println!("{message}");
    print!("[y/N] > ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y")
}

/// 実行者の回答をそのまま観測値として受け取る。
fn ask(message: &str) -> String {
    println!();
    println!("{message}");
    print!("> ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return String::new();
    }
    line.trim().to_string()
}

/// 実行者の回答を合否として受け取る。
fn operator_verdict(message: &str) -> CheckResult {
    if confirm(message) {
        passed_with("実行者が確認した")
    } else {
        Err("実行者が確認できないと回答した".to_string())
    }
}

// ---------------------------------------------------------------------------
// tool 呼び出し
// ---------------------------------------------------------------------------

/// MCP tool を実機のインスタンスへ発行する実行環境。
///
/// tool は非同期であるため、専用のランタイム上で 1 件ずつ完了まで待つ。
struct Harness {
    server: AviUtl2McpServer,
    runtime: tokio::runtime::Runtime,
    /// 直近の tool result を文字どおりに保持する。応答へ秘匿値が現れないことの
    /// 確認は、DTO へ写した後ではなく実際に返した文字列に対して行う。
    last_raw: RefCell<String>,
}

impl Harness {
    fn new(registry_dir: PathBuf) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|e| format!("ランタイムを作成できません: {e}"))?;
        Ok(Self {
            server: AviUtl2McpServer::new(registry_dir),
            runtime,
            last_raw: RefCell::new(String::new()),
        })
    }

    /// 直近の tool result の文字表現を返す。
    fn last_raw(&self) -> String {
        self.last_raw.borrow().clone()
    }

    /// tool result を型付きの結果へ写し、生の応答を控える。
    fn decode<T: DeserializeOwned>(&self, result: CallToolResult) -> Result<T, ErrorObject> {
        *self.last_raw.borrow_mut() = raw_text(&result);
        let structured = result
            .structured_content
            .clone()
            .unwrap_or(serde_json::Value::Null);
        if result.is_error == Some(true) {
            let error: ErrorObject = serde_json::from_value(structured).unwrap_or_else(|e| {
                ErrorObject::new(
                    ErrorCode::Unknown("undecodable".to_string()),
                    format!("失敗応答を解釈できません: {e}"),
                    false,
                )
            });
            return Err(error);
        }
        serde_json::from_value(structured).map_err(|e| {
            ErrorObject::new(
                ErrorCode::Unknown("undecodable".to_string()),
                format!("成功応答を解釈できません: {e}"),
                false,
            )
        })
    }

    fn list_instances(&self) -> Result<ListInstancesResponse, ErrorObject> {
        let result = self
            .runtime
            .block_on(
                self.server
                    .aviutl2_list_instances(Parameters(ListInstancesInput {
                        offset: 0,
                        limit: PAGE_LIMIT,
                    })),
            );
        self.decode(result)
    }

    fn edit_info(&self, instance: &str) -> Result<EditInfo, ErrorObject> {
        let result = self
            .runtime
            .block_on(self.server.aviutl2_get_edit_info(Parameters(InstanceInput {
                instance_id: instance.to_string(),
            })));
        self.decode(result)
    }

    fn layers(&self, instance: &str, scene_id: i32) -> Result<Vec<LayerInfo>, ErrorObject> {
        let mut items = Vec::new();
        let mut offset = 0;
        for _ in 0..MAX_PAGES {
            let result = self
                .runtime
                .block_on(self.server.aviutl2_list_layers(Parameters(ListLayersInput {
                    instance_id: instance.to_string(),
                    expected_scene_id: scene_id,
                    page: PageInput {
                        offset,
                        limit: PAGE_LIMIT,
                        snapshot_revision: None,
                    },
                })));
            let page: PagedLayers = self.decode(result)?;
            items.extend(page.items);
            match page.page.next_offset {
                Some(next) => offset = next,
                None => break,
            }
        }
        Ok(items)
    }

    fn objects(
        &self,
        instance: &str,
        scene_id: i32,
        layer: Option<usize>,
    ) -> Result<Vec<ObjectSummary>, ErrorObject> {
        let filter = layer.map(|layer| ObjectFilterInput {
            layer_min: Some(layer as u32),
            layer_max: Some(layer as u32),
        });
        let mut items = Vec::new();
        let mut offset = 0;
        for _ in 0..MAX_PAGES {
            let result = self
                .runtime
                .block_on(
                    self.server
                        .aviutl2_list_objects(Parameters(ListObjectsInput {
                            instance_id: instance.to_string(),
                            expected_scene_id: scene_id,
                            filter,
                            page: PageInput {
                                offset,
                                limit: PAGE_LIMIT,
                                snapshot_revision: None,
                            },
                        })),
                );
            let page: PagedObjects = self.decode(result)?;
            items.extend(page.items);
            match page.page.next_offset {
                Some(next) => offset = next,
                None => break,
            }
        }
        Ok(items)
    }

    fn object(
        &self,
        instance: &str,
        selector: &ObjectSelector,
    ) -> Result<ObjectDetail, ErrorObject> {
        let result = self
            .runtime
            .block_on(self.server.aviutl2_get_object(Parameters(GetObjectInput {
                instance_id: instance.to_string(),
                selector: object_selector_input(selector),
            })));
        self.decode(result)
    }

    fn available_effects(&self, instance: &str) -> Result<Vec<AvailableEffect>, ErrorObject> {
        let mut items = Vec::new();
        let mut offset = 0;
        for _ in 0..MAX_PAGES {
            let result = self
                .runtime
                .block_on(self.server.aviutl2_list_available_effects(Parameters(
                    ListAvailableEffectsInput {
                        instance_id: instance.to_string(),
                        effect_type: None,
                        page: AvailableEffectsPageInput {
                            offset,
                            limit: PAGE_LIMIT,
                            snapshot_revision: None,
                        },
                    },
                )));
            let page: PagedEffects = self.decode(result)?;
            items.extend(page.items);
            match page.page.next_offset {
                Some(next) => offset = next,
                None => break,
            }
        }
        Ok(items)
    }

    fn create_object(
        &self,
        instance: &str,
        source: ObjectSourceInput,
        placement: PlacementInput,
        expected: ExpectedInput,
    ) -> Result<EditOutcome, ErrorObject> {
        let result = self
            .runtime
            .block_on(
                self.server
                    .aviutl2_create_object(Parameters(CreateObjectInput {
                        instance_id: instance.to_string(),
                        source,
                        placement,
                        expected,
                    })),
            );
        self.decode(result)
    }

    fn move_object(
        &self,
        instance: &str,
        selector: &ObjectSelector,
        destination: DestinationInput,
        expected: ExpectedInput,
    ) -> Result<EditOutcome, ErrorObject> {
        let result = self
            .runtime
            .block_on(self.server.aviutl2_move_object(Parameters(MoveObjectInput {
                instance_id: instance.to_string(),
                selector: object_selector_input(selector),
                destination,
                expected,
            })));
        self.decode(result)
    }

    fn set_object_name(
        &self,
        instance: &str,
        selector: &ObjectSelector,
        name: Option<String>,
        expected: ExpectedInput,
    ) -> Result<EditOutcome, ErrorObject> {
        let result = self
            .runtime
            .block_on(
                self.server
                    .aviutl2_set_object_name(Parameters(SetObjectNameInput {
                        instance_id: instance.to_string(),
                        selector: object_selector_input(selector),
                        name,
                        expected,
                    })),
            );
        self.decode(result)
    }

    fn set_object_item(
        &self,
        instance: &str,
        selector: &EffectSelector,
        item: &str,
        value: &ItemValue,
        expected: ExpectedInput,
    ) -> Result<EditOutcome, ErrorObject> {
        let result = self
            .runtime
            .block_on(
                self.server
                    .aviutl2_set_object_item(Parameters(SetObjectItemInput {
                        instance_id: instance.to_string(),
                        selector: effect_selector_input(selector),
                        item: item.to_string(),
                        value: item_value_input(value),
                        expected,
                    })),
            );
        self.decode(result)
    }

    fn add_effect(
        &self,
        instance: &str,
        object: &ObjectSelector,
        effect_name: &str,
        expected: ExpectedInput,
    ) -> Result<EditOutcome, ErrorObject> {
        let result = self
            .runtime
            .block_on(self.server.aviutl2_add_effect(Parameters(AddEffectInput {
                instance_id: instance.to_string(),
                object: object_selector_input(object),
                effect_name: effect_name.to_string(),
                expected,
            })));
        self.decode(result)
    }

    fn set_effect_state(
        &self,
        instance: &str,
        selector: &EffectSelector,
        enabled: Option<bool>,
        locked: Option<bool>,
        expected: ExpectedInput,
    ) -> Result<EditOutcome, ErrorObject> {
        let result = self
            .runtime
            .block_on(
                self.server
                    .aviutl2_set_effect_state(Parameters(SetEffectStateInput {
                        instance_id: instance.to_string(),
                        selector: effect_selector_input(selector),
                        enabled,
                        locked,
                        expected,
                    })),
            );
        self.decode(result)
    }

    fn delete_effect(
        &self,
        instance: &str,
        selector: &EffectSelector,
        expected: ExpectedInput,
    ) -> Result<EditOutcome, ErrorObject> {
        let result = self
            .runtime
            .block_on(
                self.server
                    .aviutl2_delete_effect(Parameters(DeleteEffectInput {
                        instance_id: instance.to_string(),
                        selector: effect_selector_input(selector),
                        expected,
                    })),
            );
        self.decode(result)
    }

    fn delete_object(
        &self,
        instance: &str,
        selector: &ObjectSelector,
        expected: ExpectedInput,
    ) -> Result<EditOutcome, ErrorObject> {
        let result = self
            .runtime
            .block_on(
                self.server
                    .aviutl2_delete_object(Parameters(DeleteObjectInput {
                        instance_id: instance.to_string(),
                        selector: object_selector_input(selector),
                        expected,
                    })),
            );
        self.decode(result)
    }

    fn set_selection(
        &self,
        instance: &str,
        scene_id: i32,
        change: SelectionChange,
        expected: ExpectedInput,
    ) -> Result<SelectionState, ErrorObject> {
        let result = self
            .runtime
            .block_on(
                self.server
                    .aviutl2_set_selection(Parameters(SetSelectionInput {
                        instance_id: instance.to_string(),
                        expected_scene_id: scene_id,
                        cursor: change.cursor,
                        selected_range: change.selected_range,
                        focus: change.focus,
                        expected,
                    })),
            );
        self.decode(result)
    }
}

/// `aviutl2_set_selection` へ渡す変更内容。
#[derive(Default)]
struct SelectionChange {
    cursor: Option<CursorPositionInput>,
    selected_range: Option<RangeChangeInput>,
    focus: Option<FocusChangeInput>,
}

/// ページ応答のうち本ターゲットが用いる部分。
#[derive(serde::Deserialize)]
struct PageTail {
    next_offset: Option<u32>,
}

#[derive(serde::Deserialize)]
struct PagedLayers {
    items: Vec<LayerInfo>,
    page: PageTail,
}

#[derive(serde::Deserialize)]
struct PagedObjects {
    items: Vec<ObjectSummary>,
    page: PageTail,
}

#[derive(serde::Deserialize)]
struct PagedEffects {
    items: Vec<AvailableEffect>,
    page: PageTail,
}

/// tool result の文字表現を組み立てる。
fn raw_text(result: &CallToolResult) -> String {
    let mut parts = Vec::new();
    for block in &result.content {
        if let Some(text) = block.as_text() {
            parts.push(text.text.clone());
        }
    }
    if let Some(structured) = &result.structured_content {
        parts.push(structured.to_string());
    }
    parts.join("\n")
}

/// セレクターを tool の入力形式へ写す。
fn object_selector_input(selector: &ObjectSelector) -> ObjectSelectorInput {
    ObjectSelectorInput {
        project_epoch: selector.project_epoch.clone(),
        scene_id: selector.scene_id,
        layer: selector.layer as u32,
        frame: selector.frame as u32,
        name: selector.name.clone(),
        fingerprint: selector.fingerprint.to_string(),
        fingerprint_algorithm: selector.fingerprint_algorithm.to_string(),
    }
}

/// effect セレクターを tool の入力形式へ写す。
fn effect_selector_input(selector: &EffectSelector) -> EffectSelectorInput {
    EffectSelectorInput {
        object: object_selector_input(&selector.object),
        effect_name: selector.effect_name.clone(),
        effect_index: selector.effect_index as u32,
        fingerprint: selector.fingerprint.to_string(),
        fingerprint_algorithm: selector.fingerprint_algorithm.to_string(),
    }
}

/// 読み取った設定値を書き込みの入力形式へ写す。
///
/// `_` を使わない網羅 match とし、種別が増えたときにコンパイルで気付ける形にする。
fn item_value_input(value: &ItemValue) -> ItemValueInput {
    match value {
        ItemValue::Number { value } => ItemValueInput::Number { value: value.get() },
        ItemValue::Integer { value } => ItemValueInput::Integer { value: *value },
        ItemValue::Bool { value } => ItemValueInput::Bool { value: *value },
        ItemValue::Color { value } => ItemValueInput::Color {
            value: value.clone(),
        },
        ItemValue::Choice { value, index } => ItemValueInput::Choice {
            value: value.clone(),
            index: index.map(|index| index as u32),
        },
        ItemValue::File { path } => ItemValueInput::File { path: path.clone() },
        ItemValue::Folder { path } => ItemValueInput::Folder { path: path.clone() },
        ItemValue::Font { name } => ItemValueInput::Font { name: name.clone() },
        ItemValue::Text { value } => ItemValueInput::Text {
            value: value.clone(),
        },
        ItemValue::Unknown { raw } => ItemValueInput::Unknown { raw: raw.clone() },
    }
}

// ---------------------------------------------------------------------------
// 共通の補助
// ---------------------------------------------------------------------------

/// 確認の対象とする稼働中インスタンス。
struct Instance {
    /// 表示用の呼び名。
    label: &'static str,
    /// tool へ渡す ID。
    id: String,
}

/// 全区間で使い回す対象の情報。
struct Context {
    /// 開始時点の現在シーン ID。
    scene_id: i32,
    /// 主たる対象オブジェクトの位置。fingerprint は編集で変わるため位置で覚える。
    target: Placement,
    /// 空きレイヤーの先頭フレーム。
    free_slots: Vec<Placement>,
    /// 利用可能な effect のカタログ。
    catalog: Vec<AvailableEffect>,
    /// 付与に用いる effect 名。
    effect_name: String,
}

/// レイヤーと開始フレームの組。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placement {
    layer: usize,
    frame: usize,
}

impl Context {
    fn new(harness: &Harness, instance: &Instance) -> Result<Self, String> {
        let scene_id = scene_id(harness, instance)?;
        let objects = require(
            harness.objects(&instance.id, scene_id, None),
            "オブジェクトを列挙できません",
        )?;
        let first = objects
            .first()
            .ok_or_else(|| "現在シーンにオブジェクトがありません。".to_string())?;
        let target = Placement {
            layer: first.layer,
            frame: first.frame_start,
        };

        let layers = require(
            harness.layers(&instance.id, scene_id),
            "レイヤーを列挙できません",
        )?;
        let free_slots: Vec<Placement> = layers
            .iter()
            .filter(|layer| layer.object_count == 0 && !layer.locked)
            .map(|layer| Placement {
                layer: layer.index,
                frame: 0,
            })
            .collect();
        if free_slots.len() < 3 {
            return Err(format!(
                "空きレイヤーが {} 個しかありません。3 個以上あるサンプルプロジェクトを使ってください。",
                free_slots.len()
            ));
        }

        let catalog = require(
            harness.available_effects(&instance.id),
            "effect カタログを取得できません",
        )?;
        let effect_name = choose_effect_name(&catalog)?;

        println!();
        println!(
            "対象オブジェクト: layer={} frame={} name={}",
            target.layer,
            target.frame,
            first.name.as_deref().unwrap_or("（標準名）")
        );
        println!(
            "空きレイヤー: {:?}",
            free_slots.iter().map(|slot| slot.layer).collect::<Vec<_>>()
        );
        println!("付与に用いる effect: {effect_name}");

        Ok(Self {
            scene_id,
            target,
            free_slots,
            catalog,
            effect_name,
        })
    }

    /// カタログから effect 定義を引く。
    fn effect_def(&self, name: &str) -> Option<&AvailableEffect> {
        self.catalog.iter().find(|effect| effect.name == name)
    }
}

/// 付与に用いる effect 名を決める。
///
/// 環境変数の指定があればそれを使い、無ければ設定項目を持つ filter 種別の
/// 先頭を選ぶ。設定項目を持つものを選ぶのは、付与した effect をそのまま
/// 設定値の確認へ流用できるようにするためである。
fn choose_effect_name(catalog: &[AvailableEffect]) -> Result<String, String> {
    if let Ok(name) = std::env::var(EFFECT_NAME_ENV) {
        if catalog.iter().any(|effect| effect.name == name) {
            return Ok(name);
        }
        return Err(format!(
            "{EFFECT_NAME_ENV} が指す effect がカタログにありません: {name}"
        ));
    }
    catalog
        .iter()
        .find(|effect| effect.effect_type == EffectType::Filter && !effect.items.is_empty())
        .map(|effect| effect.name.clone())
        .ok_or_else(|| "設定項目を持つ filter 種別の effect がカタログにありません。".to_string())
}

/// 現在シーン ID を読み直す。
fn scene_id(harness: &Harness, instance: &Instance) -> Result<i32, String> {
    let info = require(harness.edit_info(&instance.id), "編集情報を取得できません")?;
    Ok(info.scene.id)
}

/// 直前に読み取った値を前提条件として組み立てる。
fn precondition(harness: &Harness, instance: &Instance) -> Result<ExpectedInput, String> {
    let info = require(harness.edit_info(&instance.id), "編集情報を取得できません")?;
    Ok(ExpectedInput {
        project_epoch: info.project_epoch,
        project_revision: info.project_revision,
    })
}

/// 応答が返した epoch と revision を前提条件として組み立てる。
fn outcome_precondition(outcome: &EditOutcome) -> ExpectedInput {
    ExpectedInput {
        project_epoch: outcome.project_epoch.clone(),
        project_revision: outcome.project_revision,
    }
}

/// 失敗を実行の中断理由へ写す。
fn require<T>(result: Result<T, ErrorObject>, what: &str) -> Result<T, String> {
    result.map_err(|error| format!("{what}: {}", describe_error(&error)))
}

/// 失敗を記録に残せる文字列へ写す。
fn describe_error(error: &ErrorObject) -> String {
    format!(
        "code={} retryable={} details={} message={}",
        error.code.as_snake_case(),
        error.retryable,
        error.details,
        error.message
    )
}

/// `details` の文字列値を取り出す。
fn detail_str(error: &ErrorObject, key: &str) -> Option<String> {
    error
        .details
        .get(key)
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
}

/// `details` の整数値を取り出す。
fn detail_u64(error: &ErrorObject, key: &str) -> Option<u64> {
    error.details.get(key).and_then(|value| value.as_u64())
}

/// 要求が期待どおりに拒否されたことを確かめる。
///
/// 拒否されたことだけを見ると、複数あるガードのうち 1 つしか働いていない場合でも
/// 合格になり得る。どのガードが働いたかを `mismatch` まで固定する。
fn expect_rejection<T>(
    result: Result<T, ErrorObject>,
    code: ErrorCode,
    mismatch: Option<&str>,
) -> CheckResult {
    let error = match result {
        Ok(_) => return Err("拒否されず成功しました".to_string()),
        Err(error) => error,
    };
    if error.code != code {
        return Err(format!(
            "{} を期待しましたが {}",
            code.as_snake_case(),
            describe_error(&error)
        ));
    }
    let observed = detail_str(&error, "mismatch");
    if let Some(expected) = mismatch
        && observed.as_deref() != Some(expected)
    {
        return Err(format!(
            "mismatch={expected} を期待しましたが {}",
            describe_error(&error)
        ));
    }
    Ok(vec![describe_error(&error)])
}

/// 位置からオブジェクトを引き直す。
///
/// fingerprint は編集のたびに変わるため、対象は位置で覚えて都度読み直す。
fn resolve_object(
    harness: &Harness,
    instance: &Instance,
    scene_id: i32,
    at: Placement,
) -> Result<ObjectSummary, String> {
    let objects = require(
        harness.objects(&instance.id, scene_id, Some(at.layer)),
        "オブジェクトを列挙できません",
    )?;
    objects
        .into_iter()
        .find(|object| object.frame_start == at.frame)
        .ok_or_else(|| {
            format!(
                "layer={} frame={} にオブジェクトがありません",
                at.layer, at.frame
            )
        })
}

/// 現在シーンの全オブジェクトを控える。
fn snapshot(
    harness: &Harness,
    instance: &Instance,
    scene_id: i32,
) -> Result<Vec<ObjectSummary>, String> {
    require(
        harness.objects(&instance.id, scene_id, None),
        "オブジェクトを列挙できません",
    )
}

/// 控えた内容と現在の内容が完全に一致することを確かめる。
fn expect_unchanged(before: &[ObjectSummary], after: &[ObjectSummary]) -> Result<(), String> {
    if before.len() != after.len() {
        return Err(format!(
            "オブジェクト件数が {} から {} へ変わりました",
            before.len(),
            after.len()
        ));
    }
    for (before, after) in before.iter().zip(after.iter()) {
        if before != after {
            return Err(format!(
                "layer={} frame={} のオブジェクトが変化しています",
                before.layer, before.frame_start
            ));
        }
    }
    Ok(())
}

/// 応答が返した revision で編集し、食い違えば 1 度だけ訂正して再送する。
///
/// 二重加算が起きていても後続の確認が巻き添えで落ちないようにするための経路で
/// あり、同時に「訂正すれば 2 回以内に収束するか」の測定でもある。
fn with_revision_correction<T, F>(revision: u64, mut attempt: F) -> Result<(T, u32), ErrorObject>
where
    F: FnMut(u64) -> Result<T, ErrorObject>,
{
    match attempt(revision) {
        Ok(value) => Ok((value, 1)),
        Err(error) => {
            if error.code != ErrorCode::PreconditionFailed
                || detail_str(&error, "mismatch").as_deref() != Some("project_revision")
            {
                return Err(error);
            }
            let Some(current) = detail_u64(&error, "current_project_revision") else {
                return Err(error);
            };
            attempt(current).map(|value| (value, MAX_ATTEMPTS))
        }
    }
}

/// 書き込みを公開している設定項目の種別。
const WRITABLE_ITEM_TYPES: &[EffectItemType] = &[
    EffectItemType::Integer,
    EffectItemType::Number,
    EffectItemType::Check,
    EffectItemType::Text,
    EffectItemType::String,
    EffectItemType::File,
    EffectItemType::Folder,
    EffectItemType::Font,
    EffectItemType::Color,
    EffectItemType::Select,
];

/// 書き込みを公開している種別か。
fn is_writable_type(item_type: &EffectItemType) -> bool {
    WRITABLE_ITEM_TYPES.contains(item_type)
}

/// 値を変えた別の設定値を作る。作れない種別では `None`。
///
/// 元へ戻せることが前提であるため、表記の揺れが起き得る種別（色・選択肢・
/// パス・フォント名）は対象にしない。
fn altered_value(value: &ItemValue) -> Option<ItemValue> {
    match value {
        ItemValue::Integer { value } => Some(ItemValue::Integer {
            value: value.saturating_add(1),
        }),
        ItemValue::Number { value } => Some(ItemValue::Number {
            value: aviutl2_mcp_core::FiniteF64::try_new(value.get() + 1.0)?,
        }),
        ItemValue::Bool { value } => Some(ItemValue::Bool { value: !*value }),
        ItemValue::Text { value } => Some(ItemValue::Text {
            value: format!("{value}A"),
        }),
        ItemValue::Color { .. }
        | ItemValue::Choice { .. }
        | ItemValue::File { .. }
        | ItemValue::Folder { .. }
        | ItemValue::Font { .. }
        | ItemValue::Unknown { .. } => None,
    }
}

/// 値を書き換えられる設定項目を 1 つ選ぶ。
fn alterable_item(detail: &ObjectDetail) -> Option<(EffectSelector, EffectItem, ItemValue)> {
    for effect in &detail.effects {
        for item in &effect.items {
            if !is_writable_type(&item.item_type) {
                continue;
            }
            if let Some(next) = altered_value(&item.value) {
                return Some((effect.selector.clone(), item.clone(), next));
            }
        }
    }
    None
}

/// カタログ定義から effect の種別を引く。
fn effect_type_of(context: &Context, name: &str) -> Option<EffectType> {
    context
        .effect_def(name)
        .map(|effect| effect.effect_type.clone())
}

/// 環境変数で与えられた値を取り出す。
fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

// ---------------------------------------------------------------------------
// 準備
// ---------------------------------------------------------------------------

/// 2 プロセスが揃い、いずれも複製を開いていることを確かめる。
fn prepare(harness: &Harness, report: &mut Report) -> Result<(Instance, Instance), String> {
    prompt(&format!(
        "AviUtl2 を {REQUIRED_INSTANCES} プロセス起動し、それぞれで固定サンプルプロジェクトの\n\
         別の複製を開いてください。plugin が ready になったら Enter を押してください。"
    ));

    let response = wait_for_instances(harness)?;
    let mut instances = Vec::new();
    for (index, info) in response.instances.iter().enumerate() {
        let project = match &info.project {
            Some(project) => format!(
                "{} ({})",
                project.display_name.as_deref().unwrap_or("未命名"),
                project.path.as_deref().unwrap_or("未保存")
            ),
            None => "（プロジェクトなし）".to_string(),
        };
        let label = if index == 0 { "A" } else { "B" };
        println!(
            "インスタンス {label}: instance_id={} pid={} project={project}",
            info.instance_id, info.pid
        );
        instances.push(Instance {
            label,
            id: info.instance_id.to_string(),
        });
    }

    if !confirm(
        "上記 2 つはいずれも固定サンプルプロジェクトの複製であり、原本ではありませんか。\n\
         本ターゲットは対象を実際に書き換えます。原本であれば中止してください。",
    ) {
        return Err("複製であることを確認できないため実行しません。".to_string());
    }

    let mut iter = instances.into_iter();
    let a = iter.next().expect("2 件を確認済み");
    let b = iter.next().expect("2 件を確認済み");

    // 複製であることを内容の面からも確かめる。同位置オブジェクトの fingerprint が
    // 一致しない構成では、別インスタンスの selector を拒否する確認が epoch では
    // なく fingerprint で通ってしまい、確認の意味が変わる。
    let outcome = compare_copies(harness, &a, &b);
    report.record(
        "6.前提",
        "2 つの複製が同一内容であること",
        "同位置オブジェクトの fingerprint が両インスタンスで一致する",
        Mode::Auto,
        outcome,
    );

    Ok((a, b))
}

/// 期待する件数のインスタンスが現れるまで待つ。
fn wait_for_instances(harness: &Harness) -> Result<ListInstancesResponse, String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let response = require(harness.list_instances(), "インスタンスを列挙できません")?;
        if response.instances.len() == REQUIRED_INSTANCES {
            return Ok(response);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{REQUIRED_INSTANCES} 件を期待しましたが {} 件でした。",
                response.instances.len()
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// 2 つのプロジェクトが同一内容の複製であることを確かめる。
fn compare_copies(harness: &Harness, a: &Instance, b: &Instance) -> CheckResult {
    let scene_a = scene_id(harness, a)?;
    let scene_b = scene_id(harness, b)?;
    let objects_a = snapshot(harness, a, scene_a)?;
    let objects_b = snapshot(harness, b, scene_b)?;

    if objects_a.len() != objects_b.len() {
        return Err(format!(
            "オブジェクト件数が異なります: {} と {}",
            objects_a.len(),
            objects_b.len()
        ));
    }
    for (left, right) in objects_a.iter().zip(objects_b.iter()) {
        if left.layer != right.layer || left.frame_start != right.frame_start {
            return Err("オブジェクトの配置が異なります".to_string());
        }
        if left.fingerprint != right.fingerprint {
            return Err(format!(
                "layer={} frame={} の fingerprint が一致しません",
                left.layer, left.frame_start
            ));
        }
    }
    Ok(vec![format!(
        "{} 件のオブジェクトが同位置・同 fingerprint",
        objects_a.len()
    )])
}

// ---------------------------------------------------------------------------
// 5.9 fingerprint の前提
// ---------------------------------------------------------------------------

/// 応答が返した revision で編集を連鎖させたときの収束の様子。
struct Convergence {
    /// 実施した編集の回数。
    steps: usize,
    /// 1 回目の送信で成功した回数。
    first_attempt: usize,
    /// revision を訂正した 2 回目で成功した回数。
    corrected: usize,
}

/// fingerprint の前提を最初に確かめる。
///
/// ここが破れていると以降の全ての編集が成立しないため、他のどの確認よりも先に
/// 実施する。
fn section_fingerprint_premises(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> Result<Convergence, String> {
    println!();
    println!("### 5.9 fingerprint の前提");

    let outcome = check_alias_stability(harness, report, instance, context);
    report.record(
        "5.9",
        "alias の安定性",
        format!(
            "無変更のオブジェクトを {STABILITY_READS} 回読み、fingerprint と alias が全て一致する"
        ),
        Mode::Auto,
        outcome,
    );

    let outcome = check_alias_covers_effects(harness, report, instance, context);
    report.record(
        "5.9",
        "effect の変更がオブジェクト fingerprint へ及ぶこと",
        "effect の設定値と有効状態を変えるとオブジェクトの fingerprint が変わる",
        Mode::Auto,
        outcome,
    );

    let outcome = check_effect_index_shift(harness, report, instance, context);
    report.record(
        "5.9",
        "effect の index シフトの検出",
        "同名 effect を 2 つ付与して前方を削除すると、削除前の selector での編集が拒否される",
        Mode::Auto,
        outcome,
    );

    let outcome = check_layer_lock(harness, report, instance, context);
    report.record(
        "5.9",
        "レイヤーのロック",
        "ロックしたレイヤー上の対象への移動・削除・名前変更が precondition_failed（layer_locked）になる",
        Mode::Operator,
        outcome,
    );

    let (outcome, convergence) = check_revision_convergence(harness, instance, context);
    report.record(
        "5.9",
        "revision の収束",
        format!("応答が返した revision で次の編集を行う操作を {CONVERGENCE_STEPS} 回連続し、いずれも {MAX_ATTEMPTS} 回以内で成功する"),
        Mode::Auto,
        outcome,
    );
    report.observe(
        "revision_convergence",
        "応答が返した revision による連続編集は 2 回以内で収束するか",
        format!(
            "{} 回中、1 回目で成功 {} 回 / 訂正して成功 {} 回",
            convergence.steps, convergence.first_attempt, convergence.corrected
        ),
    );

    Ok(convergence)
}

/// 無変更のオブジェクトを繰り返し読み、同一性の材料が揺れないことを確かめる。
fn check_alias_stability(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    let first = resolve_object(harness, instance, context.scene_id, context.target)?;
    let mut fingerprints = Vec::new();
    let mut aliases = Vec::new();
    for _ in 0..STABILITY_READS {
        let listed = resolve_object(harness, instance, context.scene_id, context.target)?;
        fingerprints.push(listed.fingerprint.clone());
        let detail = require(
            harness.object(&instance.id, &first.selector),
            "対象の詳細を取得できません",
        )?;
        aliases.push(detail.alias);
    }

    let stable_fingerprint = fingerprints.iter().all(|value| *value == first.fingerprint);
    let stable_alias = aliases.windows(2).all(|pair| pair[0] == pair[1]);
    report.observe(
        "object_alias_stability",
        "無変更のオブジェクトの alias と fingerprint は連続読み取りで安定するか",
        format!("fingerprint 安定={stable_fingerprint} alias 安定={stable_alias}"),
    );

    if !stable_fingerprint {
        return Err(
            "無変更のオブジェクトの fingerprint が読み取りごとに変わりました。対象が恒久的に編集不能になります。"
                .to_string(),
        );
    }
    if !stable_alias {
        return Err("無変更のオブジェクトの alias が読み取りごとに変わりました。".to_string());
    }
    Ok(vec![format!("{STABILITY_READS} 回とも同一")])
}

/// effect の変更がオブジェクトの同一性へ反映されることを確かめる。
fn check_alias_covers_effects(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    let detail = require(
        harness.object(&instance.id, &object.selector),
        "対象の詳細を取得できません",
    )?;
    let Some((selector, item, next)) = alterable_item(&detail) else {
        return Err("値を書き換えられる設定項目が対象にありません。".to_string());
    };
    let before_alias = detail.alias.clone();
    let before_fingerprint = object.fingerprint.clone();
    let original = item.value.clone();

    let expected = precondition(harness, instance)?;
    let changed = require(
        harness.set_object_item(&instance.id, &selector, &item.name, &next, expected),
        "設定値を変更できません",
    )?;
    let after_item = changed
        .object
        .clone()
        .ok_or_else(|| "設定値の変更が対象を返しませんでした".to_string())?;
    let detail_after_item = require(
        harness.object(&instance.id, &after_item.selector),
        "変更後の詳細を取得できません",
    )?;

    let item_changed_fingerprint = after_item.fingerprint != before_fingerprint;
    let item_changed_alias = detail_after_item.alias != before_alias;

    // 有効状態の変更でも同じことを見る。
    let effect_selector = detail_after_item
        .effects
        .iter()
        .find(|effect| effect.name == selector.effect_name)
        .map(|effect| effect.selector.clone())
        .ok_or_else(|| "変更した effect を再取得できませんでした".to_string())?;
    let expected = precondition(harness, instance)?;
    let toggled =
        harness.set_effect_state(&instance.id, &effect_selector, Some(false), None, expected);
    let (enabled_changed_fingerprint, enabled_changed_alias, toggled_object) = match &toggled {
        Ok(outcome) => {
            let object = outcome.object.clone();
            let alias = match &object {
                Some(object) => harness
                    .object(&instance.id, &object.selector)
                    .map(|detail| detail.alias)
                    .unwrap_or_default(),
                None => String::new(),
            };
            (
                object.as_ref().map(|object| object.fingerprint.clone())
                    != Some(after_item.fingerprint.clone()),
                alias != detail_after_item.alias,
                object,
            )
        }
        Err(_) => (false, false, None),
    };

    report.observe(
        "alias_includes_effect_state",
        "オブジェクトの alias 表現は配下 effect の設定値と有効状態を含むか",
        format!(
            "設定値変更で alias が変化={item_changed_alias} / 有効状態変更で alias が変化={enabled_changed_alias}"
        ),
    );

    // 後始末: 有効状態と設定値を元へ戻す。
    if let (Ok(_), Some(object)) = (&toggled, &toggled_object)
        && let Some(effect) = require(
            harness.object(&instance.id, &object.selector),
            "戻す前の詳細を取得できません",
        )?
        .effects
        .iter()
        .find(|effect| effect.name == selector.effect_name)
    {
        let expected = precondition(harness, instance)?;
        let _ =
            harness.set_effect_state(&instance.id, &effect.selector, Some(true), None, expected);
    }
    restore_item(
        harness,
        instance,
        context,
        &selector.effect_name,
        &item.name,
        &original,
    )?;

    if !item_changed_fingerprint {
        return Err(
            "設定値を変えてもオブジェクトの fingerprint が変わりませんでした。".to_string(),
        );
    }
    if toggled.is_ok() && !enabled_changed_fingerprint {
        return Err(
            "有効状態を変えてもオブジェクトの fingerprint が変わりませんでした。".to_string(),
        );
    }
    Ok(vec![format!(
        "設定値変更で fingerprint 変化={item_changed_fingerprint} / 有効状態変更の結果={}",
        match &toggled {
            Ok(_) => format!("成功・fingerprint 変化={enabled_changed_fingerprint}"),
            Err(error) => describe_error(error),
        }
    )])
}

/// 設定値を元の値へ戻す。
fn restore_item(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    effect_name: &str,
    item_name: &str,
    value: &ItemValue,
) -> Result<(), String> {
    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    let detail = require(
        harness.object(&instance.id, &object.selector),
        "戻す対象の詳細を取得できません",
    )?;
    let Some(effect) = detail
        .effects
        .iter()
        .find(|effect| effect.name == effect_name)
    else {
        return Err("戻す対象の effect が見つかりません".to_string());
    };
    let expected = precondition(harness, instance)?;
    require(
        harness.set_object_item(&instance.id, &effect.selector, item_name, value, expected),
        "設定値を元へ戻せません",
    )?;
    Ok(())
}

/// 同名 effect の index が繰り上がったことを検出できるかを確かめる。
fn check_effect_index_shift(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    let expected = precondition(harness, instance)?;
    let first = require(
        harness.add_effect(
            &instance.id,
            &object.selector,
            &context.effect_name,
            expected,
        ),
        "1 つ目の effect を付与できません",
    )?;
    let object_after_first = first
        .object
        .clone()
        .ok_or_else(|| "付与の応答が対象を返しませんでした".to_string())?;
    let (second, _) = with_revision_correction(first.project_revision, |revision| {
        harness.add_effect(
            &instance.id,
            &object_after_first.selector,
            &context.effect_name,
            ExpectedInput {
                project_epoch: first.project_epoch.clone(),
                project_revision: revision,
            },
        )
    })
    .map_err(|error| {
        format!(
            "2 つ目の effect を付与できません: {}",
            describe_error(&error)
        )
    })?;

    let object_now = second
        .object
        .clone()
        .ok_or_else(|| "付与の応答が対象を返しませんでした".to_string())?;
    let detail = require(
        harness.object(&instance.id, &object_now.selector),
        "付与後の詳細を取得できません",
    )?;
    let same_name: Vec<&EffectInfo> = detail
        .effects
        .iter()
        .filter(|effect| effect.name == context.effect_name)
        .collect();
    if same_name.len() < 2 {
        return Err(format!(
            "同名 effect が {} 件しかなく index シフトを再現できません",
            same_name.len()
        ));
    }
    let front = same_name[0].selector.clone();

    // 前方を削除する。以降、繰り上がった側が index 0 を名乗る。
    let expected = precondition(harness, instance)?;
    let deleted = require(
        harness.delete_effect(&instance.id, &front, expected),
        "前方の effect を削除できません",
    )?;
    let object_after_delete = deleted
        .object
        .clone()
        .ok_or_else(|| "削除の応答が対象を返しませんでした".to_string())?;

    // オブジェクト側の fingerprint は最新のものを使い、effect 側だけを削除前の
    // 値のままにする。こうしないと拒否の理由がオブジェクト側の変化に紛れる。
    let stale = EffectSelector {
        object: object_after_delete.selector.clone(),
        effect_name: front.effect_name.clone(),
        effect_index: front.effect_index,
        fingerprint: front.fingerprint.clone(),
        fingerprint_algorithm: front.fingerprint_algorithm.clone(),
    };
    let expected = precondition(harness, instance)?;
    let attempt = harness.set_effect_state(&instance.id, &stale, Some(false), None, expected);
    let outcome = expect_rejection(attempt, ErrorCode::PreconditionFailed, None);
    report.observe(
        "effect_index_shift",
        "同名 effect の前方を削除した後、削除前の selector は拒否されるか",
        match &outcome {
            Ok(notes) => format!("拒否された: {}", notes.join(" ")),
            Err(reason) => format!("拒否されなかった: {reason}"),
        },
    );

    // 後始末: 残った 1 つを削除して元の構成へ戻す。
    let object_now = resolve_object(harness, instance, context.scene_id, context.target)?;
    let detail = require(
        harness.object(&instance.id, &object_now.selector),
        "後始末の詳細を取得できません",
    )?;
    if let Some(effect) = detail
        .effects
        .iter()
        .find(|effect| effect.name == context.effect_name)
    {
        let expected = precondition(harness, instance)?;
        require(
            harness.delete_effect(&instance.id, &effect.selector, expected),
            "付与した effect を削除できません",
        )?;
    }

    outcome
}

/// ロックしたレイヤー上の対象が守られることを確かめる。
fn check_layer_lock(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    prompt(&format!(
        "AviUtl2 のインスタンス {} で、レイヤー {}（0 始まり）をロックしてから Enter を押してください。",
        instance.label, context.target.layer
    ));

    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    let destination = context.free_slots[0];

    let expected = precondition(harness, instance)?;
    let moved = harness.move_object(
        &instance.id,
        &object.selector,
        DestinationInput {
            layer: destination.layer as u32,
            frame: destination.frame as u32,
        },
        expected,
    );
    let move_outcome = expect_layer_locked(moved);

    let expected = precondition(harness, instance)?;
    let deleted = harness.delete_object(&instance.id, &object.selector, expected);
    let delete_outcome = expect_layer_locked(deleted);

    let expected = precondition(harness, instance)?;
    let renamed = harness.set_object_name(
        &instance.id,
        &object.selector,
        Some("ロック確認".to_string()),
        expected,
    );
    let rename_outcome = expect_layer_locked(renamed);

    let sdk = ask(
        "AviUtl2 の UI 上で、同じロック済みレイヤーのオブジェクトを移動・削除できますか。\n\
         SDK 側がロックを尊重するかの記録に使います（できる / できない / 未確認 で回答）。",
    );
    report.observe(
        "layer_lock_respected_by_sdk",
        "SDK 側はレイヤーのロックを尊重するか",
        format!(
            "plugin 側は要求を SDK へ渡さずに拒否した。UI 上の可否についての回答: {}",
            if sdk.is_empty() { "未回答" } else { &sdk }
        ),
    );

    prompt("レイヤーのロックを解除してから Enter を押してください。");

    let mut notes = Vec::new();
    for (label, outcome) in [
        ("移動", move_outcome),
        ("削除", delete_outcome),
        ("名前変更", rename_outcome),
    ] {
        match outcome {
            Ok(mut observed) => notes.push(format!("{label}: {}", observed.remove(0))),
            Err(reason) => return Err(format!("{label} が拒否されませんでした: {reason}")),
        }
    }
    Ok(notes)
}

/// ロックされたレイヤーに対する拒否であることを確かめる。
fn expect_layer_locked<T>(result: Result<T, ErrorObject>) -> CheckResult {
    let error = match result {
        Ok(_) => return Err("拒否されず成功しました".to_string()),
        Err(error) => error,
    };
    if error.code != ErrorCode::PreconditionFailed {
        return Err(format!(
            "precondition_failed を期待しましたが {}",
            describe_error(&error)
        ));
    }
    if detail_str(&error, "reason").as_deref() != Some("layer_locked") {
        return Err(format!(
            "reason=layer_locked を期待しましたが {}",
            describe_error(&error)
        ));
    }
    Ok(vec![describe_error(&error)])
}

/// 応答が返した revision で編集を連鎖させ、収束することを確かめる。
fn check_revision_convergence(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
) -> (CheckResult, Convergence) {
    let mut convergence = Convergence {
        steps: 0,
        first_attempt: 0,
        corrected: 0,
    };
    let result = run_revision_chain(harness, instance, context, &mut convergence);
    (result, convergence)
}

/// 対象を 2 つの位置の間で往復させ、そのたびに応答の revision を次へ渡す。
fn run_revision_chain(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    convergence: &mut Convergence,
) -> CheckResult {
    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    let home = context.target;
    let away = context.free_slots[0];

    let expected = precondition(harness, instance)?;
    let mut current = require(
        harness.move_object(
            &instance.id,
            &object.selector,
            DestinationInput {
                layer: away.layer as u32,
                frame: away.frame as u32,
            },
            expected,
        ),
        "対象を移動できません",
    )?;
    convergence.steps += 1;
    convergence.first_attempt += 1;

    let mut at_home = false;
    for _ in 1..CONVERGENCE_STEPS {
        let destination = if at_home { away } else { home };
        let selector = current
            .object
            .clone()
            .ok_or_else(|| "移動の応答が対象を返しませんでした".to_string())?
            .selector;
        let epoch = current.project_epoch.clone();
        let (next, attempts) = with_revision_correction(current.project_revision, |revision| {
            harness.move_object(
                &instance.id,
                &selector,
                DestinationInput {
                    layer: destination.layer as u32,
                    frame: destination.frame as u32,
                },
                ExpectedInput {
                    project_epoch: epoch.clone(),
                    project_revision: revision,
                },
            )
        })
        .map_err(|error| {
            format!(
                "{MAX_ATTEMPTS} 回以内に成功しませんでした: {}",
                describe_error(&error)
            )
        })?;
        convergence.steps += 1;
        if attempts == 1 {
            convergence.first_attempt += 1;
        } else {
            convergence.corrected += 1;
        }
        current = next;
        at_home = !at_home;
    }

    // 後始末: 元の位置へ戻す。
    if !at_home {
        let selector = current
            .object
            .clone()
            .ok_or_else(|| "移動の応答が対象を返しませんでした".to_string())?
            .selector;
        let epoch = current.project_epoch.clone();
        with_revision_correction(current.project_revision, |revision| {
            harness.move_object(
                &instance.id,
                &selector,
                DestinationInput {
                    layer: home.layer as u32,
                    frame: home.frame as u32,
                },
                ExpectedInput {
                    project_epoch: epoch.clone(),
                    project_revision: revision,
                },
            )
        })
        .map_err(|error| format!("元の位置へ戻せません: {}", describe_error(&error)))?;
    }

    Ok(vec![format!(
        "{} 回中、1 回目で成功 {} 回 / 訂正して成功 {} 回",
        convergence.steps, convergence.first_attempt, convergence.corrected
    )])
}

// ---------------------------------------------------------------------------
// 5.1 基本の編集
// ---------------------------------------------------------------------------

/// 全編集 tool が実機で成功することを確かめる。
fn section_basic_edits(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> Result<(), String> {
    println!();
    println!("### 5.1 基本の編集");

    match env_value(MEDIA_FILE_ENV) {
        Some(path) => {
            let outcome = check_create_from_media(harness, report, instance, context, &path);
            report.record(
                "5.1",
                "メディアファイルからの作成",
                "メディアファイルからオブジェクトを作成でき、応答が返す位置が UI 上の実配置と一致する",
                Mode::Operator,
                outcome,
            );
        }
        None => report.skip(
            "5.1",
            "メディアファイルからの作成",
            "メディアファイルからオブジェクトを作成できる",
            Mode::Operator,
            format!("{MEDIA_FILE_ENV} が設定されていません"),
        ),
    }

    let outcome = check_edit_chain(harness, report, instance, context);
    report.record(
        "5.1",
        "alias からの作成と応答 selector による連続編集",
        "作成・移動・名前変更・effect の付与・設定値の変更・有効状態の変更・effect の削除・オブジェクトの削除を、応答が返した selector だけで連続して実行できる",
        Mode::Auto,
        outcome,
    );

    Ok(())
}

/// メディアファイルからの作成と、応答が返す位置の妥当性を確かめる。
fn check_create_from_media(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
    path: &str,
) -> CheckResult {
    let slot = context.free_slots[1];
    let expected = precondition(harness, instance)?;
    let created = require(
        harness.create_object(
            &instance.id,
            ObjectSourceInput::MediaFile {
                path: path.to_string(),
            },
            PlacementInput {
                scene_id: context.scene_id,
                layer: slot.layer as u32,
                frame: slot.frame as u32,
            },
            expected,
        ),
        "メディアファイルから作成できません",
    )?;
    let object = created
        .object
        .clone()
        .ok_or_else(|| "作成の応答が対象を返しませんでした".to_string())?;

    // 応答へパスが漏れていないことを、同じ応答の文字列に対して確かめる。
    let leaked = harness.last_raw().contains(path);
    report.observe(
        "create_position_matches_ui",
        "length 自動調整が入った場合でも、作成の応答が返す位置は UI 上の実配置と一致するか",
        format!(
            "要求 layer={} frame={} / 応答 layer={} frame_start={} frame_end={}",
            slot.layer, slot.frame, object.layer, object.frame_start, object.frame_end
        ),
    );

    let confirmed = confirm(&format!(
        "AviUtl2 のインスタンス {} で、作成されたオブジェクトはレイヤー {}、開始フレーム {} にありますか。",
        instance.label, object.layer, object.frame_start
    ));

    // 後始末: 作成したオブジェクトを削除する。
    let expected = outcome_precondition(&created);
    with_revision_correction(expected.project_revision, |revision| {
        harness.delete_object(
            &instance.id,
            &object.selector,
            ExpectedInput {
                project_epoch: expected.project_epoch.clone(),
                project_revision: revision,
            },
        )
    })
    .map_err(|error| {
        format!(
            "作成したオブジェクトを削除できません: {}",
            describe_error(&error)
        )
    })?;

    if leaked {
        return Err("作成の応答に指定したパスが現れました".to_string());
    }
    if !confirmed {
        return Err("応答が返した位置が UI 上の実配置と一致しないと回答されました".to_string());
    }
    Ok(vec![format!(
        "応答 layer={} frame_start={} frame_end={}",
        object.layer, object.frame_start, object.frame_end
    )])
}

/// 応答が返した selector だけで全編集 tool を連続実行する。
fn check_edit_chain(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    let source = resolve_object(harness, instance, context.scene_id, context.target)?;
    let detail = require(
        harness.object(&instance.id, &source.selector),
        "作成元の alias を取得できません",
    )?;
    let alias = detail.alias.clone();
    let slot = context.free_slots[1];
    let moved_to = context.free_slots[2];

    let expected = precondition(harness, instance)?;
    let created = require(
        harness.create_object(
            &instance.id,
            ObjectSourceInput::ObjectAlias {
                alias: alias.clone(),
            },
            PlacementInput {
                scene_id: context.scene_id,
                layer: slot.layer as u32,
                frame: slot.frame as u32,
            },
            expected,
        ),
        "alias から作成できません",
    )?;
    let alias_leaked = harness.last_raw().contains(&alias);
    let mut steps = vec![format!("created={} 件", created.created.len())];

    let object = created
        .object
        .clone()
        .ok_or_else(|| "作成の応答が対象を返しませんでした".to_string())?;
    let mut state = ChainState {
        epoch: created.project_epoch.clone(),
        revision: created.project_revision,
        object,
    };

    // 移動。
    state = chain_step(
        harness,
        instance,
        &mut steps,
        "move_object",
        &state,
        |state, revision| {
            harness.move_object(
                &instance.id,
                &state.object.selector,
                DestinationInput {
                    layer: moved_to.layer as u32,
                    frame: moved_to.frame as u32,
                },
                ExpectedInput {
                    project_epoch: state.epoch.clone(),
                    project_revision: revision,
                },
            )
        },
    )?;

    // 名前変更。
    state = chain_step(
        harness,
        instance,
        &mut steps,
        "set_object_name",
        &state,
        |state, revision| {
            harness.set_object_name(
                &instance.id,
                &state.object.selector,
                Some("受け入れ確認".to_string()),
                ExpectedInput {
                    project_epoch: state.epoch.clone(),
                    project_revision: revision,
                },
            )
        },
    )?;

    // effect の付与。
    let added = chain_outcome(
        harness,
        instance,
        &mut steps,
        "add_effect",
        &state,
        |state, revision| {
            harness.add_effect(
                &instance.id,
                &state.object.selector,
                &context.effect_name,
                ExpectedInput {
                    project_epoch: state.epoch.clone(),
                    project_revision: revision,
                },
            )
        },
    )?;
    let effect = added
        .effect
        .clone()
        .ok_or_else(|| "付与の応答が effect を返しませんでした".to_string())?;
    state = ChainState {
        epoch: added.project_epoch.clone(),
        revision: added.project_revision,
        object: added
            .object
            .clone()
            .ok_or_else(|| "付与の応答が対象を返しませんでした".to_string())?,
    };

    // 設定値の変更。付与した effect に書き換えられる項目があるときだけ行う。
    let mut effect_selector = effect.selector.clone();
    if let Some(item) = effect
        .items
        .iter()
        .find(|item| is_writable_type(&item.item_type) && altered_value(&item.value).is_some())
    {
        let next = altered_value(&item.value).expect("書き換えられる値を選んでいる");
        let selector = effect_selector.clone();
        let changed = chain_outcome(
            harness,
            instance,
            &mut steps,
            "set_object_item",
            &state,
            |state, revision| {
                harness.set_object_item(
                    &instance.id,
                    &selector,
                    &item.name,
                    &next,
                    ExpectedInput {
                        project_epoch: state.epoch.clone(),
                        project_revision: revision,
                    },
                )
            },
        )?;
        effect_selector = changed
            .effect
            .clone()
            .ok_or_else(|| "設定値変更の応答が effect を返しませんでした".to_string())?
            .selector;
        state = ChainState {
            epoch: changed.project_epoch.clone(),
            revision: changed.project_revision,
            object: changed
                .object
                .clone()
                .ok_or_else(|| "設定値変更の応答が対象を返しませんでした".to_string())?,
        };
    } else {
        steps.push("set_object_item=対象項目なし".to_string());
    }

    // 有効状態の変更。
    let selector = effect_selector.clone();
    let disabled = chain_outcome(
        harness,
        instance,
        &mut steps,
        "set_effect_state",
        &state,
        |state, revision| {
            harness.set_effect_state(
                &instance.id,
                &selector,
                Some(false),
                None,
                ExpectedInput {
                    project_epoch: state.epoch.clone(),
                    project_revision: revision,
                },
            )
        },
    )?;
    let effect_selector = disabled
        .effect
        .clone()
        .ok_or_else(|| "有効状態変更の応答が effect を返しませんでした".to_string())?
        .selector;
    state = ChainState {
        epoch: disabled.project_epoch.clone(),
        revision: disabled.project_revision,
        object: disabled
            .object
            .clone()
            .ok_or_else(|| "有効状態変更の応答が対象を返しませんでした".to_string())?,
    };

    // effect の削除。
    let selector = effect_selector.clone();
    state = chain_step(
        harness,
        instance,
        &mut steps,
        "delete_effect",
        &state,
        |state, revision| {
            harness.delete_effect(
                &instance.id,
                &selector,
                ExpectedInput {
                    project_epoch: state.epoch.clone(),
                    project_revision: revision,
                },
            )
        },
    )?;

    // オブジェクトの削除。後始末を兼ねる。
    let selector = state.object.selector.clone();
    let epoch = state.epoch.clone();
    with_revision_correction(state.revision, |revision| {
        harness.delete_object(
            &instance.id,
            &selector,
            ExpectedInput {
                project_epoch: epoch.clone(),
                project_revision: revision,
            },
        )
    })
    .map_err(|error| format!("delete_object に失敗しました: {}", describe_error(&error)))?;
    steps.push("delete_object=成功".to_string());

    report.observe(
        "response_selector_chaining",
        "編集の応答が返した selector は読み直しなしで次の編集へ渡せるか",
        steps.join(" / "),
    );

    if alias_leaked {
        return Err("作成の応答に alias 全文が現れました".to_string());
    }
    Ok(steps)
}

/// 連続編集で持ち回る状態。
struct ChainState {
    epoch: String,
    revision: u64,
    object: ObjectSummary,
}

/// 1 手を実行し、応答から次の状態を組み立てる。
fn chain_step<F>(
    harness: &Harness,
    instance: &Instance,
    steps: &mut Vec<String>,
    label: &str,
    state: &ChainState,
    call: F,
) -> Result<ChainState, String>
where
    F: Fn(&ChainState, u64) -> Result<EditOutcome, ErrorObject>,
{
    let outcome = chain_outcome(harness, instance, steps, label, state, call)?;
    let object = outcome
        .object
        .clone()
        .ok_or_else(|| format!("{label} の応答が対象を返しませんでした"))?;
    Ok(ChainState {
        epoch: outcome.project_epoch,
        revision: outcome.project_revision,
        object,
    })
}

/// 1 手を実行して応答をそのまま返す。
fn chain_outcome<F>(
    _harness: &Harness,
    _instance: &Instance,
    steps: &mut Vec<String>,
    label: &str,
    state: &ChainState,
    call: F,
) -> Result<EditOutcome, String>
where
    F: Fn(&ChainState, u64) -> Result<EditOutcome, ErrorObject>,
{
    let (outcome, attempts) =
        with_revision_correction(state.revision, |revision| call(state, revision))
            .map_err(|error| format!("{label} に失敗しました: {}", describe_error(&error)))?;
    steps.push(format!("{label}=成功({attempts} 回目)"));
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// 5.2 Undo 境界
// ---------------------------------------------------------------------------

/// 1 回の tool 呼び出しが 1 回の取り消しで完全に元へ戻ることを確かめる。
fn section_undo(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> Result<(), String> {
    println!();
    println!("### 5.2 Undo 境界");

    let source = resolve_object(harness, instance, context.scene_id, context.target)?;
    let detail = require(
        harness.object(&instance.id, &source.selector),
        "作成元の alias を取得できません",
    )?;
    let alias = detail.alias.clone();
    let slot = context.free_slots[1];

    let outcome = check_undo(
        harness,
        instance,
        context,
        "alias からの作成",
        |state| {
            let expected = state.expected.clone();
            harness.create_object(
                &instance.id,
                ObjectSourceInput::ObjectAlias {
                    alias: alias.clone(),
                },
                PlacementInput {
                    scene_id: context.scene_id,
                    layer: slot.layer as u32,
                    frame: slot.frame as u32,
                },
                expected,
            )
        },
    );
    report.record(
        "5.2",
        "作成の Undo 単位",
        "alias から作成した結果が 1 回の取り消しで完全に元へ戻る",
        Mode::Operator,
        outcome,
    );

    match env_value(MULTI_ALIAS_FILE_ENV) {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(multi_alias) => {
                let outcome = check_undo(
                    harness,
                    instance,
                    context,
                    "複数オブジェクトの作成",
                    |state| {
                        let expected = state.expected.clone();
                        harness.create_object(
                            &instance.id,
                            ObjectSourceInput::ObjectAlias {
                                alias: multi_alias.clone(),
                            },
                            PlacementInput {
                                scene_id: context.scene_id,
                                layer: slot.layer as u32,
                                frame: slot.frame as u32,
                            },
                            expected,
                        )
                    },
                );
                report.observe(
                    "multi_object_alias_undo_unit",
                    "複数オブジェクトを含む alias の作成は 1 Undo 単位になるか",
                    match &outcome {
                        Ok(notes) => format!("1 回の取り消しで完全に戻った: {}", notes.join(" ")),
                        Err(reason) => format!("完全には戻らなかった: {reason}"),
                    },
                );
                report.record(
                    "5.2",
                    "複数オブジェクトを含む alias の Undo 単位",
                    "複数オブジェクトを含む alias の作成が、全オブジェクトまとめて 1 回の取り消しで元へ戻る",
                    Mode::Operator,
                    outcome,
                );
            }
            Err(error) => report.skip(
                "5.2",
                "複数オブジェクトを含む alias の Undo 単位",
                "複数オブジェクトを含む alias の作成が 1 回の取り消しで元へ戻る",
                Mode::Operator,
                format!("{MULTI_ALIAS_FILE_ENV} のファイルを読めません: {error}"),
            ),
        },
        None => report.skip(
            "5.2",
            "複数オブジェクトを含む alias の Undo 単位",
            "複数オブジェクトを含む alias の作成が 1 回の取り消しで元へ戻る",
            Mode::Operator,
            format!("{MULTI_ALIAS_FILE_ENV} が設定されていません"),
        ),
    }

    let outcome = check_undo(harness, instance, context, "対象の削除", |state| {
        harness.delete_object(&instance.id, &state.object.selector, state.expected.clone())
    });
    report.record(
        "5.2",
        "削除の Undo 単位",
        "削除した対象が 1 回の取り消しで完全に元へ戻る",
        Mode::Operator,
        outcome,
    );

    let outcome = check_undo(harness, instance, context, "effect の付与", |state| {
        harness.add_effect(
            &instance.id,
            &state.object.selector,
            &context.effect_name,
            state.expected.clone(),
        )
    });
    report.record(
        "5.2",
        "effect 付与の Undo 単位",
        "付与した effect が 1 回の取り消しで完全に元へ戻る",
        Mode::Operator,
        outcome,
    );

    Ok(())
}

/// Undo の確認へ渡す、編集の直前に読み取った状態。
struct UndoInput {
    object: ObjectSummary,
    expected: ExpectedInput,
}

/// 編集 → 取り消し → 全件比較で、取り消しの範囲を確かめる。
fn check_undo<F>(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    label: &str,
    edit: F,
) -> CheckResult
where
    F: FnOnce(&UndoInput) -> Result<EditOutcome, ErrorObject>,
{
    let before = snapshot(harness, instance, context.scene_id)?;
    let input = UndoInput {
        object: resolve_object(harness, instance, context.scene_id, context.target)?,
        expected: precondition(harness, instance)?,
    };
    require(edit(&input), &format!("{label} を実行できません"))?;

    let after_edit = snapshot(harness, instance, context.scene_id)?;
    if expect_unchanged(&before, &after_edit).is_ok() {
        return Err(format!("{label} がプロジェクトを変えていません"));
    }

    prompt(&format!(
        "AviUtl2 のインスタンス {} で、取り消し操作を **1 回だけ** 行ってから Enter を押してください（{label} の取り消し）。",
        instance.label
    ));

    let after_undo = snapshot(harness, instance, context.scene_id)?;
    expect_unchanged(&before, &after_undo)
        .map_err(|reason| format!("1 回の取り消しで元へ戻りませんでした: {reason}"))?;
    Ok(vec![format!("{label} は 1 回の取り消しで完全に元へ戻った")])
}

// ---------------------------------------------------------------------------
// 5.3 SDK の無言拒否
// ---------------------------------------------------------------------------

/// SDK が無言で無視する変更を、成功として返さないことを確かめる。
fn section_silent_rejection(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> Result<(), String> {
    println!();
    println!("### 5.3 SDK の無言拒否");

    match find_effect(harness, instance, context, |context, effect| {
        effect_type_of(context, &effect.name) == Some(EffectType::Output)
    })? {
        Some(found) => {
            let outcome = check_output_item_enable(harness, instance, &found);
            report.record(
                "5.3",
                "出力 item の有効状態の変更",
                "出力 item に対する enabled の変更が unsupported_operation になり、成功として返らない",
                Mode::Auto,
                outcome,
            );

            let outcome = check_output_item_lock(harness, report, instance, context, &found);
            report.record(
                "5.3",
                "出力 item のロックと入力 item への同期",
                "出力 item の locked 変更の結果が read-back と矛盾せず、同期により別 item が変わっても誤検出しない",
                Mode::Auto,
                outcome,
            );
        }
        None => {
            for (title, verified) in [
                (
                    "出力 item の有効状態の変更",
                    "出力 item に対する enabled の変更が unsupported_operation になる",
                ),
                (
                    "出力 item のロックと入力 item への同期",
                    "出力 item の locked 変更が read-back と矛盾しない",
                ),
            ] {
                report.skip(
                    "5.3",
                    title,
                    verified,
                    Mode::Auto,
                    "現在シーンのオブジェクトに出力種別の effect が見つかりません",
                );
            }
        }
    }

    match find_effect(harness, instance, context, |context, effect| {
        context
            .effect_def(&effect.name)
            .is_some_and(|def| def.flags.audio && !def.flags.video)
    })? {
        Some(found) => {
            let outcome = check_audio_effect_lock(harness, instance, &found);
            report.record(
                "5.3",
                "audio effect のロック",
                "audio effect に対する locked の変更が unsupported_operation になり、成功として返らない",
                Mode::Auto,
                outcome,
            );
        }
        None => report.skip(
            "5.3",
            "audio effect のロック",
            "audio effect に対する locked の変更が unsupported_operation になる",
            Mode::Auto,
            "現在シーンのオブジェクトに音声のみの effect が見つかりません",
        ),
    }

    Ok(())
}

/// 対象オブジェクトと、そこに積まれた effect。
struct FoundEffect {
    object: ObjectSummary,
    effect: EffectInfo,
}

/// 条件に合う effect を現在シーンから 1 つ探す。
fn find_effect<F>(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    matches: F,
) -> Result<Option<FoundEffect>, String>
where
    F: Fn(&Context, &EffectInfo) -> bool,
{
    let objects = snapshot(harness, instance, context.scene_id)?;
    for object in objects {
        let Ok(detail) = harness.object(&instance.id, &object.selector) else {
            continue;
        };
        if let Some(effect) = detail
            .effects
            .iter()
            .find(|effect| matches(context, effect))
        {
            return Ok(Some(FoundEffect {
                object,
                effect: effect.clone(),
            }));
        }
    }
    Ok(None)
}

/// 出力 item の有効状態を変えられないことを確かめる。
fn check_output_item_enable(
    harness: &Harness,
    instance: &Instance,
    found: &FoundEffect,
) -> CheckResult {
    let expected = precondition(harness, instance)?;
    let result = harness.set_effect_state(
        &instance.id,
        &found.effect.selector,
        Some(false),
        None,
        expected,
    );
    match result {
        Ok(_) => Err(format!(
            "出力 item「{}」の enabled 変更が成功として返りました",
            found.effect.name
        )),
        Err(error) if error.code == ErrorCode::UnsupportedOperation => {
            Ok(vec![describe_error(&error)])
        }
        Err(error) => Err(format!(
            "unsupported_operation を期待しましたが {}",
            describe_error(&error)
        )),
    }
}

/// 出力 item のロック変更が read-back と矛盾しないことを確かめる。
fn check_output_item_lock(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
    found: &FoundEffect,
) -> CheckResult {
    let before = require(
        harness.object(&instance.id, &found.object.selector),
        "ロック変更前の詳細を取得できません",
    )?;
    let before_locks: Vec<(String, bool)> = before
        .effects
        .iter()
        .map(|effect| (effect.name.clone(), effect.locked))
        .collect();

    let expected = precondition(harness, instance)?;
    let result = harness.set_effect_state(
        &instance.id,
        &found.effect.selector,
        None,
        Some(!found.effect.locked),
        expected,
    );

    let after_object = resolve_object(
        harness,
        instance,
        context.scene_id,
        Placement {
            layer: found.object.layer,
            frame: found.object.frame_start,
        },
    )?;
    let after = require(
        harness.object(&instance.id, &after_object.selector),
        "ロック変更後の詳細を取得できません",
    )?;
    let after_locks: Vec<(String, bool)> = after
        .effects
        .iter()
        .map(|effect| (effect.name.clone(), effect.locked))
        .collect();
    let synced: Vec<String> = before_locks
        .iter()
        .zip(after_locks.iter())
        .filter(|(before, after)| before.1 != after.1 && before.0 != found.effect.name)
        .map(|(before, after)| format!("{}: {} -> {}", before.0, before.1, after.1))
        .collect();

    report.observe(
        "output_item_lock_sync",
        "出力 item の locked 変更は入力 item へ同期するか",
        if synced.is_empty() {
            "対象以外の effect のロック状態は変わらなかった".to_string()
        } else {
            format!("対象以外も変化した: {}", synced.join(", "))
        },
    );

    let target_after = after
        .effects
        .iter()
        .find(|effect| effect.name == found.effect.name)
        .map(|effect| effect.locked);

    let note = match &result {
        Ok(_) => {
            if target_after == Some(!found.effect.locked) {
                format!(
                    "成功として返り、読み直しでも locked={} になっていた",
                    !found.effect.locked
                )
            } else {
                return Err(format!(
                    "成功として返りましたが読み直しでは locked={target_after:?} のままでした"
                ));
            }
        }
        Err(error) if error.code == ErrorCode::UnsupportedOperation => {
            if target_after == Some(found.effect.locked) {
                format!(
                    "unsupported_operation で拒否された: {}",
                    describe_error(error)
                )
            } else {
                return Err(format!(
                    "unsupported_operation で拒否されましたが locked が {target_after:?} へ変わっています"
                ));
            }
        }
        Err(error) => {
            return Err(format!(
                "成功または unsupported_operation を期待しましたが {}",
                describe_error(error)
            ));
        }
    };

    // 後始末: ロック状態を戻す。
    if result.is_ok() {
        let object = resolve_object(
            harness,
            instance,
            context.scene_id,
            Placement {
                layer: found.object.layer,
                frame: found.object.frame_start,
            },
        )?;
        let detail = require(
            harness.object(&instance.id, &object.selector),
            "戻す前の詳細を取得できません",
        )?;
        if let Some(effect) = detail
            .effects
            .iter()
            .find(|effect| effect.name == found.effect.name)
        {
            let expected = precondition(harness, instance)?;
            let _ = harness.set_effect_state(
                &instance.id,
                &effect.selector,
                None,
                Some(found.effect.locked),
                expected,
            );
        }
    }

    Ok(vec![note])
}

/// audio effect のロックを変えられないことを確かめる。
fn check_audio_effect_lock(
    harness: &Harness,
    instance: &Instance,
    found: &FoundEffect,
) -> CheckResult {
    let expected = precondition(harness, instance)?;
    let result = harness.set_effect_state(
        &instance.id,
        &found.effect.selector,
        None,
        Some(!found.effect.locked),
        expected,
    );
    match result {
        Ok(_) => Err(format!(
            "audio effect「{}」の locked 変更が成功として返りました",
            found.effect.name
        )),
        Err(error) if error.code == ErrorCode::UnsupportedOperation => {
            Ok(vec![describe_error(&error)])
        }
        Err(error) => Err(format!(
            "unsupported_operation を期待しましたが {}",
            describe_error(&error)
        )),
    }
}

// ---------------------------------------------------------------------------
// 5.4 設定値の round-trip
// ---------------------------------------------------------------------------

/// 読み取った設定値をそのまま書き戻せることを、種別ごとに確かめる。
fn section_item_round_trip(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> Result<(), String> {
    println!();
    println!("### 5.4 設定値の round-trip");

    let scratch = create_scratch_object(harness, instance, context)?;
    let added = add_effects_for_coverage(harness, instance, context, scratch)?;

    let mut results: Vec<TypeResult> = Vec::new();
    let object = resolve_object(harness, instance, context.scene_id, scratch)?;
    let detail = require(
        harness.object(&instance.id, &object.selector),
        "確認対象の詳細を取得できません",
    )?;
    let targets: Vec<(String, usize, String, EffectItemType)> = detail
        .effects
        .iter()
        .flat_map(|effect| {
            effect.items.iter().map(|item| {
                (
                    effect.name.clone(),
                    effect.index,
                    item.name.clone(),
                    item.item_type.clone(),
                )
            })
        })
        .collect();

    for (effect_name, effect_index, item_name, item_type) in targets {
        let outcome = round_trip_one(
            harness,
            instance,
            context,
            scratch,
            &effect_name,
            effect_index,
            &item_name,
        )?;
        results.push(TypeResult {
            item_type,
            item: format!("{effect_name}#{effect_index}.{item_name}"),
            outcome,
        });
    }

    let track = check_track_item(harness, instance, context, scratch)?;

    // 後始末: 追加した effect ごと対象を削除する。
    let object = resolve_object(harness, instance, context.scene_id, scratch)?;
    let expected = precondition(harness, instance)?;
    require(
        harness.delete_object(&instance.id, &object.selector, expected),
        "確認用オブジェクトを削除できません",
    )?;

    report_round_trip(report, &results, &added);
    report.record(
        "5.4",
        "trackbar 項目の変更",
        "trackbar を持つ設定項目の値を同じ経路で変更できる",
        Mode::Auto,
        track,
    );

    Ok(())
}

/// 種別ごとの round-trip の結果。
struct TypeResult {
    item_type: EffectItemType,
    item: String,
    outcome: RoundTrip,
}

/// 1 項目の round-trip の結果。
enum RoundTrip {
    /// 書き戻して読み直した値が一致した。
    Matched,
    /// 書き戻せたが読み直した値が異なった。
    Differed(String),
    /// 書き込みが拒否された。
    Rejected(ErrorCode, String),
}

/// 読み取った値をそのまま書き戻し、読み直して一致を確かめる。
fn round_trip_one(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    at: Placement,
    effect_name: &str,
    effect_index: usize,
    item_name: &str,
) -> Result<RoundTrip, String> {
    let Some((selector, value)) = locate_item(
        harness,
        instance,
        context,
        at,
        effect_name,
        effect_index,
        item_name,
    )?
    else {
        return Ok(RoundTrip::Rejected(
            ErrorCode::NotFound,
            "項目を再取得できません".to_string(),
        ));
    };

    let expected = precondition(harness, instance)?;
    let written = harness.set_object_item(&instance.id, &selector, item_name, &value, expected);
    if let Err(error) = written {
        return Ok(RoundTrip::Rejected(
            error.code.clone(),
            describe_error(&error),
        ));
    }

    let Some((_, after)) = locate_item(
        harness,
        instance,
        context,
        at,
        effect_name,
        effect_index,
        item_name,
    )?
    else {
        return Ok(RoundTrip::Differed(
            "書き戻し後に項目を再取得できません".to_string(),
        ));
    };
    if after == value {
        Ok(RoundTrip::Matched)
    } else {
        Ok(RoundTrip::Differed(format!(
            "書き戻し前後で値が異なる: 前={} 後={}",
            value.kind(),
            after.kind()
        )))
    }
}

/// 位置から設定項目を引き直す。
fn locate_item(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    at: Placement,
    effect_name: &str,
    effect_index: usize,
    item_name: &str,
) -> Result<Option<(EffectSelector, ItemValue)>, String> {
    let object = resolve_object(harness, instance, context.scene_id, at)?;
    let detail = require(
        harness.object(&instance.id, &object.selector),
        "設定項目の再取得に失敗しました",
    )?;
    let Some(effect) = detail
        .effects
        .iter()
        .find(|effect| effect.name == effect_name && effect.index == effect_index)
    else {
        return Ok(None);
    };
    let Some(item) = effect.items.iter().find(|item| item.name == item_name) else {
        return Ok(None);
    };
    Ok(Some((effect.selector.clone(), item.value.clone())))
}

/// 確認用の作業オブジェクトを作る。
fn create_scratch_object(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
) -> Result<Placement, String> {
    let source = resolve_object(harness, instance, context.scene_id, context.target)?;
    let detail = require(
        harness.object(&instance.id, &source.selector),
        "作成元の alias を取得できません",
    )?;
    let slot = context.free_slots[1];
    let expected = precondition(harness, instance)?;
    let created = require(
        harness.create_object(
            &instance.id,
            ObjectSourceInput::ObjectAlias {
                alias: detail.alias.clone(),
            },
            PlacementInput {
                scene_id: context.scene_id,
                layer: slot.layer as u32,
                frame: slot.frame as u32,
            },
            expected,
        ),
        "確認用オブジェクトを作成できません",
    )?;
    let object = created
        .object
        .ok_or_else(|| "作成の応答が対象を返しませんでした".to_string())?;
    Ok(Placement {
        layer: object.layer,
        frame: object.frame_start,
    })
}

/// 公開種別を網羅するため、足りない種別を持つ effect を作業オブジェクトへ足す。
fn add_effects_for_coverage(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    at: Placement,
) -> Result<Vec<String>, String> {
    let object = resolve_object(harness, instance, context.scene_id, at)?;
    let detail = require(
        harness.object(&instance.id, &object.selector),
        "確認対象の詳細を取得できません",
    )?;
    let mut covered: Vec<EffectItemType> = detail
        .effects
        .iter()
        .flat_map(|effect| effect.items.iter().map(|item| item.item_type.clone()))
        .collect();

    let mut added = Vec::new();
    for item_type in WRITABLE_ITEM_TYPES {
        if covered.contains(item_type) {
            continue;
        }
        let Some(candidate) = context.catalog.iter().find(|effect| {
            effect.effect_type == EffectType::Filter
                && effect.items.iter().any(|item| item.item_type == *item_type)
        }) else {
            continue;
        };
        let object = resolve_object(harness, instance, context.scene_id, at)?;
        let expected = precondition(harness, instance)?;
        match harness.add_effect(&instance.id, &object.selector, &candidate.name, expected) {
            Ok(_) => {
                covered.extend(candidate.items.iter().map(|item| item.item_type.clone()));
                added.push(candidate.name.clone());
            }
            Err(error) => {
                println!(
                    "  {} の付与に失敗したため {} の確認は既存項目のみで行います: {}",
                    candidate.name,
                    item_type.kind_name(),
                    describe_error(&error)
                );
            }
        }
    }
    Ok(added)
}

/// 種別ごとの結果を記録する。
fn report_round_trip(report: &mut Report, results: &[TypeResult], added: &[String]) {
    for item_type in WRITABLE_ITEM_TYPES {
        let matching: Vec<&TypeResult> = results
            .iter()
            .filter(|result| result.item_type == *item_type)
            .collect();
        let title = format!("設定値の round-trip（{}）", item_type.kind_name());
        let verified = "aviutl2_get_object が返した値をそのまま書き戻せ、書き戻した後の読み取りが元の値と一致する";
        if matching.is_empty() {
            report.skip(
                "5.4",
                title,
                verified,
                Mode::Auto,
                "確認対象に該当種別の設定項目がありません",
            );
            continue;
        }
        let mut notes = Vec::new();
        let mut failure = None;
        for result in &matching {
            match &result.outcome {
                RoundTrip::Matched => notes.push(format!("{}: 一致", result.item)),
                RoundTrip::Differed(reason) => {
                    failure.get_or_insert(format!("{}: {reason}", result.item));
                }
                RoundTrip::Rejected(code, detail) => {
                    failure.get_or_insert(format!(
                        "{}: 書き込みが {} で拒否された（{detail}）",
                        result.item,
                        code.as_snake_case()
                    ));
                }
            }
        }
        let outcome = match failure {
            Some(reason) => Err(reason),
            None => Ok(notes),
        };
        report.record("5.4", title, verified, Mode::Auto, outcome);
    }

    let composite: Vec<&TypeResult> = results
        .iter()
        .filter(|result| !is_writable_type(&result.item_type))
        .collect();
    let title = "書き込みを公開しない種別の拒否";
    let verified = "複合種別・未知種別への書き込みが成功として返らない";
    if composite.is_empty() {
        report.skip(
            "5.4",
            title,
            verified,
            Mode::Auto,
            "確認対象に書き込みを公開しない種別の設定項目がありません",
        );
    } else {
        let mut notes = Vec::new();
        let mut failure = None;
        for result in &composite {
            match &result.outcome {
                RoundTrip::Rejected(code, _) => notes.push(format!(
                    "{}（{}）: {}",
                    result.item,
                    result.item_type.kind_name(),
                    code.as_snake_case()
                )),
                RoundTrip::Matched | RoundTrip::Differed(_) => {
                    failure.get_or_insert(format!(
                        "{}（{}）への書き込みが拒否されませんでした",
                        result.item,
                        result.item_type.kind_name()
                    ));
                }
            }
        }
        let outcome = match failure {
            Some(reason) => Err(reason),
            None => Ok(notes),
        };
        report.record("5.4", title, verified, Mode::Auto, outcome);
    }

    let covered: Vec<String> = results
        .iter()
        .map(|result| result.item_type.kind_name())
        .collect();
    report.observe(
        "item_value_round_trip",
        "公開している各設定項目種別は read から write へ round-trip するか",
        format!(
            "確認できた種別: {} / 網羅のため付与した effect: {}",
            covered.join(", "),
            if added.is_empty() {
                "なし".to_string()
            } else {
                added.join(", ")
            }
        ),
    );
    report.observe(
        "set_effect_item_value_coverage",
        "ハンドル指定の設定値書き込みは公開している全種別で機能するか",
        summarize_write_support(results),
    );
}

/// 種別ごとの書き込み可否をまとめる。
fn summarize_write_support(results: &[TypeResult]) -> String {
    let mut lines = Vec::new();
    for item_type in WRITABLE_ITEM_TYPES {
        let matching: Vec<&TypeResult> = results
            .iter()
            .filter(|result| result.item_type == *item_type)
            .collect();
        if matching.is_empty() {
            lines.push(format!("{}=未確認", item_type.kind_name()));
            continue;
        }
        let rejected = matching
            .iter()
            .find(|result| matches!(result.outcome, RoundTrip::Rejected(_, _)));
        match rejected {
            Some(TypeResult {
                outcome: RoundTrip::Rejected(code, _),
                ..
            }) => lines.push(format!(
                "{}={}",
                item_type.kind_name(),
                code.as_snake_case()
            )),
            _ => lines.push(format!("{}=成功", item_type.kind_name())),
        }
    }
    lines.join(" / ")
}

/// trackbar を持つ項目の値を同じ経路で変更できることを確かめる。
fn check_track_item(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    at: Placement,
) -> Result<CheckResult, String> {
    let object = resolve_object(harness, instance, context.scene_id, at)?;
    let detail = require(
        harness.object(&instance.id, &object.selector),
        "確認対象の詳細を取得できません",
    )?;
    let target = detail.effects.iter().find_map(|effect| {
        effect
            .items
            .iter()
            .find(|item| item.track.is_some() && altered_value(&item.value).is_some())
            .map(|item| (effect.name.clone(), effect.index, item.clone()))
    });
    let Some((effect_name, effect_index, item)) = target else {
        return Ok(Err("trackbar を持つ設定項目が見つかりません".to_string()));
    };
    let next = altered_value(&item.value).expect("書き換えられる値を選んでいる");

    let Some((selector, before)) = locate_item(
        harness,
        instance,
        context,
        at,
        &effect_name,
        effect_index,
        &item.name,
    )?
    else {
        return Ok(Err("trackbar 項目を再取得できません".to_string()));
    };
    let expected = precondition(harness, instance)?;
    if let Err(error) =
        harness.set_object_item(&instance.id, &selector, &item.name, &next, expected)
    {
        return Ok(Err(format!(
            "trackbar 項目を変更できません: {}",
            describe_error(&error)
        )));
    }

    let Some((_, after)) = locate_item(
        harness,
        instance,
        context,
        at,
        &effect_name,
        effect_index,
        &item.name,
    )?
    else {
        return Ok(Err("変更後に trackbar 項目を再取得できません".to_string()));
    };
    if after == before {
        return Ok(Err(format!(
            "{effect_name}.{} の値が変わりませんでした",
            item.name
        )));
    }
    Ok(Ok(vec![format!(
        "{effect_name}.{} を変更できた",
        item.name
    )]))
}

// ---------------------------------------------------------------------------
// 5.5 revision の二重加算
// ---------------------------------------------------------------------------

/// 応答が返した revision をそのまま次の前提条件に使えるかを記録する。
fn section_revision(report: &mut Report, convergence: &Convergence) {
    println!();
    println!("### 5.5 revision の二重加算");

    let doubled = convergence.corrected > 0;
    report.observe(
        "revision_double_increment",
        "ホストが plugin 発の編集に対しても更新イベントを上げ、revision が二重に加算されるか",
        if doubled {
            format!(
                "{} 回中 {} 回で応答の revision が陳腐化し、current_project_revision による訂正で成功した",
                convergence.steps, convergence.corrected
            )
        } else {
            format!(
                "{} 回すべて応答の revision がそのまま次の編集に使えた（二重加算は観測されなかった）",
                convergence.steps
            )
        },
    );

    let outcome = if convergence.corrected == 0 {
        passed_with(format!("{} 回とも 1 回目で成功", convergence.first_attempt))
    } else {
        passed_with(format!(
            "{} 回で訂正を要したが、いずれも current_project_revision による再送で成功",
            convergence.corrected
        ))
    };
    report.record(
        "5.5",
        "応答が返した revision の扱い",
        "応答の project_revision をそのまま expected に使って次の編集が成功する。失敗する場合は details.current_project_revision による訂正で成功する",
        Mode::Auto,
        outcome,
    );
}

// ---------------------------------------------------------------------------
// 5.6 編集がブロックされる状態
// ---------------------------------------------------------------------------

/// 再生中・出力中の編集が型付きのエラーになることを確かめる。
fn section_blocked(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> Result<(), String> {
    println!();
    println!("### 5.6 編集がブロックされる状態");

    for (label, start, stop) in [
        (
            "再生中",
            "再生を開始し、再生し続けたまま Enter を押してください。",
            "再生を停止してから Enter を押してください。",
        ),
        (
            "出力中",
            "出力（エンコード）を開始し、出力中のまま Enter を押してください。\n\
             出力できない場合はそのまま Enter を押し、次の確認で「いいえ」と答えてください。",
            "出力を停止または完了させてから Enter を押してください。",
        ),
    ] {
        let outcome = check_blocked(harness, instance, context, label, start, stop);
        report.record(
            "5.6",
            format!("{label}の編集"),
            format!(
                "{label}の編集要求が edit_blocked になり、AviUtl2 が停止せず、プロジェクトが変更されない。終了後は同じ要求が成功する"
            ),
            Mode::Operator,
            outcome,
        );
    }

    Ok(())
}

/// ブロックされる状態での編集と、解除後の再実行を確かめる。
fn check_blocked(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    label: &str,
    start: &str,
    stop: &str,
) -> CheckResult {
    let before = snapshot(harness, instance, context.scene_id)?;
    prompt(&format!(
        "AviUtl2 のインスタンス {} で、{start}",
        instance.label
    ));

    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    let destination = context.free_slots[0];
    let expected = precondition(harness, instance)?;
    let blocked = harness.move_object(
        &instance.id,
        &object.selector,
        DestinationInput {
            layer: destination.layer as u32,
            frame: destination.frame as u32,
        },
        expected,
    );
    let blocked_note = match &blocked {
        Ok(_) => {
            prompt(&format!(
                "AviUtl2 のインスタンス {}: {stop}",
                instance.label
            ));
            return Err(format!("{label}の編集が成功として返りました"));
        }
        Err(error) if error.code == ErrorCode::EditBlocked => describe_error(error),
        Err(error) => {
            prompt(&format!(
                "AviUtl2 のインスタンス {}: {stop}",
                instance.label
            ));
            return Err(format!(
                "edit_blocked を期待しましたが {}",
                describe_error(error)
            ));
        }
    };

    let after_blocked = snapshot(harness, instance, context.scene_id)?;
    expect_unchanged(&before, &after_blocked)
        .map_err(|reason| format!("ブロックされたのにプロジェクトが変化しました: {reason}"))?;

    prompt(&format!(
        "AviUtl2 のインスタンス {}: {stop}",
        instance.label
    ));

    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    let expected = precondition(harness, instance)?;
    let moved = require(
        harness.move_object(
            &instance.id,
            &object.selector,
            DestinationInput {
                layer: destination.layer as u32,
                frame: destination.frame as u32,
            },
            expected,
        ),
        &format!("{label}の終了後に同じ要求が成功しません"),
    )?;

    // 後始末: 元の位置へ戻す。
    let selector = moved
        .object
        .clone()
        .ok_or_else(|| "移動の応答が対象を返しませんでした".to_string())?
        .selector;
    let epoch = moved.project_epoch.clone();
    with_revision_correction(moved.project_revision, |revision| {
        harness.move_object(
            &instance.id,
            &selector,
            DestinationInput {
                layer: context.target.layer as u32,
                frame: context.target.frame as u32,
            },
            ExpectedInput {
                project_epoch: epoch.clone(),
                project_revision: revision,
            },
        )
    })
    .map_err(|error| format!("元の位置へ戻せません: {}", describe_error(&error)))?;

    Ok(vec![blocked_note, format!("{label}の終了後は成功した")])
}

// ---------------------------------------------------------------------------
// 5.7 対象の取り違え防止
// ---------------------------------------------------------------------------

/// 古い selector による誤適用が拒否されることを確かめる。
fn section_target_confusion(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> Result<(), String> {
    println!();
    println!("### 5.7 対象の取り違え防止");

    let outcome = check_stale_after_ui_item_change(harness, instance, context);
    report.record(
        "5.7",
        "UI で内容を変えた後の古い selector",
        "UI で対象の設定値を変えた後、古い selector での編集が precondition_failed（mismatch=fingerprint）で拒否される",
        Mode::Operator,
        outcome,
    );

    let outcome = check_stale_after_ui_move(harness, instance, context);
    report.record(
        "5.7",
        "UI で対象を移動した後の古い selector",
        "UI で対象を移動した後、古い selector での編集が not_found で拒否される",
        Mode::Operator,
        outcome,
    );

    let outcome = check_stale_after_undo_redo(harness, report, instance, context);
    report.record(
        "5.7",
        "UI で Undo / Redo を行った後の古い selector",
        "UI で Undo / Redo を行った後、古い応答が返した expected と selector での編集が precondition_failed（mismatch=project_revision）で拒否される",
        Mode::Operator,
        outcome,
    );

    let outcome = check_same_name_objects(harness, instance, context);
    report.record(
        "5.7",
        "同名オブジェクトの取り違え",
        "同じレイヤーの別フレームに同名オブジェクトがある状態で、意図した方だけが変更される",
        Mode::Auto,
        outcome,
    );

    Ok(())
}

/// UI で内容を変えた後、内容の差で拒否されることを確かめる。
fn check_stale_after_ui_item_change(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    prompt(&format!(
        "AviUtl2 のインスタンス {} で、レイヤー {} フレーム {} のオブジェクトの設定値を 1 つ変更してください。\n\
         位置と名前は変えないでください。変更したら Enter を押してください。",
        instance.label, context.target.layer, context.target.frame
    ));

    // 前提条件は読み直す。revision の照合を通してから内容の照合へ到達させる。
    let expected = precondition(harness, instance)?;
    let attempt = harness.set_object_name(
        &instance.id,
        &object.selector,
        Some("取り違え確認".to_string()),
        expected,
    );
    let outcome = expect_rejection(attempt, ErrorCode::PreconditionFailed, Some("fingerprint"));

    prompt("UI で行った設定値の変更を取り消してから Enter を押してください。");
    outcome
}

/// UI で対象を移動した後、対象が解決できないことを確かめる。
fn check_stale_after_ui_move(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    prompt(&format!(
        "AviUtl2 のインスタンス {} で、レイヤー {} フレーム {} のオブジェクトを別の位置へ移動してください。\n\
         移動したら Enter を押してください。",
        instance.label, context.target.layer, context.target.frame
    ));

    let expected = precondition(harness, instance)?;
    let attempt = harness.set_object_name(
        &instance.id,
        &object.selector,
        Some("取り違え確認".to_string()),
        expected,
    );
    let outcome = expect_rejection(attempt, ErrorCode::NotFound, None);

    prompt("UI で行った移動を取り消し、元の位置へ戻してから Enter を押してください。");
    outcome
}

/// Undo / Redo で内容が元へ戻った後の古い前提条件が拒否されることを確かめる。
fn check_stale_after_undo_redo(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    let stale_expected = precondition(harness, instance)?;

    prompt(&format!(
        "AviUtl2 のインスタンス {} で、取り消し操作を 1 回行い、続けてやり直し操作を 1 回行ってください。\n\
         内容が元と同一へ戻った状態にしてから Enter を押してください。",
        instance.label
    ));

    let attempt = harness.set_object_name(
        &instance.id,
        &object.selector,
        Some("取り違え確認".to_string()),
        stale_expected,
    );
    let outcome = expect_rejection(
        attempt,
        ErrorCode::PreconditionFailed,
        Some("project_revision"),
    );

    // 前提条件を読み直すと内容の差が無いため受理され得る。これは検出手段が
    // 無いことの記録であり、合否の対象にはしない。
    let fresh = precondition(harness, instance)?;
    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    let retried = harness.set_object_name(
        &instance.id,
        &object.selector,
        Some("取り違え確認".to_string()),
        fresh,
    );
    let accepted = retried.is_ok();
    report.observe(
        "undo_redo_with_fresh_expected",
        "Undo / Redo で内容が元へ戻った後、前提条件を読み直した古い selector は受理されるか",
        match &retried {
            Ok(_) => "受理された（内容が同一に戻るため fingerprint では検出できない）".to_string(),
            Err(error) => format!("拒否された: {}", describe_error(error)),
        },
    );

    // 後始末: 名前を標準名へ戻す。
    if accepted {
        let object = resolve_object(harness, instance, context.scene_id, context.target)?;
        let expected = precondition(harness, instance)?;
        require(
            harness.set_object_name(&instance.id, &object.selector, None, expected),
            "名前を戻せません",
        )?;
    }

    outcome
}

/// 同名オブジェクトのうち意図した方だけが変わることを確かめる。
fn check_same_name_objects(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    let source = resolve_object(harness, instance, context.scene_id, context.target)?;
    let detail = require(
        harness.object(&instance.id, &source.selector),
        "作成元の alias を取得できません",
    )?;
    let alias = detail.alias.clone();
    let slot = context.free_slots[1];

    let expected = precondition(harness, instance)?;
    let first = require(
        harness.create_object(
            &instance.id,
            ObjectSourceInput::ObjectAlias {
                alias: alias.clone(),
            },
            PlacementInput {
                scene_id: context.scene_id,
                layer: slot.layer as u32,
                frame: slot.frame as u32,
            },
            expected,
        ),
        "1 つ目のオブジェクトを作成できません",
    )?;
    let first_object = first
        .object
        .clone()
        .ok_or_else(|| "作成の応答が対象を返しませんでした".to_string())?;
    let second_frame = first_object.frame_end + 1;

    let epoch = first.project_epoch.clone();
    let (second, _) = with_revision_correction(first.project_revision, |revision| {
        harness.create_object(
            &instance.id,
            ObjectSourceInput::ObjectAlias {
                alias: alias.clone(),
            },
            PlacementInput {
                scene_id: context.scene_id,
                layer: slot.layer as u32,
                frame: second_frame as u32,
            },
            ExpectedInput {
                project_epoch: epoch.clone(),
                project_revision: revision,
            },
        )
    })
    .map_err(|error| {
        format!(
            "2 つ目のオブジェクトを作成できません: {}",
            describe_error(&error)
        )
    })?;
    let second_object = second
        .object
        .clone()
        .ok_or_else(|| "作成の応答が対象を返しませんでした".to_string())?;

    let first_at = Placement {
        layer: first_object.layer,
        frame: first_object.frame_start,
    };
    let second_at = Placement {
        layer: second_object.layer,
        frame: second_object.frame_start,
    };
    let before_first = resolve_object(harness, instance, context.scene_id, first_at)?;

    // 2 つ目だけの名前を変える。
    let target = resolve_object(harness, instance, context.scene_id, second_at)?;
    let expected = precondition(harness, instance)?;
    require(
        harness.set_object_name(
            &instance.id,
            &target.selector,
            Some("2 つ目".to_string()),
            expected,
        ),
        "2 つ目の名前を変更できません",
    )?;

    let after_first = resolve_object(harness, instance, context.scene_id, first_at)?;
    let after_second = resolve_object(harness, instance, context.scene_id, second_at)?;
    let intended = after_second.name.as_deref() == Some("2 つ目");
    let untouched = after_first == before_first;

    // 後始末: 作成した 2 件を削除する。
    for at in [second_at, first_at] {
        let object = resolve_object(harness, instance, context.scene_id, at)?;
        let expected = precondition(harness, instance)?;
        require(
            harness.delete_object(&instance.id, &object.selector, expected),
            "作成したオブジェクトを削除できません",
        )?;
    }

    if !intended {
        return Err("意図した方の名前が変わっていません".to_string());
    }
    if !untouched {
        return Err("意図していない方のオブジェクトが変化しました".to_string());
    }
    Ok(vec![format!(
        "layer={} の frame={} と frame={} の同名 2 件のうち、後者だけが変わった",
        slot.layer, first_at.frame, second_at.frame
    )])
}

// ---------------------------------------------------------------------------
// 5.8 その他
// ---------------------------------------------------------------------------

/// 宛先重複・文字種・パス・選択状態・秘匿・シーン切替・切断を確かめる。
fn section_misc(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> Result<(), String> {
    println!();
    println!("### 5.8 その他");

    let outcome = check_destination_occupied(harness, instance, context);
    report.record(
        "5.8",
        "宛先が埋まっている場合",
        "埋まっているレイヤー / フレームへの作成・移動が precondition_failed（destination_occupied）になる",
        Mode::Auto,
        outcome,
    );

    let outcome = check_wide_characters(harness, instance, context);
    report.record(
        "5.8",
        "日本語・絵文字・長い名前",
        "日本語と絵文字を含む長い名前を設定でき、読み直した値が一致する",
        Mode::Auto,
        outcome,
    );

    let outcome = check_rejected_paths(harness, instance, context);
    report.record(
        "5.8",
        "許可されないパス",
        "device path・代替データストリーム・相対パスからの作成が invalid_argument になる",
        Mode::Auto,
        outcome,
    );

    match env_value(UNC_MEDIA_FILE_ENV) {
        Some(path) => {
            let outcome = check_unc_path(harness, instance, context, &path);
            report.record(
                "5.8",
                "ネットワークパス（UNC）",
                "UNC パスのメディアファイルから作成できる",
                Mode::Auto,
                outcome,
            );
        }
        None => report.skip(
            "5.8",
            "ネットワークパス（UNC）",
            "UNC パスのメディアファイルから作成できる",
            Mode::Auto,
            format!("{UNC_MEDIA_FILE_ENV} が設定されていません"),
        ),
    }

    let outcome = check_set_selection(harness, report, instance, context);
    report.record(
        "5.8",
        "カーソル・選択範囲・フォーカスの変更",
        "カーソル・選択範囲・フォーカスが変わり、範囲外の値がクランプされて応答が実際の値を返す",
        Mode::Auto,
        outcome,
    );

    let outcome = check_no_secret_in_response(harness, instance, context);
    report.record(
        "5.8",
        "応答への秘匿値の非混入",
        "編集応答に SDK handle / raw pointer / alias 全文 / 設定値が現れない",
        Mode::Auto,
        outcome,
    );

    let outcome = operator_verdict(
        "plugin のログに SDK handle / raw pointer / alias 全文 / 設定値が現れていませんか。\n\
         開発用ディレクトリの data/log にある最新のログを確認して回答してください。",
    );
    report.record(
        "5.8",
        "ログへの秘匿値の非混入",
        "plugin のログに SDK handle / raw pointer / alias 全文 / 設定値が現れない",
        Mode::Operator,
        outcome,
    );

    section_scene_switch(harness, report, instance, context)?;

    let outcome = check_client_disconnect(harness, instance, context);
    report.record(
        "5.8",
        "クライアント切断",
        "編集要求の送信直後にクライアントを落としても AviUtl2 が停止せず、以降の編集を受け付ける",
        Mode::Operator,
        outcome,
    );

    Ok(())
}

/// 宛先が埋まっている場合の拒否を確かめる。
fn check_destination_occupied(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    let detail = require(
        harness.object(&instance.id, &object.selector),
        "作成元の alias を取得できません",
    )?;

    let expected = precondition(harness, instance)?;
    let created = harness.create_object(
        &instance.id,
        ObjectSourceInput::ObjectAlias {
            alias: detail.alias.clone(),
        },
        PlacementInput {
            scene_id: context.scene_id,
            layer: context.target.layer as u32,
            frame: context.target.frame as u32,
        },
        expected,
    );
    let create_note = expect_destination_occupied(created)?;

    let slot = context.free_slots[1];
    let expected = precondition(harness, instance)?;
    let scratch = require(
        harness.create_object(
            &instance.id,
            ObjectSourceInput::ObjectAlias {
                alias: detail.alias.clone(),
            },
            PlacementInput {
                scene_id: context.scene_id,
                layer: slot.layer as u32,
                frame: slot.frame as u32,
            },
            expected,
        ),
        "確認用オブジェクトを作成できません",
    )?;
    let scratch_object = scratch
        .object
        .clone()
        .ok_or_else(|| "作成の応答が対象を返しませんでした".to_string())?;

    let epoch = scratch.project_epoch.clone();
    let selector = scratch_object.selector.clone();
    let moved = with_revision_correction(scratch.project_revision, |revision| {
        harness.move_object(
            &instance.id,
            &selector,
            DestinationInput {
                layer: context.target.layer as u32,
                frame: context.target.frame as u32,
            },
            ExpectedInput {
                project_epoch: epoch.clone(),
                project_revision: revision,
            },
        )
    });
    let move_note = expect_destination_occupied(moved.map(|(outcome, _)| outcome))?;

    // 後始末: 確認用オブジェクトを削除する。
    let at = Placement {
        layer: scratch_object.layer,
        frame: scratch_object.frame_start,
    };
    let object = resolve_object(harness, instance, context.scene_id, at)?;
    let expected = precondition(harness, instance)?;
    require(
        harness.delete_object(&instance.id, &object.selector, expected),
        "確認用オブジェクトを削除できません",
    )?;

    Ok(vec![
        format!("作成: {create_note}"),
        format!("移動: {move_note}"),
    ])
}

/// 宛先重複としての拒否であることを確かめる。
fn expect_destination_occupied<T>(result: Result<T, ErrorObject>) -> Result<String, String> {
    let error = match result {
        Ok(_) => return Err("拒否されず成功しました".to_string()),
        Err(error) => error,
    };
    if error.code != ErrorCode::PreconditionFailed {
        return Err(format!(
            "precondition_failed を期待しましたが {}",
            describe_error(&error)
        ));
    }
    if detail_str(&error, "reason").as_deref() != Some("destination_occupied") {
        return Err(format!(
            "reason=destination_occupied を期待しましたが {}",
            describe_error(&error)
        ));
    }
    Ok(describe_error(&error))
}

/// 日本語・絵文字を含む長い名前を扱えることを確かめる。
fn check_wide_characters(harness: &Harness, instance: &Instance, context: &Context) -> CheckResult {
    // 上限に近い長さで、日本語と絵文字を混在させる。
    let mut name = "日本語🎬".repeat(aviutl2_mcp_core::MAX_NAME_UTF16_UNITS / 24);
    if name.is_empty() {
        name = "日本語🎬".to_string();
    }
    let units = name.encode_utf16().count();

    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    let expected = precondition(harness, instance)?;
    let renamed = require(
        harness.set_object_name(&instance.id, &object.selector, Some(name.clone()), expected),
        "名前を変更できません",
    )?;
    let applied = renamed
        .object
        .clone()
        .ok_or_else(|| "名前変更の応答が対象を返しませんでした".to_string())?;

    let read_back = resolve_object(harness, instance, context.scene_id, context.target)?;
    let matched = read_back.name.as_deref() == Some(name.as_str());

    // 後始末: 標準名へ戻す。
    let expected = outcome_precondition(&renamed);
    let selector = applied.selector.clone();
    with_revision_correction(expected.project_revision, |revision| {
        harness.set_object_name(
            &instance.id,
            &selector,
            None,
            ExpectedInput {
                project_epoch: expected.project_epoch.clone(),
                project_revision: revision,
            },
        )
    })
    .map_err(|error| format!("名前を戻せません: {}", describe_error(&error)))?;

    if !matched {
        return Err(format!(
            "読み直した名前が一致しません: {:?}",
            read_back.name
        ));
    }
    Ok(vec![format!("{units} UTF-16 単位の名前を往復できた")])
}

/// 許可されないパスが要求の誤りとして拒否されることを確かめる。
fn check_rejected_paths(harness: &Harness, instance: &Instance, context: &Context) -> CheckResult {
    let slot = context.free_slots[1];
    let mut notes = Vec::new();
    for path in [
        r"\\.\pipe\aviutl2",
        r"\\?\C:\movie.mp4",
        r"..\movie.mp4",
        r"C:\movie.mp4:stream",
        "",
    ] {
        let expected = precondition(harness, instance)?;
        let result = harness.create_object(
            &instance.id,
            ObjectSourceInput::MediaFile {
                path: path.to_string(),
            },
            PlacementInput {
                scene_id: context.scene_id,
                layer: slot.layer as u32,
                frame: slot.frame as u32,
            },
            expected,
        );
        match result {
            Ok(_) => return Err(format!("{path:?} からの作成が成功しました")),
            Err(error) if error.code == ErrorCode::InvalidArgument => {
                notes.push(format!("{path:?} は invalid_argument"));
            }
            Err(error) => {
                return Err(format!(
                    "{path:?} は invalid_argument を期待しましたが {}",
                    describe_error(&error)
                ));
            }
        }
    }
    Ok(notes)
}

/// UNC パスからの作成を確かめる。
fn check_unc_path(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    path: &str,
) -> CheckResult {
    let slot = context.free_slots[1];
    let expected = precondition(harness, instance)?;
    let created = require(
        harness.create_object(
            &instance.id,
            ObjectSourceInput::MediaFile {
                path: path.to_string(),
            },
            PlacementInput {
                scene_id: context.scene_id,
                layer: slot.layer as u32,
                frame: slot.frame as u32,
            },
            expected,
        ),
        "UNC パスから作成できません",
    )?;
    let object = created
        .object
        .clone()
        .ok_or_else(|| "作成の応答が対象を返しませんでした".to_string())?;

    let expected = outcome_precondition(&created);
    let selector = object.selector.clone();
    with_revision_correction(expected.project_revision, |revision| {
        harness.delete_object(
            &instance.id,
            &selector,
            ExpectedInput {
                project_epoch: expected.project_epoch.clone(),
                project_revision: revision,
            },
        )
    })
    .map_err(|error| {
        format!(
            "作成したオブジェクトを削除できません: {}",
            describe_error(&error)
        )
    })?;

    Ok(vec![format!(
        "layer={} frame_start={} へ作成できた",
        object.layer, object.frame_start
    )])
}

/// カーソル・選択範囲・フォーカスの変更とクランプを確かめる。
fn check_set_selection(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    let info = require(harness.edit_info(&instance.id), "編集情報を取得できません")?;
    let object = resolve_object(harness, instance, context.scene_id, context.target)?;

    // 範囲内の指定で、3 項目すべてが変わることを見る。
    let expected = precondition(harness, instance)?;
    let state = require(
        harness.set_selection(
            &instance.id,
            context.scene_id,
            SelectionChange {
                cursor: Some(CursorPositionInput {
                    layer: context.target.layer as u32,
                    frame: context.target.frame as u32,
                }),
                selected_range: Some(RangeChangeInput::Set {
                    start: context.target.frame as u32,
                    end: (context.target.frame + 1) as u32,
                }),
                focus: Some(FocusChangeInput::Set {
                    object: object_selector_input(&object.selector),
                }),
            },
            expected,
        ),
        "選択状態を変更できません",
    )?;
    if state.applied.is_empty() {
        return Err("適用できた項目がありません".to_string());
    }

    // 範囲外の指定がクランプされ、応答が実際の値を返すことを見る。
    let beyond = info.extent.frame_max.saturating_add(100_000);
    let expected = precondition(harness, instance)?;
    let clamped = require(
        harness.set_selection(
            &instance.id,
            context.scene_id,
            SelectionChange {
                cursor: Some(CursorPositionInput {
                    layer: context.target.layer as u32,
                    frame: beyond as u32,
                }),
                ..SelectionChange::default()
            },
            expected,
        ),
        "範囲外のカーソル位置を指定できません",
    )?;
    let clamped_frame = clamped.cursor.frame;

    let undo = ask(
        "AviUtl2 で取り消し操作を 1 回行うと、直前のカーソル移動は元へ戻りますか。\n\
         set_selection が取り消し単位を作るかの記録に使います（戻る / 戻らない / 未確認 で回答）。",
    );
    report.observe(
        "set_selection_undo_unit",
        "カーソル・選択範囲・フォーカスの変更は取り消し単位を作るか",
        if undo.is_empty() {
            "未回答".to_string()
        } else {
            undo
        },
    );

    // 後始末: カーソルと選択範囲を元へ戻す。
    let expected = precondition(harness, instance)?;
    let _ = harness.set_selection(
        &instance.id,
        context.scene_id,
        SelectionChange {
            cursor: Some(CursorPositionInput {
                layer: info.cursor.layer as u32,
                frame: info.cursor.frame as u32,
            }),
            selected_range: Some(RangeChangeInput::Clear {}),
            focus: Some(FocusChangeInput::Clear {}),
        },
        expected,
    );

    if clamped_frame >= beyond {
        return Err(format!(
            "範囲外のフレーム {beyond} がクランプされずに {clamped_frame} として返りました"
        ));
    }
    Ok(vec![
        format!(
            "適用={:?} 未適用={:?} observed_after_edit={}",
            state.applied, state.not_applied, state.observed_after_edit
        ),
        format!("frame={beyond} の指定が {clamped_frame} へクランプされた"),
    ])
}

/// 編集応答へ秘匿値が現れないことを確かめる。
fn check_no_secret_in_response(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    let source = resolve_object(harness, instance, context.scene_id, context.target)?;
    let detail = require(
        harness.object(&instance.id, &source.selector),
        "作成元の alias を取得できません",
    )?;
    let alias = detail.alias.clone();
    let slot = context.free_slots[1];

    let expected = precondition(harness, instance)?;
    let created = require(
        harness.create_object(
            &instance.id,
            ObjectSourceInput::ObjectAlias {
                alias: alias.clone(),
            },
            PlacementInput {
                scene_id: context.scene_id,
                layer: slot.layer as u32,
                frame: slot.frame as u32,
            },
            expected,
        ),
        "確認用オブジェクトを作成できません",
    )?;
    let mut leaks = Vec::new();
    let raw = harness.last_raw();
    if raw.contains(&alias) {
        leaks.push("作成の応答に alias 全文が現れた".to_string());
    }
    for forbidden in ["handle", "0x", "secret", "nonce", "pointer"] {
        if raw.to_lowercase().contains(forbidden) {
            leaks.push(format!("作成の応答に {forbidden} が現れた"));
        }
    }

    let object = created
        .object
        .clone()
        .ok_or_else(|| "作成の応答が対象を返しませんでした".to_string())?;
    let at = Placement {
        layer: object.layer,
        frame: object.frame_start,
    };

    // 設定値を書き換えられる項目があれば、その値が応答へ出ないことも見る。
    let secret = "秘匿確認用テキスト🎬";
    let detail = require(
        harness.object(&instance.id, &object.selector),
        "確認用オブジェクトの詳細を取得できません",
    )?;
    let text_item = detail.effects.iter().find_map(|effect| {
        effect
            .items
            .iter()
            .find(|item| {
                matches!(
                    item.item_type,
                    EffectItemType::Text | EffectItemType::String
                )
            })
            .map(|item| (effect.selector.clone(), item.name.clone()))
    });
    let mut checked_value = false;
    if let Some((selector, item_name)) = text_item {
        let expected = precondition(harness, instance)?;
        let value = ItemValue::Text {
            value: secret.to_string(),
        };
        if harness
            .set_object_item(&instance.id, &selector, &item_name, &value, expected)
            .is_ok()
        {
            checked_value = true;
            if harness.last_raw().contains(secret) {
                leaks.push("設定値の変更の応答に設定値が現れた".to_string());
            }
        }
    }

    // 後始末: 確認用オブジェクトを削除する。
    let object = resolve_object(harness, instance, context.scene_id, at)?;
    let expected = precondition(harness, instance)?;
    require(
        harness.delete_object(&instance.id, &object.selector, expected),
        "確認用オブジェクトを削除できません",
    )?;

    if !leaks.is_empty() {
        return Err(leaks.join(" / "));
    }
    Ok(vec![format!(
        "alias の非混入を確認。設定値の非混入の確認={}",
        if checked_value {
            "実施"
        } else {
            "対象項目なし"
        }
    )])
}

/// シーン切替と編集の競合を確かめる。
fn section_scene_switch(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> Result<(), String> {
    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    let answer = ask(&format!(
        "AviUtl2 のインスタンス {} で、別のシーンへ切り替えてから Enter を押してください。\n\
         シーンが 1 つしか無く切り替えられない場合は skip と入力してください。",
        instance.label
    ));
    if answer.eq_ignore_ascii_case("skip") {
        for (title, verified) in [
            (
                "シーン切替後の編集",
                "切替前の selector での編集が precondition_failed（mismatch=scene_id）で拒否される",
            ),
            (
                "シーン切替後の選択状態の変更",
                "切替前の expected_scene_id での set_selection が precondition_failed（mismatch=scene_id）で拒否される",
            ),
        ] {
            report.skip(
                "5.8",
                title,
                verified,
                Mode::Operator,
                "シーンを切り替えられないため実施できません",
            );
        }
        return Ok(());
    }

    let expected = precondition(harness, instance)?;
    let attempt = harness.set_object_name(
        &instance.id,
        &object.selector,
        Some("シーン確認".to_string()),
        expected,
    );
    let outcome = expect_rejection(attempt, ErrorCode::PreconditionFailed, Some("scene_id"));
    report.record(
        "5.8",
        "シーン切替後の編集",
        "切替前の selector での編集が precondition_failed（mismatch=scene_id）で拒否される",
        Mode::Operator,
        outcome,
    );

    let expected = precondition(harness, instance)?;
    let attempt = harness.set_selection(
        &instance.id,
        context.scene_id,
        SelectionChange {
            cursor: Some(CursorPositionInput { layer: 0, frame: 0 }),
            ..SelectionChange::default()
        },
        expected,
    );
    let outcome = expect_rejection(attempt, ErrorCode::PreconditionFailed, Some("scene_id"));
    report.record(
        "5.8",
        "シーン切替後の選択状態の変更",
        "切替前の expected_scene_id での set_selection が precondition_failed（mismatch=scene_id）で拒否される",
        Mode::Operator,
        outcome,
    );

    prompt("元のシーンへ戻してから Enter を押してください。");
    Ok(())
}

/// クライアント切断で AviUtl2 が停止しないことを確かめる。
fn check_client_disconnect(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    prompt(&format!(
        "別のコンソールから MCP クライアント（server 実行ファイル）を起動し、インスタンス {} への\n\
         編集 tool を呼び出した直後にそのプロセスを強制終了してください。終わったら Enter を押してください。\n\
         実施できない場合はそのまま Enter を押し、次の確認で「いいえ」と答えてください。",
        instance.label
    ));

    // AviUtl2 が応答し続け、以降の編集も受け付けることを自動で確かめる。
    let object = resolve_object(harness, instance, context.scene_id, context.target)?;
    let expected = precondition(harness, instance)?;
    let renamed = require(
        harness.set_object_name(
            &instance.id,
            &object.selector,
            Some("切断確認".to_string()),
            expected,
        ),
        "切断後に編集を受け付けません",
    )?;
    let selector = renamed
        .object
        .clone()
        .ok_or_else(|| "名前変更の応答が対象を返しませんでした".to_string())?
        .selector;
    let epoch = renamed.project_epoch.clone();
    with_revision_correction(renamed.project_revision, |revision| {
        harness.set_object_name(
            &instance.id,
            &selector,
            None,
            ExpectedInput {
                project_epoch: epoch.clone(),
                project_revision: revision,
            },
        )
    })
    .map_err(|error| format!("名前を戻せません: {}", describe_error(&error)))?;

    if !confirm("plugin のログに、切断したクライアントへの応答送信の失敗が記録されていますか。")
    {
        return Err("切断時の送信失敗がログに残っていないと回答された".to_string());
    }
    Ok(vec!["切断後も読み取りと編集を受け付けた".to_string()])
}

// ---------------------------------------------------------------------------
// 6. 完了条件の検証手順
// ---------------------------------------------------------------------------

/// 対象 instance だけが変更され、stale selector による変更が常に拒否されることを確かめる。
///
/// 拒否されたことだけを見ると、3 つのガードのうち 1 つしか働いていなくても全手順が
/// 合格し得る。手順ごとに `details.mismatch` の期待値まで固定する。
fn section_completion(
    harness: &Harness,
    report: &mut Report,
    a: &Instance,
    b: &Instance,
) -> Result<(), String> {
    println!();
    println!("### 6 完了条件の検証手順");

    let scene_a = scene_id(harness, a)?;
    let scene_b = scene_id(harness, b)?;

    // 手順 3: 対象と前提条件を得る。
    let before_a = snapshot(harness, a, scene_a)?;
    let target = before_a
        .first()
        .cloned()
        .ok_or_else(|| "インスタンス A の現在シーンにオブジェクトがありません".to_string())?;
    let stale_expected = precondition(harness, a)?;
    let stale_selector = target.selector.clone();
    let home = Placement {
        layer: target.layer,
        frame: target.frame_start,
    };
    let destination = free_slot(harness, a, scene_a)?;
    let before_b = snapshot(harness, b, scene_b)?;

    report.record(
        "6.3",
        "対象と前提条件の取得",
        "instance A の列挙と詳細取得で selector と project_revision が得られる",
        Mode::Auto,
        passed_with(format!(
            "layer={} frame={} revision={}",
            target.layer, target.frame_start, stale_expected.project_revision
        )),
    );

    // 手順 4: A を実際に編集する。
    let moved = require(
        harness.move_object(
            &a.id,
            &stale_selector,
            DestinationInput {
                layer: destination.layer as u32,
                frame: destination.frame as u32,
            },
            stale_expected.clone(),
        ),
        "instance A の移動に失敗しました",
    )?;
    let fresh_selector = moved
        .object
        .clone()
        .ok_or_else(|| "移動の応答が対象を返しませんでした".to_string())?
        .selector;
    let after_move_a = snapshot(harness, a, scene_a)?;
    report.record(
        "6.4",
        "対象 instance の編集",
        "instance A の aviutl2_move_object が成功する",
        Mode::Auto,
        passed_with(format!(
            "layer={} frame={} へ移動した",
            destination.layer, destination.frame
        )),
    );

    // 手順 5: B が変わっていないことを見る。
    let now_b = snapshot(harness, b, scene_b)?;
    let unchanged = expect_unchanged(&before_b, &now_b)
        .map(|()| vec!["列挙結果に差が無い".to_string()])
        .map_err(|reason| format!("instance B が変化しました: {reason}"));
    let outcome = match (
        unchanged,
        confirm(&format!(
            "AviUtl2 のインスタンス {} のプロジェクトは変更されていませんか（UI で確認してください）。",
            b.label
        )),
    ) {
        (Ok(mut notes), true) => {
            notes.push("UI 上も変更されていないことを実行者が確認した".to_string());
            Ok(notes)
        }
        (Ok(_), false) => Err("UI 上で instance B が変更されたと回答された".to_string()),
        (Err(reason), _) => Err(reason),
    };
    report.record(
        "6.5",
        "他 instance の非変更",
        "instance A を編集しても instance B のプロジェクトが変更されない",
        Mode::Operator,
        outcome,
    );

    // 手順 6: A の selector を B へ渡す。epoch で拒否されなければならない。
    let expected_b = ExpectedInput {
        project_epoch: stale_expected.project_epoch.clone(),
        project_revision: stale_expected.project_revision,
    };
    let attempt = harness.move_object(
        &b.id,
        &stale_selector,
        DestinationInput {
            layer: destination.layer as u32,
            frame: destination.frame as u32,
        },
        expected_b,
    );
    let mut outcome = expect_rejection(
        attempt,
        ErrorCode::PreconditionFailed,
        Some("project_epoch"),
    );
    if outcome.is_ok() {
        let now_b = snapshot(harness, b, scene_b)?;
        outcome = match expect_unchanged(&before_b, &now_b) {
            Ok(()) => outcome.map(|mut notes| {
                notes.push("instance B は変化していない".to_string());
                notes
            }),
            Err(reason) => Err(format!(
                "拒否されたのに instance B が変化しました: {reason}"
            )),
        };
    }
    report.record(
        "6.6",
        "別 instance の selector",
        "instance A の selector を instance B の aviutl2_move_object へ渡すと precondition_failed（mismatch=project_epoch）で拒否され、instance B が変更されない",
        Mode::Auto,
        outcome,
    );

    // 手順 7: 古くなった selector と前提条件を A へ渡す。revision で拒否される。
    let attempt = harness.move_object(
        &a.id,
        &stale_selector,
        DestinationInput {
            layer: home.layer as u32,
            frame: home.frame as u32,
        },
        stale_expected,
    );
    let outcome = expect_rejection(
        attempt,
        ErrorCode::PreconditionFailed,
        Some("project_revision"),
    );
    report.record(
        "6.7",
        "古い selector の再利用",
        "instance A に対し古くなった selector で再度編集すると precondition_failed（mismatch=project_revision）で拒否される",
        Mode::Auto,
        outcome,
    );

    // 手順 8: UI で内容を変えた後、手順 4 の selector が内容の差で拒否される。
    prompt(&format!(
        "AviUtl2 のインスタンス {} で、レイヤー {} フレーム {} のオブジェクトの設定値を 1 つ変更してください。\n\
         位置と名前は変えないでください。変更したら Enter を押してください。",
        a.label, destination.layer, destination.frame
    ));
    let expected = precondition(harness, a)?;
    let attempt = harness.move_object(
        &a.id,
        &fresh_selector,
        DestinationInput {
            layer: home.layer as u32,
            frame: home.frame as u32,
        },
        expected,
    );
    let outcome = expect_rejection(attempt, ErrorCode::PreconditionFailed, Some("fingerprint"));
    report.record(
        "6.8",
        "UI 編集後の selector",
        "UI で対象を編集した後、編集の応答が返した selector での編集が precondition_failed（mismatch=fingerprint）で拒否される",
        Mode::Auto,
        outcome,
    );

    // 手順 9: 手順 8 の UI 編集を戻したうえで、手順 4 の変更が 1 回の取り消しで戻る。
    prompt(&format!(
        "AviUtl2 のインスタンス {} で、いま行った設定値の変更だけを取り消してから Enter を押してください。",
        a.label
    ));
    let restored = snapshot(harness, a, scene_a)?;
    let outcome = match expect_unchanged(&after_move_a, &restored) {
        Ok(()) => {
            prompt(&format!(
                "続けて AviUtl2 のインスタンス {} で、取り消し操作を **1 回だけ** 行ってから Enter を押してください。",
                a.label
            ));
            let after_undo = snapshot(harness, a, scene_a)?;
            expect_unchanged(&before_a, &after_undo)
                .map(|()| vec!["1 回の取り消しで移動前の状態へ戻った".to_string()])
                .map_err(|reason| format!("1 回の取り消しで元へ戻りませんでした: {reason}"))
        }
        Err(reason) => Err(format!(
            "設定値の変更を取り消した状態が手順 4 直後と一致しません: {reason}"
        )),
    };
    report.record(
        "6.9",
        "編集の取り消し",
        "手順 4 の変更が 1 回の取り消し操作で元へ戻る",
        Mode::Operator,
        outcome,
    );

    Ok(())
}

/// 空きレイヤーの先頭フレームを 1 つ選ぶ。
fn free_slot(harness: &Harness, instance: &Instance, scene_id: i32) -> Result<Placement, String> {
    let layers = require(
        harness.layers(&instance.id, scene_id),
        "レイヤーを列挙できません",
    )?;
    layers
        .iter()
        .find(|layer| layer.object_count == 0 && !layer.locked)
        .map(|layer| Placement {
            layer: layer.index,
            frame: 0,
        })
        .ok_or_else(|| format!("インスタンス {} に空きレイヤーがありません", instance.label))
}
