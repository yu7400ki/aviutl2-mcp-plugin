//! plugin 側の設定の読み書き。
//!
//! # 監視スレッドを持たない
//!
//! 読み直しの契機は 3 つだけである。
//!
//! | 契機 | 何のため |
//! |---|---|
//! | 登録の中（起動時） | 初期値を得る。**失敗しても登録を止めない** |
//! | 設定画面が保存に成功した直後 | 書いた内容をそのまま反映する。読み直さない |
//! | 要求 1 件の処理を始めるとき | 別のプロセスが書いた変更を取り込む |
//!
//! 監視スレッドを足せば、止め方・切り離し方・panic の扱いを 1 組増やすことに
//! なる。得られるのは「設定が数秒早く効く」ことだけである。plugin が設定を使う
//! のは要求を処理するときであり、**要求が来ないときに設定が古いことは誰にも
//! 観測されない。**
//!
//! 要求 1 件あたりの費用は設定ファイルの読み取り 1 回である。**読むのは印を
//! 内容から作るためであり、内容が前回と同じならそこで止まる**——解析も
//! snapshot の確保も行わない。反映の契機が要求の受理である以上、
//! **「必ず即座に反映する」を約束しない。**
//!
//! # 読めなくても止まらない
//!
//! 設定が読めないことは、インスタンスを登録しない理由にならない。保護 DACL の
//! 検証失敗が登録を止めるのとは対照的である——**守れないなら止め、決められない
//! なら既定で動く。**

