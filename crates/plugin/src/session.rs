//! 接続ごとの handshake と ping 処理。
//!
//! 全メッセージは frame 形式でやり取りする。handshake 成功後にのみ
//! `RequestEnvelope` を受理する。panic は `catch_unwind` で捕捉し、
//! 当該接続のみ切断する。

use crate::lifecycle::Lifecycle;
use crate::pipe::PipeStream;
use anyhow::{Context, Result};
use aviutl2_mcp_core::{
    ClientAuth, ClientHello, ErrorCode, ErrorObject, InstanceId, Nonce, ProtocolVersion,
    RequestEnvelope, ResponseEnvelope, ResponseResult, compute_client_mac, compute_server_mac,
    deserialize_json, negotiate, verify_mac,
};
use chrono::Utc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// handshake（M1 受信 〜 M3 検証）全体に許す上限。
///
/// handshake は接続確立直後に 3 往復で完結する軽量な処理であり、
/// クライアントの待ち時間は含まない。未応答のクライアントが待受を占有する
/// 時間をこの値に抑える。
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// 認証済み接続で次の要求フレームを待つ上限。
///
/// 1 接続は handshake → 要求 → 応答の直列で完結し、クライアントは応答受信後に
/// 切断する。したがってこの待機は実質「相手の切断（EOF）を受け取るまで」であり、
/// 通常はミリ秒で終わる。待受インスタンスは 1 本だけで、1 接続の処理中は
/// 新たな接続を受理できないため、黙り込んだクライアントが占有できる時間を
/// この値に抑える。
const REQUEST_IDLE_TIMEOUT: Duration = Duration::from_secs(15);

/// 1 フレームの送信に許す上限。
///
/// 受信側がバッファを読み出さない場合でも送信側が滞留しないようにする。
/// 要求が deadline を指定した場合は、この上限と deadline の短い方を採用する。
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// 1 接続の処理を panic boundary で包んで実行する。
pub fn handle_connection(stream: PipeStream, lifecycle: Arc<Lifecycle>) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Err(e) = run_connection(&stream, &lifecycle) {
            tracing::warn!("接続処理を終了しました: {e:?}");
        }
    }));
    if result.is_err() {
        tracing::error!("接続処理中に panic が発生しました");
    }
}

/// 接続単位のメインループ。
fn run_connection(stream: &PipeStream, lifecycle: &Lifecycle) -> Result<()> {
    let negotiated_version = perform_handshake(stream, lifecycle)?;
    run_request_loop(stream, lifecycle, negotiated_version)
}

/// handshake を実行し、採用プロトコルバージョンを返す。
///
/// 検証に失敗した場合はエラー応答を返さずに `Err` を返し、呼び出し元が接続を
/// 切断する。未認証の相手へ失敗理由を開示しないため、理由はローカルログにのみ
/// 記録する。`auth_secret`・nonce・MAC はログに出さない。
fn perform_handshake(stream: &PipeStream, lifecycle: &Lifecycle) -> Result<ProtocolVersion> {
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;

    let client_hello = read_frame_as::<ClientHello>(stream, deadline)
        .context("ClientHello の受信に失敗しました")?;

    if client_hello.instance_id != lifecycle.instance_id() {
        anyhow::bail!("ClientHello の instance_id が一致しないため接続を切断します");
    }

    let negotiated = negotiate(ProtocolVersion::CURRENT, client_hello.protocol_version)
        .map_err(|_| anyhow::anyhow!("プロトコルバージョンが一致しないため接続を切断します"))?;

    let server_nonce = Nonce::generate();
    let server_mac = compute_server_mac(
        lifecycle.auth_secret().as_bytes(),
        &client_hello.client_nonce,
        &server_nonce,
        &lifecycle.instance_id(),
        &negotiated,
    );

    let server_auth = aviutl2_mcp_core::ServerAuth {
        protocol_version: negotiated,
        instance_id: lifecycle.instance_id(),
        server_nonce,
        pid: lifecycle.descriptor().pid,
        process_created_at: lifecycle.descriptor().process_created_at.clone(),
        server_mac,
    };

    let server_auth_body =
        serde_json::to_vec(&server_auth).context("ServerAuth の JSON 直列化に失敗しました")?;
    stream
        .write_frame(&server_auth_body, deadline)
        .context("ServerAuth の送信に失敗しました")?;

    let client_auth =
        read_frame_as::<ClientAuth>(stream, deadline).context("ClientAuth の受信に失敗しました")?;
    let expected_client_mac = compute_client_mac(
        lifecycle.auth_secret().as_bytes(),
        &server_auth.server_nonce,
        &client_hello.client_nonce,
    );

    if !verify_mac(&expected_client_mac, &client_auth.client_mac) {
        anyhow::bail!("ClientAuth の MAC 検証に失敗したため接続を切断します");
    }

    Ok(negotiated)
}

