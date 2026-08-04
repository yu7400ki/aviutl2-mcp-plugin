//! 公開する tool の集合と、その変化の観測の単体テスト。

use super::*;
use crate::settings::{ParentPolicy, SettingsWatcher};
use aviutl2_mcp_core::settings::{SETTINGS_FILE_NAME, SettingsDocument, SettingsReader};
use aviutl2_mcp_core::tool::all_tool_names;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 設定の差し替えが監視スレッドへ届くまで待つ上限。
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(5);

/// 供給元が既に新しい値を持ってから、待ち受けが起きるまでに許す時間。
///
/// **押し出しであるため実際には待たない。** 有限にするのは届かない実装で試験が
/// 止まらないためであり、**この幅は「一定間隔で見に行く」実装が間に合わない
/// 長さに採ってある。**
const PUSH_DEADLINE: Duration = Duration::from_millis(100);

/// 起きないことを確かめるために様子を見る時間。
///
/// 供給元は既に新しい値を持っている。起きるなら即座に起きるため、この幅で
/// 起きなければ起きない。
const QUIET_WINDOW: Duration = Duration::from_millis(100);

/// 設定を書いた解決済み snapshot を作る。
fn settings_from(json: &str) -> Settings {
    SettingsDocument::parse(json)
        .expect("設定を解析できます")
        .resolve(&Settings::default())
        .0
}

/// server が登録している tool 名の代わりに使う catalog。
fn catalog() -> Vec<String> {
    all_tool_names().collect()
}

