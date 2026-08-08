//! 設定画面の値の詰め替え。
//!
//! **ダイアログを開く手続きから切り離してある。** ウィジェットは HWND を持たない
//! 状態でも値を保持するため、画面を開かずに「[`Settings`] を読み込んで初期値に
//! する」「入力を検証して変更点だけを取り出す」の 2 つを確かめられる。
//!
//! # 変更点だけを持つ
//!
//! 画面を開いた時点の値を項目ごとに覚えておき、[`SettingsForm::collect`] は
//! **実際に変わったものだけ**を [`SettingsChange`] に載せる。触っていない項目が
//! 載らないことは、別のプロセスが同時に変えた項目を消さないための条件である。
//!
//! # 一覧も範囲も導出である
//!
//! 並べる tool 名は [`togglable_tool_names`] から、族は [`ToolFamily`] から、
//! 数値の範囲は `aviutl2_mcp_core::settings` の下限・上限の定数から導く。
//! **書き写さないため、tool や範囲が変わったときに画面だけが古くなる経路が無い。**

use aviutl2_mcp_core::budget::{RequestBudgetKind, ScaledBudgets};
use aviutl2_mcp_core::settings::{
    AgentPluginSettings, MAX_ARTIFACT_MAX_COUNT, MAX_ARTIFACT_MAX_TOTAL_BYTES,
    MAX_ARTIFACT_TTL_SECONDS, MAX_BUDGET_SCALE_PERCENT, MAX_HANDOFF_TTL_SECONDS,
    MAX_RENDER_DRAIN_TIMEOUT_MS, MAX_SESSION_STALE_AFTER_SECONDS, MIN_ARTIFACT_MAX_COUNT,
    MIN_ARTIFACT_MAX_TOTAL_BYTES, MIN_ARTIFACT_TTL_SECONDS, MIN_BUDGET_SCALE_PERCENT,
    MIN_HANDOFF_TTL_SECONDS, MIN_RENDER_DRAIN_TIMEOUT_MS, MIN_SESSION_STALE_AFTER_SECONDS,
    Settings, SettingsChange,
};
use aviutl2_mcp_core::tool::{ToolFamily, togglable_tool_names};
use win32_ui::widget::{CheckBox, ComboBox, Number, NumberRangeError};

/// 選べるログレベル。
///
/// `RUST_LOG` の書式は任意の指定を許すが、画面から選べるのは水準そのものだけに
/// 絞る。**ファイルに書かれている指定がこの一覧に無ければ、その値を先頭へ足して
/// 選択肢に含める**——手で書いた指定を画面を開いただけで失わないためである。
const LOG_LEVELS: [&str; 5] = ["trace", "debug", "info", "warn", "error"];

/// 1 MiB のバイト数。
///
/// 成果物の総量だけはバイトで持つと桁が大きすぎるため、画面では MiB で扱う。
const BYTES_PER_MIB: u64 = 1024 * 1024;

/// 「動作」ページの中の群。
///
/// タブをこれ以上増やさずに項目を分けるための見出しである。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BehaviorGroup {
    /// ログ。
    Log,
    /// 待ち時間。
    Timing,
    /// 保存と掃除。
    Retention,
}

impl BehaviorGroup {
    /// 全 variant。並べる順でもある。
    pub const ALL: [BehaviorGroup; 3] = [
        BehaviorGroup::Log,
        BehaviorGroup::Timing,
        BehaviorGroup::Retention,
    ];

    /// 見出しの文字列。
    pub fn label(self) -> &'static str {
        match self {
            BehaviorGroup::Log => "ログ",
            BehaviorGroup::Timing => "待ち時間",
            BehaviorGroup::Retention => "保存と掃除",
        }
    }
}

/// 「動作」ページに並べる数値の項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericSetting {
    /// 予算倍率（百分率）。
    BudgetScalePercent,
    /// 終了手順が投入済みタスクの完了を待つ上限（ミリ秒）。
    RenderDrainTimeoutMs,
    /// 成果物の保存時間（秒）。
    ArtifactTtlSeconds,
    /// 同時に保持する成果物の件数の上限。
    ArtifactMaxCount,
    /// 同時に保持する成果物の総量の上限（MiB）。
    ArtifactMaxTotalMib,
    /// 引き渡し用ファイルを掃除するまでの時間（秒）。
    HandoffTtlSeconds,
    /// 放置された session ディレクトリとみなす古さ（秒）。
    SessionStaleAfterSeconds,
}

