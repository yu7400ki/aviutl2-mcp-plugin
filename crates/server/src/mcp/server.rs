//! MCP stdio サーバーの本体。
//!
//! tool call は 1 回ごとに接続を確立し、応答を受け取ったら破棄する。
//! [`crate::pipe_client::PipeClient`] は生のハンドルを持ち `!Send` であるため、
//! インスタンス解決から要求送信・切断までを 1 つのブロッキングタスクへ閉じ込め、
//! 非同期タスク間で接続が移動しないようにする。

use crate::api::{ListInstancesResponse, aviutl2_list_instances};
use crate::artifact::{Artifact, ArtifactStore, ArtifactStoreError, base_dir_for_registry};
use crate::discovery::{DiscoveryConfig, list_registered_instances, resolve_instance};
use crate::mcp::edit_input::{
    AddEffectInput, ApplyBatchInput, CreateObjectInput, DeleteEffectInput, DeleteObjectInput,
    MoveObjectInput, SetEffectEnabledInput, SetLayerStateInput, SetObjectItemInput,
    SetObjectNameInput, SetSelectionInput,
};
use crate::mcp::input::{
    GetObjectInput, InstanceInput, ListAvailableEffectsInput, ListInstancesInput, ListLayersInput,
    ListObjectsInput, parse_instance_id,
};
use crate::mcp::render::{RenderFrameInput, RenderFrameOutput};
use crate::mcp::summary::{MAX_TEXT_CHARS, clamp_chars};
use crate::mcp::tool_catalog::{ToolListWatch, ToolVisibility};
use crate::mcp::{describe, failure};
use crate::redact;
use crate::settings::SettingsSource;
use aviutl2_mcp_core::{
    BatchOutcome, EditInfo, EditOutcome, ErrorCode, ErrorObject, GetCurrentSceneParams,
    GetCurrentSceneResult, GetEditInfoParams, InstanceId, LayerStateOutcome,
    ListAvailableEffectsResult, ListLayersResult, ListObjectsResult, MAX_PAGE_LIMIT,
    OPERATION_ADD_EFFECT, OPERATION_APPLY_BATCH, OPERATION_CREATE_OBJECT, OPERATION_DELETE_EFFECT,
    OPERATION_DELETE_OBJECT, OPERATION_GET_CURRENT_SCENE, OPERATION_GET_EDIT_INFO,
    OPERATION_GET_OBJECT, OPERATION_LIST_AVAILABLE_EFFECTS, OPERATION_LIST_LAYERS,
    OPERATION_LIST_OBJECTS, OPERATION_MOVE_OBJECT, OPERATION_RENDER_FRAME,
    OPERATION_SET_EFFECT_ENABLED, OPERATION_SET_LAYER_STATE, OPERATION_SET_OBJECT_ITEM,
    OPERATION_SET_OBJECT_NAME, OPERATION_SET_SELECTION, ObjectDetail, RenderFrameResult,
    RequestBudgetKind, ScaledBudgets, SelectionState, request_budget_kind,
};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, Implementation, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult,
    Resource, ResourceContents, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, tool, tool_router};
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

/// 描画成果物の resource URI の接頭辞。
pub const ARTIFACTS_RESOURCE_URI_PREFIX: &str = "aviutl2://artifacts/";

/// resource の内容に用いる MIME type。
const RESOURCE_MIME_TYPE: &str = "application/json";

/// tool call 1 回分の実行予算。
///
/// 要求が運ぶ期限とサーバー上限の短い方が採用されるため、ここではサーバー上限を持つ。
/// 既定値は接続先と共有する配分から取る。接続先は自身の各段の上限をこの予算の
/// 内側に収めるため、既定値を延ばす分には安全だが、縮めると接続先が上限まで
/// 使った段の途中で予算が尽きる。
///
/// 要求フェーズの予算は operation の区分ごとに異なる。どれを使うかは
/// [`ipc_request_budget`](CallLimits::ipc_request_budget) が operation 名から選ぶ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallLimits {
    /// インスタンス解決（接続・handshake・ping）の期限。
    pub resolve: Duration,
    /// read operation 1 件の期限。
    pub request: Duration,
    /// 編集 operation 1 件の期限。
    pub edit_request: Duration,
    /// 一括適用 1 件の期限。
    pub batch_request: Duration,
    /// render operation 1 件の期限。
    pub render_request: Duration,
    /// 描画成果物の引き取りの期限。
    ///
    /// 引き取りは応答を受けてから始まり、[`render_request`](Self::render_request)
    /// の内側で起きる。したがって render の要求へ載せる期限は、
    /// `render_request` から本値を差し引いた残りから算出する。
    pub artifact_ingest: Duration,
}

impl Default for CallLimits {
    fn default() -> Self {
        Self::from_budgets(ScaledBudgets::unscaled())
    }
}

impl CallLimits {
    /// 倍率を適用済みの予算一式から引く。
    ///
    /// **server 側で範囲を判定し直さない。** 倍率の採否は core が不等式ごと
    /// 決めており、判定が 2 か所にあると plugin と server が同じファイルから
    /// 別の結論を得る形ができる。
    pub fn from_budgets(budgets: ScaledBudgets) -> Self {
        Self {
            resolve: budgets.server_resolve(),
            request: budgets.server_request_phase(RequestBudgetKind::Read),
            edit_request: budgets.server_request_phase(RequestBudgetKind::Edit),
            batch_request: budgets.server_request_phase(RequestBudgetKind::Batch),
            render_request: budgets.server_request_phase(RequestBudgetKind::Render),
            artifact_ingest: budgets.server_artifact_ingest(),
        }
    }
}

impl CallLimits {
    /// operation 名に応じた要求フェーズ全体の期限を返す。
    ///
    /// 区分の判定は core の選択規則（[`request_budget_kind`]）に委ねる。判定
    /// 基準を server が独自に持たないことで、片方だけ取り違えたときに検出
    /// できない状態を避ける。
    ///
    /// **これは IPC の要求へ載せる値ではない。** 応答を受け取ったあとに走る段を
    /// 持つ operation では、その取り分だけ短い
    /// [`ipc_request_budget`](Self::ipc_request_budget) を渡す。
    pub fn request_phase_budget(&self, operation: &str) -> Duration {
        self.phase_budget(request_budget_kind(operation))
    }

    /// IPC の要求 1 件へ載せる期限の長さを返す。
    ///
    /// 要求フェーズ全体の予算から、応答を受け取ったあとに同じフェーズの内側で
    /// 走る段の取り分を差し引く。**差し引きを呼び出し側の責務にしない。**
    /// 期限を組み立てる経路をこの 1 つに絞ることで、応答後の段を持つ operation
    /// が要求フェーズの予算をそのまま渡す形を作れないようにする。差し引きを
    /// 忘れると、接続先が期限いっぱいまで使った直後に応答後の段が始まり、
    /// どの層の期限にも捕まらないまま予算を超える。
    pub fn ipc_request_budget(&self, operation: &str) -> Duration {
        let kind = request_budget_kind(operation);
        let phase = self.phase_budget(kind);
        let reserve = self.post_response_reserve(kind);
        // 取り分が予算を上回ると、飽和した引き算は 0 を返して期限が「今」になり、
        // 要求が必ず期限超過で返る。既定値は core が不等式ごと固定しているが、
        // [`CallLimits`] のフィールドは公開されており任意の組を作れる。
        debug_assert!(
            reserve < phase,
            "応答後の取り分が要求フェーズの予算以上です: {reserve:?} >= {phase:?}"
        );
        phase.saturating_sub(reserve)
    }

    /// 区分ごとの要求フェーズ全体の期限。
    ///
    /// **`match` に `_` を使わない。** 既定の腕を置くと、区分が増えたときに
    /// 新しい operation が黙って別の予算で走る。
    fn phase_budget(&self, kind: RequestBudgetKind) -> Duration {
        match kind {
            RequestBudgetKind::Read => self.request,
            RequestBudgetKind::Edit => self.edit_request,
            RequestBudgetKind::Batch => self.batch_request,
            RequestBudgetKind::Render => self.render_request,
        }
    }

    /// 応答を受け取ったあと、同じ要求フェーズの内側で走る段の取り分。
    ///
    /// 描画だけがこの段を持つ。応答が運ぶ識別子で成果物を引き取り、読み込み・
    /// ダイジェストの算出・保存・引き渡し元の削除までを行う。他の operation は
    /// 応答を変換して返すだけであり、取り分を持たない。
    ///
    /// ここも `_` を使わない。区分が増えたときに、取り分の要否を判断しないまま
    /// 「無し」へ落ちることを防ぐ。
    fn post_response_reserve(&self, kind: RequestBudgetKind) -> Duration {
        match kind {
            RequestBudgetKind::Read | RequestBudgetKind::Edit | RequestBudgetKind::Batch => {
                Duration::ZERO
            }
            RequestBudgetKind::Render => self.artifact_ingest,
        }
    }
}

/// AviUtl2 の読み取りと編集を提供する MCP サーバー。
#[derive(Clone)]
pub struct AviUtl2McpServer {
    registry_dir: Arc<PathBuf>,
    /// 描画成果物の保管庫。
    ///
    /// 開いていない場合、描画と成果物 resource は使えない。保管庫は
    /// registry から導いた基底の下へディレクトリを作り、そこへ保護された DACL を
    /// 設定するため、**開くかどうかを利用側が決められる形にしてある。**
    artifacts: Option<Arc<ArtifactStore>>,
    limits: LimitsSource,
    tool_router: ToolRouter<Self>,
}

/// 実行予算と tool の公開の出所。
///
/// 設定を持つ場合は tool call のたびに現在の snapshot から引く。設定を持たない
/// 構築口は固定値を使う——予算を明示して振る舞いを観測する試験と実機受け入れの
/// ためであり、**製品の経路では使わない。** その場合、tool の公開は既定
/// （全 tool 有効）になる。
#[derive(Clone)]
enum LimitsSource {
    /// 構築時に与えられた固定値。
    ///
    /// この腕を作れるのは試験と実機受け入れの構築口だけであり、それらは
    /// `test-support` の下にある。**製品ビルドでは構築されない。**
    #[cfg_attr(not(any(test, feature = "test-support")), expect(dead_code))]
    Fixed(CallLimits),
    /// 共有設定から引く。
    Settings(Arc<SettingsSource>),
}

impl LimitsSource {
    fn limits(&self) -> CallLimits {
        match self {
            Self::Fixed(limits) => *limits,
            Self::Settings(source) => CallLimits::from_budgets(source.settings().budgets()),
        }
    }

    /// 現在公開する tool の判定。
    fn visibility(&self) -> ToolVisibility {
        match self {
            Self::Fixed(_) => ToolVisibility::all_enabled(),
            Self::Settings(source) => ToolVisibility::from_settings(&source.settings()),
        }
    }