/// テスト用のディレクトリ。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("aviutl2-mcp-tool-catalog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("一時ディレクトリを作れます");
        Self(dir)
    }

    fn settings_path(&self) -> PathBuf {
        self.0.join(SETTINGS_FILE_NAME)
    }

    /// 設定ファイルを原子的に置き換える。
    fn replace_settings(&self, text: &str) {
        let temp = self
            .0
            .join(format!("settings.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temp, text).expect("一時ファイルへ書けます");
        std::fs::rename(&temp, self.settings_path()).expect("原子的に置換できます");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 初期 snapshot を作り終えた読み取り口。
fn reader_for(dir: &TempDir) -> SettingsReader {
    let mut reader = SettingsReader::new(dir.settings_path());
    reader.refresh();
    reader
}

/// `predicate` が真になるまで待つ。
fn wait_until(predicate: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + OBSERVE_TIMEOUT;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    predicate()
}

#[test]
fn the_always_enabled_tool_survives_being_disabled() {
    // floor は判定の最終段で適用する。公開しない指定に含まれていても公開する。
    let settings =
        settings_from(r#"{"disabled_tools":["aviutl2_list_instances","aviutl2_delete_object"]}"#);
    let visibility = ToolVisibility::from_settings(&settings);

    assert!(visibility.allows(ALWAYS_ENABLED_TOOL));
    assert!(!visibility.allows("aviutl2_delete_object"));

    let visible = visibility.visible(catalog().iter().map(String::as_str));
    assert!(visible.contains(ALWAYS_ENABLED_TOOL));
    assert!(!visible.contains("aviutl2_delete_object"));
}

#[test]
fn without_settings_every_tool_is_visible() {
    let visibility = ToolVisibility::all_enabled();
    let visible = visibility.visible(catalog().iter().map(String::as_str));
    assert_eq!(visible.len(), catalog().len());
}

#[test]
fn an_unknown_disabled_name_hides_nothing() {
    // 未知の名前は無視する。新しい plugin が書いた名前を古い server が読む場合で
    // あり、既知の tool を巻き添えにしない。
    let settings = settings_from(r#"{"disabled_tools":["aviutl2_future_tool","ping"]}"#);
    let visible =
        ToolVisibility::from_settings(&settings).visible(catalog().iter().map(String::as_str));
    assert_eq!(visible.len(), catalog().len());
}

#[test]
fn a_corrupt_file_leaves_every_tool_visible_at_startup() {
    // 起動時から解析できない場合は既定へ落ちる。全 tool が公開されたままになる。
    let dir = TempDir::new();
    dir.replace_settings("{ this is not json");
    let reader = reader_for(&dir);

    let visible = ToolVisibility::from_settings(&reader.settings())
        .visible(catalog().iter().map(String::as_str));
    assert_eq!(visible.len(), catalog().len());
}

/// 供給元が新しい値を持ってから起床するまでを測る。
///
/// **ファイルの変更が監視スレッドへ届くまでは測らない。** そこは押し出しでも
/// 見に行く形でも変わらない区間であり、含めると測りたい差が埋もれる。
async fn wake_after_delivery(watch: &mut ToolListWatch) -> bool {
    tokio::time::timeout(PUSH_DEADLINE, watch.changed())
        .await
        .expect("供給元が押し出すため待たずに起床します")
}

/// 様子を見て、起床しないことを確かめる。
async fn stays_quiet(watch: &mut ToolListWatch) -> bool {
    tokio::time::timeout(QUIET_WINDOW, watch.changed())
        .await
        .is_err()
}

#[tokio::test]
async fn the_watch_wakes_only_when_the_visible_set_changes() {
    let dir = TempDir::new();
    dir.replace_settings(r#"{"log_level":"info"}"#);
    let watcher = SettingsWatcher::start(reader_for(&dir), ParentPolicy::Require)
        .expect("監視を開始できます");
    let source = watcher.source();
    let mut watch = ToolListWatch::new(&source, catalog());
    assert_eq!(watch.visible().len(), catalog().len());

    // 公開する集合に関わらない項目だけを変える。設定は差し替わるが、
    // `tools/list` を取り直させる理由は無い。
    dir.replace_settings(r#"{"log_level":"debug"}"#);
    assert!(
        wait_until(|| source.settings().log_level() == Some("debug")),
        "設定の差し替えが届きませんでした"
    );
    assert!(
        stays_quiet(&mut watch).await,
        "集合が変わっていないのに起床しました"
    );

    // 公開する集合を変える。
    dir.replace_settings(r#"{"log_level":"debug","disabled_tools":["aviutl2_delete_object"]}"#);
    assert!(
        wait_until(|| source.settings().disabled_tools().len() == 1),
        "無効化の指定が届きませんでした"
    );
    assert!(
        wake_after_delivery(&mut watch).await,
        "集合の変化で起床しませんでした"
    );
    assert!(!watch.visible().contains("aviutl2_delete_object"));

    // 同じ変化で 2 度は起きない。
    assert!(stays_quiet(&mut watch).await);
}

#[tokio::test]
async fn disabling_only_the_always_enabled_tool_is_not_a_change() {
    // floor があるため、この指定は公開する集合を 1 つも動かさない。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"log_level":"info"}"#);
    let watcher = SettingsWatcher::start(reader_for(&dir), ParentPolicy::Require)
        .expect("監視を開始できます");
    let source = watcher.source();
    let mut watch = ToolListWatch::new(&source, catalog());

    dir.replace_settings(r#"{"log_level":"info","disabled_tools":["aviutl2_list_instances"]}"#);
    assert!(
        wait_until(|| source.settings().disabled_tools().len() == 1),
        "無効化の指定が届きませんでした"
    );

    assert!(stays_quiet(&mut watch).await);
    assert!(watch.visible().contains(ALWAYS_ENABLED_TOOL));
}

#[tokio::test]
async fn the_watch_ends_when_the_source_is_dropped() {
    // 待ち受けを畳む契機は供給元が失われることである。通知タスクはこれで終わる。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"log_level":"info"}"#);
    let watcher = SettingsWatcher::start(reader_for(&dir), ParentPolicy::Require)
        .expect("監視を開始できます");
    let source = watcher.source();
    let mut watch = ToolListWatch::new(&source, catalog());

    // 待ち受ける側は供給元を生かし続けない。
    drop(source);
    drop(watcher);

    assert!(
        !wake_after_delivery(&mut watch).await,
        "供給元が失われても待ち受けが終わりませんでした"
    );
}

#[tokio::test]
async fn what_the_watch_consumed_does_not_change_what_the_visibility_reports() {
    // 通知の送信が失敗しても、待ち受けの記録だけが進む。公開の判定は毎回 snapshot を
    // 読み直すため、次の `tools/list` は正しい集合を返す。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"log_level":"info"}"#);
    let watcher = SettingsWatcher::start(reader_for(&dir), ParentPolicy::Require)
        .expect("監視を開始できます");
    let source = watcher.source();
    let mut watch = ToolListWatch::new(&source, catalog());

    dir.replace_settings(r#"{"disabled_tools":["aviutl2_delete_effect"]}"#);
    assert!(
        wait_until(|| source.settings().disabled_tools().len() == 1),
        "無効化の指定が届きませんでした"
    );
    assert!(wake_after_delivery(&mut watch).await);

    // 起床を済ませた後（＝通知を送ったつもりで失敗した後）でも、判定は snapshot を
    // 読み直して同じ結論に至る。
    let visible = ToolVisibility::from_settings(&source.settings())
        .visible(catalog().iter().map(String::as_str));
    assert!(!visible.contains("aviutl2_delete_effect"));
    assert_eq!(&visible, watch.visible());
}