impl NumericSetting {
    /// 全 variant。並べる順でもあり、群ごとに固まっている。
    pub const ALL: [NumericSetting; 7] = [
        NumericSetting::BudgetScalePercent,
        NumericSetting::RenderDrainTimeoutMs,
        NumericSetting::ArtifactTtlSeconds,
        NumericSetting::ArtifactMaxCount,
        NumericSetting::ArtifactMaxTotalMib,
        NumericSetting::HandoffTtlSeconds,
        NumericSetting::SessionStaleAfterSeconds,
    ];

    /// 属する群。
    pub fn group(self) -> BehaviorGroup {
        match self {
            NumericSetting::BudgetScalePercent | NumericSetting::RenderDrainTimeoutMs => {
                BehaviorGroup::Timing
            }
            NumericSetting::ArtifactTtlSeconds
            | NumericSetting::ArtifactMaxCount
            | NumericSetting::ArtifactMaxTotalMib
            | NumericSetting::HandoffTtlSeconds
            | NumericSetting::SessionStaleAfterSeconds => BehaviorGroup::Retention,
        }
    }

    /// 項目の名前。単位は [`NumericSetting::unit`] が持つ。
    pub fn name(self) -> &'static str {
        match self {
            NumericSetting::BudgetScalePercent => "予算の倍率",
            NumericSetting::RenderDrainTimeoutMs => "終了時に描画を待つ上限",
            NumericSetting::ArtifactTtlSeconds => "成果物の保存時間",
            NumericSetting::ArtifactMaxCount => "成果物の件数の上限",
            NumericSetting::ArtifactMaxTotalMib => "成果物の総量の上限",
            NumericSetting::HandoffTtlSeconds => "引き渡しの保持時間",
            NumericSetting::SessionStaleAfterSeconds => "session を放置とみなす古さ",
        }
    }

    /// 値の単位。
    pub fn unit(self) -> &'static str {
        match self {
            NumericSetting::BudgetScalePercent => "%",
            NumericSetting::RenderDrainTimeoutMs => "ミリ秒",
            NumericSetting::ArtifactTtlSeconds
            | NumericSetting::HandoffTtlSeconds
            | NumericSetting::SessionStaleAfterSeconds => "秒",
            NumericSetting::ArtifactMaxCount => "件",
            NumericSetting::ArtifactMaxTotalMib => "MiB",
        }
    }

    /// 入力できる下限と上限。
    ///
    /// **`aviutl2_mcp_core` の下限・上限から導く。** 引き渡しの保持時間だけは
    /// 下限が倍率後の描画の要求フェーズ予算と連動するため、予算一式を見る。
    /// **倍率は同じ画面で変えられるため、確定時には入力済みの倍率から組み直した
    /// 一式を渡す**（[`SettingsForm::collect`]）。
    pub fn range(self, budgets: ScaledBudgets) -> (i32, i32) {
        let (min, max) = match self {
            NumericSetting::BudgetScalePercent => (
                u64::from(MIN_BUDGET_SCALE_PERCENT),
                u64::from(MAX_BUDGET_SCALE_PERCENT),
            ),
            NumericSetting::RenderDrainTimeoutMs => {
                (MIN_RENDER_DRAIN_TIMEOUT_MS, MAX_RENDER_DRAIN_TIMEOUT_MS)
            }
            NumericSetting::ArtifactTtlSeconds => {
                (MIN_ARTIFACT_TTL_SECONDS, MAX_ARTIFACT_TTL_SECONDS)
            }
            NumericSetting::ArtifactMaxCount => (MIN_ARTIFACT_MAX_COUNT, MAX_ARTIFACT_MAX_COUNT),
            // 丸めの向きを分ける。下限を切り上げ、上限を切り捨てることで、
            // MiB で入力できる値が必ずバイトの範囲の内側に収まる。
            NumericSetting::ArtifactMaxTotalMib => (
                MIN_ARTIFACT_MAX_TOTAL_BYTES.div_ceil(BYTES_PER_MIB),
                MAX_ARTIFACT_MAX_TOTAL_BYTES / BYTES_PER_MIB,
            ),
            NumericSetting::HandoffTtlSeconds => {
                let floor = handoff_ttl_floor(budgets);
                (floor, MAX_HANDOFF_TTL_SECONDS.max(floor))
            }
            NumericSetting::SessionStaleAfterSeconds => (
                MIN_SESSION_STALE_AFTER_SECONDS,
                MAX_SESSION_STALE_AFTER_SECONDS,
            ),
        };
        (clamp_to_i32(min), clamp_to_i32(max))
    }

    /// 現在の設定が持つ値。
    pub fn current(self, settings: &Settings) -> i32 {
        let value = match self {
            NumericSetting::BudgetScalePercent => u64::from(settings.budgets().percent()),
            NumericSetting::RenderDrainTimeoutMs => {
                settings.render_drain_timeout().as_millis() as u64
            }
            NumericSetting::ArtifactTtlSeconds => settings.artifact_ttl().as_secs(),
            NumericSetting::ArtifactMaxCount => settings.artifact_max_count() as u64,
            // 端数は切り上げる。手で書かれた端数はここで丸まって見えるが、
            // **触らなければ変更点にならない**ため、書き戻しでは失われない。
            NumericSetting::ArtifactMaxTotalMib => {
                settings.artifact_max_total_bytes().div_ceil(BYTES_PER_MIB)
            }
            NumericSetting::HandoffTtlSeconds => settings.handoff_ttl().as_secs(),
            NumericSetting::SessionStaleAfterSeconds => settings.session_stale_after().as_secs(),
        };
        clamp_to_i32(value)
    }

    /// 変更点へ値を載せる。
    fn apply(self, value: i32, change: &mut SettingsChange) {
        let value = value.max(0) as u64;
        match self {
            NumericSetting::BudgetScalePercent => change.budget_scale_percent = Some(value),
            NumericSetting::RenderDrainTimeoutMs => change.render_drain_timeout_ms = Some(value),
            NumericSetting::ArtifactTtlSeconds => change.artifact_ttl_seconds = Some(value),
            NumericSetting::ArtifactMaxCount => change.artifact_max_count = Some(value),
            NumericSetting::ArtifactMaxTotalMib => {
                change.artifact_max_total_bytes = Some(value * BYTES_PER_MIB)
            }
            NumericSetting::HandoffTtlSeconds => change.handoff_ttl_seconds = Some(value),
            NumericSetting::SessionStaleAfterSeconds => {
                change.session_stale_after_seconds = Some(value)
            }
        }
    }
}

