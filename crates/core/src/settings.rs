//! plugin と server が共有する設定ファイル。
//!
//! 設定は 1 つのファイルを全プロセスが共有する。書き手は plugin（設定画面）で
//! あり、読み手は plugin と server の両方である。**書き手が読み手より新しい
//! build であり得る**ため、読み手は知らない内容を落とさない。
//!
//! - 未知の top-level field は許容し、読み飛ばす。書き戻すときは保持する。
//! - `disabled_tools` の未知の tool 名は無視するが、書き戻すときは保持する。
//! - 未記載の項目は既定値で動く。既定値は現在の定数そのものであり保守側である。
//!
//! # 破損しても動き続ける
//!
//! 範囲外の値は拒否せず境界へ丸め、型が違う項目だけを既定値へ戻す。1 項目の
//! ために全体を破損扱いにすると、設定画面がファイルを読めなくなり、画面から
//! 直せなくなる。丸めと差し戻しは [`SettingsIssue`] として呼び出し元へ返る。

use crate::budget::{BudgetInequality, ScaledBudgets};
use crate::render::ARTIFACT_MAX_BYTES;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// 設定ファイルの場所を上書きする環境変数の名前。
///
/// 指す先は**ファイルのパス**である。registry の上書き
/// （`AVIUTL2_MCP_REGISTRY_DIR`）がディレクトリを指すのと対になる。
pub const SETTINGS_FILE_ENV: &str = "AVIUTL2_MCP_SETTINGS_FILE";

/// 基底ディレクトリの直下に置く設定ファイルの名前。
pub const SETTINGS_FILE_NAME: &str = "settings.json";

/// 設定ファイルの schema 版。
///
/// **フィールドの追加では上げない。** 未知の top-level field は読み飛ばされる
/// ため、追加は既存の読み手を壊さない。上げるのは既存フィールドの意味が変わる
/// ときだけである。
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;

/// 読み取りが一時的に失敗したときに試みる回数。
///
/// 原子的置換の最中に掴んだ場合を想定した有限回の再試行であり、これを使い
/// 切っても読めなければ破損として扱い、直前の snapshot を維持する。
pub const SETTINGS_READ_ATTEMPTS: u32 = 3;

const FIELD_SCHEMA_VERSION: &str = "schema_version";
const FIELD_DISABLED_TOOLS: &str = "disabled_tools";
const FIELD_LOG_LEVEL: &str = "log_level";
const FIELD_BUDGET_SCALE_PERCENT: &str = "budget_scale_percent";
const FIELD_ARTIFACT: &str = "artifact";
const FIELD_HANDOFF: &str = "handoff";
const FIELD_RENDER: &str = "render";
const FIELD_SESSION: &str = "session";
const FIELD_TTL_SECONDS: &str = "ttl_seconds";
const FIELD_MAX_COUNT: &str = "max_count";
const FIELD_MAX_TOTAL_BYTES: &str = "max_total_bytes";
const FIELD_DRAIN_TIMEOUT_MS: &str = "drain_timeout_ms";
const FIELD_STALE_AFTER_SECONDS: &str = "stale_after_seconds";

/// 既定のログレベル。`RUST_LOG` と同じ書式で解釈する。
///
/// operation・correlation_id・所要時間・結果コードの記録は運用上の要求であり、
/// 何も設定しない利用者でも失われない水準を選ぶ。
pub const DEFAULT_LOG_LEVEL: &str = "info";

/// 開発ビルドで、設定に記載が無いときに用いるログレベル。
pub const DEVELOPMENT_LOG_LEVEL: &str = "debug";

/// 設定にログレベルの記載が無いときに用いる値。
///
/// **既定値が 2 つあるのではない。** 選び分けているのは「未記載のときにどれを
/// 採るか」だけであり、plugin と server はこの 1 つの規則を共有する。開発
/// ビルドで詳しく記録するのは、不具合の再現がその場で行われるためである。
pub fn default_log_level() -> &'static str {
    if cfg!(debug_assertions) {
        DEVELOPMENT_LOG_LEVEL
    } else {
        DEFAULT_LOG_LEVEL
    }
}

