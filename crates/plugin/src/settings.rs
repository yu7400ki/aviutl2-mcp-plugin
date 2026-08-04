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
//! 要求 1 件あたりの費用は `stat` 1 回である。更新時刻と大きさが同じなら再解析
//! しない。同一秒内に置換され大きさも同じ変更は取りこぼすが、次の変更で追い
//! つくため保守側に倒れている。**「必ず即座に反映する」を約束しない。**
//!
//! # 読めなくても止まらない
//!
//! 設定が読めないことは、インスタンスを登録しない理由にならない。保護 DACL の
//! 検証失敗が登録を止めるのとは対照的である——**守れないなら止め、決められない
//! なら既定で動く。**

use crate::atomic_file::write_protected_atomic;
use anyhow::{Context, Result, anyhow};
use aviutl2_mcp_core::settings::{
    Settings, SettingsChange, SettingsDocument, SettingsIssue, SettingsReader, SettingsRefresh,
    settings_path,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::PCWSTR;

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
    state().settings()
}

/// 起動時に設定を読み込む。
///
/// **失敗しても呼び出し元を止めない。** 生じた不整合を返すので、記録の準備が
/// できてから [`report_issues`] へ渡す。
pub fn initialize() -> Vec<SettingsIssue> {
    let path = match path() {
        Ok(path) => path,
        Err(_) => return Vec::new(),
    };
    let mut state = state();
    let reader = state
        .reader
        .get_or_insert_with(|| SettingsReader::new(path));
    match reader.refresh() {
        SettingsRefresh::Reloaded(issues) => issues,
        SettingsRefresh::Unchanged => Vec::new(),
        SettingsRefresh::Failed(_) => Vec::new(),
    }
}

/// 要求 1 件の処理を始めるときに呼ぶ。
///
/// 更新時刻と大きさが前回と同じなら何もしない。読み取りに失敗した場合は直前の
/// 設定を維持し、次の契機で再試行する。
pub fn refresh() {
    let mut state = state();
    let Some(reader) = state.reader.as_mut() else {
        return;
    };
    match reader.refresh() {
        SettingsRefresh::Unchanged => {}
        SettingsRefresh::Reloaded(issues) => {
            drop(state);
            report_issues(&issues);
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
    let issues = {
        let mut state = state();
        let reader = state
            .reader
            .get_or_insert_with(|| SettingsReader::new(path));
        reader.adopt(&document)
    };
    report_issues(&issues);
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
struct SettingsMutex(HANDLE);

impl SettingsMutex {
    fn acquire(timeout: Duration) -> Result<Self> {
        let name: Vec<u16> = SETTINGS_MUTEX_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `name` は NUL 終端しており、呼び出しの間だけ参照される。
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
            .context("設定の名前付き mutex を作成できませんでした")?;
        let guard = Self(handle);

        let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: `handle` は直前に作成した有効なハンドルである。
        let wait = unsafe { WaitForSingleObject(handle, millis) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            Ok(guard)
        } else {
            Err(anyhow!("設定の名前付き mutex を獲得できませんでした"))
        }
    }
}

impl Drop for SettingsMutex {
    fn drop(&mut self) {
        // SAFETY: 保持しているのは自スレッドが獲得したハンドルである。
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
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
        assert_eq!(settings.log_level(), "debug");
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
        assert_eq!(settings.log_level(), "trace");

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
                tools: BTreeMap::from([("aviutl2_delete_object".to_string(), false)]),
                ..SettingsChange::default()
            },
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("future_field"), "{text}");
        assert!(text.contains("aviutl2_future_tool"), "{text}");
        assert!(text.contains("aviutl2_delete_object"), "{text}");

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