    /// 共有設定の供給元。固定値の構築口は持たない。
    fn shared(&self) -> Option<Arc<SettingsSource>> {
        match self {
            Self::Fixed(_) => None,
            Self::Settings(source) => Some(Arc::clone(source)),
        }
    }
}

impl std::fmt::Debug for AviUtl2McpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AviUtl2McpServer")
            .field("registry_dir", &self.registry_dir)
            .field("limits", &self.limits())
            .field("artifacts", &self.artifacts.is_some())
            .finish_non_exhaustive()
    }
}

/// 成功した tool call の応答内容。
struct ToolSuccess {
    text: String,
    structured: Value,
}

impl AviUtl2McpServer {
    /// registry ディレクトリと設定の供給元を指定してサーバーを作る。
    ///
    /// 描画成果物の保管庫を開く。保管庫は registry と同じ基底の下に作られ、
    /// **このサーバーが破棄されるときにディレクトリごと消える**。成果物は
    /// このプロセスだけが読むものであり、プロセスの終了後に残す理由が無い。
    ///
    /// 実行予算も保管庫の上限も、要求のたびに供給元の現在値から引く。
    pub fn new(
        registry_dir: PathBuf,
        settings: Arc<SettingsSource>,
    ) -> Result<Self, ArtifactStoreError> {
        let store =
            ArtifactStore::open(base_dir_for_registry(&registry_dir), Arc::clone(&settings))?;
        Ok(Self {
            artifacts: Some(Arc::new(store)),
            ..Self::from_limits_source(registry_dir, LimitsSource::Settings(settings))
        })
    }

    /// 開いてある保管庫と固定の実行予算でサーバーを作る。
    ///
    /// **予算を明示して振る舞いを観測するための構築口であり、製品の経路では
    /// 使わない。** 既定では公開しないため、`.exe` にこの経路は無い。
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_artifact_store(
        registry_dir: PathBuf,
        limits: CallLimits,
        artifacts: Arc<ArtifactStore>,
    ) -> Self {
        Self {
            artifacts: Some(artifacts),
            ..Self::without_artifact_store(registry_dir, limits)
        }
    }

    /// 保管庫を持たないサーバーを作る。
    ///
    /// **`aviutl2_render_frame` は使えない。** 呼ぶと成果物を保管できないため
    /// `internal_error` になる（接続先へは要求を送らない）。成果物 resource も
    /// 1 件も並ばない。
    ///
    /// 保管庫は基底へ保護された DACL を書き込むため、描画を使わない利用者に
    /// それを強いないための構築口である。描画を提供する場合は [`Self::new`] を
    /// 使う。
    ///
    /// **予算を明示して振る舞いを観測するための構築口であり、製品の経路では
    /// 使わない。** 既定では公開しないため、`.exe` にこの経路は無い。
    #[cfg(any(test, feature = "test-support"))]
    pub fn without_artifact_store(registry_dir: PathBuf, limits: CallLimits) -> Self {
        Self::from_limits_source(registry_dir, LimitsSource::Fixed(limits))
    }

    /// 保管庫を持たず、共有設定から予算と tool の公開を引くサーバーを作る。
    ///
    /// **保管庫の用意を伴わずに設定の効き方を観測するための構築口であり、製品の
    /// 経路では使わない。** 既定では公開しないため、`.exe` にこの経路は無い。
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_settings(registry_dir: PathBuf, settings: Arc<SettingsSource>) -> Self {
        Self::from_limits_source(registry_dir, LimitsSource::Settings(settings))
    }

    fn from_limits_source(registry_dir: PathBuf, limits: LimitsSource) -> Self {
        Self {
            registry_dir: Arc::new(registry_dir),
            artifacts: None,
            limits,
            tool_router: Self::tool_router(),
        }
    }

    /// tool call 1 回分の実行予算。
    fn limits(&self) -> CallLimits {
        self.limits.limits()
    }

    /// 登録済みの tool 定義を返す。
    ///
    /// **公開の判定を通さない全体である。** 現在公開している一覧は
    /// [`ServerHandler::list_tools`] が返す。
    pub fn tools(&self) -> Vec<Tool> {
        self.tool_router.list_all()
    }

    /// 現在公開する tool の判定。
    ///
    /// **`tools/list` の filtering も call-time の受付判定もここだけを読む。**
    /// 設定は 1 回の `Arc` の差し替えで反映されるため、半分だけ適用された状態を
    /// 観測する経路が無い。
    fn tool_visibility(&self) -> ToolVisibility {
        self.limits.visibility()
    }

    /// 現在公開している tool 定義を返す。
    ///
    /// `tools/list` が返すのはこの一覧である。tool の定義そのものは router が
    /// 持つものをそのまま使う——説明と schema の出所を 2 つにしない。
    pub fn visible_tools(&self) -> Vec<Tool> {
        let visibility = self.tool_visibility();
        self.tool_router
            .list_all()
            .into_iter()
            .filter(|tool| visibility.allows(&tool.name))
            .collect()
    }

    /// tool の call を受け付けるか。
    ///
    /// [`Self::visible_tools`] と同じ判定を読む。掲載しない tool の call を
    /// 受け付ける、あるいはその逆になる余地が無い。
    pub fn accepts_tool_call(&self, name: &str) -> bool {
        self.tool_visibility().allows(name)
    }

