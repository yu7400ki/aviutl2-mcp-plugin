//! tool call の境界の検査。

use super::*;

#[tokio::test]
async fn panicking_tool_body_becomes_internal_error() {
    let result = server().run("test_tool", || panic!("意図的な panic")).await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structuredContent がある");
    assert_eq!(structured["code"], serde_json::json!("internal_error"));
    assert!(structured["correlation_id"].is_string());
}

#[tokio::test]
async fn failed_tool_call_carries_correlation_id() {
    let result = server()
        .run("test_tool", || {
            Err(failure::invalid_argument("limit が範囲外です"))
        })
        .await;
    assert_eq!(result.is_error, Some(true));
    let structured = result.structured_content.expect("structuredContent がある");
    assert_eq!(structured["code"], serde_json::json!("invalid_argument"));
    assert_eq!(structured["retryable"], serde_json::json!(false));
    assert!(
        structured["correlation_id"]
            .as_str()
            .is_some_and(|id| id.len() == 36),
        "correlation_id が UUID ではありません: {structured}"
    );
}

#[tokio::test]
async fn successful_tool_call_returns_text_and_structured_content() {
    let result = server()
        .run("test_tool", || {
            Ok(ToolSuccess {
                text: "ok".to_string(),
                structured: serde_json::json!({ "value": 1 }),
            })
        })
        .await;
    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        result.structured_content,
        Some(serde_json::json!({ "value": 1 }))
    );
    assert_eq!(
        result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone()),
        Some("ok".to_string())
    );
}

/// tool result の先頭 text content を取り出す。
fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .expect("text content がある")
}

/// tool 本体を経ていない、router が組み立てた失敗結果。
fn router_argument_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

#[tokio::test]
async fn oversized_tool_text_is_clamped_by_the_call_boundary() {
    // describe を経ずに素の文字列を返す tool を足しても上限は破れない。
    let result = server()
        .run("test_tool", || {
            Ok(ToolSuccess {
                text: "あ".repeat(MAX_TEXT_CHARS * 3),
                structured: serde_json::json!({}),
            })
        })
        .await;
    let text = text_of(&normalize_tool_result("test_tool", result));
    assert!(
        text.chars().count() <= MAX_TEXT_CHARS,
        "上限を超えています: {}",
        text.chars().count()
    );
}

#[test]
fn error_result_text_stays_within_limit() {
    let error = failure::with_correlation_id(
        failure::internal_error("え".repeat(100_000)),
        "0190abcd-1234-7def-89ab-0123456789ab",
    );
    let text = text_of(&normalize_tool_result("test_tool", error_result(&error)));
    assert!(
        text.chars().count() <= MAX_TEXT_CHARS,
        "上限を超えています: {}",
        text.chars().count()
    );
}

#[test]
fn argument_decoding_failure_gains_structured_content() {
    // router は tool 本体を呼ばずに結果を組み立てるため、そのままでは
    // code / retryable / correlation_id が欠ける。
    let result = normalize_tool_result(
        "list_layers",
        router_argument_error("failed to deserialize parameters: unknown field `future`"),
    );

    assert_eq!(result.is_error, Some(true));
    let structured = result
        .structured_content
        .as_ref()
        .expect("structuredContent がある");
    assert_eq!(structured["code"], serde_json::json!("invalid_argument"));
    assert_eq!(structured["retryable"], serde_json::json!(false));
    assert!(
        structured["correlation_id"]
            .as_str()
            .is_some_and(|id| id.len() == 36),
        "correlation_id が UUID ではありません: {structured}"
    );
    assert!(
        structured["details"].is_object() || structured["details"].is_null(),
        "details が安全な形ではありません: {structured}"
    );
    // どのフィールドが不正かは残す。
    assert!(text_of(&result).contains("future"), "{}", text_of(&result));
}

#[test]
fn argument_decoding_failure_does_not_echo_the_value() {
    // 引数の復元に失敗した理由には受け取った値がそのまま現れる。編集 tool の
    // 引数は alias・パス・設定値であり、応答へ反響させない。
    let result = normalize_tool_result(
        "create_object",
        router_argument_error(concat!(
            r#"failed to deserialize parameters: invalid type: string "C:\Users\tester\secret.mp4","#,
            " expected u32 at line 1 column 40",
        )),
    );

    let text = text_of(&result);
    let structured = result
        .structured_content
        .as_ref()
        .expect("structuredContent がある");
    let message = structured["message"].as_str().expect("message がある");
    for forbidden in ["secret", "tester", "Users"] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} が text にあります: {text}"
        );
        assert!(
            !message.contains(forbidden),
            "{forbidden} が message にあります: {message}"
        );
    }
    // どのフィールドが不正かを判断する手掛かりは残す。
    assert!(text.contains("expected u32"), "{text}");
}

