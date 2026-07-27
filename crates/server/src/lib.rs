//! AviUtl2 MCP Plugin 用 stdio MCP server のライブラリ。
//!
//! stdout は MCP プロトコル専用とし、ログは stderr へ出力する。
//! discovery / ping / `aviutl2_list_instances` を提供する。

pub mod api;
pub mod discovery;
pub mod identity;
pub mod pipe_client;

/// ログを stderr へ構造化出力するよう初期化する。
///
/// `RUST_LOG` 環境変数でレベルを、`LOG_FORMAT=json` で JSON 出力を制御する。
pub fn init_logging() {
    let format = std::env::var("LOG_FORMAT").unwrap_or_default();
    let json_mode = format.eq_ignore_ascii_case("json");

    let builder = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .with_ansi(false);

    if json_mode {
        builder.json().init();
    } else {
        builder.init();
    }
}
