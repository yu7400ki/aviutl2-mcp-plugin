//! 設定の監視と snapshot の単体テスト。

use super::*;
use crate::test_support::capture_logs;
use aviutl2_mcp_core::settings::MIN_ARTIFACT_TTL_SECONDS;
use std::path::PathBuf;

/// 変更が監視スレッドへ届くまで待つ上限。
///
/// 通知は数ミリ秒で届く。長めに採るのは、負荷の高い並列実行でも取り違えない
/// ためである。
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(5);

/// テスト用のディレクトリ。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "aviutl2-mcp-settings-watch-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn settings_path(&self) -> PathBuf {
        self.0.join(SETTINGS_FILE_NAME)
    }

    /// 設定ファイルを原子的に置き換える。
    ///
    /// 一時ファイルへ書いてから名前を差し替える。**これがファイルの identity を
    /// 差し替える経路であり、ファイルを掴んだ監視が取りこぼす形である。**
    fn replace_settings(&self, text: &str) {
        let temp = self
            .0
            .join(format!("settings.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temp, text).unwrap();
        std::fs::rename(&temp, self.settings_path()).unwrap();
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
fn the_watcher_detects_an_atomic_replace() {
    // 原子的置換はファイルの identity を差し替える。親ディレクトリを監視して
    // いれば、作成も置換も同じ経路で届く。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"log_level":"debug"}"#);
    let watcher = SettingsWatcher::start(reader_for(&dir), ParentPolicy::Require)
        .expect("監視を開始できます");
    let source = watcher.source();
    assert_eq!(source.settings().log_level(), Some("debug"));

    dir.replace_settings(r#"{"log_level":"trace"}"#);

    assert!(
        wait_until(|| source.settings().log_level() == Some("trace")),
        "原子的置換が検出されませんでした"
    );
}

#[test]
fn a_watch_request_is_recorded_as_pending() {
    // **発行の成功は「登録された」ことしか意味しない。** 完了として扱うと保留
    // 状態が記録されず、`Drop` がキャンセルを飛ばして受信バッファを解放する
    // ——カーネルはその後もそこへ書き込む。
    //
    // 危険が現れるのは「発行したまま資源を手放す」瞬間であり、試験の中では
    // 資源の解放が速すぎて観測できない。**記録が付いていることを直接見る。**
    let dir = TempDir::new();
    let directory = DirectoryHandle::open(&dir.0).expect("ディレクトリを開けます");
    let mut buffer = vec![0u32; NOTIFY_BUFFER_BYTES / size_of::<u32>()];
    // SAFETY: `directory` は本テストの終わりまで生存し、`op` はその前に drop
    // される（後に宣言したものから drop される）。`buffer` も同様である。
    let mut op = unsafe { OverlappedOp::new(directory.handle()) }.expect("I/O を用意できます");

    issue_watch(&mut op, &directory, &mut buffer).expect("変更通知を要求できます");

    assert!(
        op.is_pending(),
        "変更通知の要求が保留として記録されていません"
    );
}

#[test]
fn a_change_made_before_the_watch_was_registered_is_still_picked_up() {
    // 初期 snapshot を作ってから最初の要求を登録するまでの間に起きた変更は、
    // 通知として届かない。**ここで書き込むのは監視を始める前であり、通知は
    // 原理的に出ない。** 登録直後の読み直しだけが拾える。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"log_level":"debug"}"#);
    let reader = reader_for(&dir);
    assert_eq!(reader.settings().log_level(), Some("debug"));

    dir.replace_settings(r#"{"log_level":"trace"}"#);

    let watcher =
        SettingsWatcher::start(reader, ParentPolicy::Require).expect("監視を開始できます");
    let source = watcher.source();

    assert!(
        wait_until(|| source.settings().log_level() == Some("trace")),
        "監視を始める前の変更が取り込まれませんでした"
    );
}

#[test]
fn the_watcher_detects_a_file_created_after_it_started() {
    // 監視の起点はディレクトリなので、まだ無いファイルの作成も検出できる。
    let dir = TempDir::new();
    let watcher = SettingsWatcher::start(reader_for(&dir), ParentPolicy::Require)
        .expect("監視を開始できます");
    let source = watcher.source();
    assert_eq!(*source.settings(), Settings::default());

    dir.replace_settings(r#"{"artifact":{"ttl_seconds":1200}}"#);

    assert!(
        wait_until(|| source.settings().artifact_ttl() == Duration::from_secs(1200)),
        "作成が検出されませんでした"
    );
}

#[test]
fn consecutive_writes_settle_on_the_last_state() {
    // debounce を持たないため記録の数だけ読み直し得るが、再読込は冪等であり、
    // 観測される値は常に最後の状態である。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"log_level":"debug"}"#);
    let watcher = SettingsWatcher::start(reader_for(&dir), ParentPolicy::Require)
        .expect("監視を開始できます");
    let source = watcher.source();

    for level in ["warn", "error", "trace"] {
        dir.replace_settings(&format!(r#"{{"log_level":"{level}"}}"#));
    }

    assert!(
        wait_until(|| source.settings().log_level() == Some("trace")),
        "最後の状態が反映されませんでした"
    );
    // 中間の状態へ戻ることはない。読み直しはそのつどファイルの現在の内容を
    // 読むため、遅れて走った読み直しも同じ最後の状態しか見ない。
}

#[test]
fn a_corrupt_file_keeps_the_last_known_good_snapshot() {
    // **「何も起きないこと」を待って確かめない。** 破損の後に検出できる変更を
    // 続けて置き、それが届いた時点で差し替えの回数を数える。破損が差し替えを
    // 起こしていれば回数が 1 つ多くなる。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"artifact":{"ttl_seconds":120}}"#);
    let watcher = SettingsWatcher::start(reader_for(&dir), ParentPolicy::Require)
        .expect("監視を開始できます");
    let source = watcher.source();
    assert_eq!(source.settings().artifact_ttl(), Duration::from_secs(120));

    dir.replace_settings("{ broken");
    dir.replace_settings(r#"{"artifact":{"ttl_seconds":900}}"#);

    assert!(
        wait_until(|| source.settings().artifact_ttl() == Duration::from_secs(900)),
        "破損の後の変更が届きませんでした"
    );
    assert_eq!(
        source.applied(),
        1,
        "破損が snapshot の差し替えを起こしました"
    );
}

#[test]
fn the_watcher_stops_when_it_is_dropped() {
    // 停止は保留中の I/O のキャンセルを伴う。`OverlappedOp` の `Drop` が完了を
    // 確定させるまで戻らないため、ここで待ち続ける実装なら試験ごと固まる。
    //
    // **`drop` は監視スレッドを join する。** 戻った時点でスレッドは終わって
    // おり、以後の変更を観測する主体が居ない。**待って確かめる必要が無い。**
    let dir = TempDir::new();
    dir.replace_settings(r#"{"log_level":"debug"}"#);
    let watcher = SettingsWatcher::start(reader_for(&dir), ParentPolicy::Require)
        .expect("監視を開始できます");
    let source = watcher.source();
    assert_eq!(source.settings().log_level(), Some("debug"));

    drop(watcher);

    dir.replace_settings(r#"{"log_level":"trace"}"#);
    assert_eq!(
        source.settings().log_level(),
        Some("debug"),
        "停止後の変更が反映されました"
    );
}

#[test]
fn dropping_the_watcher_releases_the_directory() {
    // 監視を保持したままでもディレクトリを消せる（共有に削除を含めている）。
    // 消せなければ、上書き先を使う経路が後始末できない。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"log_level":"debug"}"#);
    let watcher = SettingsWatcher::start(reader_for(&dir), ParentPolicy::Require)
        .expect("監視を開始できます");

    std::fs::remove_file(dir.settings_path()).expect("監視中でも設定ファイルを消せます");
    drop(watcher);
}

#[test]
fn a_missing_parent_is_an_error_when_it_must_not_be_created() {
    // 外から指定された場所は利用者のものである。**作らないし、ACL も変えない。**
    let missing = std::env::temp_dir().join(format!(
        "aviutl2-mcp-settings-missing-{}",
        uuid::Uuid::new_v4()
    ));
    let reader = SettingsReader::new(missing.join(SETTINGS_FILE_NAME));

    let error = SettingsWatcher::start(reader, ParentPolicy::Require)
        .err()
        .expect("存在しない置き場所では監視を始められません");
    assert!(matches!(error, SettingsWatchError::ParentMissing));
    assert!(!missing.exists(), "存在しない置き場所を作りました");
}

#[test]
fn a_missing_parent_is_created_with_a_protected_dacl() {
    let base = std::env::temp_dir().join(format!(
        "aviutl2-mcp-settings-create-{}",
        uuid::Uuid::new_v4()
    ));
    let reader = SettingsReader::new(base.join(SETTINGS_FILE_NAME));

    let watcher =
        SettingsWatcher::start(reader, ParentPolicy::Create).expect("置き場所を用意できます");
    aviutl2_mcp_win::test_support::assert_protected_dacl(&base);
    drop(watcher);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn out_of_range_values_are_clamped_before_they_reach_the_snapshot() {
    // 丸めは共有の解決手続きが行う。server 側に独自の範囲判定は無い。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"artifact":{"ttl_seconds":1}}"#);
    let watcher = SettingsWatcher::start(reader_for(&dir), ParentPolicy::Require)
        .expect("監視を開始できます");

    assert_eq!(
        watcher.source().settings().artifact_ttl(),
        Duration::from_secs(MIN_ARTIFACT_TTL_SECONDS)
    );
}

#[test]
fn a_corrupt_file_is_reported_instead_of_being_swallowed_by_a_retry() {
    // **読み取り口は読めた時点で印を進める。** 破損を再試行すると 2 回目は
    // `Unchanged` になり、記録を残す枝へ永久に到達しない。運用者が設定の破損を
    // 知る機会はここしか無く、しかも設定画面で保存すると痕跡なく作り直される。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"artifact":{"ttl_seconds":120}}"#);
    let mut reader = reader_for(&dir);
    let source = SettingsSource::fixed((*reader.settings()).clone());

    dir.replace_settings("{ broken");
    let (outcome, logs) = capture_logs(|| reload(&mut reader, &source));

    assert_eq!(
        outcome,
        ReloadOutcome::Corrupt,
        "破損が破損として報告されていません"
    );
    assert!(
        logs.contains("WARN") && logs.contains("設定を解析できませんでした"),
        "破損を告げる WARN が記録されていません: {logs}"
    );
    // last-known-good は維持される。
    assert_eq!(source.settings().artifact_ttl(), Duration::from_secs(120));
    assert_eq!(source.applied(), 0);
}

#[test]
fn an_unchanged_file_is_neither_reported_nor_applied() {
    // 破損の記録が「変わっていない」ときにまで出ては、記録の意味が薄れる。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"log_level":"debug"}"#);
    let mut reader = reader_for(&dir);
    let source = SettingsSource::fixed((*reader.settings()).clone());

    let (outcome, logs) = capture_logs(|| reload(&mut reader, &source));

    assert_eq!(outcome, ReloadOutcome::Unchanged);
    assert!(!logs.contains("WARN"), "余計な WARN が出ています: {logs}");
}

// ============================================================================
// 変化の押し出し
// ============================================================================

#[test]
fn a_subscriber_is_woken_by_the_watch_thread_without_polling() {
    // 監視スレッドは値が変わったことを知っている。待ち受ける口が無ければ、
    // 下流は定期的に問い合わせるしかない——`ReadDirectoryChangesW` へ
    // 切り替えて消したはずのポーリングが 1 段下流で復活する。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"log_level":"debug"}"#);
    let watcher = SettingsWatcher::start(reader_for(&dir), ParentPolicy::Require)
        .expect("監視を開始できます");
    let mut receiver = watcher.source().subscribe();
    assert!(!receiver.has_changed().unwrap(), "最初から起床しています");

    dir.replace_settings(r#"{"log_level":"trace"}"#);

    // 問い合わせずに起床する。届いた値は最新のものである。
    let woken = std::thread::spawn(move || {
        let deadline = Instant::now() + OBSERVE_TIMEOUT;
        while Instant::now() < deadline {
            if receiver.has_changed().unwrap_or(false) {
                return Some(receiver.borrow_and_update().clone());
            }
            std::thread::yield_now();
        }
        None
    })
    .join()
    .expect("購読側のスレッドが panic しました");

    let settings = woken.expect("購読側が起床しませんでした");
    assert_eq!(settings.log_level(), Some("trace"));
}

#[test]
fn a_write_that_changes_nothing_does_not_wake_a_subscriber() {
    // 原子的置換は一時ファイルの作成と rename で複数の記録を生む。記録の数だけ
    // 知らせると、変化していない通知が並ぶ。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"log_level":"debug"}"#);
    let mut reader = reader_for(&dir);
    let source = SettingsSource::fixed((*reader.settings()).clone());
    let receiver = source.subscribe();

    dir.replace_settings(r#"{"log_level":"debug"}"#);
    assert_eq!(reload(&mut reader, &source), ReloadOutcome::Same);
    assert!(
        !receiver.has_changed().unwrap(),
        "値が変わっていないのに起床しました"
    );

    dir.replace_settings(r#"{"log_level":"trace"}"#);
    assert_eq!(reload(&mut reader, &source), ReloadOutcome::Applied);
    assert!(receiver.has_changed().unwrap(), "変化が届きませんでした");
}

#[test]
fn the_watch_thread_keeps_running_without_any_subscriber() {
    // 購読者が居ないことは通知の失敗ではない。失敗として扱うと、誰も聞いて
    // いない間に監視が止まる。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"log_level":"debug"}"#);
    let watcher = SettingsWatcher::start(reader_for(&dir), ParentPolicy::Require)
        .expect("監視を開始できます");
    let source = watcher.source();

    for level in ["warn", "error", "trace"] {
        dir.replace_settings(&format!(r#"{{"log_level":"{level}"}}"#));
        assert!(
            wait_until(|| source.settings().log_level() == Some(level)),
            "購読者が居ない状態で監視が止まりました（{level}）"
        );
    }
}

#[test]
fn dropping_the_source_ends_the_subscription() {
    // 供給元が消えたことを購読側が観測できなければ、待ち受けるタスクが
    // 終われない。
    let source = SettingsSource::fixed(Settings::default());
    let receiver = source.subscribe();
    assert!(receiver.has_changed().is_ok());

    drop(source);

    assert!(
        receiver.has_changed().is_err(),
        "供給元の消滅を観測できません"
    );
}

#[test]
fn the_watch_thread_sends_from_outside_a_runtime() {
    // 監視スレッドは `std::thread` であり、非同期ランタイムの上に無い。
    // 送出がランタイムを要求する形なら、ここで panic する。
    let source = SettingsSource::fixed(Settings::default());
    let receiver = source.subscribe();

    let changed = std::thread::spawn({
        let source = Arc::clone(&source);
        move || source.replace_if_changed(Arc::new(settings_with_log_level("trace")))
    })
    .join()
    .expect("ランタイム外からの送出で panic しました");

    assert!(changed);
    assert_eq!(receiver.borrow().log_level(), Some("trace"));
}

/// ログレベルだけを指定した設定を作る。
fn settings_with_log_level(level: &str) -> Settings {
    aviutl2_mcp_core::settings::SettingsDocument::parse(&format!(r#"{{"log_level":"{level}"}}"#))
        .unwrap()
        .resolve(&Settings::default())
        .0
}

#[test]
fn the_snapshot_is_replaced_only_when_the_value_changes() {
    // 同じ内容を書き直しても差し替えない。通知が重複しても、有効集合が実際に
    // 変わったときだけ下流へ伝わる根拠になる。
    let dir = TempDir::new();
    dir.replace_settings(r#"{"log_level":"debug"}"#);
    let mut reader = reader_for(&dir);
    let source = SettingsSource::fixed((*reader.settings()).clone());

    dir.replace_settings(r#"{"log_level":"debug"}"#);
    assert_eq!(
        reload(&mut reader, &source),
        ReloadOutcome::Same,
        "同じ内容で差し替えました"
    );
    assert_eq!(source.applied(), 0);

    dir.replace_settings(r#"{"log_level":"trace"}"#);
    assert_eq!(
        reload(&mut reader, &source),
        ReloadOutcome::Applied,
        "変更が反映されませんでした"
    );
    assert_eq!(source.applied(), 1);
    assert_eq!(source.settings().log_level(), Some("trace"));
}

// ============================================================================
// 変更記録の読み解き
// ============================================================================

/// `FILE_NOTIFY_INFORMATION` の並びを組み立てる。
fn notify_buffer(names: &[&str]) -> (Vec<u32>, u32) {
    let mut bytes: Vec<u8> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    for name in names {
        starts.push(bytes.len());
        let units: Vec<u16> = name.encode_utf16().collect();
        bytes.extend_from_slice(&0u32.to_ne_bytes()); // NextEntryOffset（後で埋める）
        bytes.extend_from_slice(&1u32.to_ne_bytes()); // Action
        bytes.extend_from_slice(&((units.len() * 2) as u32).to_ne_bytes()); // FileNameLength
        for unit in units {
            bytes.extend_from_slice(&unit.to_ne_bytes());
        }
        while !bytes.len().is_multiple_of(4) {
            bytes.push(0);
        }
    }
    for (index, start) in starts.iter().enumerate() {
        let next = match starts.get(index + 1) {
            Some(next) => (next - start) as u32,
            None => 0,
        };
        bytes[*start..*start + 4].copy_from_slice(&next.to_ne_bytes());
    }
    let transferred = bytes.len() as u32;
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    (words, transferred)
}

#[test]
fn a_record_for_our_settings_file_demands_a_reload() {
    let name = settings_file_name(Path::new(r"C:\base\settings.json"));
    let (buffer, transferred) = notify_buffer(&["settings.json"]);
    assert!(demands_reload(Some(transferred), &buffer, &name));

    // 大小違いも同じファイルである。
    let (buffer, transferred) = notify_buffer(&["SETTINGS.JSON"]);
    assert!(demands_reload(Some(transferred), &buffer, &name));
}

#[test]
fn records_for_other_files_do_not_demand_a_reload() {
    let name = settings_file_name(Path::new(r"C:\base\settings.json"));
    let (buffer, transferred) = notify_buffer(&["other.json", "settings.json.tmp"]);
    assert!(!demands_reload(Some(transferred), &buffer, &name));
}

#[test]
fn a_chain_that_contains_our_file_demands_a_reload() {
    // 原子的置換は一時ファイルの作成と rename を連ねる。連鎖の途中に本体が
    // 現れる形で届く。
    let name = settings_file_name(Path::new(r"C:\base\settings.json"));
    let (buffer, transferred) = notify_buffer(&["settings.abc.tmp", "settings.json"]);
    assert!(demands_reload(Some(transferred), &buffer, &name));
}

#[test]
fn an_overflowed_buffer_demands_a_reload_without_reading_records() {
    // バッファ溢れは「何かが変わった」以上の情報を持たない。転送 0 バイトでも
    // 取得の失敗でも、記録を読まずに読み直す。
    let name = settings_file_name(Path::new(r"C:\base\settings.json"));
    let (buffer, _) = notify_buffer(&["other.json"]);
    assert!(demands_reload(Some(0), &buffer, &name));
    assert!(demands_reload(None, &buffer, &name));
}

#[test]
fn a_truncated_record_demands_a_reload() {
    // 読み解けない記録は取りこぼす側ではなく読み直す側へ倒す。
    let name = settings_file_name(Path::new(r"C:\base\settings.json"));
    let (buffer, transferred) = notify_buffer(&["other.json"]);
    assert!(demands_reload(Some(transferred - 4), &buffer, &name));
}

#[test]
fn the_overflow_error_is_recognised_by_its_win32_code() {
    // 実際の溢れは 4 KiB を超える記録が要り、試験で確実には起こせない。
    // 分類そのものをここで固定する。
    let overflow = io::Error::from_raw_os_error(ERROR_NOTIFY_ENUM_DIR.0 as i32);
    assert!(is_notify_overflow(&overflow));
    assert!(!is_notify_overflow(&io::Error::from_raw_os_error(5)));
}
