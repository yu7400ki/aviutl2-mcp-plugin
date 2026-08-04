//! AviUtl2 実機を用いた描画の受け入れ確認。
//!
//! # 本ターゲットはプロジェクトを書き換えない
//!
//! 描画はプロジェクトを変えない。ただし 1 項目だけ、シーンの解像度を一時的に
//! 変えていただく。**確認の後に元へ戻し、いずれにせよ保存せずに AviUtl2 を
//! 閉じること。** 成果物はディスクへ書かれるため、実行の最後にそれが片付く
//! ことまで確かめる。
//!
//! 目視での確認のために、読み出した画像の複製を作業用ディレクトリへ書き出す。
//! **これは本ターゲットが応答から得た画像の中身を自分で書いたものであり、
//! 応答が実体の置き場所を教えているわけではない。**
//!
//! # 何を確かめるか
//!
//! 描画は「ホストへ投げて、完了の合図が来るのを待つ」形をとる。完了の合図が
//! 来ない・遅れて来る・二度来ることに我々がどう振る舞うかは実機を要さずに
//! 確かめられるが、**ホストが実際にそれらを起こすかは実機でしか分からない。**
//! したがって本ターゲットは、まずそれらを観測してから合否の確認へ進む。
//!
//! 中断の確認で見るのは**ホストの生死だけ**である。要求元が既に居ない場合、
//! 応答は誰にも届かない。終了の途中では応答より終了が先に来る。「エラーが
//! 返ること」を合格条件に据えると、確かめたいこととずれる。
//!
//! # 準備
//!
//! 1. `au2 develop` で plugin と server を配置し、AviUtl2 から plugin が読み込まれる
//!    状態にする。
//! 2. AviUtl2 を **1 プロセスだけ**起動し、プロジェクトを開く。
//! 3. プロジェクトは次を満たすこと。
//!    - 現在シーンに、**透明な部分を含む絵**が写るフレームがある。
//!    - シーンの解像度を一時的に変えてよい。
//!    - 期限を短縮した要求が期限内に終わらない程度には描画に時間がかかる。
//!      軽すぎるシーンでは、中断の確認のいくつかを起こせない。
//!
//! # 実行方法
//!
//! ```text
//! cargo run -p aviutl2-mcp-server --example live_render_acceptance
//! ```
//!
//! 実行者は表示される指示に従って AviUtl2 を操作する。終盤では AviUtl2 の終了と
//! 再起動を繰り返しお願いする。判定と観測値は実行の最後に一覧で出力する。
//! 不合格が 1 件でもあれば終了コード 1 で終了する。
//!
//! # 環境変数
//!
//! | 変数 | 用途 | 省略時 |
//! |---|---|---|
//! | `AVIUTL2_MCP_REGISTRY_DIR` | インスタンス登録ディレクトリ | 既定の場所 |
//! | `AVIUTL2_MCP_LIVE_RENDER_OUT_DIR` | 目視用の複製を書き出す場所 | 一時ディレクトリの下 |
//! | `AVIUTL2_MCP_LIVE_PLUGIN_LOG_DIR` | plugin のログの置き場。完了待ちの切り離しを自動で数える | 実行者へ尋ねる |
//! | `AVIUTL2_MCP_LIVE_WAIT_FOR_EXPIRY` | 成果物の失効を実時間で待つ | 待たずに未実施とする |
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
    ARTIFACT_EXTENSION, EditInfo, ErrorCode, ErrorObject, OPERATION_RENDER_FRAME, RenderFormat,
    RenderFrameParams, format_sha256,
};
use aviutl2_mcp_server::api::ListInstancesResponse;
use aviutl2_mcp_server::artifact::{
    ARTIFACT_MAX_COUNT, ARTIFACT_MAX_TOTAL_BYTES, ARTIFACT_MEDIA_TYPE, ARTIFACT_TTL, ArtifactStore,
    base_dir_for_registry,
};
use aviutl2_mcp_server::discovery::{DiscoveryConfig, default_registry_dir, resolve_instance};
use aviutl2_mcp_server::mcp::input::{InstanceInput, ListInstancesInput, parse_instance_id};
use aviutl2_mcp_server::mcp::render::{RenderFormatInput, RenderFrameInput};
use aviutl2_mcp_server::mcp::{
    ARTIFACTS_RESOURCE_URI_PREFIX, AviUtl2McpServer, CallLimits, REGISTRY_DIR_ENV,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

/// 目視用の複製を書き出す場所を与える環境変数。
const OUT_DIR_ENV: &str = "AVIUTL2_MCP_LIVE_RENDER_OUT_DIR";
/// plugin のログの置き場を与える環境変数。
const PLUGIN_LOG_DIR_ENV: &str = "AVIUTL2_MCP_LIVE_PLUGIN_LOG_DIR";
/// 成果物の失効を実時間で待つことを指示する環境変数。
const WAIT_FOR_EXPIRY_ENV: &str = "AVIUTL2_MCP_LIVE_WAIT_FOR_EXPIRY";

/// 子として起動されたときに渡す役目。
///
/// 要求を送った直後に落とされるクライアントを、実行者の手ではなく本ターゲット
/// 自身が用意するために使う。
const DOOMED_CLIENT_ARG: &str = "--doomed-client";

/// 確認に要する同時起動プロセス数。
const REQUIRED_INSTANCES: usize = 1;
/// インスタンスが現れるまで待つ上限。
const READY_TIMEOUT: Duration = Duration::from_secs(180);
/// インスタンスが消えるまで待つ上限。
///
/// 完了待ちを切り離す期限に、接続の停止と登録の削除を足しても届く長さを採る。
/// これを超えたら、終了が期限内に完了していない。
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(30);
/// 一覧の再取得間隔。
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// 一覧取得の 1 ページあたり件数。
const PAGE_LIMIT: u32 = 200;
/// 要求を受け付けない状態から戻るのを待つ上限。
///
/// 期限超過で要求元が諦めた後も、接続先は完了の合図を待ち続ける。次の要求は
/// その待ちが終わるまで接続できない。
const HOST_RECOVERY_WINDOW: Duration = Duration::from_secs(90);
/// 期限を短縮した要求の全体の予算。
const SHORT_RENDER_BUDGET: Duration = Duration::from_millis(1_200);
/// そのうち、応答を受け取った後の段へ残す取り分。
const SHORT_INGEST_BUDGET: Duration = Duration::from_millis(200);
/// 子クライアントを落とすまでの待ち。
///
/// 接続・handshake・要求の送信が終わるだけの長さを採る。長くすると描画が
/// 終わってしまい、確かめたい形にならない。
const DOOMED_CLIENT_LIFETIME: Duration = Duration::from_millis(1_500);
/// 放棄の直後に終了する手順を繰り返す回数。
///
/// **1 回通っただけでは、期限内に完了待ちが戻ったのか、たまたま完了の合図が
/// 来なかっただけなのかを区別できない。**
const SHUTDOWN_REPEATS: usize = 10;
/// 完了待ちを切り離したときに plugin が書く文言。
///
/// 期限内に戻れず、待ちのスレッドを切り離して終了へ進んだことを表す。
/// **接頭辞ではなく接尾辞で照合する。** 完了待ちについての記録は他にもあり
/// （待ちが panic で終わった場合など）、頭だけでは切り離しと区別できない。
const DRAIN_DETACH_MARKER: &str = "期限を超えたため切り離しました";
/// 引き渡しの取り残しが片付くのを待つ上限。
///
/// 応答を送れなかった成果物は接続先が消す。その削除は要求元の観測と同時では
/// ないため、1 度数えただけで取り残しと決めない。
const LEFTOVER_SETTLE_WINDOW: Duration = Duration::from_secs(15);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(role) = doomed_client_role(&args) {
        run_as_doomed_client(&role);
        return;
    }

    let mut report = Report::new();
    let outcome = run(&mut report);
    if let Err(message) = outcome {
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
    let base_dir = base_dir_for_registry(&registry_dir);
    println!("registry ディレクトリ: {}", registry_dir.display());
    print_intro();

    let out_dir = prepare_out_dir()?;
    println!("目視用の複製の置き場: {}", out_dir.display());

    let store = Arc::new(
        ArtifactStore::open(base_dir.clone())
            .map_err(|e| format!("描画成果物の保管庫を開けません: {e}"))?,
    );
    let harness = Harness::new(
        registry_dir.clone(),
        CallLimits::default(),
        Arc::clone(&store),
    );
    let short = Harness::new(registry_dir.clone(), short_limits(), Arc::clone(&store));

    let layout = Layout {
        registry_dir,
        base_dir,
        out_dir,
    };
    let target = prepare(&harness)?;

    // 実機でしか決着しない事項を先に観測する。合否の確認は、これらが分かった
    // 上でなければ何を見ているのかが定まらない。
    let target = section_observations(&harness, report, &target, &layout.out_dir)?;
    section_basics(&harness, report, &target, &layout.out_dir)?;
    section_artifacts(&harness, report, &target, &store)?;
    section_interruption(&harness, &short, report, target, &layout, &store)?;

    report_render_tallies(report, &harness, &short);
    section_leftovers(report, &layout.base_dir, &store);

    // 保管庫を閉じるのは最後である。閉じた後は成果物を読めない。
    drop(harness);
    drop(short);
    section_store_removal(report, store, &layout.base_dir);

    prompt(
        "いま行ったこと: すべての確認が終わりました。\n\
         お願いすること: AviUtl2 が起動していれば、保存せずに閉じてください。\n\
         回答: 閉じたら Enter を押してください。判定の一覧を表示します。",
    );
    Ok(())
}

/// 何をするターゲットかを実行前に告げる。
fn print_intro() {
    println!();
    println!(
        "説明: このプログラムは AviUtl2 に現在シーンのフレームを描かせ、その結果を確かめます。"
    );
    println!("      プロジェクトの内容は書き換えませんが、確認の途中で 1 度だけ");
    println!("      シーンの解像度を変えていただきます。終了後は保存せずに閉じてください。");
    println!("      終盤では AviUtl2 の終了と再起動を繰り返しお願いします。");
    println!();
    println!("フレーム番号: この画面が出すフレーム番号は 0 から数えます。");
    println!("              AviUtl2 の UI の表示とは異なることがあります。");
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

/// 期限を短縮した実行予算を作る。
///
/// 応答を受け取った後の段の取り分を全体より短く保つ。取り分が全体を上回ると
/// 期限が「今」になり、要求が接続先へ届く前に必ず期限超過になる。
fn short_limits() -> CallLimits {
    CallLimits {
        render_request: SHORT_RENDER_BUDGET,
        artifact_ingest: SHORT_INGEST_BUDGET,
        ..CallLimits::default()
    }
}

/// 目視用の複製を書き出すディレクトリを用意する。
fn prepare_out_dir() -> Result<PathBuf, String> {
    let dir = match std::env::var_os(OUT_DIR_ENV) {
        Some(dir) => PathBuf::from(dir),
        None => std::env::temp_dir().join("aviutl2-mcp-live-render"),
    };
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("複製の置き場を用意できません {}: {e}", dir.display()))?;
    Ok(dir)
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

/// 実施できたかどうかを含む確認の結果。
///
/// 前提が揃わなかった確認を合格として数えないために、実施しなかったことを
/// 結果の一部として運ぶ。
enum Attempt {
    /// 実施し、合否が決まった。
    Ran(CheckResult),
    /// 前提が揃わず実施できなかった。
    Unmet(String),
}

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

    /// 実施できなかった確認を記録する。
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

    /// 実施できたかどうかを含む結果を記録する。
    fn record_attempt(
        &mut self,
        section: &'static str,
        title: impl Into<String>,
        verified: impl Into<String>,
        mode: Mode,
        attempt: Attempt,
    ) {
        match attempt {
            Attempt::Ran(outcome) => self.record(section, title, verified, mode, outcome),
            Attempt::Unmet(reason) => self.skip(section, title, verified, mode, reason),
        }
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
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

/// 実行者へ可否を尋ねる。
fn confirm(message: &str) -> bool {
    println!();
    println!("{message}");
    print!("[y/N] > ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
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
    let _ = std::io::Write::flush(&mut std::io::stdout());
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

/// 描画の応答。
///
/// **接続先の応答をそのまま受ける型を持たない。** 要求元へ渡る形に何が載って
/// いるかを確かめるのが目的であるため、要求元が受け取れる形だけを写す。
#[derive(Debug, Clone, serde::Deserialize)]
struct RenderResponse {
    project_epoch: String,
    project_revision: u64,
    scene_id: i32,
    frame: u32,
    width: u32,
    height: u32,
    artifact: ArtifactResponse,
}

/// 応答が運ぶ成果物の参照。
#[derive(Debug, Clone, serde::Deserialize)]
struct ArtifactResponse {
    artifact_id: String,
    uri: String,
    media_type: String,
    byte_length: u64,
    sha256: String,
    expires_at: String,
}

/// 描画要求の結末の内訳。
#[derive(Debug, Default, Clone, Copy)]
struct RenderTally {
    succeeded: usize,
    timed_out: usize,
    host_busy: usize,
    other: usize,
}

impl RenderTally {
    fn record(&mut self, result: &Result<RenderResponse, ErrorObject>) {
        match result {
            Ok(_) => self.succeeded += 1,
            Err(error) if error.code == ErrorCode::Timeout => self.timed_out += 1,
            Err(error) if error.code == ErrorCode::HostBusy => self.host_busy += 1,
            Err(_) => self.other += 1,
        }
    }

    fn total(&self) -> usize {
        self.succeeded + self.timed_out + self.host_busy + self.other
    }

    fn summary(&self) -> String {
        format!(
            "{} 回中、成功 {} / 期限までに応答が返らなかった {} / 受け付けられなかった {} / その他の失敗 {}",
            self.total(),
            self.succeeded,
            self.timed_out,
            self.host_busy,
            self.other
        )
    }
}

/// MCP tool を実機のインスタンスへ発行する実行環境。
///
/// tool は非同期であるため、専用のランタイム上で 1 件ずつ完了まで待つ。
struct Harness {
    server: AviUtl2McpServer,
    runtime: tokio::runtime::Runtime,
    /// 成果物の保管庫。応答が指す成果物の実体を読むために保持する。
    store: Arc<ArtifactStore>,
    /// 直近の tool result を文字どおりに保持する。応答へ秘匿値が現れないことの
    /// 確認は、DTO へ写した後ではなく実際に返した文字列に対して行う。
    last_raw: RefCell<String>,
    /// 描画要求の結末の内訳。
    tally: RefCell<RenderTally>,
}

impl Harness {
    fn new(registry_dir: PathBuf, limits: CallLimits, store: Arc<ArtifactStore>) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("現在スレッドのランタイムを作成できる");
        Self {
            server: AviUtl2McpServer::with_artifact_store(registry_dir, limits, Arc::clone(&store)),
            runtime,
            store,
            last_raw: RefCell::new(String::new()),
            tally: RefCell::new(RenderTally::default()),
        }
    }

    /// 直近の tool result の文字表現を返す。
    fn last_raw(&self) -> String {
        self.last_raw.borrow().clone()
    }

    /// 描画要求の結末の内訳を返す。
    fn tally(&self) -> RenderTally {
        *self.tally.borrow()
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

    fn render_frame(
        &self,
        instance: &str,
        scene_id: i32,
        frame: u32,
    ) -> Result<RenderResponse, ErrorObject> {
        let result =
            self.runtime.block_on(
                self.server
                    .aviutl2_render_frame(Parameters(RenderFrameInput {
                        instance_id: instance.to_string(),
                        expected_scene_id: scene_id,
                        frame,
                        format: RenderFormatInput::Png,
                    })),
            );
        let decoded = self.decode(result);
        self.tally.borrow_mut().record(&decoded);
        decoded
    }
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

// ---------------------------------------------------------------------------
// 共通の補助
// ---------------------------------------------------------------------------

/// 実行に使うディレクトリの組。
///
/// 中断の確認は 3 つとも要る。要求元の役目の子には登録の場所を、取り残しの
/// 確認には成果物の基底を、目視の複製には作業用の場所を渡す。
struct Layout {
    /// インスタンスの登録の場所。
    registry_dir: PathBuf,
    /// 成果物と引き渡しに共通の基底。
    base_dir: PathBuf,
    /// 目視用の複製を書き出す場所。
    out_dir: PathBuf,
}

/// 描画の対象とする稼働中インスタンスと、その現在シーン。
#[derive(Debug, Clone)]
struct Target {
    /// tool へ渡す ID。再起動のたびに変わる。
    id: String,
    /// 現在シーン ID。
    scene_id: i32,
    /// シーンの横幅。
    width: u32,
    /// シーンの高さ。
    height: u32,
    /// オブジェクトが存在する最終フレーム。
    frame_max: usize,
}

impl Target {
    /// 描画に使うフレームを、シーンの範囲に収まる形で選ぶ。
    fn frames(&self) -> Vec<u32> {
        let last = u32::try_from(self.frame_max).unwrap_or(u32::MAX);
        let mut frames = vec![0, last / 2, last];
        frames.dedup();
        frames
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

/// 描画の結末を 1 行で表す。
fn describe_render(result: &Result<RenderResponse, ErrorObject>) -> String {
    match result {
        Ok(response) => format!(
            "成功 frame={} {}x{} artifact_id={}",
            response.frame, response.width, response.height, response.artifact.artifact_id
        ),
        Err(error) => describe_error(error),
    }
}

/// 起動している 1 プロセスを見つけ、現在シーンを読む。
fn wait_for_single_instance(harness: &Harness) -> Result<Target, String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let response = require(harness.list_instances(), "インスタンスを列挙できません")?;
        if response.instances.len() == REQUIRED_INSTANCES {
            let id = response.instances[0].instance_id.to_string();
            let info = require(harness.edit_info(&id), "編集情報を取得できません")?;
            return Ok(Target {
                id,
                scene_id: info.scene.id,
                width: info.scene.width,
                height: info.scene.height,
                frame_max: info.extent.frame_max,
            });
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

/// 現在シーンの情報を読み直す。
fn refresh(harness: &Harness, target: &Target) -> Result<Target, String> {
    let info = require(harness.edit_info(&target.id), "編集情報を取得できません")?;
    Ok(Target {
        id: target.id.clone(),
        scene_id: info.scene.id,
        width: info.scene.width,
        height: info.scene.height,
        frame_max: info.extent.frame_max,
    })
}

/// 終了の完了を、登録された descriptor が消えることで見る。
///
/// **一覧の tool では測れない。** 一覧は生存確認のために接続へ出るため、
/// 描画が接続を占めている間は稼働中のインスタンスも一覧から外れる。それを
/// 終了と読むと、終了していないのに終了したことになる。descriptor の削除は
/// 終了手順そのものが行うため、接続の状態に左右されない。
fn wait_until_descriptor_gone(
    registry_dir: &Path,
    instance_id: &str,
    limit: Duration,
) -> Result<Duration, String> {
    let path = registry_dir.join(format!("{instance_id}.json"));
    let started = Instant::now();
    loop {
        if !path.exists() {
            return Ok(started.elapsed());
        }
        if started.elapsed() >= limit {
            return Err(format!(
                "{} 秒待っても終了が完了しません",
                limit.as_secs_f64()
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// 要求を受け付ける状態へ戻るまで待つ。
///
/// 要求元が期限で諦めた後も、接続先は完了の合図を待ち続ける。次の要求はその
/// 待ちが終わるまで接続できないため、1 度の失敗では応答不能と決められない。
fn wait_until_responsive(
    harness: &Harness,
    instance_id: &str,
    window: Duration,
) -> Result<Duration, String> {
    let started = Instant::now();
    loop {
        let last = match harness.edit_info(instance_id) {
            Ok(_) => return Ok(started.elapsed()),
            Err(error) => describe_error(&error),
        };
        if started.elapsed() >= window {
            return Err(format!(
                "{} 秒待っても応答しません: {last}",
                window.as_secs_f64()
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// 成果物の実体を読み出す。
///
/// # 既知の限界: resource としての読み出しは通っていない
///
/// ここが行うのは保管庫からの直接の読み出しであり、**MCP の resource として
/// 読む経路そのものではない。** 引き当ての材料は同じ（保管庫が持つ一覧に対して
/// 引き当て、識別子をパスへ連結しない）が、URI の解釈・`Blob` としての包み方・
/// content type の付与は通っていない。したがって本ターゲットが確かめられるのは
/// 「応答が指す成果物の実体を識別子から取り出せること」までである。
///
/// 代わりが無いのは、resource の読み出しが要求文脈を要し、それを例から
/// 組み立てられないためである。
fn read_artifact(harness: &Harness, artifact_id: &str) -> Result<(Vec<u8>, String), String> {
    let content = harness
        .store
        .read(artifact_id)
        .ok_or_else(|| format!("成果物 {artifact_id} を読み出せません"))?;
    Ok((content.bytes, content.artifact.media_type.to_string()))
}

/// 目視できるよう、読み出した画像の複製を作業用ディレクトリへ書く。
fn write_copy(out_dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let path = out_dir.join(format!("{name}.png"));
    std::fs::write(&path, bytes)
        .map_err(|e| format!("複製を書き出せません {}: {e}", path.display()))?;
    Ok(path)
}

/// PNG の署名と最初のチャンクから、画像の幅と高さを読む。
///
/// 復号器を持たずに寸法だけを確かめる。応答が名乗る寸法と実体が食い違って
/// いれば、要求元は届いた画像を誤って扱う。
fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    const SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 24 {
        return Err(format!("{} バイトしかなく PNG ではありません", bytes.len()));
    }
    if bytes[..8] != SIGNATURE {
        return Err("PNG の署名で始まっていません".to_string());
    }
    if &bytes[12..16] != b"IHDR" {
        return Err("最初のチャンクが IHDR ではありません".to_string());
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Ok((width, height))
}

/// 実体からダイジェストを算出し、応答が名乗る書式で返す。
///
/// 応答は `"sha256:" と 64 桁の小文字十六進` で名乗る。同じ書式へ揃えてから
/// 比べる。
fn sha256_of(bytes: &[u8]) -> String {
    format_sha256(&Sha256::digest(bytes))
}

/// ディレクトリ以下にある PNG ファイルの数を数える。
///
/// 引き渡し用のファイルも保管庫の実体も同じ拡張子を持つ。**保管庫が保持して
/// いる件数と一致すれば、引き渡しの取り残しは無い。** ディレクトリの名前を
/// 知らなくても、この 1 つの数で取り残しを見つけられる。
///
/// **見ているのはファイルであってディレクトリではない。** 中身が消えた後に
/// 空のディレクトリが残っていても、この数には現れない。
fn count_png_files(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut count = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            count += count_png_files(&path);
        } else if path.extension().and_then(|e| e.to_str()) == Some(ARTIFACT_EXTENSION) {
            count += 1;
        }
    }
    count
}

// ---------------------------------------------------------------------------
// 準備
// ---------------------------------------------------------------------------

/// 1 プロセスが起動していることを確かめ、現在シーンを読む。
fn prepare(harness: &Harness) -> Result<Target, String> {
    prompt(&format!(
        "お願いすること: AviUtl2 を {REQUIRED_INSTANCES} プロセスだけ起動し、プロジェクトを開いてください。\n\
         他の AviUtl2 が起動している場合は閉じてください。\n\
         確認する場所: AviUtl2 のタイトルバー。plugin が読み込まれていること。\n\
         回答: 開き終えたら Enter を押してください。"
    ));

    let target = wait_for_single_instance(harness)?;
    println!();
    println!(
        "対象: instance_id={} scene_id={} 解像度 {}x{} 最終フレーム {}",
        target.id, target.scene_id, target.width, target.height, target.frame_max
    );
    Ok(target)
}

// ---------------------------------------------------------------------------
// 実機でのみ決着する事項の観測
// ---------------------------------------------------------------------------

/// 合否の確認より先に、ホストが実際に何をするかを観測する。
///
/// **どれも合否を付けない。** ここで分かることは、我々の実装が正しいかでは
/// なく、どの経路が実機で踏まれるかである。
fn section_observations(
    harness: &Harness,
    report: &mut Report,
    target: &Target,
    out_dir: &Path,
) -> Result<Target, String> {
    print_section(
        "観測",
        "プレビュー再生中の描画、幅が 4 の倍数でない解像度、透明部分の見え方、範囲外のフレームを試します。再生の開始と停止、解像度の変更をお願いします。",
    );

    observe_render_during_preview(harness, report, target);
    let target = observe_odd_width(harness, report, target, out_dir)?;
    observe_alpha(harness, report, &target, out_dir);
    observe_frame_max(harness, report, &target);
    skip_callback_arrivals(report);
    Ok(target)
}

/// プレビュー再生中に描画できるかを観測する。
///
/// 事前に拒否していない。描けるのに拒否すれば機能の損失であり、描けないなら
/// 投入の失敗が拾う。**どちらであるかはここでしか分からない。**
fn observe_render_during_preview(harness: &Harness, report: &mut Report, target: &Target) {
    prompt(
        "お願いすること: AviUtl2 でプレビューの再生を開始してください。\n\
         再生を止めないまま、この画面へ戻ってきてください。\n\
         確認する場所: AviUtl2 のプレビュー。再生位置が進んでいること。\n\
         回答: 再生したままの状態で Enter を押してください。",
    );
    let result = harness.render_frame(&target.id, target.scene_id, 0);
    let finding = describe_render(&result);
    prompt(
        "お願いすること: AviUtl2 でプレビューの再生を停止してください。\n\
         確認する場所: AviUtl2 のプレビュー。再生位置が止まっていること。\n\
         回答: 停止したら Enter を押してください。",
    );

    report.observe(
        "render_during_preview",
        "プレビュー再生中に描画を要求すると何が返るか",
        finding,
    );
}

/// 幅が 4 の倍数でない解像度で、詰め物を除く経路が踏まれるかを観測する。
///
/// 1 行あたりのバイト数が幅の 4 倍と一致するなら、詰め物を除く経路は実機では
/// 踏まれない。一致しないなら、除き損ねた画像は右へずれて斜めに見える。
/// **外から見えるのはその見え方だけである。**
fn observe_odd_width(
    harness: &Harness,
    report: &mut Report,
    target: &Target,
    out_dir: &Path,
) -> Result<Target, String> {
    let answer = ask(&format!(
        "いま行ったこと: いまのシーンの解像度は {}x{} です。\n\
         お願いすること: AviUtl2 でシーンの解像度を、横幅が 4 の倍数でない値へ変更してください。\n\
         例: 横幅を 1919 にする。高さは変えなくて構いません。\n\
         確認する場所: シーンの設定ダイアログ。\n\
         回答: 変更したら Enter を押してください。\n\
         解像度を変更できない場合は skip と入力してください。",
        target.width, target.height
    ));
    if answer.eq_ignore_ascii_case("skip") {
        report.observe(
            "row_padding",
            "1 行あたりのバイト数は幅の 4 倍と一致するか",
            "幅が 4 の倍数でない解像度を作れなかったため観測できていない",
        );
        return Ok(target.clone());
    }

    let odd = refresh(harness, target)?;
    if odd.width % 4 == 0 {
        report.observe(
            "row_padding",
            "1 行あたりのバイト数は幅の 4 倍と一致するか",
            format!(
                "横幅が {} であり 4 の倍数のままのため、詰め物の経路を踏ませられていない",
                odd.width
            ),
        );
    } else {
        let finding = match probe_odd_width(harness, &odd, out_dir) {
            Ok(finding) => finding,
            Err(reason) => format!("観測できませんでした: {reason}"),
        };
        report.observe(
            "row_padding",
            "1 行あたりのバイト数は幅の 4 倍と一致するか",
            finding,
        );
    }

    prompt(&format!(
        "いま行ったこと: 幅が 4 の倍数でない解像度で描画しました。\n\
         お願いすること: AviUtl2 でシーンの解像度を元の {}x{} へ戻してください。\n\
         確認する場所: シーンの設定ダイアログ。\n\
         回答: 戻したら Enter を押してください。",
        target.width, target.height
    ));
    refresh(harness, target)
}

/// 幅が 4 の倍数でない解像度で 1 枚描き、見え方を実行者へ尋ねる。
fn probe_odd_width(harness: &Harness, target: &Target, out_dir: &Path) -> Result<String, String> {
    let response = require(
        harness.render_frame(&target.id, target.scene_id, 0),
        "幅が 4 の倍数でない解像度での描画に失敗しました",
    )?;
    let (bytes, _) = read_artifact(harness, &response.artifact.artifact_id)?;
    let (width, height) = png_dimensions(&bytes)?;
    let path = write_copy(out_dir, "odd-width", &bytes)?;

    let answer = ask(&format!(
        "いま行ったこと: 横幅 {} の解像度で 1 枚描き、次の場所へ複製を書き出しました。\n\
         {}\n\
         お願いすること: その画像を開き、AviUtl2 のプレビューと見比べてください。\n\
         確認する場所: 画像の全体。行がずれて斜めに流れていないか。\n\
         回答: プレビューと同じに見えれば 同じ\n\
         斜めにずれて見えれば ずれ\n\
         確かめられなければ 未確認 と入力してください。",
        target.width,
        path.display()
    ));
    let answer = if answer.is_empty() {
        "未回答".to_string()
    } else {
        answer
    };
    Ok(format!(
        "横幅 {} で描画でき、実体の寸法は {width}x{height}（応答は {}x{}）。見え方の回答: {answer}",
        target.width, response.width, response.height
    ))
}

/// 透明部分の見え方から、アルファが乗算済みかを観測する。
///
/// 乗算済みなら PNG の扱いを決め直すことになる。**推測で除算すると、乗算済み
/// でなかった場合に色が壊れる。**
fn observe_alpha(harness: &Harness, report: &mut Report, target: &Target, out_dir: &Path) {
    let finding = match probe_alpha(harness, target, out_dir) {
        Ok(finding) => finding,
        Err(reason) => format!("観測できませんでした: {reason}"),
    };
    report.observe(
        "premultiplied_alpha",
        "描画結果のアルファは乗算済みか",
        finding,
    );
}

/// 透明部分を含むフレームを 1 枚描き、見え方を実行者へ尋ねる。
fn probe_alpha(harness: &Harness, target: &Target, out_dir: &Path) -> Result<String, String> {
    let frame = ask(
        "お願いすること: 透明な部分を含む絵が写るフレームの番号を入力してください。\n\
         確認する場所: AviUtl2 のプレビュー。背景が透けて見えるフレーム。\n\
         回答: フレーム番号（0 から数えます）を入力してください。\n\
         透明な部分を含むフレームが無ければ skip と入力してください。",
    );
    if frame.eq_ignore_ascii_case("skip") {
        return Err("透明な部分を含むフレームが無い".to_string());
    }
    let frame: u32 = frame
        .parse()
        .map_err(|_| format!("フレーム番号として解釈できません: {frame}"))?;

    let response = require(
        harness.render_frame(&target.id, target.scene_id, frame),
        "透明部分を含むフレームの描画に失敗しました",
    )?;
    let (bytes, _) = read_artifact(harness, &response.artifact.artifact_id)?;
    let path = write_copy(out_dir, "transparent", &bytes)?;

    let answer = ask(&format!(
        "いま行ったこと: フレーム {frame} を描き、次の場所へ複製を書き出しました。\n\
         {}\n\
         お願いすること: その画像を開き、AviUtl2 のプレビューと見比べてください。\n\
         確認する場所: 半透明な部分の色。\n\
         回答: プレビューと同じ色に見えれば 同じ\n\
         半透明な部分が暗く沈んで見えれば 暗い\n\
         確かめられなければ 未確認 と入力してください。",
        path.display()
    ));
    let answer = if answer.is_empty() {
        "未回答".to_string()
    } else {
        answer
    };
    Ok(format!("フレーム {frame} の見え方の回答: {answer}"))
}

/// 範囲を超えるフレームを要求したときに何が起きるかを観測する。
///
/// 我々は投入の前に拒否する。ホストへ届かないため、ホストが何をするかは
/// 分からないままである。**分からないことを記録に残す。**
fn observe_frame_max(harness: &Harness, report: &mut Report, target: &Target) {
    let beyond = u32::try_from(target.frame_max)
        .unwrap_or(u32::MAX - 1)
        .saturating_add(1);
    let result = harness.render_frame(&target.id, target.scene_id, beyond);
    report.observe(
        "frame_beyond_extent",
        "範囲を超えるフレームを要求すると何が起きるか",
        format!(
            "最終フレーム {} に対しフレーム {beyond} を要求した結果: {}",
            target.frame_max,
            describe_render(&result)
        ),
    );
}

/// 完了の合図が二度届くことがあるかは、外から数えられない。
///
/// **実行者に実施できる形へ落とせない。** 合図の受け皿は二度目の値を捨てて
/// 戻るため、二度目が届いたかどうかは応答にも成果物にも現れない。数えられるのは
/// 受け皿の側だけであり、その数は外へ出ていない。答えられない問いを残すと、
/// 答えられなかったことが不合格として数えられる。
fn skip_callback_arrivals(report: &mut Report) {
    report.skip(
        "観測",
        "完了の合図の到着回数",
        "1 回の描画につき完了の合図が何回届くかを数える",
        Mode::Auto,
        "外から数えられない。二度目の合図は受け皿が捨てて戻るため、応答にも成果物にも現れない。\
         数を知るには受け皿の側が回数を記録して外へ出す必要があり、いまの実装はそれを行っていない。\
         人へ尋ねても答えられる材料が無いため、恒久的に見送る",
    );
}

/// 描画要求の結末の内訳を、合図が届かなかった回数の記録として残す。
///
/// 期限までに応答が返らなかった回は、その時点で完了の合図が届いていない。
/// 受け付けられなかった回は、届かないまま放棄された受け皿が上限に達している。
/// **どちらも外から数えられる唯一の材料である。**
fn report_render_tallies(report: &mut Report, harness: &Harness, short: &Harness) {
    report.observe(
        "render_completion",
        "完了の合図が期限までに届かないことがあるか",
        format!(
            "通常の期限での描画: {} ／ 期限を短縮した描画: {}",
            harness.tally().summary(),
            short.tally().summary()
        ),
    );
}

// ---------------------------------------------------------------------------
// 基本
// ---------------------------------------------------------------------------

/// 描いた結果が、応答の名乗りどおりに受け取れることを確かめる。
fn section_basics(
    harness: &Harness,
    report: &mut Report,
    target: &Target,
    out_dir: &Path,
) -> Result<(), String> {
    print_section(
        "基本",
        "現在シーンの複数のフレームを描き、応答が名乗る寸法・大きさ・ダイジェストが実体と一致することを確かめます。最後に絵の見比べをお願いします。",
    );

    let mut rendered = Vec::new();
    let mut failures = Vec::new();
    for frame in target.frames() {
        match harness.render_frame(&target.id, target.scene_id, frame) {
            Ok(response) => {
                let raw = harness.last_raw();
                match read_artifact(harness, &response.artifact.artifact_id) {
                    Ok((bytes, media_type)) => rendered.push(Rendered {
                        response,
                        bytes,
                        media_type,
                        raw,
                    }),
                    Err(reason) => failures.push(format!("フレーム {frame}: {reason}")),
                }
            }
            Err(error) => failures.push(format!("フレーム {frame}: {}", describe_error(&error))),
        }
    }

    let outcome = if failures.is_empty() && !rendered.is_empty() {
        Ok(rendered
            .iter()
            .map(|item| format!("フレーム {} を描いた", item.response.frame))
            .collect())
    } else {
        Err(format!("描き切れませんでした: {}", failures.join(" / ")))
    };
    let ready = outcome.is_ok();
    report.record(
        "基本",
        "複数フレームの描画",
        "現在シーンの複数のフレームを描け、それぞれの成果物を読み出せる",
        Mode::Auto,
        outcome,
    );
    if !ready {
        return Err("フレームを描けなかったため、以降の確認へ進みません".to_string());
    }

    report.record(
        "基本",
        "応答が名乗る寸法と実体の一致",
        "応答の幅と高さがシーンの解像度と一致し、実体の PNG が同じ寸法を持つ",
        Mode::Auto,
        judge_dimensions(&rendered, target),
    );

    report.record(
        "基本",
        "応答が名乗る大きさとダイジェストの一致",
        "応答の byte_length とダイジェストが、読み出した実体から算出した値と一致する",
        Mode::Auto,
        judge_digest(&rendered),
    );

    report.record(
        "基本",
        "成果物の受け取り",
        "成果物が識別子から image/png として読み出せ、応答の URI が識別子から組み立てられている（resource として読む経路そのものは通っていない）",
        Mode::Auto,
        judge_artifact_reference(&rendered),
    );

    report.record(
        "基本",
        "応答への置き場所と引き渡しの識別子の非混入",
        "応答に実体の置き場所も、接続先が書いた引き渡しファイルの識別子も現れない",
        Mode::Auto,
        judge_no_location_in_response(&rendered),
    );

    let outcome = judge_visual_match(&rendered, out_dir);
    report.record(
        "基本",
        "描いた絵とプレビューの一致",
        "描いた絵が AviUtl2 のプレビューと一致する",
        Mode::Operator,
        outcome,
    );

    Ok(())
}

/// 1 回の描画とその実体。
struct Rendered {
    response: RenderResponse,
    bytes: Vec<u8>,
    media_type: String,
    /// その回の tool result の文字表現。
    raw: String,
}

/// 応答の寸法が、シーンの解像度とも実体の PNG とも一致することを確かめる。
fn judge_dimensions(rendered: &[Rendered], target: &Target) -> CheckResult {
    let mut notes = Vec::new();
    for item in rendered {
        let response = &item.response;
        if response.scene_id != target.scene_id {
            return Err(format!(
                "シーン {} を要求したのに応答は {} を返しました",
                target.scene_id, response.scene_id
            ));
        }
        if response.width != target.width || response.height != target.height {
            return Err(format!(
                "シーンの解像度は {}x{} ですが応答は {}x{} を返しました",
                target.width, target.height, response.width, response.height
            ));
        }
        let (width, height) = png_dimensions(&item.bytes)?;
        if width != response.width || height != response.height {
            return Err(format!(
                "応答は {}x{} を名乗りますが実体は {width}x{height} でした",
                response.width, response.height
            ));
        }
        if response.project_epoch.is_empty() {
            return Err("応答がプロジェクトの世代を運んでいません".to_string());
        }
        notes.push(format!(
            "フレーム {} は {width}x{height}（revision={}）",
            response.frame, response.project_revision
        ));
    }
    Ok(notes)
}

/// 応答が名乗る大きさとダイジェストが実体と一致することを確かめる。
fn judge_digest(rendered: &[Rendered]) -> CheckResult {
    let mut notes = Vec::new();
    for item in rendered {
        let response = &item.response;
        let actual_length = item.bytes.len() as u64;
        if response.artifact.byte_length != actual_length {
            return Err(format!(
                "応答は {} バイトを名乗りますが実体は {actual_length} バイトでした",
                response.artifact.byte_length
            ));
        }
        let actual = sha256_of(&item.bytes);
        if response.artifact.sha256 != actual {
            return Err(format!(
                "フレーム {} のダイジェストが実体と一致しません",
                response.frame
            ));
        }
        notes.push(format!(
            "フレーム {} は {actual_length} バイトで一致",
            response.frame
        ));
    }
    Ok(notes)
}

/// 成果物の参照が、読み出せる形で返っていることを確かめる。
fn judge_artifact_reference(rendered: &[Rendered]) -> CheckResult {
    let mut notes = Vec::new();
    for item in rendered {
        let artifact = &item.response.artifact;
        if item.media_type != ARTIFACT_MEDIA_TYPE || artifact.media_type != ARTIFACT_MEDIA_TYPE {
            return Err(format!(
                "{ARTIFACT_MEDIA_TYPE} を期待しましたが応答は {} 、実体は {} でした",
                artifact.media_type, item.media_type
            ));
        }
        let expected_uri = format!("{ARTIFACTS_RESOURCE_URI_PREFIX}{}", artifact.artifact_id);
        if artifact.uri != expected_uri {
            return Err(format!(
                "URI が {} であり識別子と対応しません",
                artifact.uri
            ));
        }
        let expires = chrono::DateTime::parse_from_rfc3339(&artifact.expires_at)
            .map_err(|e| format!("失効時刻を解釈できません: {e}"))?;
        if expires <= chrono::Utc::now() {
            return Err(format!(
                "失効時刻 {} が既に過ぎています",
                artifact.expires_at
            ));
        }
        notes.push(format!(
            "フレーム {} の成果物を {} として読み出せた",
            item.response.frame, item.media_type
        ));
    }
    Ok(notes)
}

/// 応答へ現れてはならない語。
///
/// 実体の置き場所も、接続先が書いた引き渡しファイルの識別子も、要求元へは
/// 渡らない。渡れば、要求元は他プロセスのファイルを名指しできる。
const FORBIDDEN_IN_RESPONSE: &[&str] = &["handoff", "token", ".png", ":\\", "\\\\", "secret"];

/// 応答が実体の置き場所も引き渡しの識別子も運ばないことを確かめる。
fn judge_no_location_in_response(rendered: &[Rendered]) -> CheckResult {
    let mut leaks = Vec::new();
    for item in rendered {
        let lowered = item.raw.to_lowercase();
        for forbidden in FORBIDDEN_IN_RESPONSE {
            if lowered.contains(forbidden) {
                leaks.push(format!(
                    "フレーム {} の応答に {forbidden} が現れた",
                    item.response.frame
                ));
            }
        }
    }
    if !leaks.is_empty() {
        return Err(leaks.join(" / "));
    }
    Ok(vec![format!("{} 件の応答を検めた", rendered.len())])
}

/// 描いた絵がプレビューと一致するかを実行者へ尋ねる。
fn judge_visual_match(rendered: &[Rendered], out_dir: &Path) -> CheckResult {
    let mut paths = Vec::new();
    for item in rendered {
        let name = format!("frame-{}", item.response.frame);
        paths.push(write_copy(out_dir, &name, &item.bytes)?);
    }
    let listed = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("\n         ");

    operator_verdict(&format!(
        "いま行ったこと: {} 枚を描き、次の場所へ複製を書き出しました。\n         {listed}\n\
         お願いすること: それぞれの画像を開き、同じフレームの AviUtl2 のプレビューと見比べてください。\n\
         確認する場所: 画像の全体。写っているものと配置。\n\
         回答: どれもプレビューと一致していれば y、1 枚でも違っていれば n を入力してください。",
        rendered.len()
    ))
}

// ---------------------------------------------------------------------------
// 成果物
// ---------------------------------------------------------------------------

/// 成果物が上限で押し出され、期限で見つからなくなることを確かめる。
fn section_artifacts(
    harness: &Harness,
    report: &mut Report,
    target: &Target,
    store: &Arc<ArtifactStore>,
) -> Result<(), String> {
    print_section(
        "成果物",
        "上限を超えるまで描き続け、古い成果物が押し出されることを確かめます。実行者の操作はログの確認だけです。",
    );

    report.record(
        "成果物",
        "件数の上限による押し出し",
        "上限を超えて描くと最も古い成果物が引き当てられなくなり、保持件数が上限を超えない",
        Mode::Auto,
        check_eviction(harness, target, store),
    );

    // 押し出しの規則は件数と総量の両方を見る。ここで動かせるのは件数だけで
    // あることを、黙って落とさずに残す。
    report.skip(
        "成果物",
        "総量の上限による押し出し",
        "保持する総量が上限を超えると、古い成果物が押し出される",
        Mode::Auto,
        format!(
            "総量の上限は {} MiB であり、件数の上限まで描いてもそこへ届かない。\
             届かせるには 1 枚あたり {} MiB を超える解像度が要り、それは 1 枚の上限に掛かる",
            ARTIFACT_MAX_TOTAL_BYTES / (1024 * 1024),
            ARTIFACT_MAX_TOTAL_BYTES / (1024 * 1024) / ARTIFACT_MAX_COUNT as u64
        ),
    );

    let expiry_verified = "保存時間を過ぎた成果物が引き当てられなくなる（応答としての not_found そのものは見ていない）";
    match std::env::var_os(WAIT_FOR_EXPIRY_ENV) {
        Some(_) => report.record(
            "成果物",
            "期限切れ",
            expiry_verified,
            Mode::Auto,
            check_expiry(harness, target, store),
        ),
        None => report.skip(
            "成果物",
            "期限切れ",
            expiry_verified,
            Mode::Auto,
            format!(
                "保存時間は {} 秒であり、実時間で待つと実行が長くなる。{WAIT_FOR_EXPIRY_ENV} を設定すると待って確かめる",
                ARTIFACT_TTL.as_secs()
            ),
        ),
    }

    let outcome = operator_verdict(
        "いま行ったこと: ここまでの確認で、plugin は多数の描画を処理してログへ書き出しました。\n\
         お願いすること: plugin のログを開き、成果物の置き場所（絶対パス）と、\n\
         引き渡しファイルの識別子が書かれていないかを探してください。\n\
         確認する場所: 開発用ディレクトリの data/log にある最新のログファイル。\n\
         回答: どちらも見つからなければ y、1 つでも見つかれば n を入力してください。",
    );
    report.record(
        "成果物",
        "ログへの置き場所と引き渡しの識別子の非混入",
        "plugin のログに成果物の置き場所と引き渡しファイルの識別子が現れない",
        Mode::Operator,
        outcome,
    );

    Ok(())
}

/// 上限を超えて描き、古い成果物が押し出されることを確かめる。
fn check_eviction(harness: &Harness, target: &Target, store: &Arc<ArtifactStore>) -> CheckResult {
    let frame = target.frames()[0];
    let mut oldest = None;
    for round in 0..=ARTIFACT_MAX_COUNT {
        let response = harness
            .render_frame(&target.id, target.scene_id, frame)
            .map_err(|error| {
                format!(
                    "{} 回目の描画に失敗しました: {}",
                    round + 1,
                    describe_error(&error)
                )
            })?;
        if oldest.is_none() {
            oldest = Some(response.artifact.artifact_id.clone());
        }
    }

    let oldest = oldest.ok_or_else(|| "1 度も描けませんでした".to_string())?;
    if store.get(&oldest).is_some() {
        return Err("上限を超えて描いても最も古い成果物が残っています".to_string());
    }
    let held = store.len();
    if held > ARTIFACT_MAX_COUNT {
        return Err(format!(
            "保持件数が上限 {ARTIFACT_MAX_COUNT} を超えて {held} 件あります"
        ));
    }
    Ok(vec![format!(
        "{} 回描いた後の保持件数は {held} 件で、最初の成果物は引き当てられない",
        ARTIFACT_MAX_COUNT + 1
    )])
}

/// 保存時間を実時間で待ち、成果物が引き当てられなくなることを確かめる。
fn check_expiry(harness: &Harness, target: &Target, store: &Arc<ArtifactStore>) -> CheckResult {
    let response = require(
        harness.render_frame(&target.id, target.scene_id, target.frames()[0]),
        "期限切れの確認用の描画に失敗しました",
    )?;
    let id = response.artifact.artifact_id.clone();
    if store.get(&id).is_none() {
        return Err("描いた直後の成果物を引き当てられません".to_string());
    }

    let wait = ARTIFACT_TTL + POLL_INTERVAL;
    println!("保存時間が過ぎるまで {} 秒待ちます。", wait.as_secs());
    std::thread::sleep(wait);

    if store.get(&id).is_some() {
        return Err("保存時間を過ぎても引き当てられます".to_string());
    }
    Ok(vec![format!(
        "{} 秒待つと引き当てられなくなった",
        wait.as_secs()
    )])
}

// ---------------------------------------------------------------------------
// 中断
// ---------------------------------------------------------------------------

/// 中断されたときに AviUtl2 が落ちず、応答不能にもならないことを確かめる。
///
/// **合格の条件は「エラーが返ること」ではない。** 要求元が既に居ない手順では
/// 応答は誰にも届かず、終了の途中の手順では応答より終了が先に来る。見るのは
/// ホストの生死だけである。
fn section_interruption(
    harness: &Harness,
    short: &Harness,
    report: &mut Report,
    target: Target,
    layout: &Layout,
    store: &Arc<ArtifactStore>,
) -> Result<(), String> {
    print_section(
        "中断",
        "出力中の描画、期限の超過、要求元の消滅、描画中の終了を試します。AviUtl2 の終了と再起動を繰り返しお願いします。",
    );
    println!("合格の条件: いずれの手順でも AviUtl2 が落ちず、応答不能にならないことです。");
    println!("            エラーが返ることは条件ではありません。要求元が既に居ない手順では");
    println!("            応答は誰にも届かず、終了の途中では応答より終了が先に来ます。");
    println!("            プレビュー再生中の描画は、合否ではなく観測として先に記録済みです。");

    let attempt = check_render_during_save(harness, &target);
    report.record_attempt(
        "中断",
        "出力中の描画",
        "出力中の描画要求が edit_blocked になり、出力の終了後に AviUtl2 が応答を続ける",
        Mode::Operator,
        attempt,
    );

    let attempt = check_short_deadline(harness, short, &target);
    report.record_attempt(
        "中断",
        "期限を超えた描画",
        "期限を超えた描画の後も AviUtl2 が応答を続け、次の描画が成功する",
        Mode::Auto,
        attempt,
    );

    let attempt = check_doomed_client(harness, report, &target, layout, store);
    report.record_attempt(
        "中断",
        "要求直後の要求元の消滅",
        "描画を要求した直後に要求元が消えても、AviUtl2 が応答を続け、引き渡しの取り残しが出ない",
        Mode::Auto,
        attempt,
    );

    let target = check_quit_during_render(harness, report, target, layout, store)?;
    check_repeated_shutdown(harness, short, report, target, &layout.registry_dir)?;
    Ok(())
}

/// 出力中の描画が型付きの失敗になることを確かめる。
fn check_render_during_save(harness: &Harness, target: &Target) -> Attempt {
    let answer = ask(
        "お願いすること: AviUtl2 で出力（ファイル書き出し）を開始してください。\n\
         出力を止めないまま、この画面へ戻ってきてください。\n\
         確認する場所: 出力の進行状況を示すウィンドウ。\n\
         回答: 出力中の状態で Enter を押してください。\n\
         出力を開始できない場合は skip と入力してください。この項目は未実施になります。",
    );
    if answer.eq_ignore_ascii_case("skip") {
        return Attempt::Unmet("出力を開始できないため実施できません".to_string());
    }

    let result = harness.render_frame(&target.id, target.scene_id, 0);
    prompt(
        "お願いすること: AviUtl2 で出力を停止するか、出力の完了を待ってください。\n\
         確認する場所: 出力の進行状況を示すウィンドウが閉じていること。\n\
         回答: 出力が終わったら Enter を押してください。",
    );

    let blocked = match result {
        Ok(_) => return Attempt::Ran(Err("出力中の描画が成功として返りました".to_string())),
        Err(error) if error.code == ErrorCode::EditBlocked => describe_error(&error),
        Err(error) => {
            return Attempt::Ran(Err(format!(
                "edit_blocked を期待しましたが {}",
                describe_error(&error)
            )));
        }
    };

    Attempt::Ran(
        wait_until_responsive(harness, &target.id, HOST_RECOVERY_WINDOW)
            .map_err(|reason| format!("出力の終了後に AviUtl2 が応答しません: {reason}"))
            .map(|waited| {
                vec![
                    blocked,
                    format!("出力の終了後 {} ミリ秒で応答した", waited.as_millis()),
                ]
            }),
    )
}

/// 期限を短縮した描画の後も、AviUtl2 が使えることを確かめる。
///
/// 短縮するのは要求元が待つ長さだけである。接続先は完了の合図を待ち続けており、
/// **次の要求はその待ちが終わるまで接続できない。** 1 度の失敗で応答不能と
/// 決めないよう、猶予を置いて繰り返す。
fn check_short_deadline(harness: &Harness, short: &Harness, target: &Target) -> Attempt {
    let result = short.render_frame(&target.id, target.scene_id, target.frames()[0]);
    let timed_out = match result {
        Ok(_) => {
            return Attempt::Unmet(format!(
                "{} ミリ秒の期限内に描画が完了したため、期限の超過を起こせなかった。\
                 より重いシーンで実行すると確かめられる",
                SHORT_RENDER_BUDGET.as_millis()
            ));
        }
        Err(error) if error.code == ErrorCode::Timeout => describe_error(&error),
        Err(error) => {
            return Attempt::Ran(Err(format!(
                "timeout を期待しましたが {}",
                describe_error(&error)
            )));
        }
    };

    let outcome = wait_until_responsive(harness, &target.id, HOST_RECOVERY_WINDOW)
        .map_err(|reason| format!("期限の超過の後に AviUtl2 が応答しません: {reason}"))
        .and_then(|waited| {
            let response = render_with_retry(harness, target, HOST_RECOVERY_WINDOW)?;
            Ok(vec![
                timed_out,
                format!("{} ミリ秒後に応答が戻った", waited.as_millis()),
                format!(
                    "その後の描画が成功した（frame={} {}x{}）",
                    response.frame, response.width, response.height
                ),
            ])
        });
    Attempt::Ran(outcome)
}

/// 成功するまで描画を繰り返す。
fn render_with_retry(
    harness: &Harness,
    target: &Target,
    window: Duration,
) -> Result<RenderResponse, String> {
    let started = Instant::now();
    loop {
        let last = match harness.render_frame(&target.id, target.scene_id, target.frames()[0]) {
            Ok(response) => return Ok(response),
            Err(error) => describe_error(&error),
        };
        if started.elapsed() >= window {
            return Err(format!(
                "{} 秒繰り返しても描画できません: {last}",
                window.as_secs_f64()
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// 要求を送った直後に消える要求元を、子プロセスとして用意して確かめる。
///
/// **実行者には実施できない。** 要求元は人ではなく、要求を組み立てて送る
/// プロセスである。要求を送った直後に自分を強制終了することを人へ求める形に
/// なるため、本ターゲット自身が同じ役目の子を起こして落とす。
fn check_doomed_client(
    harness: &Harness,
    report: &mut Report,
    target: &Target,
    layout: &Layout,
    store: &Arc<ArtifactStore>,
) -> Attempt {
    match probe_doomed_client(harness, report, target, layout, store) {
        Ok(attempt) => attempt,
        Err(reason) => Attempt::Ran(Err(reason)),
    }
}

/// 子を起こして落とし、結末を判定する。
///
/// 起こせなかった場合と、狙った形にならなかった場合を分ける。**前者は確認の
/// 失敗であり、後者は前提が揃わなかっただけである。**
fn probe_doomed_client(
    harness: &Harness,
    report: &mut Report,
    target: &Target,
    layout: &Layout,
    store: &Arc<ArtifactStore>,
) -> Result<Attempt, String> {
    let started = layout.out_dir.join("doomed-client-started.txt");
    let returned = layout.out_dir.join("doomed-client-returned.txt");
    let _ = std::fs::remove_file(&started);
    let _ = std::fs::remove_file(&returned);

    let exe =
        std::env::current_exe().map_err(|e| format!("自分の実行ファイルが分かりません: {e}"))?;
    let mut child = Command::new(exe)
        .arg(DOOMED_CLIENT_ARG)
        .arg(&target.id)
        .arg(target.scene_id.to_string())
        .arg(target.frames()[0].to_string())
        .arg(&started)
        .arg(&returned)
        .env(REGISTRY_DIR_ENV, &layout.registry_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("要求元の役目の子を起こせません: {e}"))?;

    std::thread::sleep(DOOMED_CLIENT_LIFETIME);
    let killed = child.kill();
    let _ = child.wait();
    killed.map_err(|e| format!("子を落とせません: {e}"))?;

    if returned.exists() {
        // 落とす前に応答まで進んでいた。確かめたい形になっていない。
        let note = std::fs::read_to_string(&returned).unwrap_or_default();
        return Ok(Attempt::Unmet(format!(
            "子が落とされる前に応答まで進んだため、要求元の消滅を起こせなかった（{note}）。\
             より重いシーンで実行するか、落とすまでの待ち（{} ミリ秒）を縮めると確かめられる",
            DOOMED_CLIENT_LIFETIME.as_millis()
        )));
    }
    if !started.exists() {
        // 要求が一度も送られていない。この後に見る「ホストが応答する」ことも
        // 「取り残しが無い」ことも自明に成立してしまうため、確認として数えない。
        return Ok(Attempt::Unmet(format!(
            "子が要求を送る前に落ちたため、要求元の消滅を起こせなかった。\
             接続と handshake に {} ミリ秒では足りていない",
            DOOMED_CLIENT_LIFETIME.as_millis()
        )));
    }

    let waited = wait_until_responsive(harness, &target.id, HOST_RECOVERY_WINDOW)
        .map_err(|reason| format!("要求元が消えた後に AviUtl2 が応答しません: {reason}"))?;

    let log = ask(
        "いま行ったこと: 描画を要求した直後に、その要求元のプロセスを強制終了しました。\n\
         お願いすること: plugin のログを開き、応答を送れなかったことと、\n\
         引き渡し用のファイルを片付けたことが記録されているかを見てください。\n\
         確認する場所: 開発用ディレクトリの data/log にある最新のログファイル。\n\
         回答: 両方あれば 両方\n\
         片方だけなら 片方\n\
         どちらも無ければ 無し\n\
         確かめられなければ 未確認 と入力してください。",
    );
    report.observe(
        "doomed_client_log",
        "要求元が消えたとき、送信の失敗と引き渡しの片付けが記録に残るか",
        if log.is_empty() {
            "未回答".to_string()
        } else {
            log
        },
    );

    let leftovers = settle_leftovers(&layout.base_dir, store)?;
    Ok(Attempt::Ran(Ok(vec![
        "子は要求を送った後に落とされた".to_string(),
        format!("{} ミリ秒後に応答が戻った", waited.as_millis()),
        leftovers,
    ])))
}

/// 引き渡しの取り残しが無くなるまで少し待ち、結果を返す。
///
/// 保管庫が保持している件数と、基底の下にある画像ファイルの数が一致すれば、
/// 引き渡し用のファイルは 1 つも残っていない。
fn settle_leftovers(base_dir: &Path, store: &Arc<ArtifactStore>) -> Result<String, String> {
    let started = Instant::now();
    loop {
        let held = store.len();
        let found = count_png_files(base_dir);
        if found == held {
            return Ok(format!(
                "画像ファイルは保管庫の {held} 件だけで、取り残しは無い"
            ));
        }
        if started.elapsed() >= LEFTOVER_SETTLE_WINDOW {
            return Err(format!(
                "保管庫が持つのは {held} 件ですが、基底の下には {found} 件の画像ファイルがあります"
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// 描画の実行中に AviUtl2 を終了し、終了が期限内に完了することを確かめる。
fn check_quit_during_render(
    harness: &Harness,
    report: &mut Report,
    target: Target,
    layout: &Layout,
    store: &Arc<ArtifactStore>,
) -> Result<Target, String> {
    let (sender, receiver) = channel::<String>();
    let registry = layout.registry_dir.clone();
    let store_for_thread = Arc::clone(store);
    let id = target.id.clone();
    let scene_id = target.scene_id;
    let frame = target.frames()[0];
    let waiting = std::thread::spawn(move || {
        let harness = Harness::new(registry, CallLimits::default(), store_for_thread);
        let result = harness.render_frame(&id, scene_id, frame);
        let _ = sender.send(describe_render(&result));
    });

    // 要求が接続先へ届き、描画が始まるだけの間を置く。
    std::thread::sleep(DOOMED_CLIENT_LIFETIME);
    prompt(
        "いま行ったこと: 描画を 1 件要求し、その完了を待っている最中です。\n\
         お願いすること: いま AviUtl2 を終了してください。\n\
         確認する場所: AviUtl2 のウィンドウ。\n\
         回答: 終了の操作をした直後に Enter を押してください。終了に要した時間を測ります。",
    );

    let elapsed = wait_until_descriptor_gone(&layout.registry_dir, &target.id, SHUTDOWN_LIMIT);
    let note = receiver
        .recv_timeout(HOST_RECOVERY_WINDOW)
        .unwrap_or_else(|_| "要求元は応答も失敗も受け取らなかった".to_string());
    let _ = waiting.join();

    let outcome = judge_shutdown(elapsed, "描画の実行中の終了").and_then(|mut notes| {
        notes.push(format!("要求元が観測した結末: {note}"));
        notes.push(settle_leftovers(&layout.base_dir, store)?);
        Ok(notes)
    });
    report.record(
        "中断",
        "描画の実行中の終了",
        "描画の完了を待っている最中に終了しても、AviUtl2 が期限内に終了し、異常終了しない",
        Mode::Operator,
        outcome,
    );

    relaunch(harness)
}

/// 終了に要した時間と異常終了の有無を判定する。
fn judge_shutdown(elapsed: Result<Duration, String>, what: &str) -> CheckResult {
    let elapsed = elapsed.map_err(|reason| format!("{what}が完了しません: {reason}"))?;
    if !confirm(
        "お願いすること: AviUtl2 が異常終了したことを示す表示が出ていないかを見てください。\n\
         確認する場所: 画面全体。エラーダイアログや、応答なしのウィンドウ。\n\
         回答: 異常終了の表示が無ければ y、出ていれば n を入力してください。",
    ) {
        return Err(format!("{what}で異常終了の表示が出た"));
    }
    Ok(vec![format!(
        "終了は {} ミリ秒で完了した（上限 {} 秒）",
        elapsed.as_millis(),
        SHUTDOWN_LIMIT.as_secs()
    )])
}

/// AviUtl2 を起動し直してもらい、新しいインスタンスを掴み直す。
fn relaunch(harness: &Harness) -> Result<Target, String> {
    prompt(
        "お願いすること: AviUtl2 を起動し直し、同じプロジェクトを開いてください。\n\
         確認する場所: AviUtl2 のタイトルバー。plugin が読み込まれていること。\n\
         回答: 開き終えたら Enter を押してください。",
    );
    wait_for_single_instance(harness)
}

/// 完了を待っている描画を残したまま終了する手順を、繰り返して確かめる。
///
/// **1 回通っただけでは合格と言えない。** 期限内に完了待ちが戻ったのか、
/// たまたま完了の合図が来なかっただけなのかを区別できないためである。
/// 切り離しが常態なら、アンロードの後に届く合図を防げていないことになる。
fn check_repeated_shutdown(
    harness: &Harness,
    short: &Harness,
    report: &mut Report,
    target: Target,
    registry_dir: &Path,
) -> Result<(), String> {
    println!();
    println!("繰り返し: 描画を残したまま終了する手順を {SHUTDOWN_REPEATS} 回繰り返します。");
    println!("          毎回、AviUtl2 の終了と再起動をお願いします。");
    println!("          各回について、完了待ちを切り離した記録が出たかどうかを見ます。");
    println!("          切り離しが常態であれば、それは合格ではありません。アンロードの後に");
    println!("          届く完了の合図を防げていないことを意味します。");

    let mut rounds: Vec<DrainRound> = Vec::new();
    let mut current = Some(target);
    for round in 1..=SHUTDOWN_REPEATS {
        let Some(target) = current.take() else {
            break;
        };
        println!();
        println!("--- {round} 回目 / {SHUTDOWN_REPEATS} 回 ---");

        let before = detach_marks();
        let result = short.render_frame(&target.id, target.scene_id, target.frames()[0]);
        // 期限内に終わってしまった回は、完了を待っている描画を残せていない。
        // 終了が速かったのは、待つものが無かったからかもしれない。
        let left_outstanding = matches!(&result, Err(error) if error.code == ErrorCode::Timeout);
        let render = describe_render(&result);
        prompt(&format!(
            "いま行ったこと: 期限を短縮した描画を 1 件要求しました（結果: {render}）。\n\
             接続先はまだ完了の合図を待っています。\n\
             お願いすること: いま AviUtl2 を終了してください。\n\
             回答: 終了の操作をした直後に Enter を押してください。"
        ));
        let elapsed = wait_until_descriptor_gone(registry_dir, &target.id, SHUTDOWN_LIMIT);
        let detached = detached_since(before);
        let crashed = !confirm(
            "お願いすること: AviUtl2 が異常終了したことを示す表示が出ていないかを見てください。\n\
             確認する場所: 画面全体。エラーダイアログや、応答なしのウィンドウ。\n\
             回答: 異常終了の表示が無ければ y、出ていれば n を入力してください。",
        );
        rounds.push(DrainRound {
            round,
            render,
            left_outstanding,
            elapsed,
            detached,
            crashed,
        });

        if round < SHUTDOWN_REPEATS {
            let answer = ask(
                "お願いすること: AviUtl2 を起動し直し、同じプロジェクトを開いてください。\n\
                 回答: 開き終えたら Enter を押してください。\n\
                 ここで打ち切る場合は stop と入力してください。繰り返した回数とともに記録します。",
            );
            if answer.eq_ignore_ascii_case("stop") {
                break;
            }
            current = Some(wait_for_single_instance(harness)?);
        }
    }

    report_drain_rounds(report, &rounds);
    Ok(())
}

/// 描画を残したまま終了した 1 回の記録。
struct DrainRound {
    round: usize,
    /// 期限を短縮した描画の結末。
    render: String,
    /// 完了を待っている描画を残したまま終了できたか。
    left_outstanding: bool,
    /// 終了に要した時間。
    elapsed: Result<Duration, String>,
    /// 完了待ちを切り離したか。
    detached: Detached,
    /// 異常終了の表示が出たか。
    crashed: bool,
}

/// 完了待ちを切り離したかどうか。
enum Detached {
    /// 切り離した。
    Yes,
    /// 期限内に戻った。
    No,
    /// 分からなかった。
    Unknown(String),
}

impl Detached {
    fn label(&self) -> String {
        match self {
            Detached::Yes => "切り離した".to_string(),
            Detached::No => "期限内に戻った".to_string(),
            Detached::Unknown(reason) => format!("不明（{reason}）"),
        }
    }
}

/// 繰り返した結果を判定と観測へ落とす。
fn report_drain_rounds(report: &mut Report, rounds: &[DrainRound]) {
    let details = rounds
        .iter()
        .map(|round| {
            format!(
                "{} 回目: 描画={} / 待っている描画を残せた={} / 終了={} / 完了待ち={} / 異常終了の表示={}",
                round.round,
                round.render,
                round.left_outstanding,
                match &round.elapsed {
                    Ok(elapsed) => format!("{} ミリ秒", elapsed.as_millis()),
                    Err(reason) => reason.clone(),
                },
                round.detached.label(),
                round.crashed
            )
        })
        .collect::<Vec<_>>()
        .join(" ／ ");
    report.observe(
        "render_drain_rounds",
        "描画を残したまま終了したとき、完了待ちは期限内に戻るか",
        if details.is_empty() {
            "1 回も実施していない".to_string()
        } else {
            details
        },
    );

    // 観測された異常を先に判定する。**繰り返しが足りなかったことや、狙った
    // 状況を起こせなかったことで、異常終了や期限超過が飲み込まれてはならない。**
    // 実施した回が 1 回でもあれば、その回についての主張は成り立つ。
    report_shutdown_health(report, rounds);
    report_repetition(report, rounds);
}

/// 実施した各回について、ホストが無事に終了したかを判定する。
fn report_shutdown_health(report: &mut Report, rounds: &[DrainRound]) {
    let title = "描画を残したままの終了でのホストの生死";
    let verified =
        "描画を残したまま終了した回のいずれでも、AviUtl2 が異常終了せず、期限内に終了する";
    if rounds.is_empty() {
        report.skip(
            "中断",
            title,
            verified,
            Mode::Operator,
            "この手順を 1 回も実施していない",
        );
        return;
    }

    let mut failed: Vec<String> = rounds
        .iter()
        .filter(|round| round.crashed)
        .map(|round| format!("{} 回目: 異常終了の表示が出た", round.round))
        .collect();
    failed.extend(rounds.iter().filter_map(|round| {
        round
            .elapsed
            .as_ref()
            .err()
            .map(|reason| format!("{} 回目: {reason}", round.round))
    }));

    let outcome = if failed.is_empty() {
        Ok(vec![format!(
            "実施した {} 回とも、異常終了の表示が出ず期限内に終了した",
            rounds.len()
        )])
    } else {
        Err(format!(
            "実施した {} 回のうちに、異常終了または期限内に終了しなかった回があります: {}",
            rounds.len(),
            failed.join(" / ")
        ))
    };
    report.record("中断", title, verified, Mode::Operator, outcome);
}

/// 繰り返しが足りているかと、切り離しが常態でないかを判定する。
fn report_repetition(report: &mut Report, rounds: &[DrainRound]) {
    let title = "描画を残したままの終了の繰り返し";
    let verified = format!(
        "描画を残したまま終了する手順を {SHUTDOWN_REPEATS} 回繰り返し、完了待ちの切り離しが常態にならない"
    );
    if rounds.len() < SHUTDOWN_REPEATS {
        report.skip(
            "中断",
            title,
            verified,
            Mode::Operator,
            format!(
                "{SHUTDOWN_REPEATS} 回を要するところ {} 回しか実施していない。\
                 1 回通っただけでは、期限内に戻ったのか、たまたま完了の合図が来なかっただけなのかを区別できない",
                rounds.len()
            ),
        );
        return;
    }

    let outstanding = rounds.iter().filter(|round| round.left_outstanding).count();
    if outstanding == 0 {
        report.skip(
            "中断",
            title,
            verified,
            Mode::Operator,
            format!(
                "{} 回とも短縮した期限内に描画が完了しており、完了を待っている描画を残したまま終了できていない。\
                 より重いシーンで実行すると確かめられる",
                rounds.len()
            ),
        );
        return;
    }

    let detached = rounds
        .iter()
        .filter(|round| matches!(round.detached, Detached::Yes))
        .count();
    let unknown = rounds
        .iter()
        .filter(|round| matches!(round.detached, Detached::Unknown(_)))
        .count();
    // 切り離しが常態なら、それは合格ではない。切り離した先へ届く完了の合図は
    // アンロード済みの領域へ飛ぶ。
    let outcome = if detached * 2 > rounds.len() {
        Err(format!(
            "{} 回中 {detached} 回で完了待ちを切り離しており、切り離しが常態になっています。\
             切り離した先で届く完了の合図は防げていません",
            rounds.len()
        ))
    } else {
        // **観測した範囲を超えて主張しない。** 切り離しの記録が無い回は、完了待ちが
        // 期限内に戻ったのかもしれないし、在庫が空で待ちに入らなかったのかもしれない。
        // plugin は待って戻ったことを記録しないため、外からは区別できない。
        Ok(vec![
            format!(
                "{} 回実施し、完了を待っている描画を残せたのは {outstanding} 回、\
                 切り離しの記録が出たのは {detached} 回（判別できなかった回 {unknown}）",
                rounds.len()
            ),
            "切り離しの記録が無い回は、期限内に戻ったのか、そもそも完了待ちに入らなかったのかを区別できない"
                .to_string(),
        ])
    };
    report.record("中断", title, verified, Mode::Operator, outcome);
}

/// plugin のログに現れる、完了待ちを切り離した回数を数える。
///
/// ログの置き場が分かる場合だけ数えられる。分からなければ `None` を返し、
/// 呼び出し側が実行者へ尋ねる。
fn detach_marks() -> Option<usize> {
    let dir = PathBuf::from(std::env::var_os(PLUGIN_LOG_DIR_ENV)?);
    let entries = std::fs::read_dir(dir).ok()?;
    let mut count = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        count += String::from_utf8_lossy(&bytes)
            .matches(DRAIN_DETACH_MARKER)
            .count();
    }
    Some(count)
}

/// 直前の回から、完了待ちの切り離しが増えたかを見る。
fn detached_since(before: Option<usize>) -> Detached {
    if let (Some(before), Some(after)) = (before, detach_marks()) {
        return if after > before {
            Detached::Yes
        } else {
            Detached::No
        };
    }

    let answer = ask(&format!(
        "お願いすること: plugin のログを開き、いまの終了で\n\
         「{DRAIN_DETACH_MARKER}」で終わる警告が新しく出ているかを見てください。\n\
         これは完了待ちが期限内に戻らず、待ちを切り離して終了へ進んだことを表します。\n\
         完了待ちについての記録は他にもあるため、この語尾まで一致するものだけを数えてください。\n\
         確認する場所: 開発用ディレクトリの data/log にある最新のログファイルの末尾。\n\
         回答: 出ていれば あり\n\
         出ていなければ なし\n\
         確かめられなければ 未確認 と入力してください。\n\
         {PLUGIN_LOG_DIR_ENV} にログの置き場を設定すると、この確認は自動になります。"
    ));
    match answer.as_str() {
        "あり" => Detached::Yes,
        "なし" => Detached::No,
        "" => Detached::Unknown("未回答".to_string()),
        other => Detached::Unknown(other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// 後片付け
// ---------------------------------------------------------------------------

/// AviUtl2 の終了後に、引き渡し用のファイルが残らないことを確かめる。
fn section_leftovers(report: &mut Report, base_dir: &Path, store: &Arc<ArtifactStore>) {
    print_section(
        "後片付け",
        "AviUtl2 の終了後に引き渡し用のファイルが残らないこと、保管庫を閉じると成果物が消えることを確かめます。実行者の操作はありません。",
    );

    let outcome = settle_leftovers(base_dir, store)
        .map(|note| vec![note])
        .map_err(|reason| format!("AviUtl2 の終了後に取り残しがあります: {reason}"));
    report.record(
        "後片付け",
        "AviUtl2 終了後の引き渡しの取り残し",
        "AviUtl2 の終了後、引き渡し用のファイルが 1 つも残らない（空になったディレクトリの有無は見ていない）",
        Mode::Auto,
        outcome,
    );
}

/// 保管庫を閉じると、保持していた成果物が実体ごと消えることを確かめる。
fn section_store_removal(report: &mut Report, store: Arc<ArtifactStore>, base_dir: &Path) {
    let held = store.len();
    let before = count_png_files(base_dir);
    drop(store);
    let after = count_png_files(base_dir);

    let outcome = if held == 0 {
        Err("閉じる前に成果物を 1 件も保持していないため、消えたことを確かめられません".to_string())
    } else if after == 0 {
        Ok(vec![format!(
            "閉じる前は {before} 件、閉じた後は 0 件になった（保持していたのは {held} 件）"
        )])
    } else {
        Err(format!(
            "閉じた後も {after} 件の画像ファイルが残っています（閉じる前は {before} 件）"
        ))
    };
    report.record(
        "後片付け",
        "保管庫を閉じたときの削除",
        "保管庫を閉じると、保持していた成果物の実体が残らない（空になったディレクトリの有無は見ていない）",
        Mode::Auto,
        outcome,
    );

    report.skip(
        "後片付け",
        "強制終了した保管庫の掃除",
        "次に保管庫を開いたとき、十分に古い放置ディレクトリが消え、新しいものは消えない",
        Mode::Auto,
        "実行者に実施できない。掃除の対象になるのは、持ち主が居らず十分に古い放置ディレクトリだけである。\
         その古さは実時間で計られるため、1 回の実行の中では作れない。日をまたいで確かめる形にすると、\
         実行者は前回の実行の残骸を手で見分けることになり、判定の材料が実行者の記憶になる",
    );
}

// ---------------------------------------------------------------------------
// 落とされるためだけの要求元
// ---------------------------------------------------------------------------

/// 子として起こされたときに渡される役目。
struct DoomedRole {
    instance_id: String,
    scene_id: i32,
    frame: u32,
    /// 要求を送る直前に置く印。
    started: PathBuf,
    /// 落とされる前に応答まで進んだ場合に置く印。
    returned: PathBuf,
}

/// 引数が子としての役目を表しているなら、その内容を返す。
fn doomed_client_role(args: &[String]) -> Option<DoomedRole> {
    if args.get(1).map(String::as_str) != Some(DOOMED_CLIENT_ARG) {
        return None;
    }
    Some(DoomedRole {
        instance_id: args.get(2)?.clone(),
        scene_id: args.get(3)?.parse().ok()?,
        frame: args.get(4)?.parse().ok()?,
        started: PathBuf::from(args.get(5)?),
        returned: PathBuf::from(args.get(6)?),
    })
}

/// 描画を 1 件要求し、落とされるのを待つだけの要求元として振る舞う。
///
/// **成果物の保管庫を開かない。** 受け取る前に落とされる役目であり、開くと
/// 落とされた後に持ち主の居ないディレクトリが残る。要求は接続先へ直接送る。
///
/// 印を 2 つ置く。要求を送る直前の印が無ければ、そもそも要求が届いていない。
/// 応答まで進んだ印があれば、落とすのが遅く、確かめたい形になっていない。
/// **どちらも「落としてもホストが無事だった」とは違う結末であり、印が無ければ
/// 区別できないまま確認が通ってしまう。**
fn run_as_doomed_client(role: &DoomedRole) {
    let Ok(registry_dir) = registry_dir() else {
        return;
    };
    let Ok(instance_id) = parse_instance_id(&role.instance_id) else {
        return;
    };
    let limits = CallLimits::default();
    let Ok(resolved) = resolve_instance(&registry_dir, instance_id, DiscoveryConfig::default())
    else {
        return;
    };
    let params = RenderFrameParams {
        expected_scene_id: role.scene_id,
        frame: role.frame,
        format: RenderFormat::Png,
    };

    let _ = std::fs::write(&role.started, "要求を送る");
    let deadline = Instant::now() + limits.ipc_request_budget(OPERATION_RENDER_FRAME);
    let result = resolved.client.request_typed::<_, serde_json::Value>(
        OPERATION_RENDER_FRAME,
        &params,
        deadline,
    );
    let _ = std::fs::write(
        &role.returned,
        match result {
            Ok(_) => "応答を受け取った".to_string(),
            Err(error) => format!("失敗を受け取った: {error}"),
        },
    );
}
