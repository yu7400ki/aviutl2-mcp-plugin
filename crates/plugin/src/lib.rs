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
pub mod project;
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
    project_state: Option<Arc<project::ProjectState>>,
    pipe_server: Option<pipe::PipeServer>,
}

#[cfg(windows)]
impl aviutl2::generic::GenericPlugin for AviUtl2McpPlugin {
    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        init_tracing();
        Ok(Self {
            lifecycle: None,
            project_state: None,
            pipe_server: None,
        })
    }

    fn register(&mut self, registry: &mut aviutl2::generic::HostAppHandle) {
        init_tracing();
        EDIT_HANDLE.init(registry.create_edit_handle());

        // イベントハンドラは registry への登録直後から呼ばれ得るため、
        // 失敗し得る初期化より先に用意する。
        let project_state = Arc::new(project::ProjectState::new());
        self.project_state = Some(project_state.clone());

        let instance_id = aviutl2_mcp_core::InstanceId::new_v4();
        let auth_secret = aviutl2_mcp_core::AuthSecret::generate();
        let pid = identity::current_pid();
        let process_created_at = match identity::current_process_created_at() {
            Ok(dt) => aviutl2_mcp_core::format_utc_timestamp(dt),
            Err(e) => {
                tracing::error!("プロセス作成時刻の取得に失敗しました: {e:?}");
                aviutl2_mcp_core::format_utc_timestamp(chrono::Utc::now())
            }
        };
        let hwnd = identity::current_hwnd();
        let started_at = aviutl2_mcp_core::format_utc_timestamp(chrono::Utc::now());

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

        let pipe_server = match pipe::PipeServer::start(lifecycle.clone(), project_state) {
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
        }
        // ロードはプロジェクトが切り替わる境界であり、新しい epoch を発行する。
        self.sync_project(project.get_path(), ProjectBoundary::Renewed);
    }

    fn on_project_save(&mut self, project: &mut aviutl2::generic::ProjectFile) {
        // 保存は同一プロジェクトに対する操作であり、epoch を維持する。
        self.sync_project(project.get_path(), ProjectBoundary::Retained);
    }

    fn on_clear_cache(&mut self, _edit_section: &aviutl2::generic::EditSection) {
        tracing::debug!("キャッシュ破棄イベントを受信しました");
    }

    fn event_update_object_info(&mut self) {
        if let Some(project_state) = &self.project_state {
            apply_object_update(project_state);
        }
    }

    /// 編集フレームの移動は対象の構造を変えないため revision を更新しない。
    fn event_change_edit_frame(&mut self) {}

    fn event_change_scene_info(&mut self) {
        if let Some(project_state) = &self.project_state {
            apply_scene_change(project_state);
        }
    }

    /// フォーカスの変更は対象の構造を変えないため revision を更新しない。
    fn event_change_focus_object(&mut self) {}
}

/// 対象の更新をプロジェクト状態へ反映する。
///
/// イベントハンドラはホストのグローバル write lock を保持したまま呼ばれるため、
/// 行えるのは atomic な状態更新と変更の記録だけである。SDK の read/edit section
/// 呼び出しと descriptor の書き込みは、この制約に反するため行わない。
/// ハンドラ本体をプロジェクト状態だけを受け取る関数へ切り出し、ハンドラ側を
/// 委譲だけにすることで、制約を満たすべき範囲をこの関数に閉じ込めている。
#[cfg(windows)]
fn apply_object_update(project_state: &project::ProjectState) {
    project_state.on_object_updated();
}

/// シーンの変更をプロジェクト状態へ反映する。
///
/// 制約は [`apply_object_update`] と同じ。
#[cfg(windows)]
fn apply_scene_change(project_state: &project::ProjectState) {
    project_state.on_scene_changed();
}

/// project handler が表すプロジェクト境界の扱い。
#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectBoundary {
    /// プロジェクトが切り替わった。
    Renewed,
    /// 同一プロジェクトが継続している。
    Retained,
}

#[cfg(windows)]
impl AviUtl2McpPlugin {
    /// project handler が確定したパスを read 用の状態と descriptor へ反映する。
    ///
    /// ロード時と保存時で異なるのは epoch を再発行するかどうかだけであり、
    /// descriptor への反映内容は共通である。
    fn sync_project(&self, path: Option<std::path::PathBuf>, boundary: ProjectBoundary) {
        if let Some(project_state) = &self.project_state {
            let path = path.as_ref().map(|path| path.to_string_lossy());
            match boundary {
                ProjectBoundary::Renewed => project_state.on_project_load(path.as_deref()),
                ProjectBoundary::Retained => project_state.on_project_save(path.as_deref()),
            }
        }

        let Some(lifecycle) = &self.lifecycle else {
            return;
        };

        if let Err(e) = lifecycle.update_project(path.as_deref().map(descriptor_project)) {
            tracing::error!("プロジェクト情報の更新に失敗しました: {e:?}");
        }
    }
}

