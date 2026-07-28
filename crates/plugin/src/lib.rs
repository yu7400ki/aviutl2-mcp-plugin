//! AviUtl2 MCP プラグイン。
//!
//! `.aux2` 汎用プラグインとして AviUtl2 に読み込まれ、named pipe 経由で
//! MCP server からの要求を受け付ける。

#[cfg(windows)]
pub mod identity;
#[cfg(windows)]
pub mod lifecycle;
#[cfg(windows)]
pub mod pipe;
#[cfg(windows)]
pub mod registry;
#[cfg(windows)]
pub mod security;
#[cfg(windows)]
pub mod session;
#[cfg(windows)]
mod win_io;

#[cfg(windows)]
use std::sync::Arc;

#[cfg(windows)]
use aviutl2::AnyResult;
#[cfg(windows)]
use aviutl2_mcp_core::DescriptorProject;

/// 編集ハンドル。plugin 初期化時に一度だけ設定される。
#[cfg(windows)]
static EDIT_HANDLE: aviutl2::generic::GlobalEditHandle = aviutl2::generic::GlobalEditHandle::new();

/// named pipe server 停止時の bounded join 上限。
///
/// ホスト終了時に無期限で待たないため有限にする。期限内に終了しなければ
/// スレッドを切り離してログ化する。
#[cfg(windows)]
const PIPE_SERVER_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// ログ出力レベルを上書きする環境変数名。
#[cfg(windows)]
const LOG_ENV: &str = "AVIUTL2_MCP_LOG";

/// tracing のイベントを AviUtl2 のログへ流す global subscriber を設定する。
///
/// 出力先は AviUtl2 本体のログで、level ごとに AviUtl2 側の
/// ERROR / WARN / INFO / VERBOSE 区分へ振り分けられる。
///
/// 呼び出し順序: SDK は logger ハンドルの初期化をプラグイン初期化より先に行うため、
/// `GenericPlugin::new` の時点で出力先は利用可能になっている。ここで設定しておくと、
/// `register` 内の失敗に加え、ラッパーが `register` の panic を捕捉して発行する
/// イベントも取りこぼさずに記録できる。
///
/// DLL は初期化が複数回呼ばれ得るため、設定は初回のみ行い、
/// 既に global subscriber が設定済みの場合も何もせず戻る。
///
/// 既定 level は debug ビルドで `debug`、release ビルドで `info`。
/// `AVIUTL2_MCP_LOG` 環境変数（`RUST_LOG` と同じ書式）で上書きできる。
#[cfg(windows)]
fn init_tracing() {
    use aviutl2::tracing_subscriber::EnvFilter;

    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let default_level = if cfg!(debug_assertions) {
            "debug"
        } else {
            "info"
        };
        let filter =
            EnvFilter::try_from_env(LOG_ENV).unwrap_or_else(|_| EnvFilter::new(default_level));

        // 他所で global subscriber が設定済みの場合は上書きせず、そのまま続行する。
        let _ = aviutl2::tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .event_format(aviutl2::logger::AviUtl2Formatter)
            .with_writer(aviutl2::logger::AviUtl2LogWriter)
            .try_init();
    });
}

#[cfg(windows)]
#[aviutl2::plugin(GenericPlugin)]
struct AviUtl2McpPlugin {
    lifecycle: Option<Arc<lifecycle::Lifecycle>>,
    pipe_server: Option<Arc<pipe::PipeServer>>,
}

