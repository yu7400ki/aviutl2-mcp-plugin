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

/// 1 接続の処理を panic boundary で包んで実行する。
pub fn handle_connection(stream: PipeStream, lifecycle: Arc<Lifecycle>) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Err(e) = run_connection(&stream, &lifecycle) {
            tracing::error!("接続処理中にエラーが発生しました: {e:?}");
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
fn perform_handshake(stream: &PipeStream, lifecycle: &Lifecycle) -> Result<ProtocolVersion> {
    let client_hello =
        read_frame_as::<ClientHello>(stream).context("ClientHello の受信に失敗しました")?;

    if client_hello.instance_id != lifecycle.instance_id() {
        let _ = send_error_response(
            stream,
            client_hello.protocol_version,
            aviutl2_mcp_core::RequestId::new(),
            client_hello.instance_id,
            ErrorCode::InstanceNotFound,
            "インスタンス ID が一致しません",
        );
        anyhow::bail!("ClientHello の instance_id が一致しません");
    }

    let negotiated = match negotiate(ProtocolVersion::CURRENT, client_hello.protocol_version) {
        Ok(v) => v,
        Err(_) => {
            let _ = send_error_response(
                stream,
                client_hello.protocol_version,
                aviutl2_mcp_core::RequestId::new(),
                client_hello.instance_id,
                ErrorCode::ProtocolMismatch,
                "プロトコルバージョンが一致しません",
            );
            anyhow::bail!("プロトコルバージョンが一致しません");
        }
    };

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
        .write_frame(&server_auth_body)
        .context("ServerAuth の送信に失敗しました")?;

    let client_auth =
        read_frame_as::<ClientAuth>(stream).context("ClientAuth の受信に失敗しました")?;
    let expected_client_mac = compute_client_mac(
        lifecycle.auth_secret().as_bytes(),
        &server_auth.server_nonce,
        &client_hello.client_nonce,
    );

    if !verify_mac(&expected_client_mac, &client_auth.client_mac) {
        // クライアントが read で待機している場合、即座に切断するとデッドロックし得るため、
        // エラー応答を送信してから切断する。
        let _ = send_error_response(
            stream,
            negotiated,
            aviutl2_mcp_core::RequestId::new(),
            lifecycle.instance_id(),
            ErrorCode::AuthenticationFailed,
            "認証に失敗しました",
        );
        anyhow::bail!("ClientAuth の MAC 検証に失敗しました");
    }

    Ok(negotiated)
}

/// 認証済み接続での要求処理ループ。
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

        let body = match stream
            .read_frame()
            .context("要求フレームの受信に失敗しました")?
        {
            Some(b) => b,
            None => break,
        };

        let request: RequestEnvelope = deserialize_json(&body)
            .map_err(|e| anyhow::anyhow!("RequestEnvelope のデコードに失敗しました: {e}"))?;

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

fn read_frame_as<T>(stream: &PipeStream) -> Result<T>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let body = stream
        .read_frame()
        .context("フレームの受信に失敗しました")?
        .context("接続が閉じられました")?;
    let value = deserialize_json(&body)
        .map_err(|e| anyhow::anyhow!("JSON のデコードに失敗しました: {e}"))?;
    Ok(value)
}

fn send_response(stream: &PipeStream, response: &ResponseEnvelope) -> Result<()> {
    let body = serde_json::to_vec(response).context("応答の JSON 直列化に失敗しました")?;
    stream
        .write_frame(&body)
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
