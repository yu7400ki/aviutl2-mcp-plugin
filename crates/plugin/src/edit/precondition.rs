//! 編集区間の内側で行う前提条件の照合。
//!
//! 判定は次の順に行う。ひとつでも失敗したら SDK の変更 API を呼ばずに区間を
//! 抜ける。
//!
//! 1. 前提の `project_epoch` が現在の epoch と一致するか
//! 2. セレクターの `project_epoch` が同じく一致するか
//! 3. シーンの guard が区間へ入った時点の現在シーンと一致するか
//! 4. セレクターの fingerprint 算出方式が現在生成できる方式と一致するか
//! 5. セレクターが指す対象を解決できるか
//! 6. 解決した対象の fingerprint がセレクターの値と一致するか
//! 7. operation 固有の事前条件
//!
//! 1〜4 は [`verify_boundary`] が要求全体へ適用し、5〜6 は解決処理が行う。
//! 判定を飛ばして変更へ進む経路が作れないよう、変更 API は
//! [`MutationTicket`] を要求する。権利を作れるのは [`MutationPermit::issue`] だけ、
//! permit を作れるのは [`Boundary::revalidate`] だけ、[`Boundary`] を作れるのは
//! [`verify_boundary`] だけ、という連鎖で順序を型が強制する。
//!
//! # `project_revision` を照合しない
//!
//! 要求は [`Expected::project_revision`] を必須で受け取るが、**現在の revision と
//! 比べない。** 照合を書き忘れているのではなく、意図して行っていない。
//!
//! revision はプロジェクト全体で 1 つのカウンタであり、どのオブジェクトを
//! 編集しても、UI 上の操作でも進む。利用者が UI で編集しながら要求を送る使い方
//! では、対象を読んでから要求が届くまでの間にほぼ確実にずれる。訂正して送り
//! 直す間にもまた進むため、人が手を動かしている限り収束しない。
//!
//! 一方で revision だけが捕まえるものは狭い。対象の内容が変わったことは
//! fingerprint が、別のプロジェクトであることは epoch が、同名 effect の位置の
//! 繰り上がりは effect fingerprint の材料（列の絶対位置と総数）が、それぞれ
//! 独立に捕まえる。revision だけが残るのは「内容が完全に同一の状態へ戻った」
//! 場合に限られ、しかもホスト由来の変更は非同期に届くため、その検出も確実では
//! ない。
//!
//! 同じ要求を二度発行してしまうことも revision には依存していない。effect の
//! 付与は対象オブジェクトの fingerprint を変え、オブジェクトの作成は宛先の
//! 重複を区間内で事前確認するため、いずれも再送はそこで止まる。
//!
//! 発行による revision の加算と、応答が返す revision は残す。要求元へ変更が
//! 入ったことを伝える値であり、前提条件の照合とは別の役割を持つ。

use crate::edit::error::EditError;
use crate::project::ProjectState;
use crate::read::ReadError;
use crate::read::adapter::ensure_fingerprint_algorithm;
use crate::read::host::HostEditInfo;
use aviutl2_mcp_core::{Expected, ObjectSelector};
use std::cell::Cell;
use std::marker::PhantomData;

/// 編集がプロジェクトの内容を変えるか。
///
/// カーソル・選択範囲・フォーカスはプロジェクトの内容ではない。内容を変えない
/// 操作で revision を進めると、要求元は未保存の変更が生まれたと受け取る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditKind {
    /// プロジェクトの内容を変える。変更の発行で revision を進める。
    Content,
    /// 選択状態だけを変える。revision を進めない。
    Selection,
}

impl EditKind {
    /// 変更の発行で revision を進めるか。
    fn advances_revision(self) -> bool {
        matches!(self, EditKind::Content)
    }
}

/// プロジェクト境界と revision をまとめて読み取った結果。
///
/// epoch と revision は 1 度の取得でまとめて読む。別々に読むと、プロジェクトの
/// ロードによる「epoch 差し替え → revision リセット」の途中を観測し得る。応答は
/// 両方を返すため、食い違った組を返すと要求元は存在しない世代を手にする。
#[derive(Debug)]
struct ProjectBoundary {
    epoch: String,
    revision: u64,
}

impl ProjectBoundary {
    /// 現在の境界を読み取る。
    ///
    /// 境界を守るロックは文字列の複製と差し替えだけを挟む葉のロックであり、
    /// ここでは取得して即座に解放する。取得したまま SDK を呼ばないことが
    /// 守るべき条件である。
    fn load(project: &ProjectState) -> Self {
        Self {
            epoch: project.epoch(),
            revision: project.revision(),
        }
    }
}

