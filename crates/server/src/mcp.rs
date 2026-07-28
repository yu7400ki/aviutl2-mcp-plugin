//! MCP 層。
//!
//! discovery と読み取り operation を MCP の read tool / resource として提供する。
//! stdout は MCP プロトコル専用であり、ログは stderr の構造化ログへ出す。

pub mod describe;
pub mod failure;
pub mod input;
pub mod output_schema;
pub mod summary;
