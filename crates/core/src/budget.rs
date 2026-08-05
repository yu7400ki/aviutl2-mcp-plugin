//! IPC 1 往復の期限配分。
//!
//! server はフェーズ単位（インスタンス解決・要求）の予算を持ち、その予算を
//! `deadline_unix_ms` として plugin へ伝える。plugin はフェーズを段（handshake・
//! 実行・応答送信）に分け、各段に自前の上限を持つ。両者が別々に値を決めると、
//! plugin が自らの契約どおりに使い切った時点で server の予算が尽き、生きている
//! インスタンスが期限超過として扱われる。
//!
//! そこで配分をここへ一本化し、plugin の各段の合計が server のフェーズ予算より
//! 真に小さいことを本モジュールのテストで固定する。差分は
//! [`ScaledBudgets::transport_headroom`] として明示的に残す。
//!
//! 要求フェーズの予算は read・edit・batch・render の 4 つが並ぶ。operation 名が
//! どれに属するかの判定基準は [`crate::operation::KnownOperation`] へ一本化して
//! あり、[`request_budget_kind`] はその判定を要求予算の区分へ変換するだけの
//! 薄い層である。区分で分岐する必要がある処理は、`KnownOperation` を経由する
//! ことで判断根拠を複数箇所に手書きせずに済む。

use crate::operation::KnownOperation;
use std::time::Duration;

/// server がインスタンス解決フェーズ全体に許す上限。
///
/// pipe 接続・handshake・ping 往復をこの 1 つの期限で束ねる。
pub const SERVER_RESOLVE_BUDGET: Duration = Duration::from_secs(5);

/// server が read operation 1 件の要求フェーズ全体に許す上限。
///
/// 要求の送信・plugin 側の実行・応答の受信をこの 1 つの期限で束ね、
/// 同じ期限を要求の `deadline_unix_ms` として plugin へ伝える。
pub const SERVER_READ_REQUEST_BUDGET: Duration = Duration::from_secs(5);

/// server が編集 operation 1 件の要求フェーズ全体に許す上限。
///
/// [`SERVER_READ_REQUEST_BUDGET`] と同じ役割を編集について持つ。編集は
/// `call_edit_section` の実行に read より長い上限を要するため、別の値を持つ。
pub const SERVER_EDIT_REQUEST_BUDGET: Duration = Duration::from_secs(10);

/// server が一括適用 1 件の要求フェーズ全体に許す上限。
///
/// 一括適用の費用は「異なるレイヤー数 × レイヤー内オブジェクト数」に比例し、
/// 変更の件数だけでは決まらない。単一の編集と同じ予算では、変更を 1 つも
/// 発行しないうちに予算が尽き得るため、編集より長い値を持つ。
///
/// **上限であって目標ではない。** 編集区間はホストのメインスレッドを占有する
/// ため、この上限まで費やす一括適用は同じ時間だけ UI を止める。
pub const SERVER_BATCH_REQUEST_BUDGET: Duration = Duration::from_secs(20);

/// server が render operation 1 件の要求フェーズ全体に許す上限。
///
/// 描画の完了はホスト側の非同期タスクを待つため、他のどの operation よりも
/// 長い。応答を受けたあとの成果物の引き取り
/// （[`SERVER_ARTIFACT_INGEST_BUDGET`]）もこの予算の内側で起きる。
pub const SERVER_RENDER_REQUEST_BUDGET: Duration = Duration::from_secs(30);

/// server が描画成果物の引き取りに許す上限。
///
/// 引き取りは応答の受信後に始まり、ファイルの読み込み・ダイジェストの計算・
/// 保管・元ファイルの削除を含む。**応答を受けてからさらに仕事をするのは
/// render だけである。**
///
/// この段を [`SERVER_RENDER_REQUEST_BUDGET`] の内側に数え忘れると、plugin が
/// 予算いっぱいまで費やした直後に引き取りが始まり、要求フェーズの予算を
/// 超えてから成功する経路ができる。したがって render の要求へ載せる期限は
/// 要求フェーズの予算そのものではなく、本値を差し引いた残りから算出する。
pub const SERVER_ARTIFACT_INGEST_BUDGET: Duration = Duration::from_secs(4);

