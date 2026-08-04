//! 編集 operation の手順。
//!
//! SDK 呼び出しは [`EditHost`] へ委ね、ここでは受付可否の判定・編集区間の
//! 使い方・前提条件の照合・変更の適用・効果の確認・DTO の組み立てだけを行う。
//! SDK の型は現れない。
//!
//! 1 要求の全処理は 1 回の編集区間に収める。SDK は 1 回の区間で行った変更を
//! まとめて 1 つの取り消し単位として登録するため、区間を分けると利用者が 1 回の
//! 取り消しで戻せない状態が生じる。区間を分けると、その隙間に UI 操作が入って
//! 対象が入れ替わる余地も生まれる。
//!
//! JSON への変換と応答の送信は区間の外で行う。区間の内側は SDK 呼び出しと
//! 所有型へのコピーに限る。

use crate::edit::EditAdapter;
use crate::edit::batch;
use crate::edit::error::{EditError, OccupiedRange, SectionPreconditionReason, UnsupportedReason};
use crate::edit::host::{EditHost, HostSelection, SceneEditor};
use crate::edit::precondition::{
    Boundary, EditKind, ExpectedEpoch, MutationPermit, MutationTicket, verify_boundary,
};
use crate::edit::resolve::{
    ResolvedEffect, ResolvedObject, resolve_effect, resolve_object, resolve_object_with_effects,
};
use crate::project::ProjectState;
use crate::read::ReadError;
use crate::read::adapter::{effect_info_at, object_summary};
use crate::read::host::{EditState, HostEffect, HostLayer, HostObjectPlacement};
use aviutl2_mcp_core::{
    AddEffectParams, ApplyBatchParams, BatchOperation, BatchOutcome, CreateObjectParams,
    CreateObjectSectionParams, Cursor, DeleteEffectParams, DeleteObjectParams,
    DeleteObjectSectionParams, EditOutcome, EffectInfo, EffectType, FocusChange, FrameRange,
    ItemWriteError, LayerInfo, LayerStateOutcome, MoveObjectParams, MoveObjectSectionParams,
    ObjectSectionsOutcome, ObjectSelector, ObjectSource, ObjectSummary, RangeChange, SectionRange,
    SelectionField, SelectionState, SetEffectEnabledParams, SetLayerStateParams,
    SetObjectItemParams, SetObjectNameParams, SetSelectionParams, prepare_item_write,
};
use std::ops::RangeInclusive;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

/// [`EditHost`] の上に編集 operation を実装した adapter。
pub struct HostEditAdapter<H> {
    host: H,
    project: Arc<ProjectState>,
}

impl<H> HostEditAdapter<H> {
    /// ホストとプロジェクト状態から adapter を作る。
    pub fn new(host: H, project: Arc<ProjectState>) -> Self {
        Self { host, project }
    }
}

impl<H: EditHost> HostEditAdapter<H> {
    /// 編集を受け付けられる状態かを確かめる。
    ///
    /// 準備前の編集ハンドルは編集 API の呼び出し自体が許されないため、ここを
    /// 通らない限り [`EditHost`] の他のメソッドを呼ばない。再生中・出力中は
    /// 編集区間へ入れないため、SDK を叩かずに拒否する。
    fn ensure_editable(&self) -> Result<(), EditError> {
        if !catch(|| self.host.is_ready())? {
            return Err(ReadError::NotReady.into());
        }
        match self.edit_state()? {
            EditState::Edit => Ok(()),
            state => Err(ReadError::EditBlocked { state }.into()),
        }
    }

    /// 現在の編集状態を取得する。
    fn edit_state(&self) -> Result<EditState, EditError> {
        guard(|| self.host.edit_state())
    }

