//! MCP stdio サーバーの本体。
//!
//! tool call は 1 回ごとに接続を確立し、応答を受け取ったら破棄する。
//! [`crate::pipe_client::PipeClient`] は生のハンドルを持ち `!Send` であるため、
//! インスタンス解決から要求送信・切断までを 1 つのブロッキングタスクへ閉じ込め、
//! 非同期タスク間で接続が移動しないようにする。

use crate::api::{ListInstancesResponse, list_instances};
use crate::artifact::{Artifact, ArtifactStore, ArtifactStoreError, base_dir_for_registry};
use crate::discovery::{DiscoveryConfig, list_registered_instances, resolve_instance};
use crate::mcp::edit_input::{
    AddEffectInput, ApplyBatchInput, CreateObjectInput, CreateObjectSectionInput,
    DeleteEffectInput, DeleteObjectInput, DeleteObjectSectionInput, MoveEffectInput,
    MoveObjectInput, MoveObjectSectionInput, SetEffectEnabledInput, SetGridBpmInput,
    SetLayerStateInput, SetObjectItemInput, SetObjectNameInput, SetSceneSettingsInput,
    SetSelectionInput,
};
use crate::mcp::input::{
    DescribeEffectsInput, GetEffectItemValuesInput, GetObjectInput, GetSelectionInput,
    InstanceInput, ListAvailableEffectsInput, ListFontsInput, ListInstancesInput, ListLayersInput,
    ListModulesInput, ListObjectAliasesInput, ListObjectsInput, ListPalettesInput,
    parse_instance_id,
};
use crate::mcp::render::{RenderFrameInput, RenderFrameOutput};
use crate::mcp::summary::{MAX_TEXT_CHARS, clamp_chars};
use crate::mcp::tool_catalog::{ToolListWatch, ToolVisibility};
use crate::mcp::{describe, failure};
use crate::redact;
use crate::settings::SettingsSource;
use aviutl2_mcp_core::{
    BatchOutcome, DescribeEffectsResult, EditInfo, EditOutcome, EffectItemValues, ErrorCode,
    ErrorObject, GetCurrentSceneParams, GetCurrentSceneResult, GetEditInfoParams, GridBpmOutcome,
    InstanceId, LayerStateOutcome, ListAvailableEffectsResult, ListFontsResult, ListLayersResult,
    ListModulesResult, ListObjectAliasesResult, ListObjectsResult, ListPalettesResult,
    MAX_PAGE_LIMIT, OPERATION_ADD_EFFECT, OPERATION_APPLY_BATCH, OPERATION_CREATE_OBJECT,
    OPERATION_CREATE_OBJECT_SECTION, OPERATION_DELETE_EFFECT, OPERATION_DELETE_OBJECT,
    OPERATION_DELETE_OBJECT_SECTION, OPERATION_DESCRIBE_EFFECTS, OPERATION_GET_CURRENT_SCENE,
    OPERATION_GET_EDIT_INFO, OPERATION_GET_EFFECT_ITEM_VALUES, OPERATION_GET_OBJECT,
    OPERATION_GET_SELECTION, OPERATION_LIST_AVAILABLE_EFFECTS, OPERATION_LIST_FONTS,
    OPERATION_LIST_LAYERS, OPERATION_LIST_MODULES, OPERATION_LIST_OBJECT_ALIASES,
    OPERATION_LIST_OBJECTS, OPERATION_LIST_PALETTES, OPERATION_MOVE_EFFECT, OPERATION_MOVE_OBJECT,
    OPERATION_MOVE_OBJECT_SECTION, OPERATION_RENDER_FRAME, OPERATION_SET_EFFECT_ENABLED,
    OPERATION_SET_GRID_BPM, OPERATION_SET_LAYER_STATE, OPERATION_SET_OBJECT_ITEM,
    OPERATION_SET_OBJECT_NAME, OPERATION_SET_SCENE_SETTINGS, OPERATION_SET_SELECTION, ObjectDetail,
    ObjectSectionsOutcome, RenderFrameResult, RequestBudgetKind, ScaledBudgets,
    SceneSettingsOutcome, SelectionSnapshot, SelectionState, request_budget_kind,
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
///
/// **持つのは要求フェーズの期限だけである。** インスタンス解決フェーズの配分は
/// [`DiscoveryConfig`] が持つ。接続待ちの内訳をここへ混ぜると、要求フェーズの表と
/// 解決フェーズの内訳を 1 つの型が兼ねることになる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallLimits {
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
    settings: SettingsOrFixed,
    tool_router: ToolRouter<Self>,
}

/// tool call 1 回分の期限一式。
///
/// 要求フェーズと解決フェーズは別々の型が持つが、**同じ設定の snapshot から
/// 導く**。両者が別の snapshot から来ると、要求へ載せる期限と接続待ちの配分が
/// 噛み合わない組になり得る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CallBudgets {
    /// 要求フェーズの期限。
    limits: CallLimits,
    /// インスタンス解決フェーズの配分。
    discovery: DiscoveryConfig,
}

/// 実行予算と tool の公開の出所。
///
/// 設定を持つ場合は tool call のたびに現在の snapshot から引く。設定を持たない
/// 構築口は固定値を使う——予算を明示して振る舞いを観測する試験のためであり、
/// **製品の経路では使わない。** その場合、tool の公開は既定（全 tool 有効）に
/// なる。
#[derive(Clone)]
enum SettingsOrFixed {
    /// 構築時に与えられた固定値。
    ///
    /// この腕を作れるのは試験の構築口だけであり、それらは `test-support` の
    /// 下にある。**製品ビルドでは構築されない。**
    #[cfg_attr(not(any(test, feature = "test-support")), expect(dead_code))]
    Fixed(CallLimits),
    /// 共有設定から引く。
    Settings(Arc<SettingsSource>),
}

