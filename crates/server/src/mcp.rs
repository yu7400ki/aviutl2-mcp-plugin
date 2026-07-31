//! MCP 層。
//!
//! discovery と読み取り operation を MCP の read tool / resource として、
//! 編集 operation を MCP の編集 tool として提供する。
//! stdout は MCP プロトコル専用であり、ログは stderr の構造化ログへ出す。
//!
//! resource 変更の通知は提供しない。`subscribe` と `listChanged` を capability に
//! 立てていないため protocol 上の不整合は生じず、変更の検出は read tool が返す
//! project epoch と revision の照合で行える。

pub mod describe;
pub mod edit_input;
pub mod failure;
pub mod input;
pub mod output_schema;
pub mod render;
pub mod server;
pub mod summary;

pub use server::{
    ARTIFACTS_RESOURCE_URI_PREFIX, AviUtl2McpServer, CallLimits, INSTANCES_RESOURCE_URI,
    REGISTRY_DIR_ENV,
};