/// server が 1 候補の pipe 接続待ちに許す上限。
///
/// 解決フェーズの予算から接続待ちが取り分けられる最大値であり、残りが
/// handshake と ping の持ち時間になる。接続待ちだけで予算を使い切らせないため、
/// 残り時間がこれより長くても頭打ちにする。
pub const SERVER_CONNECT_WAIT_CAP: Duration = Duration::from_secs(1);

/// plugin が handshake（M1 受信 〜 M3 検証）に許す上限。
///
/// 解決フェーズのうち plugin が handshake に費やせる時間であり、残りが
/// ping 往復の持ち時間になる。
pub const PLUGIN_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// plugin が read operation の実行に許す上限。
///
/// 要求フェーズのうち読み取りに費やせる時間であり、残りが応答送信の持ち時間に
/// なる。要求が `deadline_unix_ms` を運ぶ場合はその残り時間との短い方を採る。
pub const PLUGIN_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// plugin が編集 operation の実行に許す上限。
///
/// [`PLUGIN_READ_TIMEOUT`] と同じ役割を編集について持つ。編集区間へ入る前
/// にしか期限を判定できないため、この上限が効くのは開始前の判定に限られる。
/// 区間へ入った後の超過は制御できず、ホストが応答するまで待つ。
pub const PLUGIN_EDIT_TIMEOUT: Duration = Duration::from_secs(8);

/// plugin が一括適用の実行に許す上限。
///
/// [`PLUGIN_EDIT_TIMEOUT`] と同じ役割を一括適用について持ち、効くのが編集区間
/// へ入る前の判定に限られることも同じである。
pub const PLUGIN_BATCH_TIMEOUT: Duration = Duration::from_secs(18);

/// plugin が描画の完了通知を待つ上限。
///
/// 描画はホスト側の別スレッドで進み、取り消す手段も完了の保証も無い。上限を
/// 過ぎた要求は待機を諦めて失敗を返すため、この値は「戻らない描画に付き合う
/// 時間」の上限である。
pub const PLUGIN_RENDER_WAIT_TIMEOUT: Duration = Duration::from_secs(18);

/// plugin が描画結果の符号化と受け渡しファイルの書き出しに許す上限。
///
/// 完了通知を受けてから応答を送るまでの取り分であり、
/// [`PLUGIN_RENDER_WAIT_TIMEOUT`] とは別枠で確保する。
pub const PLUGIN_RENDER_ARTIFACT_TIMEOUT: Duration = Duration::from_secs(5);

/// plugin が応答 1 フレームの送信に許す上限。
///
/// 実行が上限を使い切っても送信の持ち時間が残るよう、実行とは別枠で確保する。
/// ping の応答送信にも同じ上限を用いる。
pub const PLUGIN_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// フェーズ予算のうち、どの段の上限にも配分せず残す余白。
///
/// フレームの直列化、pipe への書き込み、スレッドの起床、2 つの時計を読む間の
/// 誤差といった段の境界で生じる時間をここで吸収する。段の上限の合計に本値を
/// 加えてもフェーズ予算を超えないことを、本モジュールのテストで保証する。
pub const TRANSPORT_HEADROOM: Duration = Duration::from_secs(1);

/// 要求フェーズの予算区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestBudgetKind {
    /// read operation 群。`ping` や未知の名前もここに区分される
    /// （[`request_budget_kind`] の既定）。
    Read,
    /// 一括適用を除く編集 operation 群。
    Edit,
    /// 一括適用。
    Batch,
    /// render operation 群。
    Render,
}

/// operation 名から要求予算の区分を引く。
///
/// 判定は [`KnownOperation::budget_kind`] に委ね、独自の一覧は持たない。
/// いずれの族にも属さない operation 名（`ping`・未知の名前）は
/// [`RequestBudgetKind::Read`] とする。
///
/// **未知の名前が最も短い予算へ落ちることは無害である。** 未知の operation は
/// 実行される前に「未対応」として拒否され、予算を使う処理へ進まない。塞ぐ
/// 必要があるのは実行される operation の分類漏れであり、それは
/// `KnownOperation` の網羅性が塞ぐ。
pub fn request_budget_kind(operation: &str) -> RequestBudgetKind {
    match KnownOperation::from_operation_name(operation) {
        Some(operation) => operation.budget_kind(),
        None => RequestBudgetKind::Read,
    }
}