impl SettingsOrFixed {
    /// tool call 1 回分の期限。
    ///
    /// **設定は 1 度だけ引く。** 要求フェーズと解決フェーズを別々に引くと、
    /// 2 回の読み取りの間に設定が差し替わったとき、1 回の tool call が別々の
    /// snapshot から採った期限で走る。
    fn call_budgets(&self) -> CallBudgets {
        match self {
            // 固定の構築口は倍率を持たないため、解決は倍率を掛けない配分で行う。
            Self::Fixed(limits) => CallBudgets {
                limits: *limits,
                discovery: DiscoveryConfig::default(),
            },
            Self::Settings(source) => {
                let budgets = source.settings().budgets();
                CallBudgets {
                    limits: CallLimits::from_budgets(budgets),
                    discovery: DiscoveryConfig::from_budgets(budgets),
                }
            }
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
            ..Self::from_settings_or_fixed(registry_dir, SettingsOrFixed::Settings(settings))
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
    /// **`render_frame` は使えない。** 呼ぶと成果物を保管できないため
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
        Self::from_settings_or_fixed(registry_dir, SettingsOrFixed::Fixed(limits))
    }

    /// 保管庫を持たず、共有設定から予算と tool の公開を引くサーバーを作る。
    ///
    /// **保管庫の用意を伴わずに設定の効き方を観測するための構築口であり、製品の
    /// 経路では使わない。** 既定では公開しないため、`.exe` にこの経路は無い。
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_settings(registry_dir: PathBuf, settings: Arc<SettingsSource>) -> Self {
        Self::from_settings_or_fixed(registry_dir, SettingsOrFixed::Settings(settings))
    }

    fn from_settings_or_fixed(registry_dir: PathBuf, settings: SettingsOrFixed) -> Self {
        Self {
            registry_dir: Arc::new(registry_dir),
            artifacts: None,
            settings,
            tool_router: Self::tool_router(),
        }
    }

    /// tool call 1 回分の期限一式。
    fn call_budgets(&self) -> CallBudgets {
        self.settings.call_budgets()
    }

    /// tool call 1 回分の実行予算。
    fn limits(&self) -> CallLimits {
        self.call_budgets().limits
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
        self.settings.visibility()
    }

    /// 現在公開している tool 定義を返す。
    ///
    /// `tools/list` が返すのはこの一覧である。tool の定義そのものは router が
    /// 持つものをそのまま使う——説明と schema の出所を 2 つにしない。
    fn visible_tools(&self) -> Vec<Tool> {
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
    fn accepts_tool_call(&self, name: &str) -> bool {
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
    /// 他の一覧 tool と異なり結果は page オブジェクトを持たず、
    /// 件数と続きは instances と同じ階層に並ぶ。snapshot_revision の概念も無い。
    /// 生存確認は実行中の要求と競合し得るため、稼働中のインスタンスが
    /// その回の一覧から一時的に外れることがある。期待した instance_id が
    /// 見つからない場合は取り直す。
    #[tool(
        name = "list_instances",
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
    pub async fn list_instances(
        &self,
        Parameters(input): Parameters<ListInstancesInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let discovery = self.call_budgets().discovery;
        self.run("list_instances", move || {
            let response = collect_instances(&registry_dir, input, discovery)?;
            Ok(ToolSuccess {
                text: describe::instances(&response),
                structured: to_structured(&response)?,
            })
        })
        .await
    }

    /// 現在の編集情報（シーン・カーソル・表示範囲・選択範囲・revision）を取得する。
    #[tool(
        name = "get_edit_info",
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
    pub async fn get_edit_info(
        &self,
        Parameters(input): Parameters<InstanceInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("get_edit_info", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let result: EditInfo = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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
    #[tool(
        name = "get_current_scene",
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
    pub async fn get_current_scene(
        &self,
        Parameters(input): Parameters<InstanceInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("get_current_scene", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let result: GetCurrentSceneResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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
    #[tool(
        name = "list_layers",
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
    pub async fn list_layers(
        &self,
        Parameters(input): Parameters<ListLayersInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("list_layers", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListLayersResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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
    #[tool(
        name = "list_objects",
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
    pub async fn list_objects(
        &self,
        Parameters(input): Parameters<ListObjectsInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("list_objects", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListObjectsResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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
    /// effect の locked は出力項目（標準描画等）については実態を反映せず、
    /// 常に false になる。ロックは入力項目と出力項目をまとめた単位で掛かる。
    #[tool(
        name = "get_object",
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
    pub async fn get_object(
        &self,
        Parameters(input): Parameters<GetObjectInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("get_object", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ObjectDetail = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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

    /// いま選ばれているオブジェクトを取得する。set_selection の読み取り側である。
    /// focus と selected は別物である。
    /// focus はオブジェクト設定ウィンドウで選択されている 1 件、
    /// selected はタイムライン上で選択されている一覧であり、両者は一致しない。
    /// focus_section は focus の区間番号であり、区間番号 i は
    /// get_object が返す sections[i] を指す。focus が null のとき focus_section も null である。
    /// selected は layer 番号・frame_start の昇順で並び、list_objects と同じ並びである。
    /// ページ指定が掛かるのは selected だけであり、focus には掛からない。
    /// 編集カーソルとフレーム範囲選択は返さない。どちらも get_edit_info が返す。
    #[tool(
        name = "get_selection",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::get_selection()
        )
    )]
    pub async fn get_selection(
        &self,
        Parameters(input): Parameters<GetSelectionInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("get_selection", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: SelectionSnapshot = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_GET_SELECTION,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::selection(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// インスタンスが利用できる effect の一覧を取得する。
    /// 1 件につき名前・種別・対応フラグ・設定項目の数・説明を返す。
    /// description はホストが同梱する説明であり、持たない effect は null になる。空欄を推測で補わない。
    /// 設定項目の名前は返さない。対象へ付与したあと get_object を呼べば、項目名が現在値付きで得られる。
    #[tool(
        name = "list_available_effects",
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
    pub async fn list_available_effects(
        &self,
        Parameters(input): Parameters<ListAvailableEffectsInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("list_available_effects", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListAvailableEffectsResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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

    /// 名前で指定した effect の中身を取得する。
    /// effect_names には list_available_effects が返す名前を 1〜10 件指定する。
    /// 1 件につき name・description・items（name / item_type / description / choices / range / group）を返す。
    /// 設定項目の一覧はホストの列挙から得るため、必ず実際の effect と一致する。
    /// description はホストが同梱する説明であり、持たない effect と持たない項目は null になる。
    /// 説明を持たない effect は多く、とくにフィルタ効果はほとんどが null である。
    /// 空欄を推測で補わない。名前が似ている effect の使い分けは、説明ではなく
    /// items の顔ぶれで判断する。
    /// choices は選択肢の候補（values と source: builtin_table / sidecar）、range は値域と
    /// 小数桁（min・max・decimals と source）である。持たない項目は null になり、
    /// range の 3 つの値は測れた側だけが載るため個別に null になる。
    /// どちらもヒントであってゲートではない。候補に無い値でも書き込みは通り、
    /// 値域を外れる値でも書き込みは通る。候補に在る値が必ず通るとも限らない。
    /// 可否を決めるのはホストである。
    /// range は書き込む値に掛かるヒントであり、評価値の上下界ではない。
    /// 移動方法によっては、区間の境界へ書いた値が値域の内側でも、
    /// 途中のフレームの評価値が値域の外へ出る。
    /// group は設定項目が属するグループ（index と item_names）であり、座標の X / Y / Z の
    /// ように 1 つの組を成す項目を示す。属さない項目は null になる。
    /// このグループは名前を持たない。get_effect_item_values の text が示す group=<名前> は
    /// トラックバーのグループ名であり、別のものである。
    /// グループを引けなかった場合は要求全体が失敗する。null が返るのは属さない項目だけである。
    /// 登録されていない名前は not_found に並び、その名前だけが落ちる。
    /// 要求全体は失敗しないため、effects に無い名前は not_found を必ず確認すること。
    /// not_found に出た名前は綴りが違うだけであり、設定項目を持たない effect ではない。
    /// 設定項目の現在値は返さない。対象へ付与したあと get_object を呼べば現在値が得られる。
    /// ページ指定を持たない。返すのは指定した名前の分だけであり、続きのページという
    /// 概念が無いためである。
    #[tool(
        name = "describe_effects",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::describe_effects()
        )
    )]
    pub async fn describe_effects(
        &self,
        Parameters(input): Parameters<DescribeEffectsInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("describe_effects", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: DescribeEffectsResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_DESCRIBE_EFFECTS,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::effect_descriptions(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// インスタンスが利用できるフォント名の一覧を取得する。
    /// いずれも font 種別の設定項目へそのまま指定できる名前である。
    /// 名前による絞り込みは持たない。total_count で全体の件数が分かる。
    #[tool(
        name = "list_fonts",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_fonts()
        )
    )]
    pub async fn list_fonts(
        &self,
        Parameters(input): Parameters<ListFontsInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("list_fonts", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListFontsResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_LIST_FONTS,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::fonts(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// インスタンスが利用できるパレットの一覧と、各パレットの色を取得する。
    /// colors は常に 64 件であり、a は常に 255 である。
    /// つまりパレットは透明度の情報を持たない。
    /// current は現在のパレット名であり、ラベル付きの場合は [ラベル名.パレット名] の形式になる。
    /// 取得できない場合は null となるが、一覧はそのまま返る。
    /// 色を読み取れなかったパレットは一覧から除かれる。
    /// total_count から引かれるのは本ページで落とした分だけであり、
    /// 落ちたページとそうでないページで値が違い得る。全体の件数として扱わないこと。
    /// ページ内のすべてが落ちると items が空のまま has_more が true になり得る。
    /// 反復は items が空になったことではなく has_more と next_offset で終端すること。
    #[tool(
        name = "list_palettes",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_palettes()
        )
    )]
    pub async fn list_palettes(
        &self,
        Parameters(input): Parameters<ListPalettesInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("list_palettes", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListPalettesResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_LIST_PALETTES,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::palettes(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// インスタンスへ登録されているスクリプトとプラグインの一覧を取得する。
    /// information はホストが利用者へ表示する説明文である。
    /// 一覧には既知の 9 種別だけが現れる。
    /// 種別を解釈できないモジュールは一覧から欠落し得る。
    #[tool(
        name = "list_modules",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_modules()
        )
    )]
    pub async fn list_modules(
        &self,
        Parameters(input): Parameters<ListModulesInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("list_modules", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListModulesResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_LIST_MODULES,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::modules(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// インスタンスへ登録されているオブジェクトエイリアスの一覧を取得する。
    /// name はエイリアスの名前であり、create_object の alias_name へそのまま渡す値である。
    /// 一覧に出た名前は必ず作成できる。逆は保証しない。
    /// エイリアスの中身は返さない。返すのは name・label・object_count・effects だけである。
    /// label は AviUtl2 の UI 状態ファイル由来であり、欠けることがあり、
    /// 実行中の表示と一致しないことがある。
    /// label は識別子ではなく、複数のエイリアスが同じ label を共有し得る。
    /// 読み取れなかったエイリアスは一覧から除かれる。
    /// total_count から引かれるのは本ページで落とした分だけであり、
    /// 落ちたページとそうでないページで値が違い得る。全体の件数として扱わないこと。
    /// ページ内のすべてが落ちると items が空のまま has_more が true になり得る。
    /// 反復は items が空になったことではなく has_more と next_offset で終端すること。
    /// エイリアスの登録・削除・編集は AviUtl2 の UI で行う。この server は読み取りだけを提供する。
    /// AviUtl2 のデータディレクトリを解決できない環境では unsupported_operation となる。
    #[tool(
        name = "list_object_aliases",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::list_object_aliases()
        )
    )]
    pub async fn list_object_aliases(
        &self,
        Parameters(input): Parameters<ListObjectAliasesInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("list_object_aliases", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ListObjectAliasesResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_LIST_OBJECT_ALIASES,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::object_aliases(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// effect の設定項目を、指定したフレームで評価した値を取得する。
    /// frames は get_object が返した frame_start / frame_end と同じ座標であり、
    /// オブジェクトの範囲外を指定すると precondition_failed（frame_out_of_range）となる。
    /// frames に小数を指定するとフレーム間の位置を指し、中間点・加減速・時間制御を
    /// 含む補間後の値が返る。トラックバー項目は小数部をそのまま使い、
    /// チェックボックス項目は整数部を使う。
    /// items を省略すると effect のトラックバー項目とチェックボックス項目すべてが
    /// 対象になり、上限を超えた分は打ち切られて truncated が true になる。
    /// items に指定した名前が effect に無ければ not_found（target_missing）、
    /// 名前はあるが評価できない種別なら unsupported_operation（item_not_evaluatable）となる。
    /// トラックバーグループの count はグループのトラック数、item_names は所属アイテム名で
    /// あり、両者の件数は一致しない場合がある。
    /// 各項目の values は frames と同じ長さ・同じ順序で並ぶ。
    #[tool(
        name = "get_effect_item_values",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::effect_item_values()
        )
    )]
    pub async fn get_effect_item_values(
        &self,
        Parameters(input): Parameters<GetEffectItemValuesInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("get_effect_item_values", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EffectItemValues = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_GET_EFFECT_ITEM_VALUES,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::effect_item_values(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// メディアファイル・object alias・エフェクト名・登録済みエイリアス名のいずれかから
    /// オブジェクトを作成する。
    /// source が effect のとき、カタログに在る名前でも作成元にできるとは限らず、
    /// その場合は unsupported_operation（effect_not_creatable）となる。
    /// 名前がカタログに無い場合は unsupported_operation（effect_not_registered）となる。
    /// 表として読めないエイリアスは、source が alias_name でも object_alias でも
    /// invalid_argument（alias_not_parsable）で拒否される。
    /// source が alias_name のとき、effect を 1 つも含まないエイリアスは
    /// invalid_argument（alias_without_effect）で拒否される。
    /// source が object_alias のとき、移動行は設定項目へ書くときと同じ検証を通り、通らない行は
    /// invalid_argument（track_flags_not_representable / track_mode_unknown / track_mode_not_writable / track_value_count）で拒否される。
    /// source が object_alias のとき、テキスト種別（text / string）の設定項目の行は `\` の綴りを検査され、
    /// `\` の次が `n` でも `\` でもない行は invalid_argument（unescaped_backslash）で拒否される。
    /// 行の拒否は details.item に項目名を載せ、節に属する行では details.heading に節の見出しを載せる。
    /// これらの拒否はいずれも作成より前に起き、オブジェクトは 1 つも作られない。
    /// 複数オブジェクトを含む alias は全てが作成され、created に全件、object に
    /// その先頭が入る。応答の effect は常に null である。
    /// 長さと挿入位置はホストが自動調整し得るため、
    /// 応答が返す位置は要求した宛先と異なり得る。
    /// 応答が返す selector が実際の配置であり、配置を確かめるには応答の値を見る。
    /// 同じ要求を再送すると重複して作成し得る。作成先に既存オブジェクトがあれば
    /// precondition_failed（destination_occupied）となるため通常は防がれるが、
    /// ホストが挿入位置を自動調整した場合はすり抜け得る。
    /// 配置先のレイヤーがロックされている場合は precondition_failed（layer_locked）と
    /// なる。set_layer_state でロックを解除してから再実行する。
    #[tool(
        name = "create_object",
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
    pub async fn create_object(
        &self,
        Parameters(input): Parameters<CreateObjectInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("create_object", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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
    /// 配置はホストが調整し得るため、応答が返す位置は要求した宛先と異なり得る。
    /// 応答が返す selector が実際の配置であり、配置を確かめるには応答の値を見る。
    /// 宛先に既存オブジェクトがある場合は precondition_failed（destination_occupied）となる。
    /// 移動元または移動先のレイヤーがロックされている場合は
    /// precondition_failed（layer_locked）となる。set_layer_state で
    /// ロックを解除してから再実行する。
    #[tool(
        name = "move_object",
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
    pub async fn move_object(
        &self,
        Parameters(input): Parameters<MoveObjectInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("move_object", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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
    #[tool(
        name = "set_object_name",
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
    pub async fn set_object_name(
        &self,
        Parameters(input): Parameters<SetObjectNameInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("set_object_name", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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
    /// 書き込みを公開していない設定項目種別があり、その場合は unsupported_operation
    /// となる。種別は get_object の item_type で確認できる。
    /// 設定項目の種別が値の形を受け付けない場合は invalid_argument となり、
    /// details.item_type に設定項目の種別が、details.value_kind に与えた値の形が入る。
    /// トラックバー以外の設定項目へ track を書いた場合もこれになる。
    /// 移動を持つトラックバーへ number や integer を書く要求は unsupported_operation
    /// となり details.reason は track_movement_present になる。書けば移動もその
    /// パラメータも消えるためであり、消したい場合は mode を null にした track を送る。
    /// details.current_value にホストが現在保持している値が入り、書き込みは発行されない。
    /// current_value はそのまま送り返せる形ではない。移動を書き戻すには、読み取った
    /// 値ではなく get_object が返す track の形で組み直す。
    /// 移動を持たないトラックバーへ track を書く要求は通り、新しく移動が付く。
    /// 書き込みは全ての種別で、書いた直後に読み直して要求した値が入ったかを照合する。
    /// 入っていなければ unsupported_operation となり details.reason は
    /// item_value_not_applied、details.observed_value に書き込んだ直後に読み直した値が入る。
    /// この失敗では書き込みは既に発行済みだが、設定項目は書き込み前の値へ戻す。
    /// 戻せたかは details.restored が名乗り、
    /// 戻せなかった場合だけ details.consistency_unknown が true になる。
    /// 戻せていれば selector はそのまま使え、対象を読み直す必要は無い。
    /// このとき details.retry_requires は none になる。
    /// observed_value は応答が返る時点の現在値ではなく、要求の代わりに送り直す値でもない。
    /// 要求した値がホストに受け付けられなかったと解し、受け付けられる値を選び直す。
    /// 選択肢から選ぶ種別（select・combo・mask・figure）で選択肢に無い値、登録されていない
    /// フォント名、書式の合わない色はいずれもこの失敗になる。
    /// 数値が値域を外れてクランプされた場合と、小数が項目の桁数へ丸められた場合も
    /// 同じ失敗になる。ホストが値を調整したことと拒否したことは区別できないため、
    /// 要求した値を得られていない点で同じ扱いにする。
    #[tool(
        name = "set_object_item",
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
    pub async fn set_object_item(
        &self,
        Parameters(input): Parameters<SetObjectItemInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("set_object_item", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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
    /// effect_name には list_available_effects が返す名前を指定する。
    /// 登録されていない名前は unsupported_operation となる。
    /// 同じ要求を再送すると重複して付与し得る。付与によってオブジェクトの
    /// fingerprint が変わるため、同じ selector での再送は precondition_failed と
    /// なり防がれる。
    /// effect の増減は、同じオブジェクトが持つ他の effect の selector も無効にする。
    /// 応答が返すのは付与した effect の selector だけであるため、
    /// 兄弟 effect を続けて編集するには get_object を引き直す。
    #[tool(
        name = "add_effect",
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
    pub async fn add_effect(
        &self,
        Parameters(input): Parameters<AddEffectInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("add_effect", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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
    /// 応答の effect には変更後に読み直した effect が入る。
    #[tool(
        name = "set_effect_enabled",
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
    pub async fn set_effect_enabled(
        &self,
        Parameters(input): Parameters<SetEffectEnabledInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("set_effect_enabled", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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

    /// effect を effect 列の別の位置へ動かす。
    /// position は列全体での 0 始まりの位置であり、get_object の effects 配列の
    /// 添字と同じ数え方である。同名 effect の順序を表す effect_index とは別の値である。
    /// 順序を動かせるのはフィルタ効果だけであり、入力 item・出力 item は
    /// unsupported_operation（effect_not_movable）となる。
    /// position が effect の件数以上の場合は precondition_failed
    /// （effect_position_out_of_range）となり、変更は発行されない。
    /// 下限は振る舞いが違う。フィルタ効果は先頭に並ぶ入力 item・出力 item より
    /// 前へは動けず、そこを指した position は発行されたうえでホストが切り詰める。
    /// 結果は unsupported_operation（change_not_applied）であり、
    /// details.reported_position にホストが名乗った位置が入る。
    /// 切り詰めで列が動いた場合は元の並びへ戻す。details.restored が真なら列は
    /// 要求の前と同じであり、このとき details.retry_requires は none になる。
    /// 対象が既に下限に居て列が 1 件も動かなかった場合も details.restored は真になる。
    /// 列が動いていない失敗では要求に使った selector がそのまま通る。
    /// details.restored が偽なら戻せておらず details.consistency_unknown が立つ。
    /// 応答の effect には移動後に読み直した effect が入る。
    /// 成功して列の位置が変われば、要求に使った selector は使えなくなる——
    /// fingerprint が変わり、同名 effect があれば effect_index も入れ替わる。
    /// 続けて同じ effect を編集する場合は応答の effect.selector を使う。
    /// 移動は間にある effect の位置もずらすため、兄弟 effect を編集するには
    /// get_object を引き直す。
    #[tool(
        name = "move_effect",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::move_effect()
        )
    )]
    pub async fn move_effect(
        &self,
        Parameters(input): Parameters<MoveEffectInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("move_effect", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_MOVE_EFFECT,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::move_effect(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// オブジェクトから effect を削除する。
    /// 対象が既に失われている場合は not_found となり、追加の変更は起きない。
    /// 応答は effect を返さない（常に null）。
    /// effect の増減は、同じオブジェクトが持つ他の effect の selector も無効にする。
    /// 消した effect だけでなく兄弟 effect も指し直せなくなるため、
    /// 続けて編集するには get_object を引き直す。
    #[tool(
        name = "delete_effect",
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
    pub async fn delete_effect(
        &self,
        Parameters(input): Parameters<DeleteEffectInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("delete_effect", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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
    /// 他の編集 tool と異なり、応答は対象を返さない。削除した対象の selector は
    /// 以後どの編集にも使えない。
    /// 対象のレイヤーがロックされている場合は precondition_failed（layer_locked）と
    /// なる。set_layer_state でロックを解除してから再実行する。
    #[tool(
        name = "delete_object",
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
    pub async fn delete_object(
        &self,
        Parameters(input): Parameters<DeleteObjectInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("delete_object", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: EditOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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

    /// オブジェクトへ中間点を追加し、区間を 1 つ増やす。
    /// frame は中間点を置くシーンの絶対フレーム番号であり、オブジェクト内の相対位置
    /// ではない。get_object が返した sections の値をそのまま基準に使える。
    /// 応答の sections は変更後の区間の一覧であり、get_object が返すものと同じ形である。
    /// 区間の番号と中間点の番号は 1 つずれる。sections[i] が区間番号 i であり、
    /// i が 1 以上のとき sections[i].start が i 番目の中間点のフレームである。
    /// sections[0].start はオブジェクトの開始フレームであって中間点ではないため、
    /// 区間 0 は delete_object_section でも move_object_section でも指定できない。
    /// sections の末尾の end はオブジェクトの終了フレームである。
    /// frame がオブジェクトの範囲外なら precondition_failed（frame_outside_object）、
    /// 既に区間の開始フレームなら precondition_failed（section_boundary_exists）となる。
    /// 同じ要求を再送しても中間点は重複しない。2 回目は section_boundary_exists で
    /// 落ち、状態は 1 回目と同じである。
    /// 対象のレイヤーがロックされている場合は precondition_failed（layer_locked）と
    /// なる。set_layer_state でロックを解除してから再実行する。
    #[tool(
        name = "create_object_section",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::create_object_section()
        )
    )]
    pub async fn create_object_section(
        &self,
        Parameters(input): Parameters<CreateObjectSectionInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("create_object_section", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ObjectSectionsOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_CREATE_OBJECT_SECTION,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::object_sections("中間点を追加しました", &result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// オブジェクトの中間点を 1 つ削除し、前後の区間を 1 つにまとめる。
    /// section に 0 を指定すると invalid_argument（section_index_out_of_range）となる。
    /// section が区間の数以上なら precondition_failed（section_index_out_of_range）となる。
    /// 同じ事実でも、常に誤りである 0 は invalid_argument、対象の現在の状態に
    /// よって決まる範囲外は precondition_failed になる。
    /// 削除した中間点の移動パラメータは失われ、create_object_section で同じ
    /// フレームへ中間点を戻しても元の値には戻らない。
    /// 応答の sections は変更後の区間の一覧であり、get_object が返すものと同じ形である。
    /// sections の末尾の end はオブジェクトの終了フレームである。
    /// 対象のレイヤーがロックされている場合は precondition_failed（layer_locked）と
    /// なる。set_layer_state でロックを解除してから再実行する。
    #[tool(
        name = "delete_object_section",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::delete_object_section()
        )
    )]
    pub async fn delete_object_section(
        &self,
        Parameters(input): Parameters<DeleteObjectSectionInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("delete_object_section", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ObjectSectionsOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_DELETE_OBJECT_SECTION,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::object_sections("中間点を削除しました", &result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// オブジェクトの中間点を別のフレームへ移す。
    /// frame は移動先のシーンの絶対フレーム番号であり、オブジェクト内の相対位置ではない。
    /// section に 0 を指定すると invalid_argument（section_index_out_of_range）となる。
    /// sections の末尾の end はオブジェクトの終了フレームである。
    /// 中間点は隣の中間点を追い越せない。移動できるのは sections[section-1].start より後、
    /// sections[section+1].start より前（無ければオブジェクトの終了フレームまで）であり、
    /// 外れると precondition_failed（section_move_crosses_boundary）となる。
    /// section が区間の数以上なら precondition_failed（section_index_out_of_range）となる。
    /// 応答の sections は変更後の区間の一覧であり、get_object が返すものと同じ形である。
    /// 対象のレイヤーがロックされている場合は precondition_failed（layer_locked）と
    /// なる。set_layer_state でロックを解除してから再実行する。
    #[tool(
        name = "move_object_section",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::move_object_section()
        )
    )]
    pub async fn move_object_section(
        &self,
        Parameters(input): Parameters<MoveObjectSectionInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("move_object_section", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: ObjectSectionsOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_MOVE_OBJECT_SECTION,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::object_sections("中間点を移動しました", &result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// レイヤーの名前・表示・ロック状態を変更する。
    /// name と enabled と locked の 3 つ全てを省略した要求は受け付けない。
    /// name に空の名前を指定すると invalid_argument となる。標準名へ戻すには reset を指定する。
    /// レイヤーには fingerprint が無いため、読み取った時点から状態が変わっていても
    /// 検出できない。応答が返す layer には変更後に読み直した実際の状態が入るので、
    /// 意図どおりかはその値で確認する。
    /// レイヤーのロックが止める範囲は AviUtl2 が決めており、オブジェクトの削除と
    /// 時間軸上の移動にとどまらない。MCP では move_object と delete_object と
    /// create_object と create_object_section と delete_object_section と
    /// move_object_section が precondition_failed（layer_locked）になる。
    /// 設定値の変更や effect の増減は止めない。
    /// この tool 自身はロックの影響を受けない。ロックされたレイヤーでもロックを外せる。
    #[tool(
        name = "set_layer_state",
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
    pub async fn set_layer_state(
        &self,
        Parameters(input): Parameters<SetLayerStateInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("set_layer_state", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: LayerStateOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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

    /// BPM グリッドの一覧を置き換える。部分更新ではない。entries に指定した一覧が
    /// そのまま現在の一覧になり、指定しなかった要素は消える。変えたい要素だけを
    /// 差し替えるには、get_edit_info が返した grid_bpm を受け取り、その要素を書き換えて
    /// 全件を送る。一覧全体が置き換わるため、置き換え前の一覧を保持していなければ
    /// 同じ状態へは戻せない。
    /// この tool は他の編集 tool と異なり取り消し単位を作らない。実行後に取り消し
    /// 操作を行うと、グリッドではなく、その前に行った編集が取り消される。
    /// 置き換え前の一覧は取り消し操作でも戻らない。
    /// entries を空配列にするとグリッドが消える。指定できるのは 256 件までである。
    /// start が一覧の中で重複する要求は invalid_argument（duplicate_target）となる。
    /// 値が範囲外の要求は invalid_argument（grid_bpm_out_of_range）となる。
    /// tempo は単精度へ丸めた結果も 0 より大きい必要があり、極端に小さい値は
    /// 丸めると 0 になるため同じ理由で拒否される。
    /// beat が 32bit 符号付き整数に収まらない要求は
    /// invalid_argument（argument_not_representable）となる。
    /// start の昇順は求めない。並べ替えはホストが行う。
    /// 応答の entries には置き換え後に読み直した一覧が入る。ホストは tempo と offset を
    /// 単精度で受け取り並べ替えもするため、要求した値や順序と一致するとは限らない。
    /// 確かめるのは件数だけであり、件数が食い違うと unsupported_operation
    /// （change_not_applied）となる。
    #[tool(
        name = "set_grid_bpm",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_grid_bpm()
        )
    )]
    pub async fn set_grid_bpm(
        &self,
        Parameters(input): Parameters<SetGridBpmInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("set_grid_bpm", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: GridBpmOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_SET_GRID_BPM,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::grid_bpm(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// この操作は取り消せない。AviUtl2 の取り消し操作ではシーン設定は元へ戻らず、
    /// 取り消しを行うとその前に行った編集が取り消される。応答の non_undoable は
    /// 常に true であり、このことを示す。
    /// この操作は apply_batch に含められない。sub-operation として指定した要求は
    /// invalid_argument となる。
    /// シーンの名前・解像度・サンプリングレートを変更する。変更は常に現在シーンへ
    /// 掛かり、非現在シーンを指定する手段は無い。
    /// name と size と sample_rate の 3 つ全てを省略した要求は受け付けない。
    /// name に空の名前は指定できず invalid_argument（empty）となる。オブジェクト名や
    /// レイヤー名と違い、シーン名には「標準へ戻す」が無く、名前を消す手段も無い。
    /// 解像度は render_frame が描ける大きさに収まる必要がある。width と height の積が
    /// 1 フレームの非圧縮 RGBA8 の上限（256 MiB）を超える要求は invalid_argument となる。
    /// フレームレートは変更できない。現在の値は get_current_scene が返す fps_rate と
    /// fps_scale で読める。
    /// シーンには fingerprint が無いため、読み取った時点から状態が変わっていても
    /// 検出できない。応答が返す scene には変更後に観測した実際の状態が入るので、
    /// 意図どおりかはその値で確認する。
    /// 解像度とサンプリングレートの反映値は編集と原子的に観測したものではない。観測は
    /// 編集の区間を抜けた後に行い、ホストが値を調整し得るため、要求した値と異なって
    /// いても失敗にはならない。応答の observed_after_edit がこれを示す。
    /// シーン名だけは編集の区間の内側で照合する。反映されていなければ
    /// unsupported_operation（change_not_applied）となり、解像度とサンプリングレートは
    /// 1 つも変更されない。
    /// シーン設定には 0 始まりの軸が無く、応答の値は UI の表示と同じ単位である。
    #[tool(
        name = "set_scene_settings",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::set_scene_settings()
        )
    )]
    pub async fn set_scene_settings(
        &self,
        Parameters(input): Parameters<SetSceneSettingsInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("set_scene_settings", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: SceneSettingsOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_SET_SCENE_SETTINGS,
                &params,
            )?;
            Ok(ToolSuccess {
                text: describe::scene_settings(&result),
                structured: to_structured(&result)?,
            })
        })
        .await
    }

    /// どこを見て何を選んでいるかを変更する。cursor はカーソル位置、selected_range は
    /// フレーム範囲選択、focus はフォーカス対象、display はレイヤー編集の表示開始位置である。
    /// cursor と selected_range と focus と display の 4 つ全てを省略した要求は受け付けない。
    /// cursor と display はどちらも設定できる範囲へ調整されるため、要求した値が
    /// そのまま入るとは限らない。応答の cursor と display には調整後の値が入る。
    /// ただし調整の扱いは 2 つで違う。cursor はクランプされても applied に入る。
    /// 実際に入った位置は応答の cursor を読んで確かめる。
    /// display はクランプされると not_applied に入る。
    /// したがって display だけは applied を見れば要求どおりの位置か判別できる。
    /// display の反映可否は表示開始位置だけで判定する。応答が返す表示フレーム数と
    /// 表示レイヤー数は厳密な値ではなく、判定にも使えない。
    /// この tool は他の編集 tool と異なり取り消し単位を作らない。実行後に取り消し
    /// 操作を行うと、カーソルや選択範囲ではなく、その前に行った編集が取り消される。
    /// 応答が返す反映値は編集と原子的に観測したものではなく、ホストが範囲外の値を
    /// クランプした結果である。実際に適用できた項目は applied が、要求したが
    /// 適用できなかった項目は not_applied が示す。一部だけが適用されても応答は
    /// 成功であり、not_applied が空でなければ残りは反映されていない。
    #[tool(
        name = "set_selection",
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
    pub async fn set_selection(
        &self,
        Parameters(input): Parameters<SetSelectionInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("set_selection", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: SelectionState = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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
    /// この呼び出し 1 回の全体が 1 つの取り消し単位になる。
    /// 1 つの batch の中では、同じ読み取り時点の selector をそのまま並べてよい。
    /// 単独 tool を連続して呼ぶ場合と異なり、先行する変更で後続の selector が
    /// 無効にならない。全対象を変更前にまとめて照合するためである。
    /// 配列順に適用し、宛先の空きは適用時点で確かめる。したがって先行する移動が
    /// 空けた場所を、後続の移動の宛先にできる。
    /// ただし 2 つのオブジェクトが互いの位置を交換する 2 件は通らない。1 件目を
    /// 適用する時点で相手がまだ宛先に居るためである。交換は空きレイヤーを
    /// 経由する 3 件に分けること。
    /// 同じ対象の同じ状態を 2 回変更する要求は受け付けない。同じオブジェクトの
    /// 2 回の移動と、同じ設定項目への 2 回の書き込みがこれに当たる。
    /// 途中で失敗した場合はそれまでに適用した変更を自動で巻き戻す。
    /// 全て戻せた場合はプロジェクトが要求の前と同じであり、
    /// details.retry_requires は止めた失敗そのものが決める。
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
    /// レイヤー上でも通る。解除は set_layer_state で行う。
    /// 大きなプロジェクトでは適用中に AviUtl2 の UI が数秒止まり得る。
    #[tool(
        name = "apply_batch",
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
    pub async fn apply_batch(
        &self,
        Parameters(input): Parameters<ApplyBatchInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        self.run("apply_batch", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let result: BatchOutcome = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
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
    /// 描画できるのは現在シーンだけである。expected_scene_id には
    /// get_edit_info などが返した scene_id をそのまま指定する。
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
        name = "render_frame",
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
    pub async fn render_frame(
        &self,
        Parameters(input): Parameters<RenderFrameInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        let artifacts = self.artifacts.clone();
        self.run("render_frame", move || {
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
                discovery,
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
        // 層 0 が答えるのは「この server は何か」だけである。**複数 tool に
        // またがる規約をここへ書かない**——それらは値を書く場所の隣（入力
        // schema）と、作業を組み立てる時点で読まれる場所（skill）に正本を
        // 持っており、層 0 へ写せば 3 つ目の正本ができる。入力 schema は
        // 全てのクライアントが tools/list で受け取るため、写しを落としても
        // 届かなくなるものは無い。
        //
        // 残すのは宛先の在り方である。**要求が誰に向かうのかは、どの tool を
        // 呼ぶかを決める前に効く**——server が 1 つのアプリの窓口ではなく、
        // 同時に走る複数のインスタンスの窓口であることは、tool 1 個の説明にも
        // 引数 1 個の説明にも属さない。
        info.instructions = Some(
            "AviUtl2 の編集内容を読み取り、変更する。同時に起動している複数の AviUtl2 を扱い、要求ごとにどのインスタンスへ宛てるかを instance_id で指す。"
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
    /// **公開の判定を通さない。** この口は要求元へ tool を見せるためのものでは
    /// なく、要求の起動方式を検証するために dispatch が内側で引くだけである。
    /// ここで公開していない tool を「無い」ことにすると、その検証が飛ばされ、
    /// 公開していない tool への要求が受付判定へ届かずに別の失敗になる。
    /// **公開しているかどうかは [`ServerHandler::call_tool`] が判定する。**
    fn get_tool(&self, name: &str) -> Option<Tool> {
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
        let Some(source) = self.settings.shared() else {
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
        let CallBudgets { limits, discovery } = self.call_budgets();
        let artifacts = self.artifacts.clone();

        let target = parse_resource_uri(&uri)
            .ok_or_else(|| McpError::resource_not_found("未知の resource URI です", None))?;

        let uri_for_content = uri.clone();
        let contents = self
            .run_resource("resources/read", move || {
                let value = match target {
                    ResourceTarget::Instances => {
                        let response = collect_instances(
                            &registry_dir,
                            ListInstancesInput {
                                offset: 0,
                                limit: MAX_PAGE_LIMIT,
                            },
                            discovery,
                        )?;
                        fitted_instances_value(response)?
                    }
                    ResourceTarget::EditInfo(instance_id) => {
                        let info: EditInfo = request_operation(
                            &registry_dir,
                            instance_id,
                            limits,
                            discovery,
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
/// `list_instances` のページ指定で取得できる。
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
///
/// 生存確認は他の tool と同じ解決フェーズの配分で行う。一覧だけが別の配分を
/// 使うと、倍率を延ばした設定でも handshake の遅いインスタンスが一覧からだけ
/// 落ちる——**`instance_id` を要さない唯一の tool であり、そこが入口である。**
fn collect_instances(
    registry_dir: &Path,
    input: ListInstancesInput,
    discovery: DiscoveryConfig,
) -> Result<ListInstancesResponse, ErrorObject> {
    let page = input.to_page_request()?;
    list_instances(
        registry_dir,
        crate::api::ListInstancesRequest {
            offset: page.offset,
            limit: page.limit,
        },
        discovery,
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
    discovery: DiscoveryConfig,
    operation: &str,
    params: &P,
) -> Result<R, ErrorObject>
where
    P: Serialize,
    R: DeserializeOwned,
{
    let resolved = resolve_instance(registry_dir, instance_id, discovery)
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

    use aviutl2_mcp_core::{
        AvailableEffectItem, EffectItemType, ItemValue, ItemWriteError, MAX_DESCRIBED_EFFECTS,
        ReadBackCheck, prepare_item_write, read_back_check,
    };
    use rmcp::model::Tool;

    /// tool が frame / layer を入出力に持ち、0 始まりであることの明記が要るか。
    ///
    /// **未知の tool 名で落とす。** 一覧を const で持つと、どちらにも書かれて
    /// いない新しい tool が「明記が要らない」側の既定へ黙って落ちる。
    fn takes_zero_based_numbers(name: &str) -> bool {
        match name {
            "list_instances"
            | "get_edit_info"
            | "get_current_scene"
            | "list_layers"
            | "list_objects"
            | "get_object"
            | "get_effect_item_values"
            | "get_selection"
            | "create_object"
            | "move_object"
            | "set_object_name"
            | "set_object_item"
            | "add_effect"
            | "set_effect_enabled"
            | "move_effect"
            | "delete_effect"
            | "delete_object"
            | "create_object_section"
            | "delete_object_section"
            | "move_object_section"
            | "set_layer_state"
            | "set_selection"
            | "apply_batch"
            | "render_frame" => true,
            // effect カタログだけを扱い、frame も layer も現れない。
            "list_available_effects" | "describe_effects" => false,
            // 登録物の一覧だけを扱い、frame も layer も現れない。
            "list_fonts" | "list_palettes" | "list_modules" | "list_object_aliases" => false,
            // BPM グリッドはシーンに属し、位置は秒で表す。フレーム番号も
            // レイヤー番号も現れない。
            "set_grid_bpm" => false,
            // シーン設定は名前・解像度・サンプリングレートだけを扱い、0 始まりの
            // 軸を 1 つも持たない。
            "set_scene_settings" => false,
            other => panic!("{other} が 0 始まりの番号を扱うかが定義されていません"),
        }
    }

    /// tool の入力・出力 schema にレイヤー番号かフレーム番号が現れるか。
    ///
    /// [`takes_zero_based_numbers`] とは別の根拠である。前者は手書きの判定で
    /// あり、後者は tool が実際に宣言している形から読める事実である。
    fn schema_carries_a_layer_or_frame(tool: &Tool) -> bool {
        let input = Value::Object(tool.input_schema.as_ref().clone()).to_string();
        let output = tool
            .output_schema
            .as_ref()
            .map(|schema| Value::Object(schema.as_ref().clone()).to_string())
            .unwrap_or_default();
        ["layer", "frame"]
            .iter()
            .any(|name| input.contains(name) || output.contains(name))
    }

    #[test]
    fn no_tool_that_declares_a_layer_or_frame_is_exempt_from_stating_the_origin() {
        // 起点の明記が要るかは手書きの判定である。番号を扱う tool をそこで
        // 「扱わない」側へ書き換えると、判定だけを読む検査は 2 つとも黙って
        // 素通りする。免除してよいのは番号を持たない tool だけであることを、
        // schema という別の根拠から確かめる。
        //
        // 逆向きは求めない。schema に番号が現れなくても説明が番号に触れる tool
        // があり、そちらは明記を求める側であって緩める側ではない。
        for tool in tools() {
            if !schema_carries_a_layer_or_frame(&tool) {
                continue;
            }
            assert!(
                takes_zero_based_numbers(&tool.name),
                "{} は schema に番号を宣言しているのに起点の明記を免除されています",
                tool.name
            );
        }
    }

    /// 読み取り専用の tool。
    const READ_TOOLS: &[&str] = &[
        "list_instances",
        "get_edit_info",
        "get_current_scene",
        "list_layers",
        "list_objects",
        "get_object",
        "list_available_effects",
        "describe_effects",
        "get_effect_item_values",
        "get_selection",
        "list_fonts",
        "list_palettes",
        "list_modules",
        "list_object_aliases",
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
        ("create_object", false, false),
        ("move_object", false, true),
        ("set_object_name", false, true),
        ("set_object_item", false, true),
        ("add_effect", false, false),
        ("set_effect_enabled", false, true),
        // 同じ位置へ 2 度動かしても列は同じ状態になる。
        ("move_effect", false, true),
        ("delete_effect", true, true),
        ("delete_object", true, true),
        // 中間点の作成は作成系だが冪等と名乗る。あるフレームは境界であるか
        // 無いかのどちらかであり、再送しても重複して作られる余地が無い。
        ("create_object_section", false, true),
        // 中間点を消すとその位置の移動パラメータが失われ、同じ tool では戻せない。
        ("delete_object_section", true, true),
        ("move_object_section", false, true),
        // 表示を切ってもロックを掛けても内容は失われず、同じ tool で戻せる。
        // 同じ状態を 2 度設定しても追加の変更を起こさない。
        ("set_layer_state", false, true),
        ("set_selection", false, true),
        // 一覧全体が置き換わるが、同じ tool で別の一覧を書ける。同じ一覧を 2 度
        // 送っても追加の変更を起こさない。
        ("set_grid_bpm", false, true),
        // 破壊的と名乗る根拠は削除ではなく不可逆性である。削除は取り消しで戻るが、
        // シーン設定は戻らない。同じ値を 2 度設定しても追加の変更は起きないため
        // 冪等と名乗る。
        ("set_scene_settings", true, true),
    ];

    /// 一括適用の tool 名。
    const APPLY_BATCH: &str = "apply_batch";

    /// 描画の tool 名。
    const RENDER_FRAME: &str = "render_frame";

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
    /// **共有の一覧（`aviutl2_mcp_core::tool::all_tool_names`）とは別の出所で
    /// ある。** 一方は annotation と説明を検査するための手書きの表、もう一方は
    /// operation からの導出であり、**両者が router と一致することを別々の試験が
    /// 固定する**（[`all_tools_are_registered`] と
    /// [`the_registered_tools_match_the_shared_catalog`]）。
    fn annotated_tool_names() -> impl Iterator<Item = &'static str> {
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
            "create_object"
            | "move_object"
            | "set_object_name"
            | "set_object_item"
            | "add_effect"
            | "set_effect_enabled"
            | "move_effect"
            | "delete_effect"
            | "delete_object"
            | "create_object_section"
            | "delete_object_section"
            | "move_object_section"
            | "set_layer_state"
            | "set_selection"
            | "set_grid_bpm"
            | "set_scene_settings"
            | APPLY_BATCH => true,
            "list_instances"
            | "get_edit_info"
            | "get_current_scene"
            | "list_layers"
            | "list_objects"
            | "get_object"
            | "list_available_effects"
            | "describe_effects"
            | "get_effect_item_values"
            | "get_selection"
            | "list_fonts"
            | "list_palettes"
            | "list_modules"
            | "list_object_aliases"
            | RENDER_FRAME => false,
            other => panic!("{other} が編集の説明規約に従うかが定義されていません"),
        }
    }

    /// 編集の説明規約が掛かる tool。
    fn edit_like_tools() -> Vec<&'static str> {
        annotated_tool_names()
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
        let expected: std::collections::BTreeSet<String> = annotated_tool_names()
            .map(|name| name.to_string())
            .collect();
        assert_eq!(names, expected);
        // 件数そのものも固定する。router と表の両方から同じ tool を落とすと、
        // 集合の一致だけでは検出できない。
        assert_eq!(names.len(), 32, "公開する tool の数が変わりました");
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
        let server = server_with(r#"{"disabled_tools":["delete_object"]}"#);
        assert!(!visible_names(&server).contains("delete_object"));
        assert!(!server.accepts_tool_call("delete_object"));
        // 巻き添えにしない。
        assert!(server.accepts_tool_call("delete_effect"));
    }

    #[test]
    fn the_always_enabled_tool_survives_being_disabled() {
        let server = server_with(r#"{"disabled_tools":["list_instances","render_frame"]}"#);
        let visible = visible_names(&server);
        assert!(visible.contains(aviutl2_mcp_core::tool::ALWAYS_ENABLED_TOOL));
        assert!(server.accepts_tool_call(aviutl2_mcp_core::tool::ALWAYS_ENABLED_TOOL));
        assert!(!visible.contains("render_frame"));
    }

    #[test]
    fn what_is_listed_is_exactly_what_is_accepted() {
        // 掲載と受付が同じ判定を読むことを、全 tool について固定する。片方だけを
        // 絞る実装になると、掲載していない tool の call が通る。
        let server =
            server_with(r#"{"disabled_tools":["delete_object","apply_batch","list_instances"]}"#);
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
        let server = server_with(r#"{"disabled_tools":["delete_object"]}"#);
        assert!(server.accepts_tool_call("aviutl2_future_tool"));
    }

    #[test]
    fn a_disabled_tool_is_rejected_with_the_documented_code() {
        let server = server_with(r#"{"disabled_tools":["delete_object"]}"#);
        let result = server.reject_disabled_tool("delete_object");
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
            "list_instances" => schema::list_instances(),
            "get_edit_info" => schema::edit_info(),
            "get_current_scene" => schema::current_scene(),
            "list_layers" => schema::list_layers(),
            "list_objects" => schema::list_objects(),
            "get_object" => schema::object_detail(),
            "list_available_effects" => schema::list_available_effects(),
            "describe_effects" => schema::describe_effects(),
            "list_fonts" => schema::list_fonts(),
            "list_palettes" => schema::list_palettes(),
            "list_modules" => schema::list_modules(),
            "list_object_aliases" => schema::list_object_aliases(),
            "get_effect_item_values" => schema::effect_item_values(),
            "get_selection" => schema::get_selection(),
            "create_object" => schema::create_object(),
            "move_object" => schema::move_object(),
            "set_object_name" => schema::set_object_name(),
            "set_object_item" => schema::set_object_item(),
            "add_effect" => schema::add_effect(),
            "set_effect_enabled" => schema::set_effect_enabled(),
            "move_effect" => schema::move_effect(),
            "delete_effect" => schema::delete_effect(),
            "delete_object" => schema::delete_object(),
            "create_object_section" => schema::create_object_section(),
            "delete_object_section" => schema::delete_object_section(),
            "move_object_section" => schema::move_object_section(),
            "set_layer_state" => schema::set_layer_state(),
            "set_selection" => schema::set_selection(),
            "set_grid_bpm" => schema::set_grid_bpm(),
            "set_scene_settings" => schema::set_scene_settings(),
            "apply_batch" => schema::apply_batch(),
            "render_frame" => schema::render_frame(),
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
    fn the_effect_catalog_description_names_where_item_names_come_from() {
        // 一覧は設定項目の名前を返さない。どこで得られるかを書かなければ、
        // 名前を推測で組み立てるか、そもそも項目を触らないかのどちらかになる。
        let description = description_of("list_available_effects");
        assert!(
            description.contains("get_object"),
            "項目名の入手先が説明にありません: {description}"
        );
        assert!(
            description.contains("description"),
            "説明が付かない effect があることが書かれていません: {description}"
        );
    }

    #[test]
    fn describe_effects_declares_the_same_limit_it_states() {
        // 上限は 3 か所——core の検証・入力 schema・tool の説明——に現れる。
        // 片方だけを動かすと、宣言と説明と実際の受理範囲が食い違う。
        let tool = tool_named("describe_effects");
        let names = &tool.input_schema["properties"]["effect_names"];
        assert_eq!(
            names["maxItems"],
            serde_json::json!(MAX_DESCRIBED_EFFECTS),
            "宣言した上限が core の上限と違います: {names}"
        );
        assert_eq!(names["minItems"], serde_json::json!(1), "{names}");
        // 重複は畳まずに拒否する。宣言だけを落としても、実際には拒否される。
        assert_eq!(names["uniqueItems"], serde_json::json!(true), "{names}");

        let description = description_of("describe_effects");
        assert!(
            description.contains(&format!("1〜{MAX_DESCRIBED_EFFECTS} 件")),
            "説明が宣言した上限を述べていません: {description}"
        );
    }

    #[test]
    fn describe_effects_states_how_a_missing_name_comes_back() {
        // 落ちた名前に気付けなければ、要求元は「設定項目を持たない effect」と
        // 誤読する。要求全体が失敗しないことと、確認先を説明が述べる。
        let description = description_of("describe_effects");
        for phrase in [
            "not_found",
            "要求全体は失敗しない",
            "設定項目を持たない effect ではない",
            "list_available_effects",
        ] {
            assert!(
                description.contains(phrase),
                "describe_effects の説明が {phrase} に触れていません: {description}"
            );
        }
    }

    #[test]
    fn describe_effects_states_where_the_descriptions_come_from_and_when_they_are_missing() {
        // 説明を推測で補わない方針の帰結として、説明を持たない effect は多い。
        // 述べておかなければ、null を「取得に失敗した」と読まれる。
        let description = description_of("describe_effects");
        for phrase in [
            "ホストが同梱する説明",
            "null",
            "推測で補わない",
            "items の顔ぶれ",
            "get_object",
        ] {
            assert!(
                description.contains(phrase),
                "describe_effects の説明が {phrase} に触れていません: {description}"
            );
        }
    }

    #[test]
    fn describe_effects_states_that_the_facets_are_a_hint_and_not_a_gate() {
        // 面をゲートとして読まれると、載っていない値を書けるのに書かない。
        // 面を出す目的そのものが失われる。値域は候補より外れやすく、版が上がって
        // 上限が広がったときに、表が正しい値を範囲外だと言う。
        let description = description_of("describe_effects");
        for phrase in [
            "choices",
            "builtin_table",
            "sidecar",
            "候補に無い値でも書き込みは通り",
            "必ず通るとも限らない",
            "range",
            "値域を外れる値でも書き込みは通る",
            "可否を決めるのはホストである",
        ] {
            assert!(
                description.contains(phrase),
                "describe_effects の説明が {phrase} に触れていません: {description}"
            );
        }
    }

    #[test]
    fn describe_effects_states_that_range_bounds_the_written_value_and_not_the_evaluated_one() {
        // 値域を先に引いてから組む要求元ほど、書き込みが通ったことを評価の
        // 妥当性と読む。移動方法によっては、境界へ書いた値が値域の内側でも
        // 途中のフレームの評価値が外へ出る。range が掛かる先を述べておく。
        let description = description_of("describe_effects");
        for phrase in ["書き込む値に掛かる", "評価値の上下界ではない", "値域の外"]
        {
            assert!(
                description.contains(phrase),
                "describe_effects の説明が {phrase} に触れていません: {description}"
            );
        }
    }

    #[test]
    fn describe_effects_states_what_a_group_is_and_what_a_null_group_means() {
        // 設定項目の一覧は平らな列で返るため、グループが無ければどこが 1 つの組
        // かは読めない。そして「グループ」という語は get_effect_item_values でも
        // 使われており、あちらは名前を持つ。同じ語が別のものを指すことを述べて
        // おかなければ、名前の在るものとして読まれる。
        let description = description_of("describe_effects");
        for phrase in [
            "group",
            "item_names",
            "属さない項目は null",
            "名前を持たない",
            "get_effect_item_values",
            "要求全体が失敗する",
        ] {
            assert!(
                description.contains(phrase),
                "describe_effects の説明が {phrase} に触れていません: {description}"
            );
        }
    }

    #[test]
    fn describe_effects_neither_declares_nor_promises_a_page() {
        // ページの続きという概念が無い。schema が受け付けないことと、説明が
        // そう述べていることを揃える。
        let tool = tool_named("describe_effects");
        let properties = tool.input_schema["properties"]
            .as_object()
            .expect("入力が properties を宣言していません");
        for field in ["offset", "limit", "snapshot_revision"] {
            assert!(
                !properties.contains_key(field),
                "{field} を宣言しています: {properties:?}"
            );
        }
        assert_eq!(
            tool.input_schema["required"],
            serde_json::json!(["instance_id", "effect_names"]),
            "describe_effects の必須項目"
        );
        assert!(
            description_of("describe_effects").contains("ページ指定を持たない"),
            "ページを取らないことが説明されていません"
        );
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
        "create_object",
        "set_layer_state",
        "set_selection",
        "set_grid_bpm",
        "set_scene_settings",
    ];

    #[test]
    fn every_edit_tool_declares_where_the_project_boundary_is_matched() {
        // プロジェクト境界の照合材料は、要求のどこかに必ず在る——selector の中か、
        // 前提の epoch のどちらかである。**述べる場所は入力 schema である。**
        // tool の説明へ写すと、同じ 5 行が編集 tool の数だけ並ぶ一方、値を書く
        // 時点では読まれない。
        for name in edit_like_tools() {
            let schema = Value::Object(tool_named(name).input_schema.as_ref().clone()).to_string();
            assert!(
                schema.contains("project_epoch"),
                "{name} の入力 schema が境界の照合材料を持ちません"
            );
            assert!(
                !description_of(name).contains("expected_project_epoch"),
                "{name} の説明が入力 schema の写しを持っています"
            );
        }
    }

    #[test]
    fn only_the_tools_that_may_carry_no_selector_ask_for_an_expected_epoch() {
        // 前提の epoch を運ぶのは、要求が selector を 1 つも運ばないことがある
        // tool だけである。必ず運ぶ tool へ宣言すると、同じ意味の値が 1 要求の
        // 2 か所へ並ぶ。どちらの側に属するかを表で固定するので、tool を足した
        // ときに素通りしない。
        for name in edit_like_tools() {
            let properties = tool_named(name).input_schema["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{name} に properties がありません"))
                .clone();
            if TOOLS_CARRYING_AN_EXPECTED_EPOCH.contains(&name) {
                assert!(
                    properties.contains_key("expected_project_epoch"),
                    "{name} が前提の epoch を宣言していません"
                );
                continue;
            }
            assert!(
                !properties.contains_key("expected_project_epoch"),
                "{name} が運べない前提の epoch を求めています"
            );
        }
    }

    /// 入力 schema が受け取った分。
    struct InputSchemaLanding {
        /// 入力 schema で述べる句。
        phrase: &'static str,
        /// 句を運ぶ property の名前。
        ///
        /// **空なら schema のどこに在ってもよい**——共有の入力型そのものの説明が
        /// 持つ場合である。名前を挙げた行では、その property の説明だけを見る。
        /// 挙げないと、たまたま同じ語を含む無関係なフィールドが数に入り、
        /// 本来の置き場所が空になっても閾値を満たしてしまう。
        fields: &'static [&'static str],
        /// 句が届く tool の最小数。
        reaches: usize,
    }

    /// 層 1 から落とした反復句 1 件。
    ///
    /// **行き先は 1 つとは限らない。** 引数の隣に置ける部分と、組み立ての段階で
    /// 効く部分の両方を持つ事実があり、単一の行き先しか記録できない形にすると
    /// 片方が黙って落ちる。
    struct Relocation {
        /// 層 1 が述べていた事実。
        statement: &'static str,
        /// 層 1 で同じことを述べていた tool の数。
        was_stated_by: usize,
        /// その事実が実際に掛かる tool の数。
        ///
        /// **1 なら tool 固有であり、層 1 に残すべきものである。**
        /// [`was_stated_by`] とは別に持つ——複数 tool へ掛かるキーを 1 tool の
        /// 説明だけが解説している状態は、説明の数からは見えない。
        applies_to: usize,
        /// 層 1 から消えたことを確かめる句。
        dropped: &'static [&'static str],
        /// 入力 schema が受け取る分。
        to_input_schema: Option<InputSchemaLanding>,
        /// skill が受け取る分。**skill が本文に書く内容である。**
        to_skill: Option<&'static str>,
    }

    /// 層 1 から落とした反復句と、その行き先。
    ///
    /// **「落とした」と「消えた」を区別する唯一の表である。** 各行について、
    /// 句が tool の説明から消えていることと、行き先に在ることを確かめる。
    ///
    /// **`to_skill` の側は [`the_conventions_handed_to_the_skill_are_in_its_body`]
    /// が本文と突き合わせる。** 本表が層 3 側の検査の入力である——句がどこへ
    /// 行ったかを記録しているのはここだけであり、skill の本文だけを読んでも
    /// 「述べ足りない」ことは分からない。
    /// 併せて持ち越す検査は [`CHECKS_HANDED_TO_THE_SKILL`] にある。
    const RELOCATED_CONVENTIONS: &[Relocation] = &[
        Relocation {
            statement: "frame 番号と layer 番号はいずれも 0 始まりであり、UI の表示とは 1 ずれる",
            was_stated_by: 25,
            applies_to: 23,
            dropped: &[
                "番号はいずれも 0 始まり",
                "layer 番号は 0 始まり",
                "frame 番号は 0 始まり",
                "UI の表示とは異なる",
            ],
            to_input_schema: Some(InputSchemaLanding {
                phrase: "0 始まり",
                fields: &["layer", "frame", "frames", "layer_min", "layer_max"],
                reaches: 18,
            }),
            // 起点そのものは値の隣で足りるが、**UI と 1 ずれることは引数の隣に
            // 書いても遅い。** 画面で見た番号をそのまま送る判断は、要求を組み立てる
            // 前に起きる。
            to_skill: Some(
                "AviUtl2 の UI はレイヤーとフレームを 1 始まりで表示する。\
                 tool が受け渡すのは 0 始まりの番号であり、画面で見た番号より 1 小さい",
            ),
        },
        Relocation {
            statement: "応答が返した selector は組み立て直さず、読み直さずにそのまま次の要求へ渡せる",
            was_stated_by: 12,
            applies_to: 14,
            dropped: &["読み直さずにそのまま次の編集へ渡せる"],
            to_input_schema: Some(InputSchemaLanding {
                phrase: "読み直さずにそのまま次の要求へ渡せる",
                fields: &[],
                reaches: 14,
            }),
            to_skill: Some(
                "selector は自分で組み立てない。読み取りの応答が返した値をそのまま編集へ渡し、\
                 編集の応答が返した値をそのまま次の編集へ渡す",
            ),
        },
        Relocation {
            statement: "プロジェクトの世代は selector が運ぶ project_epoch で照合する",
            was_stated_by: 11,
            applies_to: 14,
            dropped: &["selector が運ぶ project_epoch"],
            to_input_schema: Some(InputSchemaLanding {
                phrase: "プロジェクトの世代はこの値で照合",
                fields: &["project_epoch"],
                reaches: 14,
            }),
            to_skill: Some(
                "プロジェクト境界の照合材料は selector が運ぶ project_epoch である。\
                 要求が selector を 1 つも運ばないことがある tool（create_object・\
                 set_layer_state・set_selection・set_grid_bpm・set_scene_settings）だけが\
                 expected_project_epoch を要求し、そちらは省略できない",
            ),
        },
        Relocation {
            statement: "対象が変化していた precondition_failed は、対象の現在の姿を details へ添える。\
                        読み直さずにそのまま次の要求の selector にできる",
            was_stated_by: 11,
            applies_to: 12,
            dropped: &["details.current_object"],
            // **キー名は層 2 に置けない。** 共有型は apply_batch の schema にも
            // 入るが、そちらは同じものを failed_object という別の名前で返す。
            // 層 2 が名乗れるのは「同じ形が添う」ことまでである。
            to_input_schema: Some(InputSchemaLanding {
                phrase: "対象の現在の姿",
                fields: &[],
                reaches: 14,
            }),
            to_skill: Some(
                "対象が変化していた precondition_failed では details.current_object に\
                 対象の現在の姿が入り、そのまま次の要求の selector にできる。\
                 apply_batch だけは何番目で落ちたかを併せて示すため details.failed_object という\
                 別のキーで返す",
            ),
        },
        Relocation {
            statement: "offset と limit（1〜200、既定 50）でページを指定し、\
                        2 ページ目以降は先頭ページが返した snapshot_revision を添える",
            was_stated_by: 11,
            applies_to: 9,
            dropped: &["offset と limit（1〜200、既定 50）"],
            to_input_schema: Some(InputSchemaLanding {
                phrase: "1 以上 200 以下",
                fields: &["limit"],
                reaches: 9,
            }),
            to_skill: None,
        },
        Relocation {
            statement: "カタログ列挙の snapshot_revision は受理されるが照合には用いない",
            was_stated_by: 5,
            applies_to: 5,
            dropped: &["snapshot_revision は受理するがページ間の照合には用いない"],
            to_input_schema: Some(InputSchemaLanding {
                phrase: "受理するがページ間の照合に用いない",
                fields: &["snapshot_revision"],
                reaches: 5,
            }),
            to_skill: None,
        },
        Relocation {
            statement: "要求は project_revision を運ばない。\
                        読み取りから編集までに revision が進んでいても拒否されない",
            was_stated_by: 16,
            applies_to: 16,
            dropped: &["project_revision を運ばない"],
            // 引数に無いものの不在は、引数の隣に書けない。
            to_input_schema: None,
            to_skill: Some(
                "要求は project_revision を運ばない。読み取りから編集までに revision が\
                 進んでいても拒否されない。拒否を避けるために revision を取り直す必要は無い",
            ),
        },
        Relocation {
            statement: "変更が起きた編集 tool の呼び出し 1 回が、1 つの取り消し単位になる。\
                        まとめて 1 単位にしたいときは apply_batch を選ぶ",
            was_stated_by: 12,
            applies_to: 16,
            dropped: &["この呼び出し 1 回が 1 つの取り消し単位になる"],
            to_input_schema: None,
            to_skill: Some(
                "変更が起きた編集 tool の呼び出し 1 回が、1 つの取り消し単位になる。\
                 まとめて 1 単位にしたいときは apply_batch を選ぶ",
            ),
        },
        Relocation {
            statement: "timeout は変更が無かったことを意味しない。\
                        details.change_applied が \"no\" なら未適用のため再送してよく、\
                        \"unknown\" なら読み直して確認してから再送する",
            was_stated_by: 16,
            applies_to: 16,
            dropped: &["details.change_applied"],
            to_input_schema: None,
            to_skill: Some(
                "timeout は変更が無かったことを意味しない。details.change_applied が \"no\" なら\
                 未適用のため再送してよく、\"unknown\" なら読み直して確認してから再送する",
            ),
        },
        Relocation {
            // **層 1 で述べていたのは 1 tool だけだが、キーは汎用である。**
            // 書き込みを発行した後に落ちた失敗すべてに付き、一括適用の
            // sub-operation でも立つ。1 tool の説明が全編集経路のキーを解説して
            // いる状態は、説明の数からは見えない。
            statement: "details.mutation_issued は、その失敗の時点で書き込みが\
                        発行済みだったかを示す",
            was_stated_by: 1,
            applies_to: 16,
            dropped: &["details.mutation_issued"],
            to_input_schema: None,
            to_skill: Some(
                "書き込みを発行した後に落ちた失敗には details.mutation_issued が true で付く。\
                 付かない失敗は 1 バイトも書いていないため、対象を読み直さずに要求を直して\
                 送り直せる。付く失敗が読み直しを要するかは details.retry_requires が名乗る\
                 ——発行した変更が戻っていれば読み直す先は無い",
            ),
        },
        Relocation {
            // 値の書式は値を書く場所の隣が正本である。層 1 にも置くと、
            // 書式が変わったときに片方だけが古くなる。
            statement: "色は 16 進 6 桁で指定する",
            was_stated_by: 1,
            applies_to: 2,
            dropped: &["色は 16 進 6 桁で指定する"],
            to_input_schema: Some(InputSchemaLanding {
                phrase: "16 進 6 桁",
                fields: &[],
                reaches: 2,
            }),
            to_skill: None,
        },
        Relocation {
            // 値の選び方そのものは、どの tool を呼ぶかを決める前に効く。
            statement: "設定項目に書ける値は describe_effects の choices と range から、\
                        フォント名は list_fonts から、表に無い項目は既存オブジェクトの値から得る",
            was_stated_by: 1,
            applies_to: 2,
            dropped: &[
                "選べる値と値域は describe_effects が返す",
                "登録済みのフォント名は list_fonts が返す",
            ],
            to_input_schema: None,
            to_skill: Some(
                "設定項目に何を書けるか分からないときは describe_effects を呼ぶ。\
                 choices が候補を、range が値域と小数桁を返す。どちらも null の項目は\
                 表に載っていないだけであり、既存オブジェクトの値を get_object で読んで倣う。\
                 フォント名は list_fonts が返す",
            ),
        },
    ];

    /// 層 3 が受け取る検査 1 件。
    struct HandedCheck {
        /// 層 1 に対して確かめていたこと。
        checked: &'static str,
        /// skill 側で何を確かめる形になるか。
        becomes: &'static str,
    }

    /// 層 3 へ持ち越す検査。
    ///
    /// **削除ではない。** いずれも「説明が嘘をつかないこと」を守っていた検査で
    /// あり、句を動かすなら検査も動かす。skill を書く作業はこの表を入力に取る。
    const CHECKS_HANDED_TO_THE_SKILL: &[HandedCheck] = &[
        HandedCheck {
            checked: "編集 tool すべての説明が「要求は project_revision を運ばない」と述べること",
            becomes: "SKILL.md の本文が同じことを 1 度述べること",
        },
        HandedCheck {
            checked: "10 tool の説明が「この呼び出し 1 回が 1 つの取り消し単位になる」と述べること",
            becomes: "SKILL.md が一般則を 1 度述べ、例外（set_selection と set_grid_bpm は\
                      単位を作らない、set_scene_settings は取り消せない）を名指しすること。\
                      層 1 に残る表明は [`undo_statement`] が持つ",
        },
        HandedCheck {
            checked: "編集 tool すべての説明が details.change_applied の 3 値の読み方を述べること",
            becomes: "SKILL.md が timeout を受けた後の手順を 1 度述べること。\
                      値そのものは失敗の text content へ出るため、書くのは読み方だけでよい",
        },
        HandedCheck {
            checked: "set_object_item の説明が、書ける値の入手先（describe_effects・list_fonts・\
                      get_object）を述べること",
            becomes: "SKILL.md が候補を引く経路を 1 度述べること。**候補の値そのものは写さない**\
                      ——正本は describe_effects が返す表である",
        },
    ];

    #[test]
    fn the_phrases_dropped_from_the_tool_descriptions_live_in_another_layer() {
        // **「落とした」と「消えた」を区別する唯一の検査である。**
        // 層 1 から句が消えたことだけを見ると、どこにも無い状態が通ってしまう。
        let descriptions: Vec<(String, String)> = tools()
            .into_iter()
            .map(|tool| (tool.name.to_string(), description_of(&tool.name)))
            .collect();
        let schemas: Vec<(String, String)> = tools()
            .into_iter()
            .map(|tool| {
                (
                    tool.name.to_string(),
                    Value::Object(tool.input_schema.as_ref().clone()).to_string(),
                )
            })
            .collect();

        for relocation in RELOCATED_CONVENTIONS {
            assert!(
                relocation.applies_to > 1,
                "1 tool にしか掛からない事実は層 1 に残すものです: {}",
                relocation.statement
            );
            // **表が記録するのは移設であって新設ではない。** 層 1 が 1 度も
            // 述べていなかった事実をここへ足すと、落とした句の帳尻が合わなくなる。
            assert!(
                relocation.was_stated_by >= 1,
                "層 1 が述べていなかった事実が表に在ります: {}",
                relocation.statement
            );
            // 行き先が 1 つも無い行は、落としただけの行である。
            assert!(
                relocation.to_input_schema.is_some() || relocation.to_skill.is_some(),
                "行き先の無い句が表に在ります（層 1 の {} tool が述べていました）: {}",
                relocation.was_stated_by,
                relocation.statement
            );
            for phrase in relocation.dropped {
                for (name, description) in &descriptions {
                    assert!(
                        !description.contains(phrase),
                        "{name} の説明が層 1 から落とした句を残しています: {phrase}"
                    );
                }
            }
            if let Some(landing) = &relocation.to_input_schema {
                let reached = tools()
                    .into_iter()
                    .filter(|tool| {
                        let schema = Value::Object(tool.input_schema.as_ref().clone());
                        if landing.fields.is_empty() {
                            // 共有の入力型そのものの説明が持つ。
                            return schema.to_string().contains(landing.phrase);
                        }
                        property_descriptions(&schema).iter().any(|(field, text)| {
                            landing.fields.contains(&field.as_str())
                                && text.contains(landing.phrase)
                        })
                    })
                    .count();
                assert!(
                    reached >= landing.reaches,
                    "{} が入力 schema で {} tool にしか届いていません（{} 以上を期待）",
                    relocation.statement,
                    reached,
                    landing.reaches
                );
            }
            if let Some(statement) = relocation.to_skill {
                // 本文との突き合わせは
                // [`the_conventions_handed_to_the_skill_are_in_its_body`] が行う。
                // ここで見るのは、行き先の宣言が空でないことだけである。
                assert!(!statement.is_empty(), "skill が受け取る内容が空です");
            }
        }

        // schemas は層 1 と層 2 の両方を 1 度に見るために組む。落とした句が
        // schema 側へも現れていないかを、行き先の宣言と突き合わせる。
        for relocation in RELOCATED_CONVENTIONS {
            if relocation.to_input_schema.is_some() {
                continue;
            }
            for phrase in relocation.dropped {
                for (name, schema) in &schemas {
                    assert!(
                        !schema.contains(*phrase),
                        "{name} の入力 schema が、入力 schema へ移さないと決めた句を持っています: {phrase}"
                    );
                }
            }
        }

        assert!(
            RELOCATED_CONVENTIONS
                .iter()
                .any(|relocation| relocation.to_skill.is_some()),
            "skill が受け取る行が 1 つもありません"
        );
        for check in CHECKS_HANDED_TO_THE_SKILL {
            assert!(
                !check.checked.is_empty() && !check.becomes.is_empty(),
                "持ち越す検査の記録が欠けています"
            );
        }
    }

    #[test]
    fn the_server_instructions_carry_no_convention_that_lives_in_another_layer() {
        // 層 0 が答えるのは「この server は何か」であり、接続時に 1 度だけ
        // 読まれる。**層 1 から落とした句をここへ寄せると、正本が 3 つになる。**
        // 移設の表を入力に取り、行き先が層 2 と層 3 に決まった句が層 0 にも
        // 現れていないことを見る。
        let instructions = ServerHandler::get_info(&server())
            .instructions
            .expect("層 0 の説明がありません");
        for relocation in RELOCATED_CONVENTIONS {
            for phrase in relocation.dropped {
                assert!(
                    !instructions.contains(phrase),
                    "層 0 が層 1 から落とした句を抱えています: {phrase}"
                );
            }
        }
        // **tool を名指ししない。** 名指しは必ず「その tool がどういうものか」を
        // 伴い、しかも数え漏らしても誰も気付けない——ここに在った
        // 「selector を持たない create_object と set_selection」は、実際には
        // 5 tool を数え落としていた。
        for tool in tools() {
            assert!(
                !instructions.contains(tool.name.as_ref()),
                "層 0 が {} を名指ししています",
                tool.name
            );
        }
        assert!(
            !instructions.contains("expected_project_epoch"),
            "層 0 が入力 schema の写しを持っています"
        );
    }

    /// 同梱する skill の `SKILL.md` を読む。
    ///
    /// **本文は plugin crate が持つ**——skill は plugin のバイナリへ埋め込まれ、
    /// plugin が書き出す。それでも突き合わせをこちら側で行うのは、**層 1 から
    /// 何を落としたかの記録がこの crate にしか無い**ためである。本文の側だけを
    /// 読んでも、述べ足りない句は見えない。
    fn skill_body() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates ディレクトリを辿れません")
            .join("plugin")
            .join("data")
            .join("skills")
            .join("aviutl2-editing")
            .join("SKILL.md");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} を読めません: {e}", path.display()))
    }

    /// 本文と句を、markdown の装飾と改行の差を無視して比べられる形へ均す。
    ///
    /// 表が持つ句は 1 続きの文であり、本文では折り返され、語の一部が
    /// コードや強調として囲まれる。**均さずに比べると、体裁を整えただけで
    /// 検査が落ちる。** 均すのは空白とバッククォートとアスタリスクだけで、
    /// 下線は残す——`set_object_item` のような名前が別語に化ける。
    fn without_markdown_noise(text: &str) -> String {
        text.chars()
            .filter(|ch| !ch.is_whitespace() && !matches!(ch, '`' | '*'))
            .collect()
    }

    /// 均した本文の中に句が現れる回数。
    fn occurrences(body: &str, phrase: &str) -> usize {
        without_markdown_noise(body)
            .matches(&without_markdown_noise(phrase))
            .count()
    }

    #[test]
    fn the_conventions_handed_to_the_skill_are_in_its_body() {
        // **C-T1 の層 3 側である。** 層 1 から句が消えたことは
        // [`the_phrases_dropped_from_the_tool_descriptions_live_in_another_layer`]
        // が見ている。こちらが見るのは、その行き先に句が在ることである。
        // 両方を持って初めて「落とした」と「消えた」が区別できる。
        let body = skill_body();
        let mut handed = 0usize;
        for relocation in RELOCATED_CONVENTIONS {
            let Some(statement) = relocation.to_skill else {
                continue;
            };
            handed += 1;
            assert!(
                occurrences(&body, statement) >= 1,
                "層 1 の {} tool が述べていた事実が skill の本文にありません: {statement}",
                relocation.was_stated_by
            );
        }
        assert!(handed > 0, "skill が受け取った句が 1 つもありません");
    }

    #[test]
    fn the_checks_handed_to_the_skill_are_satisfied_by_its_body() {
        // **削除ではなく移設であることを、移設先で確かめる。** 4 件はいずれも
        // 「説明が嘘をつかないこと」を守っていた検査であり、層 1 から消した
        // 時点で守り手が居なくなっている。
        let body = skill_body();
        assert_eq!(
            CHECKS_HANDED_TO_THE_SKILL.len(),
            4,
            "持ち越す検査が増減しています。本文側の検査も併せて見直してください"
        );

        // 1. 要求が project_revision を運ばないこと。**1 度だけ述べる**——
        //    層 1 で 16 tool へ並んでいた句を、層 3 でも並べては移した意味が無い。
        assert_eq!(
            occurrences(&body, "project_revision を運ばない"),
            1,
            "project_revision を運ばないことの記述が 1 度ではありません"
        );

        // 2. 取り消し単位。一般則を 1 度述べ、例外を名指しし、確かめていない
        //    ものを確かめていないと述べる。**どれを名指しするかは層 1 の
        //    [`undo_statement`] が持つ**——手書きの一覧にすると、tool を足した
        //    ときに片方だけが古くなる。
        assert_eq!(
            occurrences(&body, "1 つの取り消し単位になる"),
            1,
            "取り消し単位の一般則が 1 度ではありません"
        );
        let mut exceptions = 0usize;
        for name in edit_like_tools() {
            match undo_statement(name) {
                UndoStatement::NoUnitAndJumpsBack => {
                    exceptions += 1;
                    assert!(
                        body.lines()
                            .any(|line| line.contains(name)
                                && line.contains("取り消し単位を作らない")),
                        "{name} が単位を作らない例外として名指しされていません"
                    );
                }
                UndoStatement::NotUndoableAndJumpsBack => {
                    exceptions += 1;
                    assert!(
                        body.lines()
                            .any(|line| line.contains(name) && line.contains("取り消せない")),
                        "{name} が取り消せない例外として名指しされていません"
                    );
                }
                UndoStatement::ItsWholePurpose | UndoStatement::FollowsTheGeneralRule => {}
            }
        }
        assert!(exceptions > 0, "例外が 1 つも名指しされていません");
        // **確かめていないと名乗る行を残さない。** 一般則に従うと分かった tool へ
        // 札が残ると、読み手は 1 回の Undo で戻る手順を避けることになる。
        assert!(
            !body
                .lines()
                .any(|line| line.contains("取り消し") && line.contains("確かめていない")),
            "取り消し単位を確かめていないと述べる行が残っています"
        );

        // 3. timeout を受けた後の手順。**値そのものは失敗の text content へ出る**
        //    ため、本文が持つのは読み方だけでよい。
        assert_eq!(
            occurrences(&body, "details.change_applied"),
            1,
            "timeout の後の手順が 1 度ではありません"
        );

        // 4. 候補を引く経路。**候補の値そのものは写さない**——正本は
        //    describe_effects が返す表である。写しが無いことは plugin crate の
        //    検査が基底の表と突き合わせて見る。
        assert_eq!(
            occurrences(&body, "describe_effects を呼ぶ"),
            1,
            "候補を引く経路が 1 度ではありません"
        );
        assert!(
            occurrences(&body, "正本は describe_effects が返す表") >= 1,
            "候補の正本がどこに在るかを本文が述べていません"
        );
    }

    /// effect の列の変化が兄弟 effect の selector も無効にすることを述べる tool。
    ///
    /// **述べる場所は層 1 である。** 起こすのはこの 3 tool だけであり、
    /// 反復句にならない。加えて、応答が返すのは足した／消した／動かした effect の
    /// selector だけであるため、共有の selector 型が述べる「応答が返した新しい
    /// selector へ持ち替える」では兄弟を回復できない——回復の手段（get_object を
    /// 引き直す）を名指しできるのは、列を変える tool の説明だけである。
    ///
    /// 第 2 要素は、その tool が兄弟を巻き込む理由を述べる語句である。増減と
    /// 移動では巻き込み方が違うため、同じ 1 文にはならない。
    const TOOLS_THAT_INVALIDATE_SIBLING_EFFECTS: &[(&str, &str)] = &[
        (
            "add_effect",
            "effect の増減は、同じオブジェクトが持つ他の effect の selector も無効にする",
        ),
        (
            "delete_effect",
            "effect の増減は、同じオブジェクトが持つ他の effect の selector も無効にする",
        ),
        ("move_effect", "移動は間にある effect の位置もずらす"),
    ];

    #[test]
    fn the_tools_that_change_the_effect_column_say_the_siblings_go_stale() {
        // 実測では、effect を足して消すとオブジェクトと兄弟 effect の fingerprint が
        // いずれも足す前の値へ完全に戻った。fingerprint は純粋な内容ハッシュで
        // あり、列の変化は兄弟まで巻き込む。述べなければ、要求元は手元の兄弟
        // selector を使い続けて precondition_failed を踏み、対象を読み直すことに
        // なる。
        for (name, cause) in TOOLS_THAT_INVALIDATE_SIBLING_EFFECTS {
            let description = description_of(name);
            for phrase in [*cause, "兄弟 effect", "get_object を引き直す"] {
                assert!(
                    description.contains(phrase),
                    "{name} の説明が {phrase} に触れていません: {description}"
                );
            }
        }
        // 起こさない tool が述べると、掛からない制約として読まれる。
        for name in edit_like_tools() {
            if TOOLS_THAT_INVALIDATE_SIBLING_EFFECTS
                .iter()
                .any(|(listed, _)| *listed == name)
            {
                continue;
            }
            assert!(
                !description_of(name).contains("兄弟 effect"),
                "{name} の説明が起こさない無効化を述べています"
            );
        }
    }

    #[test]
    fn creation_tools_name_the_guard_that_actually_stops_a_resend() {
        // revision を照合しない以上、再送を止めるのは宛先の重複確認と対象の
        // fingerprint である。防ぐ仕組みを取り違えて案内すると、呼び出し側は
        // 効かない対策を信じて再送する。
        assert!(
            description_of("create_object").contains("destination_occupied"),
            "create_object の説明が宛先重複の確認に触れていません"
        );
        assert!(
            description_of("add_effect").contains("fingerprint が変わるため"),
            "add_effect の説明が fingerprint の変化に触れていません"
        );
        for name in ["create_object", "add_effect"] {
            assert!(
                !description_of(name).contains("同じ expected での再送"),
                "{name} の説明が expected による重複防止を主張しています"
            );
        }
    }

    #[test]
    fn create_object_states_what_an_effect_name_is_and_when_it_cannot_be_used() {
        // 作成元が 4 種であること、effect が何の値であること、カタログに在っても
        // 元にできるとは限らないこと。どれも要求元が名前を用意する前に要る。
        //
        // **名前が何の値かは作成元の分岐が述べる。** 失敗の条件だけが tool の
        // 説明に残る——値を用意する時点と、tool を選ぶ時点は別である。
        let description = description_of("create_object");
        for phrase in [
            "object alias",
            "エフェクト名",
            "effect_not_creatable",
            "effect_not_registered",
        ] {
            assert!(
                description.contains(phrase),
                "create_object の説明が {phrase} に触れていません"
            );
        }
        let source = object_source_description("effect");
        for phrase in ["effect.name", "list_available_effects"] {
            assert!(
                source.contains(phrase),
                "作成元の effect が {phrase} に触れていません: {source}"
            );
        }
    }

    /// 作成元の種別ごとの分岐に付いた説明を取り出す。
    fn object_source_description(kind: &str) -> String {
        let variants =
            tool_named("create_object").input_schema["$defs"]["ObjectSourceInput"]["oneOf"]
                .as_array()
                .expect("作成元が判別子つきの union として宣言されていません")
                .clone();
        let variant = variants
            .iter()
            .find(|variant| variant["properties"]["type"]["const"] == kind)
            .unwrap_or_else(|| panic!("{kind} 種別の分岐がありません"))
            .clone();
        // 分岐そのものの説明と、値のフィールドの説明を併せて見る。名前の由来は
        // どちらに書いても要求元へは 1 つの分岐として届く。
        let own = variant["description"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let fields: String = variant["properties"]
            .as_object()
            .expect("分岐が properties を宣言していません")
            .values()
            .filter_map(|property| property["description"].as_str())
            .collect();
        own + &fields
    }

    #[test]
    fn create_object_says_where_an_alias_name_comes_from_and_how_it_is_checked() {
        // 名前の出どころと使えない文字。どちらも要求元が名前を用意する前に要る。
        // とくに禁止文字は AviUtl2 の UI が課すものであり、我々が決めた制約では
        // ない。**値を書く場所の隣が述べる。**
        let source = object_source_description("alias_name");
        for phrase in [
            "list_object_aliases が返した名前",
            "中身を読む必要は無い",
            r#"\ / : * ? " ' < > | % = , ."#,
            "AviUtl2 の UI",
        ] {
            assert!(
                source.contains(phrase),
                "作成元の alias_name が {phrase} に触れていません: {source}"
            );
        }
        // **作成元どうしを比べない。** 2 つの作成元が通す検証は包含関係に無く、
        // 片方をもう片方より厳しいと述べれば、どちら向きに述べても嘘になる。何が
        // どちらに掛かるかは tool の説明が作成元ごとに述べる。
        for phrase in ["より検証が厳しい", "生テキスト"] {
            assert!(
                !source.contains(phrase),
                "作成元の alias_name が {phrase} と述べています: {source}"
            );
        }
        // 拒否される条件は tool の説明に残る。その呼び出しが失敗する条件そのもので
        // あり、要求を組み立て終えた後に効く。
        let description = description_of("create_object");
        for phrase in ["alias_not_parsable", "alias_without_effect"] {
            assert!(
                description.contains(phrase),
                "create_object の説明が {phrase} に触れていません"
            );
        }
    }

    #[test]
    fn create_object_states_how_a_raw_alias_is_refused_before_anything_is_created() {
        // 生テキストの移動行もテキストの行も書き込みの検証を通る。述べなければ、
        // 要求元は通らない行を含むエイリアスを送り続け、返った名前から直す先を
        // 引けない。行の在処と、1 つも作られていないことは、失敗から立ち直るのに
        // 要る。**在処の名乗りは行の種類で分かれない**——移動行だけの性質として
        // 述べれば、テキストの拒否から details を読む要求元が現れなくなる。
        let description = description_of("create_object");
        for phrase in [
            "表として読めないエイリアスは、source が alias_name でも object_alias でも",
            "source が object_alias のとき、移動行は設定項目へ書くときと同じ検証を通り",
            "track_flags_not_representable",
            "track_mode_unknown",
            "track_mode_not_writable",
            "track_value_count",
            r"テキスト種別（text / string）の設定項目の行は `\` の綴りを検査され",
            r"`\` の次が `n` でも `\` でもない行は invalid_argument（unescaped_backslash）で拒否される",
            "行の拒否は details.item に項目名を載せ、節に属する行では details.heading に節の見出しを載せる",
            "これらの拒否はいずれも作成より前に起き、オブジェクトは 1 つも作られない",
        ] {
            assert!(
                description.contains(phrase),
                "create_object の説明が {phrase} に触れていません"
            );
        }
    }

    #[test]
    fn the_catalog_tools_do_not_ask_for_a_scene_id() {
        // フォント・パレット・モジュール・エイリアスはシーンに紐づかない。何も
        // 守らない値を必須にすると、要求元は意味の無い値を用意することになる。
        for name in [
            "list_fonts",
            "list_palettes",
            "list_modules",
            "list_object_aliases",
        ] {
            let tool = tool_named(name);
            let schema = Value::Object(tool.input_schema.as_ref().clone()).to_string();
            assert!(
                !schema.contains("expected_scene_id"),
                "{name} がシーン ID を宣言しています"
            );
            assert_eq!(
                tool.input_schema["required"],
                serde_json::json!(["instance_id"]),
                "{name} の必須項目"
            );
        }
    }

    #[test]
    fn the_list_modules_input_declares_exactly_the_types_it_accepts() {
        // 種別は SDK が定めた閉じた集合である。値を落とせば既存の要求が
        // invalid_argument になり、綴りを変えれば同じ要求が通らなくなる。
        // どちらも要求元から見れば契約の破壊である。
        let tool = tool_named("list_modules");
        let names = tool.input_schema["$defs"]["ModuleTypeInput"]["enum"]
            .as_array()
            .expect("種別が値の集合として宣言されていません");
        let names: Vec<&str> = names
            .iter()
            .map(|name| name.as_str().expect("種別名は文字列である"))
            .collect();
        assert_eq!(
            names,
            vec![
                "script_filter",
                "script_object",
                "script_camera",
                "script_track",
                "script_module",
                "plugin_input",
                "plugin_output",
                "plugin_filter",
                "plugin_generic",
            ]
        );
    }

    #[test]
    fn the_catalog_tools_say_that_the_revision_is_not_matched() {
        // 受理するが照合しない値である。黙っていると、要求元は 2 ページ目が
        // 落ちない理由も、添えても取りこぼしが防げない理由も分からない。
        //
        // **述べる場所はフィールドの隣である。** 値を送るかどうかを決める時点で
        // 読まれ、共有の入力型に 1 度書けば該当する tool すべてへ届く。
        for name in catalog_page_tools() {
            let description = field_description(&name, "snapshot_revision");
            for phrase in [
                "受理するがページ間の照合に用いない",
                "revision に連動しない",
                "前のページが返した値をそのまま送り返しても拒否されない",
            ] {
                assert!(
                    description.contains(phrase),
                    "{name} の snapshot_revision が {phrase} に触れていません: {description}"
                );
            }
            assert!(
                !description_of(&name).contains("snapshot_revision"),
                "{name} の説明がページ指定を写しています"
            );
        }
    }

    #[test]
    fn list_palettes_states_what_the_colours_and_the_current_name_are() {
        // 色数と不透明度、現在のパレット名の形式、読めなかったパレットの扱い。
        // いずれも応答を受け取る前に知っている必要がある。
        //
        // total_count は本ページで落とした分だけを引いた値であり、ページごとに
        // 違い得る。全体の件数として読んで反復を組むと、集まりきらないまま
        // 終わらないループになる。落ちた分だけ短いページも空のページも起こり
        // 得るため、終端の材料が has_more と next_offset であることも述べる。
        let description = description_of("list_palettes");
        for phrase in [
            "64 件",
            "a は常に 255",
            "透明度の情報を持たない",
            "[ラベル名.パレット名]",
            "total_count から引かれるのは本ページで落とした分だけ",
            "全体の件数として扱わないこと",
            "items が空のまま has_more が true になり得る",
            "has_more と next_offset で終端すること",
        ] {
            assert!(
                description.contains(phrase),
                "list_palettes の説明が {phrase} に触れていません"
            );
        }
    }

    /// tool がページ指定を共有の入力型から受けるか。
    ///
    /// 型を共有しているため、`snapshot_revision` の説明は該当する tool すべてで
    /// 同じ文になる。
    ///
    /// **未知の tool 名で落とす。** 一覧を手書きの連結で持つと、共有の入力型へ
    /// 相乗りした tool をそこへ足し忘れたときに、説明の共有も照合しない旨の
    /// 明記も黙って未検査になる。
    fn takes_the_catalog_page(name: &str) -> bool {
        match name {
            "list_available_effects"
            | "list_fonts"
            | "list_palettes"
            | "list_modules"
            | "list_object_aliases" => true,
            "list_instances"
            | "get_edit_info"
            | "get_current_scene"
            | "list_layers"
            | "list_objects"
            | "get_object"
            // 名前を名指しして引くため、続きのページという概念が無い。
            | "describe_effects"
            | "get_effect_item_values"
            | "get_selection"
            | "create_object"
            | "move_object"
            | "set_object_name"
            | "set_object_item"
            | "add_effect"
            | "set_effect_enabled"
            | "move_effect"
            | "delete_effect"
            | "delete_object"
            | "set_selection"
            | "set_layer_state"
            | "create_object_section"
            | "delete_object_section"
            | "move_object_section"
            | "set_grid_bpm"
            | "set_scene_settings"
            | "apply_batch"
            | "render_frame" => false,
            other => panic!("{other} がページ指定を共有するかが決まっていません"),
        }
    }

    /// ページ指定を共有の入力型から受ける tool の名前を、登録済みの集合から拾う。
    fn catalog_page_tools() -> Vec<String> {
        tools()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .filter(|name| takes_the_catalog_page(name))
            .collect()
    }

    #[test]
    fn the_catalog_tools_share_one_wording_for_the_unmatched_revision() {
        // 入力型を分けると一致が崩れる。文言を特定の対象へ寄せると、共有して
        // いる tool のうち 1 つにしか当てはまらない説明が残りの schema へ載る。
        let names = catalog_page_tools();
        assert!(
            names.len() > 1,
            "共有を確かめるには 2 つ以上の tool が要ります"
        );
        let wordings: Vec<String> = names
            .iter()
            .map(|name| {
                tool_named(name).input_schema["properties"]["snapshot_revision"]["description"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{name} が snapshot_revision の説明を持ちません"))
                    .to_string()
            })
            .collect();
        for (name, wording) in names.iter().zip(&wordings) {
            assert_eq!(
                wording, &wordings[0],
                "{name} の snapshot_revision の説明が他の tool と違います"
            );
            for target in ["effect", "フォント", "パレット", "モジュール", "エイリアス"]
            {
                assert!(
                    !wording.contains(target),
                    "{name} の snapshot_revision の説明が {target} を名指ししています"
                );
            }
        }
    }

    #[test]
    fn the_list_object_aliases_input_flattens_the_page_and_asks_only_for_the_instance() {
        // ページ指定は他の列挙 tool と同じ平坦な形で受ける。入れ子で現れると、
        // 同じ意味の要求が tool ごとに違う形になる。
        let tool = tool_named("list_object_aliases");
        let properties = tool.input_schema["properties"]
            .as_object()
            .expect("入力が properties を宣言していません");
        for field in ["offset", "limit", "snapshot_revision", "label"] {
            assert!(
                properties.contains_key(field),
                "{field} が宣言されていません"
            );
        }
        assert!(
            !Value::Object(tool.input_schema.as_ref().clone())
                .to_string()
                .contains(r#""page""#),
            "ページ指定が入れ子として現れています"
        );
        assert_eq!(
            tool.input_schema["required"],
            serde_json::json!(["instance_id"]),
            "list_object_aliases の必須項目"
        );
        // 宣言した上限は接続前に実際へ確かめる。宣言だけを消しても要求元から
        // 見えるのは schema であり、検証が残っていることは伝わらない。
        assert_eq!(
            properties["label"]["maxLength"],
            serde_json::json!(crate::mcp::input::MAX_NAME_CHARS),
            "label の上限が宣言されていません"
        );
    }

    #[test]
    fn list_object_aliases_states_what_it_returns_and_what_it_refuses() {
        // 名前が作成へそのまま渡る値であること、中身を返さないこと、label が
        // 当てにならないこと、total_count の限定、読み取り専用であること。
        // いずれも応答を受け取る前に知っている必要がある。
        let description = description_of("list_object_aliases");
        for phrase in [
            "create_object の alias_name へそのまま渡す値",
            "一覧に出た名前は必ず作成できる。逆は保証しない",
            "エイリアスの中身は返さない",
            "UI 状態ファイル由来",
            "欠けることがあり",
            "実行中の表示と一致しないことがある",
            "label は識別子ではなく",
            "同じ label を共有し得る",
            "total_count から引かれるのは本ページで落とした分だけ",
            "全体の件数として扱わないこと",
            "has_more と next_offset で終端すること",
            "AviUtl2 の UI で行う",
            "読み取りだけを提供する",
            "unsupported_operation",
        ] {
            assert!(
                description.contains(phrase),
                "list_object_aliases の説明が {phrase} に触れていません"
            );
        }
    }

    #[test]
    fn list_modules_admits_that_the_list_can_be_incomplete() {
        // 種別を解釈できないモジュールは一覧へ現れない。黙っていると、要求元は
        // 一覧を登録物の全体だと読む。
        let description = description_of("list_modules");
        for phrase in ["既知の 9 種別", "欠落し得る"] {
            assert!(
                description.contains(phrase),
                "list_modules の説明が {phrase} に触れていません"
            );
        }
    }

    #[test]
    fn the_create_object_input_declares_exactly_the_sources_it_accepts() {
        // 作成元は判別子つきの union であり、未知フィールドを拒否する。variant を
        // 落とせば既存の要求が invalid_argument になり、タグ名を変えれば同じ要求が
        // 通らなくなる。どちらも要求元から見れば契約の破壊である。
        // 出力側と違い、入力 schema を丸ごと固定する検査は無い。作成元だけは
        // ここで塞ぐ。
        let tool = tool_named("create_object");
        let variants = tool.input_schema["$defs"]["ObjectSourceInput"]["oneOf"]
            .as_array()
            .expect("作成元が判別子つきの union として宣言されていません");

        let tags: Vec<&str> = variants
            .iter()
            .map(|variant| {
                variant["properties"]["type"]["const"]
                    .as_str()
                    .expect("判別子が固定値として宣言されていません")
            })
            .collect();
        assert_eq!(
            tags,
            vec!["media_file", "object_alias", "effect", "alias_name"]
        );

        // 判別子と対になる値のフィールド名も固定する。タグだけが合っていても、
        // 値の名前が動けば要求は通らない。
        let fields: Vec<Vec<&str>> = variants
            .iter()
            .map(|variant| {
                variant["required"]
                    .as_array()
                    .expect("必須フィールドが宣言されていません")
                    .iter()
                    .map(|field| field.as_str().expect("フィールド名"))
                    .collect()
            })
            .collect();
        assert_eq!(
            fields,
            vec![
                vec!["type", "path"],
                vec!["type", "alias"],
                vec!["type", "name"],
                vec!["type", "name"],
            ]
        );
    }

    /// 応答が返す位置が要求した宛先と一致するとは限らない tool。
    ///
    /// ホストが配置を調整し得るため、成功を「要求どおりの位置」と読むと、
    /// 呼び出し側が組み立てた次の要求は別の場所を指す。どちらの側に属するかを
    /// 表で固定するので、tool を足したときに素通りしない。
    const TOOLS_WHOSE_RESPONSE_CARRIES_THE_ACTUAL_PLACEMENT: &[&str] =
        &["create_object", "move_object"];

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
        /// 呼び出し全体が 1 つの取り消し単位になると述べる。
        ///
        /// **一括適用にとってはそれが tool の目的そのものである。** 一般則の
        /// 言い換えではないため層 1 に残る。
        ItsWholePurpose,
        /// 一般則（1 回の呼び出しが 1 つの取り消し単位になる）に従う。
        ///
        /// **層 1 では述べない。** 編集 tool すべてに掛かる規約であり、層 1 へ
        /// 写すと同じ 1 行が tool の数だけ並ぶ。組み立ての段階で効く——
        /// `apply_batch` を選ぶかの判断材料である。
        FollowsTheGeneralRule,
        /// 取り消し単位を作らず、取り消しが 1 つ前の編集へ飛ぶと述べる。
        ///
        /// **一般則の例外である。** 例外は一般則を置いた層には書けない——
        /// 一般則を読む全 tool へ伝わってしまう。
        NoUnitAndJumpsBack,
        /// 取り消せないことを説明の冒頭で述べ、取り消しが 1 つ前の編集へ飛ぶと
        /// 述べる。
        ///
        /// **冒頭に置くことまでを含めて固定する。** 末尾では、説明を要約する
        /// 要求元が落とす。
        NotUndoableAndJumpsBack,
    }

    /// tool 名から、説明が取り消しについて述べる内容を引く。
    ///
    /// 未知の tool 名で落とす。**説明は保証である**ため、述べるか黙るかの判断を
    /// tool ごとに 1 か所へ置き、tool を足したときに素通りしないようにする。
    fn undo_statement(name: &str) -> UndoStatement {
        match name {
            "apply_batch" => UndoStatement::ItsWholePurpose,
            "create_object"
            | "move_object"
            | "set_object_name"
            | "set_object_item"
            | "add_effect"
            | "set_effect_enabled"
            | "move_effect"
            | "delete_effect"
            | "delete_object"
            | "set_layer_state"
            | "create_object_section"
            | "delete_object_section"
            | "move_object_section" => UndoStatement::FollowsTheGeneralRule,
            // BPM グリッドはオブジェクトの編集ではない。SDK が編集区間の中で
            // Undo へ登録すると述べているのはオブジェクトについてであり、実機でも
            // 直後の取り消しは 1 つ前の編集へ飛んだ。
            "set_selection" | "set_grid_bpm" => UndoStatement::NoUnitAndJumpsBack,
            // SDK は 3 つの setter を Undo 非対応と明記している。取り消せない
            // ことは、要求を出す前に読まれる場所へ置く。
            "set_scene_settings" => UndoStatement::NotUndoableAndJumpsBack,
            other => panic!("{other} の取り消しの説明が定義されていません"),
        }
    }

    #[test]
    fn edit_tool_descriptions_state_the_undo_boundary() {
        for name in edit_like_tools() {
            let description = description_of(name);
            match undo_statement(name) {
                UndoStatement::ItsWholePurpose => assert!(
                    description.contains("1 つの取り消し単位"),
                    "{name} の説明に取り消し単位がありません"
                ),
                // 一般則に従う tool は層 1 で黙る。言い換えも塞ぐ——1 つの語だけを
                // 見ていると、別の言い回しで同じ保証が入り込む。
                UndoStatement::FollowsTheGeneralRule => {
                    for forbidden in ["取り消し単位", "取り消し", "元に戻", "Undo", "undo"]
                    {
                        assert!(
                            !description.contains(forbidden),
                            "{name} の説明が層 3 の一般則か未確認の挙動に触れています: {forbidden}"
                        );
                    }
                }
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
                UndoStatement::NotUndoableAndJumpsBack => {
                    // 要約する要求元は末尾を落とすため、冒頭に在ることまでを
                    // 固定する。
                    assert!(
                        description.starts_with("この操作は取り消せない"),
                        "{name} の説明が取り消せないことを冒頭で述べていません"
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

    /// 説明が「選択肢から選ぶ種別」として挙げている名前を並びごと取り出す。
    ///
    /// 語を含むかではなく、挙げている一覧そのものを取り出す。含むかだけを見ると、
    /// 一覧から種別が落ちても増えても気付けない。
    fn choice_item_types_named_in(description: &str) -> Vec<String> {
        const OPENING: &str = "選択肢から選ぶ種別（";
        let start = description
            .find(OPENING)
            .expect("説明が選択肢から選ぶ種別を挙げていません")
            + OPENING.len();
        let rest = &description[start..];
        let end = rest.find('）').expect("種別の一覧が閉じていません");
        rest[..end].split('・').map(str::to_string).collect()
    }

    /// 移動を含まない値を渡すときの対象。
    ///
    /// 移動の検証は対象を見なければ成立しないが、ここで見るのは種別と値の形の
    /// 対応であり、移動は渡さない。
    fn no_track_target() -> aviutl2_mcp_core::TrackWriteTarget<'static> {
        aviutl2_mcp_core::TrackWriteTarget {
            section_count: 0,
            movements: &[],
        }
    }

    /// 選択肢の値を書き込める設定項目種別を、書き込みの検証そのものから集める。
    ///
    /// 判定を書き写さず、公開されている入口へ選択肢の値を渡して受理されるかで
    /// 決める。書き込みを公開する種別と、種別が受け付ける値の形の、どちらが
    /// 動いてもここが動く。
    fn item_types_accepting_a_choice() -> Vec<String> {
        let mut names = Vec::new();
        for item_type in EffectItemType::ALL {
            let items = vec![AvailableEffectItem {
                name: "項目".to_string(),
                item_type: item_type.clone(),
            }];
            let value = ItemValue::Choice {
                value: "四角形".to_string(),
            };
            if prepare_item_write(&items, "項目", &value, no_track_target()).is_ok() {
                names.push(item_type.kind_name());
            }
        }
        names
    }

    #[test]
    fn the_description_names_every_item_type_that_takes_a_choice() {
        // 説明は保証である。挙げた種別だけが選択肢として書けると読まれるため、
        // 一覧が実態と食い違えば、書ける種別が使われないか、書けない種別が
        // 当て推量で試される。並びごと突き合わせるため、落としても足しても
        // 順序を変えても落ちる。
        let named = choice_item_types_named_in(&description_of("set_object_item"));
        assert!(!named.is_empty(), "説明が種別を 1 つも挙げていません");
        assert_eq!(
            named,
            item_types_accepting_a_choice(),
            "説明が挙げる種別と、選択肢の値を受け付ける種別が食い違います"
        );
    }

    /// 書き込みを公開する種別のうち、書き込み後に照合しないものを集める。
    ///
    /// 判定を書き写さず、公開されている入口の答えから決める。公開の可否と照合の
    /// しかたの、どちらが動いてもここが動く。
    fn writable_item_types_without_read_back() -> Vec<String> {
        let mut names = Vec::new();
        for item_type in EffectItemType::ALL {
            let items = vec![AvailableEffectItem {
                name: "項目".to_string(),
                item_type: item_type.clone(),
            }];
            let probe = ItemValue::Text {
                value: "文字列".to_string(),
            };
            // 種別への書き込みを公開しているかは、値の形の照合より先に決まる。
            // 形が合わない値を渡しても判定は変わらない。
            let writable = !matches!(
                prepare_item_write(&items, "項目", &probe, no_track_target()),
                Err(ItemWriteError::UnsupportedItemType { .. })
            );
            if writable
                && matches!(
                    read_back_check(item_type, &probe),
                    ReadBackCheck::Declared { .. }
                )
            {
                names.push(item_type.kind_name());
            }
        }
        names
    }

    #[test]
    fn the_description_states_that_every_write_is_verified_by_reading_back() {
        // 説明は保証である。照合しない種別が生まれたのに説明が「全ての種別」を
        // 名乗り続けると、要求元は掛かっていない検査を前提に書き込みを組む。
        // 実装だけを直した場合も、説明だけを直した場合も落ちる。
        let unverified = writable_item_types_without_read_back();
        assert!(
            unverified.is_empty(),
            "照合しない種別 {unverified:?} を説明が挙げていません"
        );
        assert!(
            description_of("set_object_item").contains(
                "書き込みは全ての種別で、書いた直後に読み直して要求した値が入ったかを照合する"
            ),
            "説明が全種別の照合を述べていません"
        );
    }

    /// 設定値の種別ごとの分岐に付いた説明を取り出す。
    fn item_value_description(kind: &str) -> String {
        let tool = tool_named("set_object_item");
        let variants = tool.input_schema["$defs"]["ItemValueInput"]["oneOf"]
            .as_array()
            .expect("設定値が判別子つきの union として宣言されていません")
            .clone();
        variants
            .iter()
            .find(|variant| variant["properties"]["type"]["const"] == kind)
            .unwrap_or_else(|| panic!("{kind} 種別の分岐がありません"))["description"]
            .as_str()
            .unwrap_or_else(|| panic!("{kind} 種別に説明がありません"))
            .to_string()
    }

    #[test]
    fn the_numeric_item_descriptions_name_the_only_item_type_that_takes_them() {
        // `accepts` が通すのは (Integer, Integer) と (Number, Number) だけである。
        // 片方をもう片方の項目へ書けると読める説明は、通らない要求を組み立て
        // させる——しかも失敗するのは invalid_argument であり、値を選び直しても
        // 直らない。**2 つは対称であり、片方だけを名指しすると残りが黙って古くなる。**
        for (kind, other) in [("integer", "number"), ("number", "integer")] {
            let description = item_value_description(kind);
            assert!(
                description.contains(&format!("item_type: {kind}")),
                "{kind} が書ける種別を名指ししていません: {description}"
            );
            assert!(
                description.contains(&format!("item_type: {other}"))
                    && description.contains("invalid_argument"),
                "{kind} が {other} の項目で落ちることを述べていません: {description}"
            );
            assert!(
                !description.contains(&format!("{other} と同じ")),
                "{kind} が {other} と同じ扱いだと読めます: {description}"
            );
        }

        // 説明を実装から確かめる。値の形と種別は 1 対 1 で対応する。
        for (value, accepting) in [
            (ItemValue::Integer { value: 1 }, EffectItemType::Integer),
            (
                ItemValue::Number {
                    value: aviutl2_mcp_core::FiniteF64::try_new(1.0).expect("有限である"),
                },
                EffectItemType::Number,
            ),
        ] {
            for item_type in [EffectItemType::Integer, EffectItemType::Number] {
                let accepted = item_type == accepting;
                let items = vec![AvailableEffectItem {
                    name: "項目".to_string(),
                    item_type,
                }];
                assert_eq!(
                    prepare_item_write(&items, "項目", &value, no_track_target()).is_ok(),
                    accepted,
                    "数値の値が受け付けられる種別と説明が食い違います"
                );
            }
        }
    }

    #[test]
    fn the_color_item_description_states_which_notation_the_host_accepts() {
        // 説明は保証である。受理される書式を挙動から導く材料が要求元の側に
        // 無い——外れた書式は失敗するが、何が正解かは失敗からは分からない。
        // **実測で確定した 2 点だけを書く。** 8 桁のアルファ付きなどは観測して
        // いないため、通るとも通らないとも述べない。
        assert_eq!(
            item_value_description("color"),
            "色。16 進 6 桁（例 `ff8800`）で指定する。読み直すと小文字で返る。\n\
             `#` を付けた表記と 3 桁の省略形は受け付けられず、指定した色にならない\n\
             だけでなく元の色も失われて白（`ffffff`）になる。\n\
             受け付けられなかったことは書き込みの応答が\n\
             unsupported_operation で伝える。"
        );
    }

    #[test]
    fn the_font_item_description_points_at_the_registered_names() {
        // 登録済みの名前を得る手段を示さなければ、要求元は当て推量を繰り返す。
        // 外れた名前は失敗し、設定項目は変更前のまま残る。
        assert_eq!(
            item_value_description("font"),
            "フォント名。list_fonts が返す登録済みの名前をそのまま指定する。\n\
             登録されていない名前は書き込みが unsupported_operation となり、\n\
             設定項目は変更前の値のまま残る。"
        );
    }

    #[test]
    fn the_text_item_description_states_what_survives_the_round_trip() {
        // 説明は保証である。書いた値がそのまま返ること、CRLF が LF になること、
        // 単独の CR を受け付けないことは、挙動から導く材料が要求元の側に無い。
        // 文言そのものを固定する。
        assert_eq!(
            item_value_description("text"),
            "テキスト。改行とタブを含めて書き込め、読み直すと書いたとおりに返る。\n\
             バックスラッシュも書いたとおりに保たれるため、Windows パス・正規表現・\n\
             LaTeX をそのまま指定できる。CRLF は LF として保存される。単独の CR は\n\
             受け付けない——保存はされるが描画では行が分かれず、意図を推測できない。\n\
             長さの上限は保存される表記に掛かり、`\\` と改行はそれぞれ 2 バイトを\n\
             占める。"
        );
    }

    #[test]
    fn edit_tool_descriptions_state_the_operation_specific_hazards() {
        let hazards: &[(&str, &[&str])] = &[
            (
                "create_object",
                &["全てが作成され", "自動調整", "重複して作成"],
            ),
            ("add_effect", &["fingerprint", "重複して付与"]),
            (
                "set_object_item",
                &[
                    "公開していない設定項目種別",
                    "item_type",
                    // 有効な値の一覧を返す手段が無いため、外した値の直し方を
                    // 示さなければ要求元は当て推量を繰り返す。
                    "item_value_not_applied",
                    "details.observed_value に書き込んだ直後に読み直した値が入る",
                    // 巻き戻すことを述べなければ、要求元は失敗のたびに対象を
                    // 読み直す。**戻した先を機械可読な値でも名乗る**——文章が
                    // 「読み直す必要は無い」と述べる隣で details.retry_requires が
                    // refetch を名乗っていれば、要求元は値のほうに従う。
                    "設定項目は書き込み前の値へ戻す",
                    "details.retry_requires は none になる",
                    "選択肢に無い値",
                    // クランプと丸めも失敗になることを予期できなければ、要求元は
                    // 成功するはずの要求が落ちたと読む。
                    "クランプ",
                    "丸められた",
                    // observed_value をそのまま送り直すのは、要求を諦めることで
                    // 成功させる動きである。推奨と読まれると、白へ化けた色を
                    // 書き直して成功と判断する経路ができる。**巻き戻した後は、
                    // それが「現在値の再現」にもなる。**
                    "要求の代わりに送り直す値でもない",
                    "受け付けられる値を選び直す",
                    // current_value も送り直す値ではない。**中身は alias の生の
                    // 綴りであり、入力に生文字列の形が無い。** 断りが片側だけに
                    // 在ると、断りの無いほうは送り返せると読まれる。
                    "そのまま送り返せる形ではない",
                    "get_object が返す track の形で組み直す",
                ],
            ),
            ("set_effect_enabled", &["出力 item", "読み直した effect"]),
            (
                "move_effect",
                &[
                    "effect_not_movable",
                    "effect_position_out_of_range",
                    // **上限と下限は振る舞いが違う。** 上限は発行される前に
                    // 落ち、下限は発行されたうえで切り詰められる。同じ
                    // 「範囲外」として読まれると、要求元は先頭への移動が
                    // 列を動かさないものと信じる。
                    "変更は発行されない",
                    "入力 item・出力 item より",
                    "切り詰める",
                    // 失敗が状態を残さないことを述べなければ、要求元は失敗の
                    // たびに列を読み直す。戻せなかった場合だけは読み直しが要る。
                    "元の並びへ戻す",
                    "details.restored",
                    "details.consistency_unknown",
                    "details.retry_requires は none になる",
                    // 切り詰めは列を動かすとは限らない。既に下限に居る対象へ
                    // 同じ要求を送ると列は動かず、selector も生き残る。**その
                    // 失敗も戻った側として名乗る**——動かさなかったことと動かして
                    // から戻したことは、要求元から見て区別が付かない。
                    "列が 1 件も動かなかった場合も details.restored は真になる",
                    "selector がそのまま通る",
                    // **移動は effect の内容を 1 つも変えないまま、要求に
                    // 使った selector を無効にし得る。** 名前も enabled も
                    // 設定項目も動かないため、変わらないと読める。述べ
                    // なければ、要求元は成功の直後に古い selector を送って
                    // precondition_failed を踏む。どの条件で無効になるかは
                    // [`the_move_effect_description_never_voids_the_selector_unconditionally`]
                    // が見る。
                    "selector は使えなくなる",
                    "fingerprint が変わり",
                    "effect_index も入れ替わる",
                    // 無効になった selector の代わりが応答に在ることを示さ
                    // なければ、対象を読み直す経路しか残らない。
                    "応答の effect.selector を使う",
                ],
            ),
            ("delete_effect", &["not_found", "兄弟 effect"]),
            ("delete_object", &["not_found"]),
            (
                "set_selection",
                &[
                    "原子的",
                    "クランプ",
                    "全てを省略",
                    // クランプの扱いが軸ごとに違うことを読み分けられなければ、
                    // cursor のクランプも not_applied に出ると読める。
                    "cursor はクランプされても applied に入る",
                    "display はクランプされると not_applied に入る",
                ],
            ),
            (
                "set_layer_state",
                &[
                    // レイヤーは fingerprint を持たない。読み取り時からの変化を
                    // 検出できないことは、この tool でしか起きない。
                    "fingerprint",
                    "全てを省略した要求は受け付けない",
                    "この tool 自身はロックの影響を受けない",
                    // 止まる tool の列挙が足りないと、この説明を読んだ要求元は
                    // 実際には止まる tool を止まらないものとして扱う。
                    "move_object",
                    "create_object_section",
                    "delete_object_section",
                    "move_object_section",
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

    #[test]
    fn the_move_effect_description_never_voids_the_selector_unconditionally() {
        // 列の位置が変わらなかった移動は、成功しても selector を無効にしない。
        // **握るのは言い回しではなく、断定の手前に条件が在ることである。**
        // 後続の句が条件を述べていても、断定が先に立てば要約として読まれ、
        // 要求元は動かなかった列に対しても対象を読み直す。
        let description = description_of("move_effect");
        let (before, _) = description
            .split_once("selector は使えなくなる")
            .expect("move_effect の説明が selector の無効化を述べていません");
        let clause = before.rsplit('。').next().expect("句を切り出せません");
        assert!(
            ["れば", "場合", "とき", "なら"]
                .iter()
                .any(|mark| clause.contains(mark)),
            "move_effect の説明が selector の無効化を無条件で述べています: {clause}"
        );
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
    /// 未知の tool 名で落とす。ロックが止める範囲を決めるのはホストであり、
    /// 対象を 1 か所へ置いて tool を足したときに素通りしないようにする。
    fn layer_lock_statement(name: &str) -> LayerLockStatement {
        match name {
            "create_object"
            | "move_object"
            | "delete_object"
            | "create_object_section"
            | "delete_object_section"
            | "move_object_section"
            // 一括適用が止まるのは move_object を含む場合だけだが、止まり方も
            // 解き方も同じであるため、案内する側に属する。
            | "apply_batch" => LayerLockStatement::StoppedAndNamesTheWayOut,
            "set_layer_state" => LayerLockStatement::DescribesTheScope,
            "set_object_name"
            | "set_object_item"
            | "add_effect"
            | "set_effect_enabled"
            | "move_effect"
            | "delete_effect"
            | "set_selection"
            // BPM グリッドとシーン設定はシーンに属し、どのレイヤーの対象にも
            // 触れない。
            | "set_grid_bpm"
            | "set_scene_settings" => LayerLockStatement::Silent,
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
                        description.contains("set_layer_state"),
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
            "move_object"
            | "set_object_name"
            | "set_object_item"
            | "add_effect"
            | "set_effect_enabled"
            | "move_effect"
            | "delete_effect"
            | "delete_object"
            | "set_selection"
            | "create_object_section"
            | "delete_object_section"
            | "move_object_section" => true,
            // 一括適用は 100 件のうちどれが落ちたかを併せて示す必要があるため、
            // 別のキー（failed_object）で返す。
            "create_object" | "set_layer_state" | "set_grid_bpm" | "set_scene_settings"
            | "apply_batch" => false,
            other => panic!("{other} が現在の姿を返すかが定義されていません"),
        }
    }

    #[test]
    fn no_tool_description_repeats_how_to_read_the_current_object() {
        // `details` の値は失敗の text content へキーごと出るようになった。
        // **キーが在ることを説明する必要はもう無く、要るのは値の使い方である。**
        // それは selector の使い方そのものであるため共有型の説明が持ち、
        // 11 tool の説明から落ちる。
        //
        // 一覧そのものは [`returns_a_current_object`] が保ち続ける。落とすのは
        // 説明であって事実ではない。
        for name in edit_like_tools() {
            assert!(
                !description_of(name).contains("details.current_object"),
                "{name} の説明が共有型の写しを持っています"
            );
        }
        assert!(
            shared_type_description("get_object", "ObjectSelectorInput").contains("対象の現在の姿"),
            "selector の説明が現在の姿の使い方を述べていません"
        );
        // **キー名を述べる tool は apply_batch だけである。** そちらは 100 件の
        // うちどれが落ちたかを併せて示す必要があるため、別のキーで返す。
        assert!(
            description_of(APPLY_BATCH).contains("details.failed_object"),
            "一括適用の説明が自分のキーを述べていません"
        );
        // 表が古びないよう、編集 tool の集合と突き合わせる。どちらの側に属するかは
        // tool ごとに決まる事実であり、説明を落としても失われない。
        let returning: Vec<&str> = edit_like_tools()
            .into_iter()
            .filter(|name| returns_a_current_object(name))
            .collect();
        assert_eq!(
            returning.len(),
            12,
            "現在の姿を返す tool の数が変わりました: {returning:?}"
        );
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
            // 全て戻った先は要求の前と同じである。巻き戻したこと自体が案内を
            // 決めると読まれると、要求元は解消しない失敗にも読み直しを重ねる。
            "止めた失敗そのものが決める",
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
            // 描けるのは現在シーンだけである。
            "現在シーンだけ",
            "expected_scene_id",
            "get_edit_info",
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
    fn input_schemas_declare_the_expected_epoch_only_where_it_is_used() {
        // 前提の epoch を持つのは、要求が対象を指す selector を 1 つも運ばない
        // ことがある tool だけである。必ず運ぶ tool へ宣言すると、同じ意味の値が
        // 1 要求の 2 か所へ並ぶ。
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
    fn only_deleting_a_section_is_annotated_as_destructive() {
        // 中間点を消すとその位置の移動パラメータが失われ、同じ tool では戻せない。
        // 作成と移動は戻せるため、3 つを 1 つの tool へまとめず annotation を
        // 分けている。
        let destructive = |name: &str| {
            tool_named(name)
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.destructive_hint)
        };
        assert_eq!(destructive("delete_object_section"), Some(true));
        assert_eq!(destructive("create_object_section"), Some(false));
        assert_eq!(destructive("move_object_section"), Some(false));
    }

    #[test]
    fn the_batch_input_schema_does_not_accept_a_section_change() {
        // 削除した中間点の移動パラメータを復元する手段が無く、
        // delete_object_section の逆操作を構築できない。3 つのうち一部だけを
        // 入れる形も採らない。
        let schema = tool_named(APPLY_BATCH).input_schema.clone();
        let declared = Value::Object(schema.as_ref().clone()).to_string();
        // BPM グリッドの置き換えは戻り値を持たず、成否を戻り値から知れない。
        // read-back で確認できるが、選定の基準は「戻り値で成否が分かる」ことで
        // あり例外を作らない。
        for name in [
            "create_object_section",
            "delete_object_section",
            "move_object_section",
            "set_grid_bpm",
        ] {
            assert!(
                !declared.contains(name),
                "{name} が一括適用の入力 schema に現れています"
            );
        }
        // 受け付ける 2 種は現れる。走査そのものが働いていることを併せて固定する。
        for name in ["move_object", "set_object_item"] {
            assert!(declared.contains(name), "{name} が入力 schema にありません");
        }
    }

    #[test]
    fn the_effect_item_values_input_schema_declares_the_uniqueness_it_enforces() {
        // 重複した要求は `invalid_argument` で落ちる。件数の境界だけを宣言して
        // 一意性を伏せると、同じ 1 つのフィールドについて契約の一部だけが
        // 要求元から見えなくなる。検証の実体は core の validate である。
        let tool = tool_named("get_effect_item_values");
        for (field, max) in [
            ("frames", aviutl2_mcp_core::MAX_EVALUATED_FRAMES),
            ("items", aviutl2_mcp_core::MAX_EVALUATED_ITEMS),
        ] {
            let property = tool.input_schema["properties"][field].clone();
            assert_eq!(
                property["uniqueItems"],
                serde_json::json!(true),
                "{field} が一意性を宣言していません: {property}"
            );
            // 一意性が件数の宣言と同じ位置に付いていることまで見る。位置が
            // ずれると、宣言は在るのに要求元の検証器が読まない。
            assert_eq!(property["minItems"], serde_json::json!(1), "{field}");
            assert_eq!(property["maxItems"], serde_json::json!(max), "{field}");
        }
    }

    #[test]
    fn the_grid_bpm_input_schema_declares_the_limits_it_enforces() {
        // 宣言した制約は server 側で実際に検証する。検証していない宣言を
        // schema に残さない。検証の実体は core の validate である。
        let tool = tool_named("set_grid_bpm");
        let entries = tool.input_schema["properties"]["entries"].clone();
        assert_eq!(
            entries["maxItems"],
            serde_json::json!(aviutl2_mcp_core::MAX_GRID_BPM_ENTRIES)
        );
        // 0 件はグリッドを消す指定である。下限を宣言すると手段が無くなる。
        assert!(entries.get("minItems").is_none());
        let beat = tool.input_schema["$defs"]["GridBpmInput"]["properties"]["beat"].clone();
        assert_eq!(beat["minimum"], serde_json::json!(1));
        assert_eq!(beat["maximum"], serde_json::json!(i32::MAX));
    }

    #[test]
    fn the_grid_bpm_description_states_that_the_whole_list_is_replaced() {
        // 部分更新だと読まれると、指定しなかった要素が消えたことに要求元は
        // 気付けない。
        let description = description_of("set_grid_bpm");
        for keyword in [
            "部分更新ではない",
            "指定しなかった要素は消える",
            "get_edit_info",
            "置き換え前の一覧を保持していなければ",
            "空配列",
            "256 件",
            "昇順は求めない",
            "duplicate_target",
            "grid_bpm_out_of_range",
            "argument_not_representable",
            "change_not_applied",
        ] {
            assert!(
                description.contains(keyword),
                "set_grid_bpm の説明に {keyword} がありません"
            );
        }

        // 位置の単位はフレーム番号ではない。取り違えると桁が変わるため、値を
        // 書く場所の隣が述べる。
        let entry =
            tool_named("set_grid_bpm").input_schema["$defs"]["GridBpmInput"]["properties"].clone();
        for field in ["start", "offset"] {
            let field_description = entry[field]["description"]
                .as_str()
                .unwrap_or_else(|| panic!("{field} に説明がありません"));
            assert!(
                field_description.contains("秒であり、フレーム番号ではない"),
                "{field} が単位を述べていません: {field_description}"
            );
        }
    }

    /// 入力 schema の必須フィールドを宣言順に取り出す。
    fn required_fields(tool: &Tool) -> Vec<&str> {
        tool.input_schema["required"]
            .as_array()
            .expect("必須フィールドが宣言されていません")
            .iter()
            .map(|field| field.as_str().expect("フィールド名"))
            .collect()
    }

    #[test]
    fn the_scene_settings_input_declares_three_optional_axes_and_two_preconditions() {
        // 出力側と違い、入力 schema を丸ごと固定する検査は無い。新設の tool は
        // 軸の名前・入れ子の形・前提条件の必須指定をここで塞ぐ。軸を落とせば
        // 既存の要求が invalid_argument になり、前提条件が省略可能へ緩めば
        // プロジェクト境界を照合する材料が消える。
        let tool = tool_named("set_scene_settings");
        let properties = tool.input_schema["properties"]
            .as_object()
            .expect("properties がある");
        let required = required_fields(&tool);
        for axis in ["name", "size", "sample_rate"] {
            assert!(properties.contains_key(axis), "{axis} が宣言されていません");
            assert!(!required.contains(&axis), "{axis} が必須になっています");
        }
        assert_eq!(
            required,
            vec!["instance_id", "expected_scene_id", "expected_project_epoch"]
        );

        // 解像度は組でしか綴れない。片方だけの指定は必須欠落として落ちる。
        let size = tool.input_schema["$defs"]["SceneSizeInput"].clone();
        assert_eq!(size["type"], serde_json::json!("object"));
        assert_eq!(
            size["required"]
                .as_array()
                .expect("必須フィールドが宣言されていません")
                .iter()
                .map(|field| field.as_str().expect("フィールド名"))
                .collect::<Vec<_>>(),
            vec!["width", "height"]
        );

        // 組でしか綴れないことは、値を書く場所の隣が述べる。
        assert!(
            field_description("set_scene_settings", "size")
                .contains("width と height は組で指定する"),
            "size が組であることを述べていません"
        );

        // 綴りの誤った軸が黙って無視されないこと。
        assert_eq!(
            tool.input_schema.get("additionalProperties"),
            Some(&serde_json::json!(false))
        );
    }

    #[test]
    fn the_scene_settings_input_does_not_try_to_express_the_at_least_one_rule() {
        // 「3 つのいずれかを必ず指定する」は schema で表せない。組み合わせを
        // oneOf で並べると、要求元から見た schema が 7 通りに割れる。判定は
        // 実行時の検証が担うため、表そうとしていないことを固定する。
        let tool = tool_named("set_scene_settings");
        for keyword in [
            "oneOf",
            "anyOf",
            "allOf",
            "not",
            "minProperties",
            "dependentRequired",
        ] {
            assert!(
                tool.input_schema.get(keyword).is_none(),
                "入力 schema が {keyword} で軸の組み合わせを表そうとしています"
            );
        }
    }

    #[test]
    fn the_scene_settings_tool_says_it_cannot_be_undone_before_it_is_called() {
        // 要求を出す前に読まれる口は 2 つある。人と要求を組み立てる LLM が読む
        // 説明の冒頭と、機械が読む destructiveHint である。要求のあとに読める
        // 3 つ目の口（応答の non_undoable）は統合テストが確かめる。
        let tool = tool_named("set_scene_settings");
        let description = tool.description.as_ref().expect("説明がある");
        assert!(
            description.starts_with("この操作は取り消せない"),
            "説明の冒頭に取り消せない旨がありません: {description}"
        );
        // destructiveHint の根拠は削除ではなく不可逆性である。削除系と同じ組を
        // 採るが、削除は取り消しで戻るのに対しこれは戻らない。
        assert_eq!(
            tool.annotations
                .as_ref()
                .expect("annotation がある")
                .destructive_hint,
            Some(true)
        );
    }

    #[test]
    fn the_scene_settings_description_states_what_costs_the_caller_if_assumed_wrong() {
        // どれも要求を組み立てる前に要る事項であり、誤った前提で操作すると
        // 取り消せない変更が残る。
        let description = description_of("set_scene_settings");
        for keyword in [
            // 一括適用に入らない。
            "apply_batch に含められない",
            // 何を省略でき、何を省略できないか。
            "3 つ全てを省略した要求は受け付けない",
            "空の名前は指定できず",
            "「標準へ戻す」が無く",
            // 値域と、変更できない軸。
            "render_frame が描ける大きさ",
            "フレームレートは変更できない",
            "get_current_scene",
            // 読み取り時からの変化を検出できないこと。
            "fingerprint",
            "変更後に観測した実際の状態",
            // 観測が編集と原子的でないこと。
            "原子的に観測したものではない",
            "observed_after_edit",
            // 名前だけは区間の内側で照合すること。
            "区間の内側で照合",
            "change_not_applied",
            // 応答から取り消せないことを読める場所。
            "non_undoable",
        ] {
            assert!(
                description.contains(keyword),
                "set_scene_settings の説明に {keyword} がありません"
            );
        }

        // 説明が名指しする上限は、実際に課している値から導く。数を直に書いた
        // まま定数を動かすと、説明だけが黙って古くなる。
        let mib = aviutl2_mcp_core::render::MAX_RENDER_FRAME_BYTES / (1024 * 1024);
        assert!(
            description.contains(&format!("{mib} MiB")),
            "set_scene_settings の説明が実際の上限を述べていません"
        );
    }

    #[test]
    fn the_section_input_schemas_declare_the_lower_bound_they_enforce() {
        // 宣言した制約は server 側で実際に検証する。検証していない宣言を
        // schema に残さない。検証の実体は core の validate である。
        for name in ["delete_object_section", "move_object_section"] {
            let tool = tool_named(name);
            let section = tool.input_schema["properties"]["section"].clone();
            assert_eq!(
                section["minimum"],
                serde_json::json!(1),
                "{name} の section が下限を宣言していません"
            );
        }
        // 追加は区間番号を取らない。宣言する下限も無い。
        assert!(
            tool_named("create_object_section").input_schema["properties"]
                .get("section")
                .is_none()
        );
    }

    #[test]
    fn section_tool_descriptions_explain_the_index_correspondence() {
        // 「区間の番号」と「中間点の番号」が 1 つずれることは、要求元が自力で
        // 気付ける情報ではない。
        //
        // **区間番号を引数に取る 2 tool では、値を書く場所の隣が述べる。**
        // 追加は区間番号を取らないため、応答の sections の形を述べる側として
        // 説明が持つ。
        for name in ["delete_object_section", "move_object_section"] {
            let field = field_description(name, "section");
            for keyword in [
                "sections[i] が区間番号 i",
                "sections[0].start はオブジェクトの開始フレームであって中間点ではない",
            ] {
                assert!(
                    field.contains(keyword),
                    "{name} の section に {keyword} がありません: {field}"
                );
            }
            assert!(
                !description_of(name).contains("sections[i] が区間番号 i"),
                "{name} の説明が入力 schema の写しを持っています"
            );
        }
        for keyword in [
            "sections[i] が区間番号 i",
            "sections[0].start はオブジェクトの開始フレームであって中間点ではない",
        ] {
            assert!(
                description_of("create_object_section").contains(keyword),
                "create_object_section の説明に {keyword} がありません"
            );
        }
        // 応答の sections の形は 3 つとも同じであり、いずれも説明が述べる。
        for name in [
            "create_object_section",
            "delete_object_section",
            "move_object_section",
        ] {
            assert!(
                description_of(name).contains("sections の末尾の end はオブジェクトの終了フレーム"),
                "{name} の説明が応答の sections の形を述べていません"
            );
        }
        // フレームの意味も要求元が自力では決められない。
        for name in ["create_object_section", "move_object_section"] {
            assert!(
                description_of(name).contains("シーンの絶対フレーム番号"),
                "{name} の説明が frame の意味を述べていません"
            );
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

    /// 入力 schema に現れる property を、名前と説明の対で集める。
    ///
    /// `$defs` の中も辿る。共有の入力型はそこへ展開されるため、辿らなければ
    /// selector も設定値も 1 つも見えない。
    fn property_descriptions(schema: &Value) -> Vec<(String, String)> {
        let mut found = Vec::new();
        collect_property_descriptions(schema, &mut found);
        found
    }

    fn collect_property_descriptions(value: &Value, found: &mut Vec<(String, String)>) {
        match value {
            Value::Object(map) => {
                if let Some(Value::Object(properties)) = map.get("properties") {
                    for (name, property) in properties {
                        if let Some(description) =
                            property.get("description").and_then(Value::as_str)
                        {
                            found.push((name.clone(), description.to_string()));
                        }
                    }
                }
                for child in map.values() {
                    collect_property_descriptions(child, found);
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect_property_descriptions(item, found);
                }
            }
            _ => {}
        }
    }

    /// 共有の入力型に付いた説明を取り出す。
    fn shared_type_description(tool: &str, definition: &str) -> String {
        tool_named(tool).input_schema["$defs"][definition]["description"]
            .as_str()
            .unwrap_or_else(|| panic!("{definition} に説明がありません"))
            .to_string()
    }

    /// tool の入力 schema の直下にある property の説明を取り出す。
    fn field_description(tool: &str, field: &str) -> String {
        tool_named(tool).input_schema["properties"][field]["description"]
            .as_str()
            .unwrap_or_else(|| panic!("{tool} の {field} に説明がありません"))
            .to_string()
    }

    #[test]
    fn the_fields_that_take_a_number_state_where_it_starts() {
        // 起点を取り違えると別の場所へ書く。**値を書く場所そのものが述べる。**
        // tool の説明へ写す形は、番号を扱う tool の数だけ同じ文を増やす一方、
        // 引数を埋める時点では読まれない。
        let mut checked = 0;
        for tool in tools() {
            let schema = Value::Object(tool.input_schema.as_ref().clone());
            for (field, description) in property_descriptions(&schema) {
                if !description.contains("レイヤー番号") && !description.contains("フレーム番号")
                {
                    continue;
                }
                // 番号ではないことを述べる説明は対象外である。BPM グリッドの
                // 位置は秒であり、起点を持たない。
                if description.contains("フレーム番号ではない") {
                    continue;
                }
                checked += 1;
                assert!(
                    description.contains("0 始まり"),
                    "{} の {field} が番号の起点を述べていません: {description}",
                    tool.name
                );
            }
        }
        assert!(
            checked >= 20,
            "番号を取るフィールドを検査できていません: {checked} 件"
        );
    }

    #[test]
    fn the_selector_types_state_that_they_travel_back_unchanged() {
        // selector を組み立て直さずに往復させることは、selector を受け取る全 tool に
        // 掛かる規約である。共有型に 1 度書けば schemars が各 tool の schema へ配る。
        let object = shared_type_description("get_object", "ObjectSelectorInput");
        for phrase in [
            "そのまま送り返す",
            "読み直さずにそのまま次の要求へ渡せる",
            "fingerprint が変わる",
            "precondition_failed",
            "対象の現在の姿",
        ] {
            assert!(
                object.contains(phrase),
                "オブジェクトの selector の説明が {phrase} に触れていません: {object}"
            );
        }
        // **キー名は名乗らない。** 同じ型が apply_batch の schema にも入るが、
        // そちらは同じものを details.failed_object という別の名前で返す。
        // 共有型が片方の名前を名乗ると、もう片方の tool に対して嘘になる。
        assert!(
            !object.contains("details."),
            "共有型が失敗応答のキーを名乗っています: {object}"
        );

        let effect = shared_type_description("set_object_item", "EffectSelectorInput");
        for phrase in [
            "そのまま送り返す",
            "読み直さずにそのまま次の要求へ渡せる",
            "fingerprint が変わる",
        ] {
            assert!(
                effect.contains(phrase),
                "effect の selector の説明が {phrase} に触れていません: {effect}"
            );
        }
    }

    #[test]
    fn the_selector_types_state_which_epoch_matches_the_project() {
        // プロジェクトの世代を照合する材料は selector の中に在る。値の隣で
        // 述べなければ、要求元は selector を組み立て直す口を探すことになる。
        let epoch = tool_named("get_object").input_schema["$defs"]["ObjectSelectorInput"]
            ["properties"]["project_epoch"]["description"]
            .as_str()
            .expect("project_epoch に説明がある")
            .to_string();
        for phrase in ["プロジェクトの世代はこの値で照合", "別のプロジェクト"]
        {
            assert!(
                epoch.contains(phrase),
                "selector の project_epoch が {phrase} に触れていません: {epoch}"
            );
        }
    }

    #[test]
    fn the_page_fields_explain_how_to_page() {
        // ページ指定は共有の入力型が配る。tool の説明へ写すと、同じ 3 行が
        // ページを取る tool の数だけ並ぶ。
        let mut checked = 0;
        for tool in tools() {
            let properties = tool.input_schema["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{} に properties がありません", tool.name));
            if !properties.contains_key("limit") {
                continue;
            }
            checked += 1;
            let offset = field_description(&tool.name, "offset");
            for phrase in ["0 始まりの位置", "has_more と next_offset で終端"] {
                assert!(
                    offset.contains(phrase),
                    "{} の offset が {phrase} に触れていません: {offset}",
                    tool.name
                );
            }
            let limit = field_description(&tool.name, "limit");
            for phrase in ["1 以上 200 以下", "省略すると 50"] {
                assert!(
                    limit.contains(phrase),
                    "{} の limit が {phrase} に触れていません: {limit}",
                    tool.name
                );
            }
        }
        assert!(checked >= 4, "ページ指定を持つ tool を検査していません");
    }

    #[test]
    fn the_expected_epoch_fields_say_why_they_cannot_be_omitted() {
        // 省略できない理由——対象を指す selector が無いこと——は、値を書く場所の
        // 隣に在る。
        for name in TOOLS_CARRYING_AN_EXPECTED_EPOCH {
            let description = field_description(name, "expected_project_epoch");
            for phrase in ["省略はできない", "プロジェクト境界を照合する"] {
                assert!(
                    description.contains(phrase),
                    "{name} の expected_project_epoch が {phrase} に触れていません: {description}"
                );
            }
        }
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
            if tool.name == "list_instances" {
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
            "list_layers",
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
            "create_object",
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
            "set_object_item",
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
            "list_instances",
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
        let unscaled = ScaledBudgets::unscaled();
        let limits = CallLimits::default();
        assert_eq!(
            limits.request,
            unscaled.server_request_phase(RequestBudgetKind::Read)
        );
        assert_eq!(
            limits.edit_request,
            unscaled.server_request_phase(RequestBudgetKind::Edit)
        );
        assert_eq!(
            limits.batch_request,
            unscaled.server_request_phase(RequestBudgetKind::Batch)
        );
        assert_eq!(
            limits.render_request,
            unscaled.server_request_phase(RequestBudgetKind::Render)
        );
        assert_eq!(limits.artifact_ingest, unscaled.server_artifact_ingest());
        assert_eq!(
            DiscoveryConfig::default(),
            DiscoveryConfig::from_budgets(unscaled)
        );
    }

    #[test]
    fn the_discovery_config_follows_the_shared_settings() {
        // 解決フェーズの配分を倍率へ連動させないと、期限だけが縮んだ組が
        // discovery へ渡り、接続を 1 度も試みないまま到達不能として扱われる。
        let settings = settings_with_scale(10);
        let server = AviUtl2McpServer::from_settings_or_fixed(
            PathBuf::from("registry"),
            SettingsOrFixed::Settings(SettingsSource::fixed(settings.clone())),
        );

        let budgets = server.call_budgets();
        assert_eq!(
            budgets.discovery,
            DiscoveryConfig::from_budgets(settings.budgets())
        );
        assert_ne!(budgets.discovery, DiscoveryConfig::default());
        // 要求フェーズと解決フェーズは同じ snapshot から導く。
        assert_eq!(budgets.limits, CallLimits::from_budgets(settings.budgets()));
    }

    #[test]
    fn the_limits_follow_the_shared_settings_without_a_second_judgement() {
        // 倍率の採否は core が不等式ごと決める。server 側で範囲を判定し直すと、
        // plugin と server が同じファイルから別の結論を得る形ができる。
        let settings = settings_with_scale(50);
        let source = SettingsSource::fixed(settings.clone());
        let server = AviUtl2McpServer::from_settings_or_fixed(
            PathBuf::from("registry"),
            SettingsOrFixed::Settings(source),
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
            request: Duration::from_millis(340),
            edit_request: Duration::from_millis(560),
            batch_request: Duration::from_millis(780),
            render_request: Duration::from_millis(910),
            artifact_ingest: Duration::from_millis(130),
        };
        let server = AviUtl2McpServer::without_artifact_store(PathBuf::from("registry"), limits);
        assert_eq!(server.limits().request, Duration::from_millis(340));
        assert_eq!(server.limits().edit_request, Duration::from_millis(560));
        assert_eq!(server.limits().batch_request, Duration::from_millis(780));
        assert_eq!(server.limits().render_request, Duration::from_millis(910));
        assert_eq!(server.limits().artifact_ingest, Duration::from_millis(130));
    }

    /// 区分ごとの取り違えが必ず落ちるよう、桁で離した予算。
    fn probe_limits() -> CallLimits {
        CallLimits {
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
            use aviutl2_mcp_core::EditOperation as Edit;
            // 一括適用は編集の族に属するが、費用の主項が違うため別の予算を持つ。
            //
            // **`_` を使わない網羅 match である。** `_` で受けると、新しい編集
            // operation が黙って編集予算へ落ち、「誤って分類した」を捕まえられない。
            let expected = match op {
                Edit::ApplyBatch => limits.batch_request,
                Edit::CreateObject
                | Edit::MoveObject
                | Edit::DeleteObject
                | Edit::SetObjectName
                | Edit::SetObjectItem
                | Edit::AddEffect
                | Edit::DeleteEffect
                | Edit::SetEffectEnabled
                | Edit::MoveEffect
                | Edit::SetLayerState
                | Edit::SetSelection
                | Edit::CreateObjectSection
                | Edit::DeleteObjectSection
                | Edit::MoveObjectSection
                | Edit::SetGridBpm
                | Edit::SetSceneSettings => limits.edit_request,
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
            let error = failure::from_code(code, "失敗");
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
            let mcp_error = to_mcp_error(&failure::from_code(code, "失敗"));
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
                project: aviutl2_mcp_core::InstanceProject {
                    display_name: Some("名".repeat(500)),
                    path: None,
                    epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
                    revision: 0,
                    modified: false,
                },
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
