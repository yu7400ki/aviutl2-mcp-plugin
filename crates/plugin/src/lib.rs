//! AviUtl2 MCP プラグイン。
//!
//! `.aux2` 汎用プラグインとして AviUtl2 に読み込まれ、named pipe 経由で
//! MCP server からの要求を受け付ける。

#[cfg(windows)]
pub mod agent_plugin;
#[cfg(windows)]
pub mod alias;
#[cfg(windows)]
mod atomic_file;
#[cfg(windows)]
pub mod edit;
#[cfg(windows)]
pub mod effect_help;
#[cfg(windows)]
pub mod identity;
#[cfg(windows)]
pub(crate) mod item_facets;
#[cfg(windows)]
pub mod lifecycle;
#[cfg(windows)]
pub mod movement;
#[cfg(windows)]
pub mod pipe;
#[cfg(windows)]
pub mod project;
#[cfg(windows)]
pub mod read;
/// `details.reason` の値域を、生成経路と突き合わせて固定する検査。
#[cfg(all(windows, test))]
mod reason_values;
#[cfg(windows)]
pub mod redact;
#[cfg(windows)]
pub mod registry;
#[cfg(windows)]
pub mod render;
#[cfg(windows)]
pub mod session;
#[cfg(windows)]
pub mod settings;
#[cfg(windows)]
pub mod settings_ui;
/// 同梱する skill の本文を、写しと未実測の両側から固定する検査。
#[cfg(all(windows, test))]
mod skill_body;
#[cfg(all(windows, test))]
mod test_support;
#[cfg(windows)]
mod win_io;

#[cfg(windows)]
use std::sync::Arc;
#[cfg(windows)]
use std::time::Duration;

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
/// level は `AVIUTL2_MCP_LOG` 環境変数（`RUST_LOG` と同じ書式）、共有設定の
/// `log_level`、ビルドごとの既定の順に採る。**環境変数を先に見るのは、設定
/// ファイルごと読めない状況を診断する経路を残すためである。**
///
/// **level は稼働中に差し替えられる。** filter を `reload::Layer` の下に置き、
/// 設定が変わったら [`apply_log_level`] が差し替える。挟まるのは読み取りロック
/// 1 回であり、**他の項目と同じく「保存すれば効く」形になる。**
#[cfg(windows)]
fn init_tracing() {
    use aviutl2::tracing_subscriber::layer::SubscriberExt;
    use aviutl2::tracing_subscriber::util::SubscriberInitExt;
    use aviutl2::tracing_subscriber::{fmt, reload};

    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        // 記録の準備より先に設定を読む。生じたことは subscriber が立って
        // から流す。読めなくても既定値で続行する。
        let report = settings::initialize();
        let (filter, rejected) = log_filter(&settings::current());
        let (layer, handle) = reload::Layer::new(filter);
        let _ = LOG_RELOAD.set(Box::new(move |filter| {
            if let Err(e) = handle.reload(filter) {
                tracing::warn!("ログレベルを差し替えられませんでした: {e}");
            }
        }));

        // 他所で global subscriber が設定済みの場合は上書きせず、そのまま続行する。
        let _ = aviutl2::tracing_subscriber::registry()
            .with(layer)
            .with(
                fmt::layer()
                    .with_ansi(false)
                    .event_format(aviutl2::logger::AviUtl2Formatter)
                    .with_writer(aviutl2::logger::AviUtl2LogWriter),
            )
            .try_init();

        report_rejected_log_level(rejected);
        settings::report_startup(&report);
    });
}

/// 稼働中の subscriber のレベルを差し替える口。
///
/// subscriber はプロセスに 1 つしか無いため、口も 1 つで足りる。
/// [`init_tracing`] を通っていない場合（試験など）は空のままであり、
/// [`apply_log_level`] は何もしない。
#[cfg(windows)]
#[allow(clippy::type_complexity)]
static LOG_RELOAD: std::sync::OnceLock<
    Box<dyn Fn(aviutl2::tracing_subscriber::EnvFilter) + Send + Sync>,
> = std::sync::OnceLock::new();

