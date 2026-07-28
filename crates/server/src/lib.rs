//! AviUtl2 MCP Plugin 用 stdio MCP server のライブラリ。
//!
//! stdout は MCP プロトコル専用とし、ログは stderr へ出力する。
//! discovery と読み取り operation を MCP の read tool / resource として提供する。

pub mod api;
pub mod discovery;
pub mod identity;
pub mod mcp;
pub mod pipe_client;
pub mod redact;
pub mod win_io;

/// 既定のログレベル。
///
/// operation・correlation_id・所要時間・結果コードの記録は運用上の要求であり、
/// `RUST_LOG` を設定しない利用者でも失われないよう `info` を既定とする。
/// `EnvFilter` の既定は `error` であるため、明示的に上書きする。
const DEFAULT_LOG_FILTER: &str = "info";

/// ログを stderr へ構造化出力するよう初期化する。
///
/// `RUST_LOG` 環境変数でレベルを、`LOG_FORMAT=json` で JSON 出力を制御する。
/// `RUST_LOG` が未設定または解釈できない場合は [`DEFAULT_LOG_FILTER`] を用いる。
pub fn init_logging() {
    let format = std::env::var("LOG_FORMAT").unwrap_or_default();
    let json_mode = format.eq_ignore_ascii_case("json");

    let builder = tracing_subscriber::fmt()
        .with_env_filter(default_env_filter())
        .with_writer(std::io::stderr)
        .with_ansi(false);

    if json_mode {
        builder.json().init();
    } else {
        builder.init();
    }
}

/// `RUST_LOG` を読み、未設定・不正なら既定のレベルへ落とす。
fn default_env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_LOG_FILTER))
}