/// descriptor に載せるプロジェクト情報を組み立てる。
///
/// 表示名は拡張子を除いたファイル名とし、取得できない場合は未命名として扱う。
#[cfg(windows)]
fn descriptor_project(path: &std::path::Path) -> DescriptorProject {
    let display_name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "未命名プロジェクト".to_string());
    DescriptorProject {
        display_name,
        path: path.to_string_lossy().into_owned(),
    }
}

/// 終了手順を段ごとに panic から隔離して順に実行する。
///
/// 各段はログ出力を伴い、ログ出力そのものが panic し得る。ログの出力先は
/// level ごとの mutex に守られており、その mutex が毒されると以後あらゆる
/// スレッドのログ出力が panic するためである。前段の panic で
/// `remove_descriptor` が飛ばされると、実体の無い descriptor が registry に
/// 残り続け、後続の探索が存在しないインスタンスを返してしまう。
///
/// 捕捉した panic をここでログ化しないのは、ログ経路自体が panic 源であり
/// 得るためである。また `Drop` から panic を漏らさないことで、ホストの
/// 終了処理が巻き戻り経路へ入るのも防ぐ。
#[cfg(windows)]
fn run_shutdown_sequence(
    stop_pipe: impl FnOnce(),
    drain: impl FnOnce(),
    remove_descriptor: impl FnOnce(),
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(stop_pipe));
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(drain));
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(remove_descriptor));
}

