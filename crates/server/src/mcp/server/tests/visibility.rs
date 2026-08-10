//! 公開する tool の切り替えの検査。

use super::*;

/// 共有設定を与えたサーバー。
fn server_with(settings_json: &str) -> AviUtl2McpServer {
    let settings = aviutl2_mcp_core::settings::SettingsDocument::parse(settings_json)
        .expect("設定を解析できます")
        .resolve(&aviutl2_mcp_core::settings::Settings::default())
        .0;
    AviUtl2McpServer::with_settings(
        PathBuf::from(r"C:\nonexistent-registry"),
        SettingsSource::fixed(settings),
    )
}

fn visible_names(server: &AviUtl2McpServer) -> std::collections::BTreeSet<String> {
    server
        .visible_tools()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect()
}

#[test]
fn without_settings_every_registered_tool_is_listed() {
    let server = server();
    assert_eq!(visible_names(&server).len(), tools().len());
}

#[test]
fn a_disabled_tool_is_neither_listed_nor_accepted() {
    let server = server_with(r#"{"disabled_tools":["delete_object"]}"#);
    assert!(!visible_names(&server).contains("delete_object"));
    assert!(!server.accepts_tool_call("delete_object"));
    // 巻き添えにしない。
    assert!(server.accepts_tool_call("delete_effect"));
}

#[test]
fn the_always_enabled_tool_survives_being_disabled() {
    let server = server_with(r#"{"disabled_tools":["list_instances","render_frame"]}"#);
    let visible = visible_names(&server);
    assert!(visible.contains(aviutl2_mcp_core::tool::ALWAYS_ENABLED_TOOL));
    assert!(server.accepts_tool_call(aviutl2_mcp_core::tool::ALWAYS_ENABLED_TOOL));
    assert!(!visible.contains("render_frame"));
}

#[test]
fn what_is_listed_is_exactly_what_is_accepted() {
    // 掲載と受付が同じ判定を読むことを、全 tool について固定する。片方だけを
    // 絞る実装になると、掲載していない tool の call が通る。
    let server =
        server_with(r#"{"disabled_tools":["delete_object","apply_batch","list_instances"]}"#);
    let visible = visible_names(&server);
    for tool in tools() {
        assert_eq!(
            visible.contains(tool.name.as_ref()),
            server.accepts_tool_call(&tool.name),
            "{} の掲載と受付が食い違っています",
            tool.name
        );
    }
    assert_eq!(visible.len(), tools().len() - 2);
}

#[test]
fn an_unknown_tool_name_is_not_treated_as_disabled() {
    // 未知の名前は「無効化されている」ではなく「登録されていない」である。
    // 判定を反転させると、未知の tool が tool_disabled を名乗る。
    let server = server_with(r#"{"disabled_tools":["delete_object"]}"#);
    assert!(server.accepts_tool_call("aviutl2_future_tool"));
}

#[test]
fn a_disabled_tool_is_rejected_with_the_documented_code() {
    let server = server_with(r#"{"disabled_tools":["delete_object"]}"#);
    let result = server.reject_disabled_tool("delete_object");
    assert_eq!(result.is_error, Some(true));
    let structured = result
        .structured_content
        .expect("失敗も structuredContent を持つ");
    assert_eq!(structured["code"], serde_json::json!("tool_disabled"));
    assert_eq!(structured["retryable"], serde_json::json!(false));
    assert!(
        structured["correlation_id"].is_string(),
        "correlation_id が付いていません"
    );
    assert!(structured.get("details").is_some());
}
