//! AviUtl2 実機を用いた一括適用の受け入れ確認。
//!
//! # 警告: 本ターゲットは破壊的である
//!
//! 実行するとオブジェクトの移動と設定値の書き換えを実際に行い、確認のために
//! オブジェクトの作成と削除も行う。**必ず固定サンプルプロジェクトの複製に対して
//! 実行し、原本は開かないこと。** 本ターゲットは開いているプロジェクトが複製で
//! あることを実行者へ確認し、複製であると答えられない限り実行しない。終了後は
//! 保存せずに AviUtl2 を閉じること。
//!
//! # 何を確かめるか
//!
//! 一括適用は「1 回の呼び出しの全体が 1 つの取り消し単位になる」ことと「途中で
//! 失敗したらそれまでに適用した変更を自分で巻き戻す」ことを約束している。
//! どちらもホストの取り消し履歴と実際のプロジェクトの上でしか確かめられない。
//!
//! # 最初に確かめる前提
//!
//! 編集区間の内側での列挙が、同じ区間の中で先に行った変更を反映するかどうかを
//! 最初に確かめる。適用時点での宛先の確認も、応答へ載せる対象の読み直しも、
//! 巻き戻したことの確かめ直しも、すべてこれに依っている。反映されないなら
//! 一括適用は根本から成立しないため、崩れた時点で以降を打ち切る。
//!
//! # レイヤー番号の表し方
//!
//! MCP のレイヤー番号は 0 始まりであり、AviUtl2 の UI は 1 始まりで表示する。
//! 番号だけを伝えると実行者は 1 つ隣のレイヤーを見るため、実行者へ出す文面では
//! [`layer_label`] を通して UI 上の見え方を併記する。
//!
//! # 準備
//!
//! 1. `au2 develop` で plugin と server を配置し、AviUtl2 から plugin が読み込まれる
//!    状態にする。
//! 2. 固定サンプルプロジェクトの**複製を 2 つ**用意する。2 つは同一内容の複製で
//!    なければならない。同位置オブジェクトの fingerprint が一致する構成でなければ、
//!    別インスタンスのセレクターを拒否する確認が fingerprint の確認にすり替わる。
//! 3. AviUtl2 を **2 プロセス**起動し、それぞれで別の複製を開く。
//! 4. サンプルプロジェクトは次を満たすこと。
//!    - 現在シーンに、ロックされていないレイヤー上のオブジェクトが 2 つ以上ある。
//!    - そのうち少なくとも 1 つが、値を書き換えられる設定項目を持つ。
//!    - オブジェクトを 1 つも置いていない空きレイヤーが 3 つ以上ある。
//!
//! # 実行方法
//!
//! ```text
//! cargo run -p aviutl2-mcp-server --example live_batch_acceptance
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
    BatchOutcome, EditInfo, EditOutcome, EffectItemType, EffectSelector, ErrorCode, ErrorObject,
    ItemValue, LayerInfo, MAX_BATCH_OPERATIONS, ObjectDetail, ObjectSelector, ObjectSummary,
};
use aviutl2_mcp_server::api::ListInstancesResponse;
use aviutl2_mcp_server::discovery::default_registry_dir;
use aviutl2_mcp_server::mcp::edit_input::{
    ApplyBatchInput, BatchOperationInput, CreateObjectInput, DeleteObjectInput, DestinationInput,
    EffectSelectorInput, ItemValueInput, MoveObjectInput, ObjectSourceInput, PlacementInput,
    SetObjectItemInput, SetObjectNameInput,
};
use aviutl2_mcp_server::mcp::input::{
    GetObjectInput, InstanceInput, ListInstancesInput, ListLayersInput, ListObjectsInput,
    ObjectFilterInput, ObjectSelectorInput, PageInput,
};
use aviutl2_mcp_server::mcp::{AviUtl2McpServer, CallLimits, REGISTRY_DIR_ENV};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use serde::de::DeserializeOwned;
use std::cell::RefCell;
use std::collections::HashSet;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 確認に要する同時起動プロセス数。
const REQUIRED_INSTANCES: usize = 2;
/// インスタンスが揃うまで待つ上限。
const READY_TIMEOUT: Duration = Duration::from_secs(180);
/// 一覧の再取得間隔。
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// 一覧取得の 1 ページあたり件数。
const PAGE_LIMIT: u32 = 200;
/// 一覧取得で辿るページ数の上限。
const MAX_PAGES: usize = 20;
/// 費用の観測で組み立てる sub-operation の上限。
const COST_OPERATION_TARGET: usize = MAX_BATCH_OPERATIONS;
/// 日本語と絵文字を含む設定値の確認に用いる文字列。
const WIDE_TEXT: &str = "一括適用の日本語🎬確認";
/// 秘匿の確認に用いる、応答へ現れてはならない設定値。
const SECRET_TEXT_FIRST: &str = "秘匿確認用の元値🎬";
/// 同じく、書き込む側の値。
const SECRET_TEXT_SECOND: &str = "秘匿確認用の設定値🎬";
/// 日本語と絵文字を含むオブジェクト名。
const WIDE_NAME: &str = "一括適用の対象🎬";

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

    let harness = Harness::new(registry_dir);
    let (a, b) = prepare(&harness, report)?;
    let context = Context::new(&harness, &a)?;

    // 前提が崩れたら以降は全て同じ理由で落ち、どこで壊れたのかが読めなくなる。
    // 崩れを 1 件の不合格として記録し、以降は未実施として残す。
    let mut premise = Premise::intact();
    section_inner_read_back(&harness, report, &a, &context, &mut premise);

    let guard = SectionGuard {
        harness: &harness,
        instance: &a,
        context: &context,
    };
    guard.run(report, &mut premise, "基本", |report| {
        section_basic(&harness, report, &a, &context)
    });
    guard.run(report, &mut premise, "取り消し", |report| {
        section_undo(&harness, report, &a, &context)
    });
    guard.run(report, &mut premise, "巻き戻し", |report| {
        section_rollback(&harness, report, &a, &context)
    });
    guard.run(report, &mut premise, "費用", |report| {
        section_cost(&harness, report, &a, &context)
    });
    guard.run(report, &mut premise, "その他", |report| {
        section_misc(&harness, report, &a, &b, &context)
    });

    prompt(
        "いま行ったこと: すべての確認が終わりました。プロジェクトは書き換えたままです。\n\
         お願いすること: AviUtl2 を 2 つとも、保存せずに閉じてください。\n\
         回答: 閉じたら Enter を押してください。判定の一覧を表示します。",
    );
    Ok(())
}

/// 破壊的であることを実行前に告げる。
fn print_destructive_warning() {
    println!();
    println!("警告: このプログラムは AviUtl2 で開いているプロジェクトを実際に書き換えます。");
    println!("      オブジェクトの移動と設定値の書き換えを、まとめて何度も行います。");
    println!("      確認のためにオブジェクトの作成と削除も行います。");
    println!("      開いてよいのは固定サンプルプロジェクトの複製だけです。");
    println!("      原本を開いている場合は、ここで実行を中止してください。");
    println!("      終了後は保存せずに AviUtl2 を閉じてください。");
    println!();
    println!("レイヤー番号: この画面が出す「レイヤー N」は MCP の番号で、0 から数えます。");
    println!("              AviUtl2 の UI は 1 から数えて表示するため、UI 上の表示を併記します。");
    println!("              例: {}", layer_label(1));
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
// 前提の見張り
// ---------------------------------------------------------------------------

/// 前提の崩れを記録する見出し。
const PREMISE_SECTION: &str = "前提";

/// 以降の確認が依って立つ前提が保たれているか。
///
/// 前提は 2 つ。同じ区間の中で行った変更が読み直しに現れることと、主たる 2 つの
/// 対象を元の位置へ読み直せることである。どちらかが崩れたまま先へ進むと、以降の
/// 確認は全て同じ理由で落ち、どこで壊れたのかが読めなくなる。崩れた時点で
/// 打ち切り、壊した確認の名を添えて 1 件の不合格として残す。
struct Premise {
    /// 前提を壊した確認または区間の呼び名。保たれていれば `None`。
    broken_by: Option<String>,
    /// 崩れ方。
    reason: String,
}

impl Premise {
    /// 前提が保たれている状態。
    fn intact() -> Self {
        Self {
            broken_by: None,
            reason: String::new(),
        }
    }

    fn is_broken(&self) -> bool {
        self.broken_by.is_some()
    }

    /// 実施を見送る理由。
    fn skip_reason(&self) -> String {
        match &self.broken_by {
            Some(who) => format!("{who} が前提を壊したため実施しません: {}", self.reason),
            None => String::new(),
        }
    }

    /// 前提が壊れたことを、壊した者の名を添えて記録する。
    fn break_with(&mut self, report: &mut Report, who: String, reason: String) {
        if self.is_broken() {
            return;
        }
        report.record(
            PREMISE_SECTION,
            format!("{who} の後の前提"),
            "主たる 2 つの対象を元の位置へ読み直せ、現在シーンが変わっていない",
            Mode::Auto,
            Err(reason.clone()),
        );
        self.broken_by = Some(who);
        self.reason = reason;
    }

    /// 直前に実行した確認の後で、前提が保たれているかを見る。
    fn verify_after(
        &mut self,
        harness: &Harness,
        report: &mut Report,
        instance: &Instance,
        context: &Context,
        who: String,
    ) {
        if self.is_broken() {
            return;
        }
        let Err(reason) = check_premise(harness, instance, context) else {
            return;
        };
        self.break_with(report, who, reason);
    }
}

/// 前提が保たれているかを確かめる。
fn check_premise(harness: &Harness, instance: &Instance, context: &Context) -> Result<(), String> {
    let info = require(harness.edit_info(&instance.id), "編集情報を取得できません")?;
    if info.scene.id != context.scene_id {
        return Err(format!(
            "現在シーンが {} から {} へ変わっています",
            context.scene_id, info.scene.id
        ));
    }
    read_object(harness, instance, context.scene_id, context.first)?;
    read_object(harness, instance, context.scene_id, context.second)?;
    Ok(())
}

/// 前提を見張りながら区間を実行する役。
struct SectionGuard<'a> {
    harness: &'a Harness,
    instance: &'a Instance,
    context: &'a Context,
}