/// 予算一式が満たすべき不等式。
///
/// いずれも「plugin の各段の合計と余白が、server のフェーズ予算に収まる」形を
/// している。倍率を掛けた予算一式は、採用の前にこの全てを満たすことを
/// [`ScaledBudgets::first_violation`] で確かめる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetInequality {
    /// 読み取りの各段が read の要求フェーズ予算に収まる。
    ReadRequest,
    /// 編集の各段が編集の要求フェーズ予算に収まる。
    EditRequest,
    /// 一括適用の各段が一括適用の要求フェーズ予算に収まる。
    BatchRequest,
    /// 描画の各段と成果物の引き取りが render の要求フェーズ予算に収まる。
    RenderRequest,
    /// 接続待ち・handshake・ping 応答が解決フェーズ予算に収まる。
    Resolve,
    /// 読み取りが上限まで走った後にも応答送信の持ち時間が残る。
    WriteAfterRead,
    /// 編集が上限まで走った後にも応答送信の持ち時間が残る。
    WriteAfterEdit,
}

impl BudgetInequality {
    /// 全ての不等式。
    pub const ALL: [Self; 7] = [
        Self::ReadRequest,
        Self::EditRequest,
        Self::BatchRequest,
        Self::RenderRequest,
        Self::Resolve,
        Self::WriteAfterRead,
        Self::WriteAfterEdit,
    ];

    /// 記録に用いる名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadRequest => "read_request",
            Self::EditRequest => "edit_request",
            Self::BatchRequest => "batch_request",
            Self::RenderRequest => "render_request",
            Self::Resolve => "resolve",
            Self::WriteAfterRead => "write_after_read",
            Self::WriteAfterEdit => "write_after_edit",
        }
    }
}

impl std::fmt::Display for BudgetInequality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 整数百分率を掛けた期限配分の一式。
///
/// 予算は 15 個あり、いずれも不等式で結ばれている。個別に動かせる形にすると
/// どの不等式も破れるため、可変にするのは全体へ掛かる 1 つの倍率だけとする。
///
/// **倍率が線形性を保つことは示せるが、丸めは保たない。** 整数百分率を掛けて
/// ミリ秒へ落とす際、左辺の各項の切り捨てと右辺の切り捨ては独立に起きる。
/// したがって [`ScaledBudgets::new`] は組み上げた一式が不等式を満たすことを
/// 実際に確かめ、満たさない倍率を採用させない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScaledBudgets {
    percent: u32,
    server_resolve: Duration,
    server_read_request: Duration,
    server_edit_request: Duration,
    server_batch_request: Duration,
    server_render_request: Duration,
    server_artifact_ingest: Duration,
    server_connect_wait_cap: Duration,
    plugin_handshake: Duration,
    plugin_read: Duration,
    plugin_edit: Duration,
    plugin_batch: Duration,
    plugin_render_wait: Duration,
    plugin_render_artifact: Duration,
    plugin_write: Duration,
    transport_headroom: Duration,
}

/// 期限へ整数百分率を掛ける。
///
/// ミリ秒へ切り捨てる。予算はいずれも秒の桁であり、最小の倍率でもミリ秒の桁で
/// 意味を保つ。
fn scale(base: Duration, percent: u32) -> Duration {
    let millis = base.as_millis() as u64;
    Duration::from_millis(millis.saturating_mul(u64::from(percent)) / 100)
}

impl ScaledBudgets {
    /// 倍率を掛けた予算一式を組み立て、不等式を検査する。
    ///
    /// 破れている不等式があればそれを返し、一式を渡さない。**採用の可否を
    /// ここで閉じることで、片方の側だけが倍率を採る形を作れないようにする。**
    pub fn checked(percent: u32) -> Result<Self, BudgetInequality> {
        let budgets = Self::build(percent);
        match budgets.first_violation() {
            Some(inequality) => Err(inequality),
            None => Ok(budgets),
        }
    }

