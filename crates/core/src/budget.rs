//! IPC 1 往復の期限配分。
//!
//! server はフェーズ単位（インスタンス解決・要求）の予算を持ち、その予算を
//! `deadline_unix_ms` として plugin へ伝える。plugin はフェーズを段（handshake・
//! 読み取り実行・応答送信）に分け、各段に自前の上限を持つ。両者が別々に値を
//! 決めると、plugin が自らの契約どおりに使い切った時点で server の予算が尽き、
//! 生きているインスタンスが期限超過として扱われる。
//!
//! そこで配分をここへ一本化し、plugin の各段の合計が server のフェーズ予算より
//! 真に小さいことを本モジュールのテストで固定する。差分は [`TRANSPORT_HEADROOM`]
//! として明示的に残す。

use std::time::Duration;

/// server がインスタンス解決フェーズ全体に許す上限。
///
/// pipe 接続・handshake・ping 往復をこの 1 つの期限で束ねる。
pub const SERVER_RESOLVE_BUDGET: Duration = Duration::from_secs(5);

/// server が read operation 1 件の要求フェーズ全体に許す上限。
///
/// 要求の送信・plugin 側の実行・応答の受信をこの 1 つの期限で束ね、
/// 同じ期限を要求の `deadline_unix_ms` として plugin へ伝える。
pub const SERVER_REQUEST_BUDGET: Duration = Duration::from_secs(5);

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

/// plugin が応答 1 フレームの送信に許す上限。
///
/// 読み取りが実行の上限を使い切っても送信の持ち時間が残るよう、実行とは別枠で
/// 確保する。ping の応答送信にも同じ上限を用いる。
pub const PLUGIN_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// フェーズ予算のうち、どの段の上限にも配分せず残す余白。
///
/// フレームの直列化、pipe への書き込み、スレッドの起床、2 つの時計を読む間の
/// 誤差といった段の境界で生じる時間をここで吸収する。段の上限の合計に本値を
/// 加えてもフェーズ予算を超えないことを、本モジュールのテストで保証する。
pub const TRANSPORT_HEADROOM: Duration = Duration::from_secs(1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_values_are_fixed() {
        assert_eq!(SERVER_RESOLVE_BUDGET, Duration::from_secs(5));
        assert_eq!(SERVER_REQUEST_BUDGET, Duration::from_secs(5));
        assert_eq!(SERVER_CONNECT_WAIT_CAP, Duration::from_secs(1));
        assert_eq!(PLUGIN_HANDSHAKE_TIMEOUT, Duration::from_secs(2));
        assert_eq!(PLUGIN_READ_TIMEOUT, Duration::from_secs(3));
        assert_eq!(PLUGIN_WRITE_TIMEOUT, Duration::from_secs(1));
        assert_eq!(TRANSPORT_HEADROOM, Duration::from_secs(1));
    }

    #[test]
    fn plugin_request_stages_fit_within_the_server_request_budget() {
        let stages = PLUGIN_READ_TIMEOUT + PLUGIN_WRITE_TIMEOUT;
        assert!(
            stages < SERVER_REQUEST_BUDGET,
            "読み取り {stages:?} が要求フェーズ予算 {SERVER_REQUEST_BUDGET:?} を残さない"
        );
        assert!(
            stages + TRANSPORT_HEADROOM <= SERVER_REQUEST_BUDGET,
            "要求フェーズに余白 {TRANSPORT_HEADROOM:?} が残らない"
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
        let remaining = SERVER_REQUEST_BUDGET - PLUGIN_READ_TIMEOUT;
        assert!(
            PLUGIN_WRITE_TIMEOUT < remaining,
            "読み取りが上限まで走ると応答送信 {PLUGIN_WRITE_TIMEOUT:?} が残り {remaining:?} に収まらない"
        );
    }
}
