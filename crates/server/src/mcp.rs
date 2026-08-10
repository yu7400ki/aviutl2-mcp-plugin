//! MCP 層。
//!
//! discovery と読み取り operation を MCP の read tool / resource として、
//! 編集 operation を MCP の編集 tool として提供する。
//! stdout は MCP プロトコル専用であり、ログは stderr の構造化ログへ出す。
//!
//! resource 変更の通知は提供しない。resource の `subscribe` と `listChanged` を
//! capability に立てていないため protocol 上の不整合は生じず、変更の検出は
//! read tool が返す project epoch と revision の照合で行える。
//!
//! tool 一覧の変更は通知する。公開する tool は共有設定で切り替えられるため、
//! 要求元が古い一覧のまま呼び続けないよう `listChanged` を立てる。

pub mod describe;
pub mod edit_input;
pub mod failure;
pub mod input;
pub mod output_schema;
pub mod render;
pub mod server;
pub mod summary;
pub mod tool_catalog;
mod tools;

pub use server::{
    ARTIFACTS_RESOURCE_URI_PREFIX, AviUtl2McpServer, CallLimits, INSTANCES_RESOURCE_URI,
    REGISTRY_DIR_ENV,
};
pub use tool_catalog::{ToolListWatch, ToolVisibility};