/// 引き渡しの保持時間の下限（秒）。
///
/// 固定の下限と、倍率を適用した描画の要求フェーズ予算の長い方である。解決側と
/// 同じ規則であり、画面が解決側より緩い範囲を提示しないようにする。
fn handoff_ttl_floor(budgets: ScaledBudgets) -> u64 {
    MIN_HANDOFF_TTL_SECONDS.max(
        budgets
            .server_request_phase(RequestBudgetKind::Render)
            .as_secs(),
    )
}

/// 入力欄が扱える範囲へ収める。
fn clamp_to_i32(value: u64) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// 「エージェントプラグイン」ページの切り替え。
///
/// **`generate` だけが同意である。** 他の 3 つは内訳であり、同意が無い間は
/// 無効表示になる（値は保つ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPluginToggle {
    /// 生成するかどうか。**同意そのものである。**
    Generate,
    /// Claude Code が読む形を生成する。
    Claude,
    /// Agent Plugins の仕様に従う形を生成する。
    AgentPlugins,
    /// skill を同梱する。
    Skill,
}

impl AgentPluginToggle {
    /// 全 variant。並べる順でもある。
    pub const ALL: [AgentPluginToggle; 4] = [
        AgentPluginToggle::Generate,
        AgentPluginToggle::Claude,
        AgentPluginToggle::AgentPlugins,
        AgentPluginToggle::Skill,
    ];

    /// 同意そのものか。
    pub fn is_consent(self) -> bool {
        self == AgentPluginToggle::Generate
    }