/// 認証済み接続での要求処理ループ。
///
/// 応答送信直後には閉じず、次の受信でクライアント切断（EOF）か期限超過を
/// 待ってから抜ける。送信済み応答がクライアントに読まれる前にハンドルを
/// 破棄しないための構造。
fn run_request_loop(
    stream: &PipeStream,
    lifecycle: &Lifecycle,
    negotiated_version: ProtocolVersion,
) -> Result<()> {
    loop {
        if lifecycle.state() == aviutl2_mcp_core::state::InstanceState::Draining {
            // draining では新規要求を受け付けず、接続を閉じる。
            break;
        }

        let deadline = Instant::now() + REQUEST_IDLE_TIMEOUT;
        let body = match stream
            .read_frame(deadline)
            .context("要求フレームの受信に失敗しました")?
        {
            Some(b) => b,
            None => break,
        };

        let request: RequestEnvelope = deserialize_json(&body)
            .map_err(|e| anyhow::anyhow!("RequestEnvelope のデコードに失敗しました: {e}"))?;

        match classify_version(negotiated_version, request.protocol_version) {
            VersionCheck::Compatible => {}
            VersionCheck::MinorTooHigh => {
                send_error_response(
                    stream,
                    negotiated_version,
                    request.request_id,
                    request.instance_id,
                    ErrorCode::ProtocolMismatch,
                    "要求の MINOR が交渉結果を超えています",
                    false,
                )?;
                continue;
            }
            VersionCheck::MajorMismatch => {
                // MAJOR 不一致は互換性が無く接続を継続できない。handshake は
                // 完了しているため理由を 1 度返し、以降の要求は処理せずに
                // クライアントの切断を待ってから閉じる。
                send_error_response(
                    stream,
                    negotiated_version,
                    request.request_id,
                    request.instance_id,
                    ErrorCode::ProtocolMismatch,
                    "要求の MAJOR が交渉結果と一致しません",
                    false,
                )?;
                await_peer_close(stream);
                break;
            }
        }

        if request.instance_id != lifecycle.instance_id() {
            send_error_response(
                stream,
                negotiated_version,
                request.request_id,
                request.instance_id,
                ErrorCode::InstanceNotFound,
                "インスタンス ID が一致しません",
                false,
            )?;
            continue;
        }

        if request.operation != "ping" {
            send_error_response(
                stream,
                negotiated_version,
                request.request_id,
                request.instance_id,
                ErrorCode::UnsupportedOperation,
                "未対応の operation です",
                false,
            )?;
            continue;
        }

        // 期限は operation の実行に対する制約であり、要求自体の妥当性検証
        // （version・instance・operation）を通した後に評価する。妥当性の誤りは
        // 再試行しても解消しないため、再試行可能な `timeout` より先に返す。
        let response_deadline = match resolve_request_deadline(
            Instant::now(),
            Utc::now().timestamp_millis(),
            WRITE_TIMEOUT,
            request.deadline_unix_ms,
        ) {
            RequestDeadline::Within(deadline) => deadline,
            RequestDeadline::Exceeded => {
                // 未開始の要求は中止する。副作用が無いため再試行可能として返す。
                send_error_response(
                    stream,
                    negotiated_version,
                    request.request_id,
                    request.instance_id,
                    ErrorCode::Timeout,
                    "要求の deadline を超過したため処理しません",
                    true,
                )?;
                continue;
            }
        };

        let response = ResponseEnvelope::pong(
            negotiated_version,
            request.request_id,
            lifecycle.instance_id(),
            lifecycle.state(),
        );
        send_response(stream, &response, response_deadline)?;
    }

    Ok(())
}

/// 送信済み応答が読み取られるのを待ってから接続を閉じるための待機。
///
/// クライアント切断（EOF）か期限超過まで受信を続け、受け取ったフレームは
/// 処理せずに捨てる。応答送信の直後にハンドルを破棄すると、
/// `DisconnectNamedPipe` が pipe バッファの未読データを捨てるため、
/// クライアントは応答ではなく切断を観測してしまう。
///
/// 期限超過や I/O エラーはいずれも接続を閉じる契機であり、呼び出し元は
/// この待機の成否で処理を変えないため、結果は返さない。
fn await_peer_close(stream: &PipeStream) {
    let deadline = Instant::now() + REQUEST_IDLE_TIMEOUT;
    loop {
        match stream.read_frame(deadline) {
            Ok(Some(_)) => continue,
            Ok(None) => return,
            Err(e) => {
                tracing::debug!("切断待ちを終了しました: {e}");
                return;
            }
        }
    }
}

/// 要求 1 件に対して採用する期限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestDeadline {
    /// 期限内。値は応答送信に使う単調時計上の期限。
    Within(Instant),
    /// 要求を受け取った時点で既に期限を超過している。
    Exceeded,
}