#[cfg(windows)]
impl Drop for AviUtl2McpPlugin {
    fn drop(&mut self) {
        let pipe_server = self.pipe_server.take();
        let lifecycle = self.lifecycle.take();

        // pipe を停止してから descriptor を削除する。順序を逆にすると
        // descriptor が消えた後も pipe が接続を受け付ける窓ができる。
        run_shutdown_sequence(
            || {
                if let Some(pipe_server) = pipe_server {
                    pipe_server.stop(PIPE_SERVER_STOP_TIMEOUT);
                }
            },
            || {
                if let Some(lifecycle) = &lifecycle
                    && let Err(e) = lifecycle.shutdown()
                {
                    tracing::warn!("draining への移行に失敗しました: {e:?}");
                }
            },
            || {
                if let Some(lifecycle) = &lifecycle
                    && let Err(e) = lifecycle.mark_gone()
                {
                    tracing::error!("descriptor の削除に失敗しました: {e:?}");
                }
            },
        );
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

    /// panic のたびに既定フックが標準エラーへ出力するのを抑える。
    ///
    /// フックはプロセス全体で共有されるため、復元まで含めて呼び出し側が行う。
    fn with_silent_panic_hook(f: impl FnOnce()) {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        std::panic::set_hook(previous);
        assert!(result.is_ok(), "終了手順から panic が漏れました");
    }

    #[test]
    fn shutdown_sequence_removes_descriptor_even_if_earlier_steps_panic() {
        let drained = std::cell::Cell::new(false);
        let removed = std::cell::Cell::new(false);

        with_silent_panic_hook(|| {
            run_shutdown_sequence(
                || panic!("pipe 停止時のログ出力が失敗しました"),
                || {
                    drained.set(true);
                    panic!("draining 遷移時のログ出力が失敗しました");
                },
                || removed.set(true),
            );
        });

        assert!(
            drained.get(),
            "pipe 停止の panic で draining が飛ばされました"
        );
        assert!(
            removed.get(),
            "前段の panic で descriptor の削除が飛ばされました"
        );
    }

    #[test]
    fn shutdown_sequence_runs_steps_in_order() {
        let order = std::cell::RefCell::new(Vec::new());

        run_shutdown_sequence(
            || order.borrow_mut().push("pipe"),
            || order.borrow_mut().push("drain"),
            || order.borrow_mut().push("remove"),
        );

        assert_eq!(order.into_inner(), vec!["pipe", "drain", "remove"]);
    }

    #[test]
    fn shutdown_sequence_does_not_propagate_panic() {
        with_silent_panic_hook(|| {
            run_shutdown_sequence(|| panic!("pipe"), || panic!("drain"), || panic!("remove"));
        });
    }

    /// registry ルートを一時ディレクトリに向けたライフサイクルを作る。
    fn temp_lifecycle() -> (Arc<lifecycle::Lifecycle>, std::path::PathBuf) {
        let id = aviutl2_mcp_core::InstanceId::new_v4();
        let dir = std::env::temp_dir().join(format!("aviutl2-mcp-plugin-test-{id}"));
        let _ = std::fs::remove_dir_all(&dir);
        let lifecycle = lifecycle::Lifecycle::new(
            id,
            aviutl2_mcp_core::AuthSecret::generate(),
            std::process::id(),
            "2026-01-01T00:00:00.0000000Z".to_string(),
            Some("0x0".to_string()),
            "2026-01-01T00:00:00.0000000Z".to_string(),
            registry::RegistryWriter::for_dir(dir.clone()),
        )
        .unwrap();
        (Arc::new(lifecycle), dir)
    }

    /// registry ルート配下の descriptor パス。
    fn descriptor_path(
        root: &std::path::Path,
        id: aviutl2_mcp_core::InstanceId,
    ) -> std::path::PathBuf {
        root.join("instances").join(format!("{id}.json"))
    }

    /// イベントハンドラ本体がプロジェクト状態だけを更新することを確かめる。
    ///
    /// 本体は編集ハンドルを引数に取らない自由関数であり、受け取るのは
    /// プロジェクト状態のみである。ここではその関数を直接呼び、状態の更新が
    /// 期待どおりであることと、変更が記録されることを確かめる。
    #[test]
    fn event_handler_bodies_update_project_state() {
        let project_state = project::ProjectState::new();
        project_state.on_project_load(Some(r"C:\projects\sample.aup2"));
        let epoch = project_state.epoch();
        let now = std::time::Instant::now();
        project_state.take_pending_changes(now);

        apply_object_update(&project_state);
        assert_eq!(project_state.revision(), 1);
        assert!(project_state.modified());

        apply_scene_change(&project_state);
        assert_eq!(project_state.revision(), 2);
        assert_eq!(
            project_state.epoch(),
            epoch,
            "シーンの変更で epoch が更新されました"
        );

        let taken = project_state
            .take_pending_changes(now + std::time::Duration::from_millis(100))
            .expect("イベントの変更が記録されていません");
        assert!(taken.contains(project::ChangeKind::ProjectRevision));
        assert!(taken.contains(project::ChangeKind::CurrentScene));
    }

    /// イベントハンドラが本体へ委譲し、descriptor の内容を変えないことを確かめる。
    ///
    /// 対象の更新とシーンの変更はプロジェクト状態へ反映され、編集フレームと
    /// フォーカスの変更は何も更新しない。あわせて descriptor ファイルの内容が
    /// イベントの前後で一致することを確かめる。ここで確かめられるのは内容の
    /// 同一性だけであり、書き込みが行われなかったことまでは確かめていない。
    #[test]
    fn event_handlers_delegate_to_project_state() {
        use aviutl2::generic::GenericPlugin;

        let (lifecycle, dir) = temp_lifecycle();
        let descriptor_file = descriptor_path(&dir, lifecycle.instance_id());
        let project_state = Arc::new(project::ProjectState::new());
        let descriptor_before = std::fs::read_to_string(&descriptor_file).unwrap();

        let mut plugin = AviUtl2McpPlugin {
            lifecycle: Some(lifecycle),
            project_state: Some(project_state.clone()),
            pipe_server: None,
        };

        plugin.event_change_edit_frame();
        plugin.event_change_focus_object();
        assert_eq!(
            project_state.revision(),
            0,
            "構造が変わらないイベントで revision が進みました"
        );
        assert!(!project_state.modified());

        plugin.event_update_object_info();
        assert_eq!(project_state.revision(), 1);
        assert!(project_state.modified());

        plugin.event_change_scene_info();
        assert_eq!(project_state.revision(), 2);

        assert_eq!(
            descriptor_before,
            std::fs::read_to_string(&descriptor_file).unwrap(),
            "イベントの前後で descriptor の内容が変わりました"
        );

        drop(plugin);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_tracing_is_idempotent() {
        init_tracing();
        init_tracing();

        // subscriber 設定後のイベント発行が panic しないこと。
        // AviUtl2 のログハンドルが無い環境では出力は破棄される。
        tracing::info!("tracing subscriber の初期化テスト");
    }
}
