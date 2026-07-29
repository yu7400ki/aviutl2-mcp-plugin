//! MCP stdio サーバーの本体。
//!
//! tool call は 1 回ごとに接続を確立し、応答を受け取ったら破棄する。
//! [`crate::pipe_client::PipeClient`] は生のハンドルを持ち `!Send` であるため、
//! インスタンス解決から要求送信・切断までを 1 つのブロッキングタスクへ閉じ込め、
//! 非同期タスク間で接続が移動しないようにする。

use crate::api::{ListInstancesResponse, aviutl2_list_instances};
use crate::discovery::{DiscoveryConfig, list_registered_instances, resolve_instance};
use crate::mcp::edit_input::{
    AddEffectInput, CreateObjectInput, DeleteEffectInput, DeleteObjectInput, MoveObjectInput,
    SetEffectStateInput, SetObjectItemInput, SetObjectNameInput, SetSelectionInput,
};
use crate::mcp::input::{
    GetObjectInput, InstanceInput, ListAvailableEffectsInput, ListInstancesInput, ListLayersInput,
    ListObjectsInput, parse_instance_id,
};
use crate::mcp::summary::{MAX_TEXT_CHARS, clamp_chars};
use crate::mcp::{describe, failure};
use crate::redact;
use aviutl2_mcp_core::{
    EditInfo, EditOutcome, ErrorCode, ErrorObject, GetCurrentSceneParams, GetCurrentSceneResult,
    GetEditInfoParams, InstanceId, ListAvailableEffectsResult, ListLayersResult, ListObjectsResult,
    MAX_PAGE_LIMIT, OPERATION_ADD_EFFECT, OPERATION_CREATE_OBJECT, OPERATION_DELETE_EFFECT,
    OPERATION_DELETE_OBJECT, OPERATION_GET_CURRENT_SCENE, OPERATION_GET_EDIT_INFO,
    OPERATION_GET_OBJECT, OPERATION_LIST_AVAILABLE_EFFECTS, OPERATION_LIST_LAYERS,
    OPERATION_LIST_OBJECTS, OPERATION_MOVE_OBJECT, OPERATION_SET_EFFECT_STATE,
    OPERATION_SET_OBJECT_ITEM, OPERATION_SET_OBJECT_NAME, OPERATION_SET_SELECTION, ObjectDetail,
    RequestBudgetKind, SERVER_EDIT_REQUEST_BUDGET, SERVER_READ_REQUEST_BUDGET,
    SERVER_RESOLVE_BUDGET, SelectionState, request_budget_kind,
};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, Implementation, ListResourcesResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// registry ディレクトリを上書きする環境変数。
pub const REGISTRY_DIR_ENV: &str = "AVIUTL2_MCP_REGISTRY_DIR";

/// インスタンス一覧の resource URI。
pub const INSTANCES_RESOURCE_URI: &str = "aviutl2://instances";

/// resource の内容に用いる MIME type。
const RESOURCE_MIME_TYPE: &str = "application/json";

/// tool call 1 回分の実行予算。
///
/// 要求が運ぶ期限とサーバー上限の短い方が採用されるため、ここではサーバー上限を持つ。
/// 既定値は接続先と共有する配分から取る。接続先は自身の各段の上限をこの予算の
/// 内側に収めるため、既定値を延ばす分には安全だが、縮めると接続先が上限まで
/// 使った段の途中で予算が尽きる。
///
/// 要求フェーズの予算は read と edit で異なる。どちらを使うかは
/// [`request_budget`](CallLimits::request_budget) が operation 名から選ぶ。
#[derive(Debug, Clone, Copy)]
pub struct CallLimits {
    /// インスタンス解決（接続・handshake・ping）の期限。
    pub resolve: Duration,
    /// read operation 1 件の期限。
    pub request: Duration,
    /// 編集 operation 1 件の期限。
    pub edit_request: Duration,
}

impl Default for CallLimits {
    fn default() -> Self {
        Self {
            resolve: SERVER_RESOLVE_BUDGET,
            request: SERVER_READ_REQUEST_BUDGET,
            edit_request: SERVER_EDIT_REQUEST_BUDGET,
        }
    }
}

impl CallLimits {
    /// operation 名に応じた要求フェーズの期限を返す。
    ///
    /// read か edit かの判定は core の選択規則（[`request_budget_kind`]）に
    /// 委ねる。判定基準を server が独自に持たないことで、片方だけ取り違えた
    /// ときに検出できない状態を避ける。
    pub fn request_budget(&self, operation: &str) -> Duration {
        match request_budget_kind(operation) {
            RequestBudgetKind::Read => self.request,
            RequestBudgetKind::Edit => self.edit_request,
        }
    }
}

/// AviUtl2 の読み取りと編集を提供する MCP サーバー。
#[derive(Debug, Clone)]
pub struct AviUtl2McpServer {
    registry_dir: Arc<PathBuf>,
    limits: CallLimits,
    tool_router: ToolRouter<Self>,
}

/// 成功した tool call の応答内容。
struct ToolSuccess {
    text: String,
    structured: Value,
}

impl AviUtl2McpServer {
    /// registry ディレクトリを指定してサーバーを作る。
    pub fn new(registry_dir: PathBuf) -> Self {
        Self::with_limits(registry_dir, CallLimits::default())
    }

    /// 実行予算を指定してサーバーを作る。
    pub fn with_limits(registry_dir: PathBuf, limits: CallLimits) -> Self {
        Self {
            registry_dir: Arc::new(registry_dir),
            limits,
            tool_router: Self::tool_router(),
        }
    }

    /// 登録済みの tool 定義を返す。
    pub fn tools(&self) -> Vec<rmcp::model::Tool> {
        self.tool_router.list_all()
    }

