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
use std::sync::Arc;

#[cfg(windows)]
use aviutl2::AnyResult;
#[cfg(windows)]
use aviutl2_mcp_core::DescriptorProject;

/// 編集ハンドル。plugin 初期化時に一度だけ設定される。
#[cfg(windows)]
static EDIT_HANDLE: aviutl2::generic::GlobalEditHandle = aviutl2::generic::GlobalEditHandle::new();

#[cfg(windows)]
#[aviutl2::plugin(GenericPlugin)]
struct AviUtl2McpPlugin {
    lifecycle: Option<Arc<lifecycle::Lifecycle>>,
    pipe_server: Option<Arc<pipe::PipeServer>>,
}

#[cfg(windows)]
impl aviutl2::generic::GenericPlugin for AviUtl2McpPlugin {
    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        Ok(Self {
            lifecycle: None,
            pipe_server: None,
        })
    }

    fn register(&mut self, registry: &mut aviutl2::generic::HostAppHandle) {
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
        if let Some(lifecycle) = &self.lifecycle {
            let _ = lifecycle.shutdown();
            let _ = lifecycle.mark_gone();
        }
        // pipe_server は Drop で停止する。
        self.pipe_server.take();
    }
}

aviutl2::register_generic_plugin!(AviUtl2McpPlugin);

#[cfg(not(windows))]
pub fn placeholder() {}