    /// 見出しの文字列。
    ///
    /// **「方言」と呼ばない。** 2 つの形が歩み寄らないことは生成する側の事情で
    /// あり、利用者が選んでいるのは**どの相手に向けて置くか**である。相手の名前
    /// で名乗る。
    pub fn label(self) -> &'static str {
        match self {
            AgentPluginToggle::Generate => "エージェントプラグインを生成する",
            AgentPluginToggle::Claude => "Claude Code 向けに生成する",
            AgentPluginToggle::AgentPlugins => "Agent Plugins 向けに生成する",
            AgentPluginToggle::Skill => "skill を同梱する",
        }
    }

    /// 現在の設定が持つ値。
    fn current(self, settings: &AgentPluginSettings) -> bool {
        match self {
            AgentPluginToggle::Generate => settings.generate,
            AgentPluginToggle::Claude => settings.claude,
            AgentPluginToggle::AgentPlugins => settings.agent_plugins,
            AgentPluginToggle::Skill => settings.skill,
        }
    }

    /// 変更点へ値を載せる。
    fn apply(self, value: bool, change: &mut SettingsChange) {
        match self {
            AgentPluginToggle::Generate => change.agent_plugin_generate = Some(value),
            AgentPluginToggle::Claude => change.agent_plugin_claude = Some(value),
            AgentPluginToggle::AgentPlugins => change.agent_plugin_agent_plugins = Some(value),
            AgentPluginToggle::Skill => change.agent_plugin_skill = Some(value),
        }
    }
}

/// 「エージェントプラグイン」ページの切り替え 1 つ。
pub struct AgentPluginInput {
    toggle: AgentPluginToggle,
    control: CheckBox,
    initial: bool,
}

impl AgentPluginInput {
    /// どの切り替えか。
    pub fn toggle(&self) -> AgentPluginToggle {
        self.toggle
    }

    /// 画面に置くチェックボックス。
    pub fn control(&self) -> CheckBox {
        self.control.clone()
    }
}

/// tool 1 つの切替。
pub struct ToolToggle {
    name: String,
    family: ToolFamily,
    control: CheckBox,
    initial_enabled: bool,
}

impl ToolToggle {
    /// tool の名前。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 画面に置くチェックボックス。
    pub fn control(&self) -> CheckBox {
        self.control.clone()
    }
}

/// 数値の入力欄 1 つ。
pub struct NumericInput {
    setting: NumericSetting,
    control: Number,
    initial: i32,
}

impl NumericInput {
    /// どの項目か。
    pub fn setting(&self) -> NumericSetting {
        self.setting
    }

    /// 単位と、入力欄が持つ範囲を添えた見出し。
    pub fn label(&self) -> String {
        let (min, max) = self
            .control
            .range_bounds()
            .expect("数値の入力欄は必ず範囲を持つ");
        self.label_for(min, max)
    }

    /// 単位と、与えられた範囲を添えた見出し。
    ///
    /// **確定時の判定は入力済みの倍率から範囲を引き直す**ため、そのとき伝える
    /// 見出しは入力欄が持つ範囲と食い違い得る。伝えるのは判定に使った範囲で
    /// なければならない。
    fn label_for(&self, min: i32, max: i32) -> String {
        format!(
            "{} ({}, {min}〜{max})",
            self.setting.name(),
            self.setting.unit()
        )
    }

    /// 画面に置く入力欄。
    pub fn control(&self) -> Number {
        self.control.clone()
    }
}

/// ログレベルの選択。
pub struct LogLevelChoice {
    control: ComboBox,
    items: Vec<String>,
    initial_index: usize,
}

impl LogLevelChoice {
    /// 見出し。
    pub fn label(&self) -> &'static str {
        "ログレベル"
    }

    /// 画面に置くコンボボックス。
    pub fn control(&self) -> ComboBox {
        self.control.clone()
    }

    /// 選べる値。
    pub fn items(&self) -> &[String] {
        &self.items
    }

    /// 開いた時点の値。
    fn initial(&self) -> &str {
        self.items[self.initial_index].as_str()
    }

    /// 現在選ばれている値。
    ///
    /// **選択を読めない場合は開いた時点の値へ倒す。** 何も選ばれていないときの
    /// 通知は負の値であり、先頭の要素へ倒すと「手で書かれた指定が先頭に居る」
    /// 場合に別の値を選んだことになる。**退避先を 1 つにする。**
    fn selected(&self) -> &str {
        usize::try_from(self.control.selected_index())
            .ok()
            .and_then(|index| self.items.get(index))
            .map(String::as_str)
            .unwrap_or_else(|| self.initial())
    }

    /// 開いた時点の値から変わっていれば、その値。
    fn change(&self) -> Option<String> {
        let selected = self.selected();
        (selected != self.initial()).then(|| selected.to_string())
    }
}