impl SectionGuard<'_> {
    /// 前提が保たれている間だけ区間を実行し、実行後に前提を検分する。
    fn run<F>(&self, report: &mut Report, premise: &mut Premise, section: &'static str, body: F)
    where
        F: FnOnce(&mut Report) -> Result<(), String>,
    {
        if premise.is_broken() {
            report.skip(
                section,
                format!("区間「{section}」の実行"),
                "区間の全項目を最後まで実行できる",
                Mode::Auto,
                premise.skip_reason(),
            );
            return;
        }
        if let Err(reason) = body(report) {
            report.record(
                section,
                format!("区間「{section}」の実行"),
                "区間の全項目を最後まで実行できる",
                Mode::Auto,
                Err(reason),
            );
        }
        premise.verify_after(
            self.harness,
            report,
            self.instance,
            self.context,
            format!("区間「{section}」"),
        );
    }
}

// ---------------------------------------------------------------------------
// 対話
// ---------------------------------------------------------------------------

/// レイヤー番号を、AviUtl2 の UI 上の見え方を添えて表す。
///
/// MCP のレイヤー番号は 0 始まりであり、AviUtl2 の UI は 1 始まりで表示する。
/// 番号だけを伝えると、実行者は 1 つ隣のレイヤーを見る。
fn layer_label(layer: usize) -> String {
    format!("レイヤー {layer}（UI の表示では Layer{}）", layer + 1)
}

/// 位置を、UI 上の見え方を添えて表す。
fn placement_label(at: Placement) -> String {
    format!("{} のフレーム {}", layer_label(at.layer), at.frame)
}

/// 区間の始まりと、その区間が何をするかを実行者へ告げる。
fn print_section(title: &str, doing: &str) {
    println!();
    println!("=== {title} ===");
    println!("これから行うこと: {doing}");
}

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
        passed_with("実行者が y と回答した")
    } else {
        Err("実行者が n と回答した".to_string())
    }
}

// ---------------------------------------------------------------------------
// tool 呼び出し
// ---------------------------------------------------------------------------

/// MCP tool を実機のインスタンスへ発行する実行環境。
///
/// tool は非同期であるため、専用のランタイム上で 1 件ずつ完了まで待つ。
/// 描画を行わないため成果物の保管庫は開かない。
struct Harness {
    server: AviUtl2McpServer,
    runtime: tokio::runtime::Runtime,
    /// 直近の tool result を文字どおりに保持する。応答へ秘匿値が現れないことの
    /// 確認は、DTO へ写した後ではなく実際に返した文字列に対して行う。
    last_raw: RefCell<String>,
}

impl Harness {
    fn new(registry_dir: PathBuf) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("現在スレッドのランタイムを作成できる");
        Self {
            server: AviUtl2McpServer::without_artifact_store(registry_dir, CallLimits::default()),
            runtime,
            last_raw: RefCell::new(String::new()),
        }
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

    fn apply_batch(
        &self,
        instance: &str,
        operations: Vec<BatchOperationInput>,
    ) -> Result<BatchOutcome, ErrorObject> {
        let result = self
            .runtime
            .block_on(self.server.aviutl2_apply_batch(Parameters(ApplyBatchInput {
                instance_id: instance.to_string(),
                operations,
            })));
        self.decode(result)
    }

    fn move_object(
        &self,
        instance: &str,
        selector: &ObjectSelector,
        destination: Placement,
    ) -> Result<EditOutcome, ErrorObject> {
        let result = self
            .runtime
            .block_on(self.server.aviutl2_move_object(Parameters(MoveObjectInput {
                instance_id: instance.to_string(),
                selector: object_selector_input(selector),
                destination: destination_input(destination),
            })));
        self.decode(result)
    }

    fn set_object_item(
        &self,
        instance: &str,
        selector: &EffectSelector,
        item: &str,
        value: &ItemValue,
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
                    })),
            );
        self.decode(result)
    }

    fn set_object_name(
        &self,
        instance: &str,
        selector: &ObjectSelector,
        name: Option<String>,
    ) -> Result<EditOutcome, ErrorObject> {
        let result = self
            .runtime
            .block_on(
                self.server
                    .aviutl2_set_object_name(Parameters(SetObjectNameInput {
                        instance_id: instance.to_string(),
                        selector: object_selector_input(selector),
                        name,
                    })),
            );
        self.decode(result)
    }

    fn create_object(
        &self,
        instance: &str,
        alias: &str,
        at: Placement,
        scene_id: i32,
        expected_project_epoch: String,
    ) -> Result<EditOutcome, ErrorObject> {
        let result = self
            .runtime
            .block_on(
                self.server
                    .aviutl2_create_object(Parameters(CreateObjectInput {
                        instance_id: instance.to_string(),
                        source: ObjectSourceInput::ObjectAlias {
                            alias: alias.to_string(),
                        },
                        placement: PlacementInput {
                            scene_id,
                            layer: at.layer as u32,
                            frame: at.frame as u32,
                        },
                        expected_project_epoch,
                    })),
            );
        self.decode(result)
    }

    fn delete_object(
        &self,
        instance: &str,
        selector: &ObjectSelector,
    ) -> Result<EditOutcome, ErrorObject> {
        let result = self
            .runtime
            .block_on(
                self.server
                    .aviutl2_delete_object(Parameters(DeleteObjectInput {
                        instance_id: instance.to_string(),
                        selector: object_selector_input(selector),
                    })),
            );
        self.decode(result)
    }
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
    }
}

/// effect セレクターを tool の入力形式へ写す。
fn effect_selector_input(selector: &EffectSelector) -> EffectSelectorInput {
    EffectSelectorInput {
        object: object_selector_input(&selector.object),
        effect_name: selector.effect_name.clone(),
        effect_index: selector.effect_index as u32,
        fingerprint: selector.fingerprint.to_string(),
    }
}

/// 位置を移動先の入力形式へ写す。
fn destination_input(at: Placement) -> DestinationInput {
    DestinationInput {
        layer: at.layer as u32,
        frame: at.frame as u32,
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

/// 移動の sub-operation を組み立てる。
fn move_op(selector: &ObjectSelector, to: Placement) -> BatchOperationInput {
    BatchOperationInput::MoveObject {
        selector: object_selector_input(selector),
        destination: destination_input(to),
    }
}

/// 設定値の sub-operation を組み立てる。
fn item_op(selector: &EffectSelector, item: &str, value: &ItemValue) -> BatchOperationInput {
    BatchOperationInput::SetObjectItem {
        selector: effect_selector_input(selector),
        item: item.to_string(),
        value: item_value_input(value),
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

/// レイヤーと開始フレームの組。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placement {
    layer: usize,
    frame: usize,
}

/// 全区間で使い回す対象の情報。
struct Context {
    /// 開始時点の現在シーン ID。
    scene_id: i32,
    /// 主たる対象の位置。値を書き換えられる設定項目を持つ。
    first: Placement,
    /// 入れ替えの相手の位置。
    second: Placement,
    /// 空きレイヤーの先頭フレーム。
    free_slots: Vec<Placement>,
}

impl Context {
    fn new(harness: &Harness, instance: &Instance) -> Result<Self, String> {
        let scene_id = scene_id(harness, instance)?;
        let layers = require(
            harness.layers(&instance.id, scene_id),
            "レイヤーを列挙できません",
        )?;
        let locked: HashSet<usize> = layers
            .iter()
            .filter(|layer| layer.locked)
            .map(|layer| layer.index)
            .collect();

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
                "オブジェクトが 1 つも無くロックもされていないレイヤーが {} 個しかありません。3 個以上あるサンプルプロジェクトを使ってください。",
                free_slots.len()
            ));
        }

        let objects: Vec<ObjectSummary> = require(
            harness.objects(&instance.id, scene_id, None),
            "オブジェクトを列挙できません",
        )?
        .into_iter()
        .filter(|object| !locked.contains(&object.layer))
        .collect();
        if objects.len() < 2 {
            return Err(format!(
                "ロックされていないレイヤー上のオブジェクトが {} 個しかありません。2 個以上あるサンプルプロジェクトを使ってください。",
                objects.len()
            ));
        }

        // 主たる対象は、値を書き換えられる設定項目を持つものから選ぶ。3 件の
        // 一括適用には設定値の変更が 1 件含まれるため、それを載せられる対象が要る。
        let first = objects
            .iter()
            .map(placement_of)
            .find(|at| alterable(harness, instance, scene_id, *at).is_ok())
            .ok_or_else(|| {
                "値を書き換えられる設定項目を持つオブジェクトがありません。".to_string()
            })?;
        let second = objects
            .iter()
            .map(placement_of)
            .find(|at| *at != first)
            .ok_or_else(|| "入れ替えの相手にできるオブジェクトがありません。".to_string())?;

        println!();
        println!("主に使うオブジェクト: {}", placement_label(first));
        println!("入れ替えの相手: {}", placement_label(second));
        println!(
            "オブジェクトが 1 つも無いレイヤー: {}",
            free_slots
                .iter()
                .map(|slot| layer_label(slot.layer))
                .collect::<Vec<_>>()
                .join(" / ")
        );

        Ok(Self {
            scene_id,
            first,
            second,
            free_slots,
        })
    }
}

/// オブジェクトの位置を取り出す。
fn placement_of(object: &ObjectSummary) -> Placement {
    Placement {
        layer: object.layer,
        frame: object.frame_start,
    }
}

/// 現在シーン ID を読み直す。
fn scene_id(harness: &Harness, instance: &Instance) -> Result<i32, String> {
    let info = require(harness.edit_info(&instance.id), "編集情報を取得できません")?;
    Ok(info.scene.id)
}

/// 直前に読み取った epoch を前提条件として得る。
fn precondition(harness: &Harness, instance: &Instance) -> Result<String, String> {
    let info = require(harness.edit_info(&instance.id), "編集情報を取得できません")?;
    Ok(info.project_epoch)
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

/// `details` の真偽値を取り出す。
fn detail_bool(error: &ErrorObject, key: &str) -> Option<bool> {
    error.details.get(key).and_then(|value| value.as_bool())
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
    mismatch: &str,
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
    if detail_str(&error, "mismatch").as_deref() != Some(mismatch) {
        return Err(format!(
            "mismatch={mismatch} を期待しましたが {}",
            describe_error(&error)
        ));
    }
    Ok(vec![describe_error(&error)])
}

/// 位置からオブジェクトを引き直す。
///
/// fingerprint は編集のたびに変わるため、対象は位置で覚えて都度読み直す。
fn read_object(
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
        .ok_or_else(|| format!("{} にオブジェクトがありません", placement_label(at)))
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
///
/// fingerprint は配下 effect の設定値まで含むため、位置だけでなく設定値が
/// 戻っていることもこの比較で捕まる。
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
                "{} のオブジェクトが変化しています",
                placement_label(placement_of(before))
            ));
        }
    }
    Ok(())
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
    EffectItemType::Combo,
];

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

