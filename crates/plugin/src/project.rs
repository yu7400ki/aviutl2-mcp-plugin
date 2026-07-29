//! プロジェクト境界（epoch / identity）と変更 revision の管理。
//!
//! ここで保持する値は、ホストのイベントスレッドと要求処理スレッドの双方から
//! 触られる。イベントスレッドはホストのグローバル write lock を保持したまま
//! 呼ばれるため、更新経路は待たされてはならない。そのため `revision`・
//! `modified`・変更の記録は、いずれもロックを取らない atomic 操作だけで完結させる。
//!
//! これらの atomic は値そのものだけが意味を持ち、他のデータを公開しないため
//! [`Ordering::Relaxed`] で扱う。epoch と revision のように複数の値へ跨る一貫性は
//! atomic では得られず、まとめて読み取る手段も用意しない。対象の同一性は
//! epoch と revision だけでは判断せず、scene_id と fingerprint の照合で確かめる。
//!
//! epoch と identity はプロジェクト境界でしか変わらないため [`Mutex`] で保護する。
//! 保持区間は文字列の複製と差し替えに限られ、SDK 呼び出し・ファイル I/O・
//! ログ出力を挟まない。アクセサ経由の読み取りも値を複製して即座に抜けるため、
//! どちらのスレッドが先に取得しても待たされる時間は一定であり、保持したまま
//! 他のロックを要求する経路も無い。
//!
//! 変更の集約（[`ChangeNotifier`]）は、変更通知を要求元へ送り出す際の入口となる
//! ことを見込んでいる。plugin から要求元へ変更を押し出す経路は現行の IPC 契約に
//! 無く、本モジュールが受け持つのは変更を溜めて頻度を抑えた形で取り出せるように
//! するところまでである。取り出しの呼び出し元が現時点ではテストしか無いのは
//! そのためで、[`ProjectState::take_pending_changes`] は送出側が実装された時点で
//! そのまま入口になる。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// 同一インスタンスから変更を取り出す最小間隔。10 Hz に相当する。
const NOTIFY_MIN_INTERVAL: Duration = Duration::from_millis(100);

/// 変更を一度も取り出していないことを表す番兵値。
const NEVER_TAKEN: u64 = u64::MAX;

/// プロジェクトの同一性。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectIdentity {
    /// 保存済みプロジェクト。ファイルパスと境界の世代で識別する。
    Path {
        /// プロジェクトファイルのパス。
        path: String,
        /// 境界の更新（ロード・保存）ごとに進む世代番号。
        generation: u64,
    },
    /// 未保存プロジェクト。plugin 内で生成したランダム ID で識別する。
    Unsaved {
        /// plugin 内でのみ意味を持つ識別子。
        id: String,
    },
}

/// 変更の種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChangeKind {
    /// プロジェクト境界が変わった。
    ProjectEpoch,
    /// プロジェクトの内容が変わった。
    ProjectRevision,
    /// 編集対象のシーンが変わった。
    CurrentScene,
}

impl ChangeKind {
    /// 集合表現でのビット位置。
    const fn bit(self) -> u32 {
        match self {
            ChangeKind::ProjectEpoch => 1 << 0,
            ChangeKind::ProjectRevision => 1 << 1,
            ChangeKind::CurrentScene => 1 << 2,
        }
    }
}

/// 取り出した変更種別の集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingChanges {
    bits: u32,
}

impl PendingChanges {
    /// 指定した種別の変更を含むか。
    // 現時点の呼び出し元はテストのみ。
    #[allow(dead_code)]
    pub(crate) fn contains(self, kind: ChangeKind) -> bool {
        self.bits & kind.bit() != 0
    }
}

/// 直近の取り出しから `min_interval` 以上経過しているかを判定する。
///
/// `last_taken` が `None` の場合は一度も取り出していないため常に許可する。
/// 単調時計であっても比較の向きを取り違えないよう、経過時間は飽和減算で求める。
fn admits_notification(last_taken: Option<Instant>, now: Instant, min_interval: Duration) -> bool {
    match last_taken {
        None => true,
        Some(last) => now.saturating_duration_since(last) >= min_interval,
    }
}