    /// 公開していない tool の call を拒否する。
    ///
    /// **接続先へは何も送らない。** 判定は要求を受けた直後に行い、インスタンスの
    /// 解決にも handshake にも進まない。
    fn reject_disabled_tool(&self, tool: &str) -> CallToolResult {
        let correlation_id = new_correlation_id();
        tracing::warn!(
            component = "mcp",
            operation = tool,
            correlation_id = %correlation_id,
            result = "tool_disabled",
            "tool call rejected by settings",
        );
        let error = failure::with_correlation_id(
            failure::from_code(
                ErrorCode::ToolDisabled,
                "この tool はプラグイン設定で無効化されています",
            ),
            &correlation_id,
        );
        error_result(&error)
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
        let limits = self.limits();
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
        let limits = self.limits();
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
        let limits = self.limits();
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
        let limits = self.limits();
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
    /// effect の locked は出力項目（標準描画等）については実態を反映せず、
    /// 常に false になる。ロックは入力項目と出力項目をまとめた単位で掛かる。
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
        let limits = self.limits();
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
        let limits = self.limits();
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
    /// expected_project_epoch には直前の読み取りまたは編集の応答が返した
    /// project_epoch をそのまま指定する。省略はできない。作成は対象を指す selector を
    /// 持たないため、これがプロジェクト境界を照合する唯一の材料である。
    /// 要求は project_revision を運ばない。読み取りから作成までに revision が進んで
    /// いても拒否されない。
    /// 応答が返した selector は読み直さずにそのまま次の編集へ渡せる。
    /// 複数オブジェクトを含む alias は全てが作成され、created に全件、object に
    /// その先頭が入る。長さと挿入位置はホストが自動調整し得るため、
    /// 応答が返す位置は要求した宛先と異なり得る。
    /// 応答が返す selector が実際の配置であり、配置を確かめるには応答の値を見る。
    /// 同じ要求を再送すると重複して作成し得る。作成先に既存オブジェクトがあれば
    /// precondition_failed（destination_occupied）となるため通常は防がれるが、
    /// ホストが挿入位置を自動調整した場合はすり抜け得る。
    /// 配置先のレイヤーがロックされている場合は precondition_failed（layer_locked）と
    /// なる。aviutl2_set_layer_state でロックを解除してから再実行する。
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
        let limits = self.limits();
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
    /// プロジェクトの世代は selector が運ぶ project_epoch で照合する。要求は
    /// project_revision を運ばない。読み取りから編集までに revision が進んでいても
    /// 拒否されない。対象が変化していれば fingerprint が、別のプロジェクトであれば
    /// selector の project_epoch が拒否する。
    /// selector には応答が返した値をそのまま指定する。応答が返した selector は
    /// 読み直さずにそのまま次の編集へ渡せる。
    /// 配置はホストが調整し得るため、応答が返す位置は要求した宛先と異なり得る。
    /// 応答が返す selector が実際の配置であり、配置を確かめるには応答の値を見る。
    /// 宛先に既存オブジェクトがある場合は precondition_failed となる。
    /// 移動元または移動先のレイヤーがロックされている場合は
    /// precondition_failed（layer_locked）となる。aviutl2_set_layer_state で
    /// ロックを解除してから再実行する。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// 対象が変化していた場合の precondition_failed では、details.current_object に
    /// 対象の現在の値が入る。読み直さずにそのまま次の要求の selector として使える。
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
        let limits = self.limits();
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
    /// プロジェクトの世代は selector が運ぶ project_epoch で照合する。要求は
    /// project_revision を運ばない。読み取りから編集までに revision が進んでいても
    /// 拒否されない。対象が変化していれば fingerprint が、別のプロジェクトであれば
    /// selector の project_epoch が拒否する。
    /// selector には応答が返した値をそのまま指定する。応答が返した selector は
    /// 読み直さずにそのまま次の編集へ渡せる。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// 対象が変化していた場合の precondition_failed では、details.current_object に
    /// 対象の現在の値が入る。読み直さずにそのまま次の要求の selector として使える。
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
        let limits = self.limits();
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
    /// プロジェクトの世代は selector が運ぶ project_epoch で照合する。要求は
    /// project_revision を運ばない。読み取りから編集までに revision が進んでいても
    /// 拒否されない。対象が変化していれば fingerprint が、別のプロジェクトであれば
    /// selector の project_epoch が拒否する。
    /// selector には aviutl2_get_object が返した effect の selector をそのまま指定する。
    /// 応答が返した selector は読み直さずにそのまま次の編集へ渡せる。
    /// effect の設定を変えるとそのオブジェクトの fingerprint も変わるため、変更前の
    /// selector で続けて編集すると precondition_failed となる。
    /// 書き込みを公開していない設定項目種別があり、その場合は unsupported_operation
    /// となる。種別は aviutl2_get_object の item_type で確認できる。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// 対象が変化していた場合の precondition_failed では、details.current_object に
    /// 対象の現在の値が入る。読み直さずにそのまま次の要求の selector として使える。
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
        let limits = self.limits();
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
    /// プロジェクトの世代は selector が運ぶ project_epoch で照合する。要求は
    /// project_revision を運ばない。読み取りから編集までに revision が進んでいても
    /// 拒否されない。対象が変化していれば fingerprint が、別のプロジェクトであれば
    /// selector の project_epoch が拒否する。
    /// 応答が返した selector は読み直さずにそのまま次の編集へ渡せる。
    /// effect を足すとそのオブジェクトの fingerprint も変わるため、変更前の
    /// selector で続けて編集すると precondition_failed となる。
    /// 同じ要求を再送すると重複して付与し得る。付与によってオブジェクトの
    /// fingerprint が変わるため、同じ selector での再送は precondition_failed と
    /// なり防がれる。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// 対象が変化していた場合の precondition_failed では、details.current_object に
    /// 対象の現在の値が入る。読み直さずにそのまま次の要求の selector として使える。
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
        let limits = self.limits();
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

    /// effect の有効・無効を変更する。
    /// 出力 item の有効・無効は変更できず unsupported_operation となる。
    /// frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。
    /// プロジェクトの世代は selector が運ぶ project_epoch で照合する。要求は
    /// project_revision を運ばない。読み取りから編集までに revision が進んでいても
    /// 拒否されない。対象が変化していれば fingerprint が、別のプロジェクトであれば
    /// selector の project_epoch が拒否する。
    /// selector には aviutl2_get_object が返した effect の selector をそのまま指定する。
    /// 応答が返した selector は読み直さずにそのまま次の編集へ渡せる。
    /// effect の状態を変えるとそのオブジェクトの fingerprint も変わるため、変更前の
    /// selector で続けて編集すると precondition_failed となる。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// 対象が変化していた場合の precondition_failed では、details.current_object に
    /// 対象の現在の値が入る。読み直さずにそのまま次の要求の selector として使える。
    /// この呼び出し 1 回が 1 つの取り消し単位になる。
    #[tool(
        name = "aviutl2_set_effect_enabled",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_effect_enabled()
        )
    )]
    pub async fn aviutl2_set_effect_enabled(
        &self,
        Parameters(input): Parameters<SetEffectEnabledInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits();
        self.run("aviutl2_set_effect_enabled", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_SET_EFFECT_ENABLED,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::set_effect_enabled(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// オブジェクトから effect を削除する。
    /// 対象が既に失われている場合は not_found となり、追加の変更は起きない。
    /// frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。
    /// プロジェクトの世代は selector が運ぶ project_epoch で照合する。要求は
    /// project_revision を運ばない。読み取りから編集までに revision が進んでいても
    /// 拒否されない。対象が変化していれば fingerprint が、別のプロジェクトであれば
    /// selector の project_epoch が拒否する。
    /// selector には aviutl2_get_object が返した effect の selector をそのまま指定する。
    /// 応答が返した selector は読み直さずにそのまま次の編集へ渡せる。
    /// effect を削除するとそのオブジェクトの fingerprint も変わるため、変更前の
    /// selector で続けて編集すると precondition_failed となる。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// 対象が変化していた場合の precondition_failed では、details.current_object に
    /// 対象の現在の値が入る。読み直さずにそのまま次の要求の selector として使える。
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
        let limits = self.limits();
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
    /// プロジェクトの世代は selector が運ぶ project_epoch で照合する。要求は
    /// project_revision を運ばない。読み取りから編集までに revision が進んでいても
    /// 拒否されない。対象が変化していれば fingerprint が、別のプロジェクトであれば
    /// selector の project_epoch が拒否する。
    /// selector には応答が返した値をそのまま指定する。他の編集 tool では応答が
    /// 返した selector をそのまま次の編集へ渡せるが、削除した対象の selector は
    /// 以後どの編集にも使えない。
    /// 対象のレイヤーがロックされている場合は precondition_failed（layer_locked）と
    /// なる。aviutl2_set_layer_state でロックを解除してから再実行する。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// 対象が変化していた場合の precondition_failed では、details.current_object に
    /// 対象の現在の値が入る。読み直さずにそのまま次の要求の selector として使える。
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
        let limits = self.limits();
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

    /// レイヤーの名前・表示・ロック状態を変更する。
    /// name と enabled と locked の 3 つ全てを省略した要求は受け付けない。
    /// name に {"type": "reset"} を指定すると標準のレイヤー名へ戻す。
    /// name に {"type": "set"} を指定する場合、空の名前は受け付けず
    /// invalid_argument となる。標準名へ戻すには reset を指定する。
    /// layer 番号は 0 始まりであり UI の表示とは異なる。
    /// expected_project_epoch には直前の読み取りまたは編集の応答が返した
    /// project_epoch をそのまま指定する。省略はできない。レイヤーは selector も
    /// fingerprint も持たないため、これがプロジェクト境界を照合する唯一の材料である。
    /// 要求は project_revision を運ばない。読み取りから変更までに revision が進んで
    /// いても拒否されない。
    /// レイヤーには fingerprint が無いため、読み取った時点から状態が変わっていても
    /// 検出できない。応答が返す layer には変更後に読み直した実際の状態が入るので、
    /// 意図どおりかはその値で確認する。
    /// レイヤーのロックが止めるのはオブジェクトの削除と時間軸上の移動であり、MCP では
    /// aviutl2_move_object と aviutl2_delete_object と aviutl2_create_object が
    /// precondition_failed（layer_locked）になる。設定値の変更や effect の増減は止めない。
    /// この tool 自身はロックの影響を受けない。ロックされたレイヤーでもロックを外せる。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// この呼び出し 1 回が 1 つの取り消し単位になる。
    #[tool(
        name = "aviutl2_set_layer_state",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_layer_state()
        )
    )]
    pub async fn aviutl2_set_layer_state(
        &self,
        Parameters(input): Parameters<SetLayerStateInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits();
        self.run("aviutl2_set_layer_state", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: LayerStateOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_SET_LAYER_STATE,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::layer_state(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// カーソル位置・選択範囲・フォーカス対象を変更する。
    /// cursor と selected_range と focus の 3 つ全てを省略した要求は受け付けない。
    /// frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。
    /// expected_project_epoch には直前の読み取りまたは編集の応答が返した
    /// project_epoch をそのまま指定する。省略はできない。focus を省略した要求は
    /// selector を 1 つも持たないため、これがプロジェクト境界を照合する材料である。
    /// 要求は project_revision を運ばない。読み取りから変更までに revision が進んで
    /// いても拒否されない。
    /// focus の selector には応答が返した値をそのまま指定する。指定した対象が
    /// 変化していれば fingerprint が、別のプロジェクトであれば selector の
    /// project_epoch が拒否する。応答が返した selector は読み直さずにそのまま
    /// 次の編集へ渡せる。
    /// focus の対象が変化していた場合の precondition_failed では、
    /// details.current_object に対象の現在の値が入る。読み直さずにそのまま
    /// 次の要求の selector として使える。
    /// この tool は他の編集 tool と異なり取り消し単位を作らない。実行後に取り消し
    /// 操作を行うと、カーソルや選択範囲ではなく、その前に行った編集が取り消される。
    /// 応答が返す反映値は編集と原子的に観測したものではなく、ホストが範囲外の値を
    /// クランプした結果である。実際に適用できた項目は applied が、要求したが
    /// 適用できなかった項目は not_applied が示す。一部だけが適用されても応答は
    /// 成功であり、not_applied が空でなければ残りは反映されていない。
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
        let limits = self.limits();
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

    /// 複数の編集を 1 つの取り消し単位としてまとめて適用する。
    /// operations へ入れられるのは move_object と set_object_item の 2 種だけであり、
    /// 他の編集は対応する単独 tool を使う。件数は 1 件以上 100 件以下である。
    /// frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。
    /// この呼び出し 1 回の全体が 1 つの取り消し単位になる。
    /// 1 つの batch の中では、同じ読み取り時点の selector をそのまま並べてよい。
    /// 単独 tool を連続して呼ぶ場合と異なり、先行する変更で後続の selector が
    /// 無効にならない。全対象を変更前にまとめて照合するためである。
    /// プロジェクトの世代は selector が運ぶ project_epoch で照合する。要求は
    /// project_revision を運ばない。読み取りから編集までに revision が進んでいても
    /// 拒否されない。
    /// 応答が返した selector は読み直さずにそのまま次の編集へ渡せる。
    /// 配列順に適用し、宛先の空きは適用時点で確かめる。したがって先行する移動が
    /// 空けた場所を、後続の移動の宛先にできる。
    /// ただし 2 つのオブジェクトが互いの位置を交換する 2 件は通らない。1 件目を
    /// 適用する時点で相手がまだ宛先に居るためである。交換は空きレイヤーを
    /// 経由する 3 件に分けること。
    /// 同じ対象の同じ状態を 2 回変更する要求は受け付けない。同じオブジェクトの
    /// 2 回の移動と、同じ設定項目への 2 回の書き込みがこれに当たる。
    /// 途中で失敗した場合はそれまでに適用した変更を自動で巻き戻す。
    /// 失敗したときは details.failed_index が何番目で落ちたかを返す。
    /// オブジェクトの fingerprint が食い違った場合は details.failed_object が
    /// その対象の現在の状態も返すので、100 件を読み直さずにその 1 件だけを
    /// 差し替えて再要求できる。effect の fingerprint が食い違った場合は
    /// details.failed_object が付かないため、対象オブジェクトを読み直す。
    /// details.consistency_unknown が立っている場合は巻き戻しに失敗しており、
    /// プロジェクトが中途半端な状態の可能性がある。必ず読み直すこと。
    /// details.rolled_back_count は復旧の手掛かりであって被害の正確な計量ではない。
    /// 1 件の巻き戻し失敗が後続の巻き戻しを連鎖的に失敗させ得るため、実際に
    /// 壊れている件数を過大に見積もり得る。
    /// ロックされたレイヤーが妨げるのは move_object だけであり、
    /// precondition_failed（layer_locked）となる。設定値の変更はロックされた
    /// レイヤー上でも通る。解除は aviutl2_set_layer_state で行う。
    /// timeout は変更が無かったことを意味しない。details.change_applied が "no" なら
    /// 未適用のため再送してよく、"unknown" なら読み直して確認してから再送する。
    /// 大きなプロジェクトでは適用中に AviUtl2 の UI が数秒止まり得る。
    #[tool(
        name = "aviutl2_apply_batch",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::apply_batch()
        )
    )]
    pub async fn aviutl2_apply_batch(
        &self,
        Parameters(input): Parameters<ApplyBatchInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits();
        self.run("aviutl2_apply_batch", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: BatchOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_APPLY_BATCH,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::apply_batch(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// 現在シーンの 1 フレームを描画し、成果物を resource として返す。
    /// frame 番号は 0 始まりであり UI の表示とは異なる。
    /// 描画できるのは現在シーンだけである。expected_scene_id には
    /// aviutl2_get_edit_info などが返した scene_id をそのまま指定する。
    /// 結果は画像そのものではなく resource URI で返る。内容は resources/read で
    /// 取得する。
    /// 成果物は既定で 10 分後に失効し、失効後の resources/read は not_found となる。
    /// 呼ぶたびに新しい成果物が生まれ、古いものは件数と総量の上限で押し出され得る。
    /// 出力形式は PNG のみである。
    /// プロジェクトは変更しないが、一時ファイルを作りホストの計算資源を使う。
    /// 出力（ファイル書き出し）中は edit_blocked となる。プレビュー再生中は成功し得る。
    /// 描画の途中でシーンを切り替えると precondition_failed となる。ただし
    /// 切り替えて戻した場合は検出できない。
    /// シーンの解像度が大きすぎる場合、および描いた結果が大きすぎる場合は
    /// unsupported_operation となる。どちらも要求を直しても通らない。
    /// timeout は描画されなかったことを意味する。プロジェクトは変更されていない
    /// ため、そのまま再送してよい。
    #[tool(
        name = "aviutl2_render_frame",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::render_frame()
        )
    )]
    pub async fn aviutl2_render_frame(
        &self,
        Parameters(input): Parameters<RenderFrameInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let limits = self.limits();
        let artifacts = self.artifacts.clone();
        self.run("aviutl2_render_frame", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let artifacts = artifacts
                .ok_or_else(|| failure::internal_error("描画成果物の保管庫が利用できません"))?;

            // 応答を受けたあと、同じブロッキングタスクの中で成果物を引き取る。
            // 引き渡しの識別子はここで消費して終わり、以降のどの経路にも
            // 現れない。
            let result: RenderFrameResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                OPERATION_RENDER_FRAME,
                &params,
            )?;
            let artifact = artifacts
                .ingest(
                    &instance_id,
                    &result.handoff_token,
                    result.byte_length,
                    &result.sha256,
                )
                .map_err(|error| {
                    // 理由は分類名だけを残す。引き渡しの識別子もパスも記録しない。
                    tracing::warn!(reason = error.as_code(), "描画成果物を引き取れませんでした",);
                    failure::internal_error("描画成果物を引き取れませんでした")
                })?;

            let output = RenderFrameOutput::new(&result, &artifact);
            Ok(ToolSuccess {
                text: describe::render_frame(&output),
                structured: to_structured(&output)?,
            })
        })
        .await
    }
}