/// 予算倍率の既定値（百分率）。
pub const DEFAULT_BUDGET_SCALE_PERCENT: u32 = 100;
/// 予算倍率の下限。全ての予算がミリ秒の桁で意味を保つ最小の水準。
pub const MIN_BUDGET_SCALE_PERCENT: u32 = 10;
/// 予算倍率の上限。これ以上長くすると要求元の期限に先に当たる。
pub const MAX_BUDGET_SCALE_PERCENT: u32 = 400;

/// 成果物の保存時間の既定値（秒）。
pub const DEFAULT_ARTIFACT_TTL_SECONDS: u64 = 600;
/// 成果物の保存時間の下限（秒）。
pub const MIN_ARTIFACT_TTL_SECONDS: u64 = 60;
/// 成果物の保存時間の上限（秒）。
pub const MAX_ARTIFACT_TTL_SECONDS: u64 = 3600;

/// 同時に保持する成果物の件数の既定値。
pub const DEFAULT_ARTIFACT_MAX_COUNT: u64 = 16;
/// 同時に保持する成果物の件数の下限。
pub const MIN_ARTIFACT_MAX_COUNT: u64 = 1;
/// 同時に保持する成果物の件数の上限。
pub const MAX_ARTIFACT_MAX_COUNT: u64 = 64;

/// 同時に保持する成果物の総量の既定値（バイト）。
pub const DEFAULT_ARTIFACT_MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
/// 同時に保持する成果物の総量の下限（バイト）。
///
/// **1 件分の上限（[`ARTIFACT_MAX_BYTES`]）を下回らせない。** 下回ると、
/// 1 件分の上限を満たす成果物が 1 つも入らない store ができる。
pub const MIN_ARTIFACT_MAX_TOTAL_BYTES: u64 = ARTIFACT_MAX_BYTES;
/// 同時に保持する成果物の総量の上限（バイト）。
pub const MAX_ARTIFACT_MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

/// 引き渡し用ファイルを掃除するまでの時間の既定値（秒）。
pub const DEFAULT_HANDOFF_TTL_SECONDS: u64 = 120;
/// 引き渡し用ファイルを掃除するまでの時間の下限（秒）。
///
/// **実際の下限はこの値と「倍率を適用した描画の要求フェーズ予算」の長い方で
/// ある。** 引き取り中のファイルを消さないための最低限であり、予算を伸ばせば
/// 引き取りに掛かり得る時間も伸びる。
pub const MIN_HANDOFF_TTL_SECONDS: u64 = 30;
/// 引き渡し用ファイルを掃除するまでの時間の上限（秒）。
pub const MAX_HANDOFF_TTL_SECONDS: u64 = 1800;

/// 放置された session ディレクトリとみなす古さの既定値（秒）。
pub const DEFAULT_SESSION_STALE_AFTER_SECONDS: u64 = 3600;
/// 同下限（秒）。
pub const MIN_SESSION_STALE_AFTER_SECONDS: u64 = 600;
/// 同上限（秒）。
pub const MAX_SESSION_STALE_AFTER_SECONDS: u64 = 86400;

/// 終了手順が投入済みタスクの完了を待つ上限の既定値（ミリ秒）。
pub const DEFAULT_RENDER_DRAIN_TIMEOUT_MS: u64 = 3000;
/// 同下限（ミリ秒）。0 は「待たない」を選べる形である。
pub const MIN_RENDER_DRAIN_TIMEOUT_MS: u64 = 0;
/// 同上限（ミリ秒）。利用者が「終了しない」と判断する限度。
pub const MAX_RENDER_DRAIN_TIMEOUT_MS: u64 = 30000;

/// 設定ファイルの場所と、その決まり方。
///
/// **決まり方まで返すのは、置き場所を用意してよいかが変わるためである。**
/// 基底の直下は我々が作る場所であり、無ければ作ってよい。外から与えられた
/// 場所は利用者のものであり、**存在を要求するだけで作らない。**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsLocation {
    /// 設定ファイルのパス。
    pub path: PathBuf,
    /// [`SETTINGS_FILE_ENV`] による上書きで決まったか。
    pub overridden: bool,
}

