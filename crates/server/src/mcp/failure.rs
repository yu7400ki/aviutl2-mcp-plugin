//! discovery / IPC の失敗を tool result のエラーへ変換する。
//!
//! SDK・IPC の失敗は MCP の protocol error にせず、`isError: true` の tool result
//! として返す。呼び出し側が読める説明を text に、機械可読な `code` /`retryable` /
//! `details` / `correlation_id` を `structuredContent` に載せる。

use crate::discovery::ResolveInstanceError;
use crate::mcp::summary::clamp_chars;
use crate::pipe_client::PipeClientError;
use aviutl2_mcp_core::{ErrorCode, ErrorObject};
use serde_json::{Map, Value};

/// `details` から取り除く key の断片（小文字で比較する）。
///
/// 秘匿値・生ポインタ・内部ハンドル・絶対パスは応答へ出さない。key 名の完全一致では
/// 将来の命名を取りこぼすため、断片を含むかで判定する。
const SENSITIVE_KEY_FRAGMENTS: &[&str] = &[
    "secret",
    "nonce",
    "mac",
    "token",
    "password",
    "credential",
    "pipe",
    "handle",
    "hwnd",
    "pointer",
    "ptr",
    "address",
    "alias",
    "path",
];

/// `details` の文字列値に許す最大文字数。
const MAX_DETAIL_STRING_CHARS: usize = 200;

/// `details` の配列に残す最大要素数。
const MAX_DETAIL_ARRAY_ITEMS: usize = 32;

/// `details` を辿る最大の深さ。
const MAX_DETAIL_DEPTH: usize = 8;

/// エラーメッセージに許す最大文字数。
const MAX_MESSAGE_CHARS: usize = 400;

/// 入力検証の失敗を表すエラーを作る。
pub fn invalid_argument(message: impl Into<String>) -> ErrorObject {
    from_code(ErrorCode::InvalidArgument, message)
}

/// server 内部の想定外失敗を表すエラーを作る。
pub fn internal_error(message: impl Into<String>) -> ErrorObject {
    from_code(ErrorCode::InternalError, message)
}

/// コードと説明からエラーを作る。`retryable` はコードの既定値を用いる。
pub fn from_code(code: ErrorCode, message: impl Into<String>) -> ErrorObject {
    let retryable = code.default_retryable();
    ErrorObject::new(
        code,
        clamp_chars(&message.into(), MAX_MESSAGE_CHARS),
        retryable,
    )
}

/// インスタンス解決の失敗をエラーへ変換する。
///
/// インスタンスが応答を返した場合はその [`ErrorObject`] をそのまま用い、
/// `retry_after_ms` のような待ち直しに必要な情報を落とさない。
pub fn from_resolve_error(error: &ResolveInstanceError) -> ErrorObject {
    if let Some(remote) = error.remote_error() {
        return sanitize(remote.clone());
    }
    from_code(error.error_code(), describe_resolve_error(error))
}

/// 要求送信の失敗をエラーへ変換する。
///
/// 接続先が返したエラー応答はそのまま用いる。
pub fn from_pipe_error(error: &PipeClientError) -> ErrorObject {
    if let PipeClientError::Remote(remote) = error {
        return sanitize(remote.as_ref().clone());
    }
    from_code(error.error_code(), describe_pipe_error(error))
}

/// エラーへ相関 ID を設定する。
pub fn with_correlation_id(error: ErrorObject, correlation_id: &str) -> ErrorObject {
    error.with_correlation_id(correlation_id)
}

/// エラーを `structuredContent` へ載せる形へ変換する。
pub fn structured(error: &ErrorObject) -> Value {
    serde_json::json!({
        "code": error.code.as_snake_case(),
        "message": error.message,
        "retryable": error.retryable,
        "details": error.details,
        "correlation_id": error.correlation_id,
    })
}

/// エラーの text content を組み立てる。
pub fn text(error: &ErrorObject) -> String {
    let retry = if error.retryable {
        "リトライ可能"
    } else {
        "リトライ不可"
    };
    let correlation_id = error.correlation_id.as_deref().unwrap_or("-");
    format!(
        "{} ({retry}): {}\ncorrelation_id={correlation_id}",
        error.code.as_snake_case(),
        clamp_chars(&error.message, MAX_MESSAGE_CHARS),
    )
}