impl ServerHandler for AviUtl2McpServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                // 公開する tool は共有設定で切り替わる。要求元が一覧を取り直す
                // 契機を得られるよう宣言する。
                .enable_tool_list_changed()
                .enable_resources()
                .build(),
        );
        info.server_info = Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "AviUtl2 の編集内容を読み取り、変更する。aviutl2_list_instances 以外の tool は instance_id が必須である。frame 番号と layer 番号はいずれも 0 始まりであり UI の表示とは異なる。変更する tool は対象を selector で指し、応答が返した値をそのまま送り返す。selector を持たない aviutl2_create_object と aviutl2_set_selection では、応答が返した project_epoch を expected_project_epoch に必ず指定する。"
                .to_string(),
        );
        info
    }

    /// 公開している tool を列挙する。
    ///
    /// **ハンドラの実行時に現在の snapshot を読む。** 構築時に凍結しないため、
    /// peer が成立する前に設定が変わっていても最初の `tools/list` が正しい一覧を
    /// 返す。tool の定義そのものは router が持つものをそのまま使う——説明と
    /// schema の出所を 2 つにしない。
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: self.visible_tools(),
            next_cursor: None,
            meta: None,
        })
    }

    /// 名前から tool 定義を引く。
    ///
    /// 公開していない tool は「無い」ものとして扱う。`tools/list` に載らない
    /// 名前が、別の経路からは在るものとして見えることを避ける。
    fn get_tool(&self, name: &str) -> Option<Tool> {
        if !self.accepts_tool_call(name) {
            return None;
        }
        self.tool_router.get(name).cloned()
    }

    /// tool call を処理する。
    ///
    /// 公開していない tool は接続先へ送らず `tool_disabled` で拒否する。判定は
    /// `tools/list` と同じ snapshot を読むため、無効化が call の前でも最中でも
    /// 観測される挙動は同じである。**既に実行を開始した call は途中で取り消さ
    /// ない**——判定は受付の時点だけで行う。
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
        if !self.accepts_tool_call(&tool) {
            return Ok(self.reject_disabled_tool(&tool));
        }
        let result = self
            .tool_router
            .call(ToolCallContext::new(self, request, context))
            .await?;
        Ok(normalize_tool_result(&tool, result))
    }

    /// 初期化の完了を受けて、公開する tool の集合の変化を待ち受け始める。
    ///
    /// `notifications/tools/list_changed` を送るのに要る peer は、ここで初めて
    /// 手に入る。**peer が成立する前の変更は通知しない**——最初の `tools/list` が
    /// そのときの snapshot を読むため、取りこぼしにはならない。
    ///
    /// 待ち受けは供給元からの押し出しで起きる。**設定が変わらない限り、この
    /// タスクは 1 度も起きない。** 畳む契機は 2 つだけである——供給元が失われる
    /// か、転送が閉じているかである。
    ///
    /// 通知の送信に失敗しても記録は進める。次の `tools/list` が正であり、同じ
    /// 変化を繰り返し通知しても要求元の得るものは変わらない。
    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        tracing::info!("client initialized");
        let Some(source) = self.limits.shared() else {
            return;
        };
        let catalog = self
            .tools()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        let mut watch = ToolListWatch::new(&source, catalog);
        let peer = context.peer;
        tokio::spawn(async move {
            while watch.changed().await {
                tracing::debug!("公開する tool の集合が変わりました");
                if let Err(e) = peer.notify_tool_list_changed().await {
                    tracing::warn!(error = %e, "tool 一覧の変更を通知できませんでした");
                    if peer.is_transport_closed() {
                        return;
                    }
                }
            }
        });
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

        // 成果物は自分の一覧を読むだけで済む。インスタンスへ接続しないという
        // 制約を自然に満たす。期限切れは [`ArtifactStore::list`] が先に落とす。
        let artifacts = self
            .artifacts
            .as_ref()
            .map(|store| store.list())
            .unwrap_or_default();

        let (resources, next_cursor) = resource_page(&registered, &artifacts, offset);
        let mut result = ListResourcesResult::with_all_items(resources);
        result.next_cursor = next_cursor;
        Ok(result)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let uri = request.uri;
        let registry_dir = self.registry_dir();
        let limits = self.limits();
        let artifacts = self.artifacts.clone();

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
                    ResourceTarget::Artifact(artifact_id) => {
                        // 引き当ては保管庫が持つ一覧に対してのみ行う。識別子を
                        // パスへ連結しないため、どのような文字列が来ても
                        // 「見つからない」で終わる。
                        let content = artifacts
                            .and_then(|store| store.read(&artifact_id))
                            .ok_or_else(artifact_not_found)?;
                        // text の上限は blob へ適用しない。上限は「機械可読値は
                        // structuredContent に置き text は要約に留める」という
                        // 規約に由来し、画像にはその区別が無い。大きさを縛るのは
                        // 引き取り時の上限である。
                        return Ok(ResourceContents::blob(
                            encode_base64(&content.bytes),
                            uri_for_content,
                        )
                        .with_mime_type(content.artifact.media_type));
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

/// resource 一覧 1 ページに載せる項目数の上限。
///
/// インスタンス一覧そのものの 1 項目は数に含めない。
const RESOURCES_PAGE_SIZE: usize = 100;

/// resource 一覧の 1 ページと、続きがある場合の cursor を組み立てる。
///
/// cursor は連結した一覧への 10 進インデックスであり、**instance 由来の項目の
/// あとに成果物を並べる**。成果物を持たない場合、並びも cursor も成果物を
/// 導入する前と変わらない。
fn resource_page(
    registered: &[InstanceId],
    artifacts: &[Artifact],
    offset: usize,
) -> (Vec<Resource>, Option<String>) {
    let mut resources = Vec::new();
    // インスタンス一覧そのものは登録の有無によらず読めるため、先頭ページに載せる。
    if offset == 0 {
        resources.push(
            Resource::new(INSTANCES_RESOURCE_URI, "aviutl2 instances")
                .with_description("登録されている AviUtl2 インスタンス一覧")
                .with_mime_type(RESOURCE_MIME_TYPE),
        );
    }

    let total = registered.len() + artifacts.len();
    let mut count = 0;
    for index in offset..total {
        if count == RESOURCES_PAGE_SIZE {
            break;
        }
        resources.push(match registered.get(index) {
            Some(instance_id) => edit_info_resource(instance_id),
            None => artifact_resource(&artifacts[index - registered.len()]),
        });
        count += 1;
    }

    let next_offset = offset.saturating_add(count);
    // 続きを黙って落とさず、次ページの位置を返す。
    let cursor = (next_offset < total).then(|| encode_cursor(next_offset));
    (resources, cursor)
}

/// インスタンスの編集情報 resource。
fn edit_info_resource(instance_id: &InstanceId) -> Resource {
    // 表示名には完全な instance_id を載せる。URI が同じ値をそのまま運ぶため
    // 削っても秘匿にならず、[`redact`] はログ専用である。削った名前は同じ
    // 接頭辞を持つインスタンスを見分けられなくする。
    Resource::new(
        edit_info_resource_uri(instance_id),
        format!("aviutl2 edit info {instance_id}"),
    )
    .with_description("インスタンスの現在の編集情報")
    .with_mime_type(RESOURCE_MIME_TYPE)
}

/// 描画成果物の resource。
///
/// **名前にも説明にも、画像の内容を推測させる情報を入れない。** 見分けるのに
/// 要るのは識別子と時刻だけである。
fn artifact_resource(artifact: &Artifact) -> Resource {
    Resource::new(
        artifact_resource_uri(&artifact.artifact_id),
        format!("aviutl2 render artifact {}", artifact.artifact_id),
    )
    .with_description(format!(
        "描画成果物（作成 {} / 失効 {}）",
        artifact.created_at.to_rfc3339(),
        artifact.expires_at.to_rfc3339(),
    ))
    .with_mime_type(artifact.media_type)
}

/// 成果物が引き当てられなかったことを表すエラー。
///
/// **期限切れ・未知の識別子・保管庫を開いていないことを区別しない。** 区別すると、
/// 過去に存在した識別子を総当たりで調べられる。
fn artifact_not_found() -> ErrorObject {
    failure::from_code(ErrorCode::NotFound, "指定された resource は存在しません")
}

/// バイト列を標準の base64 へ符号化する。
fn encode_base64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

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
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResourceTarget {
    /// インスタンス一覧。
    Instances,
    /// 指定インスタンスの編集情報。
    EditInfo(InstanceId),
    /// 指定 ID の描画成果物。
    ///
    /// 識別子は解釈せずそのまま保持する。**パスへ連結しない**ため、書式を
    /// 課す必要が無い。引き当てに失敗すれば見つからないで終わる。
    Artifact(String),
}

/// インスタンスの編集情報 resource の URI。
pub fn edit_info_resource_uri(instance_id: &InstanceId) -> String {
    format!("{INSTANCES_RESOURCE_URI}/{instance_id}/edit-info")
}

/// 描画成果物 resource の URI。
///
/// 識別子は保管庫が採番した値であり、接続先が書いた引き渡しファイルの名前とは
/// 別物である。URI を見ても他プロセスのファイル名は導けない。
pub fn artifact_resource_uri(artifact_id: &str) -> String {
    format!("{ARTIFACTS_RESOURCE_URI_PREFIX}{artifact_id}")
}

/// resource URI を解釈する。未知の URI は `None`。
fn parse_resource_uri(uri: &str) -> Option<ResourceTarget> {
    if uri == INSTANCES_RESOURCE_URI {
        return Some(ResourceTarget::Instances);
    }
    if let Some(artifact_id) = uri.strip_prefix(ARTIFACTS_RESOURCE_URI_PREFIX) {
        // 識別子の書式を課さない。引き当ては保管庫が持つ一覧に対してのみ行い、
        // パスへ連結しないため、書式を確かめても防げるものが無い。
        return (!artifact_id.is_empty())
            .then(|| ResourceTarget::Artifact(artifact_id.to_string()));
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
/// 要求へ載せる期限は operation 名から選ぶ（[`CallLimits::ipc_request_budget`]）。
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

    let deadline = Instant::now() + limits.ipc_request_budget(operation);
    tracing::debug!(
        instance = %redact::instance_id(&instance_id),
        operation,
        "sending request",
    );
    resolved
        .client
        .request_typed(operation, params, deadline)
        .map_err(|e| failure::from_pipe_error(&e, operation))
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
        // 失効した成果物と未知の識別子も、この server から今は取得できない
        // resource である。両者を区別しない。
        ErrorCode::InstanceNotFound
        | ErrorCode::InstanceStale
        | ErrorCode::HostBusy
        | ErrorCode::EditBlocked
        | ErrorCode::NotFound
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
        "aviutl2_set_effect_enabled",
        "aviutl2_delete_effect",
        "aviutl2_delete_object",
        "aviutl2_set_layer_state",
        "aviutl2_set_selection",
        "aviutl2_apply_batch",
        "aviutl2_render_frame",
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
    /// 宛先の重複確認と対象の fingerprint により通常は防がれるが、annotation は
    /// 「再送が安全である」と主張しない側へ倒す。
    const EDIT_TOOL_ANNOTATIONS: &[(&str, bool, bool)] = &[
        ("aviutl2_create_object", false, false),
        ("aviutl2_move_object", false, true),
        ("aviutl2_set_object_name", false, true),
        ("aviutl2_set_object_item", false, true),
        ("aviutl2_add_effect", false, false),
        ("aviutl2_set_effect_enabled", false, true),
        ("aviutl2_delete_effect", true, true),
        ("aviutl2_delete_object", true, true),
        // 表示を切ってもロックを掛けても内容は失われず、同じ tool で戻せる。
        // 同じ状態を 2 度設定しても追加の変更を起こさない。
        ("aviutl2_set_layer_state", false, true),
        ("aviutl2_set_selection", false, true),
    ];

    /// 一括適用の tool 名。
    const APPLY_BATCH: &str = "aviutl2_apply_batch";

    /// 描画の tool 名。
    const RENDER_FRAME: &str = "aviutl2_render_frame";

    /// 一括適用と描画の tool、および宣言する annotation。
    ///
    /// 値は `read_only_hint` / `destructive_hint` / `idempotent_hint` の組である。
    /// `open_world_hint` は全 tool で偽であるため表に持たない。
    ///
    /// 一括適用を冪等と名乗らないのは、冪等かどうかが中身に依存する一方、
    /// annotation は tool 単位でしか付けられないためである。作成系と同じく、
    /// 「再送が安全である」と主張しない側へ倒す。
    const PHASE4_TOOL_ANNOTATIONS: &[(&str, bool, bool)] = &[
        (APPLY_BATCH, false, false),
        // 描画はプロジェクトを変更せず、同じ要求は同じ絵を返す。
        (RENDER_FRAME, true, true),
    ];

    /// 登録済みの tool 名を、annotation の 3 表から引く。
    ///
    /// 表と router が一致することは [`all_tools_are_registered`] が固定する。
    fn all_tool_names() -> impl Iterator<Item = &'static str> {
        READ_TOOLS
            .iter()
            .copied()
            .chain(EDIT_TOOL_ANNOTATIONS.iter().map(|(name, _, _)| *name))
            .chain(PHASE4_TOOL_ANNOTATIONS.iter().map(|(name, _, _)| *name))
    }

    /// tool が編集 tool の説明規約に従うか。
    ///
    /// 一括適用は編集 tool の表には属さないが、運ぶ selector も取り消し単位も
    /// 編集と同じであるため従う側に置く。読み取りと描画はプロジェクトを
    /// 変更しないため従わない。
    ///
    /// **未知の tool 名で落とす。** 一覧を手書きの連結で持つと、そこから外した
    /// tool が説明の共通検査から黙って外れる。
    fn follows_the_edit_conventions(name: &str) -> bool {
        match name {
            "aviutl2_create_object"
            | "aviutl2_move_object"
            | "aviutl2_set_object_name"
            | "aviutl2_set_object_item"
            | "aviutl2_add_effect"
            | "aviutl2_set_effect_enabled"
            | "aviutl2_delete_effect"
            | "aviutl2_delete_object"
            | "aviutl2_set_layer_state"
            | "aviutl2_set_selection"
            | APPLY_BATCH => true,
            "aviutl2_list_instances"
            | "aviutl2_get_edit_info"
            | "aviutl2_get_current_scene"
            | "aviutl2_list_layers"
            | "aviutl2_list_objects"
            | "aviutl2_get_object"
            | "aviutl2_list_available_effects"
            | RENDER_FRAME => false,
            other => panic!("{other} が編集の説明規約に従うかが定義されていません"),
        }
    }

    /// 編集の説明規約が掛かる tool。
    fn edit_like_tools() -> Vec<&'static str> {
        all_tool_names()
            .filter(|name| follows_the_edit_conventions(name))
            .collect()
    }

    #[test]
    fn the_edit_conventions_cover_the_editing_tools_and_the_batch() {
        // 集合そのものを固定する。判定を「従わない」側へ書き換えても、対象が
        // 減ったことに気付けるようにする。
        let covered: std::collections::BTreeSet<&str> = edit_like_tools().into_iter().collect();
        let expected: std::collections::BTreeSet<&str> = EDIT_TOOL_ANNOTATIONS
            .iter()
            .map(|(name, _, _)| *name)
            .chain(std::iter::once(APPLY_BATCH))
            .collect();
        assert_eq!(covered, expected);
    }

    /// tool 定義と応答の組み立てだけを見るサーバー。
    ///
    /// 保管庫を開かない構築口を使う。開くと registry から導いた基底へ保護された
    /// DACL を書き込むため、実在しないパスや相対パスを渡す検査で実際の
    /// ディレクトリへ触れてしまう。描画の経路は統合テストが確かめる。
    fn server() -> AviUtl2McpServer {
        AviUtl2McpServer::without_artifact_store(
            PathBuf::from(r"C:\nonexistent-registry"),
            CallLimits::default(),
        )
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
        // 公開する tool の集合は 3 つの表の和集合と一致する。表に載せずに登録
        // すると annotation も説明も検査されないまま公開される。
        let names: std::collections::BTreeSet<String> =
            tools().iter().map(|tool| tool.name.to_string()).collect();
        let expected: std::collections::BTreeSet<String> =
            all_tool_names().map(|name| name.to_string()).collect();
        assert_eq!(names, expected);
        // 件数そのものも固定する。router と表の両方から同じ tool を落とすと、
        // 集合の一致だけでは検出できない。
        assert_eq!(names.len(), 19, "公開する tool の数が変わりました");
    }

    /// 共有設定を与えたサーバー。
    fn server_with(settings_json: &str) -> AviUtl2McpServer {
        let settings = aviutl2_mcp_core::settings::SettingsDocument::parse(settings_json)
            .expect("設定を解析できます")
            .resolve(&aviutl2_mcp_core::settings::Settings::default())
            .0;
        AviUtl2McpServer::with_settings(
            PathBuf::from(r"C:\nonexistent-registry"),
            SettingsSource::fixed(settings),
        )
    }

    fn visible_names(server: &AviUtl2McpServer) -> std::collections::BTreeSet<String> {
        server
            .visible_tools()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    #[test]
    fn the_registered_tools_match_the_shared_catalog() {
        // 切替の対象を列挙する側は rmcp の属性を見られないため、名前の一覧を
        // core が operation から導いている。規則から外れる tool を足すと、
        // ここで集合が食い違って落ちる。
        let registered: std::collections::BTreeSet<String> =
            tools().iter().map(|tool| tool.name.to_string()).collect();
        let shared: std::collections::BTreeSet<String> =
            aviutl2_mcp_core::tool::all_tool_names().collect();
        assert_eq!(registered, shared);
        assert!(
            shared.contains(aviutl2_mcp_core::tool::ALWAYS_ENABLED_TOOL),
            "常時有効な tool が一覧に含まれていません"
        );
    }

    #[test]
    fn without_settings_every_registered_tool_is_listed() {
        let server = server();
        assert_eq!(visible_names(&server).len(), tools().len());
    }

    #[test]
    fn a_disabled_tool_is_neither_listed_nor_accepted() {
        let server = server_with(r#"{"disabled_tools":["aviutl2_delete_object"]}"#);
        assert!(!visible_names(&server).contains("aviutl2_delete_object"));
        assert!(!server.accepts_tool_call("aviutl2_delete_object"));
        // 巻き添えにしない。
        assert!(server.accepts_tool_call("aviutl2_delete_effect"));
    }

    #[test]
    fn the_always_enabled_tool_survives_being_disabled() {
        let server =
            server_with(r#"{"disabled_tools":["aviutl2_list_instances","aviutl2_render_frame"]}"#);
        let visible = visible_names(&server);
        assert!(visible.contains(aviutl2_mcp_core::tool::ALWAYS_ENABLED_TOOL));
        assert!(server.accepts_tool_call(aviutl2_mcp_core::tool::ALWAYS_ENABLED_TOOL));
        assert!(!visible.contains("aviutl2_render_frame"));
    }

    #[test]
    fn what_is_listed_is_exactly_what_is_accepted() {
        // 掲載と受付が同じ判定を読むことを、全 tool について固定する。片方だけを
        // 絞る実装になると、掲載していない tool の call が通る。
        let server = server_with(
            r#"{"disabled_tools":["aviutl2_delete_object","aviutl2_apply_batch","aviutl2_list_instances"]}"#,
        );
        let visible = visible_names(&server);
        for tool in tools() {
            assert_eq!(
                visible.contains(tool.name.as_ref()),
                server.accepts_tool_call(&tool.name),
                "{} の掲載と受付が食い違っています",
                tool.name
            );
        }
        assert_eq!(visible.len(), tools().len() - 2);
    }

    #[test]
    fn an_unknown_tool_name_is_not_treated_as_disabled() {
        // 未知の名前は「無効化されている」ではなく「登録されていない」である。
        // 判定を反転させると、未知の tool が tool_disabled を名乗る。
        let server = server_with(r#"{"disabled_tools":["aviutl2_delete_object"]}"#);
        assert!(server.accepts_tool_call("aviutl2_future_tool"));
    }

    #[test]
    fn a_disabled_tool_is_rejected_with_the_documented_code() {
        let server = server_with(r#"{"disabled_tools":["aviutl2_delete_object"]}"#);
        let result = server.reject_disabled_tool("aviutl2_delete_object");
        assert_eq!(result.is_error, Some(true));
        let structured = result
            .structured_content
            .expect("失敗も structuredContent を持つ");
        assert_eq!(structured["code"], serde_json::json!("tool_disabled"));
        assert_eq!(structured["retryable"], serde_json::json!(false));
        assert!(
            structured["correlation_id"].is_string(),
            "correlation_id が付いていません"
        );
        assert!(structured.get("details").is_some());
    }

    #[test]
    fn phase4_tools_are_annotated_as_documented() {
        for (name, read_only, idempotent) in PHASE4_TOOL_ANNOTATIONS {
            let tool = tool_named(name);
            let annotations = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{name} に annotation がありません"));
            assert_eq!(
                annotations.read_only_hint,
                Some(*read_only),
                "{name} の readOnlyHint"
            );
            // 一括適用にも描画にも削除は入らない。
            assert_eq!(
                annotations.destructive_hint,
                Some(false),
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
            "aviutl2_set_effect_enabled" => schema::set_effect_enabled(),
            "aviutl2_delete_effect" => schema::delete_effect(),
            "aviutl2_delete_object" => schema::delete_object(),
            "aviutl2_set_layer_state" => schema::set_layer_state(),
            "aviutl2_set_selection" => schema::set_selection(),
            "aviutl2_apply_batch" => schema::apply_batch(),
            "aviutl2_render_frame" => schema::render_frame(),
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

    /// 前提の epoch を要求が運ぶ tool。
    ///
    /// 対象を指す selector を持たないため、これがプロジェクト境界を照合する
    /// 材料になる。
    const TOOLS_CARRYING_AN_EXPECTED_EPOCH: &[&str] = &[
        "aviutl2_create_object",
        "aviutl2_set_layer_state",
        "aviutl2_set_selection",
    ];

    #[test]
    fn edit_tool_descriptions_state_what_costs_the_caller_if_assumed_wrong() {
        // いずれも誤った前提で操作すると損失が生じる事項であり、説明から
        // 落とせない。
        for name in edit_like_tools() {
            let description = description_of(name);
            for keyword in [
                "0 始まり",
                "UI の表示とは異なる",
                "project_epoch",
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
    fn only_the_tools_without_a_selector_ask_for_an_expected_epoch() {
        // 前提の epoch を運ぶのは selector を持たない 2 tool だけである。他の tool の
        // 説明が求めると、呼び出し側は送れない値を探すことになる。どちらの側に
        // 属するかを表で固定するので、tool を足したときに素通りしない。
        for name in edit_like_tools() {
            let description = description_of(name);
            if TOOLS_CARRYING_AN_EXPECTED_EPOCH.contains(&name) {
                for keyword in ["expected_project_epoch", "省略はできない"] {
                    assert!(
                        description.contains(keyword),
                        "{name} の説明に {keyword} がありません"
                    );
                }
                continue;
            }
            assert!(
                !description.contains("expected_project_epoch"),
                "{name} の説明が運べない前提の epoch を求めています"
            );
            assert!(
                description.contains("selector が運ぶ project_epoch"),
                "{name} の説明が境界の照合材料を示していません"
            );
        }
    }

    #[test]
    fn edit_tool_descriptions_admit_that_the_revision_is_not_part_of_the_request() {
        // 要求は project_revision を運ばない。説明が黙っていると、呼び出し側は
        // 拒否を避けるために revision を取り直し続ける。
        for name in edit_like_tools() {
            let description = description_of(name);
            assert!(
                description.contains("project_revision を運ばない"),
                "{name} の説明が revision を要求しないことを述べていません"
            );
        }
    }

    #[test]
    fn creation_tools_name_the_guard_that_actually_stops_a_resend() {
        // revision を照合しない以上、再送を止めるのは宛先の重複確認と対象の
        // fingerprint である。防ぐ仕組みを取り違えて案内すると、呼び出し側は
        // 効かない対策を信じて再送する。
        assert!(
            description_of("aviutl2_create_object").contains("destination_occupied"),
            "aviutl2_create_object の説明が宛先重複の確認に触れていません"
        );
        assert!(
            description_of("aviutl2_add_effect").contains("fingerprint が変わるため"),
            "aviutl2_add_effect の説明が fingerprint の変化に触れていません"
        );
        for name in ["aviutl2_create_object", "aviutl2_add_effect"] {
            assert!(
                !description_of(name).contains("同じ expected での再送"),
                "{name} の説明が expected による重複防止を主張しています"
            );
        }
    }

    /// 応答が返す位置が要求した宛先と一致するとは限らない tool。
    ///
    /// ホストが配置を調整し得るため、成功を「要求どおりの位置」と読むと、
    /// 呼び出し側が組み立てた次の要求は別の場所を指す。どちらの側に属するかを
    /// 表で固定するので、tool を足したときに素通りしない。
    const TOOLS_WHOSE_RESPONSE_CARRIES_THE_ACTUAL_PLACEMENT: &[&str] =
        &["aviutl2_create_object", "aviutl2_move_object"];

    #[test]
    fn tools_that_can_land_elsewhere_say_the_response_carries_the_actual_placement() {
        for name in edit_like_tools() {
            let description = description_of(name);
            if TOOLS_WHOSE_RESPONSE_CARRIES_THE_ACTUAL_PLACEMENT.contains(&name) {
                for keyword in [
                    "応答が返す位置は要求した宛先と異なり得る",
                    "配置を確かめるには応答の値を見る",
                ] {
                    assert!(
                        description.contains(keyword),
                        "{name} の説明に {keyword} がありません"
                    );
                }
                continue;
            }
            assert!(
                !description.contains("実際の配置"),
                "{name} の説明が持たない性質を述べています"
            );
        }
    }

    /// tool の説明が取り消しについて述べる内容。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum UndoStatement {
        /// 1 回の呼び出しが 1 つの取り消し単位になると述べる。
        OneUnit,
        /// 取り消し単位を作らず、取り消しが 1 つ前の編集へ飛ぶと述べる。
        NoUnitAndJumpsBack,
    }

    /// tool 名から、説明が取り消しについて述べる内容を引く。
    ///
    /// 未知の tool 名で落とす。**説明は保証である**ため、述べるか黙るかの判断を
    /// tool ごとに 1 か所へ置き、tool を足したときに素通りしないようにする。
    fn undo_statement(name: &str) -> UndoStatement {
        match name {
            "aviutl2_create_object"
            | "aviutl2_move_object"
            | "aviutl2_set_object_name"
            | "aviutl2_set_object_item"
            | "aviutl2_add_effect"
            | "aviutl2_set_effect_enabled"
            | "aviutl2_delete_effect"
            | "aviutl2_delete_object"
            | "aviutl2_set_layer_state"
            | "aviutl2_apply_batch" => UndoStatement::OneUnit,
            "aviutl2_set_selection" => UndoStatement::NoUnitAndJumpsBack,
            other => panic!("{other} の取り消しの説明が定義されていません"),
        }
    }

    #[test]
    fn edit_tool_descriptions_state_the_undo_boundary() {
        for name in edit_like_tools() {
            let description = description_of(name);
            match undo_statement(name) {
                UndoStatement::OneUnit => assert!(
                    description.contains("1 つの取り消し単位"),
                    "{name} の説明に取り消し単位がありません"
                ),
                UndoStatement::NoUnitAndJumpsBack => {
                    // 「戻る保証が無い」は「戻るかもしれない」と読める。実際は
                    // 戻らないうえに取り消しが 1 つ前の編集まで飛ぶため、失う
                    // ものを名指しする。
                    assert!(
                        description.contains("取り消し単位を作らない"),
                        "{name} の説明に取り消し単位を作らない旨がありません"
                    );
                    assert!(
                        description.contains("その前に行った編集が取り消される"),
                        "{name} の説明が取り消しの飛び先を述べていません"
                    );
                    assert!(
                        !description.contains("1 つの取り消し単位"),
                        "{name} の説明が取り消し単位を作ると読めます"
                    );
                }
            }
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
            ("aviutl2_set_effect_enabled", &["fingerprint", "出力 item"]),
            ("aviutl2_delete_effect", &["fingerprint", "not_found"]),
            ("aviutl2_delete_object", &["not_found"]),
            (
                "aviutl2_set_selection",
                &["原子的", "クランプ", "全てを省略"],
            ),
            (
                "aviutl2_set_layer_state",
                &[
                    "fingerprint",
                    "全てを省略した要求は受け付けない",
                    "この tool 自身はロックの影響を受けない",
                    "aviutl2_move_object",
                    "reset",
                ],
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

    /// tool の説明がレイヤーのロックについて述べる内容。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum LayerLockStatement {
        /// ロックで拒否されることと、解除の手段を述べる。
        StoppedAndNamesTheWayOut,
        /// ロックが止める範囲と、自身が影響を受けないことを述べる。
        DescribesTheScope,
        /// ロックについて何も述べない。
        Silent,
    }

    /// tool 名から、説明がレイヤーのロックについて述べる内容を引く。
    ///
    /// 未知の tool 名で落とす。ロックが止めるのはオブジェクトの削除と時間軸上の
    /// 移動であり、対象を 1 か所へ置いて tool を足したときに素通りしないようにする。
    fn layer_lock_statement(name: &str) -> LayerLockStatement {
        match name {
            "aviutl2_create_object"
            | "aviutl2_move_object"
            | "aviutl2_delete_object"
            // 一括適用が止まるのは move_object を含む場合だけだが、止まり方も
            // 解き方も同じであるため、案内する側に属する。
            | "aviutl2_apply_batch" => LayerLockStatement::StoppedAndNamesTheWayOut,
            "aviutl2_set_layer_state" => LayerLockStatement::DescribesTheScope,
            "aviutl2_set_object_name"
            | "aviutl2_set_object_item"
            | "aviutl2_add_effect"
            | "aviutl2_set_effect_enabled"
            | "aviutl2_delete_effect"
            | "aviutl2_set_selection" => LayerLockStatement::Silent,
            other => panic!("{other} のレイヤーロックの説明が定義されていません"),
        }
    }

    #[test]
    fn tools_stopped_by_a_layer_lock_name_the_way_out() {
        // layer_locked の retry_requires は none である。案内が無ければ、契約に
        // 従う要求元は解ける状況で停止する。
        for name in edit_like_tools() {
            let description = description_of(name);
            match layer_lock_statement(name) {
                LayerLockStatement::StoppedAndNamesTheWayOut => {
                    assert!(
                        description.contains("layer_locked"),
                        "{name} の説明がロックによる拒否を述べていません"
                    );
                    assert!(
                        description.contains("aviutl2_set_layer_state"),
                        "{name} の説明がロックの解除手段を示していません"
                    );
                }
                LayerLockStatement::DescribesTheScope => {
                    assert!(
                        description.contains("この tool 自身はロックの影響を受けない"),
                        "{name} の説明が自身にロックが掛からないことを述べていません"
                    );
                }
                LayerLockStatement::Silent => assert!(
                    !description.contains("layer_locked"),
                    "{name} の説明が掛からないロックによる拒否を述べています"
                ),
            }
        }
    }

    /// tool 名から、失敗応答が対象の現在の姿を返し得るかを引く。
    ///
    /// 未知の tool 名で落とす。返し得るのは対象を指す selector を解決する tool
    /// だけであり、作成は対象がまだ無く、レイヤーの状態変更は対象が selector も
    /// fingerprint も持たない。**一覧を const で持つと、どちらにも書かれていない
    /// 新しい tool が「触れない」側の既定へ黙って落ちる。**
    fn returns_a_current_object(name: &str) -> bool {
        match name {
            "aviutl2_move_object"
            | "aviutl2_set_object_name"
            | "aviutl2_set_object_item"
            | "aviutl2_add_effect"
            | "aviutl2_set_effect_enabled"
            | "aviutl2_delete_effect"
            | "aviutl2_delete_object"
            | "aviutl2_set_selection" => true,
            // 一括適用は 100 件のうちどれが落ちたかを併せて示す必要があるため、
            // 別のキー（failed_object）で返す。
            "aviutl2_create_object" | "aviutl2_set_layer_state" | "aviutl2_apply_batch" => false,
            other => panic!("{other} が現在の姿を返すかが定義されていません"),
        }
    }

    #[test]
    fn tools_that_return_the_current_object_say_so() {
        // 失敗応答に載る値は tool schema に現れない。説明が触れなければ、
        // 呼び出し側はその存在を知る手段が無く読み直しに戻る。
        for name in edit_like_tools() {
            let description = description_of(name);
            assert_eq!(
                description.contains("details.current_object"),
                returns_a_current_object(name),
                "{name} の説明と現在の姿を返す tool の一覧が食い違います"
            );
        }
    }

    #[test]
    fn the_batch_description_states_what_costs_the_caller_if_assumed_wrong() {
        // 一括適用は 1 回で最大 100 件の変更を起こす。誤った期待で要求を組み
        // 立てさせると、失敗からの復帰が最も高くつく。
        let description = description_of(APPLY_BATCH);
        for keyword in [
            // 入れられる operation を取り違えさせない。
            "move_object と set_object_item",
            "単独 tool",
            "1 件以上 100 件以下",
            // 一括適用を使う 2 つの理由。
            "1 つの取り消し単位",
            "同じ読み取り時点の selector",
            "配列順",
            // 適用時点で宛先を見ることで何ができ、何ができないか。片方だけを
            // 書くと、通らない要求を組み立てさせるか、通る要求を諦めさせる。
            "先行する移動が",
            "互いの位置を交換する 2 件は通らない",
            // 拒否と失敗の読み方。
            "2 回変更する要求は受け付けない",
            "自動で巻き戻す",
            "details.failed_index",
            "details.failed_object",
            "details.consistency_unknown",
            "必ず読み直すこと",
            "details.rolled_back_count",
            "計量ではない",
            // ロックの範囲と解き方。
            "layer_locked",
            "設定値の変更はロックされた",
            // 費用。
            "UI が数秒止まり得る",
        ] {
            assert!(
                description.contains(keyword),
                "{APPLY_BATCH} の説明に {keyword} がありません"
            );
        }
    }

    #[test]
    fn the_render_description_states_what_costs_the_caller_if_assumed_wrong() {
        let description = description_of(RENDER_FRAME);
        for keyword in [
            "0 始まり",
            // 描けるのは現在シーンだけである。
            "現在シーンだけ",
            "expected_scene_id",
            "aviutl2_get_edit_info",
            // 画像は応答に埋めない。
            "resource URI で返る",
            "resources/read",
            // 後から読もうとして失敗する場面を減らす。
            "10 分後に失効",
            "not_found",
            "押し出され得る",
            "PNG のみ",
            // readOnlyHint の意味を狭く伝える。
            "プロジェクトは変更しない",
            "一時ファイル",
            "計算資源",
            // 失敗の理由と再試行の判断。
            "edit_blocked",
            "プレビュー再生中は成功し得る",
            "precondition_failed",
            "切り替えて戻した場合は検出できない",
            "unsupported_operation",
            // 上限は解像度だけで決まらない。描いた結果の大きさでも掛かる。
            "描いた結果が大きすぎる",
            "どちらも要求を直しても通らない",
            "timeout は描画されなかったことを意味する",
            "そのまま再送してよい",
        ] {
            assert!(
                description.contains(keyword),
                "{RENDER_FRAME} の説明に {keyword} がありません"
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
    fn input_schemas_declare_the_expected_epoch_only_where_it_is_used() {
        // 前提の epoch を持つのは、対象を指す selector を持たない tool だけで
        // ある。持つ tool へ宣言すると、同じ意味の値が 1 要求の 2 か所へ並ぶ。
        for name in edit_like_tools() {
            let tool = tool_named(name);
            let properties = tool
                .input_schema
                .get("properties")
                .and_then(|v| v.as_object())
                .unwrap_or_else(|| panic!("{name} に properties がありません"));

            let carries = TOOLS_CARRYING_AN_EXPECTED_EPOCH.contains(&name);
            assert_eq!(
                properties.contains_key("expected_project_epoch"),
                carries,
                "{name} の入力 schema と前提の epoch の要否が食い違います"
            );
            let required = tool
                .input_schema
                .get("required")
                .and_then(|v| v.as_array())
                .map(|items| items.contains(&serde_json::json!("expected_project_epoch")))
                .unwrap_or(false);
            assert_eq!(required, carries, "{name} の必須指定が食い違います");
        }
    }

    #[test]
    fn the_batch_input_schema_declares_the_operation_count_it_actually_enforces() {
        // 宣言した制約は server 側で実際に検証する。宣言だけがあって検証されない
        // 制約を schema に残さない。件数は core の検証が判定する。
        let tool = tool_named(APPLY_BATCH);
        let operations = tool.input_schema["properties"]["operations"].clone();
        assert_eq!(operations["minItems"], serde_json::json!(1));
        assert_eq!(
            operations["maxItems"],
            serde_json::json!(aviutl2_mcp_core::MAX_BATCH_OPERATIONS)
        );
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
            router_argument_error("failed to deserialize parameters: missing field `selector`"),
        );
        assert!(
            text_of(&result).contains("selector"),
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
            failure::from_pipe_error(
                &crate::pipe_client::PipeClientError::Remote(Box::new(remote)),
                aviutl2_mcp_core::OPERATION_MOVE_OBJECT,
            ),
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
        use aviutl2_mcp_core::{
            SERVER_ARTIFACT_INGEST_BUDGET, SERVER_BATCH_REQUEST_BUDGET, SERVER_EDIT_REQUEST_BUDGET,
            SERVER_READ_REQUEST_BUDGET, SERVER_RENDER_REQUEST_BUDGET, SERVER_RESOLVE_BUDGET,
        };
        let limits = CallLimits::default();
        assert_eq!(limits.resolve, SERVER_RESOLVE_BUDGET);
        assert_eq!(limits.request, SERVER_READ_REQUEST_BUDGET);
        assert_eq!(limits.edit_request, SERVER_EDIT_REQUEST_BUDGET);
        assert_eq!(limits.batch_request, SERVER_BATCH_REQUEST_BUDGET);
        assert_eq!(limits.render_request, SERVER_RENDER_REQUEST_BUDGET);
        assert_eq!(limits.artifact_ingest, SERVER_ARTIFACT_INGEST_BUDGET);
        assert_eq!(
            DiscoveryConfig::default().per_candidate_deadline,
            SERVER_RESOLVE_BUDGET
        );
    }

    #[test]
    fn the_limits_follow_the_shared_settings_without_a_second_judgement() {
        // 倍率の採否は core が不等式ごと決める。server 側で範囲を判定し直すと、
        // plugin と server が同じファイルから別の結論を得る形ができる。
        let settings = settings_with_scale(50);
        let source = SettingsSource::fixed(settings.clone());
        let server = AviUtl2McpServer::from_limits_source(
            PathBuf::from("registry"),
            LimitsSource::Settings(source),
        );

        let budgets = settings.budgets();
        assert_eq!(server.limits(), CallLimits::from_budgets(budgets));
        assert_eq!(
            server.limits().render_request,
            budgets.server_request_phase(RequestBudgetKind::Render)
        );
        assert_ne!(server.limits(), CallLimits::default());
    }

    /// 倍率を適用した設定を作る。
    fn settings_with_scale(percent: u64) -> aviutl2_mcp_core::settings::Settings {
        settings_from(&format!(r#"{{"budget_scale_percent":{percent}}}"#))
    }

    /// 成果物の保存時間を指定した設定を作る。
    fn settings_with_artifact_ttl(ttl: Duration) -> aviutl2_mcp_core::settings::Settings {
        settings_from(&format!(
            r#"{{"artifact":{{"ttl_seconds":{}}}}}"#,
            ttl.as_secs()
        ))
    }

    /// 設定ファイルの内容から解決済みの設定を作る。
    fn settings_from(text: &str) -> aviutl2_mcp_core::settings::Settings {
        aviutl2_mcp_core::settings::SettingsDocument::parse(text)
            .unwrap()
            .resolve(&aviutl2_mcp_core::settings::Settings::default())
            .0
    }

    #[test]
    fn call_limits_can_be_overridden() {
        let limits = CallLimits {
            resolve: Duration::from_millis(120),
            request: Duration::from_millis(340),
            edit_request: Duration::from_millis(560),
            batch_request: Duration::from_millis(780),
            render_request: Duration::from_millis(910),
            artifact_ingest: Duration::from_millis(130),
        };
        let server = AviUtl2McpServer::without_artifact_store(PathBuf::from("registry"), limits);
        assert_eq!(server.limits().resolve, Duration::from_millis(120));
        assert_eq!(server.limits().request, Duration::from_millis(340));
        assert_eq!(server.limits().edit_request, Duration::from_millis(560));
        assert_eq!(server.limits().batch_request, Duration::from_millis(780));
        assert_eq!(server.limits().render_request, Duration::from_millis(910));
        assert_eq!(server.limits().artifact_ingest, Duration::from_millis(130));
    }

    /// 区分ごとの取り違えが必ず落ちるよう、桁で離した予算。
    fn probe_limits() -> CallLimits {
        CallLimits {
            resolve: Duration::from_millis(1),
            request: Duration::from_millis(2),
            edit_request: Duration::from_millis(3),
            batch_request: Duration::from_millis(4),
            render_request: Duration::from_millis(50),
            artifact_ingest: Duration::from_millis(6),
        }
    }

    #[test]
    fn request_budget_selects_the_limit_matching_the_operation_kind() {
        let limits = probe_limits();

        for name in aviutl2_mcp_core::ReadOperation::ALL
            .into_iter()
            .map(aviutl2_mcp_core::ReadOperation::as_str)
            .chain(["ping", "future_operation"])
        {
            assert_eq!(
                limits.request_phase_budget(name),
                limits.request,
                "{name} が read 予算を使っていません"
            );
        }

        for op in aviutl2_mcp_core::EditOperation::ALL {
            // 一括適用は編集の族に属するが、費用の主項が違うため別の予算を持つ。
            let expected = match op {
                aviutl2_mcp_core::EditOperation::ApplyBatch => limits.batch_request,
                _ => limits.edit_request,
            };
            assert_eq!(
                limits.request_phase_budget(op.as_str()),
                expected,
                "{op:?} の予算が想定と異なります"
            );
        }

        for op in aviutl2_mcp_core::RenderOperation::ALL {
            assert_eq!(
                limits.request_phase_budget(op.as_str()),
                limits.render_request,
                "{op:?} が render 予算を使っていません"
            );
        }
    }

    #[test]
    fn only_the_render_request_reserves_time_for_what_happens_after_the_response() {
        // 描画だけが応答を受けたあとに成果物の引き取りを行う。要求フェーズの
        // 予算をそのまま IPC へ渡すと、接続先が期限いっぱいまで使った直後に
        // 引き取りが始まり、どの層の期限にも捕まらないまま予算を超える。
        let limits = probe_limits();

        for op in aviutl2_mcp_core::RenderOperation::ALL {
            let name = op.as_str();
            assert_eq!(
                limits.ipc_request_budget(name),
                limits.render_request - limits.artifact_ingest,
                "{name} の期限が引き取りの取り分を残していません"
            );
            assert_ne!(
                limits.ipc_request_budget(name),
                limits.render_request,
                "{name} が要求フェーズの予算をそのまま渡しています"
            );
        }

        // 他の operation は応答後の段を持たないため、要求フェーズの予算がその
        // まま期限になる。
        for name in aviutl2_mcp_core::ReadOperation::ALL
            .into_iter()
            .map(aviutl2_mcp_core::ReadOperation::as_str)
            .map(str::to_string)
            .chain(
                aviutl2_mcp_core::EditOperation::ALL
                    .into_iter()
                    .map(|op| op.as_str().to_string()),
            )
            .chain(["ping".to_string(), "future_operation".to_string()])
        {
            assert_eq!(
                limits.ipc_request_budget(&name),
                limits.request_phase_budget(&name),
                "{name} が予算から時間を差し引いています"
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
            "aviutl2://instances/not-a-uuid/edit-info",
            "aviutl2://instances//edit-info",
            "file:///etc/passwd",
            "aviutl2://instances/8df98c04-e7c2-4f98-b3ce-fc1c39d76414",
            // 識別子の無い成果物 URI は指す対象を持たない。
            "aviutl2://artifacts/",
            "aviutl2://artifacts",
        ] {
            assert_eq!(parse_resource_uri(uri), None, "{uri} を受理しています");
        }
    }

    #[test]
    fn an_artifact_uri_is_resolved_by_lookup_alone() {
        // 識別子はパスへ連結しない。どのような文字列が来ても、引き当てに
        // 失敗すれば見つからないで終わる。書式を課す必要が無い。
        for id in [
            "5d0b6f7a-1f2e-4a3b-9c8d-7e6f5a4b3c2d",
            "..",
            "../../windows/system32/config/sam",
            r"..\..\secret.png",
            "a b c",
            "%2e%2e",
        ] {
            assert_eq!(
                parse_resource_uri(&artifact_resource_uri(id)),
                Some(ResourceTarget::Artifact(id.to_string())),
                "{id} を引き当ての対象として扱っていません"
            );
        }
    }

    /// 任意の時刻を指す時計。
    struct FixedClock(std::sync::atomic::AtomicI64);

    impl crate::artifact::ArtifactClock for FixedClock {
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            chrono::DateTime::from_timestamp(self.0.load(std::sync::atomic::Ordering::SeqCst), 0)
                .expect("表現できる時刻")
        }
    }

    /// 成果物を持つ保管庫と、その基底を束ねた試験環境。
    struct StoreFixture {
        base_dir: PathBuf,
        clock: Arc<FixedClock>,
        /// 後始末で基底を消す前に閉じる必要があるため、取り出せる形で持つ。
        store: Option<ArtifactStore>,
        instance_id: InstanceId,
    }

    impl StoreFixture {
        fn open(ttl: Duration) -> Self {
            let base_dir = std::env::temp_dir().join(format!(
                "aviutl2-mcp-resource-test-{}",
                uuid::Uuid::new_v4()
            ));
            let clock = Arc::new(FixedClock(std::sync::atomic::AtomicI64::new(0)));
            let settings = SettingsSource::fixed(settings_with_artifact_ttl(ttl));
            let store = ArtifactStore::open_with(base_dir.clone(), settings, clock.clone())
                .expect("保管庫を開ける");
            Self {
                base_dir,
                clock,
                store: Some(store),
                instance_id: InstanceId::new_v4(),
            }
        }

        fn store(&self) -> &ArtifactStore {
            self.store.as_ref().expect("保管庫は後始末まで生きています")
        }

        /// 引き渡しファイルを書いて成果物として引き取る。
        fn ingest(&self, token: &str, bytes: &[u8]) -> Artifact {
            let dir = self
                .base_dir
                .join("render")
                .join(self.instance_id.to_string());
            std::fs::create_dir_all(&dir).expect("引き渡しディレクトリを作れる");
            std::fs::write(dir.join(format!("{token}.png")), bytes)
                .expect("引き渡しファイルを書ける");

            let mut sha256 = "sha256:".to_string();
            for byte in <sha2::Sha256 as sha2::Digest>::digest(bytes) {
                sha256.push_str(&format!("{byte:02x}"));
            }
            self.store()
                .ingest(&self.instance_id, token, bytes.len() as u64, &sha256)
                .expect("申告と一致する引き渡しは引き取れます")
        }

        fn advance(&self, seconds: i64) {
            self.clock
                .0
                .fetch_add(seconds, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl Drop for StoreFixture {
        fn drop(&mut self) {
            drop(self.store.take());
            let _ = std::fs::remove_dir_all(&self.base_dir);
        }
    }

    /// 有効な引き渡しの識別子を種から作る。
    fn handoff_token(seed: u8) -> String {
        format!("{seed:02x}").repeat(16)
    }

    #[test]
    fn a_listing_without_artifacts_keeps_the_shape_it_had_before() {
        // 成果物を持たない場合、並びも cursor も成果物を導入する前と変わらない。
        let registered: Vec<InstanceId> = (0..RESOURCES_PAGE_SIZE + 5)
            .map(|_| InstanceId::new_v4())
            .collect();

        let (first, cursor) = resource_page(&registered, &[], 0);
        // 先頭ページはインスタンス一覧そのものを含む。
        assert_eq!(first.len(), RESOURCES_PAGE_SIZE + 1);
        assert_eq!(first[0].uri, INSTANCES_RESOURCE_URI);
        assert_eq!(
            first[1].uri,
            edit_info_resource_uri(&registered[0]),
            "instance 由来の項目が先に来ていません"
        );
        assert_eq!(cursor.as_deref(), Some("100"));

        let (second, cursor) = resource_page(&registered, &[], 100);
        assert_eq!(second.len(), 5);
        assert!(
            second.iter().all(|item| item.uri != INSTANCES_RESOURCE_URI),
            "2 ページ目に一覧そのものが現れています"
        );
        assert_eq!(cursor, None);

        // 範囲外の位置は空のページになる。
        let (empty, cursor) = resource_page(&registered, &[], 1_000);
        assert!(empty.is_empty());
        assert_eq!(cursor, None);
    }

    #[test]
    fn artifacts_are_listed_after_the_instances() {
        let fixture = StoreFixture::open(Duration::from_secs(600));
        let first = fixture.ingest(&handoff_token(1), b"first");
        let second = fixture.ingest(&handoff_token(2), b"second");
        let registered = vec![InstanceId::new_v4()];

        let artifacts = fixture.store().list();
        let (page, cursor) = resource_page(&registered, &artifacts, 0);
        assert_eq!(cursor, None);
        let uris: Vec<&str> = page.iter().map(|item| item.uri.as_str()).collect();
        assert_eq!(
            uris,
            vec![
                INSTANCES_RESOURCE_URI,
                &edit_info_resource_uri(&registered[0]),
                &artifact_resource_uri(&first.artifact_id),
                &artifact_resource_uri(&second.artifact_id),
            ],
        );

        // cursor は連結した一覧への位置であり、成果物までまたぐ。
        let (page, cursor) = resource_page(&registered, &artifacts, 1);
        assert_eq!(cursor, None);
        assert_eq!(
            page.iter()
                .map(|item| item.uri.as_str())
                .collect::<Vec<_>>(),
            vec![
                artifact_resource_uri(&first.artifact_id).as_str(),
                artifact_resource_uri(&second.artifact_id).as_str(),
            ],
        );
    }

    #[test]
    fn an_artifact_listing_says_nothing_about_what_the_image_shows() {
        let fixture = StoreFixture::open(Duration::from_secs(600));
        let artifact = fixture.ingest(&handoff_token(3), b"image");
        let listed = artifact_resource(&artifact);

        assert_eq!(listed.mime_type.as_deref(), Some("image/png"));
        assert!(
            listed.name.contains(&artifact.artifact_id),
            "{}",
            listed.name
        );
        let description = listed.description.clone().expect("説明がある");
        assert!(
            description.contains(&artifact.created_at.to_rfc3339()),
            "{description}"
        );
        assert!(
            description.contains(&artifact.expires_at.to_rfc3339()),
            "{description}"
        );
        // 引き当てに要らない値を漏らさない。
        for forbidden in [artifact.sha256.as_str(), "render", "png\\"] {
            assert!(
                !description.contains(forbidden),
                "{forbidden} が説明にあります: {description}"
            );
        }
    }

    #[test]
    fn an_expired_artifact_and_an_unknown_id_are_both_simply_missing() {
        // 区別すると、過去に存在した識別子を総当たりで調べられる。
        let fixture = StoreFixture::open(Duration::from_secs(60));
        let artifact = fixture.ingest(&handoff_token(4), b"image");
        assert!(fixture.store().read(&artifact.artifact_id).is_some());

        fixture.advance(61);
        assert!(fixture.store().read(&artifact.artifact_id).is_none());
        assert!(fixture.store().read("unknown-artifact").is_none());
        assert!(fixture.store().list().is_empty(), "期限切れが残っています");

        // どちらも同じ失敗として返る。
        let error = to_mcp_error(&artifact_not_found());
        assert_eq!(error.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
    }

    #[test]
    fn artifact_bytes_are_encoded_as_standard_base64() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"\x89PNG\r\n\x1a\n"), "iVBORw0KGgo=");
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
            failure::from_pipe_error(
                &crate::pipe_client::PipeClientError::Remote(Box::new(remote)),
                aviutl2_mcp_core::OPERATION_MOVE_OBJECT,
            ),
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