#[cfg(windows)]
impl aviutl2::generic::GenericPlugin for AviUtl2McpPlugin {
    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        init_tracing();
        Ok(Self {
            lifecycle: None,
            pipe_server: None,
        })
    }

    fn register(&mut self, registry: &mut aviutl2::generic::HostAppHandle) {
        init_tracing();
        EDIT_HANDLE.init(registry.create_edit_handle());

        let instance_id = aviutl2_mcp_core::InstanceId::new_v4();
        let auth_secret = aviutl2_mcp_core::AuthSecret::generate();
        let pid = identity::current_pid();
        let process_created_at = match identity::current_process_created_at() {
            Ok(dt) => dt.to_rfc3339(),
            Err(e) => {
                tracing::error!("プロセス作成時刻の取得に失敗しました: {e:?}");
                chrono::Utc::now().to_rfc3339()
            }
        };
        let hwnd = identity::current_hwnd();
        let started_at = chrono::Utc::now().to_rfc3339();

        let writer = match registry::RegistryWriter::new() {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("registry writer の作成に失敗しました: {e:?}");
                return;
            }
        };

        let lifecycle = match lifecycle::Lifecycle::new(
            instance_id,
            auth_secret,
            pid,
            process_created_at,
            hwnd,
            started_at,
            writer,
        ) {
            Ok(l) => Arc::new(l),
            Err(e) => {
                tracing::error!("lifecycle の初期化に失敗しました: {e:?}");
                return;
            }
        };

        let pipe_server = match pipe::PipeServer::start(lifecycle.clone()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("named pipe server の起動に失敗しました: {e:?}");
                return;
            }
        };

        tracing::info!(
            instance_id = %instance_id,
            pid,
            "plugin を登録し named pipe server を起動しました"
        );

        self.lifecycle = Some(lifecycle);
        self.pipe_server = Some(pipe_server);
    }

    fn plugin_info(&self) -> aviutl2::generic::GenericPluginTable {
        aviutl2::generic::GenericPluginTable {
            name: "AviUtl2 MCP Plugin".to_string(),
            information: format!(
                "AviUtl2 MCP Plugin / v{version}",
                version = env!("CARGO_PKG_VERSION")
            ),
        }
    }

    fn on_project_load(&mut self, project: &mut aviutl2::generic::ProjectFile) {
        if let Some(lifecycle) = &self.lifecycle {
            let _ = lifecycle.transition_to(aviutl2_mcp_core::state::InstanceState::Ready);

            let project_info = project.get_path().map(|path| {
                let display_name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "未命名プロジェクト".to_string());
                DescriptorProject {
                    display_name,
                    path: path.to_string_lossy().into_owned(),
                }
            });

            if let Err(e) = lifecycle.update_project(project_info) {
                tracing::error!("プロジェクト情報の更新に失敗しました: {e:?}");
            }
        }
    }

    fn on_project_save(&mut self, project: &mut aviutl2::generic::ProjectFile) {
        if let Some(lifecycle) = &self.lifecycle {
            let project_info = project.get_path().map(|path| {
                let display_name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "未命名プロジェクト".to_string());
                DescriptorProject {
                    display_name,
                    path: path.to_string_lossy().into_owned(),
                }
            });

            if let Err(e) = lifecycle.update_project(project_info) {
                tracing::error!("プロジェクト情報の更新に失敗しました: {e:?}");
            }
        }
    }

    fn on_clear_cache(&mut self, _edit_section: &aviutl2::generic::EditSection) {
        tracing::debug!("キャッシュ破棄イベントを受信しました");
    }

    fn event_update_object_info(&mut self) {}
    fn event_change_edit_frame(&mut self) {}
    fn event_change_scene_info(&mut self) {}
    fn event_change_focus_object(&mut self) {}
}

#[cfg(windows)]
impl Drop for AviUtl2McpPlugin {
    fn drop(&mut self) {
        // pipe を停止してから descriptor を削除する。順序を逆にすると
        // descriptor が消えた後も pipe が接続を受け付ける窓ができる。
        if let Some(pipe_server) = self.pipe_server.take() {
            pipe_server.stop(PIPE_SERVER_STOP_TIMEOUT);
        }
        if let Some(lifecycle) = &self.lifecycle {
            if let Err(e) = lifecycle.shutdown() {
                tracing::warn!("draining への移行に失敗しました: {e:?}");
            }
            if let Err(e) = lifecycle.mark_gone() {
                tracing::error!("descriptor の削除に失敗しました: {e:?}");
            }
        }
    }
}

// SDK が要求する C エクスポートを生成する。展開結果は Windows 限定の
// `AviUtl2McpPlugin` を参照するため、モジュール群と同じ条件で展開する。
#[cfg(windows)]
aviutl2::register_generic_plugin!(AviUtl2McpPlugin);

#[cfg(not(windows))]
pub fn placeholder() {}

#[cfg(all(windows, test))]
mod tests {
    use super::*;

    #[test]
    fn init_tracing_is_idempotent() {
        init_tracing();
        init_tracing();

        // subscriber 設定後のイベント発行が panic しないこと。
        // AviUtl2 のログハンドルが無い環境では出力は破棄される。
        tracing::info!("tracing subscriber の初期化テスト");
    }
}
