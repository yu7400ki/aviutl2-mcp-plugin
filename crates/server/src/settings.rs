//! server 側の設定の監視と snapshot。
//!
//! # 監視するのは親ディレクトリであり、ファイルではない
//!
//! 設定ファイルは原子的置換で差し替わる。置換はファイルの identity を差し替える
//! ため、**ファイルを掴んだ監視は rename-replace を取りこぼす。** ディレクトリを
//! 監視すれば、作成・置換・削除のいずれも同じ経路で届く。
//!
//! # 新しい足回りを書き起こさない
//!
//! `ReadDirectoryChangesW` は overlapped I/O であり、`OVERLAPPED` の寿命と保留
//! I/O の排出を誤ると、カーネルが解放済みメモリへ書き込む。その規律は
//! [`crate::win_io`] が既に持っている——[`OverlappedOp`] は `OVERLAPPED` をヒープへ
//! 固定し、`Drop` で `CancelIoEx` と完了待ちを行う。停止も [`EventHandle`] を
//! [`wait_any`] へ並べるだけで済む。**この監視は既存の作法の上に乗るだけであり、
//! 新しい依存も新しい unsafe の型も持ち込まない。**
//!
//! # debounce を持たない
//!
//! 原子的置換は一時ファイルの作成と rename で複数の記録を生むが、再読込は冪等で
//! あり、更新時刻と大きさが同じなら再解析もしない。**余分に読み直すだけで、
//! 観測される値は常に最後の状態である。**
//!
//! # snapshot は 1 回の差し替えで反映する
//!
//! 読み手は [`SettingsSource::settings`] で現在の `Arc` を取り、その後の
//! 差し替えに影響されない。半分だけ適用された状態を観測する経路が無い。

use crate::win_io::{EventHandle, OverlappedOp, WaitAnyOutcome, WinIoError, wait_any};
use aviutl2_mcp_core::settings::{
    SETTINGS_FILE_NAME, SETTINGS_READ_ATTEMPTS, Settings, SettingsReadError, SettingsReader,
    SettingsRefresh,
};
use aviutl2_mcp_win::create_protected_directory;
use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tracing::{debug, error, warn};
use windows::Win32::Foundation::{CloseHandle, ERROR_NOTIFY_ENUM_DIR, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
};
use windows::core::PCWSTR;

/// 変更の記録を受け取るバッファの大きさ（バイト）。
///
/// 溢れても正しさは失われない。溢れた場合は記録を読まずに読み直す。
const NOTIFY_BUFFER_BYTES: usize = 4096;

/// `FILE_NOTIFY_INFORMATION` の名前より前の部分の大きさ（バイト）。
///
/// `NextEntryOffset` / `Action` / `FileNameLength` の 3 つの `u32` が並び、その
/// 直後から名前が UTF-16 で続く。**構造体の大きさを使わない**——末尾の
/// `FileName` は長さ 1 の配列として宣言されており、整列のための詰め物が入る。
const NOTIFY_HEADER_BYTES: usize = 3 * size_of::<u32>();

/// 監視する変更の種類。
///
/// 置換は名前の付け替えとして、直接の編集は最終書き込みと大きさとして届く。
/// 部分木は見ない。設定ファイルは親ディレクトリの直下にしか無い。
const NOTIFY_FILTER: windows::Win32::Storage::FileSystem::FILE_NOTIFY_CHANGE =
    windows::Win32::Storage::FileSystem::FILE_NOTIFY_CHANGE(
        FILE_NOTIFY_CHANGE_FILE_NAME.0
            | FILE_NOTIFY_CHANGE_LAST_WRITE.0
            | FILE_NOTIFY_CHANGE_SIZE.0,
    );

/// 一時的な読み取り失敗を試み直すまでの間隔。
///
/// 原子的置換が対象を差し替えている窓はミリ秒に満たない。**間隔を置かずに
/// 試行回数を使い切ると、再試行がその窓をまたがず、何度呼んでも同じ瞬間を
/// 見ることになる。**
const SETTINGS_READ_RETRY_INTERVAL: Duration = Duration::from_millis(20);

/// 既にシグナル状態の完了を取り出すときに与える猶予。
///
/// [`wait_any`] が完了を告げた後にしか使わないため、実際には待たない。有限に
/// するのは、万一シグナルが消えていても監視スレッドが止まらないためである。
const COMPLETION_GRACE: Duration = Duration::from_secs(5);

