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

use crate::alias::AdmittedAlias;
use crate::edit::EditAdapter;
use crate::edit::batch;
use crate::edit::error::{
    EditError, EffectPreconditionReason, ItemRestore, OccupiedRange, SectionPreconditionReason,
    UnsupportedReason,
};
use crate::edit::host::{EditHost, HostSelection, SceneEditor};
use crate::edit::precondition::{
    Boundary, EditKind, ExpectedEpoch, MutationPermit, MutationTicket, verify_boundary,
};
use crate::edit::resolve::{
    ResolvedEffect, ResolvedObject, resolve_effect, resolve_effect_of, resolve_object,
    resolve_object_with_effects,
};
use crate::project::ProjectState;
use crate::read::ReadError;
use crate::read::host::{EditState, HostEffect, HostLayer, HostObjectPlacement};
use crate::read::resolve::{effect_info_at, object_summary, scene_info};
use aviutl2_mcp_core::{
    AddEffectParams, ApplyBatchParams, AvailableEffectItem, BatchOperation, BatchOutcome,
    CreateObjectParams, CreateObjectSectionParams, Cursor, DeleteEffectParams, DeleteObjectParams,
    DeleteObjectSectionParams, DisplayRange, DisplayStart, EditOutcome, EffectInfo, EffectType,
    FocusChange, FrameRange, GridBpmOutcome, ItemValue, ItemWrite, ItemWriteError, LayerInfo,
    LayerStateOutcome, MoveEffectParams, MoveObjectParams, MoveObjectSectionParams, Movement,
    ObjectSectionsOutcome, ObjectSelector, ObjectSource, ObjectSummary, ObservedSelection,
    RangeChange, ReadBackCheck, SceneSettingsOutcome, SectionRange, SelectionField, SelectionState,
    SetEffectEnabledParams, SetGridBpmParams, SetLayerStateParams, SetObjectItemParams,
    SetObjectNameParams, SetSceneSettingsParams, SetSelectionParams, TrackWriteTarget,
    movement_check_reads_current_value, prepare_item_write, write_drops_existing_movement,
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
            state => Err(EditError::EditBlocked { state }),
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
    /// 落ちたのは編集であり、[`Self::ensure_editable`] が同じ状態で拒む失敗と
    /// 同じものを名乗る。両者の違いは、受付判定と確保の間に再生や出力が始まった
    /// 競合かどうかだけである。
    ///
    /// 区間へ入れなかった以上プロジェクトは変更されておらず、部分適用は生じない。
    fn classify_section_failure(&self, error: EditError) -> EditError {
        match self.edit_state() {
            Ok(EditState::Edit) | Err(_) => error,
            Ok(state) => EditError::EditBlocked { state },
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

    /// 名前で指定された登録済みエイリアスを、編集区間へ入る前に読み終える。
    ///
    /// ファイルの読み取りとパースはホストのメインスレッドを保持したまま行って
    /// よい仕事ではない。区間へ持ち込むのは読み取った生バイト列だけであり、
    /// 呼び出し側はファイルの中身を 1 度も見ない。
    ///
    /// 落ちた条件はそのまま失敗へ写す。一覧が除外に使う規則と同じ関数の同じ
    /// 戻り値であり、片方にだけ条件を足すことができない。
    ///
    /// データディレクトリを解決できないことだけは規則の外にある。要求そのものは
    /// 正しく、この AviUtl2 では機能が使えないことを述べている。判定の順序は
    /// 受け入れ規則の側が持つ——ここでホストへ先に問い合わせると、誤った名前へ
    /// 環境の失敗を返すことになる。
    fn admit_alias(&self, name: &str) -> Result<AdmittedAlias, EditError> {
        let data_dir = self.host.alias_data_directory();
        Ok(crate::alias::admit_alias_in(data_dir.as_deref(), name)?)
    }

    /// 生テキストで指定されたエイリアスの行を、編集区間へ入る前に検証する。
    ///
    /// パースも移動方法の一覧の解決も設定項目の一覧の引き当ても、ホストの
    /// メインスレッドを保持したまま行ってよい仕事ではない。区間へ持ち込むのは
    /// 受け取った文字列だけである。設定項目の一覧は登録済みプラグインが定める
    /// ものであり、区間の外で引いても対象が入れ替わらない。
    ///
    /// **名前で指定されたエイリアスには掛けない。** 一覧は本文の行を 1 つも
    /// 見ておらず、作成にだけ条件を足せば「一覧に出た名前は必ず作成できる」が
    /// 崩れる。人が書いたファイルが壊れていることと、その名前を一覧が出した
    /// うえで作成が拒むことは、要求元にとって別の困り方であり、後者は要求元の
    /// 側から直せない。
    ///
    /// 移動方法の一覧は設定項目を書く経路と同じ口から引く
    /// （[`EditHost::movements`]）。設定項目の一覧を引けない効果名は `None` へ
    /// 畳み、その節の行を通す。
    fn admit_alias_rows(&self, alias: &str) -> Result<(), EditError> {
        let movements = self.host.movements();
        Ok(crate::alias::admit_rows(
            alias,
            &movements,
            |effect_name| guard(|| self.host.effect_item_catalog(effect_name)).ok(),
        )?)
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

    /// 順序を動かせないと分かる対象を、編集区間へ入る前に弾く。
    ///
    /// 判定の位置づけは [`Self::ensure_effect_enabled_writable`] と同じである。
    /// 順序を動かせるのはフィルタ効果だけであり、**カタログの種別が既知で
    /// フィルタでない対象**をここで落とす。
    ///
    /// **カタログに無い effect と、種別が未知の effect は通す。** 我々がモデル化
    /// していない種別を我々の都合で拒まない。判定は発行後の読み直しが引き受ける。
    fn ensure_effect_movable(&self, effect_name: &str) -> Result<(), EditError> {
        let catalog = guard(|| self.host.effect_catalog())?;
        let Some(effect) = catalog.iter().find(|effect| effect.name == effect_name) else {
            return Ok(());
        };
        let known_non_filter = match effect.effect_type {
            EffectType::Filter | EffectType::Unknown(_) => false,
            EffectType::Input
            | EffectType::Transition
            | EffectType::Control
            | EffectType::Output => true,
        };
        if known_non_filter {
            return Err(EditError::UnsupportedTarget {
                reason: UnsupportedReason::EffectNotMovable,
            });
        }
        Ok(())
    }

    /// 中間点を変える 3 つの operation に共通する手順。
    ///
    /// 区間の読み直しを 1 回前倒しして事前確認へ使い、変更のあとにもう 1 度
    /// 読み直して応答へ載せる。追加の SDK 呼び出しはこの読み直しだけである。
    ///
    /// レイヤーのロックは 3 operation に共通するこの経路で 1 度だけ確かめる。
    /// 個別のメソッドへ書くと、operation を足したときに書き忘れる場所ができる。
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
            // ロックの判定は対象固有の事前確認より先に置く。逆順にすると、
            // ロックされたレイヤーへの誤った要求が事前確認の理由を名乗り、
            // 要求元は事前確認を直しては送り直す往復に入る。
            ensure_layer_unlocked(editor, object.layer())?;
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
/// 削除と、時間軸上の移動と、中間点の追加・移動・削除である。設定値の変更も
/// effect の増減も UI の設定パネルから行えるため、ここでは止めない。
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

/// 移動先が、いま読み直した effect の列を指していることを確かめる。
///
/// 位置の値域は要求内容だけの検証で済んでいるため、ここで見るのは列の長さとの
/// 比較だけである。列の長さは対象の現在の状態であり、要求元の手元では確定しない。
///
/// ホストが範囲外の移動先を切り詰めるかどうかは問わない。切り詰めるなら要求と
/// 違う位置で成功を返すことになり、切り詰めないなら理由の読めない失敗になる。
fn ensure_effect_position_in_range(count: usize, position: usize) -> Result<(), EditError> {
    if position < count {
        return Ok(());
    }
    Err(EditError::EffectPrecondition {
        reason: EffectPreconditionReason::PositionOutOfRange,
    })
}

/// 移動の前後で同じ effect を指しているかを判定する。
///
/// 比べるのは名前・有効・ロック・設定項目の値である。fingerprint は材料に列の
/// 位置と effect の総数を含むため、移動が成功すれば必ず変わる。同名 effect の
/// 順序も、同名の 1 件を動かせば入れ替わる。
///
/// **同じ材料を持つ effect が 2 つ並んでいる場合は区別できない。** 移動先に
/// 求めた状態が在るかを見る限り、区別する必要が無い——観測できる状態が同じで
/// あれば、要求した状態は達成されている。
fn is_same_effect(before: &HostEffect, after: &HostEffect) -> bool {
    before.name == after.name
        && before.enabled == after.enabled
        && before.locked == after.locked
        && before.items == after.items
}

/// 2 つの列が 1 件ずつ同じ effect を並べているかを判定する。
///
/// 同名 effect の順序は列の並びから決まるため、[`is_same_effect`] の材料が位置
/// ごとに一致すれば順序も一致する。
fn is_same_effect_column(before: &[HostEffect], after: &[HostEffect]) -> bool {
    before.len() == after.len()
        && before
            .iter()
            .zip(after)
            .all(|(before, after)| is_same_effect(before, after))
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

/// 書き込んだ設定値が要求どおり入ったことを、読み直して確かめる。
///
/// 照合した種別では、そのとき読み直した対象を [`Some`] で返す。照合しない種別は
/// 読み直しを行わず [`None`] を返す。
///
/// SDK の書き込みは成否を返さない。値域を外れた数値は切り詰められ、小数は項目の
/// 桁へ丸められ、書式の合わない色は既定値へ落ち、未登録のフォント名と選択肢に
/// 無い値は黙って捨てられる。読み直さない限り、いずれも「書けたのに要求した値が
/// 入っていない」状態を成功として報告してしまう。
///
/// 比べるのは **SDK へ渡した文字列と読み直した文字列**である。書き込みの前後を
/// 比べても、同じ値を書いた場合と値が無視された場合はどちらも前後が等しくなり、
/// 区別できない。比較の規則は種別ごとに違い、[`ItemWrite`] が持つ。
///
/// **設定値を読む前に、対象を 1 度読み直す。** ホストは受理した綴りを後から
/// 解釈し直して保存値を差し替えることがあり、書き込みの直後に設定値だけを読むと
/// 差し替わる前の綴りが返る。差し替えは対象の読み直しを経た後の設定値に現れる。
///
/// **読み直した対象を呼び出し側へ渡すのは、応答の材料が同じものだからである。**
/// 照合を通った時点で対象は動いていない。同じ状態を 2 度読まない——効果を多く
/// 持つ対象では、詳細の 1 回が SDK 呼び出しの数十回になる。
///
/// **読み直す位置は解決済みトークンから引き直す。** 解決の時点の位置は先行する
/// sub-operation の移動で古くなり得る。トークンは移動で失効しないため、位置は
/// そこから読める。
///
/// 費用は sub-operation 1 件あたり「位置の読み 1 回 ＋ 対象の読み直し 1 回 ＋
/// 設定値の読み 1 回」である。**照合しない種別はどれも行わない。** 費用は照合の
/// 有無と一致する。
///
/// 単独の変更と一括適用が同じ入力に対して同じ失敗を返すよう、判定はこの 1 か所
/// だけに置く。
pub(crate) fn verify_written_item(
    editor: &dyn SceneEditor,
    permit: &MutationPermit<'_>,
    boundary: &Boundary,
    object: &ResolvedObject<'_>,
    effect: &ResolvedEffect<'_>,
    item: &str,
    write: &ItemWrite,
) -> Result<Option<(ObjectSummary, Vec<HostEffect>)>, EditError> {
    let ReadBackCheck::Compare(_) = write.read_back() else {
        return Ok(None);
    };
    let position = attribute(permit, boundary, editor.object_position(object))?;
    let reread = attribute(
        permit,
        boundary,
        reread_with_effects(editor, boundary, position.layer, position.frame_start),
    )?;
    let observed = attribute(permit, boundary, editor.effect_item_value(effect, item))?;
    // 照合しない種別はこの位置へ来ない。来たとしても `None` は一致を名乗らない。
    if write.read_back_matches(&observed) == Some(true) {
        return Ok(Some(reread));
    }
    // 巻き戻しはこの関数の外で行う。一括適用も同じ判定を通るが、あちらは要求
    // 全体の巻き戻し計画を別に持つ。
    Err(permit.attribute(boundary, EditError::ItemValueNotApplied { observed }))
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

/// 移動を書き込む対象の性質を、いま読み直して組み立てる。
///
/// 区間の数は [`SceneEditor::object_sections`] が返す並びの要素数である。
/// `get_object` が `sections` として返すのと同じ経路であり、要求元が読んだ
/// 区間の数がそのまま「値の個数 - 1」になる。
///
/// 移動方法の一覧は区間の外で引き終えたものを受け取る
/// （[`EditHost::movements`]）。プロジェクトの内容に連動しないため、区間の
/// 内側で引く理由が無い。
///
/// **値の形で呼び分けない。** 移動を含まない値では中身が参照されないが、
/// 呼び分けを置くと、その判定を誤った経路が検証を通らずに符号化へ届く。
/// 一覧に無い移動方法はホストのプロセスを落とすため、迂回できる経路を作らない。
pub(crate) fn track_write_target(
    editor: &dyn SceneEditor,
    object: &ResolvedObject<'_>,
    movements: &[Movement],
) -> Result<TrackTarget, EditError> {
    Ok(TrackTarget {
        section_count: editor.object_sections(object)?.len(),
        movements: movements.to_vec(),
    })
}

/// 読み直した対象の性質を所有する形。
///
/// [`TrackWriteTarget`] は一覧を借用するため、読み直した値をそのまま返せない。
/// 所有する側をここに置き、借用は呼び出し側の局所で作る。
pub(crate) struct TrackTarget {
    section_count: usize,
    movements: Vec<Movement>,
}

impl TrackTarget {
    /// 書き込みの検証へ渡す形を借用として作る。
    pub(crate) fn as_write_target(&self) -> TrackWriteTarget<'_> {
        TrackWriteTarget {
            section_count: self.section_count,
            movements: &self.movements,
        }
    }
}

/// 書き込みが対象の移動を消さないことを確かめる。
///
/// 判定そのものは [`write_drops_existing_movement`] が持つ。単独の変更と一括適用は
/// どちらもこの 1 か所を通り、同じ入力に対して同じ失敗を返す。
///
/// **移動を書く要求は拒まない。** 対象がいま移動を持つかによらず通り、持たない
/// 項目には新しく移動が付く。移動を持ち得ない種別への移動は、種別と値の形の
/// 照合が先に拒む（[`prepare_item_write`]）。
///
/// `current` はホストが返した生の文字列である。**要求元が与えた値ではない。**
/// 対象がいまどの移動を持つかはこの文字列にしか現れず、応答へ載せるのも同じ
/// 値である。
///
/// 一覧に無い項目はここへ来ない。書き込む文字列の組み立てが先に失敗するため
/// である（[`prepare_item_write`]）。
pub(crate) fn ensure_movement_write(
    items: &[AvailableEffectItem],
    item: &str,
    value: &ItemValue,
    current: &str,
) -> Result<(), EditError> {
    let Some(entry) = items.iter().find(|entry| entry.name == item) else {
        return Ok(());
    };
    match write_drops_existing_movement(&entry.item_type, value, current) {
        true => Err(EditError::MovementWouldBeLost {
            current_value: current.to_string(),
        }),
        false => Ok(()),
    }
}

/// 書き込みの前に読んだ生文字列で [`ensure_movement_write`] を掛ける。
///
/// **材料が手元に無いのに判定が要る組み合わせは失敗させる。** 読む条件
/// （[`ReadBackCheck::Compare`]）と判定が要る条件
/// （[`movement_check_reads_current_value`]）は別の述語である。今日は前者が
/// 後者を含むが、片方だけを変えれば包含は破れる。そのとき「読んでいないから
/// 判定しない」と倒すと、移動を黙って消す書き込みが素通りする。**到達不能で
/// あることを根拠に分岐を消さず、到達したら落ちる形にする。**
///
/// 材料が無く判定も要らない組み合わせは通す。判定の対象そのものが無い。
fn ensure_movement_write_with_origin(
    items: &[AvailableEffectItem],
    item: &str,
    value: &ItemValue,
    origin: Option<&str>,
) -> Result<(), EditError> {
    if let Some(current) = origin {
        return ensure_movement_write(items, item, value, current);
    }
    let required = items
        .iter()
        .find(|entry| entry.name == item)
        .is_some_and(|entry| movement_check_reads_current_value(&entry.item_type, value));
    match required {
        true => Err(EditError::UnsupportedTarget {
            reason: UnsupportedReason::InverseUnavailable,
        }),
        false => Ok(()),
    }
}

/// 書き込みを発行する前に、対象がいま持つ生文字列を 1 回だけ読む。
///
/// **読むのは照合する種別だけである**（[`ReadBackCheck::Compare`]）。この値の
/// 用途は 2 つあり、どちらも照合する種別でしか要らない。
///
/// - **巻き戻しの材料。** 書き込み検証が落ちたときに書き戻す文字列である。
///   照合しない種別は検証が落ちる契機を持たないため、戻す場面が生じない
/// - **移動の事前確認**（[`ensure_movement_write`]）。移動を持ち得る種別は
///   いずれも照合する側にある
///
/// [`verify_written_item`] は「費用は照合の有無と一致する」と宣言している。
/// 書き込みの前の読み取りも同じ規則へ揃える。**現に到達し得る書き込みは
/// すべて照合する**——書き込みを公開していない種別は書き込む文字列の組み立てが
/// 先に拒み、[`ReadBackCheck::Declared`] を持つ [`ItemWrite`] は生まれない。
/// 狭めても実際に読まなくなる書き込みは無く、宣言だけが保たれる。
///
/// **移動の事前確認と巻き戻しの材料は同じ値である。** 対象がいまどの移動を
/// 持つかも、書き戻す文字列も、ホストが返すこの 1 つの文字列にしか現れない。
/// 2 度読まない。
///
/// **符号化を挟まない。** [`ItemValue`] へ解釈してから書き戻す形にすると、
/// 解釈できない値を戻せなくなる。ホスト自身が直前に返した文字列である以上、
/// 書式も移動方法の名前も必ず妥当であり、不正な移動方法名でホストを落とす
/// 経路へも入らない。
fn read_before_write(
    editor: &dyn SceneEditor,
    effect: &ResolvedEffect<'_>,
    item: &str,
    write: &ItemWrite,
) -> Result<Option<String>, EditError> {
    let ReadBackCheck::Compare(_) = write.read_back() else {
        return Ok(None);
    };
    editor.effect_item_value(effect, item).map(Some)
}

/// 書き込み検証が落ちたとき、対象を書き込み前の値へ戻してから失敗を返す。
///
/// **[`verify_written_item`] の内側へは入れない。** 一括適用も同じ関数を通るが、
/// あちらは要求全体の巻き戻し計画を別に持ち、逆操作の材料も戻す順序もそちらが
/// 握っている。sub-operation ごとにここで戻せば、同じ書き込みを 2 度戻すことに
/// なる。
///
/// 戻す書き込みを発行するかは、**書き込み前の値と読み直した値の比較**で決まる。
/// 同じならホストは何も変えておらず（選択肢に無い値・未登録のフォント名・書式の
/// 合わない色がこれにあたる）、戻すものが無い。**階級に名前は与えない**——
/// 要求元が取る行動はどちらでも変わらず、変わるのは我々が発行する書き込みの数
/// だけである。
///
/// **読み直しそのものが落ちた場合も戻しに行く。** 比較する材料は無いが、書き
/// 戻す材料は手元にある。適用されたかが分からない以上、戻さずに残す理由が無い。
/// 戻せたことを確かめられなければ [`ItemRestore::Failed`] であり、確かめられ
/// ないまま「戻せた」と名乗らない。
fn restore_after_failed_verification(
    editor: &dyn SceneEditor,
    permit: &MutationPermit<'_>,
    boundary: &Boundary,
    effect: &ResolvedEffect<'_>,
    item: &str,
    origin: Option<&str>,
    error: EditError,
) -> EditError {
    // 照合しない種別はここへ来ない。検証が落ちる契機を持たず、書き込みの前の
    // 読み取りも行っていない。
    let Some(origin) = origin else {
        return error;
    };
    let restore = match error.observed_item_value() {
        // ホストは値を動かしていない。戻すものが無い。
        Some(observed) if observed == origin => ItemRestore::Restored,
        _ => restore_item_value(editor, permit, boundary, effect, item, origin),
    };
    error.with_item_restore(restore)
}

/// 書き込み前の生文字列を書き戻し、戻せたことを読み直して確かめる。
///
/// **発行は同じ [`MutationPermit`] で行う。** 最初の発行で確定した revision が
/// そのまま応答へ載るため、巻き戻しを挟んでも 1 要求が進める revision は高々 1
/// である。
///
/// **「書き込み API が真を返した」を成功と読まない。** ホストは書き込みの成否を
/// 返さず、返った真は要求した値が入ったことを示さない。読み直して元の文字列と
/// 一致することだけが、戻せたことの根拠である。
fn restore_item_value(
    editor: &dyn SceneEditor,
    permit: &MutationPermit<'_>,
    boundary: &Boundary,
    effect: &ResolvedEffect<'_>,
    item: &str,
    origin: &str,
) -> ItemRestore {
    let outcome = permit
        .issue(boundary, |ticket| {
            editor.set_effect_item(ticket, effect, item, origin)
        })
        .and_then(|()| editor.effect_item_value(effect, item))
        .and_then(|current| match current == origin {
            true => Ok(()),
            // 書き込みも読み直しも通ったのに値が戻っていない。ホストが黙って
            // 捨てた場合がこれである。
            false => Err(EditError::UnsupportedTarget {
                reason: UnsupportedReason::ChangeNotApplied,
            }),
        });
    let Err(error) = outcome else {
        return ItemRestore::Restored;
    };
    tracing::warn!(
        item,
        code = %error.error_code().as_snake_case(),
        "書き込み検証に落ちた設定値を元へ戻せませんでした"
    );
    ItemRestore::Failed
}

/// 要求と違う位置へ動いた effect を、移動前の位置へ戻す。
///
/// **発行は同じ [`MutationPermit`] で行う。** 最初の発行で確定した revision が
/// そのまま応答へ載るため、巻き戻しを挟んでも 1 要求が進める revision は高々 1
/// である。
///
/// 戻す先は移動前の位置である。その effect が現に居た位置であり、受け付けられる
/// 移動先であることを、居たという事実が示している。
///
/// **戻せたことの根拠は列全体の一致である。** 移動は 1 件を抜いて別の位置へ
/// 挿し込むため、間に在った effect もすべてずれる。移動前の位置に居ることだけ
/// では並びが戻ったことを示さない。
fn restore_moved_effect<'sec>(
    editor: &'sec dyn SceneEditor,
    permit: &MutationPermit<'_>,
    boundary: &Boundary,
    object: &ResolvedObject<'sec>,
    before: &[HostEffect],
    observed: &[HostEffect],
    from: usize,
) -> ItemRestore {
    let outcome = restore_target(before, observed, from)
        .and_then(|position| effect_info_at(&object.summary().selector, observed, position))
        .ok_or(EditError::UnsupportedTarget {
            reason: UnsupportedReason::ChangeNotApplied,
        })
        .and_then(|info| resolve_effect_of(editor, object, observed, &info.selector))
        .and_then(|target| {
            permit.issue(boundary, |ticket| {
                editor.move_effect(ticket, object, &target, from)
            })
        })
        .and_then(|_| reread_with_effects(editor, boundary, object.layer(), object.frame_start()))
        .and_then(
            |(_, restored)| match is_same_effect_column(before, &restored) {
                true => Ok(()),
                // 戻す移動が効かなかった場合も、動いたのに並びが揃わない場合も
                // ここへ来る。
                false => Err(EditError::UnsupportedTarget {
                    reason: UnsupportedReason::ChangeNotApplied,
                }),
            },
        );
    let Err(error) = outcome else {
        return ItemRestore::Restored;
    };
    tracing::warn!(
        code = %error.error_code().as_snake_case(),
        "要求と違う位置へ動いた effect を移動前の位置へ戻せませんでした"
    );
    ItemRestore::Failed
}

/// 読み直した列のうち、戻せば移動前の並びになる 1 件の位置を求める。
///
/// **先頭から一致を採ってはならない。** [`is_same_effect`] の材料は同じ設定の
/// effect を区別しないため、動いていない方を掴み得る。抜いて `from` へ挿した
/// 列が移動前と一致するものだけが、戻せる 1 件である。
///
/// 条件を満たす位置が複数あっても、戻した列はいずれも移動前と一致する。
fn restore_target(before: &[HostEffect], observed: &[HostEffect], from: usize) -> Option<usize> {
    (0..observed.len()).find(|&position| {
        let mut candidate = observed.to_vec();
        let moved = candidate.remove(position);
        candidate.insert(from, moved);
        is_same_effect_column(before, &candidate)
    })
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

/// 編集区間の内側で呼ぶ作成 API と、その引数。
///
/// 区間の外で答えが出る仕事は区間の外で終える。名前で指定されたエイリアスは
/// 区間へ入る前に生バイト列となり、生テキストの作成元とまったく同じ腕へ入る
/// ——SDK から見れば 2 つは同じ要求であり、区別を区間の内側へ持ち込む理由が無い。
#[derive(Debug, Clone, Copy)]
enum ResolvedSource<'a> {
    /// メディアファイルの絶対パス。
    MediaFile(&'a str),
    /// エイリアスの生バイト列。
    Alias(&'a str),
    /// 登録済み effect の名前。
    Effect(&'a str),
}

impl<H: EditHost> EditAdapter for HostEditAdapter<H> {
    fn create_object(&self, params: &CreateObjectParams) -> Result<EditOutcome, EditError> {
        self.ensure_editable()?;
        // 作成元が effect 名の場合だけ、付与と同じ経路でカタログを確かめる。
        // 種別による絞り込みは行わない。どの effect が作成の元になれるかは
        // SDK が述べておらず、絞れば実際に作れる effect を拒み得る。
        if let ObjectSource::Effect { name } = &params.source {
            self.ensure_effect_registered(name)?;
        }
        // ファイルの読み取りとパースは区間の外で終える。区間の内側でしか答えの
        // 出ない検査だけを内側へ残し、外で決まる検査は前提条件の照合よりも前に
        // 返る。
        let admitted;
        let source = match &params.source {
            ObjectSource::MediaFile { path } => ResolvedSource::MediaFile(path),
            ObjectSource::ObjectAlias { alias } => {
                self.admit_alias_rows(alias)?;
                ResolvedSource::Alias(alias)
            }
            ObjectSource::Effect { name } => ResolvedSource::Effect(name),
            ObjectSource::AliasName { name } => {
                admitted = self.admit_alias(name)?;
                ResolvedSource::Alias(&admitted.raw)
            }
        };
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
            if let ResolvedSource::MediaFile(path) = source
                && !editor.supports_media_file(path)?
            {
                return Err(EditError::UnsupportedTarget {
                    reason: UnsupportedReason::MediaNotSupported,
                });
            }

            let permit = boundary.issue_permit(project)?;
            permit.issue(&boundary, |ticket| match source {
                ResolvedSource::MediaFile(path) => {
                    editor.create_object_from_media_file(ticket, path, layer, frame)
                }
                ResolvedSource::Alias(alias) => {
                    editor.create_object_from_alias(ticket, alias, layer, frame)
                }
                ResolvedSource::Effect(name) => {
                    editor.create_object_from_effect(ticket, name, layer, frame)
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
        let movements = self.host.movements();

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
            let target = track_write_target(editor, &object, &movements)?;
            let write = match prepare_item_write(
                &items,
                &params.item,
                &params.value,
                target.as_write_target(),
            ) {
                Ok(write) => write,
                Err(ItemWriteError::ItemNotFound { item }) => {
                    return Err(unlisted_item(editor, &effect, &item));
                }
                Err(error) => return Err(EditError::ItemWrite(error)),
            };
            // 巻き戻しの材料を、書き込みを発行する前に読む。
            let origin_value = read_before_write(editor, &effect, &params.item, &write)?;
            // 書き込みを発行する前に確かめる。発行してしまえば、消えた移動を
            // 復元する手段がこちら側に無い。**事前に断れる失敗を事後の巻き戻し
            // へ落とさない**——巻き戻しは失敗し得るが、発行しないことは失敗
            // しない。
            ensure_movement_write_with_origin(
                &items,
                &params.item,
                &params.value,
                origin_value.as_deref(),
            )?;

            let permit = boundary.issue_permit(project)?;
            permit.issue(&boundary, |ticket| {
                editor.set_effect_item(ticket, &effect, &params.item, write.value())
            })?;
            let verified = match verify_written_item(
                editor,
                &permit,
                &boundary,
                &object,
                &effect,
                &params.item,
                &write,
            ) {
                Ok(verified) => verified,
                Err(error) => {
                    return Err(restore_after_failed_verification(
                        editor,
                        &permit,
                        &boundary,
                        &effect,
                        &params.item,
                        origin_value.as_deref(),
                        error,
                    ));
                }
            };

            // 照合を通った値を、種別に応じた形へ解釈し直して応答へ載せる。表記は
            // ホストが整えたものであり、要求した値そのものである。読み直しは
            // 照合が済ませており、対象はそれから動いていない。
            let (summary, effects) = match verified {
                Some(reread) => reread,
                // 照合しない種別は読み直していない。応答は対象を要する。
                None => attribute(
                    &permit,
                    &boundary,
                    reread_with_effects(editor, &boundary, object.layer(), object.frame_start()),
                )?,
            };
            let info = effect_info_at(&summary.selector, &effects, effect.position()).ok_or_else(
                || {
                    permit.attribute(
                        &boundary,
                        EditError::Sdk {
                            operation: "get_effect_list",
                        },
                    )
                },
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

    fn move_effect(&self, params: &MoveEffectParams) -> Result<EditOutcome, EditError> {
        self.ensure_editable()?;
        self.ensure_effect_movable(&params.selector.effect_name)?;
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
            // 移動先が列に収まるかを見るため、対象と一緒に列の長さを得る。列は
            // 巻き戻しの照合にも使う。
            let (object, before) =
                resolve_object_with_effects(editor, &boundary, &params.selector.object)?;
            let effect = resolve_effect_of(editor, &object, &before, &params.selector)?;
            ensure_effect_position_in_range(before.len(), params.position)?;
            // 移動で変わらない材料は、解決した列の 1 件がそのまま持つ。列の位置も
            // 同名内の順序も動くため、fingerprint は同一性の材料にならない。
            let from = effect.position();
            let moved = &before[from];

            let permit = boundary.issue_permit(project)?;
            // 移動先が現在位置と同じでも短絡しない。成功を我々が決めることに
            // なり、他の編集と性質が変わる。
            let reported_position = permit.issue(&boundary, |ticket| {
                editor.move_effect(ticket, &object, &effect, params.position)
            })?;

            let (summary, effects) = attribute(
                &permit,
                &boundary,
                reread_with_effects(editor, &boundary, object.layer(), object.frame_start()),
            )?;
            let not_applied = || {
                permit.attribute(
                    &boundary,
                    EditError::EffectMoveNotApplied { reported_position },
                )
            };
            // 移動は effect を増やしも減らしもしない。長さが変わった列は 1 度の
            // 移動では移動前の並びへ戻せない。
            if effects.len() != before.len() {
                return Err(not_applied().with_item_restore(ItemRestore::Failed));
            }
            let arrived = effects
                .get(params.position)
                .is_some_and(|current| is_same_effect(moved, current));
            let Some(info) =
                effect_info_at(&summary.selector, &effects, params.position).filter(|_| arrived)
            else {
                // 列が移動前と 1 件ずつ一致すれば、ホストは何も動かしておらず
                // 戻すものが無い。同じ設定の effect が並ぶ列では、ずれ込んだ
                // 1 件が移動前の位置に座るため、その 1 件だけでは判定できない。
                //
                // 戻す移動は発行しないが、列は書き込み前の並びを持つ。要求元から
                // 見た状態は戻した場合と区別がつかない。
                if is_same_effect_column(&before, &effects) {
                    return Err(not_applied().with_item_restore(ItemRestore::Restored));
                }
                // 要求と違う位置へ動いている。列を動かしたまま失敗を返すと、
                // 要求元の selector も一緒に無効になる。
                let restore = restore_moved_effect(
                    editor, &permit, &boundary, &object, &before, &effects, from,
                );
                return Err(not_applied().with_item_restore(restore));
            };
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

    fn set_grid_bpm(&self, params: &SetGridBpmParams) -> Result<GridBpmOutcome, EditError> {
        self.ensure_editable()?;
        let project = self.project.as_ref();
        let entries = params.entries.as_slice();

        self.edit_section(move |editor| {
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                ExpectedEpoch::Only(params.expected_project_epoch.as_str()),
                EditKind::Content,
                &[params.expected_scene_id],
                &[],
            )?;

            let permit = boundary.issue_permit(project)?;
            permit.issue(&boundary, |ticket| {
                editor.set_grid_bpm_list(ticket, entries)
            })?;

            // 置き換えの API は戻り値を持たない。同一区間内で読み直す。
            let applied = attribute(&permit, &boundary, editor.reader().grid_bpm())?;
            // **照合するのは件数だけである。** ホストは単精度で受け取るため
            // 値は丸められ、順序も要求したものではない。どちらも正常な正規化で
            // あり、失敗と診断してはならない。「送ったのに入っていない」は
            // 正規化では説明できない。
            if applied.len() != entries.len() {
                return Err(permit.attribute(
                    &boundary,
                    EditError::UnsupportedTarget {
                        reason: UnsupportedReason::ChangeNotApplied,
                    },
                ));
            }
            Ok(GridBpmOutcome {
                project_epoch: boundary.epoch().to_string(),
                project_revision: permit.project_revision(&boundary),
                entries: applied,
            })
        })
    }

    fn set_scene_settings(
        &self,
        params: &SetSceneSettingsParams,
    ) -> Result<SceneSettingsOutcome, EditError> {
        self.ensure_editable()?;
        let project = self.project.as_ref();
        let name = params.name.as_deref();
        let size = params
            .size
            .as_ref()
            .map(|size| (index(size.width), index(size.height)));
        let sample_rate = params.sample_rate.map(index);

        let (epoch, revision, issued) = self.edit_section(move |editor| {
            let boundary = verify_boundary(
                project,
                editor.entry_edit_info(),
                ExpectedEpoch::Only(params.expected_project_epoch.as_str()),
                EditKind::Content,
                &[params.expected_scene_id],
                &[],
            )?;

            let permit = boundary.issue_permit(project)?;
            // 発行したかどうかは区間の内側でしか分からない。区間を抜けた後の
            // 失敗に「変更は発行済み」の印を付けるのは、実際に発行したときだけ
            // である。要求の軸から推し量ると、1 つも発行していない要求でも印が
            // 付く。
            let mut issued = false;
            // 確かめられる軸を先に出す。名前の照合は区間の内側で完結するため、
            // 反映されていなければ残る 2 つを 1 つも発行せずに戻れる。逆順に
            // すると、取り消せない変更が「失敗」の応答とともに残る。
            if let Some(name) = name {
                permit.issue(&boundary, |ticket| editor.set_scene_name(ticket, name))?;
                issued = true;
                if editor.reader().scene_name().as_deref() != Some(name) {
                    return Err(permit.attribute(
                        &boundary,
                        EditError::UnsupportedTarget {
                            reason: UnsupportedReason::ChangeNotApplied,
                        },
                    ));
                }
            }
            if let Some((width, height)) = size {
                permit.issue(&boundary, |ticket| {
                    editor.set_scene_size(ticket, width, height)
                })?;
                issued = true;
            }
            if let Some(sample_rate) = sample_rate {
                permit.issue(&boundary, |ticket| {
                    editor.set_scene_sample_rate(ticket, sample_rate)
                })?;
                issued = true;
            }
            Ok((
                boundary.epoch().to_string(),
                permit.project_revision(&boundary),
                issued,
            ))
        })?;

        // 解像度とサンプリングレートの反映値は編集情報にしか現れず、区間が持つ
        // 編集情報は入口の複製である。したがって区間を抜けてから観測する。
        // 観測に失敗した時点で発行済みの変更は取り消せないため、発行の印を
        // 付けて返す。
        let observed = guard(|| self.host.observed_scene()).map_err(|error| match issued {
            true => error.after_mutation(revision),
            false => error,
        })?;
        // 要求値と観測値の差異は失敗にしない。ホストが値を調整し得るうえ、区間を
        // 抜けてから観測するまでの間に UI 操作が入り得る。差異を失敗にすると、
        // 成功した変更を失敗として報告する経路ができる。
        Ok(SceneSettingsOutcome {
            project_epoch: epoch,
            project_revision: revision,
            scene: scene_info(&observed.info, observed.name),
            observed_after_edit: true,
            non_undoable: true,
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
        Ok(selection_state(
            epoch,
            revision,
            observed,
            outcome,
            params.display.as_ref(),
        ))
    }

    fn apply_batch(&self, params: &ApplyBatchParams) -> Result<BatchOutcome, EditError> {
        self.ensure_editable()?;
        let project = self.project.as_ref();
        let movements = self.host.movements();

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
            batch::apply_batch(editor, project, &boundary, &params.operations, &movements)
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
/// ここが持つのは**変更 API を呼んだ結果だけ**であり、応答の `applied` とは
/// 別である。要求どおりに反映されたかまで見る軸があるため、応答の一覧は
/// [`selection_state`] が観測と突き合わせてから組み立てる。
struct SelectionOutcome {
    /// 変更を要求された項目。応答での並び順を決める。
    requested: Vec<SelectionField>,
    /// 変更 API の呼び出しが成功した項目。
    succeeded: Vec<SelectionField>,
}

/// カーソル・選択範囲・表示開始位置・フォーカスを固定の順序で適用する。
///
/// 順序を固定するのは、途中で失敗したときの状態を一意にするためである。
/// フォーカスはどのみち区間の処理の最後に適用されるため、この順序は SDK の
/// 挙動とも整合する。
///
/// 表示開始位置をフォーカスより前に置くのは、この 2 つだけが同じ値を動かし得る
/// ためである。フォーカスの設定は区間の処理が終わってから反映され、対象を
/// 見せるために表示位置を動かす余地がある。表示開始位置を後に置いても、区間の
/// 内側での順序はフォーカスの反映時点を追い越せず、要求を上書きから守れない。
/// 逆順にしてもホストの挙動は変えられないので、区間の内側で決着する軸を先に
/// 済ませる。
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
    let mut succeeded = Vec::new();
    let mut failure = None;

    if let Some(cursor) = &params.cursor {
        requested.push(SelectionField::Cursor);
        let layer = index(cursor.layer);
        let frame = index(cursor.frame);
        match permit.issue(boundary, |ticket| editor.set_cursor(ticket, layer, frame)) {
            Ok(()) => succeeded.push(SelectionField::Cursor),
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
                Ok(()) => succeeded.push(SelectionField::SelectedRange),
                Err(error) => failure = Some(error),
            }
        }
    }
    if let Some(display) = &params.display {
        requested.push(SelectionField::Display);
        let layer = index(display.layer);
        let frame = index(display.frame);
        if failure.is_none() {
            match permit.issue(boundary, |ticket| {
                editor.set_display_start(ticket, layer, frame)
            }) {
                Ok(()) => succeeded.push(SelectionField::Display),
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
                Ok(()) => succeeded.push(SelectionField::Focus),
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
    SelectionOutcome {
        requested,
        succeeded,
    }
}

/// 表示開始位置が要求どおりに反映されたか。
///
/// 見るのは開始位置だけである。表示フレーム数・表示レイヤー数は厳密な値では
/// ないと編集情報の側が断っており、成否の判定に使えない。
fn display_start_applied(observed: &DisplayRange, requested: &DisplayStart) -> bool {
    observed.frame_start == index(requested.frame) && observed.layer_start == index(requested.layer)
}

/// 軸ごとに、何をもって「適用できた」と呼ぶかを決める。
///
/// [`SelectionField`] に対する網羅 `match` であり `_` を使わない。**軸を足すと
/// ここが落ち、その軸の判定を決めるまでコンパイルできない。**
///
/// **判定が軸によって違うのは、反映値を伝える手段が違うためである。** カーソル・
/// 選択範囲・フォーカスは反映値そのものが応答に載るため、範囲へ丸められたか
/// どうかは要求元が応答を読めば分かる。ここで観測一致まで求めると、範囲外を
/// 送るたびに `not_applied` が立ち、一覧が「何かが失敗した」の合図として
/// 読めなくなる。
///
/// 表示開始位置だけは反映値から判定できない。応答が運ぶ [`DisplayRange`] は
/// 開始位置以外が概数であり、要求どおりかを要求元が決められない。判定できない
/// この 1 軸だけを、観測と突き合わせてから伝える。
fn selection_applied(
    field: SelectionField,
    succeeded: bool,
    observed: &HostSelection,
    display: Option<&DisplayStart>,
) -> bool {
    match field {
        SelectionField::Cursor | SelectionField::SelectedRange | SelectionField::Focus => succeeded,
        SelectionField::Display => {
            succeeded
                && display
                    .is_some_and(|requested| display_start_applied(&observed.display, requested))
        }
    }
}

/// 観測した選択状態から応答を組み立てる。
///
/// 要求した軸は [`selection_applied`] の判定に従って `applied` と `not_applied`
/// のどちらか一方へ入る。どちらにも入らない軸は無い。
fn selection_state(
    epoch: String,
    revision: u64,
    observed: HostSelection,
    outcome: SelectionOutcome,
    display: Option<&DisplayStart>,
) -> SelectionState {
    let focus = observed
        .focus
        .as_ref()
        .map(|object| object_summary(&epoch, observed.scene_id, object));
    let applied: Vec<SelectionField> = outcome
        .requested
        .iter()
        .copied()
        .filter(|field| {
            selection_applied(
                *field,
                outcome.succeeded.contains(field),
                &observed,
                display,
            )
        })
        .collect();
    let not_applied = outcome
        .requested
        .into_iter()
        .filter(|field| !applied.contains(field))
        .collect();
    SelectionState::observed(
        epoch,
        revision,
        ObservedSelection {
            cursor: Cursor {
                frame: observed.cursor.frame,
                layer: observed.cursor.layer,
            },
            selected_range: observed.selected_range,
            focus,
            display: observed.display,
        },
        applied,
        not_applied,
    )
}

#[cfg(test)]
pub(crate) mod tests;