/// 設定画面が扱う値の全体。
///
/// **ウィジェットのクローンを保持する。** ダイアログは閉じるときに最終状態を
/// 取り込むため、閉じた後でも同じクローンから値を読める。
pub struct SettingsForm {
    tools: Vec<ToolToggle>,
    log_level: LogLevelChoice,
    numbers: Vec<NumericInput>,
    agent_plugin: Vec<AgentPluginInput>,
    /// 開いた時点の予算一式。倍率の入力が使えないときの退避先である。
    budgets: ScaledBudgets,
}

impl SettingsForm {
    /// 現在の設定を初期値として組み立てる。
    pub fn new(settings: &Settings) -> Self {
        let tools = togglable_tool_names()
            .map(|name| {
                let enabled = !settings.disabled_tools().contains(&name);
                let family = family_of(&name);
                ToolToggle {
                    control: CheckBox::new(&name).checked(enabled),
                    family,
                    name,
                    initial_enabled: enabled,
                }
            })
            .collect();

        let budgets = settings.budgets();
        let numbers = NumericSetting::ALL
            .into_iter()
            .map(|setting| {
                let (min, max) = setting.range(budgets);
                let initial = setting.current(settings).clamp(min, max);
                NumericInput {
                    setting,
                    control: Number::new().range(min, max).value(initial),
                    initial,
                }
            })
            .collect();

        Self {
            tools,
            log_level: log_level_choice(settings),
            numbers,
            agent_plugin: agent_plugin_toggles(settings.agent_plugin()),
            budgets,
        }
    }

    /// 並べる tool の全体。族の順に並ぶ。
    pub fn tools(&self) -> &[ToolToggle] {
        &self.tools
    }

    /// 族に属する tool。
    pub fn tools_in(&self, family: ToolFamily) -> impl Iterator<Item = &ToolToggle> {
        self.tools.iter().filter(move |tool| tool.family == family)
    }

    /// ログレベルの選択。
    pub fn log_level(&self) -> &LogLevelChoice {
        &self.log_level
    }

    /// 群に属する数値の入力欄。
    pub fn numbers_in(&self, group: BehaviorGroup) -> impl Iterator<Item = &NumericInput> {
        self.numbers
            .iter()
            .filter(move |input| input.setting.group() == group)
    }

    /// 「エージェントプラグイン」ページの切り替え。
    pub fn agent_plugin(&self) -> &[AgentPluginInput] {
        &self.agent_plugin
    }