/// 設定ファイルの場所を解決する。
///
/// [`SETTINGS_FILE_ENV`] が空でない値を持つ場合はそれを採り、そうでなければ
/// `base_dir` の直下の [`SETTINGS_FILE_NAME`] とする。**plugin と server は
/// 同じ規則を用いる。**
pub fn settings_location(base_dir: &Path) -> SettingsLocation {
    match std::env::var(SETTINGS_FILE_ENV) {
        Ok(value) if !value.trim().is_empty() => SettingsLocation {
            path: PathBuf::from(value),
            overridden: true,
        },
        _ => SettingsLocation {
            path: base_dir.join(SETTINGS_FILE_NAME),
            overridden: false,
        },
    }
}

/// 設定ファイルのパスだけを解決する。
///
/// 決まり方を問わない呼び出し側のための薄い口である。
pub fn settings_path(base_dir: &Path) -> PathBuf {
    settings_location(base_dir).path
}

/// 設定の解決で生じた不整合。
///
/// **いずれも致命ではない。** 呼び出し元は WARN として記録し、解決済みの値で
/// 動き続ける。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsIssue {
    /// 対象の項目（`artifact.ttl_seconds` のようにドットで繋いだ名前）。
    pub field: String,
    /// 何が起きたか。
    pub reason: SettingsIssueReason,
}

/// [`SettingsIssue`] の理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsIssueReason {
    /// 型が違うため既定値へ戻した。丸められないためである。
    TypeMismatch,
    /// 値を解釈できないため既定値へ戻した。
    Unparsable,
    /// 範囲外のため境界へ丸めた。
    Clamped {
        /// ファイルに書かれていた値。
        requested: u64,
        /// 実際に採用した値。
        applied: u64,
    },
    /// 予算の不等式を破るため採用せず、直前の値を維持した。
    BudgetInequalityViolated(BudgetInequality),
}

impl std::fmt::Display for SettingsIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reason {
            SettingsIssueReason::TypeMismatch => {
                write!(f, "{} の型が異なるため既定値を用います", self.field)
            }
            SettingsIssueReason::Unparsable => {
                write!(f, "{} を解釈できないため既定値を用います", self.field)
            }
            SettingsIssueReason::Clamped { requested, applied } => write!(
                f,
                "{} の値 {requested} が範囲外のため {applied} へ丸めました",
                self.field
            ),
            SettingsIssueReason::BudgetInequalityViolated(inequality) => write!(
                f,
                "{} が予算の不等式 {inequality} を破るため直前の値を維持します",
                self.field
            ),
        }
    }
}

/// 解決済みの設定。
///
/// 全ての値が範囲内へ丸められており、そのまま使える。**全項目が全プロセスに
/// 効く。** 予算の不等式は「ある plugin とある server の組」についての性質で
/// あり、プロセスごとに違う値を持たせると静的にも動的にも保てない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    log_level: Option<String>,
    budgets: ScaledBudgets,
    disabled_tools: BTreeSet<String>,
    artifact_ttl: Duration,
    artifact_max_count: usize,
    artifact_max_total_bytes: u64,
    handoff_ttl: Duration,
    render_drain_timeout: Duration,
    session_stale_after: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            log_level: None,
            budgets: ScaledBudgets::unscaled(),
            disabled_tools: BTreeSet::new(),
            artifact_ttl: Duration::from_secs(DEFAULT_ARTIFACT_TTL_SECONDS),
            artifact_max_count: DEFAULT_ARTIFACT_MAX_COUNT as usize,
            artifact_max_total_bytes: DEFAULT_ARTIFACT_MAX_TOTAL_BYTES,
            handoff_ttl: Duration::from_secs(DEFAULT_HANDOFF_TTL_SECONDS),
            render_drain_timeout: Duration::from_millis(DEFAULT_RENDER_DRAIN_TIMEOUT_MS),
            session_stale_after: Duration::from_secs(DEFAULT_SESSION_STALE_AFTER_SECONDS),
        }
    }
}

impl Settings {
    /// 設定ファイルに書かれていたログレベル（`RUST_LOG` と同じ書式）。
    ///
    /// **未記載は `None` である。** 「書かれていない」と「`info` と書かれて
    /// いる」を区別できる形にしてあるのは、未記載のときに何を選ぶかが
    /// ビルドによって変わるためである（[`Settings::effective_log_level`]）。
    pub fn log_level(&self) -> Option<&str> {
        self.log_level.as_deref()
    }