/// 判定 1〜4 を通した編集区間の前提。
///
/// この型を作れるのは [`verify_boundary`] だけである。
#[derive(Debug)]
pub(crate) struct Boundary {
    observed: ProjectBoundary,
    scene_id: i32,
    kind: EditKind,
    /// 変更の許可を既に取り出したか。
    ///
    /// 許可はそれぞれ独立に発行を数える。1 要求で 2 つ取ると、同じ要求の中で
    /// revision が 2 度進み、応答が返す値がどちらの許可のものか定まらない。
    spent: Cell<bool>,
}

impl Boundary {
    /// 判定を通った時点のプロジェクト epoch。
    pub(crate) fn epoch(&self) -> &str {
        &self.observed.epoch
    }

    /// 判定を通った時点のプロジェクト revision。
    pub(crate) fn revision(&self) -> u64 {
        self.observed.revision
    }

    /// 区間へ入った時点の現在シーン ID。
    pub(crate) fn scene_id(&self) -> i32 {
        self.scene_id
    }

    /// 変更 API を発行する直前にプロジェクト境界を再検証する。
    ///
    /// 判定 1〜2 と実際の変更の間には、対象の解決と fingerprint の再計算が挟まる。
    /// 大きなレイヤーでは相応の時間がかかり、その間に別のプロジェクトが開かれ
    /// 得る。ここで不一致なら SDK を呼ばずに中断する。まだ何も変更していない
    /// ため中断は安全であり、変更の発行も記録されない。
    ///
    /// 1 要求で 2 度呼ぶことはできない。許可はそれぞれ独立に発行を数えるため、
    /// 2 つ取ると revision が 2 度進み、応答が返す値が定まらない。
    pub(crate) fn revalidate<'a>(
        &self,
        project: &'a ProjectState,
    ) -> Result<MutationPermit<'a>, EditError> {
        if self.spent.replace(true) {
            return Err(EditError::MutationPermitReissued);
        }
        let current = ProjectBoundary::load(project);
        if current.epoch != self.observed.epoch {
            return Err(ReadError::EpochMismatch.into());
        }
        Ok(MutationPermit {
            project,
            records_revision: self.kind.advances_revision(),
            issued: Cell::new(None),
        })
    }
}

/// 変更 API を発行してよいことの証。
///
/// 変更 API はこの型を要求するため、前提条件の照合と再検証を経ずに変更を
/// 発行する経路が存在しない。
///
/// 発行の記録もここで行う。記録の引き金は「要求全体の成功」ではなく「SDK の
/// 変更 API を 1 回でも発行したこと」である。成功が確定してから記録する形に
/// すると、発行した後に失敗した要求で、変更は入ったのに未保存の変更が無いと
/// 主張し続けることになる。それを信じて閉じれば変更は失われる。適用されたか
/// 判別できない場合は、変更が入った側へ倒す。
pub(crate) struct MutationPermit<'a> {
    project: &'a ProjectState,
    /// 発行で revision を進めるか。
    records_revision: bool,
    /// 最初の発行で確定した revision。
    issued: Cell<Option<u64>>,
}

impl MutationPermit<'_> {
    /// 変更 API を 1 回発行する。
    ///
    /// 権利を作れるのはここだけであり、変更 API は権利を値で要求する。したがって
    /// 記録を伴わずに変更を発行する経路が無い。
    ///
    /// 記録するのは呼び出しが SDK へ届いた場合に限る。届いていれば、適用された
    /// かどうかは戻り値からは判断できないため変更が入った側へ倒す。届いて
    /// いない失敗（対象の不在・引数を写せない）はプロジェクトを一切変えて
    /// いないので、記録すると「何も変わっていないのに未保存の変更あり」を残し、
    /// 要求元に無意味な読み直しを強いる。
    pub(crate) fn issue<T>(
        &self,
        boundary: &Boundary,
        call: impl FnOnce(MutationTicket<'_>) -> Result<T, EditError>,
    ) -> Result<T, EditError> {
        let result = call(MutationTicket {
            _permit: PhantomData,
        });
        if !matches!(result, Err(EditError::NotIssued { .. })) {
            self.record_issue();
        }
        result.map_err(|error| self.attribute(boundary, error))
    }

    /// 変更 API の発行を記録する。
    fn record_issue(&self) {
        if !self.records_revision || self.issued.get().is_some() {
            return;
        }
        self.issued.set(Some(self.project.on_edit_issued()));
    }

    /// 応答へ載せる revision。
    ///
    /// 変更を発行していれば加算後の値、発行していなければ判定時点の値を返す。
    /// 加算後に改めて読み直すと、その間にホストのイベントが配送された場合に
    /// 別の値を読み、返す値が非決定になる。
    pub(crate) fn project_revision(&self, boundary: &Boundary) -> u64 {
        self.issued.get().unwrap_or_else(|| boundary.revision())
    }

    /// 変更を発行した後の失敗として包み直す。発行していなければそのまま返す。
    pub(crate) fn attribute(&self, boundary: &Boundary, error: EditError) -> EditError {
        match self.issued.get() {
            Some(_) => error.after_mutation(self.project_revision(boundary)),
            None => error,
        }
    }
}