    /// 入力を検証し、開いた時点から変わった項目だけを取り出す。
    ///
    /// **範囲外や整数でない入力は変更点を返さず、利用者へ示す文言を返す。**
    /// 入力欄の範囲指定はスピンボタンとカーソルキーしか縛らないため、直接
    /// 入力された値はここで初めて弾かれる。**これは読み込み時の丸めの代わりでは
    /// ない**——ファイルを手で編集する経路が残るため、保証は読み手側が与える。
    ///
    /// 検証は 2 段である。**まず全項目を読んで整数として解釈できることを確かめ、
    /// 次に入力済みの倍率から範囲を引き直して判定する。** 引き渡しの保持時間の
    /// 下限は倍率後の描画の要求フェーズ予算と連動しており、**倍率は同じ画面で
    /// 変えられる**——入力欄が持つ範囲は開いた時点のものであり、確定の判定を
    /// そちらに委ねると、倍率を上げた場合は解決側が丸める値を通してしまい、
    /// 下げた場合は解決側が受け取る値を拒んでしまう。
    pub fn collect(&self) -> Result<SettingsChange, Vec<String>> {
        let mut change = SettingsChange::default();
        let mut errors = Vec::new();
        let mut entered = Vec::new();

        for input in &self.numbers {
            match input.control.validate() {
                Ok(value) => entered.push((input, value)),
                // 範囲の判定は引き直した後に行う。ここで拾うのは値そのものである。
                Err(NumberRangeError::OutOfRange { value, .. }) => entered.push((input, value)),
                Err(e) => errors.push(describe_range_error(&input.label(), &e)),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        let budgets = self.entered_budgets(&entered);
        for (input, value) in &entered {
            let (min, max) = input.setting.range(budgets);
            if *value < min || *value > max {
                errors.push(describe_range_error(
                    &input.label_for(min, max),
                    &NumberRangeError::OutOfRange {
                        value: *value,
                        min,
                        max,
                    },
                ));
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        for (input, value) in entered {
            if value != input.initial {
                input.setting.apply(value, &mut change);
            }
        }

        for tool in &self.tools {
            let enabled = tool.control.is_checked();
            if enabled != tool.initial_enabled {
                change.tools.insert(tool.name.clone(), enabled);
            }
        }
        change.log_level = self.log_level.change();

        for input in &self.agent_plugin {
            let checked = input.control.is_checked();
            if checked != input.initial {
                input.toggle.apply(checked, &mut change);
            }
        }

        Ok(change)
    }

    /// 入力済みの倍率から組み直した予算一式。
    ///
    /// 倍率が不等式を破る場合は開いた時点の一式を使う。**解決側が同じ判断を
    /// する**——破る倍率は採用されず、直前の値が維持される。
    fn entered_budgets(&self, entered: &[(&NumericInput, i32)]) -> ScaledBudgets {
        entered
            .iter()
            .find(|(input, _)| input.setting == NumericSetting::BudgetScalePercent)
            .and_then(|(_, value)| u32::try_from(*value).ok())
            .and_then(|percent| ScaledBudgets::checked(percent).ok())
            .unwrap_or(self.budgets)
    }
}

/// tool 名が属する族。
///
/// 一覧が族ごとの導出であるため、名前から引き直しても同じ族になる。
fn family_of(name: &str) -> ToolFamily {
    ToolFamily::ALL
        .into_iter()
        .find(|family| family.tool_names().any(|candidate| candidate == name))
        .expect("切替の対象はいずれかの族に属する")
}

/// 「エージェントプラグイン」ページの切り替えを組み立てる。
///
/// **同意が off の間、内訳は無効表示になる。** 同意の切り替えに応じて内訳の
/// 有効・無効を追随させるのは、同意を立てた直後に内訳へ手が届くようにする
/// ためである——1 度閉じ直させると、opt-in が 2 手になる。
fn agent_plugin_toggles(settings: AgentPluginSettings) -> Vec<AgentPluginInput> {
    let inputs: Vec<AgentPluginInput> = AgentPluginToggle::ALL
        .into_iter()
        .map(|toggle| {
            let initial = toggle.current(&settings);
            AgentPluginInput {
                toggle,
                control: CheckBox::new(toggle.label())
                    .checked(initial)
                    .enabled(toggle.is_consent() || settings.generate),
                initial,
            }
        })
        .collect();

    let breakdown: Vec<CheckBox> = inputs
        .iter()
        .filter(|input| !input.toggle.is_consent())
        .map(|input| input.control.clone())
        .collect();
    if let Some(consent) = inputs.iter().find(|input| input.toggle.is_consent()) {
        consent.control.clone().on_change(move |checked| {
            for toggle in &breakdown {
                toggle.set_enabled(checked);
            }
        });
    }
    inputs
}

/// ログレベルの選択肢と初期値を組み立てる。
fn log_level_choice(settings: &Settings) -> LogLevelChoice {
    let current = settings.effective_log_level();
    let mut items: Vec<String> = LOG_LEVELS.iter().map(|level| level.to_string()).collect();
    if !items.iter().any(|item| item == current) {
        items.insert(0, current.to_string());
    }
    let index = items
        .iter()
        .position(|item| item == current)
        .expect("現在の値は必ず選択肢に含まれる");
    LogLevelChoice {
        control: ComboBox::new(items.iter().map(String::as_str).collect())
            .selected(clamp_to_i32(index as u64)),
        items,
        initial_index: index,
    }
}

/// 範囲の検査で弾かれた理由を利用者へ示す文言にする。
fn describe_range_error(label: &str, error: &NumberRangeError) -> String {
    match error {
        NumberRangeError::NotANumber { text } => {
            format!("{label}: 「{text}」は整数として読めません")
        }
        NumberRangeError::OutOfRange { value, min, max } => {
            format!("{label}: {value} は {min}〜{max} の範囲にありません")
        }
    }
}

#[cfg(test)]
mod tests;