/// 設定のログレベルを稼働中の subscriber へ反映する。
#[cfg(windows)]
pub fn apply_log_level(settings: &aviutl2_mcp_core::settings::Settings) {
    let Some(reload) = LOG_RELOAD.get() else {
        return;
    };
    let (filter, rejected) = log_filter(settings);
    report_rejected_log_level(rejected);
    reload(filter);
}

/// 設定のログレベルから filter を組み立てる。
///
/// 戻り値の 2 つ目は、**解釈できなかったために既定へ戻した指定**である。
/// `EnvFilter::new` は解釈に失敗しても値を返す lossy な口であり、そのまま使うと
/// 記録が `error` 以下へ落ちたことを誰も知らせない——運用上の記録（operation・
/// correlation_id・所要時間・結果コード）がまとめて消える。
#[cfg(windows)]
fn log_filter(
    settings: &aviutl2_mcp_core::settings::Settings,
) -> (aviutl2::tracing_subscriber::EnvFilter, Option<String>) {
    use aviutl2::tracing_subscriber::EnvFilter;
    use aviutl2_mcp_core::settings::DEFAULT_LOG_LEVEL;

    let configured = settings.effective_log_level();
    match EnvFilter::try_from_env(LOG_ENV) {
        Ok(filter) => (filter, None),
        Err(_) => match EnvFilter::try_new(configured) {
            Ok(filter) => (filter, None),
            Err(_) => (
                EnvFilter::new(DEFAULT_LOG_LEVEL),
                Some(configured.to_string()),
            ),
        },
    }
}

/// 解釈できなかったログレベルの指定を記録する。
#[cfg(windows)]
fn report_rejected_log_level(rejected: Option<String>) {
    if let Some(rejected) = rejected {
        let default = aviutl2_mcp_core::settings::DEFAULT_LOG_LEVEL;
        tracing::warn!("ログレベル {rejected} を解釈できないため {default} を用います");
    }
}

#[cfg(windows)]
#[aviutl2::plugin(GenericPlugin)]
struct AviUtl2McpPlugin {
    lifecycle: Option<Arc<lifecycle::Lifecycle>>,
    project_state: Option<Arc<project::ProjectState>>,
    pipe_server: Option<pipe::PipeServer>,
    /// レンダリングの実行口。終了手順が投入済みタスクの在庫を数えるために持つ。
    render_adapter: Option<Arc<render::HostRenderAdapter<render::sdk::SdkRenderHost>>>,
}