    /// tool call 1 回をブロッキングタスクで実行し、結果を tool result へ変換する。
    ///
    /// `body` の panic はタスク境界で捕捉し `internal_error` として隔離する。
    ///
    /// 成功・失敗・panic のいずれでも `structuredContent` を設定する。この不変条件が
    /// [`normalize_tool_result`] の判別の根拠であり、崩すと tool 本体を経た結果が
    /// 引数の拒否として組み直されてしまう。
    async fn run<F>(&self, tool: &'static str, body: F) -> CallToolResult
    where
        F: FnOnce() -> Result<ToolSuccess, ErrorObject> + Send + 'static,
    {
        let correlation_id = new_correlation_id();
        let span = tracing::info_span!(
            "mcp_tool_call",
            component = "mcp",
            operation = tool,
            correlation_id = %correlation_id,
        );
        let started = Instant::now();

        let joined = {
            let span = span.clone();
            tokio::task::spawn_blocking(move || {
                let _entered = span.enter();
                body()
            })
            .await
        };

        let _entered = span.enter();
        let duration_ms = started.elapsed().as_millis();
        match joined {
            Ok(Ok(success)) => {
                tracing::info!(duration_ms, result = "ok", "tool call succeeded");
                let mut result = CallToolResult::success(vec![ContentBlock::text(success.text)]);
                result.structured_content = Some(success.structured);
                result
            }
            Ok(Err(error)) => {
                let error = failure::with_correlation_id(error, &correlation_id);
                tracing::warn!(
                    duration_ms,
                    result = %error.code.as_snake_case(),
                    retryable = error.retryable,
                    "tool call failed",
                );
                error_result(&error)
            }
            Err(_) => {
                // spawn_blocking は body の panic を join の失敗として返す。
                let error = failure::with_correlation_id(
                    failure::internal_error("tool の実行が異常終了しました"),
                    &correlation_id,
                );
                tracing::error!(duration_ms, result = "internal_error", "tool call panicked");
                error_result(&error)
            }
        }
    }

    /// resource 要求 1 件をブロッキングタスクで実行する。
    async fn run_resource<T, F>(&self, operation: &'static str, body: F) -> Result<T, McpError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, ErrorObject> + Send + 'static,
    {
        let correlation_id = new_correlation_id();
        let span = tracing::info_span!(
            "mcp_resource",
            component = "mcp",
            operation = operation,
            correlation_id = %correlation_id,
        );

        let joined = {
            let span = span.clone();
            tokio::task::spawn_blocking(move || {
                let _entered = span.enter();
                body()
            })
            .await
        };

        let _entered = span.enter();
        match joined {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => {
                let error = failure::with_correlation_id(error, &correlation_id);
                tracing::warn!(result = %error.code.as_snake_case(), "resource request failed");
                Err(to_mcp_error(&error))
            }
            Err(_) => {
                tracing::error!(result = "internal_error", "resource request panicked");
                Err(McpError::internal_error(
                    "resource の取得が異常終了しました",
                    None,
                ))
            }
        }
    }

    /// ブロッキングタスクへ渡す registry ディレクトリの複製。
    fn registry_dir(&self) -> Arc<PathBuf> {
        Arc::clone(&self.registry_dir)
    }
}

#[tool_router(router = tool_router)]
impl AviUtl2McpServer {
    /// 生存確認済みの AviUtl2 インスタンスを列挙する。
    /// 返る instance_id は他のすべての tool で必須の引数となる。
    /// 本サーバーが扱う frame 番号と layer 番号はいずれも 0 始まりである。
    /// offset と limit（1〜200、既定 50）でページを指定する。
    /// 他の一覧 tool と異なり結果は page オブジェクトを持たず、
    /// 件数と続きは instances と同じ階層に並ぶ。snapshot_revision の概念も無い。
    /// 生存確認は実行中の要求と競合し得るため、稼働中のインスタンスが
    /// その回の一覧から一時的に外れることがある。期待した instance_id が
    /// 見つからない場合は取り直す。
    #[tool(
        name = "aviutl2_list_instances",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_instances()
        )
    )]
    pub async fn aviutl2_list_instances(
        &self,
        Parameters(input): Parameters<ListInstancesInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        self.run("aviutl2_list_instances", move || {
            let response = list_instances(&registry_dir, input)?;
            Ok(ToolSuccess {
                text: describe::instances(&response),
                structured: to_structured(&response)?,
            })
        })
        .await
    }

    /// 現在の編集情報（シーン・カーソル・表示範囲・選択範囲・revision）を取得する。
    /// frame 番号と layer 番号はいずれも 0 始まりである。
    #[tool(
        name = "aviutl2_get_edit_info",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::edit_info()
        )
    )]
    pub async fn aviutl2_get_edit_info(
        &self,
        Parameters(input): Parameters<InstanceInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_get_edit_info", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let result: EditInfo = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_GET_EDIT_INFO,
                &GetEditInfoParams {},
            )?;
            Ok(ToolSuccess {
                text: describe::edit_info(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// 現在シーンの情報と取得時点の project_revision を取得する。
    /// frame 番号と layer 番号はいずれも 0 始まりである。
    #[tool(
        name = "aviutl2_get_current_scene",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::current_scene()
        )
    )]
    pub async fn aviutl2_get_current_scene(
        &self,
        Parameters(input): Parameters<InstanceInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_get_current_scene", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let result: GetCurrentSceneResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_GET_CURRENT_SCENE,
                &GetCurrentSceneParams {},
            )?;
            Ok(ToolSuccess {
                text: describe::current_scene(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// 現在シーンのレイヤーを列挙する。
    /// layer 番号は 0 始まりであり、frame 番号も 0 始まりである。
    /// offset と limit（1〜200、既定 50）でページを指定し、
    /// 2 ページ目以降は先頭ページが返した snapshot_revision を添える。
    #[tool(
        name = "aviutl2_list_layers",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_layers()
        )
    )]
    pub async fn aviutl2_list_layers(
        &self,
        Parameters(input): Parameters<ListLayersInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_list_layers", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListLayersResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_LIST_LAYERS,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::layers(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// 現在シーンのオブジェクトを列挙する。
    /// frame 番号と layer 番号はいずれも 0 始まりである。
    /// 各要素の selector は aviutl2_get_object へそのまま渡せる。
    /// offset と limit（1〜200、既定 50）でページを指定し、
    /// 2 ページ目以降は先頭ページが返した snapshot_revision を添える。
    #[tool(
        name = "aviutl2_list_objects",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_objects()
        )
    )]
    pub async fn aviutl2_list_objects(
        &self,
        Parameters(input): Parameters<ListObjectsInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_list_objects", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListObjectsResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_LIST_OBJECTS,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::objects(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// オブジェクトの詳細（alias・中間点区間・effect・revision）を取得する。
    /// frame 番号と layer 番号はいずれも 0 始まりである。
    /// selector には aviutl2_list_objects が返した値をそのまま指定する。
    #[tool(
        name = "aviutl2_get_object",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::object_detail()
        )
    )]
    pub async fn aviutl2_get_object(
        &self,
        Parameters(input): Parameters<GetObjectInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_get_object", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ObjectDetail = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_GET_OBJECT,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::object_detail(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// インスタンスが利用できる effect の一覧と設定項目の定義を取得する。
    /// effect_type を指定すると種別で絞り込める。
    /// offset と limit（1〜200、既定 50）でページを指定する。
    /// snapshot_revision は受理するがページ間の照合には用いない。
    /// effect カタログは登録済みプラグインの集合であり、プロジェクトの revision に連動しないためである。
    #[tool(
        name = "aviutl2_list_available_effects",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_available_effects()
        )
    )]
    pub async fn aviutl2_list_available_effects(
        &self,
        Parameters(input): Parameters<ListAvailableEffectsInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_list_available_effects", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListAvailableEffectsResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_LIST_AVAILABLE_EFFECTS,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::available_effects(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// メディアファイルまたは object alias からオブジェクトを作成する。
    /// frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。
    /// expected には直前の読み取りまたは編集の応答が返した project_epoch と
    /// project_revision をそのまま指定する。省略はできない。
    /// 応答が返した selector は読み直さずにそのまま次の編集へ渡せる。
    /// 複数オブジェクトを含む alias は全てが作成され、created に全件、object に
    /// その先頭が入る。長さと挿入位置はホストが自動調整し得るため、応答が返す
    /// 位置が実際の配置である。
    /// 同じ要求を再送すると重複して作成し得る。成功すると project_revision が
    /// 進むため、同じ expected での再送は precondition_failed となり通常は防がれる。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// この呼び出し 1 回が 1 つの取り消し単位になる。
    #[tool(
        name = "aviutl2_create_object",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::create_object()
        )
    )]
    pub async fn aviutl2_create_object(
        &self,
        Parameters(input): Parameters<CreateObjectInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_create_object", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_CREATE_OBJECT,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::create_object(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// オブジェクトのレイヤーと開始フレームを変更する。
    /// frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。
    /// expected には直前の読み取りまたは編集の応答が返した project_epoch と
    /// project_revision をそのまま指定する。省略はできない。
    /// selector には応答が返した値をそのまま指定する。応答が返した selector は
    /// 読み直さずにそのまま次の編集へ渡せる。
    /// 宛先に既存オブジェクトがある場合は precondition_failed となる。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// この呼び出し 1 回が 1 つの取り消し単位になる。
    #[tool(
        name = "aviutl2_move_object",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::move_object()
        )
    )]
    pub async fn aviutl2_move_object(
        &self,
        Parameters(input): Parameters<MoveObjectInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_move_object", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_MOVE_OBJECT,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::move_object(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// オブジェクト名を変更する。name を省略するか null にすると標準名へ戻す。
    /// frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。
    /// expected には直前の読み取りまたは編集の応答が返した project_epoch と
    /// project_revision をそのまま指定する。省略はできない。
    /// selector には応答が返した値をそのまま指定する。応答が返した selector は
    /// 読み直さずにそのまま次の編集へ渡せる。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// この呼び出し 1 回が 1 つの取り消し単位になる。
    #[tool(
        name = "aviutl2_set_object_name",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_object_name()
        )
    )]
    pub async fn aviutl2_set_object_name(
        &self,
        Parameters(input): Parameters<SetObjectNameInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_set_object_name", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_SET_OBJECT_NAME,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::set_object_name(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// effect の設定項目またはトラックバーの値を変更する。
    /// 設定項目はいずれかの effect に属するため、対象は effect の selector で指す。
    /// frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。
    /// expected には直前の読み取りまたは編集の応答が返した project_epoch と
    /// project_revision をそのまま指定する。省略はできない。
    /// selector には aviutl2_get_object が返した effect の selector をそのまま指定する。
    /// 応答が返した selector は読み直さずにそのまま次の編集へ渡せる。
    /// effect の設定を変えるとそのオブジェクトの fingerprint も変わるため、変更前の
    /// selector で続けて編集すると precondition_failed となる。
    /// 書き込みを公開していない設定項目種別があり、その場合は unsupported_operation
    /// となる。種別は aviutl2_get_object の item_type で確認できる。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// この呼び出し 1 回が 1 つの取り消し単位になる。
    #[tool(
        name = "aviutl2_set_object_item",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_object_item()
        )
    )]
    pub async fn aviutl2_set_object_item(
        &self,
        Parameters(input): Parameters<SetObjectItemInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_set_object_item", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_SET_OBJECT_ITEM,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::set_object_item(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// オブジェクトへ effect を付与する。
    /// effect_name には aviutl2_list_available_effects が返す名前を指定する。
    /// 登録されていない名前は unsupported_operation となる。
    /// frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。
    /// expected には直前の読み取りまたは編集の応答が返した project_epoch と
    /// project_revision をそのまま指定する。省略はできない。
    /// 応答が返した selector は読み直さずにそのまま次の編集へ渡せる。
    /// effect を足すとそのオブジェクトの fingerprint も変わるため、変更前の
    /// selector で続けて編集すると precondition_failed となる。
    /// 同じ要求を再送すると重複して付与し得る。成功すると project_revision が
    /// 進むため、同じ expected での再送は precondition_failed となり通常は防がれる。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// この呼び出し 1 回が 1 つの取り消し単位になる。
    #[tool(
        name = "aviutl2_add_effect",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::add_effect()
        )
    )]
    pub async fn aviutl2_add_effect(
        &self,
        Parameters(input): Parameters<AddEffectInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_add_effect", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_ADD_EFFECT,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::add_effect(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// effect の有効・無効とロック状態を変更する。
    /// enabled と locked の両方を省略した要求は受け付けない。
    /// 出力 item の有効化と、audio effect / 出力 item のロックは変更できず
    /// unsupported_operation となる。
    /// frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。
    /// expected には直前の読み取りまたは編集の応答が返した project_epoch と
    /// project_revision をそのまま指定する。省略はできない。
    /// selector には aviutl2_get_object が返した effect の selector をそのまま指定する。
    /// 応答が返した selector は読み直さずにそのまま次の編集へ渡せる。
    /// effect の状態を変えるとそのオブジェクトの fingerprint も変わるため、変更前の
    /// selector で続けて編集すると precondition_failed となる。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// この呼び出し 1 回が 1 つの取り消し単位になる。
    #[tool(
        name = "aviutl2_set_effect_state",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_effect_state()
        )
    )]
    pub async fn aviutl2_set_effect_state(
        &self,
        Parameters(input): Parameters<SetEffectStateInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_set_effect_state", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_SET_EFFECT_STATE,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::set_effect_state(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// オブジェクトから effect を削除する。
    /// 対象が既に失われている場合は not_found となり、追加の変更は起きない。
    /// frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。
    /// expected には直前の読み取りまたは編集の応答が返した project_epoch と
    /// project_revision をそのまま指定する。省略はできない。
    /// selector には aviutl2_get_object が返した effect の selector をそのまま指定する。
    /// 応答が返した selector は読み直さずにそのまま次の編集へ渡せる。
    /// effect を削除するとそのオブジェクトの fingerprint も変わるため、変更前の
    /// selector で続けて編集すると precondition_failed となる。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// この呼び出し 1 回が 1 つの取り消し単位になる。
    #[tool(
        name = "aviutl2_delete_effect",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::delete_effect()
        )
    )]
    pub async fn aviutl2_delete_effect(
        &self,
        Parameters(input): Parameters<DeleteEffectInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_delete_effect", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_DELETE_EFFECT,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::delete_effect(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// オブジェクトを削除する。
    /// 対象が既に失われている場合は not_found となり、追加の変更は起きない。
    /// frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。
    /// expected には直前の読み取りまたは編集の応答が返した project_epoch と
    /// project_revision をそのまま指定する。省略はできない。
    /// selector には応答が返した値をそのまま指定する。他の編集 tool では応答が
    /// 返した selector をそのまま次の編集へ渡せるが、削除した対象の selector は
    /// 以後どの編集にも使えない。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// この呼び出し 1 回が 1 つの取り消し単位になる。
    #[tool(
        name = "aviutl2_delete_object",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::delete_object()
        )
    )]
    pub async fn aviutl2_delete_object(
        &self,
        Parameters(input): Parameters<DeleteObjectInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_delete_object", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_DELETE_OBJECT,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::delete_object(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// カーソル位置・選択範囲・フォーカス対象を変更する。
    /// cursor と selected_range と focus の 3 つ全てを省略した要求は受け付けない。
    /// frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。
    /// expected には直前の読み取りまたは編集の応答が返した project_epoch と
    /// project_revision をそのまま指定する。省略はできない。
    /// focus の selector には応答が返した値をそのまま指定する。応答が返した
    /// selector は読み直さずにそのまま次の編集へ渡せる。
    /// この tool は取り消し操作で元へ戻る保証が無く、他の編集 tool と異なり
    /// 取り消し単位を作らない。
    /// 応答が返す反映値は編集と原子的に観測したものではなく、ホストが範囲外の値を
    /// クランプした結果である。実際に適用できた項目は applied が示す。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    #[tool(
        name = "aviutl2_set_selection",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_selection()
        )
    )]
    pub async fn aviutl2_set_selection(
        &self,
        Parameters(input): Parameters<SetSelectionInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits;
        self.run("aviutl2_set_selection", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: SelectionState = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_SET_SELECTION,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::selection_state(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AviUtl2McpServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        );
        info.server_info = Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "AviUtl2 の編集内容を読み取り、変更する。aviutl2_list_instances 以外の tool は instance_id が必須である。frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。変更する tool は expected に直前の読み取りまたは編集の応答が返した project_epoch と project_revision を必ず指定する。"
                .to_string(),
        );
        info
    }

    /// tool call を処理する。
    ///
    /// tool router は引数を型へ写せなかった場合、tool 本体を呼ばずに
    /// `isError: true` の結果を自前で組み立てて返す。その結果は本 server の
    /// 応答規約を通っていないため、ここで組み直したうえで返す。判別は
    /// [`normalize_tool_result`] が構造で行う。
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let tool = request.name.to_string();
        let result = self
            .tool_router
            .call(ToolCallContext::new(self, request, context))
            .await?;
        Ok(normalize_tool_result(&tool, result))
    }

    /// resource を列挙する。
    ///
    /// ここでは registry の descriptor を読むだけで、インスタンスへは接続しない。
    /// plugin の pipe は同時 1 接続しか受け付けないため、一覧のたびに生存確認へ
    /// 出ると実行中の tool call と競合して双方を失敗させてしまう。生存確認は
    /// `read_resource` と tool が要求を送る時点で行う。
    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let offset = decode_cursor(request.and_then(|params| params.cursor).as_deref())?;
        let registry_dir = self.registry_dir();
        let registered = self
            .run_resource("resources/list", move || {
                list_registered_instances(&registry_dir).map_err(|_| {
                    failure::from_code(
                        ErrorCode::InternalError,
                        "インスタンス登録情報を読み取れませんでした",
                    )
                })
            })
            .await?;

        // インスタンス一覧そのものは登録の有無によらず読めるため、先頭ページに載せる。
        let mut resources = Vec::new();
        if offset == 0 {
            resources.push(
                Resource::new(INSTANCES_RESOURCE_URI, "aviutl2 instances")
                    .with_description("登録されている AviUtl2 インスタンス一覧")
                    .with_mime_type(RESOURCE_MIME_TYPE),
            );
        }

        let page: Vec<&InstanceId> = registered
            .iter()
            .skip(offset)
            .take(RESOURCES_PAGE_SIZE)
            .collect();
        for instance_id in &page {
            // 表示名には完全な instance_id を載せる。URI が同じ値をそのまま
            // 運ぶため削っても秘匿にならず、[`redact`] はログ専用である。
            // 削った名前は同じ接頭辞を持つインスタンスを見分けられなくする。
            resources.push(
                Resource::new(
                    edit_info_resource_uri(instance_id),
                    format!("aviutl2 edit info {instance_id}"),
                )
                .with_description("インスタンスの現在の編集情報")
                .with_mime_type(RESOURCE_MIME_TYPE),
            );
        }

        let mut result = ListResourcesResult::with_all_items(resources);
        let next_offset = offset.saturating_add(page.len());
        if next_offset < registered.len() {
            // 続きを黙って落とさず、次ページの位置を返す。
            result.next_cursor = Some(encode_cursor(next_offset));
        }
        Ok(result)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let uri = request.uri;
        let registry_dir = self.registry_dir();
        let limits = self.limits;

        let target = parse_resource_uri(&uri)
            .ok_or_else(|| McpError::resource_not_found("未知の resource URI です", None))?;

        let uri_for_content = uri.clone();
        let contents = self
            .run_resource("resources/read", move || {
                let value = match target {
                    ResourceTarget::Instances => {
                        let response = list_instances(
                            &registry_dir,
                            ListInstancesInput {
                                offset: 0,
                                limit: MAX_PAGE_LIMIT,
                            },
                        )?;
                        fitted_instances_value(response)?
                    }
                    ResourceTarget::EditInfo(instance_id) => {
                        let info: EditInfo = request_operation(
                            &registry_dir,
                            instance_id,
                            limits,
                            OPERATION_GET_EDIT_INFO,
                            &GetEditInfoParams {},
                        )?;
                        to_structured(&info)?
                    }
                };
                Ok(
                    ResourceContents::text(resource_text(&value)?, uri_for_content)
                        .with_mime_type(RESOURCE_MIME_TYPE),
                )
            })
            .await?;

        Ok(ReadResourceResult::new(vec![contents]))
    }
}

