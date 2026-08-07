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
//! - read operation の名前と params / result、編集・render operation の名前
//! - 一括適用と描画の params / result
//! - 描画成果物の引き渡し: ディレクトリ名・拡張子・media type・識別子の構文と
//!   パスの組み立て
//! - IPC 1 往復の期限配分
//!
//! SDK や Windows API には依存しない。所有型のみを定義し、SDK からの変換は
//! plugin 側が行う。

pub mod batch;
pub mod budget;
pub mod descriptor;
pub mod digest;
pub mod edit;
pub mod edit_info;
pub mod effect;
pub mod envelope;
pub mod error;
pub mod fingerprint;
pub mod framing;
pub mod handoff;
pub mod handshake;
pub mod identifier;
pub mod item_value;
pub mod json;
mod kind;
pub mod module;
pub mod number;
pub mod object;
pub mod operation;
pub mod page;
pub mod palette;
pub mod render;
pub mod selector;
pub mod settings;
pub mod state;
pub mod text_codec;
pub mod tool;
pub mod track_value;
pub mod validation;
pub mod wire_format;

#[cfg(test)]
mod tests;

pub use batch::{
    ApplyBatchParams, BatchInputError, BatchOperation, BatchOutcome, BatchStepOutcome,
    MAX_BATCH_OPERATIONS,
};
pub use budget::{BudgetInequality, RequestBudgetKind, ScaledBudgets, request_budget_kind};
pub use descriptor::{
    AuthSecret, DescriptorProject, InstanceDescriptor, InstanceInfo, InstanceProject,
};
pub use digest::{SHA256_HEX_LEN, SHA256_PREFIX, format_sha256};
pub use edit::{
    AddEffectParams, CreateObjectParams, CreateObjectSectionParams, CursorPosition,
    DeleteEffectParams, DeleteObjectParams, DeleteObjectSectionParams, Destination, DisplayStart,
    EditInputError, EditOutcome, FocusChange, GridBpmOutcome, LayerNameChange, LayerStateOutcome,
    MAX_GRID_BPM_ENTRIES, MAX_POSITION, MoveObjectParams, MoveObjectSectionParams,
    ObjectSectionsOutcome, ObjectSource, ObservedSelection, Placement, RangeChange,
    SceneSettingsOutcome, SceneSize, SelectionField, SelectionState, SetEffectEnabledParams,
    SetGridBpmParams, SetLayerStateParams, SetObjectItemParams, SetObjectNameParams,
    SetSceneSettingsParams, SetSelectionParams,
};
pub use edit_info::{Cursor, DisplayRange, EditInfo, Extent, FrameRange, GridBpm, SceneInfo};
pub use effect::{
    AvailableEffect, AvailableEffectItem, EffectDescription, EffectFlags, EffectInfo, EffectItem,
    EffectItemDescription, EffectItemType, EffectType, EvaluatedItemKind, ItemChoices, ItemRange,
    TableSource, TrackInfo,
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
pub use handoff::{
    ARTIFACT_EXTENSION, ARTIFACT_MEDIA_TYPE, HANDOFF_DIR, HANDOFF_TOKEN_LEN, HandoffToken,
    HandoffTokenFormatError, TEMP_EXTENSION, handoff_dir, handoff_file, handoff_temp_file,
};
pub use handshake::{
    ClientAuth, ClientHello, Mac, Nonce, ServerAuth, compute_client_mac, compute_server_mac,
    verify_mac,
};
pub use identifier::{InstanceId, ProtocolVersion, RequestId, pipe_name_for};
pub use item_value::{
    ItemValue, ItemWrite, ItemWriteError, ReadBackCheck, ReadBackComparison, ReadBackNotVerified,
    movement_check_reads_current_value, parse_check_value, prepare_item_write, read_back_check,
    validate_item_value, write_drops_existing_movement,
};
pub use json::{JsonStrictError, deserialize_json, parse_json};
pub use module::{ModuleEntry, ModuleType};
pub use number::FiniteF64;
pub use object::{LayerInfo, ObjectDetail, ObjectSummary, SectionRange};
pub use operation::{
    DescribeEffectsInputError, DescribeEffectsParams, DescribeEffectsResult, EditOperation,
    EffectItemValues, EffectItemValuesInputError, EvaluatedItem, GetCurrentSceneParams,
    GetCurrentSceneResult, GetEditInfoParams, GetEffectItemValuesParams, GetObjectParams,
    GetSelectionParams, KnownOperation, ListAvailableEffectsParams, ListAvailableEffectsResult,
    ListFontsParams, ListFontsResult, ListLayersParams, ListLayersResult, ListModulesParams,
    ListModulesResult, ListObjectAliasesParams, ListObjectAliasesResult, ListObjectsParams,
    ListObjectsResult, ListPalettesParams, ListPalettesResult, MAX_DESCRIBED_EFFECTS,
    MAX_EVALUATED_FRAMES, MAX_EVALUATED_ITEMS, OPERATION_ADD_EFFECT, OPERATION_APPLY_BATCH,
    OPERATION_CREATE_OBJECT, OPERATION_CREATE_OBJECT_SECTION, OPERATION_DELETE_EFFECT,
    OPERATION_DELETE_OBJECT, OPERATION_DELETE_OBJECT_SECTION, OPERATION_DESCRIBE_EFFECTS,
    OPERATION_GET_CURRENT_SCENE, OPERATION_GET_EDIT_INFO, OPERATION_GET_EFFECT_ITEM_VALUES,
    OPERATION_GET_OBJECT, OPERATION_GET_SELECTION, OPERATION_LIST_AVAILABLE_EFFECTS,
    OPERATION_LIST_FONTS, OPERATION_LIST_LAYERS, OPERATION_LIST_MODULES,
    OPERATION_LIST_OBJECT_ALIASES, OPERATION_LIST_OBJECTS, OPERATION_LIST_PALETTES,
    OPERATION_MOVE_OBJECT, OPERATION_MOVE_OBJECT_SECTION, OPERATION_RENDER_FRAME,
    OPERATION_SET_EFFECT_ENABLED, OPERATION_SET_GRID_BPM, OPERATION_SET_LAYER_STATE,
    OPERATION_SET_OBJECT_ITEM, OPERATION_SET_OBJECT_NAME, OPERATION_SET_SCENE_SETTINGS,
    OPERATION_SET_SELECTION, ObjectAliasSummary, ObjectFilter, ObjectFilterError, ReadOperation,
    RenderOperation, SelectionSnapshot, TrackGroup,
};
pub use page::{
    DEFAULT_PAGE_LIMIT, LimitOutOfRange, MAX_PAGE_LIMIT, PageMeta, PageRequest, PageWindow,
    SnapshotRevisionMismatch, ValidatedPageRequest, take_page, take_window,
};
pub use palette::{PALETTE_COLOR_COUNT, PaletteEntry, Rgba};
pub use render::{
    ARTIFACT_MAX_BYTES, MAX_RENDER_FRAME_BYTES, RenderFormat, RenderFrameParams, RenderFrameResult,
    RenderInputError,
};
pub use selector::{EffectSelector, ObjectSelector};
pub use settings::{
    SETTINGS_FILE_ENV, SETTINGS_FILE_NAME, SETTINGS_READ_ATTEMPTS, SETTINGS_SCHEMA_VERSION,
    Settings, SettingsChange, SettingsDocument, SettingsIssue, SettingsIssueReason,
    SettingsLocation, SettingsParseError, SettingsReadError, SettingsReader, SettingsRefresh,
    settings_location, settings_path,
};
pub use state::InstanceState;
pub use text_codec::{decode_host_text, encode_host_text};
pub use tool::{ALWAYS_ENABLED_TOOL, ToolFamily, all_tool_names, togglable_tool_names};
pub use track_value::{
    TrackValue, TrackValueError, TrackWriteTarget, decode_track_value, encode_track_value,
    validate_track_value,
};
pub use validation::{
    MAX_ALIAS_BYTES, MAX_ITEM_VALUE_BYTES, MAX_NAME_UTF16_UNITS, MAX_PATH_UTF16_UNITS,
    PathSyntaxError, TextSyntaxError, limit_item_value_bytes, validate_alias,
    validate_control_free, validate_control_free_except_layout, validate_item_text,
    validate_multiline_item_text, validate_name, validate_object_alias_name, validate_path,
};
pub use wire_format::{format_utc_timestamp, parse_utc_timestamp};
