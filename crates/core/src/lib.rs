//! AviUtl2 MCP プラグインの core crate。
//!
//! plugin/server 双方が共有する契約を提供する。
//!
//! - IPC の外枠: 識別子、Envelope、framing、バージョン交渉、HMAC handshake、
//!   strict JSON、エラーモデル
//! - 読み取りデータモデル: 編集情報・シーン・レイヤー・オブジェクト・effect の
//!   DTO と設定値
//! - 対象の再指定: opaque handle を公開しないセレクターと同一性検証の
//!   fingerprint
//! - 一覧取得: ページ要求・応答メタと切り出し規則
//! - read operation の名前と params / result、編集 operation の名前
//! - IPC 1 往復の期限配分
//!
//! SDK や Windows API には依存しない。所有型のみを定義し、SDK からの変換は
//! plugin 側が行う。

pub mod budget;
pub mod descriptor;
pub mod edit;
pub mod edit_info;
pub mod effect;
pub mod envelope;
pub mod error;
pub mod fingerprint;
pub mod framing;
pub mod handshake;
pub mod identifier;
pub mod item_value;
pub mod json;
pub mod number;
pub mod object;
pub mod operation;
pub mod page;
pub mod selector;
pub mod state;
pub mod validation;
pub mod wire_format;

#[cfg(test)]
mod tests;

pub use budget::{
    PLUGIN_EDIT_TIMEOUT, PLUGIN_HANDSHAKE_TIMEOUT, PLUGIN_READ_TIMEOUT, PLUGIN_WRITE_TIMEOUT,
    RequestBudgetKind, SERVER_CONNECT_WAIT_CAP, SERVER_EDIT_REQUEST_BUDGET,
    SERVER_READ_REQUEST_BUDGET, SERVER_RESOLVE_BUDGET, TRANSPORT_HEADROOM, request_budget_kind,
};
pub use descriptor::{
    AuthSecret, DescriptorProject, InstanceDescriptor, InstanceInfo, InstanceProject,
};
pub use edit::{
    AddEffectParams, CreateObjectParams, CursorPosition, DeleteEffectParams, DeleteObjectParams,
    Destination, EditInputError, EditOutcome, FocusChange, LayerNameChange, LayerStateOutcome,
    MoveObjectParams, ObjectSource, Placement, RangeChange, SelectionField, SelectionState,
    SetEffectEnabledParams, SetLayerStateParams, SetObjectItemParams, SetObjectNameParams,
    SetSelectionParams,
};
pub use edit_info::{Cursor, DisplayRange, EditInfo, Extent, FrameRange, SceneInfo, SceneRef};
pub use effect::{
    AvailableEffect, AvailableEffectItem, EffectFlags, EffectInfo, EffectItem, EffectItemType,
    EffectType, TrackInfo,
};
pub use envelope::{
    PongProject, PongResult, RequestEnvelope, RequestKind, ResponseEnvelope, ResponseKind,
    ResponseResult,
};
pub use error::{ErrorCode, ErrorObject};
pub use fingerprint::{
    EffectFingerprintInput, Fingerprint, FingerprintAlgorithm, FingerprintFormatError,
    ObjectFingerprintInput, effect_fingerprint, object_fingerprint,
};
pub use framing::{
    DecoderState, FrameDecoder, FrameError, MAX_FRAME_SIZE, encode_frame, encode_length,
};
pub use handshake::{
    ClientAuth, ClientHello, Mac, Nonce, ServerAuth, compute_client_mac, compute_server_mac,
    verify_mac,
};
pub use identifier::{InstanceId, ProtocolVersion, RequestId, pipe_name_for};
pub use item_value::{
    ItemValue, ItemWriteError, encode_item_value, prepare_item_write, validate_item_value,
};
pub use json::{JsonStrictError, deserialize_json, parse_json};
pub use number::FiniteF64;
pub use object::{LayerInfo, ObjectDetail, ObjectSummary, SectionRange};
pub use operation::{
    EditOperation, GetCurrentSceneParams, GetCurrentSceneResult, GetEditInfoParams,
    GetObjectParams, ListAvailableEffectsParams, ListAvailableEffectsResult, ListLayersParams,
    ListLayersResult, ListObjectsParams, ListObjectsResult, OPERATION_ADD_EFFECT,
    OPERATION_CREATE_OBJECT, OPERATION_DELETE_EFFECT, OPERATION_DELETE_OBJECT,
    OPERATION_GET_CURRENT_SCENE, OPERATION_GET_EDIT_INFO, OPERATION_GET_OBJECT,
    OPERATION_LIST_AVAILABLE_EFFECTS, OPERATION_LIST_LAYERS, OPERATION_LIST_OBJECTS,
    OPERATION_MOVE_OBJECT, OPERATION_SET_EFFECT_ENABLED, OPERATION_SET_LAYER_STATE,
    OPERATION_SET_OBJECT_ITEM, OPERATION_SET_OBJECT_NAME, OPERATION_SET_SELECTION, ObjectFilter,
    ObjectFilterError,
};
pub use page::{DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT, PageError, PageMeta, PageRequest, take_page};
pub use selector::{EffectSelector, ObjectSelector};
pub use state::InstanceState;
pub use validation::{
    MAX_ALIAS_BYTES, MAX_ITEM_VALUE_BYTES, MAX_NAME_UTF16_UNITS, MAX_PATH_UTF16_UNITS,
    PathSyntaxError, TextSyntaxError, validate_alias, validate_control_free,
    validate_control_free_except_layout, validate_item_text, validate_multiline_item_text,
    validate_name, validate_path,
};
pub use wire_format::{format_hwnd, format_utc_timestamp, parse_utc_timestamp};