    /// panic を捕捉した状態で編集区間へ入る。
    ///
    /// 編集区間のコールバックは C の関数ポインタから呼ばれるため、panic を
    /// 境界の外へ伝播させるとホストのプロセスごと落ちる。クロージャを捕捉層で
    /// 包んでからホストへ渡し、境界を越える巻き戻しを起こさない。
    ///
    /// 編集区間へ入る呼び出し自体も panic し得る。準備前の編集ハンドルは
    /// 呼び出しの入口で落ち、ホストがコールバックを呼ばなかった場合や 2 度
    /// 呼んだ場合にも落ちる。いずれも渡すクロージャだけを包んでも捕捉できず、
    /// 捕捉しなければ接続の境界まで巻き戻って要求元は応答ではなく切断を観測
    /// する。呼び出し全体を捕捉層で包む。
    ///
    /// クロージャを保持する領域は呼び出しごとに解放されないため、捕らえる値は
    /// 参照と数値だけに留め、所有値を移し込まない。
    fn edit_section<T, F>(&self, f: F) -> Result<T, EditError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn SceneEditor) -> Result<T, EditError> + Send,
    {
        // panic を捕捉すると、発行の記録を持つ許可ごと巻き戻る。区間の前後で
        // revision が動いたかを見て、変更が入った可能性を応答へ載せ直す。載せ
        // なければ、revision は進んでいるのに要求元は「変更は入っていない恒久
        // 失敗」と読む。ホストのイベント由来の加算を拾って過大に報告すること
        // はあるが、取りこぼすより害が小さい。
        let before = self.project.revision();
        let entered = catch(|| {
            self.host
                .enter_edit_section(move |editor| guard(|| f(editor)))
        })?;
        match entered {
            Ok(result) => result.map_err(|error| self.attribute_lost_issue(error, before)),
            Err(error) => Err(self.classify_section_failure(error)),
        }
    }

    /// 許可を経ずに区間を抜けた失敗を、変更が入った可能性つきの失敗へ写す。
    ///
    /// panic の捕捉は発行の記録を持つ許可ごと巻き戻し、2 つ目の許可の要求は
    /// 許可を通らずに戻る。どちらも発行済みの変更を失敗へ結び付ける経路を
    /// 持たないため、区間の前後で revision が動いたかで補う。
    fn attribute_lost_issue(&self, error: EditError, revision_before: u64) -> EditError {
        let current = self.project.revision();
        let lost = matches!(
            error,
            EditError::Panicked | EditError::MutationPermitReissued
        );
        if lost && current != revision_before {
            return error.after_mutation(current);
        }
        error
    }

    /// 編集区間へ入れなかった失敗を、現在の編集状態で分類し直す。
    ///
    /// 編集区間の確保は再生中・出力中に失敗する。受付判定と確保の間に再生や
    /// 出力が始まる競合がこの失敗の主因であり、戻り値だけでは他の失敗と
    /// 区別できない。編集状態を読み直して再生・出力中であれば、時間を置けば
    /// 解消する失敗として返す。読み直しにも失敗した場合は元の分類を保つ。
    ///
    /// 区間へ入れなかった以上プロジェクトは変更されておらず、部分適用は生じない。
    fn classify_section_failure(&self, error: EditError) -> EditError {
        match self.edit_state() {
            Ok(EditState::Edit) | Err(_) => error,
            Ok(state) => ReadError::EditBlocked { state }.into(),
        }
    }

    /// 登録済みの effect 名かを、編集区間へ入る前に確かめる。
    ///
    /// SDK の付与失敗は理由を伝えないため、失敗そのものから未登録だと名乗る
    /// ことはできない。呼ぶ前に判定できた場合にだけ未登録として返す。カタログは
    /// 登録済みプラグインの集合でありプロジェクトの編集内容から独立しているため、
    /// 区間の外で確かめても対象が入れ替わることはない。
    fn ensure_effect_registered(&self, effect_name: &str) -> Result<(), EditError> {
        let catalog = guard(|| self.host.effect_catalog())?;
        if catalog.iter().any(|effect| effect.name == effect_name) {
            return Ok(());
        }
        Err(EditError::UnsupportedTarget {
            reason: UnsupportedReason::EffectNotRegistered,
        })
    }

    /// 有効・無効を変更できないと分かる対象を、編集区間へ入る前に弾く。
    ///
    /// 種別による判定は「早く分かる場合に早く返す」ためだけに用いる。ホストが
    /// 無言で拒否したかどうかの最終的な判定は read-back に委ねる。列挙時に
    /// 未知種別が落ちるため、種別からは判断できない対象が残るからである。
    ///
    /// それでも早く返す意味はある。SDK を呼んでしまえば、届いた以上は変更が
    /// 入った側へ倒して revision を進めるほかない。呼ぶ前に分かる対象は呼ばずに
    /// 弾けば、何も変わっていないのに revision が進むことを避けられる。
    ///
    /// 出力項目は有効・無効を変更できない。
    fn ensure_effect_enabled_writable(&self, effect_name: &str) -> Result<(), EditError> {
        let catalog = guard(|| self.host.effect_catalog())?;
        let Some(effect) = catalog.iter().find(|effect| effect.name == effect_name) else {
            return Ok(());
        };
        if effect.effect_type == EffectType::Output {
            return Err(EditError::UnsupportedTarget {
                reason: UnsupportedReason::EffectStateImmutable,
            });
        }
        Ok(())
    }

    /// 中間点を変える 3 つの operation に共通する手順。
    ///
    /// 区間の読み直しを 1 回前倒しして事前確認へ使い、変更のあとにもう 1 度
    /// 読み直して応答へ載せる。追加の SDK 呼び出しはこの読み直しだけである。
    ///
    /// **レイヤーのロックは確かめない。** 中間点はオブジェクトの位置も長さも
    /// 変えず、ロックが止める削除とも時間軸上の移動とも別である。確かめると、
    /// UI が許している編集をここからだけ拒むことになる。
    fn change_sections(
        &self,
        selector: &ObjectSelector,
        precheck: impl Fn(&[SectionRange]) -> Result<(), EditError> + Send,
        apply: impl Fn(
            &dyn SceneEditor,
            MutationTicket<'_>,
            &ResolvedObject<'_>,
        ) -> Result<(), EditError>
        + Send,
    ) -> Result<ObjectSectionsOutcome, EditError> {
        self.ensure_editable()?;
        let project = self.project.as_ref();

        self.edit_section(move |editor| {
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                ExpectedEpoch::Absent,
                EditKind::Content,
                &[],
                &[selector],
            )?;
            let object = resolve_object(editor, &boundary, selector)?;
            // 事前確認は編集区間の内側で読み直した実態に対して行う。要求元が
            // 持っているのは読み取った時点の複製であり、その間に UI で中間点が
            // 動き得る。
            precheck(&editor.object_sections(&object)?)?;

            let permit = boundary.issue_permit(project)?;
            permit.issue(&boundary, |ticket| apply(editor, ticket, &object))?;

            // 中間点はオブジェクトの位置も長さも変えないため、対象は同じ位置で
            // 読み直せる。要求した中間点が実際にどこへ入ったかは区間の一覧が
            // 答える。
            let object_summary = attribute(
                &permit,
                &boundary,
                reread(editor, &boundary, object.layer(), object.frame_start()),
            )?;
            let sections = attribute(&permit, &boundary, editor.object_sections(&object))?;
            Ok(ObjectSectionsOutcome {
                project_epoch: boundary.epoch().to_string(),
                project_revision: permit.project_revision(&boundary),
                object: object_summary,
                sections,
            })
        })
    }
}

/// クロージャの panic を型付きの失敗へ変換し、戻り値はそのまま返す。
fn catch<T>(f: impl FnOnce() -> T) -> Result<T, EditError> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|_| EditError::Panicked)
}

/// 失敗を返し得るクロージャの panic を型付きの失敗へ変換する。
fn guard<T>(f: impl FnOnce() -> Result<T, EditError>) -> Result<T, EditError> {
    catch(f).and_then(|result| result)
}