    /// 検査を経ずに一式を組み立てる。
    fn build(percent: u32) -> Self {
        Self {
            percent,
            server_resolve: scale(SERVER_RESOLVE_BUDGET, percent),
            server_read_request: scale(SERVER_READ_REQUEST_BUDGET, percent),
            server_edit_request: scale(SERVER_EDIT_REQUEST_BUDGET, percent),
            server_batch_request: scale(SERVER_BATCH_REQUEST_BUDGET, percent),
            server_render_request: scale(SERVER_RENDER_REQUEST_BUDGET, percent),
            server_artifact_ingest: scale(SERVER_ARTIFACT_INGEST_BUDGET, percent),
            server_connect_wait_cap: scale(SERVER_CONNECT_WAIT_CAP, percent),
            plugin_handshake: scale(PLUGIN_HANDSHAKE_TIMEOUT, percent),
            plugin_read: scale(PLUGIN_READ_TIMEOUT, percent),
            plugin_edit: scale(PLUGIN_EDIT_TIMEOUT, percent),
            plugin_batch: scale(PLUGIN_BATCH_TIMEOUT, percent),
            plugin_render_wait: scale(PLUGIN_RENDER_WAIT_TIMEOUT, percent),
            plugin_render_artifact: scale(PLUGIN_RENDER_ARTIFACT_TIMEOUT, percent),
            plugin_write: scale(PLUGIN_WRITE_TIMEOUT, percent),
            transport_headroom: scale(TRANSPORT_HEADROOM, percent),
        }
    }

    /// 倍率を掛けない予算一式（100%）。
    pub fn unscaled() -> Self {
        Self::checked(100).expect("既定の予算は不等式を満たす")
    }

    /// 適用した整数百分率。
    pub fn percent(self) -> u32 {
        self.percent
    }

    /// 破れている不等式のうち最初のもの。全て成り立つなら `None`。
    pub fn first_violation(self) -> Option<BudgetInequality> {
        BudgetInequality::ALL
            .into_iter()
            .find(|inequality| !self.holds(*inequality))
    }

    /// 指定した不等式が成り立つか。
    pub fn holds(self, inequality: BudgetInequality) -> bool {
        match inequality {
            BudgetInequality::ReadRequest => {
                self.plugin_read + self.plugin_write + self.transport_headroom
                    <= self.server_read_request
            }
            BudgetInequality::EditRequest => {
                self.plugin_edit + self.plugin_write + self.transport_headroom
                    <= self.server_edit_request
            }
            BudgetInequality::BatchRequest => {
                self.plugin_batch + self.plugin_write + self.transport_headroom
                    <= self.server_batch_request
            }
            BudgetInequality::RenderRequest => {
                self.plugin_render_wait
                    + self.plugin_render_artifact
                    + self.plugin_write
                    + self.server_artifact_ingest
                    + self.transport_headroom
                    <= self.server_render_request
            }
            BudgetInequality::Resolve => {
                self.server_connect_wait_cap
                    + self.plugin_handshake
                    + self.plugin_write
                    + self.transport_headroom
                    <= self.server_resolve
            }
            BudgetInequality::WriteAfterRead => {
                self.plugin_write < self.server_read_request.saturating_sub(self.plugin_read)
            }
            BudgetInequality::WriteAfterEdit => {
                self.plugin_write < self.server_edit_request.saturating_sub(self.plugin_edit)
            }
        }
    }

    /// server の解決フェーズ予算。
    pub fn server_resolve(self) -> Duration {
        self.server_resolve
    }

    /// server の 1 候補あたりの接続待ちの上限。
    pub fn server_connect_wait_cap(self) -> Duration {
        self.server_connect_wait_cap
    }

