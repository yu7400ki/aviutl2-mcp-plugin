//! MCP stdio サーバーの本体。
//!
//! tool call は 1 回ごとに接続を確立し、応答を受け取ったら破棄する。
//! [`crate::pipe_client::PipeClient`] は生のハンドルを持ち `!Send` であるため、
//! インスタンス解決から要求送信・切断までを 1 つのブロッキングタスクへ閉じ込め、
//! 非同期タスク間で接続が移動しないようにする。

use crate::api::{ListInstancesResponse, list_instances};
use crate::artifact::{Artifact, ArtifactStore, ArtifactStoreError, base_dir_for_registry};
use crate::discovery::{DiscoveryConfig, list_registered_instances, resolve_instance};
use crate::mcp::input::{ListInstancesInput, parse_instance_id};
use crate::mcp::summary::{MAX_TEXT_CHARS, clamp_chars};
use crate::mcp::tool_catalog::{ToolListWatch, ToolVisibility};
use crate::mcp::{describe, failure};
use crate::redact;
use crate::settings::SettingsSource;
use aviutl2_mcp_core::{
    EditInfo, ErrorCode, ErrorObject, GetEditInfoParams, InstanceId, MAX_PAGE_LIMIT,
    OPERATION_GET_EDIT_INFO, RequestBudgetKind, ScaledBudgets, request_budget_kind,
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
    pub(super) artifacts: Option<Arc<ArtifactStore>>,
    settings: SettingsOrFixed,
    tool_router: ToolRouter<Self>,
}

/// tool call 1 回分の期限一式。
///
/// 要求フェーズと解決フェーズは別々の型が持つが、**同じ設定の snapshot から
/// 導く**。両者が別の snapshot から来ると、要求へ載せる期限と接続待ちの配分が
/// 噛み合わない組になり得る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CallBudgets {
    /// 要求フェーズの期限。
    pub(super) limits: CallLimits,
    /// インスタンス解決フェーズの配分。
    pub(super) discovery: DiscoveryConfig,
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
pub(super) struct ToolSuccess {
    pub(super) text: String,
    pub(super) structured: Value,
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
            tool_router: Self::tool_router()
                + Self::read_tools_router()
                + Self::edit_tools_router()
                + Self::render_tools_router(),
        }
    }

    /// tool call 1 回分の期限一式。
    pub(super) fn call_budgets(&self) -> CallBudgets {
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
    pub(super) async fn run<F>(&self, tool: &'static str, body: F) -> CallToolResult
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
    pub(super) fn registry_dir(&self) -> Arc<PathBuf> {
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
pub(super) fn request_operation<P, R>(
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
pub(super) fn to_structured<T: Serialize>(value: &T) -> Result<Value, ErrorObject> {
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
pub(super) mod tests;