/// 変更 API を 1 回発行する権利。
///
/// SDK の変更 API はこの型を値で受け取る。複製できないため 1 つの権利で 2 回
/// 発行することはできず、生成は [`MutationPermit::issue`] に限られるので、
/// 発行の記録を伴わずに変更へ進む経路が無い。
pub struct MutationTicket<'a> {
    _permit: PhantomData<&'a ()>,
}

/// 判定 1〜4 を要求全体へ適用する。
///
/// `guards` には要求が持つ全てのシーン guard を、`selectors` には要求が含む
/// 全ての [`ObjectSelector`] をネストも含めて渡す。判定を段ごとに全対象へ
/// 適用するので、ある selector だけが照合を免れることがない。
pub(crate) fn verify_boundary(
    project: &ProjectState,
    entry_info: &HostEditInfo,
    expected: &Expected,
    kind: EditKind,
    guards: &[i32],
    selectors: &[&ObjectSelector],
) -> Result<Boundary, EditError> {
    let observed = ProjectBoundary::load(project);

    // 1. 前提の epoch。
    if expected.project_epoch != observed.epoch {
        return Err(ReadError::EpochMismatch.into());
    }
    // 2. セレクターの epoch。前提とセレクターは別々の応答に由来し得るため、
    //    両方を照合して初めて「同じプロジェクトの、その時点の対象」が定まる。
    for selector in selectors {
        if selector.project_epoch != observed.epoch {
            return Err(ReadError::EpochMismatch.into());
        }
    }
    // 3. シーンの guard。区間へ入った時点の編集情報と照合する。
    let scene_id = entry_info.scene_id;
    for guard in guards {
        ensure_scene(*guard, scene_id)?;
    }
    for selector in selectors {
        ensure_scene(selector.scene_id, scene_id)?;
    }
    // 4. fingerprint の算出方式。
    for selector in selectors {
        ensure_fingerprint_algorithm(selector.fingerprint_algorithm.as_ref())?;
    }

    Ok(Boundary {
        observed,
        scene_id,
        kind,
        spent: Cell::new(false),
    })
}