    /// 実際に用いるログレベル。
    ///
    /// 未記載なら [`default_log_level`] を選ぶ。**既定値は 1 つのままであり、
    /// 選び分けているのは「未記載のときにどれを採るか」だけである。**
    pub fn effective_log_level(&self) -> &str {
        match self.log_level.as_deref() {
            Some(level) => level,
            None => default_log_level(),
        }
    }

    /// 倍率を適用した期限配分の一式。
    pub fn budgets(&self) -> ScaledBudgets {
        self.budgets
    }

    /// 公開しない tool の名前。
    ///
    /// **未知の名前もそのまま含む。** 何が既知かは読み手が決めることであり、
    /// 設定ファイルの解決はそれを知らない。
    pub fn disabled_tools(&self) -> &BTreeSet<String> {
        &self.disabled_tools
    }

    /// 成果物の保存時間。
    pub fn artifact_ttl(&self) -> Duration {
        self.artifact_ttl
    }

    /// 同時に保持する成果物の件数の上限。
    pub fn artifact_max_count(&self) -> usize {
        self.artifact_max_count
    }

    /// 同時に保持する成果物の総量の上限。
    pub fn artifact_max_total_bytes(&self) -> u64 {
        self.artifact_max_total_bytes
    }

    /// 引き渡し用ファイルを掃除するまでの時間。
    pub fn handoff_ttl(&self) -> Duration {
        self.handoff_ttl
    }

    /// 終了手順が投入済みタスクの完了を待つ上限。
    pub fn render_drain_timeout(&self) -> Duration {
        self.render_drain_timeout
    }

    /// 放置された session ディレクトリとみなす古さ。
    pub fn session_stale_after(&self) -> Duration {
        self.session_stale_after
    }
}

/// 設定画面が保持する変更点。
///
/// **画面を開いてから実際に変えたものだけを持つ。** 書き戻しは最新のファイルへ
/// この変更点だけを重ねるため、別のダイアログが同時に変えた他の項目は残る。
/// 同じ項目を変えた場合だけ last-writer-wins になる。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsChange {
    /// tool 名 → 公開するかどうか。ここに現れない名前はファイルの内容が残る。
    pub tools: BTreeMap<String, bool>,
    /// ログレベル。
    pub log_level: Option<String>,
    /// 予算倍率（百分率）。
    pub budget_scale_percent: Option<u64>,
    /// 成果物の保存時間（秒）。
    pub artifact_ttl_seconds: Option<u64>,
    /// 同時に保持する成果物の件数の上限。
    pub artifact_max_count: Option<u64>,
    /// 同時に保持する成果物の総量の上限（バイト）。
    pub artifact_max_total_bytes: Option<u64>,
    /// 引き渡し用ファイルを掃除するまでの時間（秒）。
    pub handoff_ttl_seconds: Option<u64>,
    /// 終了手順が投入済みタスクの完了を待つ上限（ミリ秒）。
    pub render_drain_timeout_ms: Option<u64>,
    /// 放置された session ディレクトリとみなす古さ（秒）。
    pub session_stale_after_seconds: Option<u64>,
}

impl SettingsChange {
    /// 変更点を 1 つも持たないか。
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// 設定ファイルの解析に失敗した理由。
#[derive(Debug, thiserror::Error)]
pub enum SettingsParseError {
    /// JSON として読めない。
    #[error("設定ファイルを JSON として解析できませんでした: {0}")]
    Json(#[from] serde_json::Error),
    /// 最上位が object ではない。
    #[error("設定ファイルの最上位が object ではありません")]
    NotAnObject,
}

/// 設定ファイルの内容そのもの。
///
/// **知らない内容を落とさない**ために、最上位を `Map` のまま保持する。既知の
/// 項目は [`SettingsDocument::resolve`] が取り出し、未知の項目は書き戻しで
/// そのまま残る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsDocument {
    fields: Map<String, Value>,
}

impl Default for SettingsDocument {
    fn default() -> Self {
        let mut fields = Map::new();
        fields.insert(
            FIELD_SCHEMA_VERSION.to_string(),
            Value::from(SETTINGS_SCHEMA_VERSION),
        );
        Self { fields }
    }
}

impl SettingsDocument {
    /// JSON の文字列から読む。
    pub fn parse(text: &str) -> Result<Self, SettingsParseError> {
        match serde_json::from_str::<Value>(text)? {
            Value::Object(fields) => Ok(Self { fields }),
            _ => Err(SettingsParseError::NotAnObject),
        }
    }