/// 設定ファイルの置き場所をどう扱うか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentPolicy {
    /// 基底の直下。無ければ保護 DACL 付きで用意する。
    Create,
    /// 外から指定された場所。**存在を要求し、既存の ACL を変えない。**
    Require,
}

/// 監視を始められなかった理由。
#[derive(Debug, thiserror::Error)]
pub enum SettingsWatchError {
    /// 設定ファイルの置き場所を決められない。
    #[error("設定ファイルの置き場所を決められませんでした")]
    NoParent,
    /// 指定された置き場所が存在しない。
    #[error("設定ファイルの置き場所が存在しません")]
    ParentMissing,
    /// 置き場所を用意できない、または監視を開始できない。
    #[error("設定ファイルの監視を開始できませんでした: {0}")]
    Io(#[from] io::Error),
}

/// 現在の設定を配る口。
///
/// 反映は 1 回の差し替えで行う。`tools/list` の判定も call-time の受付判定も
/// 同じ供給元を読むため、半適用の状態で食い違うことがない。
///
/// # 変化は押し出す
///
/// 監視スレッドは**値が変わったことを知っている**。それを待ち受ける口が無いと、
/// 下流は定期的に問い合わせるしかない——`ReadDirectoryChangesW` へ切り替えて
/// 消したはずのポーリングが、1 段下流で復活する。[`SettingsSource::subscribe`]
/// は最新の値だけを届ける。**連続した変更が畳まれることは、「有効集合が実際に
/// 変化した場合だけ知らせる」という要件とそのまま噛み合う。**
#[derive(Debug)]
pub struct SettingsSource {
    /// 現在の値と、変化の通知路を兼ねる。
    ///
    /// 値の保持と通知を 1 つにすることで、「差し替えたが知らせ忘れた」形が
    /// 作れなくなる。
    changed: watch::Sender<Arc<Settings>>,
}

impl SettingsSource {
    /// 固定の設定を配る口を作る。
    ///
    /// 監視を持たない構築口であり、試験と、設定を必要としない利用側が使う。
    pub fn fixed(settings: Settings) -> Arc<Self> {
        Arc::new(Self::new(Arc::new(settings)))
    }

    fn new(settings: Arc<Settings>) -> Self {
        Self {
            changed: watch::Sender::new(settings),
        }
    }

    /// 現在の設定。
    pub fn settings(&self) -> Arc<Settings> {
        Arc::clone(&self.changed.borrow())
    }

    /// 設定が変わったことを待ち受ける。
    ///
    /// **最新の値だけが届く。** 待っている間に何度変わっても、起床したときに
    /// 見えるのは最後の状態である。購読者が居なくても差し替えは進む。
    ///
    /// この口の供給元が drop されると、待ち受けは失敗を返して終わる。
    pub fn subscribe(&self) -> watch::Receiver<Arc<Settings>> {
        self.changed.subscribe()
    }

    /// 値が変わっていれば差し替え、購読者へ知らせる。変わったかどうかを返す。
    ///
    /// **同じ内容を読み直しても知らせない。** 原子的置換は一時ファイルの作成と
    /// rename で複数の記録を生むため、記録の数だけ知らせると変化していない
    /// 通知が並ぶ。
    ///
    /// 差し替えたかどうかは戻り値がそのまま運ぶ。**保持と通知が 1 つであるため、
    /// 「差し替えたが知らせ忘れた」形が書けない。**
    fn replace_if_changed(&self, settings: Arc<Settings>) -> bool {
        self.changed.send_if_modified(|current| {
            if **current == *settings {
                return false;
            }
            *current = settings;
            true
        })
    }
}

/// 設定ファイルの親ディレクトリを監視する 1 本のスレッド。
///
/// **plugin と違い server はスレッドを 1 本持つ。** 有効 tool の集合が変わった
/// ことを要求元へ知らせる経路は、要求が来るまで気付けない形では動かない。
/// server は AviUtl2 のプロセスの外にあり、スレッドを 1 本増やす費用は plugin と
/// 同じではない。
pub struct SettingsWatcher {
    source: Arc<SettingsSource>,
    stop: Arc<EventHandle>,
    handle: Option<JoinHandle<()>>,
}

impl SettingsWatcher {
    /// 初期 snapshot を作り終えた読み取り口を引き継いで監視を始める。
    ///
    /// 初期 snapshot は呼び出し元が作る。**MCP の受付を始める前に読み終えて
    /// いる**ことが要るためであり、その時点の記録の準備は呼び出し元しか
    /// 知らない。
    pub fn start(
        reader: SettingsReader,
        parent_policy: ParentPolicy,
    ) -> Result<Self, SettingsWatchError> {
        let parent = reader
            .path()
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or(SettingsWatchError::NoParent)?
            .to_path_buf();
        match parent_policy {
            ParentPolicy::Create => {
                create_protected_directory(&parent).map_err(|e| SettingsWatchError::Io(e.into()))?
            }
            ParentPolicy::Require => {
                if !parent.is_dir() {
                    return Err(SettingsWatchError::ParentMissing);
                }
            }
        }

        let directory = DirectoryHandle::open(&parent)?;
        let stop = Arc::new(EventHandle::new_manual_reset()?);
        let source = Arc::new(SettingsSource::new(reader.settings()));

        let handle = std::thread::Builder::new()
            .name("aviutl2-mcp-settings".to_string())
            .spawn({
                let source = Arc::clone(&source);
                let stop = Arc::clone(&stop);
                move || watch(directory, stop, reader, source)
            })?;

        Ok(Self {
            source,
            stop,
            handle: Some(handle),
        })
    }