/// 変更の集約。
///
/// 変更種別ごとの未取り出しフラグと、直近に取り出した時刻だけを持つ。
/// 記録は無制限に行い、取り出しを [`NOTIFY_MIN_INTERVAL`] で制限する。
/// 制限中に生じた変更はフラグとして残るため、次の取り出しでまとめて観測できる。
/// 取り出しが遅れて個々の変更を観測できなくても、revision の照合で変更の
/// 見落としは検出できる。
struct ChangeNotifier {
    pending: AtomicU32,
    /// 直近に変更を取り出した時刻。`origin` からの経過ナノ秒で保持する。
    last_taken_nanos: AtomicU64,
    /// 経過ナノ秒の基準時刻。
    origin: Instant,
}

impl ChangeNotifier {
    fn new() -> Self {
        Self {
            pending: AtomicU32::new(0),
            last_taken_nanos: AtomicU64::new(NEVER_TAKEN),
            origin: Instant::now(),
        }
    }

    /// 変更を記録する。イベントスレッドから呼ばれるため待ち時間を持たない。
    fn record(&self, kind: ChangeKind) {
        self.pending.fetch_or(kind.bit(), Ordering::Relaxed);
    }

    /// 最小間隔を満たしていれば、未取り出しの変更を取り出してクリアする。
    ///
    /// 未取り出しの変更が無い場合は間隔を消費せずに `None` を返す。
    fn take(&self, now: Instant) -> Option<PendingChanges> {
        if self.pending.load(Ordering::Relaxed) == 0 {
            return None;
        }
        if !self.admit(now) {
            return None;
        }
        let bits = self.pending.swap(0, Ordering::Relaxed);
        (bits != 0).then_some(PendingChanges { bits })
    }

    /// 最小間隔を満たす場合にのみ取り出し時刻を更新する。
    ///
    /// 複数スレッドが同時に取り出そうとしても、compare-exchange に成功した
    /// 一つだけが許可される。
    fn admit(&self, now: Instant) -> bool {
        let now_nanos = self.nanos_since_origin(now);
        let mut stored = self.last_taken_nanos.load(Ordering::Relaxed);
        loop {
            let last = (stored != NEVER_TAKEN).then(|| self.origin + Duration::from_nanos(stored));
            if !admits_notification(last, now, NOTIFY_MIN_INTERVAL) {
                return false;
            }
            match self.last_taken_nanos.compare_exchange_weak(
                stored,
                now_nanos,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => stored = actual,
            }
        }
    }

    /// 基準時刻からの経過ナノ秒。番兵値と衝突しないよう丸める。
    fn nanos_since_origin(&self, now: Instant) -> u64 {
        let elapsed = now.saturating_duration_since(self.origin).as_nanos();
        u64::try_from(elapsed).unwrap_or(NEVER_TAKEN - 1)
    }
}

/// epoch と identity の組。プロジェクト境界を表す。
struct Boundary {
    epoch: String,
    identity: ProjectIdentity,
    /// 境界を更新した回数。保存済み identity の世代番号に用いる。
    generation: u64,
}

/// 新しい epoch を生成する。
fn new_epoch() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 未保存プロジェクトの識別子を生成する。
fn new_unsaved_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 現在の identity と確定したパスから、次の identity を求める。
///
/// パスを確定できない場合、既に未保存として識別していればその ID を保つ。
/// 保存済みから未保存へ移る場合は別のプロジェクトとして新しい ID を発行する。
fn next_identity(
    current: &ProjectIdentity,
    path: Option<&str>,
    generation: u64,
) -> ProjectIdentity {
    match path {
        Some(path) => ProjectIdentity::Path {
            path: path.to_string(),
            generation,
        },
        None => match current {
            ProjectIdentity::Unsaved { id } => ProjectIdentity::Unsaved { id: id.clone() },
            ProjectIdentity::Path { .. } => ProjectIdentity::Unsaved {
                id: new_unsaved_id(),
            },
        },
    }
}

/// read 応答が参照するプロジェクトの状態。
///
/// ライフサイクル（インスタンスの登録状態）とは独立しており、descriptor への
/// 書き込みを伴わない。イベントスレッドから更新できるよう、全ての更新は
/// ロックを取らないか、境界更新時の短い Mutex 保持だけで完結する。
pub struct ProjectState {
    boundary: Mutex<Boundary>,
    revision: AtomicU64,
    modified: AtomicBool,
    notifier: ChangeNotifier,
}

impl Default for ProjectState {
    fn default() -> Self {
        Self::new()
    }
}