/// 接続先が返したエラーから、外部へ出してよい部分だけを残す。
fn sanitize(error: ErrorObject) -> ErrorObject {
    let details = sanitize_details(&error.details, 0);
    let message = clamp_chars(&error.message, MAX_MESSAGE_CHARS);
    ErrorObject::new(error.code, message, error.retryable).with_details(details)
}

/// `details` から秘匿され得る値と過大な値を取り除く。
pub fn sanitize_details(value: &Value, depth: usize) -> Value {
    if depth >= MAX_DETAIL_DEPTH {
        return Value::Null;
    }
    match value {
        Value::Object(map) => {
            let mut sanitized = Map::new();
            for (key, item) in map {
                if is_sensitive_key(key) {
                    continue;
                }
                sanitized.insert(key.clone(), sanitize_details(item, depth + 1));
            }
            Value::Object(sanitized)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .take(MAX_DETAIL_ARRAY_ITEMS)
                .map(|item| sanitize_details(item, depth + 1))
                .collect(),
        ),
        Value::String(text) => Value::String(clamp_chars(text, MAX_DETAIL_STRING_CHARS)),
        other => other.clone(),
    }
}

/// key が秘匿対象の断片を含むか判定する。
fn is_sensitive_key(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    SENSITIVE_KEY_FRAGMENTS
        .iter()
        .any(|fragment| lowered.contains(fragment))
}

/// インスタンス解決の失敗を、内部構造を明かさない説明にする。
fn describe_resolve_error(error: &ResolveInstanceError) -> &'static str {
    match error {
        ResolveInstanceError::NotRegistered => {
            "指定された instance_id のインスタンスは登録されていません。aviutl2_list_instances で現在のインスタンスを取得してください"
        }
        ResolveInstanceError::Excluded(_) => {
            "指定されたインスタンスの生存確認に失敗しました。aviutl2_list_instances で一覧を取り直してください"
        }
        ResolveInstanceError::Rejected(_) => "インスタンスが要求を受け付けられませんでした",
    }
}