use crate::atomic_file::write_protected_atomic;
use anyhow::{Context, Result, anyhow};
use aviutl2_mcp_core::settings::{
    Settings, SettingsChange, SettingsDocument, SettingsIssue, SettingsReadError, SettingsReader,
    SettingsRefresh, settings_path,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;
use windows::Win32::Foundation::{HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::{Owned, PCWSTR};

/// read-modify-write を調停するユーザー単位の名前付き mutex。
///
/// `Local\` 名前空間はログオンセッションに閉じる。設定ファイルも
/// `%LOCALAPPDATA%` の下にあり、共有する範囲が一致する。
const SETTINGS_MUTEX_NAME: &str = r"Local\AviUtl2Mcp-settings";

/// 名前付き mutex の獲得を待つ上限。
///
/// 保持するのは「最新ファイルの再読込 → 変更点の merge → 原子的置換」の区間
/// だけであり、通常はミリ秒で終わる。待ち切れない場合は保存を失敗させ、
/// 設定画面が利用者へ伝える。
const SETTINGS_MUTEX_TIMEOUT: Duration = Duration::from_secs(5);

/// 設定の現在値と読み取り口。
struct SettingsState {
    /// 場所を解決できた場合の読み取り口。解決できなければ既定値で動く。
    reader: Option<SettingsReader>,
    /// 読み取り口が無い場合に配る既定値。
    fallback: Arc<Settings>,
}

impl SettingsState {
    fn settings(&self) -> Arc<Settings> {
        match &self.reader {
            Some(reader) => reader.settings(),
            None => Arc::clone(&self.fallback),
        }
    }
}

static STATE: LazyLock<Mutex<SettingsState>> = LazyLock::new(|| {
    Mutex::new(SettingsState {
        reader: None,
        fallback: Arc::new(Settings::default()),
    })
});

fn state() -> MutexGuard<'static, SettingsState> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 設定ファイルの場所。
///
/// registry と同じ基底から導く。環境変数による上書きの規則は server と共通で
/// ある（[`settings_path`]）。
pub fn path() -> Result<PathBuf> {
    Ok(settings_path(&crate::registry::discovery_root()?))
}

/// 現在の設定。
///
/// 一度も読み込めていない場合は既定値を返す。
pub fn current() -> Arc<Settings> {
    #[cfg(test)]
    if let Some(settings) = test_override::current() {
        return settings;
    }
    state().settings()
}

/// 呼び出しスレッドに閉じた設定の差し替え。
///
/// 設定を読む側は差し替えを知らないまま現在値として受け取る。**差し替えを
/// スレッドに閉じるのは、同時に走る他の検査が本来の設定を読み続けるためである**
/// ——共有の現在値を書き換えると、無関係な検査が縮んだ期限で走る。
#[cfg(test)]
pub(crate) mod test_override {
    use super::Settings;
    use std::cell::RefCell;
    use std::sync::Arc;

    thread_local! {
        static OVERRIDE: RefCell<Option<Arc<Settings>>> = const { RefCell::new(None) };
    }

    /// 差し替えが有効な間だけ生きる印。
    ///
    /// 破棄で直前の差し替えへ戻す。入れ子にした差し替えが外側の値を消さない
    /// ようにするためであり、いずれの層も自分が置いた値だけを片付ける。
    pub(crate) struct Guard(Option<Arc<Settings>>);

    impl Drop for Guard {
        fn drop(&mut self) {
            let previous = self.0.take();
            OVERRIDE.with(|slot| *slot.borrow_mut() = previous);
        }
    }

    /// このスレッドの現在値を差し替える。
    ///
    /// **返り値を捨てると差し替えはその場で終わる。** 印が生きている間だけ
    /// 有効であることを、束縛の忘れが黙って通らない形で示す。
    #[must_use = "印を束縛しないと差し替えは即座に元へ戻る"]
    pub(crate) fn install(settings: Settings) -> Guard {
        let previous = OVERRIDE.with(|slot| slot.borrow_mut().replace(Arc::new(settings)));
        Guard(previous)
    }

    /// このスレッドの差し替え。
    pub(crate) fn current() -> Option<Arc<Settings>> {
        OVERRIDE.with(|slot| slot.borrow().clone())
    }
}

/// 起動時の読み込みで生じたこと。
///
/// **記録の準備が整う前に読むため、その場では流せない。** subscriber を立てて
/// から [`report_startup`] へ渡す。ログレベルが設定から決まる以上、読むのが先に
/// なるのは避けられない。
#[derive(Debug, Default)]
pub struct StartupReport {
    /// 丸めた項目・既定へ戻した項目。
    pub issues: Vec<SettingsIssue>,
    /// 読み込みそのものが失敗した理由。
    pub failure: Option<SettingsReadError>,
}

/// 起動時に設定を読み込む。
///
/// **失敗しても呼び出し元を止めない。** 設定が読めないことは、インスタンスを
/// 登録しない理由にならない。ただし**理由は捨てない**——起動時に壊れていた
/// ことを運用者が知る機会はここしか無い。
pub fn initialize() -> StartupReport {
    let path = match path() {
        Ok(path) => path,
        Err(_) => return StartupReport::default(),
    };
    let mut state = state();
    let reader = state
        .reader
        .get_or_insert_with(|| SettingsReader::new(path));
    match reader.refresh() {
        SettingsRefresh::Reloaded(issues) => StartupReport {
            issues,
            failure: None,
        },
        SettingsRefresh::Unchanged => StartupReport::default(),
        SettingsRefresh::Failed(e) => StartupReport {
            issues: Vec::new(),
            failure: Some(e),
        },
    }
}

/// 起動時の読み込みで生じたことを記録する。
///
/// subscriber を立ててから呼ぶ。
pub fn report_startup(report: &StartupReport) {
    if let Some(failure) = &report.failure {
        tracing::warn!("設定を読み込めませんでした。既定値で続行します: {failure}");
    }
    report_issues(&report.issues);
}

/// 要求 1 件の処理を始めるときに呼ぶ。
///
/// 内容が前回と同じなら何もしない。読み取りに失敗した場合は直前の設定を維持し、
/// 次の契機で再試行する。
pub fn refresh() {
    let mut state = state();
    let Some(reader) = state.reader.as_mut() else {
        return;
    };
    match reader.refresh() {
        SettingsRefresh::Unchanged => {}
        SettingsRefresh::Reloaded(issues) => {
            let settings = reader.settings();
            drop(state);
            report_issues(&issues);
            crate::apply_log_level(&settings);
        }
        SettingsRefresh::Failed(e) => {
            drop(state);
            tracing::warn!("設定を読み直せませんでした: {e}");
        }
    }
}

/// 変更点を設定ファイルへ書き、書いた内容をそのまま現在値へ反映する。
///
/// **保存した内容を読み直さない。** 書いた本人は内容を知っており、読み直すと
/// 別のプロセスの書き込みと競合したときに自分の変更が見えない窓ができる。
pub fn save(change: &SettingsChange) -> Result<()> {
    let path = path()?;
    let document = save_to(&path, change)?;
    let (issues, settings) = {
        let mut state = state();
        let reader = state
            .reader
            .get_or_insert_with(|| SettingsReader::new(path));
        let issues = reader.adopt(&document);
        (issues, reader.settings())
    };
    report_issues(&issues);
    crate::apply_log_level(&settings);
    Ok(())
}

/// 解決で生じた不整合を記録する。
///
/// **いずれも致命ではない。** 丸めた値・既定へ戻した値で動き続ける。
pub fn report_issues(issues: &[SettingsIssue]) {
    for issue in issues {
        tracing::warn!("設定を補正しました: {issue}");
    }
}

/// `path` の設定ファイルへ変更点を merge して書き込む。
///
/// 名前付き mutex を「最新ファイルの再読込 → merge → 原子的置換」の区間だけ
/// 保持する。異なる項目を変えた並行の保存は両方が残り、同じ項目を変えた場合
/// だけ後勝ちになる。**未知の項目と未知の tool 名は保持して書き戻す。**
pub(crate) fn save_to(path: &Path, change: &SettingsChange) -> Result<SettingsDocument> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("設定ファイルの置き場所を決められませんでした"))?;

    let _guard = SettingsMutex::acquire(SETTINGS_MUTEX_TIMEOUT)?;

    let mut document = match std::fs::read_to_string(path) {
        Ok(text) => match SettingsDocument::parse(&text) {
            Ok(document) => document,
            Err(e) => {
                // 壊れた内容は merge できない。**保持できないものを保持した
                // ふりをするより、書き直せる形へ戻すほうがよい**——設定画面が
                // 唯一の修復手段であるためである。
                tracing::warn!("設定ファイルを解析できないため作り直します: {e}");
                SettingsDocument::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SettingsDocument::default(),
        Err(e) => return Err(e).context("設定ファイルを読み取れませんでした"),
    };
    document.apply(change);

    let temp_path = parent.join(format!("settings.json.{}.tmp", uuid::Uuid::new_v4()));
    write_protected_atomic(&temp_path, path, document.to_json().as_bytes())
        .context("設定ファイルの書き込みに失敗しました")?;
    Ok(document)
}

/// 名前付き mutex の保持。
///
/// `Drop` で必ず解放する。保持したまま panic した場合、Windows は待機側へ
/// `WAIT_ABANDONED` を返す——その場合も所有権は移るため、後続の保存は止まらない。
struct SettingsMutex(MutexObject);

/// 名前付き mutex のハンドルそのもの。
///
/// **所有と獲得を分ける。** ハンドルは作った時点で閉じる責任が生じるが、
/// 所有権は獲得に成功して初めて生じる。1 つの型に畳むと、獲得に失敗した経路でも
/// `ReleaseMutex` を呼ぶ形になり、意図を読み違えやすい。
struct MutexObject(Owned<HANDLE>);

impl MutexObject {
    fn create() -> Result<Self> {
        let name: Vec<u16> = SETTINGS_MUTEX_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `name` は NUL 終端しており、呼び出しの間だけ参照される。
        // 返るハンドルは成功時のみ得られ、その所有権はこの値だけが持つ。
        let handle = unsafe {
            Owned::new(
                CreateMutexW(None, false, PCWSTR(name.as_ptr()))
                    .context("設定の名前付き mutex を作成できませんでした")?,
            )
        };
        Ok(Self(handle))
    }
}

impl SettingsMutex {
    /// 名前付き mutex を獲得する。
    ///
    /// 獲得できた場合にだけ所有を表す値を返す。放棄された mutex
    /// （`WAIT_ABANDONED`）も所有権は移るため、後続の保存は止まらない。
    fn acquire(timeout: Duration) -> Result<Self> {
        let object = MutexObject::create()?;
        let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: `object` は直前に作成した有効なハンドルを所有している。
        let wait = unsafe { WaitForSingleObject(*object.0, millis) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            Ok(Self(object))
        } else {
            Err(anyhow!("設定の名前付き mutex を獲得できませんでした"))
        }
    }
}

impl Drop for SettingsMutex {
    /// 獲得した所有権を返す。ハンドルは [`MutexObject`] が閉じる。
    fn drop(&mut self) {
        // SAFETY: 保持しているのは自スレッドが獲得した所有権である。
        unsafe {
            let _ = ReleaseMutex(*(self.0).0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::settings::{MIN_ARTIFACT_TTL_SECONDS, SETTINGS_SCHEMA_VERSION};
    use std::collections::BTreeMap;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aviutl2-mcp-settings-write-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn saving_creates_a_protected_file_with_the_schema_version() {
        let dir = temp_dir();
        let path = dir.join("settings.json");

        save_to(
            &path,
            &SettingsChange {
                log_level: Some("debug".to_string()),
                ..SettingsChange::default()
            },
        )
        .unwrap();

        aviutl2_mcp_win::test_support::assert_protected_dacl(&path);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(&format!("\"schema_version\": {SETTINGS_SCHEMA_VERSION}")));
        assert!(text.contains("debug"));
        // 一時ファイルを残さない。
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "一時ファイルが残っています");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_writer_keeps_the_first_writers_unrelated_change() {
        // 異なる項目を変えた並行の保存は両方が残る。
        let dir = temp_dir();
        let path = dir.join("settings.json");

        save_to(
            &path,
            &SettingsChange {
                log_level: Some("debug".to_string()),
                ..SettingsChange::default()
            },
        )
        .unwrap();
        let document = save_to(
            &path,
            &SettingsChange {
                artifact_ttl_seconds: Some(1200),
                ..SettingsChange::default()
            },
        )
        .unwrap();

        let (settings, _) = document.resolve(&Settings::default());
        assert_eq!(settings.log_level(), Some("debug"));
        assert_eq!(settings.artifact_ttl(), Duration::from_secs(1200));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_last_writer_wins_on_the_same_field() {
        let dir = temp_dir();
        let path = dir.join("settings.json");

        for level in ["debug", "trace"] {
            save_to(
                &path,
                &SettingsChange {
                    log_level: Some(level.to_string()),
                    ..SettingsChange::default()
                },
            )
            .unwrap();
        }

        let text = std::fs::read_to_string(&path).unwrap();
        let document = SettingsDocument::parse(&text).unwrap();
        let (settings, _) = document.resolve(&Settings::default());
        assert_eq!(settings.log_level(), Some("trace"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn another_processes_unknown_fields_and_tool_names_are_not_erased() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"schema_version":1,"future_field":42,"disabled_tools":["aviutl2_future_tool"]}"#,
        )
        .unwrap();

        save_to(
            &path,
            &SettingsChange {
                tools: BTreeMap::from([("delete_object".to_string(), false)]),
                ..SettingsChange::default()
            },
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("future_field"), "{text}");
        assert!(text.contains("aviutl2_future_tool"), "{text}");
        assert!(text.contains("delete_object"), "{text}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_over_a_corrupt_file_produces_a_readable_one() {
        let dir = temp_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{ broken").unwrap();

        save_to(
            &path,
            &SettingsChange {
                artifact_ttl_seconds: Some(1),
                ..SettingsChange::default()
            },
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let document = SettingsDocument::parse(&text).unwrap();
        let (settings, issues) = document.resolve(&Settings::default());
        assert_eq!(
            settings.artifact_ttl(),
            Duration::from_secs(MIN_ARTIFACT_TTL_SECONDS)
        );
        assert!(!issues.is_empty(), "丸めが記録されていません");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_named_mutex_is_released_when_the_guard_is_dropped() {
        // 解放しなければ 2 度目の獲得が期限まで待って失敗する。
        for _ in 0..3 {
            let guard = SettingsMutex::acquire(Duration::from_millis(200)).unwrap();
            drop(guard);
        }
    }
}