#[cfg(windows)]
impl aviutl2::generic::GenericPlugin for AviUtl2McpPlugin {
    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        init_tracing();
        Ok(Self {
            lifecycle: None,
            project_state: None,
            pipe_server: None,
            render_adapter: None,
        })
    }

    fn register(&mut self, registry: &mut aviutl2::generic::HostAppHandle) {
        init_tracing();
        EDIT_HANDLE.init(registry.create_edit_handle());

        // 設定画面はこの後の初期化に何も依存しない。**先に登録するのは、以降の
        // いずれかの段が失敗して戻った場合にも画面を開けるようにするためで
        // ある**——コールバックが触れるのは設定の読み書き口だけであり、
        // ここで用意する状態を 1 つも参照しない。
        //
        // ラッパーの設定メニュー用のマクロを使わない。マクロが生成するブリッジは
        // plugin の singleton のロックを保持したままハンドラを実行するが、
        // ハンドラは利用者がダイアログを閉じるまで戻らない。**その間の終了手順が
        // singleton へ到達できなくなる。**
        registry.register_config_menu(settings_ui::MENU_NAME, settings_ui::config_menu_callback);

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
            started_at,
            writer,
        ) {
            Ok(l) => Arc::new(l),
            Err(e) => {
                tracing::error!("lifecycle の初期化に失敗しました: {e:?}");
                return;
            }
        };

        // 停止の合図をここで作る。接続受理ループとレンダリングの完了待ちが
        // **同一の合図**を見る必要があり、待つ側は起動より前に組み立てる。
        let stop_signal = match pipe::StopSignal::new() {
            Ok(s) => Arc::new(s),
            Err(e) => {
                tracing::error!("停止イベントの作成に失敗しました: {e:?}");
                return;
            }
        };

        let read_adapter = read::sdk_read_adapter(project_state.clone());
        let edit_adapter = edit::sdk_edit_adapter(project_state.clone());
        // 成果物の置き場は descriptor と同じ基底から導く。基底を求められない
        // 場合は登録ごと打ち切る。既に作成済みの registry writer が同じ基底を
        // 要求しており、そこで失敗していれば手前で戻っているため、ここへ到達
        // するのは基底の解決が 2 回の間に壊れた場合だけである。**その状態で
        // 進んでも、書き出せない実行口を配ることにしかならない。**
        let render_adapter =
            match render::sdk_render_adapter(project_state, &instance_id, stop_signal.clone()) {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!("レンダリングの実行口の初期化に失敗しました: {e:?}");
                    return;
                }
            };
        let pipe_server = match pipe::PipeServer::start(
            lifecycle.clone(),
            read_adapter,
            edit_adapter,
            render_adapter.clone(),
            stop_signal,
        ) {
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
        self.render_adapter = Some(render_adapter);

        // agent plugin の生成は登録の最後に置く。**AviUtl2 の編集機能にも
        // instance の登録にも要らないものであり、要る側を先に済ませる。**
        // 手前のいずれかが失敗して戻った場合はここへ到達しないが、それでよい
        // ——registry writer が失敗していればルートを保護できておらず、pipe
        // server が失敗していれば marketplace の指す先が動かない。**どちらも
        // 「生成しない」が正しい。**
        //
        // 失敗も panic も呼び出し先が握って記録する。設定画面で確定したときと
        // **同じ関数を呼ぶ**——起動時のものは差分の是正であり、別の判断を
        // 持たない。
        agent_plugin::sync();
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

/// 終了へ向けてレンダリングの後始末をする。
///
/// **接続受理を止めた後に呼ぶ。** 止める前に在庫を数えると、その後に投入された
/// タスクを取りこぼす。
///
/// 本体を、後始末に要る口だけを受け取る関数へ切り出してある。終了手順の側を
/// 委譲だけにすることで、ここで行うことが plugin の他の状態に依存しないことを
/// 型で示し、SDK 無しでも確かめられるようにしている。
///
/// 待つ上限は呼び出し元が渡す。0 を渡せば待たずに切り離す。**設定をここで
/// 引かないのは、待つかどうかで振る舞いが変わる関数を、設定の現在値に依らず
/// 確かめられるようにするためである。**
#[cfg(windows)]
fn shutdown_renders<D>(render_adapter: Option<&Arc<D>>, timeout: Duration)
where
    D: render::RenderDrain + 'static,
{
    let Some(render_adapter) = render_adapter else {
        return;
    };
    render::drain_render_tasks(render_adapter, timeout);
    // 以後この instance が成果物を書くことはない。
    render_adapter.discard_artifacts();
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
///
/// `drain_renders` を `stop_pipe` の直後に置く順序が要である。接続受理を止める
/// 前にレンダリングの在庫を数えると、その後に投入されたタスクを取りこぼす。
#[cfg(windows)]
fn run_shutdown_sequence(
    stop_pipe: impl FnOnce(),
    drain_renders: impl FnOnce(),
    drain: impl FnOnce(),
    remove_descriptor: impl FnOnce(),
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(stop_pipe));
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(drain_renders));
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(drain));
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(remove_descriptor));
}

