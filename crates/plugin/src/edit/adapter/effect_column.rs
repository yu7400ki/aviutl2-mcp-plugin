//! effect の列の判定と読み直し。

use super::reread_with_effects;
use crate::edit::error::{EditError, EffectPreconditionReason};
use crate::edit::host::SceneEditor;
use crate::edit::precondition::Boundary;
use crate::edit::resolve::ResolvedObject;
use crate::read::host::HostEffect;
use crate::read::resolve::effect_info_at;
use aviutl2_mcp_core::{EffectInfo, ObjectSummary};

/// 移動先が、いま読み直した effect の列を指していることを確かめる。
///
/// 位置の値域は要求内容だけの検証で済んでいるため、ここで見るのは列の長さとの
/// 比較だけである。列の長さは対象の現在の状態であり、要求元の手元では確定しない。
///
/// ホストが範囲外の移動先を切り詰めるかどうかは問わない。切り詰めるなら要求と
/// 違う位置で成功を返すことになり、切り詰めないなら理由の読めない失敗になる。
pub(super) fn ensure_effect_position_in_range(
    count: usize,
    position: usize,
) -> Result<(), EditError> {
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
pub(super) fn is_same_effect(before: &HostEffect, after: &HostEffect) -> bool {
    before.name == after.name
        && before.enabled == after.enabled
        && before.locked == after.locked
        && before.items == after.items
}

/// 2 つの列が 1 件ずつ同じ effect を並べているかを判定する。
///
/// 同名 effect の順序は列の並びから決まるため、[`is_same_effect`] の材料が位置
/// ごとに一致すれば順序も一致する。
pub(super) fn is_same_effect_column(before: &[HostEffect], after: &[HostEffect]) -> bool {
    before.len() == after.len()
        && before
            .iter()
            .zip(after)
            .all(|(before, after)| is_same_effect(before, after))
}

/// 変更後の対象から、指定位置の effect 情報を読み直す。
pub(super) fn reread_effect(
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
pub(super) fn added_effect_position(before: &[String], after: &[String]) -> Option<usize> {
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
pub(super) fn effect_names(effects: &[HostEffect]) -> Vec<String> {
    effects.iter().map(|effect| effect.name.clone()).collect()
}
