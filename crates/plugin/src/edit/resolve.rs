//! 編集区間の内側での対象解決。
//!
//! 解決済みトークン（[`ResolvedObject`] / [`ResolvedEffect`]）を生成できるのは
//! 本モジュールだけであり、生成の時点で fingerprint の照合が済んでいる。トークン
//! は編集区間の生存期間で縛られており、区間の外へは持ち出せない。
//!
//! オブジェクトの候補探索と fingerprint 照合は読み取り経路と同じ実装を呼ぶ。
//! 別実装で算出すると、読み取りが返した fingerprint と編集が照合する fingerprint
//! がずれ、一致するはずの対象を拒む経路が生まれる。

use crate::edit::error::EditError;
use crate::edit::host::{EffectSlot, ObjectSlot, SceneEditor};
use crate::edit::precondition::Boundary;
use crate::read::adapter::{
    effect_fingerprint_inputs, resolve_selected_detail, resolve_selected_object,
};
use crate::read::host::HostEffect;
use aviutl2_mcp_core::{EffectInfo, EffectSelector, ObjectSelector, ObjectSummary};
use std::marker::PhantomData;

/// 解決済みのオブジェクト。編集区間の内側でのみ存在する。
///
/// 生成できるのは本モジュールの解決処理だけであり、生成時点で fingerprint の
/// 照合が済んでいる。内部にはハンドルを表す識別子だけを封じ、外へは出さない。
pub struct ResolvedObject<'sec> {
    slot: ObjectSlot,
    summary: ObjectSummary,
    _section: PhantomData<&'sec ()>,
}

impl ResolvedObject<'_> {
    /// SDK 側が対象を指すための内部識別子。
    pub(crate) fn slot(&self) -> ObjectSlot {
        self.slot
    }

    /// 解決時に読み直した対象の概要。
    pub(crate) fn summary(&self) -> &ObjectSummary {
        &self.summary
    }

    /// 対象が属するレイヤー番号。
    pub(crate) fn layer(&self) -> usize {
        self.summary.layer
    }

    /// 対象の開始フレーム番号。
    pub(crate) fn frame_start(&self) -> usize {
        self.summary.frame_start
    }
}

/// 解決済みの effect。位置づけは [`ResolvedObject`] と同じ。
pub struct ResolvedEffect<'sec> {
    slot: EffectSlot,
    /// effect 列全体での位置。read-back で同じ要素を読み直すのに使う。
    position: usize,
    info: EffectInfo,
    _section: PhantomData<&'sec ()>,
}

impl ResolvedEffect<'_> {
    /// SDK 側が対象を指すための内部識別子。
    pub(crate) fn slot(&self) -> EffectSlot {
        self.slot
    }

    /// effect 列全体での 0 始まりの位置。
    pub(crate) fn position(&self) -> usize {
        self.position
    }

    /// 解決時に読み直した effect の情報。
    pub(crate) fn info(&self) -> &EffectInfo {
        &self.info
    }
}

/// セレクターが指すオブジェクトを解決する（判定 5〜6）。
///
/// 候補の探索と fingerprint の照合は読み取り経路と同一の実装を用いる。判定
/// 1〜4（epoch・シーン・算出方式）は呼び出し側が [`Boundary`] を得た時点で
/// 済んでおり、その [`Boundary`] を要求することで順序を型として要求している。
///
/// 配下 effect は読まない。オブジェクトの同一性は alias だけで決まるため、
/// effect を必要としない operation が effect の読み取り失敗に巻き込まれない。
pub(crate) fn resolve_object<'sec>(
    editor: &'sec dyn SceneEditor,
    boundary: &Boundary,
    selector: &ObjectSelector,
) -> Result<ResolvedObject<'sec>, EditError> {
    let (summary, _) = resolve_selected_object(
        editor.reader(),
        boundary.epoch(),
        boundary.scene_id(),
        selector,
    )?;
    bind(editor, summary)
}

