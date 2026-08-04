//! server 側の設定の監視と snapshot。
//!
//! # ディレクトリ監視ではなくポーリングで見る
//!
//! 設定ファイルは原子的置換で差し替わる。**開いたままのハンドルを持つ監視は
//! identity の差し替えを取りこぼす**が、毎回パスを `stat` する方式は取りこぼさ
//! ない——ハンドルを持たないためである。退けたのは「ファイルを監視する」ことで
//! あって、「ファイルを見に行く」ことではない。
//!
//! | 観点 | ディレクトリ監視 | ポーリング |
//! |---|---|---|
//! | 依存 | 監視の crate、または自前の `ReadDirectoryChangesW` | 無し |
//! | debounce | 要る（一時ファイル作成 + rename が連発する） | 要らない（間隔がそのまま debounce になる） |
//! | 取りこぼし | ハンドル方式では起きる | 同一間隔内の 2 回の変更は最後の状態だけが見える |
//!
//! **設定変更は人手による稀な操作であり、1 秒の遅れは体感されない。** 間隔は
//! 設定にしない。利用者が調整して得るものが無い。
//!
//! # snapshot は 1 回の差し替えで反映する
//!
//! 読み手は [`SettingsSource::settings`] で現在の `Arc` を取り、その後の
//! 差し替えに影響されない。半分だけ適用された状態を観測する経路が無い。

use aviutl2_mcp_core::settings::{Settings, SettingsReader, SettingsRefresh};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::warn;

/// 設定ファイルを見に行く間隔。
///
/// **設定にしない。** 通知は最適化であり、正しさは要求のたびの判定が担保する。
/// 間隔を延ばしても壊れない量に、利用者が触る意味は無い。
pub const SETTINGS_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// 現在の設定を配る口。
///
/// 反映は 1 回の `Arc` の差し替えで行う。`tools/list` の判定も call-time の
/// 受付判定も同じ供給元を読むため、半適用の状態で食い違うことがない。
#[derive(Debug)]
pub struct SettingsSource {
    current: RwLock<Arc<Settings>>,
    /// 差し替えた回数。
    ///
    /// 間隔内の複数回の書き込みが 1 回だけ反映されることを、試験がこの値で
    /// 確かめる。
    applied: AtomicU64,
}

impl SettingsSource {
    /// 固定の設定を配る口を作る。
    ///
    /// 監視を持たない構築口であり、試験と、設定を必要としない利用側が使う。
    pub fn fixed(settings: Settings) -> Arc<Self> {
        Arc::new(Self {
            current: RwLock::new(Arc::new(settings)),
            applied: AtomicU64::new(0),
        })
    }