impl ProjectState {
    /// plugin 登録時の初期状態を作る。
    ///
    /// epoch を新規に発行し、identity は未保存プロジェクトとして始める。
    pub fn new() -> Self {
        Self {
            boundary: Mutex::new(Boundary {
                epoch: new_epoch(),
                identity: ProjectIdentity::Unsaved {
                    id: new_unsaved_id(),
                },
                generation: 0,
            }),
            revision: AtomicU64::new(0),
            modified: AtomicBool::new(false),
            notifier: ChangeNotifier::new(),
        }
    }

    /// プロジェクトのロードを反映する。
    ///
    /// プロジェクトが切り替わる境界であるため、新しい epoch を発行し
    /// revision を 0 へ戻す。
    pub fn on_project_load(&self, path: Option<&str>) {
        self.update_boundary(path, true);
        self.revision.store(0, Ordering::Relaxed);
        self.modified.store(false, Ordering::Relaxed);
        self.notifier.record(ChangeKind::ProjectEpoch);
    }

    /// プロジェクトの保存を反映する。
    ///
    /// 同一プロジェクトの保存であるため epoch と revision は維持し、
    /// パスの確定と未保存状態の解消だけを反映する。
    pub fn on_project_save(&self, path: Option<&str>) {
        self.update_boundary(path, false);
        self.modified.store(false, Ordering::Relaxed);
    }

    /// 対象更新イベントを反映する。
    pub fn on_object_updated(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
        self.modified.store(true, Ordering::Relaxed);
        self.notifier.record(ChangeKind::ProjectRevision);
    }

    /// plugin が SDK の変更 API を発行したことを反映する。加算後の値を返す。
    ///
    /// 引き金は「要求全体の成功」ではなく「変更 API を 1 回でも発行したこと」で
    /// ある。逆にすると、変更は入ったのに revision が据え置かれ、同じ前提での
    /// 再送が前提条件を通ってしまい二重に適用される。未保存の変更が無いという
    /// 誤った主張が残るのも、それを信じて閉じれば変更が失われるため許容できない。
    ///
    /// 加算後の値を返すのは、応答へ載せる値を確定させるためである。加算した後に
    /// 改めて読み直すと、その間にイベントスレッドが対象更新を配送した場合に別の
    /// 値を読み、返す値が非決定になる。
    pub(crate) fn on_edit_issued(&self) -> u64 {
        let next = self.revision.fetch_add(1, Ordering::Relaxed) + 1;
        self.modified.store(true, Ordering::Relaxed);
        self.notifier.record(ChangeKind::ProjectRevision);
        next
    }

    /// シーン変更イベントを反映する。
    ///
    /// このイベントはシーンの切り替えとシーン情報の更新の双方で発生し、
    /// イベントスレッドからは両者を区別できない。切り替えのたびに epoch を
    /// 更新すると参照の無効化が頻発するため、epoch は据え置いて revision の
    /// 増加と変更の記録を行う。
    ///
    /// `modified` は立てる。シーン情報の更新は未保存の変更そのものであり、
    /// 切り替えと区別できない以上どちらかに倒す必要がある。`modified` が偽で
    /// あることは「未保存の変更が無い」という積極的な主張であり、それを信じて
    /// 閉じたり上書きしたりすれば変更が失われる。過大に報告して余分な保存を
    /// 促す方が、取りこぼして失わせるより害が小さい。
    pub fn on_scene_changed(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
        self.modified.store(true, Ordering::Relaxed);
        self.notifier.record(ChangeKind::CurrentScene);
    }

    /// 境界を更新する。`renew_epoch` が真なら epoch も再発行する。
    fn update_boundary(&self, path: Option<&str>, renew_epoch: bool) {
        let mut boundary = self.lock_boundary();
        boundary.generation += 1;
        boundary.identity = next_identity(&boundary.identity, path, boundary.generation);
        if renew_epoch {
            boundary.epoch = new_epoch();
        }
    }

    /// 境界のガードを取得する。毒された場合も状態は一貫しているため継続する。
    fn lock_boundary(&self) -> std::sync::MutexGuard<'_, Boundary> {
        self.boundary.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 現在の epoch。
    ///
    /// 更新されるのはプロジェクトのロードだけである。プロジェクトを開いたまま
    /// 新規作成した場合はロードハンドラが呼ばれず、境界を検出できないため
    /// epoch は据え置かれる。epoch の一致は対象が同一であることの十分条件では
    /// ないので、同一性は scene_id と fingerprint の照合で確かめる。
    pub(crate) fn epoch(&self) -> String {
        self.lock_boundary().epoch.clone()
    }