/// 値を書き換えられる設定項目 1 件と、その読み取り時点のセレクター。
///
/// fingerprint は編集のたびに変わるため、使う直前に取り直す。
struct Alterable {
    /// 読み取り時点の effect セレクター。
    selector: EffectSelector,
    /// effect 名。位置から引き直すときの手掛かりになる。
    effect_name: String,
    /// 設定項目名。
    item: String,
    /// 現在の値。
    original: ItemValue,
    /// 書き込む別の値。
    altered: ItemValue,
}

/// 指定位置のオブジェクトから、値を書き換えられる設定項目を 1 つ選ぶ。
fn alterable(
    harness: &Harness,
    instance: &Instance,
    scene_id: i32,
    at: Placement,
) -> Result<Alterable, String> {
    let object = read_object(harness, instance, scene_id, at)?;
    let detail = require(
        harness.object(&instance.id, &object.selector),
        "対象の詳細を取得できません",
    )?;
    for effect in &detail.effects {
        for item in &effect.items {
            if !WRITABLE_ITEM_TYPES.contains(&item.item_type) {
                continue;
            }
            if let Some(altered) = altered_value(&item.value) {
                return Ok(Alterable {
                    selector: effect.selector.clone(),
                    effect_name: effect.name.clone(),
                    item: item.name.clone(),
                    original: item.value.clone(),
                    altered,
                });
            }
        }
    }
    Err(format!(
        "{} のオブジェクトに値を書き換えられる設定項目がありません",
        placement_label(at)
    ))
}

/// 指定位置のオブジェクトから、文字列を書き込める設定項目を 1 つ選ぶ。
fn text_item(
    harness: &Harness,
    instance: &Instance,
    scene_id: i32,
    at: Placement,
    value: &str,
) -> Result<Alterable, String> {
    let object = read_object(harness, instance, scene_id, at)?;
    let detail = require(
        harness.object(&instance.id, &object.selector),
        "対象の詳細を取得できません",
    )?;
    for effect in &detail.effects {
        for item in &effect.items {
            if !matches!(
                item.item_type,
                EffectItemType::Text | EffectItemType::String
            ) {
                continue;
            }
            return Ok(Alterable {
                selector: effect.selector.clone(),
                effect_name: effect.name.clone(),
                item: item.name.clone(),
                original: item.value.clone(),
                altered: ItemValue::Text {
                    value: value.to_string(),
                },
            });
        }
    }
    Err(format!(
        "{} のオブジェクトに文字列を書き込める設定項目がありません",
        placement_label(at)
    ))
}

/// 位置と effect 名と項目名から、いまのセレクターと値を引き直す。
fn reread_item(
    harness: &Harness,
    instance: &Instance,
    scene_id: i32,
    at: Placement,
    effect_name: &str,
    item_name: &str,
) -> Result<(EffectSelector, ItemValue), String> {
    let object = read_object(harness, instance, scene_id, at)?;
    let detail = require(
        harness.object(&instance.id, &object.selector),
        "対象の詳細を取得できません",
    )?;
    let effect = detail
        .effects
        .iter()
        .find(|effect| effect.name == effect_name)
        .ok_or_else(|| format!("effect {effect_name} が見つかりません"))?;
    let item = effect
        .items
        .iter()
        .find(|item| item.name == item_name)
        .ok_or_else(|| format!("設定項目 {item_name} が見つかりません"))?;
    Ok((effect.selector.clone(), item.value.clone()))
}

/// 設定値を指定した値へ戻す。既にその値なら何もしない。
fn restore_item(
    harness: &Harness,
    instance: &Instance,
    scene_id: i32,
    at: Placement,
    effect_name: &str,
    item_name: &str,
    value: &ItemValue,
) -> Result<(), String> {
    let (selector, current) = reread_item(harness, instance, scene_id, at, effect_name, item_name)?;
    if &current == value {
        return Ok(());
    }
    require(
        harness.set_object_item(&instance.id, &selector, item_name, value),
        "設定値を元へ戻せません",
    )?;
    Ok(())
}

/// 対象を指定した位置へ戻す。
///
/// 移動の応答は要求した宛先ではなく実際の配置を返す。成功したことだけでは
/// 戻ったことにならないため、着地点が元の位置であることまで確かめる。
fn move_home(
    harness: &Harness,
    instance: &Instance,
    scene_id: i32,
    from: Placement,
    home: Placement,
) -> Result<(), String> {
    if from == home {
        return Ok(());
    }
    let object = read_object(harness, instance, scene_id, from)?;
    let moved = require(
        harness.move_object(&instance.id, &object.selector, home),
        "対象を元の位置へ戻せません",
    )?;
    let landed = moved
        .object
        .ok_or_else(|| "移動の応答が対象を返しませんでした".to_string())?;
    if placement_of(&landed) != home {
        return Err(format!(
            "{} へ戻したはずが {} に居ます",
            placement_label(home),
            placement_label(placement_of(&landed))
        ));
    }
    Ok(())
}

/// 対象が元の位置に居ることを確かめ、離れていれば戻す。
fn restore_placement(
    harness: &Harness,
    instance: &Instance,
    scene_id: i32,
    strayed: Placement,
    home: Placement,
) -> Result<(), String> {
    if read_object(harness, instance, scene_id, home).is_ok() {
        return Ok(());
    }
    move_home(harness, instance, scene_id, strayed, home)
}

// ---------------------------------------------------------------------------
// 準備
// ---------------------------------------------------------------------------