/// resource 一覧 1 ページに載せるインスタンス数の上限。
const RESOURCES_PAGE_SIZE: usize = 100;

/// 次ページの位置を cursor へ符号化する。
fn encode_cursor(offset: usize) -> String {
    offset.to_string()
}

/// cursor から開始位置を復元する。未指定は先頭を意味する。
fn decode_cursor(cursor: Option<&str>) -> Result<usize, McpError> {
    match cursor {
        None => Ok(0),
        Some(value) => value
            .parse()
            .map_err(|_| McpError::invalid_params("cursor を解釈できません", None)),
    }
}

/// resource contents の text を組み立てる。
///
/// 上限を超える内容は途中で切ると JSON として読めなくなるため、超過した事実だけを
/// 返して対応する tool のページ指定へ誘導する。
fn resource_text(value: &Value) -> Result<String, ErrorObject> {
    let text = pretty_json(value)?;
    if text.chars().count() <= MAX_TEXT_CHARS {
        return Ok(text);
    }
    pretty_json(&serde_json::json!({
        "truncated": true,
        "max_chars": MAX_TEXT_CHARS,
        "reason": "resource の内容が上限を超えました。対応する tool にページ指定を与えて取得してください",
    }))
}

/// インスタンス一覧を、resource の上限に収まる件数まで絞って値にする。
///
/// 落とした分は `has_more` / `next_offset` が示すため、続きは
/// `aviutl2_list_instances` のページ指定で取得できる。
fn fitted_instances_value(mut response: ListInstancesResponse) -> Result<Value, ErrorObject> {
    loop {
        let value = to_structured(&response)?;
        if response.instances.is_empty() || pretty_json(&value)?.chars().count() <= MAX_TEXT_CHARS {
            return Ok(value);
        }
        let keep = response.instances.len() / 2;
        response.instances.truncate(keep);
        response.count = keep as u32;
        response.has_more = true;
        response.next_offset = Some(response.offset.saturating_add(keep as u32));
    }
}