/// 要求が持つ 0 始まりの位置を内部の添字へ写す。
///
/// 値の範囲は要求の復号時に検証済みであり、対応するプラットフォームでは必ず
/// 収まる。収まらない場合も上限へ丸めれば SDK が範囲外として失敗させる。
pub(crate) fn index(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// レイヤーのロックが止める編集であることを確かめる。
///
/// SDK はレイヤーのロックを尊重しないため、このガードだけが利用者の表明を守る。
/// ただし守る範囲はレイヤーのロックが UI で止めるものに揃える——オブジェクトの
/// 削除と、時間軸上の移動である。設定値の変更も effect の増減も UI の設定
/// パネルから行えるため、ここでは止めない。
///
/// 同じレイヤーを 2 度渡しても読み取りは 1 回に畳む。移動元と移動先が同じ
/// レイヤーになる移動が最も多い使い方であり、そこで 2 回読む理由が無い。
///
/// 読むのはロック状態だけである。ここで使うのは 1 ビットであり、名前と表示の
/// 読み取り失敗が移動や削除の可否を左右する理由が無い。
pub(crate) fn ensure_layers_unlocked(
    editor: &dyn SceneEditor,
    layers: [usize; 2],
) -> Result<(), EditError> {
    let [first, second] = layers;
    ensure_layer_unlocked(editor, first)?;
    if second != first {
        ensure_layer_unlocked(editor, second)?;
    }
    Ok(())
}

/// 対象のレイヤーがロックされていないことを確かめる。
fn ensure_layer_unlocked(editor: &dyn SceneEditor, layer: usize) -> Result<(), EditError> {
    if editor.reader().layer_locked(layer)? {
        return Err(EditError::LayerLocked { layer });
    }
    Ok(())
}

/// 宛先が空いていることを確かめる。
///
/// `moving_from` には移動する対象自身の開始フレームを渡す。自分自身との重なりを
/// 塞がりとして扱わないためである。
///
/// 事前確認と SDK の失敗の双方を用いる。事前確認だけでは足りない——作成される
/// オブジェクトの長さはホストが決めるため、開始位置が空いていても後続の対象と
/// 重なり得る。SDK の失敗だけでも足りない——失敗は理由を区別しないため、何が
/// 起きたのかを要求元へ伝えられない。
///
/// 塞いでいた対象の範囲は失敗へ載せる。要求元は「どこまで塞がっているか」を
/// 知らなければ次の宛先を選べず、走査済みの値を捨てると読み直しを強いることに
/// なる。
pub(crate) fn ensure_destination_free(
    occupants: &[HostObjectPlacement],
    layer: usize,
    frame: usize,
    moving_from: Option<usize>,
) -> Result<(), EditError> {
    let occupant = occupants
        .iter()
        .filter(|placement| placement.layer == layer)
        .filter(|placement| Some(placement.frame_start) != moving_from)
        .find(|placement| placement.frame_start <= frame && frame <= placement.frame_end);
    if let Some(occupant) = occupant {
        return Err(EditError::DestinationOccupied {
            layer,
            frame,
            occupied_by: OccupiedRange {
                frame_start: occupant.frame_start,
                frame_end: occupant.frame_end,
            },
        });
    }
    Ok(())
}

/// 中間点を置くフレームが、いま読み直した区間と両立することを確かめる。
///
/// 見るのはオブジェクトの範囲に入ることと、既存の境界と重ならないことである。
/// どちらも対象の現在の状態で決まるため、要求内容だけの検証では判定できない。
fn ensure_section_can_be_created(sections: &[SectionRange], frame: usize) -> Result<(), EditError> {
    let outside = || EditError::SectionPrecondition {
        reason: SectionPreconditionReason::FrameOutsideObject,
    };
    let (Some(first), Some(last)) = (sections.first(), sections.last()) else {
        return Err(outside());
    };
    if frame < first.start || last.end < frame {
        return Err(outside());
    }
    if sections.iter().any(|section| section.start == frame) {
        return Err(EditError::SectionPrecondition {
            reason: SectionPreconditionReason::SectionBoundaryExists,
        });
    }
    Ok(())
}

/// 区間番号が、いま読み直した区間の列を指していることを確かめる。
///
/// 番号 0 は要求内容だけで拒否済みであるため、ここで見るのは総数との比較だけで
/// ある。区間の数はオブジェクトの現在の状態であり、要求元の手元では確定しない。
fn ensure_section_exists(sections: &[SectionRange], section: usize) -> Result<(), EditError> {
    if section < sections.len() {
        return Ok(());
    }
    Err(EditError::SectionPrecondition {
        reason: SectionPreconditionReason::SectionIndexOutOfRange,
    })
}

/// 中間点の移動先が隣の中間点を越えないことを確かめる。
///
/// 動かせるのは 1 つ前の区間の開始位置より後、1 つ後の区間の開始位置より前
/// である。1 つ後が無ければオブジェクトの終了フレームまでとなる。中間点の順序が
/// 入れ替わらないことは SDK の不変条件であり、崩す要求は届く前に落とす。
fn ensure_section_move_stays_between_neighbours(
    sections: &[SectionRange],
    section: usize,
    frame: usize,
) -> Result<(), EditError> {
    let crosses = EditError::SectionPrecondition {
        reason: SectionPreconditionReason::SectionMoveCrossesBoundary,
    };
    let Some(previous) = section.checked_sub(1).and_then(|index| sections.get(index)) else {
        return Err(crosses);
    };
    if frame <= previous.start {
        return Err(crosses);
    }
    let upper_limit_passed = match sections.get(section + 1) {
        Some(next) => frame >= next.start,
        None => sections.last().is_none_or(|last| frame > last.end),
    };
    if upper_limit_passed {
        return Err(crosses);
    }
    Ok(())
}

/// 変更後の対象を読み直し、応答へ載せる概要を得る。
///
/// 配下 effect は読まない。応答が effect を含まない operation では、effect の
/// 読み取り失敗が反映済みの変更を失敗として報告させてしまう。
fn reread(
    editor: &dyn SceneEditor,
    boundary: &Boundary,
    layer: usize,
    frame_start: usize,
) -> Result<ObjectSummary, EditError> {
    let object = editor.reader().object_identity(layer, frame_start)?;
    Ok(object_summary(
        boundary.epoch(),
        boundary.scene_id(),
        &object,
    ))
}

/// 変更後の対象を、配下 effect の列とともに読み直す。
pub(crate) fn reread_with_effects(
    editor: &dyn SceneEditor,
    boundary: &Boundary,
    layer: usize,
    frame_start: usize,
) -> Result<(ObjectSummary, Vec<HostEffect>), EditError> {
    let detail = editor.reader().object_detail(layer, frame_start)?;
    let summary = object_summary(boundary.epoch(), boundary.scene_id(), &detail.object);
    Ok((summary, detail.effects))
}

/// 変更後の対象から、指定位置の effect 情報を読み直す。
fn reread_effect(
    editor: &dyn SceneEditor,
    boundary: &Boundary,
    object: &ResolvedObject<'_>,
    position: usize,
) -> Result<(ObjectSummary, EffectInfo), EditError> {
    let (summary, effects) =
        reread_with_effects(editor, boundary, object.layer(), object.frame_start())?;
    let info = effect_info_at(&summary.selector, &effects, position).ok_or(EditError::Sdk {
        operation: "get_effect_list",
    })?;
    Ok((summary, info))
}

/// 付与前後の effect 名の列から、増えた 1 件の列全体での位置を求める。
///
/// ハンドルの同値比較には依存しない。生ポインタの一致が同一 effect を意味する
/// 保証は無く、差分は名前の列だけで取れる。
///
/// 差分が 1 件でない場合は位置を確定できない。付与されたのに応答の selector を
/// 組み立てられない状態であり、呼び出し側は失敗として扱う。
fn added_effect_position(before: &[String], after: &[String]) -> Option<usize> {
    if after.len() != before.len() + 1 {
        return None;
    }
    let position = before
        .iter()
        .zip(after)
        .position(|(before, after)| before != after)
        .unwrap_or(before.len());
    if before[position..] != after[position + 1..] {
        return None;
    }
    Some(position)
}

/// effect 名の列を取り出す。
fn effect_names(effects: &[HostEffect]) -> Vec<String> {
    effects.iter().map(|effect| effect.name.clone()).collect()
}

/// 作成の差分を取るために走査するレイヤーの範囲。
///
/// 配置先のレイヤーだけでは足りない。複数オブジェクトを含む alias は各
/// オブジェクトが自分のレイヤーを持てるため、別のレイヤーへ作られた分が差分に
/// 現れず、要求元は自分が作ったものを移動も削除もできなくなる。
///
/// 上限は、オブジェクトが存在する最大レイヤーと `floor` の大きい方とする。
/// 作成の前は配置先を `floor` に渡す——まだ何も無いレイヤーへ作れば最大が配置先
/// まで伸びるためである。作成の後は作成前の上限を渡し、最大を読み直す。alias が
/// どのレイヤーへ展開するかは事前に分からないが、作られたものは必ず「存在する
/// 最大レイヤー」の内側にあるため、読み直した値までを見れば取りこぼさない。
fn creation_scan_range(
    editor: &dyn SceneEditor,
    floor: usize,
) -> Result<RangeInclusive<usize>, EditError> {
    Ok(0..=editor.occupied_layer_max()?.max(floor))
}

/// 指定範囲のレイヤーからオブジェクトの位置を集める。
///
/// alias も effect も読まない。差分を取るのに要るのは位置だけである。
fn scene_placements(
    editor: &dyn SceneEditor,
    layers: RangeInclusive<usize>,
) -> Result<Vec<HostObjectPlacement>, EditError> {
    let mut placements = Vec::new();
    for layer in layers {
        placements.extend(editor.reader().object_placements(layer)?);
    }
    Ok(placements)
}

/// 作成前後の走査から、新たに現れた対象のレイヤーと開始フレームを求める。
///
/// SDK は複数オブジェクトを含む alias でも先頭のハンドルしか返さない。差分を
/// 取らないと 2 件目以降が要求元から到達不能になり、個別に移動も削除もできなく
/// なる。
fn created_placements(
    before: &[HostObjectPlacement],
    after: Vec<HostObjectPlacement>,
) -> Vec<(usize, usize)> {
    let mut created: Vec<(usize, usize)> = after
        .into_iter()
        .map(|placement| (placement.layer, placement.frame_start))
        .filter(|created| {
            !before
                .iter()
                .any(|placement| (placement.layer, placement.frame_start) == *created)
        })
        .collect();
    created.sort_unstable();
    created
}

/// 列挙に現れない設定項目名を、値が読めるかどうかで分ける。
///
/// 設定項目の列挙は未知種別の項目を落とす。落ちた項目への書き込みを「項目が
/// 見つからない」として返すと、要求元は存在しない問題を指す失敗を受け取り、
/// 名前を直そうとして直らない。項目名で値を読めるなら項目は存在しており、
/// 書き込みを公開していない種別である。
///
/// 読めなかった場合は不在として扱う。値の取得は名前の誤りと取得そのものの
/// 失敗を区別しないため、この分岐は「項目が無い」と断定できていない。それでも
/// 不在側へ倒すのは、逆へ倒すと存在しない項目を書き込み可能な対象として扱う
/// ことになるからである。取り違えの向きは、要求元が名前を疑う側に留まる。
///
/// 追加の呼び出しはこの失敗経路でだけ 1 回行う。成功する要求の費用は変わらない。
pub(crate) fn unlisted_item(
    editor: &dyn SceneEditor,
    effect: &ResolvedEffect<'_>,
    item: &str,
) -> EditError {
    match editor.effect_item_value(effect, item) {
        Ok(_) => EditError::UnsupportedTarget {
            reason: UnsupportedReason::ItemTypeNotWritable,
        },
        Err(_) => EditError::ItemWrite(ItemWriteError::ItemNotFound {
            item: item.to_string(),
        }),
    }
}

/// オブジェクト名の要求値を、標準名へ戻す指定と区別できる形へ揃える。
///
/// SDK は `None` と空文字のどちらでも標準名へ戻す。読み直した名前は標準名の
/// とき `None` になるため、照合の前に空文字を `None` へ寄せる。
fn requested_name(name: Option<&str>) -> Option<&str> {
    name.filter(|name| !name.is_empty())
}

/// 読み直したレイヤーの状態が、要求した軸の全てで要求値と一致するか。
///
/// 要求されなかった軸は見ない。ロックだけを変える要求で、他所から名前が
/// 変わっていたことを理由に失敗させる理由が無い。
fn layer_state_applied(
    state: &HostLayer,
    name: Option<Option<&str>>,
    enabled: Option<bool>,
    locked: Option<bool>,
) -> bool {
    if let Some(name) = name
        && state.name.as_deref() != name
    {
        return false;
    }
    if let Some(enabled) = enabled
        && state.enabled != enabled
    {
        return false;
    }
    if let Some(locked) = locked
        && state.locked != locked
    {
        return false;
    }
    true
}

impl<H: EditHost> EditAdapter for HostEditAdapter<H> {
    fn create_object(&self, params: &CreateObjectParams) -> Result<EditOutcome, EditError> {
        self.ensure_editable()?;
        let project = self.project.as_ref();
        let layer = index(params.placement.layer);
        let frame = index(params.placement.frame);

        self.edit_section(move |editor| {
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                ExpectedEpoch::Only(params.expected_project_epoch.as_str()),
                EditKind::Content,
                &[params.placement.scene_id],
                &[],
            )?;
            ensure_layer_unlocked(editor, layer)?;
            // 差分はシーン全体から取る。走査は宛先の事前確認にも使うため、
            // 作成前の走査はここ 1 回で足りる。
            let layers = creation_scan_range(editor, layer)?;
            let before = scene_placements(editor, layers.clone())?;
            ensure_destination_free(&before, layer, frame, None)?;
            // 拡張子だけの確認に留める。実際に読めるかを調べる確認はファイルを
            // 開くため、割り込めない編集区間の内側では行えない。拡張子が通った
            // うえでの失敗は理由を区別できないので、対応していないファイルだとは
            // 名乗らない。
            if let ObjectSource::MediaFile { path } = &params.source
                && !editor.supports_media_file(path)?
            {
                return Err(EditError::UnsupportedTarget {
                    reason: UnsupportedReason::MediaNotSupported,
                });
            }

            let permit = boundary.issue_permit(project)?;
            permit.issue(&boundary, |ticket| match &params.source {
                ObjectSource::MediaFile { path } => {
                    editor.create_object_from_media_file(ticket, path, layer, frame)
                }
                ObjectSource::ObjectAlias { alias } => {
                    editor.create_object_from_alias(ticket, alias, layer, frame)
                }
            })?;

            // 作成は最大レイヤーを配置先より先へも伸ばし得る。走査済みの範囲を
            // 下限に、読み直した最大レイヤーまで広げてから差分を取る。
            let grown = attribute(
                &permit,
                &boundary,
                creation_scan_range(editor, *layers.end()),
            )?;
            let after = attribute(&permit, &boundary, scene_placements(editor, grown))?;
            let created = created_placements(&before, after);
            if created.is_empty() {
                // 作成されたのに位置を特定できない状態であり、応答の selector を
                // 組み立てられない。
                return Err(permit.attribute(
                    &boundary,
                    EditError::Sdk {
                        operation: "get_object_layer_frame",
                    },
                ));
            }

            let mut summaries = Vec::with_capacity(created.len());
            for (created_layer, frame_start) in created {
                let summary = attribute(
                    &permit,
                    &boundary,
                    reread(editor, &boundary, created_layer, frame_start),
                )?;
                summaries.push(summary);
            }
            Ok(EditOutcome::created(
                boundary.epoch(),
                permit.project_revision(&boundary),
                summaries,
            ))
        })
    }

    fn move_object(&self, params: &MoveObjectParams) -> Result<EditOutcome, EditError> {
        self.ensure_editable()?;
        let project = self.project.as_ref();
        let layer = index(params.destination.layer);
        let frame = index(params.destination.frame);

        self.edit_section(move |editor| {
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                ExpectedEpoch::Absent,
                EditKind::Content,
                &[],
                &[&params.selector],
            )?;
            let object = resolve_object(editor, &boundary, &params.selector)?;
            ensure_layers_unlocked(editor, [object.layer(), layer])?;
            let moving_from = (layer == object.layer()).then(|| object.frame_start());
            let occupants = editor.reader().object_placements(layer)?;
            ensure_destination_free(&occupants, layer, frame, moving_from)?;

            let permit = boundary.issue_permit(project)?;
            permit.issue(&boundary, |ticket| {
                editor.move_object(ticket, &object, layer, frame)
            })?;

            // 要求した宛先ではなく、実際の配置を読み直して応答へ載せる。ホストは
            // 位置を調整し得るため、要求値との一致を求めると成功した移動が対象の
            // 不在として返る。移動は対象を破棄しないためトークンは有効なままで
            // あり、位置を直接読める。
            let position = attribute(&permit, &boundary, editor.object_position(&object))?;
            let summary = attribute(
                &permit,
                &boundary,
                reread(editor, &boundary, position.layer, position.frame_start),
            )?;
            Ok(EditOutcome::object_changed(
                boundary.epoch(),
                permit.project_revision(&boundary),
                summary,
            ))
        })
    }

    fn delete_object(&self, params: &DeleteObjectParams) -> Result<EditOutcome, EditError> {
        self.ensure_editable()?;
        let project = self.project.as_ref();

        self.edit_section(move |editor| {
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                ExpectedEpoch::Absent,
                EditKind::Content,
                &[],
                &[&params.selector],
            )?;
            let object = resolve_object(editor, &boundary, &params.selector)?;
            ensure_layer_unlocked(editor, object.layer())?;

            let permit = boundary.issue_permit(project)?;
            permit.issue(&boundary, |ticket| editor.delete_object(ticket, &object))?;

            // 削除は戻り値を持たない。同一区間内で解決し直し、不在を確かめる。
            match editor
                .reader()
                .object_identity(object.layer(), object.frame_start())
            {
                Err(ReadError::ObjectNotFound { .. }) => Ok(EditOutcome::deleted(
                    boundary.epoch(),
                    permit.project_revision(&boundary),
                )),
                Ok(_) => Err(permit.attribute(
                    &boundary,
                    EditError::UnsupportedTarget {
                        reason: UnsupportedReason::ChangeNotApplied,
                    },
                )),
                Err(error) => Err(permit.attribute(&boundary, error.into())),
            }
        })
    }

    fn set_object_name(&self, params: &SetObjectNameParams) -> Result<EditOutcome, EditError> {
        self.ensure_editable()?;
        let project = self.project.as_ref();
        let name = requested_name(params.name.as_deref());

        self.edit_section(move |editor| {
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                ExpectedEpoch::Absent,
                EditKind::Content,
                &[],
                &[&params.selector],
            )?;
            let object = resolve_object(editor, &boundary, &params.selector)?;

            let permit = boundary.issue_permit(project)?;
            permit.issue(&boundary, |ticket| {
                editor.set_object_name(ticket, &object, name)
            })?;

            // 名前の設定は戻り値を持たない。読み直して反映を確かめる。
            let summary = attribute(
                &permit,
                &boundary,
                reread(editor, &boundary, object.layer(), object.frame_start()),
            )?;
            if summary.name.as_deref() != name {
                return Err(permit.attribute(
                    &boundary,
                    EditError::UnsupportedTarget {
                        reason: UnsupportedReason::ChangeNotApplied,
                    },
                ));
            }
            Ok(EditOutcome::object_changed(
                boundary.epoch(),
                permit.project_revision(&boundary),
                summary,
            ))
        })
    }

    fn set_object_item(&self, params: &SetObjectItemParams) -> Result<EditOutcome, EditError> {
        self.ensure_editable()?;
        let project = self.project.as_ref();

        self.edit_section(move |editor| {
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                ExpectedEpoch::Absent,
                EditKind::Content,
                &[],
                &[&params.selector.object],
            )?;
            let (object, effect) = resolve_effect(editor, &boundary, &params.selector)?;

            // 設定項目の実在と種別の照合は、対象 effect が公開する一覧に対して
            // 行う。要求内容だけでは判定できない。
            let items = editor.effect_items(&effect)?;
            let value = match prepare_item_write(&items, &params.item, &params.value) {
                Ok(value) => value,
                Err(ItemWriteError::ItemNotFound { item }) => {
                    return Err(unlisted_item(editor, &effect, &item));
                }
                Err(error) => return Err(EditError::ItemWrite(error)),
            };

            let permit = boundary.issue_permit(project)?;
            permit.issue(&boundary, |ticket| {
                editor.set_effect_item(ticket, &effect, &params.item, &value)
            })?;

            // 読み直した値は成否の判定に使わない。ホスト側で正規化され得るため、
            // 書いた文字列との一致を求めると正常な正規化を失敗と誤診断する。
            // 読み直した値は正規化値として応答へ載せる。
            let (summary, info) = attribute(
                &permit,
                &boundary,
                reread_effect(editor, &boundary, &object, effect.position()),
            )?;
            Ok(EditOutcome::effect_changed(
                boundary.epoch(),
                permit.project_revision(&boundary),
                summary,
                info,
            ))
        })
    }

    fn add_effect(&self, params: &AddEffectParams) -> Result<EditOutcome, EditError> {
        self.ensure_editable()?;
        self.ensure_effect_registered(&params.effect_name)?;
        let project = self.project.as_ref();

        self.edit_section(move |editor| {
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                ExpectedEpoch::Absent,
                EditKind::Content,
                &[],
                &[&params.object],
            )?;
            // 付与位置は前後の差分から求めるため、付与前の effect 列が必要になる。
            let (object, effects) = resolve_object_with_effects(editor, &boundary, &params.object)?;
            let before = effect_names(&effects);

            let permit = boundary.issue_permit(project)?;
            permit.issue(&boundary, |ticket| {
                editor.create_effect(ticket, &object, &params.effect_name)
            })?;

            let (summary, effects) = attribute(
                &permit,
                &boundary,
                reread_with_effects(editor, &boundary, object.layer(), object.frame_start()),
            )?;
            let position =
                added_effect_position(&before, &effect_names(&effects)).ok_or_else(|| {
                    permit.attribute(
                        &boundary,
                        EditError::Sdk {
                            operation: "create_effect",
                        },
                    )
                })?;
            let info = effect_info_at(&summary.selector, &effects, position).ok_or_else(|| {
                permit.attribute(
                    &boundary,
                    EditError::Sdk {
                        operation: "get_effect_list",
                    },
                )
            })?;
            Ok(EditOutcome::effect_changed(
                boundary.epoch(),
                permit.project_revision(&boundary),
                summary,
                info,
            ))
        })
    }

    fn delete_effect(&self, params: &DeleteEffectParams) -> Result<EditOutcome, EditError> {
        self.ensure_editable()?;
        let project = self.project.as_ref();

        self.edit_section(move |editor| {
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                ExpectedEpoch::Absent,
                EditKind::Content,
                &[],
                &[&params.selector.object],
            )?;
            let (object, effect) = resolve_effect(editor, &boundary, &params.selector)?;

            let permit = boundary.issue_permit(project)?;
            permit.issue(&boundary, |ticket| {
                editor.delete_effect(ticket, &object, &effect)
            })?;

            let summary = attribute(
                &permit,
                &boundary,
                reread(editor, &boundary, object.layer(), object.frame_start()),
            )?;
            Ok(EditOutcome::object_changed(
                boundary.epoch(),
                permit.project_revision(&boundary),
                summary,
            ))
        })
    }

    fn set_effect_enabled(
        &self,
        params: &SetEffectEnabledParams,
    ) -> Result<EditOutcome, EditError> {
        self.ensure_editable()?;
        self.ensure_effect_enabled_writable(&params.selector.effect_name)?;
        let project = self.project.as_ref();

        self.edit_section(move |editor| {
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                ExpectedEpoch::Absent,
                EditKind::Content,
                &[],
                &[&params.selector.object],
            )?;
            let (object, effect) = resolve_effect(editor, &boundary, &params.selector)?;

            let permit = boundary.issue_permit(project)?;
            permit.issue(&boundary, |ticket| {
                editor.set_effect_enabled(ticket, &effect, params.enabled)
            })?;

            // 有効・無効の設定は戻り値を持たず、対象によっては無言で無視される。
            // 読み直しが可否の最終的な判定になる。
            let (summary, info) = attribute(
                &permit,
                &boundary,
                reread_effect(editor, &boundary, &object, effect.position()),
            )?;
            if info.enabled != params.enabled {
                return Err(permit.attribute(
                    &boundary,
                    EditError::UnsupportedTarget {
                        reason: UnsupportedReason::EffectStateImmutable,
                    },
                ));
            }
            Ok(EditOutcome::effect_changed(
                boundary.epoch(),
                permit.project_revision(&boundary),
                summary,
                info,
            ))
        })
    }

    fn create_object_section(
        &self,
        params: &CreateObjectSectionParams,
    ) -> Result<ObjectSectionsOutcome, EditError> {
        let frame = index(params.frame);
        self.change_sections(
            &params.selector,
            move |sections| ensure_section_can_be_created(sections, frame),
            move |editor, ticket, object| editor.create_object_section(ticket, object, frame),
        )
    }

    fn delete_object_section(
        &self,
        params: &DeleteObjectSectionParams,
    ) -> Result<ObjectSectionsOutcome, EditError> {
        let section = index(params.section);
        self.change_sections(
            &params.selector,
            move |sections| ensure_section_exists(sections, section),
            move |editor, ticket, object| editor.delete_object_section(ticket, object, section),
        )
    }

    fn move_object_section(
        &self,
        params: &MoveObjectSectionParams,
    ) -> Result<ObjectSectionsOutcome, EditError> {
        let section = index(params.section);
        let frame = index(params.frame);
        self.change_sections(
            &params.selector,
            move |sections| {
                ensure_section_exists(sections, section)?;
                ensure_section_move_stays_between_neighbours(sections, section, frame)
            },
            move |editor, ticket, object| {
                editor.move_object_section(ticket, object, section, frame)
            },
        )
    }

    fn set_layer_state(
        &self,
        params: &SetLayerStateParams,
    ) -> Result<LayerStateOutcome, EditError> {
        self.ensure_editable()?;
        let project = self.project.as_ref();
        let layer = index(params.layer);
        // 標準名へ戻す指定だけが `None` になる。空文字は要求の検証が弾いている
        // ため、ここで `None` へ寄せ直さない。寄せると、要求元が言っていない
        // 「標準名へ戻す」を行って成功を返すことになる。
        let name = params
            .name
            .as_ref()
            .map(aviutl2_mcp_core::LayerNameChange::requested);

        self.edit_section(move |editor| {
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                ExpectedEpoch::Only(params.expected_project_epoch.as_str()),
                EditKind::Content,
                &[params.expected_scene_id],
                &[],
            )?;
            // レイヤーのロックは確かめない。確かめると、ロックされたレイヤーの
            // ロックを外せなくなる。

            let permit = boundary.issue_permit(project)?;
            if let Some(name) = name {
                permit.issue(&boundary, |ticket| {
                    editor.set_layer_name(ticket, layer, name)
                })?;
            }
            if let Some(enabled) = params.enabled {
                permit.issue(&boundary, |ticket| {
                    editor.set_layer_enabled(ticket, layer, enabled)
                })?;
            }
            if let Some(locked) = params.locked {
                permit.issue(&boundary, |ticket| {
                    editor.set_layer_locked(ticket, layer, locked)
                })?;
            }

            // 3 つの setter はいずれも戻り値を持たない。同一区間内で読み直し、
            // 要求した軸が要求値になっていることを確かめる。3 つの属性は
            // レイヤー 1 つあたりの読み取り 1 回で揃う。
            let state = attribute(&permit, &boundary, editor.reader().layer(layer))?;
            if !layer_state_applied(&state, name, params.enabled, params.locked) {
                return Err(permit.attribute(
                    &boundary,
                    EditError::UnsupportedTarget {
                        reason: UnsupportedReason::ChangeNotApplied,
                    },
                ));
            }
            // 応答へ載せる概要は件数も含む。**件数の取得はレイヤー内の走査で
            // あり、オブジェクト数に比例した SDK 呼び出しを伴う。** 属性の
            // 照合を通ってから読むことで、反映されなかった要求ではこの走査を
            // 行わない。
            let object_count = attribute(&permit, &boundary, editor.reader().object_count(layer))?;
            Ok(LayerStateOutcome {
                project_epoch: boundary.epoch().to_string(),
                project_revision: permit.project_revision(&boundary),
                layer: LayerInfo {
                    index: layer,
                    name: state.name,
                    enabled: state.enabled,
                    locked: state.locked,
                    object_count,
                },
            })
        })
    }

    fn set_selection(&self, params: &SetSelectionParams) -> Result<SelectionState, EditError> {
        self.ensure_editable()?;
        let project = self.project.as_ref();

        let (epoch, revision, outcome) = self.edit_section(move |editor| {
            let focus_selector = match &params.focus {
                Some(FocusChange::Set { object }) => Some(object),
                _ => None,
            };
            // 選択状態の変更だけが epoch を 2 か所から受け取る。focus を指定した
            // 要求では、どちらで落ちたかを伝えなければ要求元は直す先を選べない。
            let expected = params.expected_project_epoch.as_str();
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                match focus_selector {
                    Some(_) => ExpectedEpoch::WithFocus(expected),
                    None => ExpectedEpoch::Only(expected),
                },
                EditKind::Selection,
                &[params.expected_scene_id],
                focus_selector.as_slice(),
            )?;
            // フォーカス対象も解決を経る。座標で渡すと、フォーカスだけが
            // fingerprint の照合を経ずに設定される。解決できなければ設定を
            // 行わず、黙って解除もしない。
            let focus = focus_selector
                .map(|selector| resolve_object(editor, &boundary, selector))
                .transpose()?;

            let permit = boundary.issue_permit(project)?;
            let outcome = apply_selection(editor, &permit, &boundary, params, focus.as_ref());
            Ok((
                boundary.epoch().to_string(),
                permit.project_revision(&boundary),
                outcome,
            ))
        })?;

        // 反映値は区間を抜けてから読む。区間が保持する編集情報は入口の複製で
        // あり、フォーカスは区間の処理が終わってから適用されるため、区間内では
        // 観測できない。観測が編集と原子的でないことは応答で伝える。
        let observed = guard(|| self.host.observed_selection())?;
        Ok(selection_state(epoch, revision, observed, outcome))
    }

    fn apply_batch(&self, params: &ApplyBatchParams) -> Result<BatchOutcome, EditError> {
        self.ensure_editable()?;
        let project = self.project.as_ref();

        let applied = self.edit_section(move |editor| {
            // 全 sub-operation のセレクターをまとめて渡す。判定は段ごとに全対象へ
            // 適用されるため、ある sub-operation だけが照合を免れることがない。
            let selectors: Vec<&ObjectSelector> = params
                .operations
                .iter()
                .map(BatchOperation::object_selector)
                .collect();
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                // 全 sub-operation がセレクターを運ぶため、前提の epoch を
                // 別に受け取らない。同じ意味の値を 2 か所へ置くと、不整合な組を
                // 作れる余地だけが増える。
                ExpectedEpoch::Absent,
                EditKind::Content,
                &[],
                &selectors,
            )
            .map_err(|error| {
                batch::locate_boundary_failure(&params.operations, &project.epoch(), error)
            })?;
            batch::apply_batch(editor, project, &boundary, &params.operations)
        });
        applied.map_err(batch::mark_lost_section)
    }
}

