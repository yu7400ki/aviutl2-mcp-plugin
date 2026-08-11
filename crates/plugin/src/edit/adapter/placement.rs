//! オブジェクトの配置に関わる前提条件と、作成された対象の走査。

use crate::edit::error::{EditError, OccupiedRange};
use crate::edit::host::SceneEditor;
use crate::read::host::HostObjectPlacement;
use std::ops::RangeInclusive;

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
pub(super) fn ensure_layer_unlocked(
    editor: &dyn SceneEditor,
    layer: usize,
) -> Result<(), EditError> {
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

/// 作成元が要求した配置の並びを、相対位置と配置先から求める。
///
/// 並びは渡された相対位置の並びであり、**先頭は必ず在る。** 宛先の事前確認は
/// その 1 件を見る。
///
/// 相対位置を持たない作成元——メディアファイルと effect 名、および構造を読め
/// なかったエイリアス——は、配置先そのものの 1 件になる。
///
/// 0 未満へ回った位置は 0 へ寄せる。
pub(super) fn requested_placements(
    relative: &[(i64, i64)],
    layer: usize,
    frame: usize,
) -> Vec<(usize, usize)> {
    if relative.is_empty() {
        return vec![(layer, frame)];
    }
    relative
        .iter()
        .map(|(relative_layer, relative_frame)| {
            (
                absolute(layer, *relative_layer),
                absolute(frame, *relative_frame),
            )
        })
        .collect()
}

/// 配置先へ相対値を加える。
fn absolute(base: usize, relative: i64) -> usize {
    let base = i64::try_from(base).unwrap_or(i64::MAX);
    usize::try_from(base.saturating_add(relative).max(0)).unwrap_or(usize::MAX)
}

/// 要求した配置と、実際に生まれた配置が 1 件でも違うか。
///
/// 比べるのは `(レイヤー, 開始フレーム)` の組だけであり、並び順を持たない
/// 多重集合として突き合わせる。件数が違えば真である。
pub(super) fn placement_adjusted(requested: &[(usize, usize)], created: &[(usize, usize)]) -> bool {
    if requested.len() != created.len() {
        return true;
    }
    let mut requested = requested.to_vec();
    requested.sort_unstable();
    let mut created = created.to_vec();
    created.sort_unstable();
    requested != created
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
pub(super) fn creation_scan_range(
    editor: &dyn SceneEditor,
    floor: usize,
) -> Result<RangeInclusive<usize>, EditError> {
    Ok(0..=editor.occupied_layer_max()?.max(floor))
}

/// 指定範囲のレイヤーからオブジェクトの位置を集める。
///
/// alias も effect も読まない。差分を取るのに要るのは位置だけである。
pub(super) fn scene_placements(
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
pub(super) fn created_placements(
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