/// 値を読みやすい JSON へ直列化する。
fn pretty_json(value: &Value) -> Result<String, ErrorObject> {
    serde_json::to_string_pretty(value)
        .map_err(|_| failure::internal_error("resource を直列化できませんでした"))
}

/// resource URI が指す対象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceTarget {
    /// インスタンス一覧。
    Instances,
    /// 指定インスタンスの編集情報。
    EditInfo(InstanceId),
}

/// インスタンスの編集情報 resource の URI。
pub fn edit_info_resource_uri(instance_id: &InstanceId) -> String {
    format!("{INSTANCES_RESOURCE_URI}/{instance_id}/edit-info")
}

/// resource URI を解釈する。未知の URI は `None`。
fn parse_resource_uri(uri: &str) -> Option<ResourceTarget> {
    if uri == INSTANCES_RESOURCE_URI {
        return Some(ResourceTarget::Instances);
    }
    let rest = uri
        .strip_prefix(INSTANCES_RESOURCE_URI)?
        .strip_prefix('/')?;
    let id = rest.strip_suffix("/edit-info")?;
    parse_instance_id(id).ok().map(ResourceTarget::EditInfo)
}

/// 発見した全インスタンスを列挙する。
fn list_instances(
    registry_dir: &Path,
    input: ListInstancesInput,
) -> Result<ListInstancesResponse, ErrorObject> {
    let page = input.to_page_request()?;
    aviutl2_list_instances(
        registry_dir,
        crate::api::ListInstancesRequest {
            offset: page.offset,
            limit: page.limit,
        },
    )
    .map_err(|e| failure::from_code(e.error_code(), e.to_string()))
}

/// 対象インスタンスへ operation を 1 件送り、結果を型付きで受け取る。
///
/// 接続は本関数の中で確立し、応答を受け取ったところで破棄する。フレーム境界を
/// 見失った接続を持ち越さないため、接続の再利用は行わない。
///
/// 要求フェーズの期限は operation 名から選ぶ（[`CallLimits::request_budget`]）。
/// 編集は read より長くかかるため、選び違えると応答しているインスタンスを
/// 打ち切ってしまう。
fn request_operation<P, R>(
    registry_dir: &Path,
    instance_id: InstanceId,
    limits: CallLimits,
    operation: &str,
    params: &P,
) -> Result<R, ErrorObject>
where
    P: Serialize,
    R: DeserializeOwned,
{
    let config = DiscoveryConfig {
        per_candidate_deadline: limits.resolve,
    };
    let resolved = resolve_instance(registry_dir, instance_id, config)
        .map_err(|e| failure::from_resolve_error(&e))?;

    let deadline = Instant::now() + limits.request_budget(operation);
    tracing::debug!(
        instance = %redact::instance_id(&instance_id),
        operation,
        "sending request",
    );
    resolved
        .client
        .request_typed(operation, params, deadline)
        .map_err(|e| failure::from_pipe_error(&e))
}

/// tool call ごとの相関 ID を発番する。
fn new_correlation_id() -> String {
    uuid::Uuid::now_v7().as_hyphenated().to_string()
}

/// DTO を `structuredContent` へ載せる値へ変換する。
fn to_structured<T: Serialize>(value: &T) -> Result<Value, ErrorObject> {
    serde_json::to_value(value).map_err(|_| failure::internal_error("応答を直列化できませんでした"))
}

/// 引数を解釈できなかった理由として残す最大文字数。
const MAX_ARGUMENT_ERROR_DETAIL_CHARS: usize = 300;

/// tool router が返した結果を、この server の応答規約へ揃える。
///
/// router は引数を型へ写せなかった場合、tool 本体を呼ばずに `isError: true` の
/// 結果を返す。この経路の判別に router のメッセージ文言は用いない。文言は SDK の
/// 都合で変わり得るため、変わった瞬間に判別が黙って外れる。代わりに構造で判別する。
/// 本 server の tool は成功・失敗のいずれでも `structuredContent` を設定するため、
/// `isError` が真で `structuredContent` を持たない結果は tool 本体を経ていない。
///
/// text content の上限もここで保証する。router が組み立てた結果はクライアントが
/// 送った key をそのまま含み得るため、応答を返す唯一の経路で必ず切り詰める。
fn normalize_tool_result(tool: &str, mut result: CallToolResult) -> CallToolResult {
    if result.is_error == Some(true) && result.structured_content.is_none() {
        let correlation_id = new_correlation_id();
        tracing::warn!(
            component = "mcp",
            operation = tool,
            correlation_id = %correlation_id,
            result = "invalid_argument",
            "tool call rejected before dispatch",
        );
        let error = failure::with_correlation_id(
            failure::invalid_argument(argument_error_message(&result)),
            &correlation_id,
        );
        result = error_result(&error);
    }
    clamp_text_content(&mut result);
    result
}