    /// 現在の identity。
    // 現時点の呼び出し元はテストのみ。
    #[allow(dead_code)]
    pub(crate) fn identity(&self) -> ProjectIdentity {
        self.lock_boundary().identity.clone()
    }

    /// 保存済みプロジェクトのパス。未保存なら `None`。
    // 現時点の呼び出し元はテストのみ。
    #[allow(dead_code)]
    pub(crate) fn identity_path(&self) -> Option<String> {
        match &self.lock_boundary().identity {
            ProjectIdentity::Path { path, .. } => Some(path.clone()),
            ProjectIdentity::Unsaved { .. } => None,
        }
    }

    /// 現在の revision。
    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    /// 最後の load/save 以降に未保存の変更が生じたか。
    ///
    /// 対象の更新イベントに加え、シーン変更イベントでも真になる。シーン情報の
    /// 更新と切り替えはイベントスレッドから区別できないため、未保存の変更を
    /// 取りこぼさない側へ倒している。
    pub(crate) fn modified(&self) -> bool {
        self.modified.load(Ordering::Relaxed)
    }

    /// 未取り出しの変更を取り出してクリアする。
    ///
    /// 直近の取り出しから [`NOTIFY_MIN_INTERVAL`] が経過していない場合と、
    /// 未取り出しの変更が無い場合は `None` を返す。抑止された変更は次の
    /// 取り出しでまとめて観測できる。
    // 現時点の呼び出し元はテストのみ。
    #[allow(dead_code)]
    pub(crate) fn take_pending_changes(&self, now: Instant) -> Option<PendingChanges> {
        self.notifier.take(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_starts_with_epoch_and_zero_revision() {
        let state = ProjectState::new();

        assert!(!state.epoch().is_empty());
        assert_eq!(state.revision(), 0);
        assert!(!state.modified());
        assert!(matches!(state.identity(), ProjectIdentity::Unsaved { .. }));
    }

    #[test]
    fn separate_states_have_different_epochs() {
        assert_ne!(ProjectState::new().epoch(), ProjectState::new().epoch());
    }

    #[test]
    fn project_load_renews_epoch_and_resets_revision() {
        let state = ProjectState::new();
        let before = state.epoch();
        state.on_object_updated();
        assert_eq!(state.revision(), 1);

        state.on_project_load(Some(r"C:\projects\sample.aup2"));

        assert_ne!(state.epoch(), before);
        assert_eq!(state.revision(), 0);
        assert!(!state.modified());
        assert_eq!(
            state.identity_path().as_deref(),
            Some(r"C:\projects\sample.aup2")
        );
    }

    #[test]
    fn project_save_keeps_epoch_and_clears_modified() {
        let state = ProjectState::new();
        state.on_project_load(Some(r"C:\projects\sample.aup2"));
        let epoch = state.epoch();
        state.on_object_updated();
        assert!(state.modified());
        let revision = state.revision();

        state.on_project_save(Some(r"C:\projects\sample.aup2"));

        assert_eq!(state.epoch(), epoch);
        assert_eq!(state.revision(), revision);
        assert!(!state.modified());
    }

    #[test]
    fn object_update_increments_revision_and_marks_modified() {
        let state = ProjectState::new();

        state.on_object_updated();
        state.on_object_updated();

        assert_eq!(state.revision(), 2);
        assert!(state.modified());
    }

    #[test]
    fn scene_change_increments_revision() {
        let state = ProjectState::new();
        state.on_project_load(Some(r"C:\projects\sample.aup2"));

        state.on_scene_changed();

        assert_eq!(state.revision(), 1);
    }

    #[test]
    fn scene_change_keeps_epoch() {
        let state = ProjectState::new();
        let before = state.epoch();

        state.on_scene_changed();
        state.on_scene_changed();

        assert_eq!(state.epoch(), before, "シーン変更で epoch が更新されました");
        assert_eq!(state.revision(), 2);
    }

    #[test]
    fn scene_change_keeps_epoch_after_project_load() {
        let state = ProjectState::new();
        state.on_project_load(Some(r"C:\projects\sample.aup2"));
        let epoch = state.epoch();

        state.on_scene_changed();

        assert_eq!(state.epoch(), epoch);
    }

    #[test]
    fn scene_change_marks_modified() {
        let state = ProjectState::new();
        state.on_project_load(Some(r"C:\projects\sample.aup2"));
        assert!(!state.modified());

        // シーン情報の更新は切り替えと区別できないため、未保存の変更を
        // 取りこぼさない側へ倒す。
        state.on_scene_changed();

        assert!(state.modified());
    }

    #[test]
    fn unsaved_project_keeps_identity_across_saves() {
        let state = ProjectState::new();
        state.on_project_load(None);
        let identity = state.identity();

        state.on_project_save(None);

        assert_eq!(state.identity(), identity);
        assert_eq!(state.identity_path(), None);
    }

    #[test]
    fn boundary_updates_advance_path_generation() {
        let state = ProjectState::new();
        state.on_project_load(None);
        assert!(matches!(state.identity(), ProjectIdentity::Unsaved { .. }));

        state.on_project_save(Some(r"C:\projects\sample.aup2"));
        let ProjectIdentity::Path {
            path: saved_path,
            generation: saved,
        } = state.identity()
        else {
            panic!("保存後も未保存の identity のままです");
        };
        assert_eq!(saved_path, r"C:\projects\sample.aup2");

        state.on_project_load(Some(r"C:\projects\sample.aup2"));
        let ProjectIdentity::Path {
            generation: reloaded,
            ..
        } = state.identity()
        else {
            panic!("ロード後の identity が保存済みではありません");
        };

        assert!(
            reloaded > saved,
            "再ロードで世代が進みませんでした: {saved} → {reloaded}"
        );
    }

    #[test]
    fn next_identity_issues_new_id_when_path_is_lost() {
        let saved = ProjectIdentity::Path {
            path: r"C:\projects\sample.aup2".to_string(),
            generation: 1,
        };

        let ProjectIdentity::Unsaved { id } = next_identity(&saved, None, 2) else {
            panic!("パスを失った identity が保存済みのままです");
        };
        assert!(!id.is_empty());
    }

    #[test]
    fn admits_notification_requires_minimum_interval() {
        let origin = Instant::now();

        assert!(admits_notification(None, origin, NOTIFY_MIN_INTERVAL));
        assert!(!admits_notification(
            Some(origin),
            origin + Duration::from_millis(99),
            NOTIFY_MIN_INTERVAL
        ));
        assert!(admits_notification(
            Some(origin),
            origin + Duration::from_millis(100),
            NOTIFY_MIN_INTERVAL
        ));
    }

    #[test]
    fn changes_are_not_taken_more_than_ten_times_per_second() {
        let state = ProjectState::new();
        let origin = Instant::now();

        state.on_object_updated();
        assert!(
            state.take_pending_changes(origin).is_some(),
            "最初の取り出しが抑止されました"
        );

        state.on_scene_changed();
        assert!(
            state
                .take_pending_changes(origin + Duration::from_millis(99))
                .is_none(),
            "100ms 未満の間隔で取り出せてしまいました"
        );
    }

    #[test]
    fn suppressed_changes_are_taken_after_minimum_interval() {
        let state = ProjectState::new();
        let origin = Instant::now();

        state.on_object_updated();
        state.take_pending_changes(origin).unwrap();

        // 抑止される間の変更は取り出し可能になった時点でまとめて観測できる。
        state.on_scene_changed();
        assert!(
            state
                .take_pending_changes(origin + Duration::from_millis(50))
                .is_none()
        );
        state.on_project_load(None);

        let taken = state
            .take_pending_changes(origin + Duration::from_millis(100))
            .expect("最小間隔の経過後も取り出せませんでした");
        assert!(taken.contains(ChangeKind::CurrentScene));
        assert!(taken.contains(ChangeKind::ProjectEpoch));
        assert!(!taken.contains(ChangeKind::ProjectRevision));
    }

    #[test]
    fn taking_changes_clears_them() {
        let state = ProjectState::new();
        let origin = Instant::now();
        state.on_object_updated();

        let taken = state.take_pending_changes(origin).unwrap();
        assert!(taken.contains(ChangeKind::ProjectRevision));
        assert!(
            state
                .take_pending_changes(origin + Duration::from_millis(100))
                .is_none(),
            "取り出し済みの変更が再度取り出されました"
        );
    }

    #[test]
    fn taking_without_changes_does_not_consume_the_interval() {
        let state = ProjectState::new();
        let origin = Instant::now();

        assert!(state.take_pending_changes(origin).is_none());

        state.on_object_updated();
        assert!(
            state
                .take_pending_changes(origin + Duration::from_millis(1))
                .is_some(),
            "変更が無い取り出しが間隔を消費しました"
        );
    }
}
