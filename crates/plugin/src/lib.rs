//! AviUtl2 MCP プラグイン。
//!
//! `.aux2` 汎用プラグインとして AviUtl2 に読み込まれ、named pipe 経由で
//! MCP server からの要求を受け付ける。

#[cfg(windows)]
pub mod edit;
#[cfg(windows)]
pub mod identity;
#[cfg(windows)]
pub mod lifecycle;
#[cfg(windows)]
pub mod pipe;
#[cfg(windows)]
pub mod project;
#[cfg(windows)]
pub mod read;
#[cfg(windows)]
pub mod redact;
#[cfg(windows)]
pub mod registry;
#[cfg(windows)]
pub mod security;
#[cfg(windows)]
pub mod session;
#[cfg(all(windows, test))]
mod test_support;
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
pub(crate) static EDIT_HANDLE: aviutl2::generic::GlobalEditHandle =
    aviutl2::generic::GlobalEditHandle::new();

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

        let read_adapter = read::sdk_read_adapter(project_state.clone());
        let edit_adapter = edit::sdk_edit_adapter(project_state);
        let pipe_server =
            match pipe::PipeServer::start(lifecycle.clone(), read_adapter, edit_adapter) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("named pipe server の起動に失敗しました: {e:?}");
                    return;
                }
            };

        tracing::info!(
            instance_id = %redact::instance_id(&instance_id),
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
        apply_project_load(
            self.lifecycle.as_ref(),
            self.project_state.as_deref(),
            project.get_path().as_deref(),
        );
    }

    fn on_project_save(&mut self, project: &mut aviutl2::generic::ProjectFile) {
        apply_project_save(
            self.lifecycle.as_ref(),
            self.project_state.as_deref(),
            project.get_path().as_deref(),
        );
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
/// `event_*` ハンドラはホストのイベント用スレッドから、plugin 本体の write lock を
/// 保持したまま呼ばれる。イベント処理からは SDK の編集セクションを利用できず、
/// ファイル I/O を挟めばホストの編集操作をその間だけ止めることになる。そのため
/// ここで行えるのは atomic な状態更新と変更の記録だけであり、SDK の read/edit
/// section 呼び出しと descriptor の書き込みは行わない。ホストはイベントの
/// コールバックから編集区間を開始することを禁じている。
/// ハンドラ本体をプロジェクト状態だけを受け取る関数へ切り出し、ハンドラ側を
/// 委譲だけにすることで、制約を満たすべき範囲をこの関数に閉じ込めている。
/// 読み取り口も編集口もこの関数からは参照できず、到達する経路が型として無い。
///
/// この制約が掛かるのは `event_*` ハンドラだけである。プロジェクトのロード・
/// 保存ハンドラはイベント用スレッドから呼ばれず、境界ごとに一度しか発生しない
/// ため、descriptor の更新は [`AviUtl2McpPlugin::sync_project`] で行う。
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

/// プロジェクトのロードを反映する。
///
/// 初回のロードが readiness の境界であり、ここでインスタンスは読み取りを
/// 受け付けられる状態になる。ロードはプロジェクトが切り替わる境界でもあるため、
/// 新しい epoch を発行する。
///
/// ハンドラ本体を、ハンドラが持つ状態だけを受け取る関数へ切り出し、ハンドラ側を
/// 委譲だけにすることで、境界の扱いをこの関数に閉じ込めている。
#[cfg(windows)]
fn apply_project_load(
    lifecycle: Option<&Arc<lifecycle::Lifecycle>>,
    project_state: Option<&project::ProjectState>,
    path: Option<&std::path::Path>,
) {
    if let Some(lifecycle) = lifecycle {
        let _ = lifecycle.transition_to(aviutl2_mcp_core::state::InstanceState::Ready);
    }
    sync_project(lifecycle, project_state, path, ProjectBoundary::Renewed);
}

/// プロジェクトの保存を反映する。
///
/// 保存は同一プロジェクトに対する操作であり、epoch を維持する。readiness の
/// 境界でもないため、状態遷移は行わない。
#[cfg(windows)]
fn apply_project_save(
    lifecycle: Option<&Arc<lifecycle::Lifecycle>>,
    project_state: Option<&project::ProjectState>,
    path: Option<&std::path::Path>,
) {
    sync_project(lifecycle, project_state, path, ProjectBoundary::Retained);
}

/// project handler が確定したパスを read 用の状態と descriptor へ反映する。
///
/// ロード時と保存時で異なるのは epoch を再発行するかどうかだけであり、
/// descriptor への反映内容は共通である。
///
/// descriptor の書き込みを伴うため、呼び出せるのはプロジェクトのロード・
/// 保存ハンドラからだけである。`event_*` ハンドラから呼んではならない。
#[cfg(windows)]
fn sync_project(
    lifecycle: Option<&Arc<lifecycle::Lifecycle>>,
    project_state: Option<&project::ProjectState>,
    path: Option<&std::path::Path>,
    boundary: ProjectBoundary,
) {
    if let Some(project_state) = project_state {
        let path = path.map(|path| path.to_string_lossy());
        match boundary {
            ProjectBoundary::Renewed => project_state.on_project_load(path.as_deref()),
            ProjectBoundary::Retained => project_state.on_project_save(path.as_deref()),
        }
    }

    let Some(lifecycle) = lifecycle else {
        return;
    };

    if let Err(e) = lifecycle.update_project(path.map(descriptor_project)) {
        tracing::error!("プロジェクト情報の更新に失敗しました: {e:?}");
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
                    pipe_server.stop(pipe::STOP_TIMEOUT);
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
    use crate::test_support::with_silent_panic_hook;

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
    /// 本体は編集ハンドルも編集口も引数に取らない自由関数であり、受け取るのは
    /// プロジェクト状態のみである。ホストはイベントのコールバックから編集区間を
    /// 開始することを禁じており、この形であれば編集口へ到達する経路が型として
    /// 存在しない。ここではその関数を直接呼び、状態の更新が期待どおりであることと、
    /// 変更が記録されることを確かめる。引数の型が広がれば、この呼び出しが
    /// そのままコンパイルできなくなる。
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

    /// 初回のプロジェクトロードが readiness の境界であることを確かめる。
    ///
    /// 遷移と同時にプロジェクト境界も更新されるため、旧プロジェクトを指す
    /// セレクターは epoch の照合で拒否されるようになる。
    #[test]
    fn project_load_makes_the_instance_ready_and_renews_the_boundary() {
        let (lifecycle, dir) = temp_lifecycle();
        let project_state = project::ProjectState::new();
        project_state.on_object_updated();
        let epoch = project_state.epoch();
        assert_eq!(
            lifecycle.state(),
            aviutl2_mcp_core::state::InstanceState::Starting
        );

        apply_project_load(
            Some(&lifecycle),
            Some(&project_state),
            Some(std::path::Path::new(r"C:\projects\sample.aup2")),
        );

        assert_eq!(
            lifecycle.state(),
            aviutl2_mcp_core::state::InstanceState::Ready,
            "初回のプロジェクトロードで ready になりませんでした"
        );
        assert_ne!(
            project_state.epoch(),
            epoch,
            "プロジェクトロードで epoch が更新されませんでした"
        );
        assert_eq!(project_state.revision(), 0);
        assert!(!project_state.modified());
        assert_eq!(
            project_state.identity_path().as_deref(),
            Some(r"C:\projects\sample.aup2")
        );
        assert_eq!(
            lifecycle.descriptor().project.map(|p| p.display_name),
            Some("sample".to_string())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// プロジェクトの保存は readiness の境界ではないことを確かめる。
    #[test]
    fn project_save_neither_makes_the_instance_ready_nor_renews_the_boundary() {
        let (lifecycle, dir) = temp_lifecycle();
        let project_state = project::ProjectState::new();
        project_state.on_object_updated();
        let epoch = project_state.epoch();
        let revision = project_state.revision();

        apply_project_save(
            Some(&lifecycle),
            Some(&project_state),
            Some(std::path::Path::new(r"C:\projects\sample.aup2")),
        );

        assert_eq!(
            lifecycle.state(),
            aviutl2_mcp_core::state::InstanceState::Starting,
            "保存で ready になりました"
        );
        assert_eq!(
            project_state.epoch(),
            epoch,
            "保存で epoch が更新されました"
        );
        assert_eq!(project_state.revision(), revision);
        assert!(!project_state.modified());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// tracing イベントの出力先として使う共有バッファ。
    #[derive(Clone, Default)]
    struct LogCapture(Arc<std::sync::Mutex<Vec<u8>>>);

    impl LogCapture {
        fn contents(&self) -> String {
            let buffer = self.0.lock().unwrap_or_else(|e| e.into_inner());
            String::from_utf8_lossy(&buffer).into_owned()
        }
    }

    impl std::io::Write for LogCapture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> aviutl2::tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
        type Writer = LogCapture;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// `f` の実行中に発行された tracing イベントを集めて返す。
    ///
    /// 出力先はこのスレッドの subscriber に限られるため、ホストのログ設定にも
    /// 他のテストにも影響しない。
    fn capture_logs(f: impl FnOnce()) -> String {
        let capture = LogCapture::default();
        let subscriber = aviutl2::tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(capture.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        capture.contents()
    }

    /// ログに完全な識別子も絶対パスも現れないことを確かめる。
    ///
    /// 出力先はホストのログファイルであり、不具合の報告に添えて持ち出される。
    /// ここで通すのは、状態遷移と descriptor 削除の記録に加え、descriptor の
    /// 書き込み先を塞いだ状態での更新と終了手順である。後者は registry と
    /// セキュリティ記述子の失敗理由が anyhow の連鎖としてログへ流れる経路で、
    /// 直接ログへ渡している値だけを見ても漏れの有無が分からない。
    #[test]
    fn logs_expose_neither_full_identifiers_nor_absolute_paths() {
        // registry ルートの名前に instance_id を含めない。含めると、絶対パスが
        // 出ていないことの確認が完全な識別子の確認と区別できなくなる。
        let root = std::env::temp_dir().join(format!(
            "aviutl2-mcp-redaction-test-{}",
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&root);

        let instance_id = aviutl2_mcp_core::InstanceId::new_v4();
        let lifecycle = Arc::new(
            lifecycle::Lifecycle::new(
                instance_id,
                aviutl2_mcp_core::AuthSecret::generate(),
                std::process::id(),
                "2026-01-01T00:00:00.0000000Z".to_string(),
                Some("0x0".to_string()),
                "2026-01-01T00:00:00.0000000Z".to_string(),
                registry::RegistryWriter::for_dir(root.clone()),
            )
            .unwrap(),
        );
        let plugin = AviUtl2McpPlugin {
            lifecycle: Some(lifecycle.clone()),
            project_state: Some(Arc::new(project::ProjectState::new())),
            pipe_server: None,
        };

        let logs = capture_logs(|| {
            lifecycle
                .transition_to(aviutl2_mcp_core::state::InstanceState::Ready)
                .unwrap();

            // descriptor の書き込み先をファイルで塞ぎ、以降の更新を失敗させる。
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("instances"), b"").unwrap();

            apply_project_save(
                plugin.lifecycle.as_ref(),
                plugin.project_state.as_deref(),
                Some(std::path::Path::new(r"C:\projects\sample.aup2")),
            );
            drop(plugin);
        });

        let _ = std::fs::remove_dir_all(&root);

        // パスの検査を先に行う。registry のパスには instance_id が現れないため、
        // 順序を逆にするとパスの漏れが識別子の漏れとして報告される。
        assert!(
            !logs.contains(&std::env::temp_dir().display().to_string()),
            "利用者のディレクトリがログに出ています: {logs}"
        );
        assert!(
            !logs.contains(&root.display().to_string()),
            "registry の絶対パスがログに出ています: {logs}"
        );

        let anonymized = redact::instance_id(&instance_id);
        assert!(
            logs.contains(&anonymized),
            "匿名化した instance_id が記録されていません: {logs}"
        );
        assert!(
            !logs.contains(&instance_id.to_string()),
            "完全な instance_id がログに出ています: {logs}"
        );
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
