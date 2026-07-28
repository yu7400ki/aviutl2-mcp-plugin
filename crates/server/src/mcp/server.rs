//! MCP stdio サーバーの本体。
//!
//! tool call は 1 回ごとに接続を確立し、応答を受け取ったら破棄する。
//! [`crate::pipe_client::PipeClient`] は生のハンドルを持ち `!Send` であるため、
//! インスタンス解決から要求送信・切断までを 1 つのブロッキングタスクへ閉じ込め、
//! 非同期タスク間で接続が移動しないようにする。

use crate::api::{ListInstancesResponse, aviutl2_list_instances};
use crate::discovery::{DiscoveryConfig, resolve_instance};
use crate::mcp::input::{
    GetObjectInput, InstanceInput, ListAvailableEffectsInput, ListInstancesInput, ListLayersInput,
    ListObjectsInput, parse_instance_id,
};
use crate::mcp::summary::clamp_chars;
use crate::mcp::{describe, failure};
use aviutl2_mcp_core::{
    EditInfo, ErrorCode, ErrorObject, GetCurrentSceneParams, GetCurrentSceneResult,
    GetEditInfoParams, InstanceId, ListAvailableEffectsResult, ListLayersResult, ListObjectsResult,
    MAX_PAGE_LIMIT, OPERATION_GET_CURRENT_SCENE, OPERATION_GET_EDIT_INFO, OPERATION_GET_OBJECT,
    OPERATION_LIST_AVAILABLE_EFFECTS, OPERATION_LIST_LAYERS, OPERATION_LIST_OBJECTS, ObjectDetail,
};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents, ServerCapabilities,
    ServerInfo,
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

/// 匿名化した instance_id に残す先頭文字数。
const ANONYMIZED_ID_CHARS: usize = 8;

/// tool call 1 回分の実行予算。
///
/// 要求が運ぶ期限とサーバー上限の短い方が採用されるため、ここではサーバー上限を持つ。
#[derive(Debug, Clone, Copy)]
pub struct CallLimits {
    /// インスタンス解決（接続・handshake・ping）の期限。
    pub resolve: Duration,
    /// read operation 1 件の期限。
    pub request: Duration,
}

impl Default for CallLimits {
    fn default() -> Self {
        Self {
            resolve: Duration::from_secs(5),
            request: Duration::from_secs(5),
        }
    }
}

/// AviUtl2 の読み取りを提供する MCP サーバー。
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
            let result: EditInfo = request_read(
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
            let result: GetCurrentSceneResult = request_read(
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
    /// 2 ページ目以降は先頭ページが返した snapshot_revision を指定する。
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
            let result: ListLayersResult = request_read(
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
            let result: ListObjectsResult = request_read(
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
            let result: ObjectDetail = request_read(
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
    /// frame 番号と layer 番号はいずれも 0 始まりである。
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
            let result: ListAvailableEffectsResult = request_read(
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
            "AviUtl2 の編集内容を読み取る。aviutl2_list_instances 以外の tool は instance_id が必須である。frame 番号と layer 番号はいずれも 0 始まりである。"
                .to_string(),
        );
        info
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let registry_dir = self.registry_dir();
        let response = self
            .run_resource("resources/list", move || {
                list_instances(
                    &registry_dir,
                    ListInstancesInput {
                        offset: 0,
                        limit: MAX_PAGE_LIMIT,
                    },
                )
            })
            .await?;

        let mut resources = vec![
            Resource::new(INSTANCES_RESOURCE_URI, "aviutl2 instances")
                .with_description("生存確認済みの AviUtl2 インスタンス一覧")
                .with_mime_type(RESOURCE_MIME_TYPE),
        ];
        for info in &response.instances {
            resources.push(
                Resource::new(
                    edit_info_resource_uri(&info.instance_id),
                    format!("aviutl2 edit info {}", info.instance_id),
                )
                .with_description("インスタンスの現在の編集情報")
                .with_mime_type(RESOURCE_MIME_TYPE),
            );
        }
        Ok(ListResourcesResult::with_all_items(resources))
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
                        to_structured(&response)?
                    }
                    ResourceTarget::EditInfo(instance_id) => {
                        let info: EditInfo = request_read(
                            &registry_dir,
                            instance_id,
                            limits,
                            OPERATION_GET_EDIT_INFO,
                            &GetEditInfoParams {},
                        )?;
                        to_structured(&info)?
                    }
                };
                let text = serde_json::to_string_pretty(&value)
                    .map_err(|_| failure::internal_error("resource を直列化できませんでした"))?;
                Ok(
                    ResourceContents::text(text, uri_for_content)
                        .with_mime_type(RESOURCE_MIME_TYPE),
                )
            })
            .await?;

        Ok(ReadResourceResult::new(vec![contents]))
    }
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

/// 対象インスタンスへ read operation を 1 件送り、結果を型付きで受け取る。
///
/// 接続は本関数の中で確立し、応答を受け取ったところで破棄する。フレーム境界を
/// 見失った接続を持ち越さないため、接続の再利用は行わない。
fn request_read<P, R>(
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

    let deadline = Instant::now() + limits.request;
    tracing::debug!(
        instance = %anonymized_instance_id(&instance_id),
        operation,
        "sending read request",
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

/// ログへ出す instance_id の匿名化表現。
fn anonymized_instance_id(instance_id: &InstanceId) -> String {
    clamp_chars(&instance_id.to_string(), ANONYMIZED_ID_CHARS)
}

/// DTO を `structuredContent` へ載せる値へ変換する。
fn to_structured<T: Serialize>(value: &T) -> Result<Value, ErrorObject> {
    serde_json::to_value(value).map_err(|_| failure::internal_error("応答を直列化できませんでした"))
}

/// エラーを `isError: true` の tool result へ変換する。
fn error_result(error: &ErrorObject) -> CallToolResult {
    let mut result = CallToolResult::error(vec![ContentBlock::text(failure::text(error))]);
    result.structured_content = Some(failure::structured(error));
    result
}

/// エラーを resource 応答用の protocol error へ変換する。
fn to_mcp_error(error: &ErrorObject) -> McpError {
    let message = failure::text(error);
    match error.code {
        ErrorCode::InstanceNotFound => McpError::resource_not_found(message, None),
        ErrorCode::InvalidArgument => McpError::invalid_params(message, None),
        _ => McpError::internal_error(message, None),
    }
}