    /// 接続待ちが食い潰してはならない、handshake と ping の取り分。
    ///
    /// [`BudgetInequality::Resolve`] が解決フェーズ予算から差し引く 2 項そのもので
    /// ある。合成を利用側へ置くと、片方だけが倍率を採る形を再び作れてしまう。
    pub fn server_connect_reserve(self) -> Duration {
        self.plugin_handshake.saturating_add(self.plugin_write)
    }

    /// server の成果物引き取りの上限。
    pub fn server_artifact_ingest(self) -> Duration {
        self.server_artifact_ingest
    }

    /// 区分ごとの server 要求フェーズ予算。
    pub fn server_request_phase(self, kind: RequestBudgetKind) -> Duration {
        match kind {
            RequestBudgetKind::Read => self.server_read_request,
            RequestBudgetKind::Edit => self.server_edit_request,
            RequestBudgetKind::Batch => self.server_batch_request,
            RequestBudgetKind::Render => self.server_render_request,
        }
    }

    /// plugin の handshake の上限。
    pub fn plugin_handshake(self) -> Duration {
        self.plugin_handshake
    }

    /// plugin の応答 1 フレームの送信の上限。
    pub fn plugin_write(self) -> Duration {
        self.plugin_write
    }

    /// plugin が描画の完了通知を待つ上限。
    pub fn plugin_render_wait(self) -> Duration {
        self.plugin_render_wait
    }

    /// plugin が描画結果の符号化と書き出しに使う上限。
    pub fn plugin_render_artifact(self) -> Duration {
        self.plugin_render_artifact
    }

    /// 区分ごとの plugin 実行段の上限。
    ///
    /// render は完了待ちと成果物の書き出しの合計であり、内訳ごとの上限は
    /// レンダリングの実行口が持つ。
    pub fn plugin_execution(self, kind: RequestBudgetKind) -> Duration {
        match kind {
            RequestBudgetKind::Read => self.plugin_read,
            RequestBudgetKind::Edit => self.plugin_edit,
            RequestBudgetKind::Batch => self.plugin_batch,
            RequestBudgetKind::Render => self
                .plugin_render_wait
                .saturating_add(self.plugin_render_artifact),
        }
    }

