//! 読み取りと編集が共有する解決。
//!
//! セレクターが指す対象の絞り込みと fingerprint の照合、effect 列での位置の
//! 特定、応答へ載せる概要の組み立てを集める。読み取り operation と編集
//! operation の双方がこの 1 つの実装を通ることで、同じ材料から同じ判定と同じ
//! fingerprint が得られる。SDK の型は現れない。

use crate::read::error::ReadError;
use crate::read::host::{
    HostEditInfo, HostEffect, HostObject, HostObjectDetail, HostObjectPlacement, SceneReader,
};
use aviutl2_mcp_core::{
    EffectFingerprintInput, EffectInfo, FiniteF64, ObjectFingerprintInput, ObjectSelector,
    ObjectSummary, PageMeta, SceneInfo,
};

/// 切り出した後に落とした件数をページのメタ情報へ反映する。
///
/// 総件数と本ページの件数だけを減らし、次ページの位置は動かさない。位置は
/// 列挙が返した並びに対する添字であり、落とした分だけ詰めると、次の要求が同じ
/// 対象を読み直して先へ進まなくなる。
///
/// 総件数へ反映するのは本ページで落とした分だけである。他のページで落ちるかは
/// そのページを切り出すまで分からず、読まずに数えることはできない。
pub(crate) fn dropped_from_page(meta: PageMeta, dropped: usize, count: usize) -> PageMeta {
    PageMeta {
        total_count: meta.total_count.saturating_sub(dropped as u32),
        count: count as u32,
        ..meta
    }
}

/// セレクターが指す対象を、候補の絞り込みと fingerprint の照合まで済ませて返す。
///
/// 読み取りと編集はこの 1 つの実装を共有する。別々に実装すると、読み取りが
/// 返した fingerprint と編集が照合する fingerprint がずれ、一致するはずの対象を
/// 拒む経路が生まれる。
///
/// ここで判定するのは候補の探索と fingerprint の照合だけである。epoch・算出方式・
/// シーンの照合は呼び出し側が済ませておく。読み取りは参照区間の外で、編集は
/// 編集区間の内側で判定するため、判定の場所を共有できない。
///
/// 同一性の材料の読み取りと fingerprint の算出を同じ呼び出しの中で行うので、
/// 照合した対象と返す対象が同じ読み取りに由来することが構造として保証される。
pub(crate) fn resolve_selected_object(
    scene: &dyn SceneReader,
    epoch: &str,
    scene_id: i32,
    selector: &ObjectSelector,
) -> Result<(ObjectSummary, HostObject), ReadError> {
    let candidate = resolve_candidate_of(scene, selector)?;
    let object = scene.object_identity(selector.layer, candidate.frame_start)?;
    let summary = verified_summary(epoch, scene_id, &object, selector)?;
    Ok((summary, object))
}

/// セレクターが指す対象を、配下 effect と中間点まで含めて解決する。
///
/// 照合は [`resolve_selected_object`] と同じ材料・同じ判定で行う。effect の一覧を
/// 必要とする経路だけがこちらを使う。
pub(crate) fn resolve_selected_detail(
    scene: &dyn SceneReader,
    epoch: &str,
    scene_id: i32,
    selector: &ObjectSelector,
) -> Result<(ObjectSummary, HostObjectDetail), ReadError> {
    let candidate = resolve_candidate_of(scene, selector)?;
    let detail = scene.object_detail(selector.layer, candidate.frame_start)?;
    let summary = verified_summary(epoch, scene_id, &detail.object, selector)?;
    Ok((summary, detail))
}

/// セレクターが指す候補を 1 件へ絞る。
///
/// 絞り込みは位置だけで決まる。ここでレイヤー内の全対象の alias まで読むと、
/// 無関係な対象の読み取り失敗が要求全体を巻き込み、対象自体は健全なのに取得
/// できなくなる。
fn resolve_candidate_of(
    scene: &dyn SceneReader,
    selector: &ObjectSelector,
) -> Result<HostObjectPlacement, ReadError> {
    resolve_candidate(scene.object_placements(selector.layer)?, selector.frame)
}

/// 読み直した対象の概要を、セレクターの fingerprint と照合してから返す。
///
/// 食い違った場合は読み直した概要をそのまま失敗へ載せる。この時点で対象は既に
/// 読み直されており、要求元へ現在の姿を渡すのに追加の読み取りは要らない。
fn verified_summary(
    epoch: &str,
    scene_id: i32,
    object: &HostObject,
    selector: &ObjectSelector,
) -> Result<ObjectSummary, ReadError> {
    let summary = object_summary(epoch, scene_id, object);
    if summary.fingerprint != selector.fingerprint {
        return Err(ReadError::FingerprintMismatch {
            current_object: Box::new(summary),
        });
    }
    Ok(summary)
}