#[test]
fn argument_decoding_failure_keeps_the_field_name() {
    // フィールド名はバッククォートで囲まれるため、値を伏せても残る。
    let result = normalize_tool_result(
        "set_object_item",
        router_argument_error("failed to deserialize parameters: missing field `selector`"),
    );
    assert!(
        text_of(&result).contains("selector"),
        "{}",
        text_of(&result)
    );
}

#[test]
fn quoted_values_are_redacted_even_when_they_contain_quotes() {
    // 値の中の引用符でも伏せる範囲が終わらない。終われば続きが漏れる。
    let redacted = redact_quoted_values(r#"invalid type: string "秘\"密", expected u32"#);
    assert!(!redacted.contains('秘'), "{redacted}");
    assert!(!redacted.contains('密'), "{redacted}");
    assert!(redacted.contains("expected u32"), "{redacted}");

    // 閉じない引用符は末尾まで落とす。値が漏れる側へ倒れない。
    let redacted = redact_quoted_values(r#"invalid type: string "秘密"#);
    assert!(!redacted.contains('秘'), "{redacted}");
}

#[test]
fn argument_decoding_failure_text_stays_within_limit() {
    // 拒否の説明にはクライアントが送った key がそのまま現れるため、
    // 巨大な key を送られても text は上限に収まらなければならない。
    let key = "k".repeat(100_000);
    let result = normalize_tool_result(
        "list_instances",
        router_argument_error(format!(
            "failed to deserialize parameters: unknown field `{key}`, expected `offset` or `limit`"
        )),
    );
    let text = text_of(&result);
    assert!(
        text.chars().count() <= MAX_TEXT_CHARS,
        "上限を超えています: {}",
        text.chars().count()
    );
    let structured = result.structured_content.expect("structuredContent がある");
    assert!(
        structured["message"]
            .as_str()
            .is_some_and(|message| message.chars().count() <= MAX_TEXT_CHARS),
        "message が上限を超えています: {structured}"
    );
}

#[tokio::test]
async fn tool_results_pass_through_normalization_unchanged() {
    // tool 本体を経た結果は structuredContent を持つため組み直さない。
    for expected in [
        server()
            .run("test_tool", || {
                Ok(ToolSuccess {
                    text: "ok".to_string(),
                    structured: serde_json::json!({ "value": 1 }),
                })
            })
            .await,
        server()
            .run("test_tool", || Err(failure::invalid_argument("範囲外")))
            .await,
    ] {
        let normalized = normalize_tool_result("test_tool", expected.clone());
        assert_eq!(normalized.content, expected.content);
        assert_eq!(normalized.structured_content, expected.structured_content);
        assert_eq!(normalized.is_error, expected.is_error);
    }
}

#[test]
fn error_result_excludes_secrets_and_handles() {
    let remote = aviutl2_mcp_core::ErrorObject::new(ErrorCode::SdkError, "失敗", false)
        .with_details(serde_json::json!({
            "auth_secret": "s3cret",
            "server_nonce": "n0nce",
            "object_handle": 1234,
            "raw_pointer": "0xdeadbeef",
            "pipe_name": r"\\.\pipe\aviutl2-mcp",
            "current_project_revision": 7,
        }));
    let error = failure::with_correlation_id(
        failure::from_pipe_error(
            &crate::pipe_client::PipeClientError::Remote(Box::new(remote)),
            aviutl2_mcp_core::OPERATION_MOVE_OBJECT,
        ),
        "correlation",
    );
    let result = error_result(&error);
    let serialized = serde_json::to_string(&result).expect("直列化できる");
    for forbidden in ["s3cret", "n0nce", "0xdeadbeef", "pipe"] {
        assert!(
            !serialized.contains(forbidden),
            "{forbidden} が応答に含まれています: {serialized}"
        );
    }
    let structured = result.structured_content.expect("structuredContent がある");
    assert_eq!(structured["code"], serde_json::json!("sdk_error"));
    assert_eq!(
        structured["details"]["current_project_revision"],
        serde_json::json!(7)
    );
    assert_eq!(
        structured["correlation_id"],
        serde_json::json!("correlation")
    );
}