    /// JSON の文字列へ書き出す。
    ///
    /// [`SETTINGS_SCHEMA_VERSION`] を必ず含める。未知の項目は読んだままの形で
    /// 残る。
    pub fn to_json(&self) -> String {
        let mut fields = self.fields.clone();
        fields
            .entry(FIELD_SCHEMA_VERSION.to_string())
            .or_insert_with(|| Value::from(SETTINGS_SCHEMA_VERSION));
        serde_json::to_string_pretty(&Value::Object(fields)).expect("Map からの直列化は失敗しない")
    }

    /// 変更点を重ねる。
    ///
    /// tool の切替は `disabled_tools` の集合への出し入れとして反映する。**集合に
    /// 元から入っていた未知の名前には触れない。**
    pub fn apply(&mut self, change: &SettingsChange) {
        if !change.tools.is_empty() {
            let mut disabled = self.raw_disabled_tools();
            for (name, enabled) in &change.tools {
                if *enabled {
                    disabled.remove(name);
                } else {
                    disabled.insert(name.clone());
                }
            }
            let values = disabled.into_iter().map(Value::from).collect::<Vec<_>>();
            self.fields
                .insert(FIELD_DISABLED_TOOLS.to_string(), Value::Array(values));
        }
        if let Some(log_level) = &change.log_level {
            self.fields
                .insert(FIELD_LOG_LEVEL.to_string(), Value::from(log_level.clone()));
        }
        if let Some(percent) = change.budget_scale_percent {
            self.fields
                .insert(FIELD_BUDGET_SCALE_PERCENT.to_string(), Value::from(percent));
        }
        self.set_group_field(
            FIELD_ARTIFACT,
            FIELD_TTL_SECONDS,
            change.artifact_ttl_seconds,
        );
        self.set_group_field(FIELD_ARTIFACT, FIELD_MAX_COUNT, change.artifact_max_count);
        self.set_group_field(
            FIELD_ARTIFACT,
            FIELD_MAX_TOTAL_BYTES,
            change.artifact_max_total_bytes,
        );
        self.set_group_field(FIELD_HANDOFF, FIELD_TTL_SECONDS, change.handoff_ttl_seconds);
        self.set_group_field(
            FIELD_RENDER,
            FIELD_DRAIN_TIMEOUT_MS,
            change.render_drain_timeout_ms,
        );
        self.set_group_field(
            FIELD_SESSION,
            FIELD_STALE_AFTER_SECONDS,
            change.session_stale_after_seconds,
        );
    }

