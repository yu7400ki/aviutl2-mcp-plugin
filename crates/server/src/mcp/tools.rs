//! tool の実装。
//!
//! tool は役割ごとの inherent impl に分かれ、それぞれが自身の router を生成する。
//! [`crate::mcp::server::AviUtl2McpServer`] はそれらを合成して 1 つの router を持つ。

mod edit;
mod read;
mod render;

#[cfg(test)]
mod tests;
