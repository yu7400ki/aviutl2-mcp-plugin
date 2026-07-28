//! MCP stdio サーバーの本体。
//!
//! tool call は 1 回ごとに接続を確立し、応答を受け取ったら破棄する。
//! [`crate::pipe_client::PipeClient`] は生のハンドルを持ち `!Send` であるため、
//! インスタンス解決から要求送信・切断までを 1 つのブロッキングタスクへ閉じ込め、
//! 非同期タスク間で接続が移動しないようにする。

use crate::api::{ListInstancesResponse, aviutl2_list_instances};
use crate::discovery::{DiscoveryConfig, list_registered_instances, resolve_instance};
use crate::mcp::input::{
    GetObjectInput, InstanceInput, ListAvailableEffectsInput, ListInstancesInput, ListLayersInput,
    ListObjectsInput, parse_instance_id,
};
use crate::mcp::summary::{MAX_TEXT_CHARS, clamp_chars};
use crate::mcp::{describe, failure};
use crate::redact;
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
                let mut result = CallToolResult::success(vec![text_content(&success.text)]);
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
            resources.push(
                Resource::new(
                    edit_info_resource_uri(instance_id),
                    format!("aviutl2 edit info {}", redact::instance_id(instance_id)),
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
        instance = %redact::instance_id(&instance_id),
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

/// DTO を `structuredContent` へ載せる値へ変換する。
fn to_structured<T: Serialize>(value: &T) -> Result<Value, ErrorObject> {
    serde_json::to_value(value).map_err(|_| failure::internal_error("応答を直列化できませんでした"))
}

/// text content を上限内へ収めて 1 ブロックにする。
///
/// [`describe`] は予算を管理して組み立てるが、tool を足すときに素の文字列を
/// 渡しても上限を破れないよう、応答を作る唯一の経路をここへ通す。
fn text_content(text: &str) -> ContentBlock {
    ContentBlock::text(clamp_chars(text, MAX_TEXT_CHARS))
}

/// エラーを `isError: true` の tool result へ変換する。
///
/// `structuredContent` は宣言済みの `outputSchema`（成功時の result DTO）には
/// 適合しない。MCP は失敗を tool result で表す経路に別 schema を持たず、
/// 呼び出し側が機械的に扱えるのは code / retryable / details / correlation_id で
/// あるため、成功時の形に寄せるより失敗の内訳を残す方を採る。
fn error_result(error: &ErrorObject) -> CallToolResult {
    let mut result = CallToolResult::error(vec![text_content(&failure::text(error))]);
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
    ];

    /// 現在登録されている全 tool の一覧。読み取り専用の read tool のみで構成される。
    const READ_TOOLS: &[&str] = &[
        "aviutl2_list_instances",
        "aviutl2_get_edit_info",
        "aviutl2_get_current_scene",
        "aviutl2_list_layers",
        "aviutl2_list_objects",
        "aviutl2_get_object",
        "aviutl2_list_available_effects",
    ];

    fn server() -> AviUtl2McpServer {
        AviUtl2McpServer::new(PathBuf::from(r"C:\nonexistent-registry"))
    }

    fn tools() -> Vec<Tool> {
        server().tools()
    }

    #[test]
    fn read_tools_are_registered() {
        let names: std::collections::BTreeSet<String> =
            tools().iter().map(|tool| tool.name.to_string()).collect();
        let expected: std::collections::BTreeSet<String> =
            READ_TOOLS.iter().map(|name| name.to_string()).collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn read_tools_are_annotated_as_read_only() {
        for tool in tools() {
            let annotations = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{} に annotation がありません", tool.name));
            assert_eq!(annotations.read_only_hint, Some(true), "{}", tool.name);
            assert_eq!(annotations.destructive_hint, Some(false), "{}", tool.name);
            assert_eq!(annotations.idempotent_hint, Some(true), "{}", tool.name);
            assert_eq!(annotations.open_world_hint, Some(false), "{}", tool.name);
        }
    }

    #[test]
    fn read_tools_declare_output_schema() {
        for tool in tools() {
            let schema = tool
                .output_schema
                .as_ref()
                .unwrap_or_else(|| panic!("{} に outputSchema がありません", tool.name));
            assert_eq!(schema["type"], serde_json::json!("object"), "{}", tool.name);
            assert!(schema.contains_key("properties"), "{}", tool.name);
        }
    }

    #[test]
    fn read_tool_descriptions_state_zero_based_numbering() {
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
        let text = result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone())
            .expect("text content がある");
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
        let result = error_result(&error);
        let text = result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone())
            .expect("text content がある");
        assert!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "上限を超えています: {}",
            text.chars().count()
        );
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
                    display_name: "名".repeat(500),
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