    /// 直前の設定を土台に、書かれている内容を解決する。
    ///
    /// 範囲外は境界へ丸め、型が違う項目は既定値へ戻す。**予算倍率だけは
    /// `previous` を使う** — 丸めた倍率が不等式を破る場合、その項目を捨てて
    /// 直前の値（起動時なら既定）を維持するためである。
    ///
    /// **その差し戻しは、現在の定数の下では製品の入力から到達しない。** 丸めが
    /// 先に効くため検査へ届くのは 10〜400 の倍率だけであり、その全数が不等式を
    /// 満たすことを [`crate::budget`] の全数検査が示している。**定数を動かした
    /// ときに初めて発動する経路であり、そのときは全数検査が先に落ちる。**
    pub fn resolve(&self, previous: &Settings) -> (Settings, Vec<SettingsIssue>) {
        let mut issues = Vec::new();

        let log_level = self.log_level(&mut issues);
        let percent = self.number(
            None,
            FIELD_BUDGET_SCALE_PERCENT,
            u64::from(DEFAULT_BUDGET_SCALE_PERCENT),
            u64::from(MIN_BUDGET_SCALE_PERCENT),
            u64::from(MAX_BUDGET_SCALE_PERCENT),
            &mut issues,
        ) as u32;
        let budgets = match ScaledBudgets::checked(percent) {
            Ok(budgets) => budgets,
            Err(violated) => {
                issues.push(SettingsIssue {
                    field: FIELD_BUDGET_SCALE_PERCENT.to_string(),
                    reason: SettingsIssueReason::BudgetInequalityViolated(violated),
                });
                previous.budgets
            }
        };

        let artifact_ttl = self.number(
            Some(FIELD_ARTIFACT),
            FIELD_TTL_SECONDS,
            DEFAULT_ARTIFACT_TTL_SECONDS,
            MIN_ARTIFACT_TTL_SECONDS,
            MAX_ARTIFACT_TTL_SECONDS,
            &mut issues,
        );
        let artifact_max_count = self.number(
            Some(FIELD_ARTIFACT),
            FIELD_MAX_COUNT,
            DEFAULT_ARTIFACT_MAX_COUNT,
            MIN_ARTIFACT_MAX_COUNT,
            MAX_ARTIFACT_MAX_COUNT,
            &mut issues,
        );
        let artifact_max_total_bytes = self.number(
            Some(FIELD_ARTIFACT),
            FIELD_MAX_TOTAL_BYTES,
            DEFAULT_ARTIFACT_MAX_TOTAL_BYTES,
            MIN_ARTIFACT_MAX_TOTAL_BYTES,
            MAX_ARTIFACT_MAX_TOTAL_BYTES,
            &mut issues,
        );
        // 引き取り中のファイルを消さない下限は、倍率を適用した描画の要求
        // フェーズ予算より短くならない。予算を伸ばせば引き取りに掛かり得る
        // 時間も伸びるため、固定の 30 秒だけでは足りなくなる。
        let handoff_floor = MIN_HANDOFF_TTL_SECONDS.max(
            budgets
                .server_request_phase(crate::budget::RequestBudgetKind::Render)
                .as_secs(),
        );
        let handoff_ttl = self.number(
            Some(FIELD_HANDOFF),
            FIELD_TTL_SECONDS,
            DEFAULT_HANDOFF_TTL_SECONDS.max(handoff_floor),
            handoff_floor,
            MAX_HANDOFF_TTL_SECONDS.max(handoff_floor),
            &mut issues,
        );
        let render_drain_timeout_ms = self.number(
            Some(FIELD_RENDER),
            FIELD_DRAIN_TIMEOUT_MS,
            DEFAULT_RENDER_DRAIN_TIMEOUT_MS,
            MIN_RENDER_DRAIN_TIMEOUT_MS,
            MAX_RENDER_DRAIN_TIMEOUT_MS,
            &mut issues,
        );
        let session_stale_after = self.number(
            Some(FIELD_SESSION),
            FIELD_STALE_AFTER_SECONDS,
            DEFAULT_SESSION_STALE_AFTER_SECONDS,
            MIN_SESSION_STALE_AFTER_SECONDS,
            MAX_SESSION_STALE_AFTER_SECONDS,
            &mut issues,
        );

        let settings = Settings {
            log_level,
            budgets,
            disabled_tools: self.disabled_tools(&mut issues),
            artifact_ttl: Duration::from_secs(artifact_ttl),
            artifact_max_count: artifact_max_count as usize,
            artifact_max_total_bytes,
            handoff_ttl: Duration::from_secs(handoff_ttl),
            render_drain_timeout: Duration::from_millis(render_drain_timeout_ms),
            session_stale_after: Duration::from_secs(session_stale_after),
        };
        (settings, issues)
    }

