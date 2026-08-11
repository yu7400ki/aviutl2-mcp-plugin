//! 設定項目の書き込みの前提条件と読み直しによる検証。

use super::{attribute, reread_with_effects};
use crate::edit::error::{EditError, UnsupportedReason};
use crate::edit::host::SceneEditor;
use crate::edit::precondition::{Boundary, MutationPermit};
use crate::edit::resolve::{ResolvedEffect, ResolvedObject};
use crate::read::host::HostEffect;
use aviutl2_mcp_core::{
    AvailableEffectItem, ItemValue, ItemWrite, ItemWriteError, Movement, ObjectSummary,
    ReadBackCheck, TrackWriteTarget, movement_check_reads_current_value,
    write_drops_existing_movement,
};

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
pub(super) fn ensure_movement_write_with_origin(
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
pub(super) fn read_before_write(
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