/// 変更 API の呼び出し結果を、発行後の失敗として印を付ける。
pub(crate) fn attribute<T>(
    permit: &MutationPermit<'_>,
    boundary: &Boundary,
    result: Result<T, impl Into<EditError>>,
) -> Result<T, EditError> {
    result.map_err(|error| permit.attribute(boundary, error.into()))
}

/// 適用を試みた結果。
///
/// 要求された項目は必ず `applied` か `not_applied` のどちらかに現れる。
struct SelectionOutcome {
    /// 実際に適用できた項目。
    applied: Vec<SelectionField>,
    /// 要求されたが適用できなかった項目。
    not_applied: Vec<SelectionField>,
}

/// カーソル・選択範囲・フォーカスを固定の順序で適用する。
///
/// 順序を固定するのは、途中で失敗したときの状態を一意にするためである。
/// フォーカスはどのみち区間の処理の最後に適用されるため、この順序は SDK の
/// 挙動とも整合する。
///
/// 途中で失敗しても先に適用した分は巻き戻さず、以降も試みない。適用の可否は
/// **常に成功応答の 2 つの一覧で伝える**。失敗として返すと、どこまで適用された
/// かを載せる場所が無くなる（失敗の補助情報に項目の一覧を置く余地は無い）。
/// 一方で「何件適用できたか」で成功と失敗を分けると、同じ失敗が同時に何を
/// 要求したかによって成功にも失敗にもなり、要求元から予測できない。
fn apply_selection(
    editor: &dyn SceneEditor,
    permit: &MutationPermit<'_>,
    boundary: &Boundary,
    params: &SetSelectionParams,
    focus: Option<&ResolvedObject<'_>>,
) -> SelectionOutcome {
    let mut requested = Vec::new();
    let mut applied = Vec::new();
    let mut failure = None;

    if let Some(cursor) = &params.cursor {
        requested.push(SelectionField::Cursor);
        let layer = index(cursor.layer);
        let frame = index(cursor.frame);
        match permit.issue(boundary, |ticket| editor.set_cursor(ticket, layer, frame)) {
            Ok(()) => applied.push(SelectionField::Cursor),
            Err(error) => failure = Some(error),
        }
    }
    if let Some(change) = &params.selected_range {
        requested.push(SelectionField::SelectedRange);
        let range = match change {
            RangeChange::Set { start, end } => Some(FrameRange {
                start: index(*start),
                end: index(*end),
            }),
            RangeChange::Clear {} => None,
        };
        if failure.is_none() {
            match permit.issue(boundary, |ticket| editor.set_select_range(ticket, range)) {
                Ok(()) => applied.push(SelectionField::SelectedRange),
                Err(error) => failure = Some(error),
            }
        }
    }
    if let Some(change) = &params.focus {
        requested.push(SelectionField::Focus);
        let target = match change {
            FocusChange::Set { .. } => focus,
            FocusChange::Clear {} => None,
        };
        if failure.is_none() {
            match permit.issue(boundary, |ticket| editor.set_focus_object(ticket, target)) {
                Ok(()) => applied.push(SelectionField::Focus),
                Err(error) => failure = Some(error),
            }
        }
    }

    if let Some(error) = failure {
        tracing::warn!(
            code = %error.error_code().as_snake_case(),
            "選択状態の一部を適用できませんでした"
        );
    }
    let not_applied = requested
        .into_iter()
        .filter(|field| !applied.contains(field))
        .collect();
    SelectionOutcome {
        applied,
        not_applied,
    }
}

/// 観測した選択状態から応答を組み立てる。
fn selection_state(
    epoch: String,
    revision: u64,
    observed: HostSelection,
    outcome: SelectionOutcome,
) -> SelectionState {
    let focus = observed
        .focus
        .as_ref()
        .map(|object| object_summary(&epoch, observed.scene_id, object));
    SelectionState::observed(
        epoch,
        revision,
        Cursor {
            frame: observed.cursor.frame,
            layer: observed.cursor.layer,
        },
        observed.selected_range,
        focus,
        outcome.applied,
        outcome.not_applied,
    )
}

#[cfg(test)]
mod tests;