/// 要求送信の失敗を、内部構造を明かさない説明にする。
fn describe_pipe_error(error: &PipeClientError) -> &'static str {
    match error {
        PipeClientError::Timeout => "要求が期限内に完了しませんでした",
        PipeClientError::AuthenticationFailed => "インスタンスとの認証に失敗しました",
        PipeClientError::ProtocolMismatch => {
            "インスタンスのプロトコルバージョンが互換ではありません"
        }
        PipeClientError::Remote(_) => "インスタンスがエラーを返しました",
        _ => {
            "インスタンスとの通信に失敗しました。aviutl2_list_instances で一覧を取り直してください"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::ExclusionReason;

    #[test]
    fn invalid_argument_is_not_retryable() {
        let error = invalid_argument("limit が範囲外です");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(!error.retryable);
    }

    #[test]
    fn not_registered_maps_to_instance_not_found() {
        let error = from_resolve_error(&ResolveInstanceError::NotRegistered);
        assert_eq!(error.code, ErrorCode::InstanceNotFound);
    }

    #[test]
    fn excluded_maps_to_instance_stale() {
        let error = from_resolve_error(&ResolveInstanceError::Excluded(
            ExclusionReason::PipeUnreachable,
        ));
        assert_eq!(error.code, ErrorCode::InstanceStale);
        assert!(error.retryable, "取り直しを促せるようリトライ可能にする");
    }

    #[test]
    fn rejected_keeps_retry_after_ms() {
        let remote = ErrorObject::new(ErrorCode::HostBusy, "起動処理中です", true)
            .with_details(serde_json::json!({ "retry_after_ms": 500 }));
        let error = from_resolve_error(&ResolveInstanceError::Rejected(Box::new(remote)));
        assert_eq!(error.code, ErrorCode::HostBusy);
        assert!(error.retryable);
        assert_eq!(error.details["retry_after_ms"], serde_json::json!(500));
    }

    #[test]
    fn remote_pipe_error_is_preserved() {
        let remote = ErrorObject::new(ErrorCode::PreconditionFailed, "scene が変化しました", true)
            .with_details(serde_json::json!({ "current_project_revision": 12 }));
        let error = from_pipe_error(&PipeClientError::Remote(Box::new(remote)));
        assert_eq!(error.code, ErrorCode::PreconditionFailed);
        assert_eq!(
            error.details["current_project_revision"],
            serde_json::json!(12)
        );
    }

    #[test]
    fn timeout_maps_to_timeout_code() {
        let error = from_pipe_error(&PipeClientError::Timeout);
        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(error.retryable);
    }

    #[test]
    fn desynced_connection_maps_to_instance_stale() {
        let error = from_pipe_error(&PipeClientError::Desynced);
        assert_eq!(error.code, ErrorCode::InstanceStale);
    }

    #[test]
    fn sensitive_details_are_removed() {
        let remote = ErrorObject::new(ErrorCode::InternalError, "失敗", false).with_details(
            serde_json::json!({
                "auth_secret": "s3cret",
                "client_nonce": "abcd",
                "server_mac": "ffff",
                "pipe_name": r"\\.\pipe\aviutl2-mcp-1",
                "object_handle": 12345,
                "raw_pointer": "0x7ffdeadbeef",
                "project_path": r"C:\\Users\\me\\project.aup2",
                "alias": "[vo]",
                "retry_after_ms": 100,
            }),
        );
        let error = from_pipe_error(&PipeClientError::Remote(Box::new(remote)));
        let details = error.details.as_object().expect("details は object");
        for key in [
            "auth_secret",
            "client_nonce",
            "server_mac",
            "pipe_name",
            "object_handle",
            "raw_pointer",
            "project_path",
            "alias",
        ] {
            assert!(!details.contains_key(key), "{key} が残っています");
        }
        assert_eq!(details["retry_after_ms"], serde_json::json!(100));
    }

    #[test]
    fn sensitive_details_are_removed_from_nested_objects() {
        let details = sanitize_details(
            &serde_json::json!({
                "outer": { "auth_secret": "x", "revision": 3 },
                "list": [{ "server_nonce": "y", "count": 1 }],
            }),
            0,
        );
        assert_eq!(
            details,
            serde_json::json!({
                "outer": { "revision": 3 },
                "list": [{ "count": 1 }],
            })
        );
    }

    #[test]
    fn long_detail_strings_are_clamped() {
        let details = sanitize_details(&serde_json::json!({ "note": "あ".repeat(1_000) }), 0);
        let note = details["note"].as_str().expect("note は文字列");
        assert!(note.chars().count() <= MAX_DETAIL_STRING_CHARS);
    }

    #[test]
    fn deep_details_are_dropped() {
        let mut value = serde_json::json!({ "leaf": 1 });
        for _ in 0..MAX_DETAIL_DEPTH + 2 {
            value = serde_json::json!({ "nested": value });
        }
        let sanitized = sanitize_details(&value, 0);
        let text = serde_json::to_string(&sanitized).expect("直列化できる");
        assert!(!text.contains("leaf"), "深すぎる値が残っています: {text}");
    }

    #[test]
    fn structured_error_carries_required_fields() {
        let error = with_correlation_id(
            invalid_argument("limit が範囲外です"),
            "0190abcd-1234-7def-89ab-0123456789ab",
        );
        let value = structured(&error);
        assert_eq!(value["code"], serde_json::json!("invalid_argument"));
        assert_eq!(value["retryable"], serde_json::json!(false));
        assert!(value["details"].is_object() || value["details"].is_null());
        assert_eq!(
            value["correlation_id"],
            serde_json::json!("0190abcd-1234-7def-89ab-0123456789ab")
        );
    }

    #[test]
    fn error_text_mentions_code_and_correlation_id() {
        let error = with_correlation_id(invalid_argument("範囲外"), "correlation");
        let text = text(&error);
        assert!(text.contains("invalid_argument"));
        assert!(text.contains("correlation"));
    }

    #[test]
    fn long_remote_message_is_clamped() {
        let remote = ErrorObject::new(ErrorCode::SdkError, "え".repeat(10_000), false);
        let error = from_pipe_error(&PipeClientError::Remote(Box::new(remote)));
        assert!(error.message.chars().count() <= MAX_MESSAGE_CHARS);
    }
}
