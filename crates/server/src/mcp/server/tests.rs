//! mcp::server の検査。

use super::*;

mod budgets;
mod call_boundary;
mod resources;
mod visibility;

/// tool 定義と応答の組み立てだけを見るサーバー。
///
/// 保管庫を開かない構築口を使う。開くと registry から導いた基底へ保護された
/// DACL を書き込むため、実在しないパスや相対パスを渡す検査で実際の
/// ディレクトリへ触れてしまう。描画の経路は統合テストが確かめる。
pub(in crate::mcp) fn server() -> AviUtl2McpServer {
    AviUtl2McpServer::without_artifact_store(
        PathBuf::from(r"C:\nonexistent-registry"),
        CallLimits::default(),
    )
}

pub(in crate::mcp) fn tools() -> Vec<Tool> {
    server().tools()
}

pub(in crate::mcp) fn tool_named(name: &str) -> Tool {
    tools()
        .into_iter()
        .find(|tool| tool.name == name)
        .unwrap_or_else(|| panic!("{name} が登録されていません"))
}

/// 成果物の保存時間を指定した設定を作る。
fn settings_with_artifact_ttl(ttl: Duration) -> aviutl2_mcp_core::settings::Settings {
    settings_from(&format!(
        r#"{{"artifact":{{"ttl_seconds":{}}}}}"#,
        ttl.as_secs()
    ))
}

/// 設定ファイルの内容から解決済みの設定を作る。
fn settings_from(text: &str) -> aviutl2_mcp_core::settings::Settings {
    aviutl2_mcp_core::settings::SettingsDocument::parse(text)
        .unwrap()
        .resolve(&aviutl2_mcp_core::settings::Settings::default())
        .0
}