#[cfg(windows)]
impl Drop for AviUtl2McpPlugin {
    fn drop(&mut self) {
        let pipe_server = self.pipe_server.take();
        let lifecycle = self.lifecycle.take();
        let render_adapter = self.render_adapter.take();

        // pipe を停止してから descriptor を削除する。順序を逆にすると
        // descriptor が消えた後も pipe が接続を受け付ける窓ができる。
        //
        // レンダリングの在庫を空にするのは pipe を止めた直後である。止めた後で
        // なければ在庫が増え続け、数えた値がすぐ古くなる。
        run_shutdown_sequence(
            || {
                if let Some(pipe_server) = pipe_server {
                    pipe_server.stop(pipe::STOP_TIMEOUT);
                }
            },
            || shutdown_renders(render_adapter.as_ref(), render::render_drain_timeout()),
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
    use crate::test_support::{capture_logs, with_silent_panic_hook};

    #[test]
    fn shutdown_sequence_removes_descriptor_even_if_earlier_steps_panic() {
        let renders_drained = std::cell::Cell::new(false);
        let drained = std::cell::Cell::new(false);
        let removed = std::cell::Cell::new(false);

        with_silent_panic_hook(|| {
            run_shutdown_sequence(
                || panic!("pipe 停止時のログ出力が失敗しました"),
                || {
                    renders_drained.set(true);
                    panic!("レンダリングの完了待ちのログ出力が失敗しました");
                },
                || {
                    drained.set(true);
                    panic!("draining 遷移時のログ出力が失敗しました");
                },
                || removed.set(true),
            );
        });

        assert!(
            renders_drained.get(),
            "pipe 停止の panic でレンダリングの完了待ちが飛ばされました"
        );
        assert!(
            drained.get(),
            "レンダリングの完了待ちの panic で draining が飛ばされました"
        );
        assert!(
            removed.get(),
            "前段の panic で descriptor の削除が飛ばされました"
        );
    }

    /// 在庫と後始末の呼び出しを記録する終了口。
    #[derive(Default)]
    struct FakeRenderDrain {
        outstanding: usize,
        calls: std::sync::Mutex<Vec<&'static str>>,
    }

    impl FakeRenderDrain {
        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl render::RenderDrain for FakeRenderDrain {
        fn outstanding(&self) -> usize {
            self.outstanding
        }

        fn wait_all_tasks(&self) {
            self.calls.lock().unwrap().push("wait_all_tasks");
        }

        fn discard_artifacts(&self) {
            self.calls.lock().unwrap().push("discard_artifacts");
        }
    }

    /// 完了待ちが期限切れにならない上限。
    const AMPLE_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

    #[test]
    fn shutting_down_waits_for_the_renders_and_then_clears_the_artifacts() {
        // 待つ前に成果物を消しても、後で書かれた分が残る。順序も含めて固定する。
        let drain = Arc::new(FakeRenderDrain {
            outstanding: 1,
            ..FakeRenderDrain::default()
        });

        shutdown_renders(Some(&drain), AMPLE_DRAIN_TIMEOUT);

        assert_eq!(drain.calls(), vec!["wait_all_tasks", "discard_artifacts"]);
    }

    #[test]
    fn shutting_down_without_renders_still_clears_the_artifacts() {
        // 在庫が空でも、前の要求が残した成果物は消す。
        let drain = Arc::new(FakeRenderDrain::default());

        shutdown_renders(Some(&drain), AMPLE_DRAIN_TIMEOUT);

        assert_eq!(drain.calls(), vec!["discard_artifacts"]);
    }

    #[test]
    fn shutting_down_before_the_render_adapter_exists_does_nothing() {
        // 登録が途中で打ち切られた場合、実行口は無い。
        shutdown_renders(Option::<&Arc<FakeRenderDrain>>::None, AMPLE_DRAIN_TIMEOUT);
    }

    #[test]
    fn shutdown_sequence_runs_steps_in_order() {
        // レンダリングの在庫を数えるのは接続受理を止めた後でなければならない。
        // 止める前に数えると、その後に投入されたタスクを取りこぼす。
        let order = std::cell::RefCell::new(Vec::new());

        run_shutdown_sequence(
            || order.borrow_mut().push("pipe"),
            || order.borrow_mut().push("render"),
            || order.borrow_mut().push("drain"),
            || order.borrow_mut().push("remove"),
        );

        assert_eq!(
            order.into_inner(),
            vec!["pipe", "render", "drain", "remove"]
        );
    }

    #[test]
    fn shutdown_sequence_does_not_propagate_panic() {
        with_silent_panic_hook(|| {
            run_shutdown_sequence(
                || panic!("pipe"),
                || panic!("render"),
                || panic!("drain"),
                || panic!("remove"),
            );
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
            render_adapter: None,
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
                "2026-01-01T00:00:00.0000000Z".to_string(),
                registry::RegistryWriter::for_dir(root.clone()),
            )
            .unwrap(),
        );
        let plugin = AviUtl2McpPlugin {
            lifecycle: Some(lifecycle.clone()),
            project_state: Some(Arc::new(project::ProjectState::new())),
            pipe_server: None,
            render_adapter: None,
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

    /// ワークスペースのルート `Cargo.toml` が panic 戦略を上書きしていないこと。
    ///
    /// **`panic = "abort"` を持ち込むと、この crate の `catch_unwind` が
    /// すべて無言で死ぬ。** コンパイルも実行も通るが、panic が unwind しない
    /// ため捕捉に到達せず、要求 1 件の panic がホストのプロセスを落とす。
    ///
    /// 落ちたときに名前で理由を告げるための検査である。**性質としての確認は
    /// [`catch_unwind_actually_catches_a_panic`] が行う。どちらか一方にしない。**
    #[test]
    fn the_workspace_does_not_abort_on_panic() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("ワークスペースルートを辿れません")
            .join("Cargo.toml");
        let manifest = std::fs::read_to_string(&root).expect("ルートの Cargo.toml を読めません");

        for line in manifest.lines() {
            let line = line.trim();
            let Some(value) = line.strip_prefix("panic") else {
                continue;
            };
            let Some(value) = value.trim_start().strip_prefix('=') else {
                continue;
            };
            assert_eq!(
                value.trim().trim_matches('"'),
                "unwind",
                "ルートの Cargo.toml が panic 戦略を上書きしています: {line}"
            );
        }
    }

    /// `catch_unwind` が実際に panic を捕まえること。
    ///
    /// **`panic = "abort"` では、このテストはプロセスごと落ちて失敗する。**
    /// 設定ファイルの文字列を読む検査より強い——捕捉層が効いているかどうかを
    /// 性質として確かめる。
    #[test]
    fn catch_unwind_actually_catches_a_panic() {
        let caught = with_silent_panic_hook(|| {
            std::panic::catch_unwind(|| panic!("捕捉されるべき panic")).is_err()
        });
        assert!(caught, "panic が捕捉されませんでした");
    }

    /// 解釈できないログレベルが記録を消さないことを確かめる。
    ///
    /// `EnvFilter::new` は解釈に失敗しても値を返す lossy な口である。そのまま
    /// 使うと記録が `error` 以下へ落ち、`info` の運用ログ（operation・
    /// correlation_id・所要時間・結果コード）がまとめて消える。しかもそれを
    /// 告げる WARN 自体も出ない。
    #[test]
    fn an_unparsable_log_level_falls_back_to_the_default_and_is_reported() {
        use aviutl2::tracing_subscriber::filter::LevelFilter;
        use aviutl2_mcp_core::settings::{DEFAULT_LOG_LEVEL, Settings, SettingsDocument};

        let broken = SettingsDocument::parse(r#"{"log_level":"!!!"}"#)
            .unwrap()
            .resolve(&Settings::default())
            .0;
        assert_eq!(broken.effective_log_level(), "!!!");

        let (filter, rejected) = log_filter(&broken);

        assert_eq!(
            rejected.as_deref(),
            Some("!!!"),
            "解釈できない指定が記録されていません"
        );
        assert_eq!(
            filter.max_level_hint(),
            aviutl2::tracing_subscriber::EnvFilter::new(DEFAULT_LOG_LEVEL).max_level_hint(),
            "解釈できない指定で記録の水準が落ちました"
        );
        assert!(
            filter.max_level_hint() > Some(LevelFilter::ERROR),
            "運用ログが残らない水準へ落ちました"
        );

        // 解釈できる指定は素通しし、記録もしない。
        let sane = SettingsDocument::parse(r#"{"log_level":"warn"}"#)
            .unwrap()
            .resolve(&Settings::default())
            .0;
        assert_eq!(log_filter(&sane).1, None);
    }

    /// 起動時に設定が壊れていても登録は止めないが、理由は捨てない。
    #[test]
    fn a_startup_failure_is_kept_for_the_log() {
        use aviutl2_mcp_core::settings::SettingsReadError;

        let report = settings::StartupReport {
            issues: Vec::new(),
            failure: Some(SettingsReadError::Parse(
                aviutl2_mcp_core::settings::SettingsParseError::NotAnObject,
            )),
        };
        let logs = capture_logs(|| settings::report_startup(&report));

        assert!(
            logs.contains("WARN") && logs.contains("設定を読み込めませんでした"),
            "起動時の破損が記録されていません: {logs}"
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
