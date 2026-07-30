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
use crate::edit::error::{EditError, UnsupportedReason};
use crate::edit::host::{EditHost, HostSelection, SceneEditor};
use crate::edit::precondition::{Boundary, EditKind, MutationPermit, verify_boundary};
use crate::edit::resolve::{
    ResolvedObject, effect_info_at, resolve_effect, resolve_object, resolve_object_with_effects,
};
use crate::project::ProjectState;
use crate::read::ReadError;
use crate::read::adapter::object_summary;
use crate::read::host::{EditState, HostEffect, HostObjectPlacement};
use aviutl2_mcp_core::{
    AddEffectParams, CreateObjectParams, Cursor, DeleteEffectParams, DeleteObjectParams,
    EditOutcome, EffectInfo, EffectType, FocusChange, FrameRange, MoveObjectParams, ObjectSource,
    ObjectSummary, RangeChange, SelectionField, SelectionState, SetEffectStateParams,
    SetObjectItemParams, SetObjectNameParams, SetSelectionParams, prepare_item_write,
};
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

    /// 有効・ロックを変更できないと分かる対象を、編集区間へ入る前に弾く。
    ///
    /// 種別による判定は「早く分かる場合に早く返す」ためだけに用いる。ホストが
    /// 無言で拒否したかどうかの最終的な判定は read-back に委ねる。列挙時に
    /// 未知種別が落ちるため、種別からは判断できない対象が残るからである。
    ///
    /// それでも早く返す意味はある。SDK を呼んでしまえば、届いた以上は変更が
    /// 入った側へ倒して revision を進めるほかない。呼ぶ前に分かる対象は呼ばずに
    /// 弾けば、何も変わっていないのに revision が進むことを避けられる。
    ///
    /// 判定できるのは 2 つだけである。出力項目は有効・無効を変更できない。
    /// 音声だけを扱う effect はロックを変更できない。フラグは画像と音声が同時に
    /// 立ち得るため、音声のフラグが立っていることだけでは音声 effect と断定
    /// できない。画像を扱わないことまで確かめる。
    fn ensure_effect_state_writable(
        &self,
        effect_name: &str,
        params: &SetEffectStateParams,
    ) -> Result<(), EditError> {
        if params.enabled.is_none() && params.locked.is_none() {
            return Ok(());
        }
        let catalog = guard(|| self.host.effect_catalog())?;
        let Some(effect) = catalog.iter().find(|effect| effect.name == effect_name) else {
            return Ok(());
        };
        let immutable_enabled =
            params.enabled.is_some() && effect.effect_type == EffectType::Output;
        let immutable_locked = params.locked.is_some() && effect.flags.audio && !effect.flags.video;
        if immutable_enabled || immutable_locked {
            return Err(EditError::UnsupportedTarget {
                reason: UnsupportedReason::EffectStateImmutable,
            });
        }
        Ok(())
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
fn index(value: u32) -> usize {
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
fn ensure_layers_unlocked(editor: &dyn SceneEditor, layers: [usize; 2]) -> Result<(), EditError> {
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
fn ensure_destination_free(
    occupants: &[HostObjectPlacement],
    layer: usize,
    frame: usize,
    moving_from: Option<usize>,
) -> Result<(), EditError> {
    let occupied = occupants
        .iter()
        .filter(|placement| Some(placement.frame_start) != moving_from)
        .any(|placement| placement.frame_start <= frame && frame <= placement.frame_end);
    if occupied {
        return Err(EditError::DestinationOccupied { layer, frame });
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
fn reread_with_effects(
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

/// 作成前後のレイヤー列挙から、新たに現れた対象の開始フレームを求める。
///
/// SDK は複数オブジェクトを含む alias でも先頭のハンドルしか返さない。差分を
/// 取らないと 2 件目以降が要求元から到達不能になり、個別に移動も削除もできなく
/// なる。
fn created_frame_starts(
    before: &[HostObjectPlacement],
    after: Vec<HostObjectPlacement>,
) -> Vec<usize> {
    let mut created: Vec<usize> = after
        .into_iter()
        .map(|placement| placement.frame_start)
        .filter(|frame_start| {
            !before
                .iter()
                .any(|placement| placement.frame_start == *frame_start)
        })
        .collect();
    created.sort_unstable();
    created
}

/// オブジェクト名の要求値を、標準名へ戻す指定と区別できる形へ揃える。
///
/// SDK は `None` と空文字のどちらでも標準名へ戻す。読み直した名前は標準名の
/// とき `None` になるため、照合の前に空文字を `None` へ寄せる。
fn requested_name(name: Option<&str>) -> Option<&str> {
    name.filter(|name| !name.is_empty())
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
                Some(params.expected_project_epoch.as_str()),
                EditKind::Content,
                &[params.placement.scene_id],
                &[],
            )?;
            ensure_layer_unlocked(editor, layer)?;
            let before = editor.reader().object_placements(layer)?;
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

            let after = attribute(&permit, &boundary, editor.reader().object_placements(layer))?;
            let created = created_frame_starts(&before, after);
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
            for frame_start in created {
                let summary = attribute(
                    &permit,
                    &boundary,
                    reread(editor, &boundary, layer, frame_start),
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
                None,
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

            let summary = attribute(&permit, &boundary, reread(editor, &boundary, layer, frame))?;
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
                None,
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
                None,
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
                None,
                EditKind::Content,
                &[],
                &[&params.selector.object],
            )?;
            let (object, effect) = resolve_effect(editor, &boundary, &params.selector)?;

            // 設定項目の実在と種別の照合は、対象 effect が公開する一覧に対して
            // 行う。要求内容だけでは判定できない。
            let items = editor.effect_items(&effect)?;
            let value = prepare_item_write(&items, &params.item, &params.value)
                .map_err(EditError::ItemWrite)?;

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
                None,
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
                None,
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

    fn set_effect_state(&self, params: &SetEffectStateParams) -> Result<EditOutcome, EditError> {
        self.ensure_editable()?;
        self.ensure_effect_state_writable(&params.selector.effect_name, params)?;
        let project = self.project.as_ref();

        self.edit_section(move |editor| {
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                None,
                EditKind::Content,
                &[],
                &[&params.selector.object],
            )?;
            let (object, effect) = resolve_effect(editor, &boundary, &params.selector)?;

            let permit = boundary.issue_permit(project)?;
            if let Some(enabled) = params.enabled {
                permit.issue(&boundary, |ticket| {
                    editor.set_effect_enabled(ticket, &effect, enabled)
                })?;
            }
            if let Some(locked) = params.locked {
                permit.issue(&boundary, |ticket| {
                    editor.set_effect_locked(ticket, &effect, locked)
                })?;
            }

            // 有効・ロックの設定は戻り値を持たず、対象によっては無言で無視される。
            // 読み直しが可否の最終的な判定になる。
            let (summary, info) = attribute(
                &permit,
                &boundary,
                reread_effect(editor, &boundary, &object, effect.position()),
            )?;
            let applied = params.enabled.is_none_or(|enabled| info.enabled == enabled)
                && params.locked.is_none_or(|locked| info.locked == locked);
            if !applied {
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

    fn set_selection(&self, params: &SetSelectionParams) -> Result<SelectionState, EditError> {
        self.ensure_editable()?;
        let project = self.project.as_ref();

        let (epoch, revision, outcome) = self.edit_section(move |editor| {
            let focus_selector = match &params.focus {
                Some(FocusChange::Set { object }) => Some(object),
                _ => None,
            };
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                Some(params.expected_project_epoch.as_str()),
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
}

/// 変更 API の呼び出し結果を、発行後の失敗として印を付ける。
fn attribute<T>(
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
