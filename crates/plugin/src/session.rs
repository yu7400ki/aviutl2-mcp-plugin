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
use std::sync::Arc;
use std::time::{Duration, Instant};

/// handshake（M1 受信 〜 M3 検証）全体に許す上限。
///
/// handshake は接続確立直後に 3 往復で完結する軽量な処理であり、
/// クライアントの待ち時間は含まない。未応答のクライアントが待受を占有する
/// 時間をこの値に抑える。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// 認証済み接続で次の要求フレームを待つ上限。
///
/// discovery クライアントは接続を保ったまま間欠的に要求を送るため、
/// handshake より長くとる。一方で切断を検知できないまま待受を占有し続けない
/// よう有限にする。
const REQUEST_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// 1 フレームの送信に許す上限。
///
/// 受信側がバッファを読み出さない場合でも送信側が滞留しないようにする。
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

        if !is_compatible(negotiated_version, request.protocol_version) {
            send_error_response(
                stream,
                negotiated_version,
                request.request_id,
                request.instance_id,
                ErrorCode::ProtocolMismatch,
                "要求のプロトコルバージョンが交渉結果と互換ではありません",
            )?;
            continue;
        }

        if request.instance_id != lifecycle.instance_id() {
            send_error_response(
                stream,
                negotiated_version,
                request.request_id,
                request.instance_id,
                ErrorCode::InstanceNotFound,
                "インスタンス ID が一致しません",
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
            )?;
            continue;
        }

        let response = ResponseEnvelope::pong(
            negotiated_version,
            request.request_id,
            lifecycle.instance_id(),
            lifecycle.state(),
        );
        send_response(stream, &response)?;
    }

    Ok(())
}

/// 要求の `protocol_version` が交渉結果と互換かを判定する。
///
/// MAJOR は完全一致、MINOR は交渉結果以下でなければならない。
fn is_compatible(negotiated: ProtocolVersion, requested: ProtocolVersion) -> bool {
    requested.major == negotiated.major && requested.minor <= negotiated.minor
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

fn send_response(stream: &PipeStream, response: &ResponseEnvelope) -> Result<()> {
    let body = serde_json::to_vec(response).context("応答の JSON 直列化に失敗しました")?;
    stream
        .write_frame(&body, Instant::now() + WRITE_TIMEOUT)
        .context("応答の送信に失敗しました")?;
    Ok(())
}

fn send_error_response(
    stream: &PipeStream,
    protocol_version: ProtocolVersion,
    request_id: aviutl2_mcp_core::RequestId,
    instance_id: InstanceId,
    code: ErrorCode,
    message: &str,
) -> Result<()> {
    let response = ResponseEnvelope {
        kind: aviutl2_mcp_core::ResponseKind::Response,
        protocol_version,
        request_id,
        instance_id,
        result: ResponseResult::Err {
            error: ErrorObject::new(code, message, false),
        },
    };
    send_response(stream, &response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatible_when_same_major_and_minor_within_negotiated() {
        let negotiated = ProtocolVersion { major: 1, minor: 3 };
        assert!(is_compatible(
            negotiated,
            ProtocolVersion { major: 1, minor: 3 }
        ));
        assert!(is_compatible(
            negotiated,
            ProtocolVersion { major: 1, minor: 0 }
        ));
    }

    #[test]
    fn incompatible_on_major_mismatch_or_higher_minor() {
        let negotiated = ProtocolVersion { major: 1, minor: 3 };
        assert!(!is_compatible(
            negotiated,
            ProtocolVersion { major: 2, minor: 3 }
        ));
        assert!(!is_compatible(
            negotiated,
            ProtocolVersion { major: 1, minor: 4 }
        ));
    }
}