/// 引数を解釈できなかった旨の説明を組み立てる。
///
/// router が付けた説明はどのフィールドが不正かを示すため残す価値があるが、
/// 受け取った値そのものも含む。値は alias・パス・設定値になり得るため、
/// [`redact_quoted_values`] で伏せたうえで長さを抑えて載せる。
fn argument_error_message(result: &CallToolResult) -> String {
    let detail: String = result
        .content
        .iter()
        .filter_map(|content| content.as_text())
        .map(|text| text.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let detail = redact_quoted_values(detail.trim());
    let detail = clamp_chars(detail.trim(), MAX_ARGUMENT_ERROR_DETAIL_CHARS);
    if detail.is_empty() {
        "引数を解釈できませんでした".to_string()
    } else {
        format!("引数を解釈できませんでした: {detail}")
    }
}

/// 伏せた値の位置を示す表記。
const REDACTED_VALUE: &str = "\"…\"";

/// 二重引用符で囲まれた部分を伏せる。
///
/// 引数を解釈できなかった理由には、どのフィールドが不正かを示す名前と、
/// 受け取った値そのものの両方が現れる。名前は要求を訂正するのに要るが、
/// 値は利用者の内容そのものであり、alias・パス・設定値をそのまま応答へ
/// 反響させることになる。値は二重引用符、フィールド名はバッククォートで
/// 囲まれるため、二重引用符の中だけを落として名前は残す。
///
/// 引用符が閉じない入力では、開いた位置から末尾までを落とす。読める説明が
/// 短くなるだけで、値が漏れる側へは倒れない。
fn redact_quoted_values(detail: &str) -> String {
    let mut redacted = String::with_capacity(detail.len());
    let mut inside = false;
    let mut escaped = false;
    for c in detail.chars() {
        if inside {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                inside = false;
            }
            continue;
        }
        if c == '"' {
            redacted.push_str(REDACTED_VALUE);
            inside = true;
            continue;
        }
        redacted.push(c);
    }
    redacted
}

/// tool result の text content を [`MAX_TEXT_CHARS`] 以内へ収める。
fn clamp_text_content(result: &mut CallToolResult) {
    for content in &mut result.content {
        if let ContentBlock::Text(block) = content
            && block.text.chars().count() > MAX_TEXT_CHARS
        {
            block.text = clamp_chars(&block.text, MAX_TEXT_CHARS);
        }
    }
}

/// エラーを `isError: true` の tool result へ変換する。
///
/// `structuredContent` は宣言済みの `outputSchema`（成功時の result DTO）には
/// 適合しない。MCP は失敗を tool result で表す経路に別 schema を持たず、
/// 呼び出し側が機械的に扱えるのは code / retryable / details / correlation_id で
/// あるため、成功時の形に寄せるより失敗の内訳を残す方を採る。
fn error_result(error: &ErrorObject) -> CallToolResult {
    let mut result = CallToolResult::error(vec![ContentBlock::text(failure::text(error))]);
    result.structured_content = Some(failure::structured(error));
    result
}