    fn set_group_field(&mut self, group: &str, field: &str, value: Option<u64>) {
        let Some(value) = value else {
            return;
        };
        let entry = self
            .fields
            .entry(group.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        if let Some(object) = entry.as_object_mut() {
            object.insert(field.to_string(), Value::from(value));
        }
    }

    /// ファイルに書かれている `disabled_tools` を、未知の名前も含めて取る。
    fn raw_disabled_tools(&self) -> BTreeSet<String> {
        self.fields
            .get(FIELD_DISABLED_TOOLS)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn disabled_tools(&self, issues: &mut Vec<SettingsIssue>) -> BTreeSet<String> {
        match self.fields.get(FIELD_DISABLED_TOOLS) {
            None | Some(Value::Null) => BTreeSet::new(),
            Some(Value::Array(values)) => {
                if values.iter().any(|value| !value.is_string()) {
                    issues.push(SettingsIssue {
                        field: FIELD_DISABLED_TOOLS.to_string(),
                        reason: SettingsIssueReason::TypeMismatch,
                    });
                }
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            }
            Some(_) => {
                issues.push(SettingsIssue {
                    field: FIELD_DISABLED_TOOLS.to_string(),
                    reason: SettingsIssueReason::TypeMismatch,
                });
                BTreeSet::new()
            }
        }
    }

    /// ログレベルを取り出す。
    ///
    /// **書式の妥当性はここでは判定しない。** `RUST_LOG` の書式を解釈するのは
    /// 記録の層であり、`crates/core` はそれに依存しない。解釈できない指定を
    /// 既定へ戻すのは読み手の責務である（[`Settings::effective_log_level`] を
    /// 通した後に判定する）。ここで見るのは型と、空でないことだけである。
    fn log_level(&self, issues: &mut Vec<SettingsIssue>) -> Option<String> {
        match self.fields.get(FIELD_LOG_LEVEL) {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
            Some(Value::String(_)) => {
                issues.push(SettingsIssue {
                    field: FIELD_LOG_LEVEL.to_string(),
                    reason: SettingsIssueReason::Unparsable,
                });
                None
            }
            Some(_) => {
                issues.push(SettingsIssue {
                    field: FIELD_LOG_LEVEL.to_string(),
                    reason: SettingsIssueReason::TypeMismatch,
                });
                None
            }
        }
    }

    /// 数値の項目を、型と範囲を見て取り出す。
    ///
    /// 未記載なら既定値、型が違えば既定値と [`SettingsIssueReason::TypeMismatch`]、
    /// 範囲外なら境界と [`SettingsIssueReason::Clamped`] を返す。
    fn number(
        &self,
        group: Option<&str>,
        field: &str,
        default: u64,
        min: u64,
        max: u64,
        issues: &mut Vec<SettingsIssue>,
    ) -> u64 {
        let path = match group {
            Some(group) => format!("{group}.{field}"),
            None => field.to_string(),
        };
        let value = match group {
            Some(group) => match self.fields.get(group) {
                None | Some(Value::Null) => None,
                Some(Value::Object(object)) => object.get(field),
                Some(_) => {
                    // 群の型違いは群につき 1 回だけ記録する。**同じ群の項目を
                    // 引くたびに積むと、同じ 1 行が項目の数だけ WARN に並ぶ。**
                    let issue = SettingsIssue {
                        field: group.to_string(),
                        reason: SettingsIssueReason::TypeMismatch,
                    };
                    if !issues.contains(&issue) {
                        issues.push(issue);
                    }
                    return default;
                }
            },
            None => self.fields.get(field),
        };
        let requested = match value {
            None | Some(Value::Null) => return default,
            Some(value) => match value.as_u64() {
                Some(number) => number,
                None => {
                    issues.push(SettingsIssue {
                        field: path,
                        reason: SettingsIssueReason::TypeMismatch,
                    });
                    return default;
                }
            },
        };
        let applied = requested.clamp(min, max);
        if applied != requested {
            issues.push(SettingsIssue {
                field: path,
                reason: SettingsIssueReason::Clamped { requested, applied },
            });
        }
        applied
    }
}

/// 設定ファイルの更新時刻と大きさの組。
///
/// **再パースするかどうかの判定にだけ使う。** 同一秒内に置換され、かつ大きさが
/// 同じであれば取りこぼす。取りこぼしても次の変更で追いつくため、古い設定＝
/// 直前まで有効だった設定で動くという保守側に倒れている。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsStamp {
    modified: Option<SystemTime>,
    len: u64,
}

/// 設定ファイルの読み取りに失敗した理由。
#[derive(Debug, thiserror::Error)]
pub enum SettingsReadError {
    /// ファイルを読めなかった。置換の最中に掴んだ場合を含む。
    #[error("設定ファイルを読み取れませんでした: {0}")]
    Io(#[from] std::io::Error),
    /// 内容を解析できなかった。
    #[error(transparent)]
    Parse(#[from] SettingsParseError),
}

/// [`SettingsReader::refresh`] の結果。
#[derive(Debug)]
pub enum SettingsRefresh {
    /// 更新時刻と大きさが前回と同じであった。**読み直していない。**
    Unchanged,
    /// 読み直し、snapshot を差し替えた。
    Reloaded(Vec<SettingsIssue>),
    /// 読み取りに失敗した。直前の snapshot を維持している。
    Failed(SettingsReadError),
}

/// 設定ファイルの読み取り口。
///
/// **plugin と server が同じ型を使う。** 片方だけが丸める形を作らないため、
/// 解決の手続きはここに 1 つしか無い。
///
/// 読み直しの契機は呼び出し元が決める。plugin は要求 1 件の処理を始めるときに、
/// server は変更の通知を受けたときに [`SettingsReader::refresh`] を呼ぶ。
#[derive(Debug)]
pub struct SettingsReader {
    path: PathBuf,
    stamp: Option<SettingsStamp>,
    settings: Arc<Settings>,
    loads: u64,
}

impl SettingsReader {
    /// 既定値から始める読み取り口を作る。
    ///
    /// この時点ではファイルを読まない。最初の [`SettingsReader::refresh`] が
    /// 読む。
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            stamp: None,
            settings: Arc::new(Settings::default()),
            loads: 0,
        }
    }