/// 2 プロセスが揃い、いずれも複製を開いていることを確かめる。
fn prepare(harness: &Harness, report: &mut Report) -> Result<(Instance, Instance), String> {
    prompt(&format!(
        "お願いすること: AviUtl2 を {REQUIRED_INSTANCES} プロセス起動してください。\n\
         そのうえで、固定サンプルプロジェクトの複製を 1 つずつ、別々のプロセスで開いてください。\n\
         2 つのプロセスで同じファイルを開かないでください。\n\
         確認する場所: それぞれの AviUtl2 のタイトルバー。plugin が読み込まれていること。\n\
         回答: 2 つとも開き終えたら Enter を押してください。"
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
        "いま行ったこと: 起動中の AviUtl2 を 2 つ見つけ、開いているプロジェクトを上に表示しました。\n\
         お願いすること: 上の 2 行が、どちらも固定サンプルプロジェクトの複製かを見分けてください。\n\
         確認する場所: 上の 2 行の project= に出ているファイル名とパス。\n\
         AviUtl2 のタイトルバーでも同じものを確かめられます。\n\
         回答: 2 つとも複製なら y、どちらか 1 つでも原本なら n を入力してください。\n\
         n を入力した場合は、何も書き換えずに実行を終えます。",
    ) {
        return Err("実行者が n（複製ではない）と回答したため実行しません。".to_string());
    }

    let mut iter = instances.into_iter();
    let a = iter.next().expect("2 件を確認済み");
    let b = iter.next().expect("2 件を確認済み");

    // 複製であることを内容の面からも確かめる。同位置オブジェクトの fingerprint が
    // 一致しない構成では、別インスタンスのセレクターを拒否する確認が
    // プロジェクトの世代ではなく内容の食い違いで通ってしまい、意味が変わる。
    let outcome = compare_copies(harness, &a, &b);
    report.record(
        "準備",
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
        if placement_of(left) != placement_of(right) {
            return Err("オブジェクトの配置が異なります".to_string());
        }
        if left.fingerprint != right.fingerprint {
            return Err(format!(
                "{} の fingerprint が一致しません",
                placement_label(placement_of(left))
            ));
        }
    }
    Ok(vec![format!(
        "{} 件のオブジェクトが同位置・同 fingerprint",
        objects_a.len()
    )])
}

// ---------------------------------------------------------------------------
// 前提: 同じ区間の中で行った変更が読み直しに現れること
// ---------------------------------------------------------------------------

/// 一括適用が依って立つ前提を、他のどの確認よりも先に確かめる。
///
/// 応答が返す対象は、全 sub-operation を適用し終えた後に編集区間の内側で
/// 読み直したものである。読み直しが変更前の状態を返すなら、適用時点での宛先の
/// 確認も巻き戻したことの確かめ直しも同じく変更前を見ることになり、一括適用は
/// 根本から成立しない。**崩れた時点で以降を打ち切る。**
fn section_inner_read_back(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
    premise: &mut Premise,
) {
    print_section(
        "前提",
        "1 件だけの一括適用でオブジェクトを動かし、同じ呼び出しの応答が新しい位置を返すかを確かめます。実行者の操作はありません。",
    );

    let destination = context.free_slots[0];
    let outcome = check_inner_read_back(harness, instance, context, destination);
    let broken = outcome.as_ref().err().cloned();
    report.record(
        PREMISE_SECTION,
        "同じ呼び出しの中での読み直し",
        "1 件の移動を含む一括適用の応答が、移動後の位置を返す",
        Mode::Auto,
        outcome,
    );
    if let Some(reason) = broken {
        premise.break_with(
            report,
            "前提の「同じ呼び出しの中での読み直し」".to_string(),
            format!(
                "応答が適用後の状態を返さないため、宛先の確認・結果の組み立て・巻き戻しの確かめ直しがいずれも成立しません: {reason}"
            ),
        );
    }
}

/// 1 件の移動を一括適用し、応答が移動後の位置を返すことを確かめる。
fn check_inner_read_back(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    destination: Placement,
) -> CheckResult {
    let object = read_object(harness, instance, context.scene_id, context.first)?;
    let outcome = require(
        harness.apply_batch(&instance.id, vec![move_op(&object.selector, destination)]),
        "1 件の一括適用に失敗しました",
    )?;

    let probed = judge_inner_read_back(&outcome, destination);
    // 後始末: 動かした対象を元の位置へ戻す。判定の成否によらず通す。
    let cleaned = restore_placement(
        harness,
        instance,
        context.scene_id,
        destination,
        context.first,
    );
    match (probed, cleaned) {
        (Ok(notes), Ok(())) => Ok(notes),
        (Ok(_), Err(reason)) => Err(format!("後始末に失敗しました: {reason}")),
        (Err(reason), _) => Err(reason),
    }
}

/// 応答が返した対象が移動後の位置を指していることを確かめる。
fn judge_inner_read_back(outcome: &BatchOutcome, destination: Placement) -> CheckResult {
    let step = outcome
        .results
        .first()
        .ok_or_else(|| "応答が結果を 1 件も返しませんでした".to_string())?;
    let landed = placement_of(&step.object);
    if landed != destination {
        return Err(format!(
            "{} へ動かしたのに応答は {} を返しました",
            placement_label(destination),
            placement_label(landed)
        ));
    }
    if step.effect.is_some() {
        return Err("移動の結果に effect が載っています".to_string());
    }
    Ok(vec![format!("応答が {} を返した", placement_label(landed))])
}

// ---------------------------------------------------------------------------
// 基本
// ---------------------------------------------------------------------------

/// 一括適用が受け付ける組み合わせと、単独 tool との違いを確かめる。
fn section_basic(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> Result<(), String> {
    print_section(
        "基本",
        "移動だけ・設定値だけ・両方を混ぜた一括適用と、2 つのオブジェクトの入れ替えを試します。実行者の操作はありません。",
    );

    let outcome = check_moves_only(harness, instance, context);
    report.record(
        "基本",
        "移動だけの一括適用",
        "2 件の移動だけを含む一括適用が成功し、両方が指定した位置へ移る",
        Mode::Auto,
        outcome,
    );

    let outcome = check_items_only(harness, instance, context);
    report.record(
        "基本",
        "設定値だけの一括適用",
        "設定値の変更だけを含む一括適用が成功し、読み直した値が書き込んだ値と一致する",
        Mode::Auto,
        outcome,
    );

    let outcome = check_mixed(harness, instance, context);
    report.record(
        "基本",
        "同じ読み取り時点のセレクターを並べた一括適用",
        "1 回の読み取りで得た同じセレクターを移動と設定値の変更で並べても、両方が成功する",
        Mode::Auto,
        outcome,
    );

    let outcome = check_single_tools_reject_the_second_call(harness, instance, context);
    report.record(
        "基本",
        "単独 tool を続けて呼んだ場合",
        "同じ読み取り時点のセレクターで単独 tool を続けて呼ぶと、2 回目が precondition_failed（mismatch=fingerprint）で拒まれる",
        Mode::Auto,
        outcome,
    );

    let outcome = check_swap(harness, instance, context);
    report.record(
        "基本",
        "2 つのオブジェクトの入れ替え",
        "互いの位置を交換する 2 件の移動が、1 回の一括適用で成功する",
        Mode::Auto,
        outcome,
    );

    let outcome = check_selector_chain(harness, instance, context);
    report.record(
        "基本",
        "応答が返したセレクターの再利用",
        "一括適用の応答が返したセレクターを読み直さずに次の一括適用へ渡せる",
        Mode::Auto,
        outcome,
    );

    observe_move_destination(harness, report, instance, context);

    Ok(())
}

/// 移動だけを含む一括適用を確かめる。
fn check_moves_only(harness: &Harness, instance: &Instance, context: &Context) -> CheckResult {
    let first_to = context.free_slots[0];
    let second_to = context.free_slots[1];
    let first = read_object(harness, instance, context.scene_id, context.first)?;
    let second = read_object(harness, instance, context.scene_id, context.second)?;

    let applied = harness.apply_batch(
        &instance.id,
        vec![
            move_op(&first.selector, first_to),
            move_op(&second.selector, second_to),
        ],
    );

    let probed = judge_moves(applied, &[first_to, second_to]);
    // 後始末: 2 つとも元の位置へ戻す。
    let cleaned = restore_placement(harness, instance, context.scene_id, first_to, context.first)
        .and_then(|()| {
            restore_placement(
                harness,
                instance,
                context.scene_id,
                second_to,
                context.second,
            )
        });
    finish(probed, cleaned)
}

/// 移動の結果が、要求した宛先の順に並んでいることを確かめる。
fn judge_moves(
    applied: Result<BatchOutcome, ErrorObject>,
    destinations: &[Placement],
) -> CheckResult {
    let outcome = require(applied, "一括適用に失敗しました")?;
    if outcome.results.len() != destinations.len() {
        return Err(format!(
            "{} 件を要求しましたが結果は {} 件でした",
            destinations.len(),
            outcome.results.len()
        ));
    }
    let mut notes = Vec::new();
    for (index, destination) in destinations.iter().enumerate() {
        let landed = placement_of(&outcome.results[index].object);
        if landed != *destination {
            return Err(format!(
                "{index} 件目を {} へ動かしたのに応答は {} を返しました",
                placement_label(*destination),
                placement_label(landed)
            ));
        }
        notes.push(format!("{index} 件目は {}", placement_label(landed)));
    }
    Ok(notes)
}

/// 設定値の変更だけを含む一括適用を確かめる。
fn check_items_only(harness: &Harness, instance: &Instance, context: &Context) -> CheckResult {
    let target = alterable(harness, instance, context.scene_id, context.first)?;
    let applied = harness.apply_batch(
        &instance.id,
        vec![item_op(&target.selector, &target.item, &target.altered)],
    );

    let probed = judge_item_write(harness, instance, context, &target, applied);
    // 後始末: 設定値を元へ戻す。
    let cleaned = restore_item(
        harness,
        instance,
        context.scene_id,
        context.first,
        &target.effect_name,
        &target.item,
        &target.original,
    );
    finish(probed, cleaned)
}

/// 書き込んだ設定値が、応答と読み直しの双方に現れることを確かめる。
fn judge_item_write(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    target: &Alterable,
    applied: Result<BatchOutcome, ErrorObject>,
) -> CheckResult {
    let outcome = require(applied, "一括適用に失敗しました")?;
    let step = outcome
        .results
        .last()
        .ok_or_else(|| "応答が結果を 1 件も返しませんでした".to_string())?;
    let effect = step
        .effect
        .as_ref()
        .ok_or_else(|| "設定値の変更の結果に effect が載っていません".to_string())?;
    let in_response = effect
        .items
        .iter()
        .find(|item| item.name == target.item)
        .map(|item| item.value.clone())
        .ok_or_else(|| "応答の effect に書き込んだ設定項目がありません".to_string())?;
    if in_response != target.altered {
        return Err("応答が返した設定値が書き込んだ値と一致しません".to_string());
    }

    let (_, read_back) = reread_item(
        harness,
        instance,
        context.scene_id,
        context.first,
        &target.effect_name,
        &target.item,
    )?;
    if read_back != target.altered {
        return Err("読み直した設定値が書き込んだ値と一致しません".to_string());
    }
    Ok(vec![format!("設定項目 {} を書き換えた", target.item)])
}

/// 1 回の読み取りで得たセレクターを、移動と設定値の変更で並べられることを確かめる。
///
/// **単独 tool ではこの形が成立しない。** 先行する移動が対象の fingerprint を
/// 変えるため、同じ読み取り時点のセレクターを持つ 2 件目は拒まれる。ここで
/// 確かめるのは、一括適用が全対象を変更前にまとめて照合することの帰結である。
fn check_mixed(harness: &Harness, instance: &Instance, context: &Context) -> CheckResult {
    let destination = context.free_slots[0];
    // 移動と設定値の変更の双方が、この 1 回の読み取りから作られる。
    let target = alterable(harness, instance, context.scene_id, context.first)?;
    let object = target.selector.object.clone();

    let applied = harness.apply_batch(
        &instance.id,
        vec![
            move_op(&object, destination),
            item_op(&target.selector, &target.item, &target.altered),
        ],
    );

    let probed = judge_mixed(applied, destination, &target);
    // 後始末: 設定値を戻してから元の位置へ戻す。
    let cleaned = restore_item(
        harness,
        instance,
        context.scene_id,
        destination,
        &target.effect_name,
        &target.item,
        &target.original,
    )
    .and_then(|()| {
        restore_placement(
            harness,
            instance,
            context.scene_id,
            destination,
            context.first,
        )
    });
    finish(probed, cleaned)
}

/// 混在した一括適用の結果が、両方の変更を反映した同じ対象を返すことを確かめる。
fn judge_mixed(
    applied: Result<BatchOutcome, ErrorObject>,
    destination: Placement,
    target: &Alterable,
) -> CheckResult {
    let outcome = require(applied, "一括適用に失敗しました")?;
    if outcome.results.len() != 2 {
        return Err(format!(
            "2 件を要求しましたが結果は {} 件でした",
            outcome.results.len()
        ));
    }
    // 同じ対象を指す 2 件は、いずれも全件を適用し終えた後の同じ姿を返す。
    if outcome.results[0].object != outcome.results[1].object {
        return Err("同じ対象を指す 2 件が異なる状態を返しました".to_string());
    }
    let landed = placement_of(&outcome.results[0].object);
    if landed != destination {
        return Err(format!(
            "{} へ動かしたのに応答は {} を返しました",
            placement_label(destination),
            placement_label(landed)
        ));
    }
    if outcome.results[0].effect.is_some() {
        return Err("移動の結果に effect が載っています".to_string());
    }
    if outcome.results[1].effect.is_none() {
        return Err("設定値の変更の結果に effect が載っていません".to_string());
    }
    Ok(vec![format!(
        "移動と設定項目 {} の変更が 1 回で通った",
        target.item
    )])
}

/// 同じ読み取り時点のセレクターで単独 tool を続けて呼ぶと拒まれることを確かめる。
///
/// 一括適用が同じ並びを受け付けることと対にして見る。片方だけを確かめると、
/// 一括適用の約束が単独 tool と何が違うのかを示せない。
fn check_single_tools_reject_the_second_call(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    let destination = context.free_slots[0];
    let target = alterable(harness, instance, context.scene_id, context.first)?;
    let object = target.selector.object.clone();

    let moved = require(
        harness.move_object(&instance.id, &object, destination),
        "単独 tool での移動に失敗しました",
    )?;
    let landed = moved
        .object
        .as_ref()
        .map(placement_of)
        .ok_or_else(|| "移動の応答が対象を返しませんでした".to_string())?;

    // 移動より前に読んだ effect セレクターのまま設定値を変えにいく。
    let attempt = harness.set_object_item(
        &instance.id,
        &target.selector,
        &target.item,
        &target.altered,
    );
    let probed = expect_rejection(attempt, ErrorCode::PreconditionFailed, "fingerprint");

    // 後始末: 設定値は変わっていないはずだが、念のため確かめてから位置を戻す。
    let cleaned = restore_item(
        harness,
        instance,
        context.scene_id,
        landed,
        &target.effect_name,
        &target.item,
        &target.original,
    )
    .and_then(|()| restore_placement(harness, instance, context.scene_id, landed, context.first));
    finish(probed, cleaned)
}

/// 2 つのオブジェクトの入れ替えを確かめる。
///
/// **宛先の空きを事前解決の時点で判定する実装では必ず失敗する。** 入れ替えは
/// 互いの現在位置を宛先にするため、適用時点まで宛先の判定を遅らせて初めて通る。
fn check_swap(harness: &Harness, instance: &Instance, context: &Context) -> CheckResult {
    let first = read_object(harness, instance, context.scene_id, context.first)?;
    let second = read_object(harness, instance, context.scene_id, context.second)?;

    let applied = harness.apply_batch(
        &instance.id,
        vec![
            move_op(&first.selector, context.second),
            move_op(&second.selector, context.first),
        ],
    );
    let probed = judge_moves(applied, &[context.second, context.first]);

    // 後始末: もう一度入れ替えて元へ戻す。戻せたことは位置の読み直しで確かめる。
    let cleaned = swap_back(harness, instance, context);
    finish(probed, cleaned)
}

/// 入れ替えた 2 つを、もう一度の入れ替えで元へ戻す。
fn swap_back(harness: &Harness, instance: &Instance, context: &Context) -> Result<(), String> {
    let at_second = read_object(harness, instance, context.scene_id, context.second)?;
    let at_first = read_object(harness, instance, context.scene_id, context.first)?;
    require(
        harness.apply_batch(
            &instance.id,
            vec![
                move_op(&at_second.selector, context.first),
                move_op(&at_first.selector, context.second),
            ],
        ),
        "入れ替えを元へ戻せません",
    )?;
    read_object(harness, instance, context.scene_id, context.first)?;
    read_object(harness, instance, context.scene_id, context.second)?;
    Ok(())
}

/// 応答が返したセレクターで次の一括適用を組み立てられることを確かめる。
fn check_selector_chain(harness: &Harness, instance: &Instance, context: &Context) -> CheckResult {
    let destination = context.free_slots[0];
    let object = read_object(harness, instance, context.scene_id, context.first)?;
    let first = require(
        harness.apply_batch(&instance.id, vec![move_op(&object.selector, destination)]),
        "1 回目の一括適用に失敗しました",
    )?;
    let returned = first
        .results
        .first()
        .map(|step| step.object.selector.clone())
        .ok_or_else(|| "応答が結果を 1 件も返しませんでした".to_string())?;

    // 読み直さずに、応答が返したセレクターのまま次を組み立てる。
    let applied = harness.apply_batch(&instance.id, vec![move_op(&returned, context.first)]);
    let probed = judge_moves(applied, &[context.first]);

    // 後始末: どこに居ても元の位置へ戻す。
    let cleaned = restore_placement(
        harness,
        instance,
        context.scene_id,
        destination,
        context.first,
    );
    finish(probed, cleaned)
}

/// ホストが移動の宛先を調整するかを観測する。
///
/// 調整するなら、巻き戻しが元の位置と完全一致を求める確かめ直しで誤検出し、
/// 成功した巻き戻しが「整合性が不明」を名乗ることになる。
fn observe_move_destination(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) {
    let finding = match probe_move_destination(harness, instance, context) {
        Ok(finding) => finding,
        Err(reason) => format!("観測できませんでした: {reason}"),
    };
    report.observe(
        "move_destination_readback",
        "ホストは移動の宛先と長さを調整するか",
        finding,
    );
}

/// 空きレイヤーの途中のフレームへ動かし、着地点と長さを要求と比べる。
fn probe_move_destination(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
) -> Result<String, String> {
    let object = read_object(harness, instance, context.scene_id, context.first)?;
    let length = object.frame_end.saturating_sub(object.frame_start);
    // 先頭ではない位置を狙う。先頭だけを見ると、開始位置を切り上げる調整に
    // 気付けない。
    let destination = Placement {
        layer: context.free_slots[0].layer,
        frame: object.frame_start + 1,
    };

    let outcome = require(
        harness.apply_batch(&instance.id, vec![move_op(&object.selector, destination)]),
        "観測用の移動に失敗しました",
    )?;
    let moved = outcome
        .results
        .first()
        .map(|step| step.object.clone())
        .ok_or_else(|| "応答が結果を 1 件も返しませんでした".to_string())?;
    let landed = placement_of(&moved);
    let landed_length = moved.frame_end.saturating_sub(moved.frame_start);

    // 後始末: 着地した位置から元へ戻す。
    restore_placement(harness, instance, context.scene_id, landed, context.first)?;

    Ok(format!(
        "要求 {} 長さ {length} に対し、着地は {} 長さ {landed_length}（開始位置の調整={} / 長さの調整={}）",
        placement_label(destination),
        placement_label(landed),
        landed != destination,
        landed_length != length
    ))
}

/// 確認の結果と後始末の結果をまとめる。
///
/// 後始末は判定の成否によらず通す。変えたまま抜けると、以降の確認が別の状態の
/// プロジェクトに対して走る。
fn finish(probed: CheckResult, cleaned: Result<(), String>) -> CheckResult {
    match (probed, cleaned) {
        (Ok(notes), Ok(())) => Ok(notes),
        (Ok(_), Err(reason)) => Err(format!("後始末に失敗しました: {reason}")),
        (Err(reason), Ok(())) => Err(reason),
        (Err(reason), Err(cleanup)) => Err(format!("{reason}（後始末にも失敗: {cleanup}）")),
    }
}

// ---------------------------------------------------------------------------
// 取り消しの単位
// ---------------------------------------------------------------------------

/// 3 件の変更を含む一括適用と、その各件の宛先。
///
/// 3 件目を移動にしてある。宛先を塞げば 3 件目だけを失敗させられ、それより前の
/// 2 件が発行済みの状態で巻き戻しが走る形を作れる。
struct ThreeSteps {
    operations: Vec<BatchOperationInput>,
    first_to: Placement,
    second_to: Placement,
    item: Alterable,
}

/// 移動 2 件と設定値 1 件からなる一括適用を組み立てる。
///
/// セレクターはすべて、この関数の中で行う 1 回の読み取りから作る。
fn build_three_steps(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
) -> Result<ThreeSteps, String> {
    let first_to = context.free_slots[0];
    let second_to = context.free_slots[1];
    let item = alterable(harness, instance, context.scene_id, context.first)?;
    let first = item.selector.object.clone();
    let second = read_object(harness, instance, context.scene_id, context.second)?;

    Ok(ThreeSteps {
        operations: vec![
            move_op(&first, first_to),
            item_op(&item.selector, &item.item, &item.altered),
            move_op(&second.selector, second_to),
        ],
        first_to,
        second_to,
        item,
    })
}

/// 3 件の変更が 1 回の取り消しで戻り、やり直しで再び適用されることを確かめる。
fn section_undo(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> Result<(), String> {
    print_section(
        "取り消しの単位",
        "3 件の変更をまとめて 1 回で適用し、AviUtl2 側で「元に戻す」と「やり直し」を 1 回ずつ押していただきます。",
    );

    let before = snapshot(harness, instance, context.scene_id)?;
    let steps = build_three_steps(harness, instance, context)?;
    let applied = require(
        harness.apply_batch(&instance.id, steps.operations.clone()),
        "3 件の一括適用に失敗しました",
    )?;
    let outcome = judge_three_steps_applied(harness, instance, context, &steps, &applied, &before);
    let applied_ok = outcome.is_ok();
    report.record(
        "取り消しの単位",
        "3 件の一括適用",
        "移動 2 件と設定値 1 件が 1 回の呼び出しで適用され、読み直しに 3 件すべてが現れる",
        Mode::Auto,
        outcome,
    );
    if !applied_ok {
        // 何が適用されたか分からない状態で取り消しを頼むと、実行者の 1 回が
        // 何を戻したのかも分からなくなる。ここで区間を閉じる。
        restore_three_steps(harness, instance, context, &steps)?;
        return Err(
            "3 件の一括適用が期待どおりに適用されなかったため、取り消しの確認へ進みません"
                .to_string(),
        );
    }
    let after_apply = snapshot(harness, instance, context.scene_id)?;

    prompt(&format!(
        "いま行ったこと: MCP から 3 件の変更（オブジェクトの移動 2 件と設定値の変更 1 件）を\n\
         1 回の呼び出しで適用しました。\n\
         お願いすること: インスタンス {} の AviUtl2 で「元に戻す」を 1 回だけ実行してください。\n\
         2 回以上は実行しないでください。\n\
         確認する場所: タイムライン上のオブジェクトの位置と、そのオブジェクトの設定パネル。\n\
         回答: 1 回実行したら Enter を押してください。",
        instance.label
    ));
    let after_undo = snapshot(harness, instance, context.scene_id)?;
    let outcome = expect_unchanged(&before, &after_undo)
        .map(|()| {
            vec!["3 件すべてが 1 回の取り消しで戻り、2 回目を要する状態が残らなかった".to_string()]
        })
        .map_err(|reason| format!("1 回の取り消しでは元へ戻りませんでした: {reason}"));
    let undone = outcome.is_ok();
    report.record(
        "取り消しの単位",
        "1 回の取り消しで戻ること",
        "1 回の取り消し操作で 3 件すべてが元へ戻り、2 回目の取り消しを要する状態が残らない",
        Mode::Operator,
        outcome,
    );

    if !undone {
        // 戻り方が分からない状態でやり直しを頼んでも、何が再適用されたのかを
        // 判定できない。後始末だけを済ませて区間を閉じる。
        restore_three_steps(harness, instance, context, &steps)?;
        return Err("1 回の取り消しで戻らなかったため、やり直しの確認へ進みません".to_string());
    }

    prompt(&format!(
        "いま行ったこと: 3 件すべてが 1 回の取り消しで戻ったことを確かめました。\n\
         お願いすること: インスタンス {} の AviUtl2 で「やり直し」を 1 回だけ実行してください。\n\
         2 回以上は実行しないでください。\n\
         確認する場所: タイムライン上のオブジェクトが、取り消す前の位置へ戻ること。\n\
         回答: 1 回実行したら Enter を押してください。",
        instance.label
    ));
    let after_redo = snapshot(harness, instance, context.scene_id)?;
    let outcome = expect_unchanged(&after_apply, &after_redo)
        .map(|()| vec!["1 回のやり直しで 3 件すべてが再び適用された".to_string()])
        .map_err(|reason| format!("1 回のやり直しで再適用されませんでした: {reason}"));
    report.record(
        "取り消しの単位",
        "やり直しでの再適用",
        "1 回のやり直し操作で 3 件すべてが再び適用される",
        Mode::Operator,
        outcome,
    );

    // 後始末: やり直しで戻った変更を消す。実行者の取り消しで戻らなければ、
    // 読み直したうえで自分で戻す。
    prompt(&format!(
        "いま行ったこと: やり直しによって 3 件の変更が再び適用された状態です。\n\
         お願いすること: インスタンス {} の AviUtl2 で「元に戻す」をもう 1 回だけ実行してください。\n\
         確認する場所: タイムライン上のオブジェクトが、この区間を始める前の位置へ戻ること。\n\
         回答: 1 回実行したら Enter を押してください。",
        instance.label
    ));
    restore_three_steps(harness, instance, context, &steps)?;
    let restored = snapshot(harness, instance, context.scene_id)?;
    expect_unchanged(&before, &restored)
        .map_err(|reason| format!("区間を始める前の状態へ戻せませんでした: {reason}"))?;

    Ok(())
}

/// 3 件が適用され、読み直しにも現れることを確かめる。
fn judge_three_steps_applied(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    steps: &ThreeSteps,
    applied: &BatchOutcome,
    before: &[ObjectSummary],
) -> CheckResult {
    if applied.results.len() != 3 {
        return Err(format!(
            "3 件を要求しましたが結果は {} 件でした",
            applied.results.len()
        ));
    }
    let now = snapshot(harness, instance, context.scene_id)?;
    if expect_unchanged(before, &now).is_ok() {
        return Err("一括適用がプロジェクトを変えていません".to_string());
    }
    read_object(harness, instance, context.scene_id, steps.first_to)?;
    read_object(harness, instance, context.scene_id, steps.second_to)?;
    let (_, value) = reread_item(
        harness,
        instance,
        context.scene_id,
        steps.first_to,
        &steps.item.effect_name,
        &steps.item.item,
    )?;
    if value != steps.item.altered {
        return Err("読み直した設定値が書き込んだ値と一致しません".to_string());
    }
    Ok(vec![format!(
        "移動 2 件（{} と {}）と設定項目 {} の変更が反映された",
        placement_label(steps.first_to),
        placement_label(steps.second_to),
        steps.item.item
    )])
}

/// 3 件の変更を、いまの状態から自分で元へ戻す。
///
/// 実行者の取り消しで既に戻っている場合は何もしない。
fn restore_three_steps(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    steps: &ThreeSteps,
) -> Result<(), String> {
    for (strayed, home) in [
        (steps.first_to, context.first),
        (steps.second_to, context.second),
    ] {
        restore_placement(harness, instance, context.scene_id, strayed, home)?;
    }
    restore_item(
        harness,
        instance,
        context.scene_id,
        context.first,
        &steps.item.effect_name,
        &steps.item.item,
        &steps.item.original,
    )
}

// ---------------------------------------------------------------------------
// 巻き戻し
// ---------------------------------------------------------------------------

/// 途中で失敗した一括適用が、それまでの変更を自分で巻き戻すことを確かめる。
///
/// **拒否されたことだけを見ない。** 事前の照合だけで落ちていても、プロジェクトは
/// 変わらないまま拒否が返る。変更が実際に発行されてから戻ったことは、応答が
/// 巻き戻しを行ったと名乗ることでしか区別できない。
fn section_rollback(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> Result<(), String> {
    print_section(
        "巻き戻し",
        "3 件目の移動先をあらかじめ塞いだうえで同じ一括適用を流し、途中まで適用された変更が戻ることを確かめます。途中で「元に戻す」をお願いします。",
    );

    let base = snapshot(harness, instance, context.scene_id)?;
    let blocked_at = context.free_slots[1];
    let occupier = occupy(harness, instance, context, blocked_at)?;
    let blocked = snapshot(harness, instance, context.scene_id)?;

    let steps = build_three_steps(harness, instance, context)?;
    if steps.second_to != blocked_at {
        // 塞いだ位置と 3 件目の宛先がずれていると、確かめたい失敗が起きない。
        let _ = delete_at(harness, instance, context, occupier);
        return Err(format!(
            "3 件目の宛先が {} であり、塞いだ {} と一致しません",
            placement_label(steps.second_to),
            placement_label(blocked_at)
        ));
    }

    let attempt = harness.apply_batch(&instance.id, steps.operations.clone());
    let outcome = judge_rollback(attempt);
    report.record(
        "巻き戻し",
        "途中で失敗した一括適用の応答",
        "precondition_failed（reason=destination_occupied）が failed_index=2 と巻き戻し済みの印を伴って返り、整合性が不明の印は立たない",
        Mode::Auto,
        outcome,
    );

    let now = snapshot(harness, instance, context.scene_id)?;
    let outcome = expect_unchanged(&blocked, &now)
        .map(|()| vec!["1 件目・2 件目が適用されたまま残っていない".to_string()])
        .map_err(|reason| format!("巻き戻したはずのプロジェクトが変化しています: {reason}"));
    report.record(
        "巻き戻し",
        "巻き戻し後のプロジェクト",
        "失敗した一括適用の直前と、プロジェクトが完全に一致する",
        Mode::Auto,
        outcome,
    );

    let undo = judge_undo_after_rollback(harness, report, instance, context, &base, &blocked);
    report.record(
        "巻き戻し",
        "巻き戻しの後の取り消し",
        "巻き戻しの直後に取り消しても、それより前の変更を失わずに元の状態まで戻せる",
        Mode::Operator,
        undo,
    );

    // 後始末: 塞ぐために作ったオブジェクトが残っていれば消し、元の状態へ戻す。
    delete_at(harness, instance, context, blocked_at)?;
    let restored = snapshot(harness, instance, context.scene_id)?;
    expect_unchanged(&base, &restored)
        .map_err(|reason| format!("区間を始める前の状態へ戻せませんでした: {reason}"))?;
    Ok(())
}

/// 指定した位置へオブジェクトを 1 つ作り、宛先を塞ぐ。
fn occupy(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    at: Placement,
) -> Result<Placement, String> {
    let source = read_object(harness, instance, context.scene_id, context.first)?;
    let alias = require(
        harness.object(&instance.id, &source.selector),
        "塞ぐために使う alias を取得できません",
    )?
    .alias;

    let epoch = precondition(harness, instance)?;
    let created = require(
        harness.create_object(&instance.id, &alias, at, context.scene_id, epoch),
        "宛先を塞ぐオブジェクトを作成できません",
    )?;
    let placed = created
        .object
        .as_ref()
        .map(placement_of)
        .ok_or_else(|| "作成の応答が対象を返しませんでした".to_string())?;
    if placed != at {
        return Err(format!(
            "{} を塞ぐつもりが {} に作られました",
            placement_label(at),
            placement_label(placed)
        ));
    }
    println!("{} を塞ぎました。", placement_label(at));
    Ok(placed)
}

/// 指定した位置にオブジェクトがあれば消す。
fn delete_at(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    at: Placement,
) -> Result<(), String> {
    let Ok(object) = read_object(harness, instance, context.scene_id, at) else {
        return Ok(());
    };
    require(
        harness.delete_object(&instance.id, &object.selector),
        "塞ぐために作ったオブジェクトを削除できません",
    )?;
    Ok(())
}

/// 失敗の名乗り方を、巻き戻しが働いたことまで含めて確かめる。
fn judge_rollback(attempt: Result<BatchOutcome, ErrorObject>) -> CheckResult {
    let error = match attempt {
        Ok(_) => return Err("宛先が塞がっているのに成功しました".to_string()),
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
    if detail_u64(&error, "failed_index") != Some(2) {
        return Err(format!(
            "failed_index=2 を期待しましたが {}",
            describe_error(&error)
        ));
    }
    // 事前の照合だけで落ちた場合、巻き戻すものが無いためこの印は立たない。
    // 立っていることが、変更が実際に発行されてから戻されたことの証拠になる。
    if detail_bool(&error, "rolled_back") != Some(true) {
        return Err(format!(
            "巻き戻し済みの印を期待しましたが {}",
            describe_error(&error)
        ));
    }
    if detail_bool(&error, "consistency_unknown") == Some(true) {
        return Err(format!(
            "巻き戻しに失敗したことを示す印が立っています: {}",
            describe_error(&error)
        ));
    }
    Ok(vec![describe_error(&error)])
}

/// 巻き戻しの直後の取り消しが、それより前の履歴を壊していないことを確かめる。
///
/// 打ち消し合った変更に対して取り消しの単位が積まれるかは、ここでしか分からない。
/// 積まれていれば 1 回目の取り消しは何も変えず、積まれていなければ 1 回目が
/// 塞ぐために行った作成を戻す。どちらであるかを観測として残す。
fn judge_undo_after_rollback(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
    base: &[ObjectSummary],
    blocked: &[ObjectSummary],
) -> CheckResult {
    prompt(&format!(
        "いま行ったこと: 3 件の変更のうち 3 件目だけが失敗する一括適用を流し、\n\
         それまでに適用された 2 件が戻ったことを確かめました。\n\
         その前に、このプログラムが {} へオブジェクトを 1 つ作っています。\n\
         お願いすること: インスタンス {} の AviUtl2 で「元に戻す」を 1 回だけ実行してください。\n\
         2 回以上は実行しないでください。\n\
         確認する場所: タイムライン。\n\
         回答: 1 回実行したら Enter を押してください。",
        placement_label(context.free_slots[1]),
        instance.label
    ));

    let after_first = snapshot(harness, instance, context.scene_id)?;
    if expect_unchanged(base, &after_first).is_ok() {
        report.observe(
            "empty_undo_unit",
            "巻き戻しが全件成功した一括適用は、空の取り消し単位を積むか",
            "積まない。1 回目の取り消しで、その前に行った作成が戻った",
        );
        return Ok(vec![
            "1 回の取り消しで、巻き戻しより前の変更まで戻った".to_string(),
        ]);
    }
    if expect_unchanged(blocked, &after_first).is_err() {
        return Err(
            "1 回目の取り消しの後が、巻き戻し直後とも、それより前の状態とも一致しません"
                .to_string(),
        );
    }

    report.observe(
        "empty_undo_unit",
        "巻き戻しが全件成功した一括適用は、空の取り消し単位を積むか",
        "積む。1 回目の取り消しはプロジェクトを変えなかった",
    );
    prompt(&format!(
        "いま行ったこと: 1 回目の取り消しではプロジェクトが変わりませんでした。\n\
         打ち消し合った変更に対して、空の取り消し単位が積まれたことになります。\n\
         お願いすること: インスタンス {} の AviUtl2 で「元に戻す」をもう 1 回だけ実行してください。\n\
         確認する場所: このプログラムが作ったオブジェクトが {} から消えること。\n\
         回答: 1 回実行したら Enter を押してください。",
        instance.label,
        placement_label(context.free_slots[1])
    ));
    let after_second = snapshot(harness, instance, context.scene_id)?;
    expect_unchanged(base, &after_second)
        .map(|()| {
            vec![
                "空の取り消し単位が 1 つ積まれたが、2 回目の取り消しで以前の変更まで戻った"
                    .to_string(),
            ]
        })
        .map_err(|reason| format!("2 回の取り消しでも以前の状態へ戻りませんでした: {reason}"))
}

// ---------------------------------------------------------------------------
// 費用
// ---------------------------------------------------------------------------

/// 大きな一括適用が何を費やすかを観測する。
///
/// **合否を付けない。** 実行の上限は上限であって目標ではなく、速さの基準を
/// ここで決める材料も無い。測った値と、UI が止まった長さについての実行者の
/// 見立てを記録として残す。
fn section_cost(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> Result<(), String> {
    print_section(
        "費用",
        "できるだけ多くの sub-operation を 1 回に詰めて流し、所要時間と AviUtl2 の UI が止まる長さを測ります。止まった長さの見立てをお尋ねします。",
    );

    let before = snapshot(harness, instance, context.scene_id)?;
    let plan = build_cost_plan(harness, instance, context)?;
    if plan.operations.len() < 2 {
        report.observe(
            "large_batch_cost",
            "多数の sub-operation を含む一括適用は何を費やすか",
            "組み立てられる sub-operation が 2 件未満のため測れませんでした",
        );
        return Ok(());
    }

    println!(
        "{} 件の sub-operation（{} レイヤーにまたがる）を 1 回で流します。",
        plan.operations.len(),
        plan.layers
    );
    println!(
        "うち 1 件は、オブジェクトが 1 つも無い {} を宛先とする移動です。",
        layer_label(plan.move_to.layer)
    );

    let started = Instant::now();
    let applied = harness.apply_batch(&instance.id, plan.operations.clone());
    let elapsed = started.elapsed();

    match &applied {
        Ok(outcome) => report.observe(
            "large_batch_cost",
            "多数の sub-operation を含む一括適用は何を費やすか",
            format!(
                "{} 件（{} レイヤー、空きレイヤーへの移動 1 件を含む）が {} ミリ秒で完了し、結果は {} 件返った",
                plan.operations.len(),
                plan.layers,
                elapsed.as_millis(),
                outcome.results.len()
            ),
        ),
        Err(error) => {
            report.observe(
                "large_batch_cost",
                "多数の sub-operation を含む一括適用は何を費やすか",
                format!(
                    "{} 件（{} レイヤー）が {} ミリ秒で失敗した: {}",
                    plan.operations.len(),
                    plan.layers,
                    elapsed.as_millis(),
                    describe_error(error)
                ),
            );
            // 失敗した一括適用は自分で巻き戻す。戻っていることだけ確かめる。
            let now = snapshot(harness, instance, context.scene_id)?;
            expect_unchanged(&before, &now)
                .map_err(|reason| format!("失敗した一括適用の後が元と一致しません: {reason}"))?;
            return Ok(());
        }
    }

    let freeze = ask(&format!(
        "いま行ったこと: {} 件の変更を 1 回の呼び出しで適用しました。\n\
         お願いすること: 適用の間、インスタンス {} の AviUtl2 の画面が操作を受け付けなかった\n\
         おおよその長さを答えてください。\n\
         確認する場所: AviUtl2 のウィンドウ全体。\n\
         回答: 秒数か「止まらなかった」を入力してください。分からなければ 未確認 と入力してください。",
        plan.operations.len(),
        instance.label
    ));
    report.observe(
        "large_batch_ui_freeze",
        "多数の sub-operation を含む一括適用は AviUtl2 の UI をどれだけ止めるか",
        if freeze.is_empty() {
            "未回答".to_string()
        } else {
            freeze
        },
    );

    // 後始末: 逆向きの一括適用で全件を元へ戻す。戻す側の所要時間も記録に残す。
    let started = Instant::now();
    revert_cost_plan(harness, instance, context, &plan)?;
    let reverted = started.elapsed();
    report.observe(
        "large_batch_revert_cost",
        "同じ規模の一括適用で元へ戻すのに何を費やすか",
        format!("{} ミリ秒で戻した", reverted.as_millis()),
    );

    let after = snapshot(harness, instance, context.scene_id)?;
    expect_unchanged(&before, &after)
        .map_err(|reason| format!("区間を始める前の状態へ戻せませんでした: {reason}"))?;
    Ok(())
}

/// 費用の観測で流す一括適用と、それを元へ戻すための材料。
struct CostPlan {
    operations: Vec<BatchOperationInput>,
    /// 設定値を変えた対象と、その元の値。
    items: Vec<CostItem>,
    /// 移動させる対象の元の位置。
    move_from: Placement,
    /// その移動先。
    move_to: Placement,
    /// sub-operation がまたがるレイヤー数。
    layers: usize,
}

/// 費用の観測で書き換えた設定項目 1 件。
struct CostItem {
    at: Placement,
    effect_name: String,
    item: String,
    original: ItemValue,
}

/// できるだけ多くの sub-operation を含む一括適用を組み立てる。
///
/// 空きレイヤーを宛先とする移動を 1 件だけ混ぜる。空きレイヤーの属性を読む経路を
/// 一括適用の事前解決も通ることを、費用の観測のついでに確かめられる。**これを
/// 合否の対象にはしない。**
fn build_cost_plan(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
) -> Result<CostPlan, String> {
    let layers = require(
        harness.layers(&instance.id, context.scene_id),
        "レイヤーを列挙できません",
    )?;
    let locked: HashSet<usize> = layers
        .iter()
        .filter(|layer| layer.locked)
        .map(|layer| layer.index)
        .collect();

    let move_from = context.second;
    let move_to = context.free_slots[2];
    let moving = read_object(harness, instance, context.scene_id, move_from)?;
    let mut operations = vec![move_op(&moving.selector, move_to)];
    let mut items = Vec::new();
    let mut touched: HashSet<usize> = HashSet::new();
    touched.insert(move_from.layer);
    touched.insert(move_to.layer);

    let objects = snapshot(harness, instance, context.scene_id)?;
    for object in objects {
        if operations.len() >= COST_OPERATION_TARGET {
            break;
        }
        let at = placement_of(&object);
        if locked.contains(&at.layer) || at == move_from {
            continue;
        }
        let Ok(target) = alterable(harness, instance, context.scene_id, at) else {
            continue;
        };
        operations.push(item_op(&target.selector, &target.item, &target.altered));
        touched.insert(at.layer);
        items.push(CostItem {
            at,
            effect_name: target.effect_name,
            item: target.item,
            original: target.original,
        });
    }

    Ok(CostPlan {
        operations,
        items,
        move_from,
        move_to,
        layers: touched.len(),
    })
}

/// 費用の観測で行った変更を、同じ規模の一括適用で元へ戻す。
fn revert_cost_plan(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    plan: &CostPlan,
) -> Result<(), String> {
    let moved = read_object(harness, instance, context.scene_id, plan.move_to)?;
    let mut operations = vec![move_op(&moved.selector, plan.move_from)];
    for item in &plan.items {
        let (selector, _) = reread_item(
            harness,
            instance,
            context.scene_id,
            item.at,
            &item.effect_name,
            &item.item,
        )?;
        operations.push(item_op(&selector, &item.item, &item.original));
    }
    require(
        harness.apply_batch(&instance.id, operations),
        "費用の観測で行った変更を元へ戻せません",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// その他
// ---------------------------------------------------------------------------

/// 文字種・シーン切替・別インスタンス・秘匿を確かめる。
fn section_misc(
    harness: &Harness,
    report: &mut Report,
    a: &Instance,
    b: &Instance,
    context: &Context,
) -> Result<(), String> {
    print_section(
        "その他",
        "日本語と絵文字を含む値の往復、応答とログへ出てはならない値、シーンの切り替え、別インスタンスのセレクターを確かめます。ログの確認とシーンの切り替えをお願いします。",
    );

    match text_item(harness, a, context.scene_id, context.first, WIDE_TEXT) {
        Ok(target) => {
            let outcome = check_wide_value(harness, a, context, &target);
            report.record(
                "その他",
                "日本語・絵文字を含む設定値",
                "日本語と絵文字を含む設定値を一括適用で書き込め、読み直した値が一致する",
                Mode::Auto,
                outcome,
            );
        }
        Err(reason) => report.skip(
            "その他",
            "日本語・絵文字を含む設定値",
            "日本語と絵文字を含む設定値を一括適用で書き込め、読み直した値が一致する",
            Mode::Auto,
            reason,
        ),
    }

    let outcome = check_wide_name(harness, a, context);
    report.record(
        "その他",
        "日本語・絵文字を含む名前の対象",
        "日本語と絵文字を含む名前を運ぶセレクターで一括適用が成功する",
        Mode::Auto,
        outcome,
    );

    let outcome = check_no_secret_in_response(harness, a, context);
    report.record(
        "その他",
        "応答への秘匿値の非混入",
        "一括適用の応答に SDK handle / raw pointer / alias 全文 / 設定値 / 元値が現れない",
        Mode::Auto,
        outcome,
    );

    let outcome = operator_verdict(
        "いま行ったこと: ここまでの確認で、plugin は多数の一括適用を処理してログへ書き出しました。\n\
         お願いすること: plugin のログを開き、SDK handle・raw pointer・alias の全文・設定値の中身・\n\
         書き換える前の値が書かれていないかを探してください。\n\
         確認する場所: 開発用ディレクトリの data/log にある最新のログファイル。\n\
         回答: どれも見つからなければ y、1 つでも見つかれば n を入力してください。",
    );
    report.record(
        "その他",
        "ログへの秘匿値の非混入",
        "plugin のログに SDK handle / raw pointer / alias 全文 / 設定値 / 元値が現れない",
        Mode::Operator,
        outcome,
    );

    check_scene_switch(harness, report, a, context)?;

    let outcome = check_other_instance(harness, a, b, context);
    report.record(
        "その他",
        "別インスタンスのセレクター",
        "インスタンス A のセレクターをインスタンス B の一括適用へ渡すと precondition_failed（mismatch=project_epoch）で拒まれ、B が変更されない",
        Mode::Auto,
        outcome,
    );

    Ok(())
}

/// 日本語と絵文字を含む設定値を往復できることを確かめる。
fn check_wide_value(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    target: &Alterable,
) -> CheckResult {
    let applied = harness.apply_batch(
        &instance.id,
        vec![item_op(&target.selector, &target.item, &target.altered)],
    );
    let probed = judge_item_write(harness, instance, context, target, applied);
    // 後始末: 設定値を元へ戻す。
    let cleaned = restore_item(
        harness,
        instance,
        context.scene_id,
        context.first,
        &target.effect_name,
        &target.item,
        &target.original,
    );
    finish(probed, cleaned)
}

/// 日本語と絵文字を含む名前の対象を一括適用で扱えることを確かめる。
///
/// 名前はセレクターが運ぶ材料であり、一括適用の sub-operation では変えられない。
/// ここで確かめるのは、そういう名前を持つ対象を指せるかどうかである。
fn check_wide_name(harness: &Harness, instance: &Instance, context: &Context) -> CheckResult {
    let object = read_object(harness, instance, context.scene_id, context.first)?;
    let original_name = object.name.clone();
    let renamed = require(
        harness.set_object_name(&instance.id, &object.selector, Some(WIDE_NAME.to_string())),
        "名前を変更できません",
    )?;
    let named = renamed
        .object
        .clone()
        .ok_or_else(|| "名前変更の応答が対象を返しませんでした".to_string())?;

    let probed = judge_wide_name(harness, instance, context, &named);
    // 後始末: 名前を元へ戻す。位置は判定の中で戻している。
    let cleaned = restore_object_name(harness, instance, context, &original_name);
    finish(probed, cleaned)
}

/// 名前を運ぶセレクターで一括適用が通ることを確かめる。
fn judge_wide_name(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    named: &ObjectSummary,
) -> CheckResult {
    if named.name.as_deref() != Some(WIDE_NAME) {
        return Err(format!("名前が {:?} になりました", named.name));
    }
    let destination = context.free_slots[0];
    let applied = harness.apply_batch(&instance.id, vec![move_op(&named.selector, destination)]);
    let probed = judge_moves(applied, &[destination]);
    let cleaned = restore_placement(
        harness,
        instance,
        context.scene_id,
        destination,
        context.first,
    );
    finish(probed, cleaned)
}

/// 対象の名前を控えた値へ戻す。
fn restore_object_name(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    original: &Option<String>,
) -> Result<(), String> {
    let object = read_object(harness, instance, context.scene_id, context.first)?;
    if &object.name == original {
        return Ok(());
    }
    require(
        harness.set_object_name(&instance.id, &object.selector, original.clone()),
        "対象の名前を戻せません",
    )?;
    Ok(())
}

/// 応答へ現れてはならない語。
const FORBIDDEN_IN_RESPONSE: &[&str] = &["handle", "0x", "pointer", "secret", "nonce"];

/// 一括適用の応答へ秘匿値が現れないことを確かめる。
fn check_no_secret_in_response(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
) -> CheckResult {
    let object = read_object(harness, instance, context.scene_id, context.first)?;
    let alias = require(
        harness.object(&instance.id, &object.selector),
        "対象の alias を取得できません",
    )?
    .alias;

    let destination = context.free_slots[0];
    let mut leaks = Vec::new();
    require(
        harness.apply_batch(&instance.id, vec![move_op(&object.selector, destination)]),
        "確認用の一括適用に失敗しました",
    )?;
    inspect_last_raw(harness, &alias, &[], &mut leaks, "移動");
    restore_placement(
        harness,
        instance,
        context.scene_id,
        destination,
        context.first,
    )?;

    // 設定値と元値の双方を、応答へ現れたら分かる文字列にしてから確かめる。
    let checked_value = match text_item(
        harness,
        instance,
        context.scene_id,
        context.first,
        SECRET_TEXT_FIRST,
    ) {
        Ok(target) => {
            let probed =
                probe_secret_values(harness, instance, context, &target, &alias, &mut leaks);
            let cleaned = restore_item(
                harness,
                instance,
                context.scene_id,
                context.first,
                &target.effect_name,
                &target.item,
                &target.original,
            );
            finish(probed, cleaned)?;
            true
        }
        Err(_) => false,
    };

    if !leaks.is_empty() {
        return Err(leaks.join(" / "));
    }
    Ok(vec![format!(
        "alias と禁止語の非混入を確認。設定値と元値の非混入の確認={}",
        if checked_value {
            "実施"
        } else {
            "対象項目なし"
        }
    )])
}

/// 分かる文字列を 2 度書き込み、設定値も元値も応答へ現れないことを確かめる。
fn probe_secret_values(
    harness: &Harness,
    instance: &Instance,
    context: &Context,
    target: &Alterable,
    alias: &str,
    leaks: &mut Vec<String>,
) -> CheckResult {
    require(
        harness.apply_batch(
            &instance.id,
            vec![item_op(&target.selector, &target.item, &target.altered)],
        ),
        "確認用の設定値を書き込めません",
    )?;
    inspect_last_raw(
        harness,
        alias,
        &[SECRET_TEXT_FIRST],
        leaks,
        "1 度目の設定値の変更",
    );

    // 2 度目は、直前に書いた値が元値になる。応答が元値を運んでいれば分かる。
    let (selector, _) = reread_item(
        harness,
        instance,
        context.scene_id,
        context.first,
        &target.effect_name,
        &target.item,
    )?;
    let second_value = ItemValue::Text {
        value: SECRET_TEXT_SECOND.to_string(),
    };
    require(
        harness.apply_batch(
            &instance.id,
            vec![item_op(&selector, &target.item, &second_value)],
        ),
        "確認用の設定値を書き換えられません",
    )?;
    inspect_last_raw(
        harness,
        alias,
        &[SECRET_TEXT_FIRST, SECRET_TEXT_SECOND],
        leaks,
        "2 度目の設定値の変更",
    );
    Ok(Vec::new())
}

/// 直近の応答に、現れてはならない文字列が含まれていないかを見る。
fn inspect_last_raw(
    harness: &Harness,
    alias: &str,
    values: &[&str],
    leaks: &mut Vec<String>,
    what: &str,
) {
    let raw = harness.last_raw();
    if !alias.is_empty() && raw.contains(alias) {
        leaks.push(format!("{what}の応答に alias 全文が現れた"));
    }
    for value in values {
        if raw.contains(value) {
            leaks.push(format!("{what}の応答に設定値が現れた"));
        }
    }
    let lowered = raw.to_lowercase();
    for forbidden in FORBIDDEN_IN_RESPONSE {
        if lowered.contains(forbidden) {
            leaks.push(format!("{what}の応答に {forbidden} が現れた"));
        }
    }
}

/// シーンを切り替えた後の一括適用が拒まれることを確かめる。
fn check_scene_switch(
    harness: &Harness,
    report: &mut Report,
    instance: &Instance,
    context: &Context,
) -> Result<(), String> {
    let destination = context.free_slots[0];
    let object = read_object(harness, instance, context.scene_id, context.first)?;
    let answer = ask(&format!(
        "いま行ったこと: いま開いているシーンで、対象オブジェクトの指定を読み取りました。\n\
         お願いすること: インスタンス {} の AviUtl2 で、別のシーンへ切り替えてください。\n\
         確認する場所: 現在のシーン名の表示。別のシーン名に変わること。\n\
         回答: 切り替えたら Enter を押してください。\n\
         シーンが 1 つしか無く切り替えられない場合は skip と入力してください。",
        instance.label
    ));
    if answer.eq_ignore_ascii_case("skip") {
        report.skip(
            "その他",
            "シーン切替後の一括適用",
            "切替前のセレクターでの一括適用が precondition_failed（mismatch=scene_id）で拒まれる",
            Mode::Operator,
            "シーンを切り替えられないため実施できません",
        );
        return Ok(());
    }

    let attempt = harness.apply_batch(&instance.id, vec![move_op(&object.selector, destination)]);
    let outcome = expect_rejection(attempt, ErrorCode::PreconditionFailed, "scene_id");
    report.record(
        "その他",
        "シーン切替後の一括適用",
        "切替前のセレクターでの一括適用が precondition_failed（mismatch=scene_id）で拒まれる",
        Mode::Operator,
        outcome,
    );

    prompt(&format!(
        "いま行ったこと: 切り替える前のシーンを指した指定で、一括適用を試しました。\n\
         お願いすること: インスタンス {} の AviUtl2 で、元のシーンへ戻してください。\n\
         確認する場所: 現在のシーン名の表示。切り替える前のシーン名に戻ること。\n\
         回答: 戻したら Enter を押してください。",
        instance.label
    ));
    read_object(harness, instance, context.scene_id, context.first)?;
    Ok(())
}

/// 別インスタンスのセレクターが拒まれ、そちらが変わらないことを確かめる。
fn check_other_instance(
    harness: &Harness,
    a: &Instance,
    b: &Instance,
    context: &Context,
) -> CheckResult {
    let scene_b = scene_id(harness, b)?;
    let before_b = snapshot(harness, b, scene_b)?;
    let object = read_object(harness, a, context.scene_id, context.first)?;
    let destination = context.free_slots[0];

    let attempt = harness.apply_batch(&b.id, vec![move_op(&object.selector, destination)]);
    let mut outcome = expect_rejection(attempt, ErrorCode::PreconditionFailed, "project_epoch");
    if outcome.is_ok() {
        let now_b = snapshot(harness, b, scene_b)?;
        outcome = match expect_unchanged(&before_b, &now_b) {
            Ok(()) => outcome.map(|mut notes| {
                notes.push(format!("インスタンス {} は変化していない", b.label));
                notes
            }),
            Err(reason) => Err(format!(
                "拒まれたのにインスタンス {} が変化しました: {reason}",
                b.label
            )),
        };
    }
    outcome
}