/// エラーを resource 応答用の protocol error へ変換する。
///
/// resource には tool result のような失敗表現が無く、protocol error だけが
/// 返せる。コードを潰すと恒久的な失敗と区別できなくなるため、対象が今この
/// server から取得できないことを表すコードは `resource_not_found` へ写し、
/// リトライ可否や `retry_after_ms` は `data` に残して呼び出し側へ渡す。
fn to_mcp_error(error: &ErrorObject) -> McpError {
    let message = failure::text(error);
    let data = Some(failure::structured(error));
    match error.code {
        // 登録が無い、生存確認に失敗した、インスタンスが今は応じられないの
        // いずれも「今は取得できないが後で取得し得る resource」である。
        // `internal_error` は server 自身の不具合を意味するため、待てば解消する
        // 失敗をそこへ寄せると恒久的な障害と読まれてしまう。
        ErrorCode::InstanceNotFound
        | ErrorCode::InstanceStale
        | ErrorCode::HostBusy
        | ErrorCode::EditBlocked
        | ErrorCode::Timeout => McpError::resource_not_found(message, data),
        ErrorCode::InvalidArgument => McpError::invalid_params(message, data),
        _ => McpError::internal_error(message, data),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rmcp::model::Tool;

    /// frame / layer を入出力に持ち、0 始まりであることの明記が要る tool。
    ///
    /// `aviutl2_list_available_effects` は effect カタログだけを扱い frame も
    /// layer も現れないため、ここには含めない。
    const ZERO_BASED_TOOLS: &[&str] = &[
        "aviutl2_list_instances",
        "aviutl2_get_edit_info",
        "aviutl2_get_current_scene",
        "aviutl2_list_layers",
        "aviutl2_list_objects",
        "aviutl2_get_object",
        "aviutl2_create_object",
        "aviutl2_move_object",
        "aviutl2_set_object_name",
        "aviutl2_set_object_item",
        "aviutl2_add_effect",
        "aviutl2_set_effect_state",
        "aviutl2_delete_effect",
        "aviutl2_delete_object",
        "aviutl2_set_selection",
    ];

    /// 読み取り専用の tool。
    const READ_TOOLS: &[&str] = &[
        "aviutl2_list_instances",
        "aviutl2_get_edit_info",
        "aviutl2_get_current_scene",
        "aviutl2_list_layers",
        "aviutl2_list_objects",
        "aviutl2_get_object",
        "aviutl2_list_available_effects",
    ];

    /// 編集 tool と、宣言する annotation。
    ///
    /// 値は `destructive_hint` / `idempotent_hint` の組である。`read_only_hint` は
    /// 全編集 tool で偽、`open_world_hint` も全 tool で偽であるため表に持たない。
    ///
    /// 作成系を冪等と名乗らないのは、再送で重複して作られ得るためである。
    /// `expected` の検証により通常は防がれるが、annotation は「再送が安全である」
    /// と主張しない側へ倒す。
    const EDIT_TOOL_ANNOTATIONS: &[(&str, bool, bool)] = &[
        ("aviutl2_create_object", false, false),
        ("aviutl2_move_object", false, true),
        ("aviutl2_set_object_name", false, true),
        ("aviutl2_set_object_item", false, true),
        ("aviutl2_add_effect", false, false),
        ("aviutl2_set_effect_state", false, true),
        ("aviutl2_delete_effect", true, true),
        ("aviutl2_delete_object", true, true),
        ("aviutl2_set_selection", false, true),
    ];

    fn server() -> AviUtl2McpServer {
        AviUtl2McpServer::new(PathBuf::from(r"C:\nonexistent-registry"))
    }

    fn tools() -> Vec<Tool> {
        server().tools()
    }

    fn tool_named(name: &str) -> Tool {
        tools()
            .into_iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("{name} が登録されていません"))
    }

    #[test]
    fn all_tools_are_registered() {
        let names: std::collections::BTreeSet<String> =
            tools().iter().map(|tool| tool.name.to_string()).collect();
        let expected: std::collections::BTreeSet<String> = READ_TOOLS
            .iter()
            .copied()
            .chain(EDIT_TOOL_ANNOTATIONS.iter().map(|(name, _, _)| *name))
            .map(|name| name.to_string())
            .collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn read_tools_are_annotated_as_read_only() {
        for name in READ_TOOLS {
            let tool = tool_named(name);
            let annotations = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{name} に annotation がありません"));
            assert_eq!(annotations.read_only_hint, Some(true), "{name}");
            assert_eq!(annotations.destructive_hint, Some(false), "{name}");
            assert_eq!(annotations.idempotent_hint, Some(true), "{name}");
            assert_eq!(annotations.open_world_hint, Some(false), "{name}");
        }
    }

    #[test]
    fn edit_tools_are_annotated_as_mutating() {
        for (name, destructive, idempotent) in EDIT_TOOL_ANNOTATIONS {
            let tool = tool_named(name);
            let annotations = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{name} に annotation がありません"));
            assert_eq!(annotations.read_only_hint, Some(false), "{name}");
            assert_eq!(
                annotations.destructive_hint,
                Some(*destructive),
                "{name} の destructiveHint"
            );
            assert_eq!(
                annotations.idempotent_hint,
                Some(*idempotent),
                "{name} の idempotentHint"
            );
            assert_eq!(annotations.open_world_hint, Some(false), "{name}");
        }
    }

    /// tool 名から、その tool が返す result の schema を返す。
    ///
    /// 未知の tool 名で落とす。tool を足したときに結線の検査から漏れない。
    fn expected_output_schema(name: &str) -> Value {
        use crate::mcp::output_schema as schema;
        match name {
            "aviutl2_list_instances" => schema::list_instances(),
            "aviutl2_get_edit_info" => schema::edit_info(),
            "aviutl2_get_current_scene" => schema::current_scene(),
            "aviutl2_list_layers" => schema::list_layers(),
            "aviutl2_list_objects" => schema::list_objects(),
            "aviutl2_get_object" => schema::object_detail(),
            "aviutl2_list_available_effects" => schema::list_available_effects(),
            "aviutl2_create_object" => schema::create_object(),
            "aviutl2_move_object" => schema::move_object(),
            "aviutl2_set_object_name" => schema::set_object_name(),
            "aviutl2_set_object_item" => schema::set_object_item(),
            "aviutl2_add_effect" => schema::add_effect(),
            "aviutl2_set_effect_state" => schema::set_effect_state(),
            "aviutl2_delete_effect" => schema::delete_effect(),
            "aviutl2_delete_object" => schema::delete_object(),
            "aviutl2_set_selection" => schema::set_selection(),
            other => panic!("{other} の outputSchema が定義されていません"),
        }
    }

    #[test]
    fn tools_declare_the_output_schema_of_their_own_result() {
        // schema そのものが DTO と一致していても、tool へ別の result の schema を
        // 結んでしまえば正常な応答が自分の宣言に適合しなくなる。結線まで固定する。
        for tool in tools() {
            let declared = tool
                .output_schema
                .as_ref()
                .unwrap_or_else(|| panic!("{} に outputSchema がありません", tool.name));
            assert_eq!(
                Value::Object(declared.as_ref().clone()),
                expected_output_schema(&tool.name),
                "{} が別の result の schema を宣言しています",
                tool.name
            );
        }
    }

    #[test]
    fn tool_descriptions_state_zero_based_numbering() {
        for tool in tools() {
            let description = tool
                .description
                .as_ref()
                .unwrap_or_else(|| panic!("{} に説明がありません", tool.name));
            if !ZERO_BASED_TOOLS.contains(&tool.name.as_ref()) {
                continue;
            }
            assert!(
                description.contains("0 始まり"),
                "{} の説明に 0 始まりの明記がありません",
                tool.name
            );
        }
    }

    /// 説明の一部を取り出す。
    fn description_of(name: &str) -> String {
        tool_named(name)
            .description
            .as_ref()
            .unwrap_or_else(|| panic!("{name} に説明がありません"))
            .to_string()
    }

    #[test]
    fn edit_tool_descriptions_state_what_costs_the_caller_if_assumed_wrong() {
        // いずれも誤った前提で操作すると損失が生じる事項であり、説明から
        // 落とせない。
        for (name, _, _) in EDIT_TOOL_ANNOTATIONS {
            let description = description_of(name);
            for keyword in [
                "0 始まり",
                "UI の表示とは異なる",
                "expected",
                "省略はできない",
                "selector",
                "change_applied",
                "unknown",
            ] {
                assert!(
                    description.contains(keyword),
                    "{name} の説明に {keyword} がありません"
                );
            }
        }
    }

    #[test]
    fn edit_tool_descriptions_state_the_undo_boundary() {
        for (name, _, _) in EDIT_TOOL_ANNOTATIONS {
            let description = description_of(name);
            if *name == "aviutl2_set_selection" {
                // 取り消しで元へ戻る保証が無いことを、単位を作ると読める文言に
                // 紛れさせない。
                assert!(
                    description.contains("取り消し操作で元へ戻る保証が無く"),
                    "{name} の説明に取り消しの保証が無い旨がありません"
                );
                assert!(
                    !description.contains("1 つの取り消し単位"),
                    "{name} の説明が取り消し単位を作ると読めます"
                );
                continue;
            }
            assert!(
                description.contains("1 つの取り消し単位"),
                "{name} の説明に取り消し単位がありません"
            );
        }
    }

    #[test]
    fn edit_tool_descriptions_state_the_operation_specific_hazards() {
        let hazards: &[(&str, &[&str])] = &[
            (
                "aviutl2_create_object",
                &["全てが作成され", "自動調整", "重複して作成"],
            ),
            ("aviutl2_add_effect", &["fingerprint", "重複して付与"]),
            (
                "aviutl2_set_object_item",
                &["fingerprint", "公開していない設定項目種別", "item_type"],
            ),
            (
                "aviutl2_set_effect_state",
                &["fingerprint", "出力 item", "audio effect", "両方を省略"],
            ),
            ("aviutl2_delete_effect", &["fingerprint", "not_found"]),
            ("aviutl2_delete_object", &["not_found"]),
            (
                "aviutl2_set_selection",
                &["原子的", "クランプ", "全てを省略"],
            ),
        ];
        for (name, keywords) in hazards {
            let description = description_of(name);
            for keyword in *keywords {
                assert!(
                    description.contains(keyword),
                    "{name} の説明に {keyword} がありません"
                );
            }
        }
    }

    #[test]
    fn paginated_tool_descriptions_explain_page_arguments() {
        let mut checked = 0;
        for tool in tools() {
            let properties = tool
                .input_schema
                .get("properties")
                .and_then(|v| v.as_object())
                .unwrap_or_else(|| panic!("{} に properties がありません", tool.name));
            let description = tool
                .description
                .as_ref()
                .unwrap_or_else(|| panic!("{} に説明がありません", tool.name));

            // ページ指定を受け取る tool は、その使い方を説明にも書く。
            if properties.contains_key("limit") {
                checked += 1;
                for keyword in ["offset", "limit"] {
                    assert!(
                        description.contains(keyword),
                        "{} の説明に {keyword} がありません",
                        tool.name
                    );
                }
            }
            if properties.contains_key("snapshot_revision") {
                assert!(
                    description.contains("snapshot_revision"),
                    "{} の説明に snapshot_revision がありません",
                    tool.name
                );
            }
        }
        assert!(checked >= 4, "ページ指定を持つ tool を検査していません");
    }

    #[test]
    fn input_schemas_reject_unknown_fields() {
        for tool in tools() {
            assert_eq!(
                tool.input_schema.get("additionalProperties"),
                Some(&serde_json::json!(false)),
                "{} の入力 schema が未知フィールドを許しています",
                tool.name
            );
        }
    }

    #[test]
    fn instance_id_is_required_except_for_list_instances() {
        for tool in tools() {
            let required = tool
                .input_schema
                .get("required")
                .and_then(|v| v.as_array())
                .map(|items| items.contains(&serde_json::json!("instance_id")))
                .unwrap_or(false);
            if tool.name == "aviutl2_list_instances" {
                assert!(!required, "一覧取得は instance_id を要求しない");
            } else {
                assert!(required, "{} は instance_id を必須にする", tool.name);
            }
        }
    }

    #[tokio::test]
    async fn panicking_tool_body_becomes_internal_error() {
        let result = server().run("test_tool", || panic!("意図的な panic")).await;
        assert_eq!(result.is_error, Some(true));
        let structured = result.structured_content.expect("structuredContent がある");
        assert_eq!(structured["code"], serde_json::json!("internal_error"));
        assert!(structured["correlation_id"].is_string());
    }

    #[tokio::test]
    async fn failed_tool_call_carries_correlation_id() {
        let result = server()
            .run("test_tool", || {
                Err(failure::invalid_argument("limit が範囲外です"))
            })
            .await;
        assert_eq!(result.is_error, Some(true));
        let structured = result.structured_content.expect("structuredContent がある");
        assert_eq!(structured["code"], serde_json::json!("invalid_argument"));
        assert_eq!(structured["retryable"], serde_json::json!(false));
        assert!(
            structured["correlation_id"]
                .as_str()
                .is_some_and(|id| id.len() == 36),
            "correlation_id が UUID ではありません: {structured}"
        );
    }

    #[tokio::test]
    async fn successful_tool_call_returns_text_and_structured_content() {
        let result = server()
            .run("test_tool", || {
                Ok(ToolSuccess {
                    text: "ok".to_string(),
                    structured: serde_json::json!({ "value": 1 }),
                })
            })
            .await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            result.structured_content,
            Some(serde_json::json!({ "value": 1 }))
        );
        assert_eq!(
            result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| t.text.clone()),
            Some("ok".to_string())
        );
    }

    /// tool result の先頭 text content を取り出す。
    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone())
            .expect("text content がある")
    }

    /// tool 本体を経ていない、router が組み立てた失敗結果。
    fn router_argument_error(message: impl Into<String>) -> CallToolResult {
        CallToolResult::error(vec![ContentBlock::text(message.into())])
    }

    #[tokio::test]
    async fn oversized_tool_text_is_clamped_by_the_call_boundary() {
        // describe を経ずに素の文字列を返す tool を足しても上限は破れない。
        let result = server()
            .run("test_tool", || {
                Ok(ToolSuccess {
                    text: "あ".repeat(MAX_TEXT_CHARS * 3),
                    structured: serde_json::json!({}),
                })
            })
            .await;
        let text = text_of(&normalize_tool_result("test_tool", result));
        assert!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "上限を超えています: {}",
            text.chars().count()
        );
    }

    #[test]
    fn error_result_text_stays_within_limit() {
        let error = failure::with_correlation_id(
            failure::internal_error("え".repeat(100_000)),
            "0190abcd-1234-7def-89ab-0123456789ab",
        );
        let text = text_of(&normalize_tool_result("test_tool", error_result(&error)));
        assert!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "上限を超えています: {}",
            text.chars().count()
        );
    }

    #[test]
    fn argument_decoding_failure_gains_structured_content() {
        // router は tool 本体を呼ばずに結果を組み立てるため、そのままでは
        // code / retryable / correlation_id が欠ける。
        let result = normalize_tool_result(
            "aviutl2_list_layers",
            router_argument_error("failed to deserialize parameters: unknown field `future`"),
        );

        assert_eq!(result.is_error, Some(true));
        let structured = result
            .structured_content
            .as_ref()
            .expect("structuredContent がある");
        assert_eq!(structured["code"], serde_json::json!("invalid_argument"));
        assert_eq!(structured["retryable"], serde_json::json!(false));
        assert!(
            structured["correlation_id"]
                .as_str()
                .is_some_and(|id| id.len() == 36),
            "correlation_id が UUID ではありません: {structured}"
        );
        assert!(
            structured["details"].is_object() || structured["details"].is_null(),
            "details が安全な形ではありません: {structured}"
        );
        // どのフィールドが不正かは残す。
        assert!(text_of(&result).contains("future"), "{}", text_of(&result));
    }

    #[test]
    fn argument_decoding_failure_does_not_echo_the_value() {
        // 引数の復元に失敗した理由には受け取った値がそのまま現れる。編集 tool の
        // 引数は alias・パス・設定値であり、応答へ反響させない。
        let result = normalize_tool_result(
            "aviutl2_create_object",
            router_argument_error(concat!(
                r#"failed to deserialize parameters: invalid type: string "C:\Users\tester\secret.mp4","#,
                " expected u32 at line 1 column 40",
            )),
        );

        let text = text_of(&result);
        let structured = result
            .structured_content
            .as_ref()
            .expect("structuredContent がある");
        let message = structured["message"].as_str().expect("message がある");
        for forbidden in ["secret", "tester", "Users"] {
            assert!(
                !text.contains(forbidden),
                "{forbidden} が text にあります: {text}"
            );
            assert!(
                !message.contains(forbidden),
                "{forbidden} が message にあります: {message}"
            );
        }
        // どのフィールドが不正かを判断する手掛かりは残す。
        assert!(text.contains("expected u32"), "{text}");
    }

    #[test]
    fn argument_decoding_failure_keeps_the_field_name() {
        // フィールド名はバッククォートで囲まれるため、値を伏せても残る。
        let result = normalize_tool_result(
            "aviutl2_set_object_item",
            router_argument_error("failed to deserialize parameters: missing field `expected`"),
        );
        assert!(
            text_of(&result).contains("expected"),
            "{}",
            text_of(&result)
        );
    }

    #[test]
    fn quoted_values_are_redacted_even_when_they_contain_quotes() {
        // 値の中の引用符でも伏せる範囲が終わらない。終われば続きが漏れる。
        let redacted = redact_quoted_values(r#"invalid type: string "秘\"密", expected u32"#);
        assert!(!redacted.contains('秘'), "{redacted}");
        assert!(!redacted.contains('密'), "{redacted}");
        assert!(redacted.contains("expected u32"), "{redacted}");

        // 閉じない引用符は末尾まで落とす。値が漏れる側へ倒れない。
        let redacted = redact_quoted_values(r#"invalid type: string "秘密"#);
        assert!(!redacted.contains('秘'), "{redacted}");
    }

    #[test]
    fn argument_decoding_failure_text_stays_within_limit() {
        // 拒否の説明にはクライアントが送った key がそのまま現れるため、
        // 巨大な key を送られても text は上限に収まらなければならない。
        let key = "k".repeat(100_000);
        let result = normalize_tool_result(
            "aviutl2_list_instances",
            router_argument_error(format!(
                "failed to deserialize parameters: unknown field `{key}`, expected `offset` or `limit`"
            )),
        );
        let text = text_of(&result);
        assert!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "上限を超えています: {}",
            text.chars().count()
        );
        let structured = result.structured_content.expect("structuredContent がある");
        assert!(
            structured["message"]
                .as_str()
                .is_some_and(|message| message.chars().count() <= MAX_TEXT_CHARS),
            "message が上限を超えています: {structured}"
        );
    }

    #[tokio::test]
    async fn tool_results_pass_through_normalization_unchanged() {
        // tool 本体を経た結果は structuredContent を持つため組み直さない。
        for expected in [
            server()
                .run("test_tool", || {
                    Ok(ToolSuccess {
                        text: "ok".to_string(),
                        structured: serde_json::json!({ "value": 1 }),
                    })
                })
                .await,
            server()
                .run("test_tool", || Err(failure::invalid_argument("範囲外")))
                .await,
        ] {
            let normalized = normalize_tool_result("test_tool", expected.clone());
            assert_eq!(normalized.content, expected.content);
            assert_eq!(normalized.structured_content, expected.structured_content);
            assert_eq!(normalized.is_error, expected.is_error);
        }
    }

    #[test]
    fn error_result_excludes_secrets_and_handles() {
        let remote = aviutl2_mcp_core::ErrorObject::new(ErrorCode::SdkError, "失敗", false)
            .with_details(serde_json::json!({
                "auth_secret": "s3cret",
                "server_nonce": "n0nce",
                "object_handle": 1234,
                "raw_pointer": "0xdeadbeef",
                "pipe_name": r"\\.\pipe\aviutl2-mcp",
                "current_project_revision": 7,
            }));
        let error = failure::with_correlation_id(
            failure::from_pipe_error(&crate::pipe_client::PipeClientError::Remote(Box::new(
                remote,
            ))),
            "correlation",
        );
        let result = error_result(&error);
        let serialized = serde_json::to_string(&result).expect("直列化できる");
        for forbidden in ["s3cret", "n0nce", "0xdeadbeef", "pipe"] {
            assert!(
                !serialized.contains(forbidden),
                "{forbidden} が応答に含まれています: {serialized}"
            );
        }
        let structured = result.structured_content.expect("structuredContent がある");
        assert_eq!(structured["code"], serde_json::json!("sdk_error"));
        assert_eq!(
            structured["details"]["current_project_revision"],
            serde_json::json!(7)
        );
        assert_eq!(
            structured["correlation_id"],
            serde_json::json!("correlation")
        );
    }

    #[test]
    fn default_limits_come_from_the_shared_budget() {
        // 既定値を接続先と共有する配分から外すと、接続先が自身の上限まで使った
        // 段の途中で予算が尽き、応答しているインスタンスが期限超過になる。
        let limits = CallLimits::default();
        assert_eq!(limits.resolve, SERVER_RESOLVE_BUDGET);
        assert_eq!(limits.request, SERVER_READ_REQUEST_BUDGET);
        assert_eq!(limits.edit_request, SERVER_EDIT_REQUEST_BUDGET);
        assert_eq!(
            DiscoveryConfig::default().per_candidate_deadline,
            SERVER_RESOLVE_BUDGET
        );
    }

    #[test]
    fn call_limits_can_be_overridden() {
        let limits = CallLimits {
            resolve: Duration::from_millis(120),
            request: Duration::from_millis(340),
            edit_request: Duration::from_millis(560),
        };
        let server = AviUtl2McpServer::with_limits(PathBuf::from("registry"), limits);
        assert_eq!(server.limits.resolve, Duration::from_millis(120));
        assert_eq!(server.limits.request, Duration::from_millis(340));
        assert_eq!(server.limits.edit_request, Duration::from_millis(560));
    }

    #[test]
    fn request_budget_selects_the_limit_matching_the_operation_kind() {
        let limits = CallLimits {
            resolve: Duration::from_millis(1),
            request: Duration::from_millis(2),
            edit_request: Duration::from_millis(3),
        };

        for name in [
            OPERATION_GET_EDIT_INFO,
            OPERATION_GET_CURRENT_SCENE,
            OPERATION_LIST_LAYERS,
            OPERATION_LIST_OBJECTS,
            OPERATION_GET_OBJECT,
            OPERATION_LIST_AVAILABLE_EFFECTS,
            "ping",
            "future_operation",
        ] {
            assert_eq!(
                limits.request_budget(name),
                limits.request,
                "{name} が read 予算を使っていません"
            );
        }

        for op in aviutl2_mcp_core::EditOperation::ALL {
            assert_eq!(
                limits.request_budget(op.as_str()),
                limits.edit_request,
                "{op:?} が edit 予算を使っていません"
            );
        }
    }

    #[test]
    fn resource_uri_for_instances_is_recognized() {
        assert_eq!(
            parse_resource_uri(INSTANCES_RESOURCE_URI),
            Some(ResourceTarget::Instances)
        );
    }

    #[test]
    fn edit_info_resource_uri_round_trips() {
        let id = InstanceId::new_v4();
        let uri = edit_info_resource_uri(&id);
        assert_eq!(parse_resource_uri(&uri), Some(ResourceTarget::EditInfo(id)));
    }

    #[test]
    fn unknown_resource_uri_is_rejected() {
        for uri in [
            "aviutl2://artifacts/1",
            "aviutl2://instances/not-a-uuid/edit-info",
            "aviutl2://instances//edit-info",
            "file:///etc/passwd",
            "aviutl2://instances/8df98c04-e7c2-4f98-b3ce-fc1c39d76414",
        ] {
            assert_eq!(parse_resource_uri(uri), None, "{uri} を受理しています");
        }
    }

    #[test]
    fn correlation_ids_are_unique_uuids() {
        let first = new_correlation_id();
        let second = new_correlation_id();
        assert_ne!(first, second);
        assert_eq!(first.len(), 36);
    }

    #[test]
    fn instance_not_found_becomes_resource_not_found() {
        let error =
            failure::from_resolve_error(&crate::discovery::ResolveInstanceError::NotRegistered);
        let mcp_error = to_mcp_error(&error);
        assert_eq!(mcp_error.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
    }

    #[test]
    fn stale_instance_becomes_resource_not_found() {
        // 一覧を取り直せば解消し得るため、恒久的な内部エラーにはしない。
        let error = failure::from_resolve_error(&crate::discovery::ResolveInstanceError::Excluded(
            crate::discovery::ExclusionReason::PipeUnreachable,
        ));
        let mcp_error = to_mcp_error(&error);
        assert_eq!(mcp_error.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
    }

    #[test]
    fn invalid_argument_becomes_invalid_params() {
        let mcp_error = to_mcp_error(&failure::invalid_argument("limit が範囲外です"));
        assert_eq!(mcp_error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn transient_failures_become_resource_not_found() {
        // 待てば取得し得る失敗を server の不具合と読ませない。
        for code in [
            ErrorCode::HostBusy,
            ErrorCode::EditBlocked,
            ErrorCode::Timeout,
        ] {
            let error = failure::from_code(code.clone(), "失敗");
            assert!(error.retryable, "{code}");
            let mcp_error = to_mcp_error(&error);
            assert_eq!(
                mcp_error.code,
                rmcp::model::ErrorCode::RESOURCE_NOT_FOUND,
                "{code}"
            );
            assert_eq!(
                mcp_error.data.expect("data がある")["retryable"],
                serde_json::json!(true),
                "{code}"
            );
        }
    }

    #[test]
    fn other_errors_become_internal_error() {
        for code in [
            ErrorCode::SdkError,
            ErrorCode::InternalError,
            ErrorCode::UnsupportedOperation,
            ErrorCode::AuthenticationFailed,
        ] {
            let mcp_error = to_mcp_error(&failure::from_code(code.clone(), "失敗"));
            assert_eq!(
                mcp_error.code,
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                "{code}"
            );
        }
    }

    #[test]
    fn mcp_error_carries_structured_details() {
        let remote =
            aviutl2_mcp_core::ErrorObject::new(ErrorCode::HostBusy, "起動処理中です", true)
                .with_details(serde_json::json!({ "retry_after_ms": 500 }));
        let error = failure::with_correlation_id(
            failure::from_pipe_error(&crate::pipe_client::PipeClientError::Remote(Box::new(
                remote,
            ))),
            "correlation",
        );
        let data = to_mcp_error(&error).data.expect("data がある");
        assert_eq!(data["code"], serde_json::json!("host_busy"));
        assert_eq!(data["retryable"], serde_json::json!(true));
        assert_eq!(data["details"]["retry_after_ms"], serde_json::json!(500));
        assert_eq!(data["correlation_id"], serde_json::json!("correlation"));
    }

    #[test]
    fn cursor_round_trips() {
        assert_eq!(decode_cursor(None).expect("未指定は先頭"), 0);
        assert_eq!(
            decode_cursor(Some(&encode_cursor(100))).expect("符号化した位置を戻せる"),
            100
        );
    }

    #[test]
    fn malformed_cursor_is_rejected() {
        for cursor in ["", "-1", "abc", "1.5"] {
            let error = decode_cursor(Some(cursor)).expect_err("解釈できない cursor は拒否する");
            assert_eq!(
                error.code,
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "{cursor}"
            );
        }
    }

    /// 表示名が長いインスタンスを指定件数だけ並べた一覧。
    fn oversized_instances(count: usize) -> ListInstancesResponse {
        let instances: Vec<aviutl2_mcp_core::InstanceInfo> = (0..count)
            .map(|_| aviutl2_mcp_core::InstanceInfo {
                instance_id: InstanceId::new_v4(),
                state: aviutl2_mcp_core::InstanceState::Ready,
                pid: 1234,
                started_at: "2026-01-01T00:00:00.0000000Z".to_string(),
                project: Some(aviutl2_mcp_core::InstanceProject {
                    display_name: Some("名".repeat(500)),
                    path: None,
                    epoch: None,
                    revision: None,
                    modified: None,
                }),
                scene: None,
            })
            .collect();
        ListInstancesResponse {
            total_count: instances.len() as u32,
            count: instances.len() as u32,
            instances,
            offset: 0,
            has_more: false,
            next_offset: None,
        }
    }

    #[test]
    fn instances_resource_shrinks_until_it_fits() {
        let response = oversized_instances(MAX_PAGE_LIMIT as usize);
        let value = fitted_instances_value(response).expect("値へ変換できる");
        let text = pretty_json(&value).expect("直列化できる");
        assert!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "上限を超えています: {}",
            text.chars().count()
        );
        // 落とした分は続きとして示され、黙って欠落しない。
        assert_eq!(value["has_more"], serde_json::json!(true));
        assert!(value["next_offset"].is_number());
        assert!(
            value["count"].as_u64().expect("count は数値") < MAX_PAGE_LIMIT as u64,
            "件数が絞られていません"
        );
    }

    #[test]
    fn small_instances_resource_is_not_shrunk() {
        let response = oversized_instances(1);
        let value = fitted_instances_value(response).expect("値へ変換できる");
        assert_eq!(value["count"], serde_json::json!(1));
        assert_eq!(value["has_more"], serde_json::json!(false));
    }

    #[test]
    fn resource_text_stays_within_limit() {
        let value = serde_json::json!({ "note": "え".repeat(MAX_TEXT_CHARS * 2) });
        let text = resource_text(&value).expect("代替内容を返せる");
        assert!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "上限を超えています: {}",
            text.chars().count()
        );
        // 途中で切らず、読み取れる JSON のまま超過を伝える。
        let decoded: Value = serde_json::from_str(&text).expect("JSON として読める");
        assert_eq!(decoded["truncated"], serde_json::json!(true));
    }

    #[test]
    fn resource_text_keeps_content_within_limit_intact() {
        let value = serde_json::json!({ "note": "短い" });
        let text = resource_text(&value).expect("直列化できる");
        let decoded: Value = serde_json::from_str(&text).expect("JSON として読める");
        assert_eq!(decoded, value);
    }
}
