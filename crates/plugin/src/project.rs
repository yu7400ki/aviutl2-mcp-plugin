//! プロジェクト境界（epoch / identity）と変更 revision の管理。
//!
//! ここで保持する値は、ホストのイベントスレッドとインスタンスの要求処理
//! スレッドの双方から触られる。イベントスレッドはホストのグローバル write lock
//! を保持したまま呼ばれるため、更新経路は待たされてはならない。そのため
//! `revision` と `modified` の更新はロックを取らない atomic 操作だけで完結させる。
//!
//! epoch と identity はプロジェクト境界でしか変わらないため [`Mutex`] で保護する。
//! この Mutex を保持する区間は文字列の複製と差し替えに限られ、SDK 呼び出し・
//! ファイル I/O・ログ出力を挟まない。要求処理側も epoch/identity を読み取る
//! 短い区間でしか取得しないため、どちらのスレッドが先に取得しても相手を
//! 待たせる時間は一定であり、保持したまま他のロックを要求する経路も無い。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// プロジェクトの同一性。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectIdentity {
    /// 保存済みプロジェクト。ファイルパスとロード世代で識別する。
    Path {
        /// プロジェクトファイルのパス。
        path: String,
        /// 同じパスの再ロードを区別するための世代番号。
        generation: u64,
    },
    /// 未保存プロジェクト。plugin 内で生成したランダム ID で識別する。
    Unsaved {
        /// plugin 内でのみ意味を持つ識別子。
        id: String,
    },
}

/// epoch と identity の組。プロジェクト境界を表す。
struct Boundary {
    epoch: String,
    identity: ProjectIdentity,
    /// identity を更新した回数。保存済み identity の世代番号に用いる。
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
    /// identity をライフサイクルフックで確定できているか。
    ///
    /// 確定していない間は、シーン変更がプロジェクト切り替えを伴う可能性を
    /// 否定できないため、epoch を保守的に更新する判断材料にする。
    identity_confirmed: AtomicBool,
    revision: AtomicU64,
    modified: AtomicBool,
}

impl Default for ProjectState {
    fn default() -> Self {
        Self::new()
    }
}

impl ProjectState {
    /// plugin 登録時の初期状態を作る。
    ///
    /// epoch を新規に発行し、identity は未確定の未保存プロジェクトとする。
    pub fn new() -> Self {
        Self {
            boundary: Mutex::new(Boundary {
                epoch: new_epoch(),
                identity: ProjectIdentity::Unsaved {
                    id: new_unsaved_id(),
                },
                generation: 0,
            }),
            identity_confirmed: AtomicBool::new(false),
            revision: AtomicU64::new(0),
            modified: AtomicBool::new(false),
        }
    }

    /// 現在の epoch。
    pub fn epoch(&self) -> String {
        self.lock_boundary().epoch.clone()
    }

    /// 現在の identity。
    pub fn identity(&self) -> ProjectIdentity {
        self.lock_boundary().identity.clone()
    }

    /// 保存済みプロジェクトのパス。未保存なら `None`。
    pub fn identity_path(&self) -> Option<String> {
        match &self.lock_boundary().identity {
            ProjectIdentity::Path { path, .. } => Some(path.clone()),
            ProjectIdentity::Unsaved { .. } => None,
        }
    }

    /// 現在の revision。
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// 最後の load/save 以降に更新イベントを受け取ったか。
    pub fn modified(&self) -> bool {
        self.modified.load(Ordering::Acquire)
    }

    /// プロジェクトのロードを反映する。
    ///
    /// プロジェクトが切り替わる境界であるため、新しい epoch を発行し
    /// revision を 0 へ戻す。
    pub fn on_project_load(&self, path: Option<&str>) {
        self.update_boundary(path, true);
        self.revision.store(0, Ordering::Release);
        self.modified.store(false, Ordering::Release);
    }

    /// プロジェクトの保存を反映する。
    ///
    /// 同一プロジェクトの保存であるため epoch と revision は維持し、
    /// パスの確定と未保存状態の解消だけを反映する。
    pub fn on_project_save(&self, path: Option<&str>) {
        self.update_boundary(path, false);
        self.modified.store(false, Ordering::Release);
    }

    /// 対象更新イベントを反映する。
    pub fn on_object_updated(&self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
        self.modified.store(true, Ordering::Release);
    }

    /// シーン変更イベントを反映する。
    ///
    /// identity を確定できていない間はプロジェクトの切り替えと区別できないため、
    /// epoch を保守的に更新して既存の参照を無効化する。
    pub fn on_scene_changed(&self) {
        self.revision.fetch_add(1, Ordering::AcqRel);

        if !self.identity_confirmed.load(Ordering::Acquire) {
            self.lock_boundary().epoch = new_epoch();
        }
    }

    /// 境界を更新する。`renew_epoch` が真なら epoch も再発行する。
    fn update_boundary(&self, path: Option<&str>, renew_epoch: bool) {
        {
            let mut boundary = self.lock_boundary();
            boundary.generation += 1;
            boundary.identity = next_identity(&boundary.identity, path, boundary.generation);
            if renew_epoch {
                boundary.epoch = new_epoch();
            }
        }
        self.identity_confirmed.store(true, Ordering::Release);
    }

    /// 境界のガードを取得する。毒された場合も状態は一貫しているため継続する。
    fn lock_boundary(&self) -> std::sync::MutexGuard<'_, Boundary> {
        self.boundary.lock().unwrap_or_else(|e| e.into_inner())
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
    fn scene_change_renews_epoch_while_identity_is_unconfirmed() {
        let state = ProjectState::new();
        let before = state.epoch();

        state.on_scene_changed();

        assert_ne!(
            state.epoch(),
            before,
            "identity 未確定のシーン変更で epoch が維持されました"
        );
    }

    #[test]
    fn scene_change_keeps_epoch_after_identity_is_confirmed() {
        let state = ProjectState::new();
        state.on_project_load(None);
        let epoch = state.epoch();

        state.on_scene_changed();

        assert_eq!(state.epoch(), epoch);
    }

    #[test]
    fn scene_change_does_not_mark_modified() {
        let state = ProjectState::new();
        state.on_project_load(Some(r"C:\projects\sample.aup2"));

        state.on_scene_changed();

        assert!(!state.modified());
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
    fn saving_unsaved_project_advances_generation() {
        let state = ProjectState::new();
        state.on_project_load(None);
        assert!(matches!(state.identity(), ProjectIdentity::Unsaved { .. }));

        state.on_project_save(Some(r"C:\projects\sample.aup2"));
        let ProjectIdentity::Path {
            path: first_path,
            generation: first,
        } = state.identity()
        else {
            panic!("保存後も未保存の identity のままです");
        };
        assert_eq!(first_path, r"C:\projects\sample.aup2");

        state.on_project_load(Some(r"C:\projects\sample.aup2"));
        let ProjectIdentity::Path {
            generation: second, ..
        } = state.identity()
        else {
            panic!("ロード後の identity が保存済みではありません");
        };

        assert!(
            second > first,
            "再ロードで世代が進みませんでした: {first} → {second}"
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
}