    /// 段の境界に残す余白。
    pub fn transport_headroom(self) -> Duration {
        self.transport_headroom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_values_are_fixed() {
        assert_eq!(SERVER_RESOLVE_BUDGET, Duration::from_secs(5));
        assert_eq!(SERVER_READ_REQUEST_BUDGET, Duration::from_secs(5));
        assert_eq!(SERVER_EDIT_REQUEST_BUDGET, Duration::from_secs(10));
        assert_eq!(SERVER_BATCH_REQUEST_BUDGET, Duration::from_secs(20));
        assert_eq!(SERVER_RENDER_REQUEST_BUDGET, Duration::from_secs(30));
        assert_eq!(SERVER_ARTIFACT_INGEST_BUDGET, Duration::from_secs(4));
        assert_eq!(SERVER_CONNECT_WAIT_CAP, Duration::from_secs(1));
        assert_eq!(PLUGIN_HANDSHAKE_TIMEOUT, Duration::from_secs(2));
        assert_eq!(PLUGIN_READ_TIMEOUT, Duration::from_secs(3));
        assert_eq!(PLUGIN_EDIT_TIMEOUT, Duration::from_secs(8));
        assert_eq!(PLUGIN_BATCH_TIMEOUT, Duration::from_secs(18));
        assert_eq!(PLUGIN_RENDER_WAIT_TIMEOUT, Duration::from_secs(18));
        assert_eq!(PLUGIN_RENDER_ARTIFACT_TIMEOUT, Duration::from_secs(5));
        assert_eq!(PLUGIN_WRITE_TIMEOUT, Duration::from_secs(1));
        assert_eq!(TRANSPORT_HEADROOM, Duration::from_secs(1));
    }

    #[test]
    fn plugin_request_stages_fit_within_the_server_request_budget() {
        let stages = PLUGIN_READ_TIMEOUT + PLUGIN_WRITE_TIMEOUT;
        assert!(
            stages < SERVER_READ_REQUEST_BUDGET,
            "読み取り {stages:?} が要求フェーズ予算 {SERVER_READ_REQUEST_BUDGET:?} を残さない"
        );
        assert!(
            stages + TRANSPORT_HEADROOM <= SERVER_READ_REQUEST_BUDGET,
            "要求フェーズに余白 {TRANSPORT_HEADROOM:?} が残らない"
        );
    }

    #[test]
    fn plugin_edit_stages_fit_within_the_server_edit_request_budget() {
        let stages = PLUGIN_EDIT_TIMEOUT + PLUGIN_WRITE_TIMEOUT;
        assert!(
            stages < SERVER_EDIT_REQUEST_BUDGET,
            "編集 {stages:?} が編集要求フェーズ予算 {SERVER_EDIT_REQUEST_BUDGET:?} を残さない"
        );
        assert!(
            stages + TRANSPORT_HEADROOM <= SERVER_EDIT_REQUEST_BUDGET,
            "編集要求フェーズに余白 {TRANSPORT_HEADROOM:?} が残らない"
        );
    }

    #[test]
    fn plugin_batch_stages_fit_within_the_server_batch_request_budget() {
        let stages = PLUGIN_BATCH_TIMEOUT + PLUGIN_WRITE_TIMEOUT;
        assert!(
            stages < SERVER_BATCH_REQUEST_BUDGET,
            "一括適用 {stages:?} が要求フェーズ予算 {SERVER_BATCH_REQUEST_BUDGET:?} を残さない"
        );
        assert!(
            stages + TRANSPORT_HEADROOM <= SERVER_BATCH_REQUEST_BUDGET,
            "一括適用の要求フェーズに余白 {TRANSPORT_HEADROOM:?} が残らない"
        );
    }

    #[test]
    fn render_stages_fit_within_the_server_render_request_budget() {
        // 応答を受けたあとに続く成果物の引き取りも、要求フェーズ予算の内側で
        // 起きる段として数える。数え忘れると、plugin が上限まで費やした直後に
        // 引き取りが始まり、どの段の上限にも掛からないまま予算を超える。
        let stages = PLUGIN_RENDER_WAIT_TIMEOUT
            + PLUGIN_RENDER_ARTIFACT_TIMEOUT
            + PLUGIN_WRITE_TIMEOUT
            + SERVER_ARTIFACT_INGEST_BUDGET;
        assert!(
            stages < SERVER_RENDER_REQUEST_BUDGET,
            "描画 {stages:?} が要求フェーズ予算 {SERVER_RENDER_REQUEST_BUDGET:?} を残さない"
        );
        assert!(
            stages + TRANSPORT_HEADROOM <= SERVER_RENDER_REQUEST_BUDGET,
            "描画の要求フェーズに余白 {TRANSPORT_HEADROOM:?} が残らない"
        );
    }

    #[test]
    fn artifact_ingest_leaves_room_for_the_ipc_round_trip() {
        // 引き取りの取り分を差し引いた残りが、plugin の各段の合計を覆う。
        // 差し引いた値が要求の期限になるため、覆えなければ成功した描画が
        // 期限超過として捨てられる。
        let ipc = SERVER_RENDER_REQUEST_BUDGET - SERVER_ARTIFACT_INGEST_BUDGET;
        let stages =
            PLUGIN_RENDER_WAIT_TIMEOUT + PLUGIN_RENDER_ARTIFACT_TIMEOUT + PLUGIN_WRITE_TIMEOUT;
        assert!(
            stages + TRANSPORT_HEADROOM <= ipc,
            "引き取りを差し引いた残り {ipc:?} に描画の各段 {stages:?} が収まらない"
        );
    }

    #[test]
    fn plugin_resolve_stages_fit_within_the_server_resolve_budget() {
        assert!(
            PLUGIN_HANDSHAKE_TIMEOUT < SERVER_RESOLVE_BUDGET,
            "handshake が解決フェーズ予算を使い切る"
        );
        let stages = SERVER_CONNECT_WAIT_CAP + PLUGIN_HANDSHAKE_TIMEOUT + PLUGIN_WRITE_TIMEOUT;
        assert!(
            stages < SERVER_RESOLVE_BUDGET,
            "接続待ちと handshake と ping 応答 {stages:?} が解決フェーズ予算 {SERVER_RESOLVE_BUDGET:?} を残さない"
        );
        assert!(
            stages + TRANSPORT_HEADROOM <= SERVER_RESOLVE_BUDGET,
            "解決フェーズに余白 {TRANSPORT_HEADROOM:?} が残らない"
        );
    }

    #[test]
    fn write_budget_survives_a_full_length_read() {
        // 読み取りが上限まで走った後に応答送信を始めても、要求フェーズ予算の
        // 内側に収まる。読み取りの結果を送れない窓を作らないための関係。
        let remaining = SERVER_READ_REQUEST_BUDGET - PLUGIN_READ_TIMEOUT;
        assert!(
            PLUGIN_WRITE_TIMEOUT < remaining,
            "読み取りが上限まで走ると応答送信 {PLUGIN_WRITE_TIMEOUT:?} が残り {remaining:?} に収まらない"
        );
    }

    #[test]
    fn write_budget_survives_a_full_length_edit() {
        // 編集が上限まで走った後に応答送信を始めても、編集要求フェーズ予算の
        // 内側に収まる。編集の結果を送れない窓を作らないための関係。
        let remaining = SERVER_EDIT_REQUEST_BUDGET - PLUGIN_EDIT_TIMEOUT;
        assert!(
            PLUGIN_WRITE_TIMEOUT < remaining,
            "編集が上限まで走ると応答送信 {PLUGIN_WRITE_TIMEOUT:?} が残り {remaining:?} に収まらない"
        );
    }

    #[test]
    fn request_budget_kind_routes_edit_operations() {
        for op in crate::operation::EditOperation::ALL {
            let expected = match op {
                crate::operation::EditOperation::ApplyBatch => RequestBudgetKind::Batch,
                _ => RequestBudgetKind::Edit,
            };
            assert_eq!(
                request_budget_kind(op.as_str()),
                expected,
                "{op:?} の予算区分が想定と異なります"
            );
        }
    }

    #[test]
    fn request_budget_kind_routes_render_operations() {
        for op in crate::operation::RenderOperation::ALL {
            assert_eq!(
                request_budget_kind(op.as_str()),
                RequestBudgetKind::Render,
                "{op:?} が描画として区分されていません"
            );
        }
    }

    #[test]
    fn unscaled_budgets_match_the_constants() {
        // 倍率 100% の一式が定数そのものであること。ここが崩れると、倍率を
        // 設定しない利用者の挙動が黙って変わる。**定数は crate の外から見えず、
        // 一式が唯一の入口である**ため、この対応を確かめられるのはここだけで
        // ある。
        let budgets = ScaledBudgets::unscaled();
        assert_eq!(budgets.percent(), 100);
        assert_eq!(budgets.server_resolve(), SERVER_RESOLVE_BUDGET);
        assert_eq!(budgets.server_connect_wait_cap(), SERVER_CONNECT_WAIT_CAP);
        assert_eq!(
            budgets.server_artifact_ingest(),
            SERVER_ARTIFACT_INGEST_BUDGET
        );
        assert_eq!(
            budgets.server_request_phase(RequestBudgetKind::Read),
            SERVER_READ_REQUEST_BUDGET
        );
        assert_eq!(
            budgets.server_request_phase(RequestBudgetKind::Edit),
            SERVER_EDIT_REQUEST_BUDGET
        );
        assert_eq!(
            budgets.server_request_phase(RequestBudgetKind::Batch),
            SERVER_BATCH_REQUEST_BUDGET
        );
        assert_eq!(
            budgets.server_request_phase(RequestBudgetKind::Render),
            SERVER_RENDER_REQUEST_BUDGET
        );
        assert_eq!(budgets.plugin_handshake(), PLUGIN_HANDSHAKE_TIMEOUT);
        assert_eq!(budgets.plugin_write(), PLUGIN_WRITE_TIMEOUT);
        assert_eq!(
            budgets.server_connect_reserve(),
            PLUGIN_HANDSHAKE_TIMEOUT + PLUGIN_WRITE_TIMEOUT
        );
        assert_eq!(budgets.plugin_render_wait(), PLUGIN_RENDER_WAIT_TIMEOUT);
        assert_eq!(
            budgets.plugin_render_artifact(),
            PLUGIN_RENDER_ARTIFACT_TIMEOUT
        );
        assert_eq!(
            budgets.plugin_execution(RequestBudgetKind::Read),
            PLUGIN_READ_TIMEOUT
        );
        assert_eq!(
            budgets.plugin_execution(RequestBudgetKind::Edit),
            PLUGIN_EDIT_TIMEOUT
        );
        assert_eq!(
            budgets.plugin_execution(RequestBudgetKind::Batch),
            PLUGIN_BATCH_TIMEOUT
        );
        assert_eq!(
            budgets.plugin_execution(RequestBudgetKind::Render),
            PLUGIN_RENDER_WAIT_TIMEOUT + PLUGIN_RENDER_ARTIFACT_TIMEOUT
        );
        assert_eq!(budgets.transport_headroom(), TRANSPORT_HEADROOM);
    }

    #[test]
    fn every_budget_scale_in_range_satisfies_all_inequalities() {
        // 10〜400 の全数（391 通り）を確かめる。倍率が線形性を保つことは
        // 示せるが、ミリ秒への切り捨ては保たない。破れる倍率が実在しなくても
        // この検査は残す — 定数を変えたときに気付ける唯一の場所である。
        let mut rejected = Vec::new();
        for percent in 10..=400u32 {
            match ScaledBudgets::checked(percent) {
                Ok(budgets) => assert_eq!(budgets.percent(), percent),
                Err(inequality) => rejected.push((percent, inequality)),
            }
        }
        assert!(
            rejected.is_empty(),
            "不等式を破る倍率があります: {rejected:?}"
        );
    }

    #[test]
    fn the_connect_reserve_is_the_middle_of_the_resolve_inequality() {
        // 接続待ちの取り分と予約の合計が、余白を残して解決フェーズ予算に収まる。
        // アクセサが `Resolve` の項からずれると、接続待ちが handshake と ping の
        // 持ち時間を食う配分を利用側へ渡せてしまう。
        for percent in 10..=400u32 {
            let budgets = ScaledBudgets::checked(percent).expect("倍率が採用される");
            let spent = budgets.server_connect_wait_cap()
                + budgets.server_connect_reserve()
                + budgets.transport_headroom();
            assert!(
                spent <= budgets.server_resolve(),
                "倍率 {percent}% で接続待ちと予約 {spent:?} が解決フェーズ予算 {:?} に収まらない",
                budgets.server_resolve()
            );
        }
    }

    #[test]
    fn a_budget_scale_that_breaks_an_inequality_is_rejected_with_its_name() {
        // 拒否の経路そのものを確かめる。倍率 0 では全ての予算が 0 になり、
        // 応答送信の持ち時間が残らない。破れた不等式が名前で返ることも
        // あわせて固定する。
        assert_eq!(
            ScaledBudgets::checked(0),
            Err(BudgetInequality::WriteAfterRead)
        );
    }

    #[test]
    fn each_inequality_can_be_observed_independently() {
        // 7 つの不等式が別々の判定であること。1 つに畳まれていると、
        // 破れた理由を記録できない。
        let budgets = ScaledBudgets::unscaled();
        for inequality in BudgetInequality::ALL {
            assert!(
                budgets.holds(inequality),
                "既定の予算が {inequality} を満たしません"
            );
        }
        assert_eq!(budgets.first_violation(), None);
    }

    #[test]
    fn request_budget_kind_routes_read_operations_and_unknown_names_to_read() {
        for name in crate::operation::ReadOperation::ALL
            .into_iter()
            .map(crate::operation::ReadOperation::as_str)
            .chain(["ping", "", "future_operation"])
        {
            assert_eq!(
                request_budget_kind(name),
                RequestBudgetKind::Read,
                "{name} が read として区分されていません"
            );
        }
    }
}
