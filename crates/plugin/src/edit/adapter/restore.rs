//! 書き込み検証が落ちたときの巻き戻し。

use super::effect_column::is_same_effect_column;
use super::reread_with_effects;
use crate::edit::error::{EditError, ItemRestore, UnsupportedReason};
use crate::edit::host::SceneEditor;
use crate::edit::precondition::{Boundary, MutationPermit};
use crate::edit::resolve::{ResolvedEffect, ResolvedObject, resolve_effect_of};
use crate::read::host::HostEffect;
use crate::read::resolve::effect_info_at;
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
pub(super) fn restore_after_failed_verification(
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
pub(super) fn restore_moved_effect<'sec>(
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