/// シーンの guard が現在シーンと一致することを確かめる。
fn ensure_scene(expected: i32, current: i32) -> Result<(), EditError> {
    if expected == current {
        return Ok(());
    }
    Err(ReadError::SceneMismatch { expected, current }.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::{
        ErrorCode, FingerprintAlgorithm, ObjectFingerprintInput, ObjectSummary,
    };
    use serde_json::json;
    use std::sync::Arc;

    fn edit_info(scene_id: i32) -> HostEditInfo {
        HostEditInfo {
            scene_id,
            width: 1920,
            height: 1080,
            fps_rate: 30,
            fps_scale: 1,
            sample_rate: 48000,
            cursor_frame: 0,
            cursor_layer: 0,
            frame_max: 100,
            layer_max: 2,
            display_frame_start: 0,
            display_layer_start: 0,
            display_frame_num: 10,
            display_layer_num: 10,
            select_range_start: None,
            select_range_end: None,
        }
    }

    fn selector(epoch: &str) -> ObjectSelector {
        ObjectSummary::new(
            epoch,
            ObjectFingerprintInput {
                scene_id: 0,
                layer: 1,
                frame_start: 100,
                frame_end: 200,
                name: None,
                alias: "alias",
            },
        )
        .selector
    }

    fn expected(epoch: &str, revision: u64) -> Expected {
        Expected {
            project_epoch: epoch.to_string(),
            project_revision: revision,
        }
    }

    fn state() -> Arc<ProjectState> {
        Arc::new(ProjectState::new())
    }

    #[test]
    fn boundary_is_returned_when_every_check_passes() {
        let project = state();
        let epoch = project.epoch();
        let boundary = verify_boundary(
            &project,
            &edit_info(0),
            &expected(&epoch, 0),
            EditKind::Content,
            &[0],
            &[&selector(&epoch)],
        )
        .expect("全ての判定を通る要求が拒否されました");
        assert_eq!(boundary.epoch(), epoch);
        assert_eq!(boundary.revision(), 0);
        assert_eq!(boundary.scene_id(), 0);
    }

    #[test]
    fn expected_epoch_is_checked_before_the_selector() {
        let project = state();
        let epoch = project.epoch();
        // 前提とセレクターの双方が食い違う要求で、先に落ちる段を確かめる。
        let error = verify_boundary(
            &project,
            &edit_info(0),
            &expected("別のプロジェクト", 0),
            EditKind::Content,
            &[0],
            &[&selector("さらに別のプロジェクト")],
        )
        .expect_err("epoch 不一致が受理されました");
        assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
        assert_eq!(error.details()["mismatch"], json!("project_epoch"));
        let _ = epoch;
    }

    #[test]
    fn either_epoch_alone_is_enough_to_reject() {
        let project = state();
        let epoch = project.epoch();
        for (expected_epoch, selector_epoch) in [
            ("別のプロジェクト".to_string(), epoch.clone()),
            (epoch.clone(), "別のプロジェクト".to_string()),
        ] {
            let error = verify_boundary(
                &project,
                &edit_info(0),
                &expected(&expected_epoch, 0),
                EditKind::Content,
                &[0],
                &[&selector(&selector_epoch)],
            )
            .expect_err("片方だけの epoch 不一致が受理されました");
            assert_eq!(error.details()["mismatch"], json!("project_epoch"));
        }
    }

    #[test]
    fn a_stale_revision_does_not_reject_a_content_edit() {
        let project = state();
        project.on_object_updated();
        let epoch = project.epoch();
        verify_boundary(
            &project,
            &edit_info(0),
            &expected(&epoch, 0),
            EditKind::Content,
            &[0],
            &[&selector(&epoch)],
        )
        .expect("古い revision の前提が拒否されました");
    }

    #[test]
    fn a_stale_revision_does_not_hide_the_scene_check() {
        // revision を見なくなっても、後続の判定はそのまま働く。
        let project = state();
        project.on_object_updated();
        let epoch = project.epoch();
        let error = verify_boundary(
            &project,
            &edit_info(7),
            &expected(&epoch, 0),
            EditKind::Content,
            &[0],
            &[&selector(&epoch)],
        )
        .expect_err("シーンの食い違いが受理されました");
        assert_eq!(error.details()["mismatch"], json!("scene_id"));
    }

    #[test]
    fn a_stale_revision_does_not_reject_a_selection_edit() {
        let project = state();
        project.on_object_updated();
        let epoch = project.epoch();
        verify_boundary(
            &project,
            &edit_info(0),
            &expected(&epoch, 0),
            EditKind::Selection,
            &[0],
            &[],
        )
        .expect("選択状態の変更が古い revision で拒否されました");
    }

    #[test]
    fn scene_is_checked_before_the_fingerprint_algorithm() {
        let project = state();
        let epoch = project.epoch();
        let mut stale = selector(&epoch);
        stale.fingerprint_algorithm = Some(FingerprintAlgorithm::Unknown(
            "sha256-future-v9".to_string(),
        ));
        stale.scene_id = 9;
        let error = verify_boundary(
            &project,
            &edit_info(0),
            &expected(&epoch, 0),
            EditKind::Content,
            &[],
            &[&stale],
        )
        .expect_err("シーンの食い違いが受理されました");
        assert_eq!(error.details()["mismatch"], json!("scene_id"));
        assert_eq!(error.details()["expected_scene_id"], json!(9));
        assert_eq!(error.details()["current_scene_id"], json!(0));
    }

    #[test]
    fn unknown_fingerprint_algorithm_is_rejected() {
        let project = state();
        let epoch = project.epoch();
        let mut stale = selector(&epoch);
        stale.fingerprint_algorithm = Some(FingerprintAlgorithm::Unknown(
            "sha256-future-v9".to_string(),
        ));
        let error = verify_boundary(
            &project,
            &edit_info(0),
            &expected(&epoch, 0),
            EditKind::Content,
            &[],
            &[&stale],
        )
        .expect_err("未知の算出方式が受理されました");
        assert_eq!(error.details()["mismatch"], json!("fingerprint_algorithm"));
    }

    #[test]
    fn revalidation_rejects_a_project_boundary_change() {
        let project = state();
        let epoch = project.epoch();
        let boundary = verify_boundary(
            &project,
            &edit_info(0),
            &expected(&epoch, 0),
            EditKind::Content,
            &[],
            &[],
        )
        .unwrap();
        project.on_project_load(Some(r"C:\projects\other.aup2"));
        let error = boundary
            .revalidate(&project)
            .err()
            .expect("境界の更新後に変更が許可されました");
        assert_eq!(error.details()["mismatch"], json!("project_epoch"));
        assert_eq!(project.revision(), 0, "中断したのに revision が進みました");
    }

    #[test]
    fn revalidation_ignores_a_revision_change() {
        // 再検証が見るのはプロジェクト境界だけである。解決中に他所の変更が
        // 入っても、対象の内容が変わっていなければ変更を止めない。
        let project = state();
        let epoch = project.epoch();
        let boundary = verify_boundary(
            &project,
            &edit_info(0),
            &expected(&epoch, 0),
            EditKind::Content,
            &[],
            &[],
        )
        .unwrap();
        project.on_object_updated();
        boundary
            .revalidate(&project)
            .expect("revision の変化で変更が拒否されました");
    }

    #[test]
    fn a_second_permit_cannot_be_taken_for_the_same_request() {
        // 許可はそれぞれ独立に発行を数える。2 つ取れると、同じ要求の中で
        // revision が 2 度進み、応答が返す値がどちらのものか定まらない。
        let project = state();
        let epoch = project.epoch();
        let boundary = verify_boundary(
            &project,
            &edit_info(0),
            &expected(&epoch, 0),
            EditKind::Content,
            &[],
            &[],
        )
        .unwrap();
        boundary.revalidate(&project).expect("1 度目の許可");
        let Err(error) = boundary.revalidate(&project) else {
            panic!("同じ要求で 2 つ目の許可が取れました");
        };
        assert_eq!(error.error_code(), ErrorCode::InternalError);
        // 巻き戻しは起きていない。捕捉した panic を名乗ると、運用者は起きて
        // いない panic の原因を探すことになる。
        assert!(
            matches!(error, EditError::MutationPermitReissued),
            "{error} が 2 つ目の許可として報告されていません"
        );
    }

    #[test]
    fn the_first_issue_advances_the_revision_once() {
        let project = state();
        let epoch = project.epoch();
        let boundary = verify_boundary(
            &project,
            &edit_info(0),
            &expected(&epoch, 0),
            EditKind::Content,
            &[],
            &[],
        )
        .unwrap();
        let permit = boundary.revalidate(&project).unwrap();
        assert_eq!(permit.project_revision(&boundary), 0);

        let _ = permit.issue(&boundary, |_ticket| Ok(()));
        let _ = permit.issue(&boundary, |_ticket| Ok(()));

        assert_eq!(permit.project_revision(&boundary), 1);
        assert_eq!(project.revision(), 1);
        assert!(project.modified());
    }

    #[test]
    fn selection_edits_do_not_advance_the_revision() {
        let project = state();
        let epoch = project.epoch();
        let boundary = verify_boundary(
            &project,
            &edit_info(0),
            &expected(&epoch, 0),
            EditKind::Selection,
            &[],
            &[],
        )
        .unwrap();
        let permit = boundary.revalidate(&project).unwrap();
        let _ = permit.issue(&boundary, |_ticket| Ok(()));
        assert_eq!(project.revision(), 0);
        assert!(!project.modified());
    }

    #[test]
    fn failures_before_the_first_issue_are_not_attributed_to_a_mutation() {
        let project = state();
        let epoch = project.epoch();
        let boundary = verify_boundary(
            &project,
            &edit_info(0),
            &expected(&epoch, 0),
            EditKind::Content,
            &[],
            &[],
        )
        .unwrap();
        let permit = boundary.revalidate(&project).unwrap();
        let error = permit.attribute(&boundary, EditError::Panicked);
        assert!(error.details().get("mutation_issued").is_none());

        let _ = permit.issue(&boundary, |_ticket| Ok(()));
        let error = permit.attribute(&boundary, EditError::Panicked);
        assert_eq!(error.details()["mutation_issued"], json!(true));
        assert_eq!(error.details()["current_project_revision"], json!(1));
    }
}
