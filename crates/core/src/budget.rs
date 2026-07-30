//! IPC 1 往復の期限配分。
//!
//! server はフェーズ単位（インスタンス解決・要求）の予算を持ち、その予算を
//! `deadline_unix_ms` として plugin へ伝える。plugin はフェーズを段（handshake・
//! 実行・応答送信）に分け、各段に自前の上限を持つ。両者が別々に値を決めると、
//! plugin が自らの契約どおりに使い切った時点で server の予算が尽き、生きている
//! インスタンスが期限超過として扱われる。
//!
//! そこで配分をここへ一本化し、plugin の各段の合計が server のフェーズ予算より
//! 真に小さいことを本モジュールのテストで固定する。差分は [`TRANSPORT_HEADROOM`]
//! として明示的に残す。
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