    /// 現在の設定。
    pub fn settings(&self) -> Arc<Settings> {
        Arc::clone(&self.current.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// 差し替えた回数。
    pub fn applied(&self) -> u64 {
        self.applied.load(Ordering::Acquire)
    }

    fn replace(&self, settings: Arc<Settings>) {
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = settings;
        self.applied.fetch_add(1, Ordering::AcqRel);
    }
}

/// 設定ファイルを一定間隔で見に行く 1 本のスレッド。
///
/// **plugin と違い server はスレッドを 1 本持つ。** 有効 tool の集合が変わった
/// ことを要求元へ知らせる経路は、要求が来るまで気付けない形では動かない。
/// server は AviUtl2 のプロセスの外にあり、スレッドを 1 本増やす費用は plugin と
/// 同じではない。
pub struct SettingsWatcher {
    source: Arc<SettingsSource>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl SettingsWatcher {
    /// 起動時に読んだ読み取り口を引き継いで監視を始める。
    ///
    /// 初期 snapshot は呼び出し元が作る。**MCP の受付を始める前に読み終えて
    /// いる**ことが要るためであり、その時点の記録の準備は呼び出し元しか
    /// 知らない。
    pub fn start(reader: SettingsReader, interval: Duration) -> Self {
        let source = Arc::new(SettingsSource {
            current: RwLock::new(reader.settings()),
            applied: AtomicU64::new(0),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let handle = std::thread::Builder::new()
            .name("aviutl2-mcp-settings".to_string())
            .spawn({
                let source = Arc::clone(&source);
                let stop = Arc::clone(&stop);
                let mut reader = reader;
                move || {
                    while !stop.load(Ordering::Acquire) {
                        std::thread::sleep(interval);
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        poll_once(&mut reader, &source);
                    }
                }
            })
            .ok();
        Self {
            source,
            stop,
            handle,
        }
    }

    /// 現在の設定を配る口。
    pub fn source(&self) -> Arc<SettingsSource> {
        Arc::clone(&self.source)
    }
}

impl Drop for SettingsWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// 1 回分のポーリング。
///
/// 更新時刻と大きさが前回と同じなら何もしない。読み取りが一時的に失敗した
/// 場合は有限回だけ試み、なお失敗すれば破損として直前の snapshot を維持する。
/// **破損が、無効化していた tool を無言で再公開してはならない。**
fn poll_once(reader: &mut SettingsReader, source: &SettingsSource) -> bool {
    for attempt in 1..=aviutl2_mcp_core::settings::SETTINGS_READ_ATTEMPTS {
        match reader.refresh() {
            SettingsRefresh::Unchanged => return false,
            SettingsRefresh::Reloaded(issues) => {
                for issue in &issues {
                    warn!("設定を補正しました: {issue}");
                }
                source.replace(reader.settings());
                return true;
            }
            SettingsRefresh::Failed(e) => {
                if attempt == aviutl2_mcp_core::settings::SETTINGS_READ_ATTEMPTS {
                    warn!("設定を読み直せませんでした。直前の設定を維持します: {e}");
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::settings::MIN_ARTIFACT_TTL_SECONDS;
    use std::path::{Path, PathBuf};

    fn temp_settings_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "aviutl2-mcp-server-settings-{}.json",
            uuid::Uuid::new_v4()
        ))
    }

    fn write(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_replaced_file_becomes_visible_on_the_next_poll() {
        // 原子的置換で identity が差し替わっても、毎回パスを見に行く方式は
        // 取りこぼさない。
        let path = temp_settings_path();
        write(&path, r#"{"log_level":"debug"}"#);
        let mut reader = SettingsReader::new(path.clone());
        reader.refresh();
        let source = SettingsSource::fixed((*reader.settings()).clone());
        assert_eq!(source.settings().log_level(), "debug");

        let replacement = path.with_extension("tmp");
        write(&replacement, r#"{"log_level":"trace"}"#);
        std::fs::rename(&replacement, &path).unwrap();

        assert!(poll_once(&mut reader, &source));
        assert_eq!(source.settings().log_level(), "trace");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn several_writes_within_one_interval_apply_once_as_the_last_state() {
        // 間隔がそのまま debounce になる。間隔内の複数回の書き込みは、最後の
        // 状態として 1 回だけ反映される。
        let path = temp_settings_path();
        write(&path, r#"{"log_level":"debug"}"#);
        let mut reader = SettingsReader::new(path.clone());
        reader.refresh();
        let source = SettingsSource::fixed((*reader.settings()).clone());
        let applied = source.applied();

        write(&path, r#"{"log_level":"warn"}"#);
        write(&path, r#"{"log_level":"error"}"#);
        write(&path, r#"{"log_level":"trace"}"#);

        assert!(poll_once(&mut reader, &source));
        assert_eq!(
            source.applied(),
            applied + 1,
            "書き込みの回数だけ反映しました"
        );
        assert_eq!(source.settings().log_level(), "trace");

        // 変わっていなければ差し替えない。
        assert!(!poll_once(&mut reader, &source));
        assert_eq!(source.applied(), applied + 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_corrupt_file_keeps_the_last_known_good_snapshot() {
        let path = temp_settings_path();
        write(&path, r#"{"artifact":{"ttl_seconds":120}}"#);
        let mut reader = SettingsReader::new(path.clone());
        reader.refresh();
        let source = SettingsSource::fixed((*reader.settings()).clone());

        write(&path, "{ broken");
        assert!(!poll_once(&mut reader, &source));
        assert_eq!(source.settings().artifact_ttl(), Duration::from_secs(120));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_file_yields_the_defaults() {
        let path = temp_settings_path();
        let mut reader = SettingsReader::new(path.clone());
        let source = SettingsSource::fixed(Settings::default());

        assert!(poll_once(&mut reader, &source));
        assert_eq!(*source.settings(), Settings::default());
    }

    #[test]
    fn out_of_range_values_are_clamped_before_they_reach_the_snapshot() {
        // 丸めは共有の解決手続きが行う。server 側に独自の範囲判定は無い。
        let path = temp_settings_path();
        write(&path, r#"{"artifact":{"ttl_seconds":1}}"#);
        let mut reader = SettingsReader::new(path.clone());
        let source = SettingsSource::fixed(Settings::default());

        assert!(poll_once(&mut reader, &source));
        assert_eq!(
            source.settings().artifact_ttl(),
            Duration::from_secs(MIN_ARTIFACT_TTL_SECONDS)
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_watcher_stops_when_it_is_dropped() {
        let path = temp_settings_path();
        write(&path, r#"{"log_level":"debug"}"#);
        let mut reader = SettingsReader::new(path.clone());
        reader.refresh();

        let watcher = SettingsWatcher::start(reader, Duration::from_millis(10));
        let source = watcher.source();
        assert_eq!(source.settings().log_level(), "debug");
        drop(watcher);

        let _ = std::fs::remove_file(&path);
    }
}
