//! AviUtl2 MCP プラグインの core crate。
//!
//! plugin/server 双方が共有する IPC 契約（識別子、Envelope、framing、
//! バージョン交渉、HMAC handshake）を提供する。

pub mod descriptor;
pub mod edit_info;
pub mod envelope;
pub mod error;
pub mod framing;
pub mod handshake;
pub mod identifier;
pub mod item_value;
pub mod json;
pub mod number;
pub mod page;
pub mod state;
pub mod version;
pub mod wire_format;

#[cfg(test)]
mod tests;

pub use descriptor::{
    AuthSecret, DescriptorProject, InstanceDescriptor, InstanceInfo, InstanceProject,
};
pub use edit_info::{Cursor, DisplayRange, EditInfo, Extent, FrameRange, SceneInfo, SceneRef};
pub use envelope::{RequestEnvelope, RequestKind, ResponseEnvelope, ResponseKind, ResponseResult};
pub use error::{ErrorCode, ErrorObject};
pub use framing::{
    DecoderState, FrameDecoder, FrameError, MAX_FRAME_SIZE, encode_frame, encode_length,
};
pub use handshake::{
    ClientAuth, ClientHello, Mac, Nonce, ServerAuth, compute_client_mac, compute_server_mac,
    verify_mac,
};
pub use identifier::{InstanceId, ProtocolVersion, RequestId, pipe_name_for};
pub use item_value::ItemValue;
pub use json::{JsonStrictError, deserialize_json, parse_json};
pub use number::FiniteF64;
pub use page::{DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT, PageError, PageMeta, PageRequest, take_page};
pub use state::InstanceState;
pub use version::negotiate;
pub use wire_format::{format_hwnd, format_utc_timestamp, parse_utc_timestamp};