    /// 現在の設定を配る口。
    pub fn source(&self) -> Arc<SettingsSource> {
        Arc::clone(&self.source)
    }
}

impl Drop for SettingsWatcher {
    /// 停止を合図し、監視スレッドの終了を待つ。
    ///
    /// **待ち合わせるのが要である。** 監視スレッドは `OverlappedOp` を保持して
    /// おり、その `Drop` が保留中の `ReadDirectoryChangesW` をキャンセルして
    /// 完了を確定させる。待たずに戻ると、カーネルが受信バッファを手放す前に
    /// スレッドの資源が解放され得る。
    fn drop(&mut self) {
        if let Err(e) = self.stop.set() {
            error!(error = %e, "設定監視の停止を合図できませんでした");
        }
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            error!("設定監視スレッドが panic で終了しました");
        }
    }
}

/// 監視対象のディレクトリハンドル。
struct DirectoryHandle(HANDLE);

// SAFETY: ファイルハンドルはスレッドを跨いで使用でき、`DirectoryHandle` は
// 所有権を単一の値に閉じている。閉じるのは `Drop` だけである。
unsafe impl Send for DirectoryHandle {}

impl DirectoryHandle {
    /// overlapped I/O で開く。
    ///
    /// 共有は読み・書き・削除のすべてを許す。**削除を許さないと、監視している
    /// 間そのディレクトリを消せなくなる**——上書きで指定された場所を使う試験が
    /// 後始末できない。
    fn open(path: &Path) -> io::Result<Self> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` は NUL 終端したパスであり、呼び出しの間だけ参照される。
        // ディレクトリを開くには backup semantics が要る。
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .map_err(|e| io::Error::other(format!("ディレクトリを開けませんでした: {e}")))?;
        Ok(Self(handle))
    }

    fn handle(&self) -> HANDLE {
        self.0
    }
}

impl Drop for DirectoryHandle {
    fn drop(&mut self) {
        // SAFETY: 本型のみが所有しており、ここでのみ閉じられる。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// 監視スレッドの本体。
///
/// `OverlappedOp` は `directory` より先に drop されなければならない。ここでは
/// 関数内の変数の宣言順がそれを与える（後に宣言したものから drop される）。
fn watch(
    directory: DirectoryHandle,
    stop: Arc<EventHandle>,
    mut reader: SettingsReader,
    source: Arc<SettingsSource>,
) {
    let settings_file = settings_file_name(reader.path());
    // 記録は `FILE_NOTIFY_INFORMATION` の並びであり DWORD 境界に整列している
    // 必要がある。`u32` の列として確保することで整列を型から得る。
    let mut buffer = vec![0u32; NOTIFY_BUFFER_BYTES / size_of::<u32>()];

    // SAFETY: `directory` は本関数の最後まで生存し、`op` はその前に drop される
    // （後に宣言したものから drop される）。
    let mut op = match unsafe { OverlappedOp::new(directory.handle()) } {
        Ok(op) => op,
        Err(e) => {
            error!(error = %e, "設定監視の overlapped I/O を用意できませんでした");
            return;
        }
    };

    let mut first_issue = true;
    loop {
        if let Err(e) = issue_watch(&mut op, &directory, &mut buffer) {
            error!(error = %e, "設定監視の変更通知を要求できませんでした");
            return;
        }

        // 監視を登録し終えた直後に 1 度だけ読み直す。**初期 snapshot を作って
        // から最初の要求を登録するまでの間に起きた変更は、通知として届かない。**
        // 更新時刻と大きさが同じなら再解析しないため、費用は `stat` 1 回である。
        if std::mem::take(&mut first_issue) {
            reload(&mut reader, &source);
        }

        match wait_any(&[op.event(), stop.handle()], None) {
            WaitAnyOutcome::Signaled(0) => {}
            // 停止。`op` の `Drop` が保留中の I/O をキャンセルして完了を
            // 確定させる。
            WaitAnyOutcome::Signaled(_) => return,
            WaitAnyOutcome::TimedOut => continue,
            WaitAnyOutcome::Failed(e) => {
                error!(error = %e, "設定監視の待機に失敗しました");
                return;
            }
        }

        let transferred = match op.await_completion(Instant::now() + COMPLETION_GRACE) {
            Ok(transferred) => Some(transferred),
            // 溢れは「何かが変わった」以上の情報を持たない。記録を読まずに
            // 読み直す。
            Err(WinIoError::Io(e)) if is_notify_overflow(&e) => None,
            Err(e) => {
                error!(error = %e, "設定監視の変更通知を取得できませんでした");
                return;
            }
        };

        if demands_reload(transferred, &buffer, &settings_file) {
            reload(&mut reader, &source);
        }
    }
}

/// 変更通知を 1 件要求する。
///
/// **発行の成功は「登録された」ことしか意味しない。** 完了として扱うと保留状態
/// が記録されず、`Drop` がキャンセルを飛ばして受信バッファを解放する経路が
/// できる——カーネルはその後もそこへ書き込む。`OverlappedOp::issue_queued` は
/// 分類の仕方を選ばせないため、この形でしか要求を出せない。
fn issue_watch(
    op: &mut OverlappedOp,
    directory: &DirectoryHandle,
    buffer: &mut [u32],
) -> io::Result<()> {
    let pointer = buffer.as_mut_ptr().cast::<c_void>();
    let length = size_of_val(buffer) as u32;
    // SAFETY: `buffer` は呼び出し元が `op` より長く生存させ、`op` の `Drop` が
    // I/O の完了を待ち合わせるため、カーネルの書き込み先は常に有効である。
    unsafe {
        op.issue_queued(|overlapped| {
            ReadDirectoryChangesW(
                directory.handle(),
                pointer,
                length,
                false,
                NOTIFY_FILTER,
                None,
                Some(overlapped),
                None,
            )
        })
    }
}

/// 記録の取得の失敗がバッファ溢れかどうか。
///
/// 溢れは「何かが変わった」以上の情報を持たない。記録を読まずに読み直す。
///
/// **溢れは 2 つの形で届き得る。** `STATUS_NOTIFY_ENUM_DIR` は severity が
/// SUCCESS であるため、`GetOverlappedResult` は成功したうえで転送 0 バイトを
/// 返すのが通常の形である。それを [`demands_reload`] が拾う。ここで拾うのは
/// 失敗として届いた場合であり、**どちらの経路でも読み直しへ倒れる。**
fn is_notify_overflow(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_NOTIFY_ENUM_DIR.0 as i32)
}

/// 受け取った記録が設定の読み直しを要求するか。
///
/// **判定できない場合は読み直す側へ倒す。** 取りこぼすより余分に読む方が安全で
/// ある。転送 0 バイトはバッファ溢れであり、記録は 1 件も入っていない。
fn demands_reload(transferred: Option<u32>, buffer: &[u32], settings_file: &[u16]) -> bool {
    let Some(transferred) = transferred else {
        return true;
    };
    if transferred == 0 {
        return true;
    }
    // SAFETY: `buffer` は `u32` の列として確保してあり、その全体をバイト列と
    // して読み直すだけである。参照の寿命は元の借用に縛られる。
    let raw =
        unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), size_of_val(buffer)) };
    let bytes = (transferred as usize).min(raw.len());

    let mut offset = 0usize;
    let mut seen = false;
    while offset + NOTIFY_HEADER_BYTES <= bytes {
        let next = read_u32(raw, offset) as usize;
        let name_bytes = read_u32(raw, offset + 2 * size_of::<u32>()) as usize;
        let name_offset = offset + NOTIFY_HEADER_BYTES;
        if name_offset + name_bytes > bytes || !name_bytes.is_multiple_of(size_of::<u16>()) {
            // 記録が途中で切れている。読み直す側へ倒す。
            return true;
        }
        let name: Vec<u16> = raw[name_offset..name_offset + name_bytes]
            .chunks_exact(size_of::<u16>())
            .map(|unit| u16::from_ne_bytes([unit[0], unit[1]]))
            .collect();
        seen = true;
        if same_file_name(&name, settings_file) {
            return true;
        }
        if next == 0 {
            break;
        }
        offset += next;
    }
    // 1 件も読み解けなければ読み直す。
    !seen
}

/// バイト列から native endian の `u32` を読む。
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_ne_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

/// 監視対象のファイル名を UTF-16 で取る。
fn settings_file_name(path: &Path) -> Vec<u16> {
    path.file_name()
        .map(|name| name.encode_wide().collect())
        .unwrap_or_else(|| SETTINGS_FILE_NAME.encode_utf16().collect())
}

/// ファイル名が一致するか。Windows のファイル名は大小を区別しない。
fn same_file_name(left: &[u16], right: &[u16]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(l, r)| to_lower_ascii(*l) == to_lower_ascii(*r))
}

fn to_lower_ascii(unit: u16) -> u16 {
    match u8::try_from(unit) {
        Ok(byte) => u16::from(byte.to_ascii_lowercase()),
        Err(_) => unit,
    }
}

/// 設定を読み直した結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReloadOutcome {
    /// 更新時刻と大きさが前回と同じで、読み直していない。
    Unchanged,
    /// 読み直し、snapshot を差し替えた。
    Applied,
    /// 読み直したが、解決した値は前と同じであった。
    Same,
    /// 解析できなかった。直前の snapshot を維持した。
    Corrupt,
    /// 読み取れなかった。直前の snapshot を維持した。
    Unreadable,
}

/// 設定を読み直し、値が変わっていれば snapshot を差し替える。
///
/// 更新時刻と大きさが前回と同じなら何もしない。**破損が、無効化していた tool を
/// 無言で再公開してはならない**ため、いずれの失敗でも直前の snapshot を維持する。
///
/// **失敗の 2 種類で扱いが違う。**
///
/// - **解析できない（破損）**: 再試行しない。内容は変わらないためである。加えて
///   読み取り口は読めた時点で印を進めるため、**再試行すると 2 回目は
///   [`SettingsRefresh::Unchanged`] になり、記録を残さないまま抜ける。**
///   運用者が設定の破損を知る機会はここしか無い。
/// - **読み取れない**: 原子的置換の最中に掴んだ場合であり、窓はごく短い。
///   間隔を置いて有限回だけ試す。**間隔を置かずに数マイクロ秒で使い切ると、
///   再試行が窓をまたがない。**
fn reload(reader: &mut SettingsReader, source: &SettingsSource) -> ReloadOutcome {
    for attempt in 1..=SETTINGS_READ_ATTEMPTS {
        match reader.refresh() {
            SettingsRefresh::Unchanged => return ReloadOutcome::Unchanged,
            SettingsRefresh::Reloaded(issues) => {
                for issue in &issues {
                    warn!("設定を補正しました: {issue}");
                }
                let settings = reader.settings();
                return if source.replace_if_changed(Arc::clone(&settings)) {
                    // 記録の層まで届けなければ、`log_level` だけが「保存しても
                    // 効かない」項目になる。
                    crate::apply_log_level(&settings);
                    debug!("設定を反映しました");
                    ReloadOutcome::Applied
                } else {
                    ReloadOutcome::Same
                };
            }
            SettingsRefresh::Failed(e @ SettingsReadError::Parse(_)) => {
                warn!("設定を解析できませんでした。直前の設定を維持します: {e}");
                return ReloadOutcome::Corrupt;
            }
            SettingsRefresh::Failed(e) => {
                if attempt == SETTINGS_READ_ATTEMPTS {
                    warn!("設定を読み取れませんでした。直前の設定を維持します: {e}");
                } else {
                    std::thread::sleep(SETTINGS_READ_RETRY_INTERVAL);
                }
            }
        }
    }
    ReloadOutcome::Unreadable
}

#[cfg(test)]
mod tests;