/// セレクターが指すオブジェクトを、配下 effect の列とともに解決する。
///
/// 照合は [`resolve_object`] と同じ材料で行う。effect の列を必要とする
/// operation だけがこちらを使う。
pub(crate) fn resolve_object_with_effects<'sec>(
    editor: &'sec dyn SceneEditor,
    boundary: &Boundary,
    selector: &ObjectSelector,
) -> Result<(ResolvedObject<'sec>, Vec<HostEffect>), EditError> {
    let (summary, detail) = resolve_selected_detail(
        editor.reader(),
        boundary.epoch(),
        boundary.scene_id(),
        selector,
    )?;
    Ok((bind(editor, summary)?, detail.effects))
}

/// 照合済みの概要から解決済みトークンを作る。
fn bind<'sec>(
    editor: &'sec dyn SceneEditor,
    summary: ObjectSummary,
) -> Result<ResolvedObject<'sec>, EditError> {
    let slot = editor.bind_object(summary.layer, summary.frame_start)?;
    Ok(ResolvedObject {
        slot,
        summary,
        _section: PhantomData,
    })
}

/// セレクターが指す effect を解決する（判定 5〜6）。
///
/// オブジェクトと effect の**両方**の fingerprint を照合する。片方だけでは、
/// 別オブジェクトの同名 effect へ適用する誤りを防げない。
pub(crate) fn resolve_effect<'sec>(
    editor: &'sec dyn SceneEditor,
    boundary: &Boundary,
    selector: &EffectSelector,
) -> Result<(ResolvedObject<'sec>, ResolvedEffect<'sec>), EditError> {
    let (object, effects) = resolve_object_with_effects(editor, boundary, &selector.object)?;
    let effect = resolve_effect_of(editor, &object, &effects, selector)?;
    Ok((object, effect))
}

/// 解決済みオブジェクトの配下から effect を解決する。
pub(crate) fn resolve_effect_of<'sec>(
    editor: &'sec dyn SceneEditor,
    object: &ResolvedObject<'sec>,
    effects: &[HostEffect],
    selector: &EffectSelector,
) -> Result<ResolvedEffect<'sec>, EditError> {
    let position = find_effect_position(effects, &selector.effect_name, selector.effect_index)
        .ok_or_else(|| EditError::EffectNotFound {
            effect_name: selector.effect_name.clone(),
            effect_index: selector.effect_index,
        })?;
    let info =
        effect_info_at(&object.summary().selector, effects, position).ok_or(EditError::Sdk {
            operation: "get_effect_list",
        })?;
    if info.fingerprint != selector.fingerprint {
        // オブジェクト側の照合はここへ来る前に通っている。読み直すべきは
        // effect の一覧であり、オブジェクトの概要は要求元が既に持っている値と
        // 同じである。
        return Err(EditError::EffectFingerprintMismatch);
    }
    let slot = editor.bind_effect(object.slot(), position)?;
    Ok(ResolvedEffect {
        slot,
        position,
        info,
        _section: PhantomData,
    })
}

/// effect 名と同名内の順序から、effect 列全体での位置を求める。
///
/// 同名内の順序は読み取り経路と同じ採番規則に従う。ずれると同名 effect の
/// 別インスタンスを書き換える。
pub(crate) fn find_effect_position(
    effects: &[HostEffect],
    effect_name: &str,
    effect_index: usize,
) -> Option<usize> {
    effects
        .iter()
        .position(|effect| effect.name == effect_name && effect.index == effect_index)
}

/// effect 列の指定位置から effect の情報を組み立てる。
///
/// 材料には effect 列の絶対位置と総数が含まれるため、要素を単独では組み立て
/// られない。読み取り経路と同じ入力の組み立てを共有する。
pub(crate) fn effect_info_at(
    object: &ObjectSelector,
    effects: &[HostEffect],
    position: usize,
) -> Option<EffectInfo> {
    effect_fingerprint_inputs(effects)
        .nth(position)
        .map(|input| EffectInfo::new(object.clone(), input))
}