    /// 設定ファイルの場所。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 現在の設定。
    pub fn settings(&self) -> Arc<Settings> {
        Arc::clone(&self.settings)
    }

    /// ファイルを解析した回数。
    ///
    /// **更新時刻と大きさが変わらなければ増えない。** 要求 1 件あたりの費用が
    /// `stat` 1 回に留まることを、呼び出し元の試験がこの値で確かめる。
    pub fn loads(&self) -> u64 {
        self.loads
    }

    /// 更新時刻と大きさが前回と違えば読み直す。
    pub fn refresh(&mut self) -> SettingsRefresh {
        let stamp = match std::fs::metadata(&self.path) {
            Ok(metadata) => Some(SettingsStamp {
                modified: metadata.modified().ok(),
                len: metadata.len(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return SettingsRefresh::Failed(SettingsReadError::Io(e)),
        };
        if self.stamp == stamp && self.loads > 0 {
            return SettingsRefresh::Unchanged;
        }

        let Some(_) = stamp else {
            // ファイルが無い状態は破損ではない。全項目を既定値へ戻す。
            self.stamp = None;
            self.loads += 1;
            self.settings = Arc::new(Settings::default());
            return SettingsRefresh::Reloaded(Vec::new());
        };

        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) => return SettingsRefresh::Failed(SettingsReadError::Io(e)),
        };
        // 読めた以上、同じ内容を毎回読み直す理由は無い。解析に失敗しても
        // 印は進め、直前の snapshot を維持する。
        self.stamp = stamp;
        self.loads += 1;
        match SettingsDocument::parse(&text) {
            Ok(document) => {
                let (settings, issues) = document.resolve(&self.settings);
                self.settings = Arc::new(settings);
                SettingsRefresh::Reloaded(issues)
            }
            Err(e) => SettingsRefresh::Failed(SettingsReadError::Parse(e)),
        }
    }

    /// 自分が書いた内容をその場で反映する。
    ///
    /// **ファイルを読み直さない。** 書いた内容をそのまま解決し、次の
    /// [`SettingsReader::refresh`] が同じ内容を読み直さないよう印も更新する。
    pub fn adopt(&mut self, document: &SettingsDocument) -> Vec<SettingsIssue> {
        let (settings, issues) = document.resolve(&self.settings);
        self.settings = Arc::new(settings);
        self.stamp = std::fs::metadata(&self.path)
            .ok()
            .map(|metadata| SettingsStamp {
                modified: metadata.modified().ok(),
                len: metadata.len(),
            });
        self.loads += 1;
        issues
    }
}

#[cfg(test)]
mod tests;