/// 要求の `deadline_unix_ms` とサーバー側上限から、実際に採用する期限を決める。
///
/// 採用するのは両者の短い方であり、`deadline_unix_ms` が未指定の要求には
/// サーバー側上限だけを適用する。
///
/// `deadline_unix_ms` は壁時計（Unix epoch ミリ秒）基準で、`now` の単調時計とは
/// 基準が異なる。そのため `now_unix_ms` との差から残り時間を求め、それを `now` へ
/// 加算して単調時計上の期限に直す。
///
/// 壁時計は時刻調整で前後し得るため、極端な値は次のように扱う。
/// - 遠い未来: サーバー側上限との短い方を採るため、上限を超えて待つことはない。
/// - 過去: 期限超過として扱う。要求元と本プロセスは同一ホストの同一壁時計を参照する
///   ので、往復のミリ秒の間に過去へ回るのは時刻調整に限られる。その場合も要求は
///   未実行のまま中止され副作用が残らず、再試行可能なエラーとして通知できる。
fn resolve_request_deadline(
    now: Instant,
    now_unix_ms: i64,
    server_limit: Duration,
    deadline_unix_ms: Option<u64>,
) -> RequestDeadline {
    let Some(deadline_unix_ms) = deadline_unix_ms else {
        return RequestDeadline::Within(now + server_limit);
    };

    let remaining_ms = i128::from(deadline_unix_ms) - i128::from(now_unix_ms);
    if remaining_ms <= 0 {
        return RequestDeadline::Exceeded;
    }

    // 上限との短い方を採るため、表現できない大きさは上限へ丸めて差し支えない。
    let remaining = Duration::from_millis(u64::try_from(remaining_ms).unwrap_or(u64::MAX));
    RequestDeadline::Within(now + remaining.min(server_limit))
}

/// 要求の `protocol_version` を交渉結果と照合した結果。
#[derive(Debug, PartialEq, Eq)]
enum VersionCheck {
    /// MAJOR 一致かつ MINOR が交渉結果以下。
    Compatible,
    /// MAJOR は一致するが MINOR が交渉結果を超えている。
    MinorTooHigh,
    /// MAJOR が一致しない。
    MajorMismatch,
}

/// 要求の `protocol_version` が交渉結果と互換かを判定する。
fn classify_version(negotiated: ProtocolVersion, requested: ProtocolVersion) -> VersionCheck {
    if requested.major != negotiated.major {
        VersionCheck::MajorMismatch
    } else if requested.minor > negotiated.minor {
        VersionCheck::MinorTooHigh
    } else {
        VersionCheck::Compatible
    }
}

fn read_frame_as<T>(stream: &PipeStream, deadline: Instant) -> Result<T>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let body = stream
        .read_frame(deadline)
        .context("フレームの受信に失敗しました")?
        .context("接続が閉じられました")?;
    let value = deserialize_json(&body)
        .map_err(|e| anyhow::anyhow!("JSON のデコードに失敗しました: {e}"))?;
    Ok(value)
}

/// 応答を `deadline` までに送信する。
fn send_response(
    stream: &PipeStream,
    response: &ResponseEnvelope,
    deadline: Instant,
) -> Result<()> {
    let body = serde_json::to_vec(response).context("応答の JSON 直列化に失敗しました")?;
    stream
        .write_frame(&body, deadline)
        .context("応答の送信に失敗しました")?;
    Ok(())
}

/// エラー応答を送信する。
///
/// 送信の期限には要求の deadline ではなくサーバー側上限を使う。期限超過を伝える
/// 応答まで当の期限で打ち切ると、クライアントは理由を得られないまま切断だけを
/// 観測することになる。
fn send_error_response(
    stream: &PipeStream,
    protocol_version: ProtocolVersion,
    request_id: aviutl2_mcp_core::RequestId,
    instance_id: InstanceId,
    code: ErrorCode,
    message: &str,
    retryable: bool,
) -> Result<()> {
    let response = ResponseEnvelope {
        kind: aviutl2_mcp_core::ResponseKind::Response,
        protocol_version,
        request_id,
        instance_id,
        result: ResponseResult::Err {
            error: ErrorObject::new(code, message, retryable),
        },
    };
    send_response(stream, &response, Instant::now() + WRITE_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEGOTIATED: ProtocolVersion = ProtocolVersion { major: 1, minor: 3 };

    #[test]
    fn compatible_when_same_major_and_minor_within_negotiated() {
        for minor in 0..=3 {
            assert_eq!(
                classify_version(NEGOTIATED, ProtocolVersion { major: 1, minor }),
                VersionCheck::Compatible
            );
        }
    }

    #[test]
    fn minor_above_negotiated_is_rejected() {
        assert_eq!(
            classify_version(NEGOTIATED, ProtocolVersion { major: 1, minor: 4 }),
            VersionCheck::MinorTooHigh
        );
    }

    #[test]
    fn major_mismatch_is_rejected() {
        assert_eq!(
            classify_version(NEGOTIATED, ProtocolVersion { major: 2, minor: 3 }),
            VersionCheck::MajorMismatch
        );
        assert_eq!(
            classify_version(NEGOTIATED, ProtocolVersion { major: 0, minor: 0 }),
            VersionCheck::MajorMismatch
        );
    }

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
}