/// 開始フレームの完全一致で候補を 1 件へ絞る。
///
/// 「指定フレーム以降」の探索結果をそのまま候補にしない。セレクターの `frame` は
/// 対象の開始フレームであり、途中フレームでの重なりを表さない。
///
/// セレクターの `name` は絞り込みに使わない。レイヤー内の走査は対象の終端の次へ
/// 厳密に前進するため開始フレームは相異なり、名前は候補を減らさない。一方で
/// 名前は fingerprint の材料であり、名前が変わった対象は fingerprint の照合が
/// 捕まえる。絞り込みに使うと、読み直せば作り直せる要求が「一致する対象なし」
/// として返り、要求元は復帰する手立てを失う。
///
/// 候補が複数になる分岐は残す。走査の実装が変わったときに、黙って別の対象を
/// 選ぶより型付きの失敗で止まる方が安全である。
fn resolve_candidate(
    objects: Vec<HostObjectPlacement>,
    frame: usize,
) -> Result<HostObjectPlacement, ReadError> {
    let mut candidates: Vec<HostObjectPlacement> = objects
        .into_iter()
        .filter(|object| object.frame_start == frame)
        .collect();

    match candidates.len() {
        0 => Err(ReadError::ObjectNotFound {
            detected_by: "find_object",
        }),
        1 => Ok(candidates.remove(0)),
        candidate_count => Err(ReadError::AmbiguousObject { candidate_count }),
    }
}

/// effect 列の各要素について fingerprint の入力を組み立てる。
///
/// 列の絶対位置と総数も材料に含めるため、要素を単独では組み立てられない。
/// 一覧と詳細で同じ列から同じ入力が得られるよう、組み立てはここへ集約する。
pub(super) fn effect_fingerprint_inputs(
    effects: &[HostEffect],
) -> impl Iterator<Item = EffectFingerprintInput<'_>> {
    let effect_count = effects.len();
    effects
        .iter()
        .enumerate()
        .map(move |(position, effect)| EffectFingerprintInput {
            effect_name: &effect.name,
            effect_index: effect.index,
            position,
            effect_count,
            enabled: effect.enabled,
            locked: effect.locked,
            items: &effect.items,
        })
}

/// effect 名と同名内の順序から、effect 列全体での位置を求める。
///
/// 同名内の順序は effect の一覧を組み立てた採番規則に従う。ずれると同名 effect の
/// 別インスタンスを指す。読み取りと編集はこの 1 つの実装を共有する。
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
/// られない。fingerprint の入力の組み立てを読み取りと編集で共有する。
pub(crate) fn effect_info_at(
    object: &ObjectSelector,
    effects: &[HostEffect],
    position: usize,
) -> Option<EffectInfo> {
    effect_fingerprint_inputs(effects)
        .nth(position)
        .map(|input| EffectInfo::new(object.clone(), input))
}

/// オブジェクトの概要を組み立てる。
///
/// fingerprint を算出するのはこの 1 か所だけである。
///
/// **型が守るのは 1 点だけである。** 入力は [`HostObject`] であり、位置と名前
/// だけの軽量走査が返す [`HostObjectPlacement`] は渡せない。軽量走査の結果から
/// fingerprint を算出することは、この署名によって不可能になっている。
///
/// **同じ材料であることは型では守られていない。** [`HostObject`] を返す経路は
/// [`SceneReader::object_identity`] と [`SceneReader::object_detail`] の 2 つが
/// あり、両者が同じ材料を読むことは trait の契約と、SDK 実装が同じ写し取りを
/// 共有していることによって約束されているだけである。片方の読み取りだけを
/// 変えても署名は通るため、変えるときは両方を同時に見る必要がある。
pub(crate) fn object_summary(epoch: &str, scene_id: i32, object: &HostObject) -> ObjectSummary {
    ObjectSummary::new(
        epoch,
        ObjectFingerprintInput {
            scene_id,
            layer: object.placement.layer,
            frame_start: object.placement.frame_start,
            frame_end: object.placement.frame_end,
            name: object.placement.name.as_deref(),
            alias: &object.alias,
        },
    )
}

/// シーン情報を組み立てる。
///
/// 読み取りと編集の応答が同じ材料からシーンを組み立てるよう、両方がここを通る。
/// 別々に組み立てると、同じシーンが読みと書きで別の形になり得る。
pub(crate) fn scene_info(info: &HostEditInfo, name: Option<String>) -> SceneInfo {
    SceneInfo {
        id: info.scene_id,
        name,
        width: info.width,
        height: info.height,
        fps: fps(info.fps_rate, info.fps_scale),
        fps_rate: info.fps_rate,
        fps_scale: info.fps_scale,
        sample_rate: info.sample_rate,
    }
}

/// フレームレートを算出する。分母が 0 の場合は算出できない。
fn fps(rate: i32, scale: i32) -> Option<FiniteF64> {
    if scale == 0 {
        return None;
    }
    FiniteF64::try_new(f64::from(rate) / f64::from(scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 位置と名前だけを持つテスト用のオブジェクト。
    fn object(
        layer: usize,
        frame_start: usize,
        frame_end: usize,
        name: Option<&str>,
    ) -> HostObject {
        HostObject {
            placement: HostObjectPlacement {
                layer,
                frame_start,
                frame_end,
                name: name.map(str::to_string),
            },
            alias: format!("[{layer}:{frame_start}]"),
        }
    }

    #[test]
    fn resolve_candidate_requires_exact_start_frame() {
        let objects = vec![object(1, 100, 200, None).placement];
        assert!(matches!(
            resolve_candidate(objects.clone(), 150),
            Err(ReadError::ObjectNotFound { .. })
        ));
        assert_eq!(resolve_candidate(objects, 100).unwrap().frame_start, 100);
    }

    #[test]
    fn fingerprint_of_a_moved_object_differs() {
        let base = object(1, 100, 200, Some("立ち絵"));
        let moved = object(1, 101, 200, Some("立ち絵"));
        assert_ne!(
            object_summary("epoch", 0, &base).fingerprint,
            object_summary("epoch", 0, &moved).fingerprint
        );
    }
}
