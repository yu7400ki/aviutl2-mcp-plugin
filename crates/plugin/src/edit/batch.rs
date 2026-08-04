//! 一括適用の 3 相。
//!
//! 事前解決相・適用相・巻き戻し相・結果の組み立ての全てを 1 回の編集区間へ収め、
//! SDK 上では 1 つの取り消し単位にする。相ごとに区間を分けると取り消しが割れ、
//! 隙間に UI 操作が入って対象が入れ替わる余地も生まれる。
//!
//! 事前解決相の産物は [`BatchPlan`] だけであり、適用相と巻き戻し相はそれしか
//! 見ない。
//!
//! # 型が強制する範囲
//!
//! [`plan`] は変更の許可も [`ProjectState`] も引数に取らないため、計画を
//! 組み立てる間に変更を発行する経路が型として存在しない。許可を先に作っても
//! 同じである——許可は発行に使われるまで何も起こさず、[`plan`] はその許可を
//! 受け取れない。
//!
//! **言えるのはここまでである。** 適用相と巻き戻し相も `&dyn SceneEditor` を
//! 持つため、対象を解決し直すことはできてしまう。次の 2 つは型では強制できず、
//! 呼び出し順の記録で固定している。
//!
//! - 最初の変更より前に、全 sub-operation の解決と逆操作の材料の読み取りが
//!   終わっていること。
//! - 最初の変更より後に、新たな解決を行わないこと。
//!
//! **記録が固定するのは「最初の変更」を境とする順序であって、許可を取る位置
//! そのものではない。** 許可の取得はホストへの呼び出しを伴わないため、記録に
//! 現れる手掛かりが無い。ここで「許可より前」と書けば、テストが裏付けていない
//! ことを主張することになる。

use crate::edit::adapter::{
    attribute, ensure_destination_free, ensure_layers_unlocked, index, reread_with_effects,
    unlisted_item,
};
use crate::edit::error::{EditError, RollbackOutcome, UnsupportedReason};
use crate::edit::host::{ObjectPosition, SceneEditor};
use crate::edit::precondition::{Boundary, MutationPermit};
use crate::edit::resolve::{ResolvedEffect, ResolvedObject, resolve_effect, resolve_object};
use crate::project::ProjectState;
use crate::read::ReadError;
use crate::read::adapter::effect_info_at;
use crate::read::host::{
    HostEditInfo, HostEffect, HostLayer, HostObject, HostObjectDetail, HostObjectPlacement,
    SceneReader,
};
use aviutl2_mcp_core::{
    AvailableEffectItem, BatchOperation, BatchOutcome, BatchStepOutcome, FiniteF64, FrameRange,
    ItemWriteError, ObjectSelector, ObjectSummary, prepare_item_write,
};
use std::cell::RefCell;
use std::collections::HashMap;

/// 一括適用を編集区間の内側で実行する。
///
/// `boundary` は要求全体の照合を通った前提であり、この関数は変更許可を
/// **1 つだけ**取り出す。許可あたりの加算は 1 回へ丸められるため、要求全体で
/// 進む revision も 1 だけである。
pub(crate) fn apply_batch(
    editor: &dyn SceneEditor,
    project: &ProjectState,
    boundary: &Boundary,
    operations: &[BatchOperation],
) -> Result<BatchOutcome, EditError> {
    let cache = CachingEditor::new(editor);
    let plan = plan(&cache, boundary, operations)?;

    // 許可は全 sub-operation の解決と逆操作の構築が終わってから取る。
    let permit = boundary.issue_permit(project)?;
    if let Err(failure) = apply(&plan, editor, &permit, boundary) {
        let rollback = roll_back(&plan, failure.applied, editor, &permit, boundary);
        return Err(EditError::Batch {
            source: Box::new(failure.error),
            failed_index: Some(failure.index),
            rollback,
        });
    }
    summarize(&plan, editor, &permit, boundary)
}

/// 許可を経ずに区間を抜けた失敗へ、中途半端な状態が残った可能性を添える。
///
/// panic の捕捉は逆操作を保持する計画ごと巻き戻すため、どこまで適用したかも、
/// 巻き戻しの途中だったかも分からない。1 つの変更しか持たない編集と違い、
/// 一括適用では「一部だけ適用された状態」が実際に起こり得る。起こり得る状態を
/// 表せる語彙が既にあるのに使わない理由が無い。
///
/// **変更を発行していない失敗には添えない。** 事前解決相の panic は変更を 1 つも
/// 発行していないため、中途半端な状態は生じ得ない。無条件に添えると、何も起きて
/// いない失敗が最も重い失敗として報告され、要求元に無用の読み直しを強いる。
pub(crate) fn mark_lost_section(error: EditError) -> EditError {
    let lost = matches!(
        &error,
        EditError::AfterMutation { source, .. }
            if matches!(**source, EditError::Panicked | EditError::MutationPermitReissued)
    );
    if !lost {
        return error;
    }
    EditError::Batch {
        source: Box::new(error),
        failed_index: None,
        rollback: RollbackOutcome::Impossible,
    }
}

/// 要求全体への境界の照合で落ちた sub-operation の位置を添える。
///
/// 境界の照合は要求全体を 1 度に見るため、どの sub-operation で落ちたかを
/// 返さない。失敗そのものが名乗る材料（食い違ったシーン、現在の epoch）で同じ
/// 順に走査し直し、最初に食い違う位置を添える。位置を特定できない失敗はそのまま
/// 返す。
///
/// # 正しい位置を指すための条件
///
/// 照合は要求と同じ順序で走り、最初の食い違いで返る。したがって述語に当てはまる
/// 最初の位置が落ちた位置そのものになる。**これは呼び出し側の渡し方に依存する。**
///
/// - **セレクターを `operations` と同じ順序・同じ件数で渡すこと。** 並べ替えたり
///   間引いたりすると、ここで数える位置が要求の位置とずれる。
/// - **シーンの guard を渡さないこと。** 照合はセレクターより先に guard を見る
///   ため、非空の guard を渡すと guard 由来のシーンの食い違いが、たまたま同じ
///   シーンを名乗るセレクターの位置として報告され得る。
/// - **区間の内側で epoch が変わらないこと。** 照合が見た値そのものではなく、
///   ここで読み直した値と比べている。プロジェクト境界の更新と編集区間の
///   コールバックはホストの同一スレッドで走るため、区間内で入れ替わる経路は
///   存在しない。
pub(crate) fn locate_boundary_failure(
    operations: &[BatchOperation],
    epoch: &str,
    error: EditError,
) -> EditError {
    let found = match &error {
        EditError::Read(ReadError::EpochMismatch) => {
            position(operations, |selector| selector.project_epoch != epoch)
        }
        EditError::Read(ReadError::SceneMismatch { expected, .. }) => {
            position(operations, |selector| selector.scene_id == *expected)
        }
        _ => None,
    };
    match found {
        Some(index) => at(index, error),
        None => error,
    }
}

/// 条件に当てはまる selector を持つ最初の sub-operation の位置。
fn position(
    operations: &[BatchOperation],
    matches: impl Fn(&ObjectSelector) -> bool,
) -> Option<usize> {
    operations
        .iter()
        .position(|operation| matches(operation.object_selector()))
}

/// 事前解決相が組み立てた計画。
///
/// 生成できるのは [`plan`] だけであり、生成の時点で全 sub-operation の解決・
/// 照合・逆操作の構築が済んでいる。解決済みトークンを保持するため、編集区間の
/// 外へは持ち出せない。
struct BatchPlan<'sec> {
    steps: Vec<PlannedStep<'sec>>,
}

/// レイヤーと開始フレームの組。
///
/// 2 つの `usize` をそのまま持ち回すと、取り違えても型検査を通る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placement {
    layer: usize,
    frame: usize,
}

/// 適用する変更 1 件と、その逆操作。
enum PlannedStep<'sec> {
    /// レイヤーと開始フレームを変更する。
    Move {
        object: ResolvedObject<'sec>,
        /// 要求された宛先。**発行にだけ用い、結果の組み立てには用いない。**
        /// ホストは位置を調整し得るため、要求値は実配置ではない。
        destination: Placement,
        /// 変更前の位置。逆操作の宛先になる。
        origin: Placement,
    },
    /// 設定項目の値を変更する。
    SetItem {
        object: ResolvedObject<'sec>,
        /// 解決済みの effect。要素の大きさが移動の枝と釣り合わないため、
        /// 計画の配列が最大の枝に合わせて膨らまないよう間接参照にする。
        effect: Box<ResolvedEffect<'sec>>,
        item: String,
        /// 要求された値を SDK の形式へ写したもの。
        value: String,
        /// 変更前の、SDK が返した生の文字列。
        origin_value: String,
    },
}

impl<'sec> PlannedStep<'sec> {
    /// 変更が触れる対象オブジェクト。
    fn object(&self) -> &ResolvedObject<'sec> {
        match self {
            PlannedStep::Move { object, .. } | PlannedStep::SetItem { object, .. } => object,
        }
    }
}

/// 事前解決相。
///
/// 全 sub-operation の対象を解決し、順序に依存しない事前条件を判定し、逆操作を
/// 組み立てる。**1 つでも失敗したらそこで返る。** 変更は 1 つも発行されて
/// いないため、プロジェクトは無傷である。
///
/// 宛先の占有はここで判定しない（[`apply`]）。
fn plan<'sec>(
    editor: &'sec dyn SceneEditor,
    boundary: &Boundary,
    operations: &[BatchOperation],
) -> Result<BatchPlan<'sec>, EditError> {
    let mut steps = Vec::with_capacity(operations.len());
    for (position, operation) in operations.iter().enumerate() {
        let step = plan_step(editor, boundary, operation).map_err(|error| at(position, error))?;
        steps.push(step);
    }
    ensure_distinct(&steps)?;
    Ok(BatchPlan { steps })
}

/// sub-operation 1 件を解決し、逆操作まで組み立てる。
fn plan_step<'sec>(
    editor: &'sec dyn SceneEditor,
    boundary: &Boundary,
    operation: &BatchOperation,
) -> Result<PlannedStep<'sec>, EditError> {
    match operation {
        BatchOperation::MoveObject {
            selector,
            destination,
        } => {
            let object = resolve_object(editor, boundary, selector)?;
            let origin = Placement {
                layer: object.layer(),
                frame: object.frame_start(),
            };
            let destination = Placement {
                layer: index(destination.layer),
                frame: index(destination.frame),
            };
            // レイヤーのロックは一括適用の実行中に変わらない——レイヤーの属性を
            // 変える operation は一括適用に入らない。したがってここで判定できる。
            // 掛かるのは移動だけであり、設定値の変更だけの要求はロックの影響を
            // 受けない。
            ensure_layers_unlocked(editor, [origin.layer, destination.layer])?;
            Ok(PlannedStep::Move {
                object,
                destination,
                origin,
            })
        }
        BatchOperation::SetObjectItem {
            selector,
            item,
            value,
        } => {
            let (object, effect) = resolve_effect(editor, boundary, selector)?;
            let items = editor.effect_items(&effect)?;
            let value = match prepare_item_write(&items, item, value) {
                Ok(value) => value,
                Err(ItemWriteError::ItemNotFound { item }) => {
                    return Err(unlisted_item(editor, &effect, &item));
                }
                Err(error) => return Err(EditError::ItemWrite(error)),
            };
            // 逆操作は、変更前に SDK が返した文字列をそのまま書き戻す。読み取り
            // 経路が解釈した値を文字列へ戻す形にはしない——その往復が破れると、
            // 元と違う値が「元へ戻した」として静かに残る。前向きの変更で破れれば
            // 要求元は応答の正規化値から気付けるが、巻き戻しでは失敗の応答しか
            // 返らず、誰も値を検分しない。**同じ欠陥でも巻き戻しで踏んだときだけ
            // 観測されない。**
            let origin_value = editor.effect_item_value(&effect, item).map_err(|_| {
                EditError::UnsupportedTarget {
                    reason: UnsupportedReason::InverseUnavailable,
                }
            })?;
            Ok(PlannedStep::SetItem {
                object,
                effect: Box::new(effect),
                item: item.clone(),
                value,
                origin_value,
            })
        }
    }
}

/// 解決した結果として同じ状態を書き換える組が無いことを確かめる。
///
/// 要求内容だけの検証はセレクターの文字列としての同一性を見る。名前を指定した
/// セレクターと指定しないセレクターは文字列として異なるが、同じオブジェクトへ
/// 解決し得るため、そこだけでは取りこぼす。**正しさを担保するのはここである。**
///
/// 同一性は解決結果の位置と effect の列位置で見る。ハンドルの同値比較には
/// 依存しない——生ポインタの一致が同一対象を意味する保証は無い。
fn ensure_distinct(steps: &[PlannedStep<'_>]) -> Result<(), EditError> {
    let mut seen: Vec<TargetKey<'_>> = Vec::with_capacity(steps.len());
    for (position, step) in steps.iter().enumerate() {
        let key = target_key(step);
        if seen.contains(&key) {
            return Err(at(position, EditError::DuplicateTarget));
        }
        seen.push(key);
    }
    Ok(())
}

/// sub-operation が書き換える状態の単位。
#[derive(PartialEq, Eq)]
enum TargetKey<'a> {
    /// オブジェクトの位置。
    Position { layer: usize, frame_start: usize },
    /// effect の設定項目。
    Item {
        layer: usize,
        frame_start: usize,
        effect: usize,
        item: &'a str,
    },
}

/// 解決結果から、書き換える状態の単位を取り出す。
fn target_key<'a>(step: &'a PlannedStep<'_>) -> TargetKey<'a> {
    match step {
        PlannedStep::Move { object, .. } => TargetKey::Position {
            layer: object.layer(),
            frame_start: object.frame_start(),
        },
        PlannedStep::SetItem {
            object,
            effect,
            item,
            ..
        } => TargetKey::Item {
            layer: object.layer(),
            frame_start: object.frame_start(),
            effect: effect.position(),
            item,
        },
    }
}

/// 適用相の失敗と、そこまでに発行し終えた件数。
struct ApplyFailure {
    /// 落ちた sub-operation の位置。
    index: usize,
    /// 落ちた理由。
    error: EditError,
    /// 先頭から数えて発行に成功した件数。巻き戻しの対象そのものである。
    applied: usize,
}

/// 適用相。要求の配列順に発行する。
fn apply(
    plan: &BatchPlan<'_>,
    editor: &dyn SceneEditor,
    permit: &MutationPermit<'_>,
    boundary: &Boundary,
) -> Result<(), ApplyFailure> {
    for (position, step) in plan.steps.iter().enumerate() {
        if let Err(error) = issue(step, editor, permit, boundary) {
            // 落ちた step 自身は巻き戻しの対象にならない。宛先の確認で落ちた
            // 場合は SDK を呼んでおらず、SDK へ届かなかった失敗も同じである。
            return Err(ApplyFailure {
                index: position,
                error,
                applied: position,
            });
        }
    }
    Ok(())
}

/// 計画済みの変更を 1 件発行する。
fn issue(
    step: &PlannedStep<'_>,
    editor: &dyn SceneEditor,
    permit: &MutationPermit<'_>,
    boundary: &Boundary,
) -> Result<(), EditError> {
    match step {
        PlannedStep::Move {
            object,
            destination,
            origin,
        } => {
            // 宛先の占有は発行の直前に判定する。事前解決相で一括判定すると、
            // 先行 sub-operation が空けた宛先へ移動する要求が必ず失敗する。
            // 走査もここで行うため、先行 sub-operation の効果が反映されている。
            let occupants = attribute(
                permit,
                boundary,
                editor.reader().object_placements(destination.layer),
            )?;
            let moving_from = (destination.layer == origin.layer).then_some(origin.frame);
            attribute(
                permit,
                boundary,
                ensure_destination_free(
                    &occupants,
                    destination.layer,
                    destination.frame,
                    moving_from,
                ),
            )?;
            permit.issue(boundary, |ticket| {
                editor.move_object(ticket, object, destination.layer, destination.frame)
            })
        }
        PlannedStep::SetItem {
            effect,
            item,
            value,
            ..
        } => permit.issue(boundary, |ticket| {
            editor.set_effect_item(ticket, effect, item, value)
        }),
    }
}

/// 巻き戻し相。発行済みの逆操作を、適用と**逆の順序**で全件試みる。
///
/// 逆順は慣習ではなく、移動の逆操作が成立する唯一の順序である。移動を元位置へ
/// 戻すには元位置が空いている必要があり、その位置を塞ぎ得るのは「元位置へ後から
/// 移動してきた別の sub-operation」だけである。それは必ず後続であり、逆順なら
/// 先に戻る。設定値の書き戻しは順序に依存しないが、1 つの規則で両方が成立する
/// なら規則を分けない。
///
/// **1 件失敗しても止めずに残りを続ける。** 止めると、戻せたはずのものまで
/// 戻さないことになる。ただし続行は上の順序の保証を無効にする——移動の逆操作が
/// 1 件失敗すると、その対象は先行する移動の元位置を塞いだままになり、連鎖して
/// 失敗し得る。それでも続行する。連鎖して失敗した分も不整合の内側にあり、
/// 要求元に求める行動（読み直す）は変わらない。
fn roll_back(
    plan: &BatchPlan<'_>,
    applied: usize,
    editor: &dyn SceneEditor,
    permit: &MutationPermit<'_>,
    boundary: &Boundary,
) -> RollbackOutcome {
    let mut restored = 0;
    for (position, step) in plan.steps[..applied].iter().enumerate().rev() {
        match restore(step, editor, permit, boundary) {
            Ok(()) => restored += 1,
            Err(error) => tracing::warn!(
                index = position,
                code = %error.error_code().as_snake_case(),
                "sub-operation を元へ戻せませんでした"
            ),
        }
    }
    if restored == applied {
        RollbackOutcome::Complete { count: restored }
    } else {
        RollbackOutcome::Incomplete { count: restored }
    }
}

/// 逆操作を 1 件発行し、戻せたことを読み直して確かめる。
///
/// 発行しただけでは戻せたと言えない。宛先の占有は事前に確かめない——逆順で
/// あれば宛先は空いているはずであり、空いていなければ SDK が失敗を返す。事前
/// 確認を挟むと失敗の理由が分かれるが、どちらも同じ扱いになるため区別に意味が
/// 無い。
fn restore(
    step: &PlannedStep<'_>,
    editor: &dyn SceneEditor,
    permit: &MutationPermit<'_>,
    boundary: &Boundary,
) -> Result<(), EditError> {
    match step {
        PlannedStep::Move { object, origin, .. } => {
            permit.issue(boundary, |ticket| {
                editor.move_object(ticket, object, origin.layer, origin.frame)
            })?;
            let position = editor.object_position(object)?;
            ensure_restored(position.layer == origin.layer && position.frame_start == origin.frame)
        }
        PlannedStep::SetItem {
            effect,
            item,
            origin_value,
            ..
        } => {
            permit.issue(boundary, |ticket| {
                editor.set_effect_item(ticket, effect, item, origin_value)
            })?;
            // 生の文字列の完全一致を求められるのは、書き戻す値がホスト自身が
            // 返した文字列だからである。正規化が冪等でなければ、戻せていたのに
            // 不整合を名乗ることになる。**それでよい。** 逆へ倒すと、戻って
            // いない値を戻ったと報告する。
            let current = editor.effect_item_value(effect, item)?;
            ensure_restored(current == *origin_value)
        }
    }
}

/// 読み直した状態が元値と一致することを確かめる。
fn ensure_restored(restored: bool) -> Result<(), EditError> {
    if restored {
        return Ok(());
    }
    Err(EditError::UnsupportedTarget {
        reason: UnsupportedReason::ChangeNotApplied,
    })
}

/// 全 sub-operation の適用を終えてから、触れた対象を読み直して結果を組み立てる。
///
/// sub-operation ごとに直後の状態で組み立ててはならない。後続の sub-operation が
/// 同じ対象を変えると、先に組み立てた結果が返すセレクターと fingerprint は既に
/// 無効になる。応答は「次の要求へそのまま使えること」を目的としており、無効な
/// セレクターを返すことはその目的を裏切る。
///
/// **移動後の位置を要求値から取らない。** ホストが位置を調整すればそれは実配置
/// ではない。移動は対象を破棄しないためトークンは有効なままであり、位置を直接
/// 読める。
///
/// ここでの失敗では巻き戻さない。全変更は成功しており、失敗したのは応答の
/// 組み立てだけである。
fn summarize(
    plan: &BatchPlan<'_>,
    editor: &dyn SceneEditor,
    permit: &MutationPermit<'_>,
    boundary: &Boundary,
) -> Result<BatchOutcome, EditError> {
    let mut positions = Vec::with_capacity(plan.steps.len());
    for step in &plan.steps {
        positions.push(attribute(
            permit,
            boundary,
            editor.object_position(step.object()),
        )?);
    }

    // 異なる対象ごとに 1 度だけ読み直す。同じ対象を指す複数の sub-operation は
    // 同一の要約を持つ。これは重複ではなく事実の反映である。
    let mut targets: Vec<(ObjectPosition, ObjectSummary, Vec<HostEffect>)> = Vec::new();
    let mut slots = Vec::with_capacity(positions.len());
    for position in &positions {
        let slot = match targets.iter().position(|(seen, ..)| seen == position) {
            Some(slot) => slot,
            None => {
                let (summary, effects) = attribute(
                    permit,
                    boundary,
                    reread_with_effects(editor, boundary, position.layer, position.frame_start),
                )?;
                targets.push((*position, summary, effects));
                targets.len() - 1
            }
        };
        slots.push(slot);
    }

    let mut results = Vec::with_capacity(plan.steps.len());
    for (step, slot) in plan.steps.iter().zip(&slots) {
        let (_, summary, effects) = &targets[*slot];
        let effect = match step {
            // 移動は effect を返さない。読まなかったものを返す場所は無い。
            PlannedStep::Move { .. } => None,
            PlannedStep::SetItem { effect, .. } => Some(
                effect_info_at(&summary.selector, effects, effect.position()).ok_or_else(|| {
                    permit.attribute(
                        boundary,
                        EditError::Sdk {
                            operation: "get_effect_list",
                        },
                    )
                })?,
            ),
        };
        results.push(BatchStepOutcome {
            object: summary.clone(),
            effect,
        });
    }

    Ok(BatchOutcome {
        project_epoch: boundary.epoch().to_string(),
        project_revision: permit.project_revision(boundary),
        results,
    })
}

/// 失敗へ sub-operation の位置を添える。変更は 1 つも発行されていない。
fn at(index: usize, error: EditError) -> EditError {
    EditError::Batch {
        source: Box::new(error),
        failed_index: Some(index),
        rollback: RollbackOutcome::NotAttempted,
    }
}

/// 事前解決相のあいだだけ読み取りを覚える編集口。
///
/// 同じ対象を指す sub-operation がいくつあっても、対象の詳細は 1 度しか読まない。
///
/// 費用の話だけではない。**同一区間内の 2 回の読み取りが異なる値を返せば、同じ
/// 対象を指す 2 つの sub-operation の一方だけが前提条件の不一致になり、要求元は
/// 何が起きたか理解できない。**「同じ対象は同じ値」を読み取り回数に依存させない。
///
/// **適用相へは持ち越さない。** 持ち越すと、宛先の確認も結果の組み立ても変更前の
/// 値を見ることになる。
struct CachingEditor<'a> {
    inner: &'a dyn SceneEditor,
    placements: RefCell<HashMap<usize, Vec<HostObjectPlacement>>>,
    identities: RefCell<HashMap<(usize, usize), HostObject>>,
    details: RefCell<HashMap<(usize, usize), HostObjectDetail>>,
}

impl<'a> CachingEditor<'a> {
    fn new(inner: &'a dyn SceneEditor) -> Self {
        Self {
            inner,
            placements: RefCell::new(HashMap::new()),
            identities: RefCell::new(HashMap::new()),
            details: RefCell::new(HashMap::new()),
        }
    }
}

impl SceneReader for CachingEditor<'_> {
    fn scene_name(&self) -> Option<String> {
        self.inner.reader().scene_name()
    }

    fn grid_bpm(&self) -> Result<Vec<FiniteF64>, ReadError> {
        self.inner.reader().grid_bpm()
    }

    fn layer(&self, layer: usize) -> Result<HostLayer, ReadError> {
        self.inner.reader().layer(layer)
    }

    fn layer_locked(&self, layer: usize) -> Result<bool, ReadError> {
        self.inner.reader().layer_locked(layer)
    }

    fn object_count(&self, layer: usize) -> Result<usize, ReadError> {
        self.inner.reader().object_count(layer)
    }

    fn object_placements(&self, layer: usize) -> Result<Vec<HostObjectPlacement>, ReadError> {
        if let Some(cached) = self.placements.borrow().get(&layer) {
            return Ok(cached.clone());
        }
        let placements = self.inner.reader().object_placements(layer)?;
        self.placements
            .borrow_mut()
            .insert(layer, placements.clone());
        Ok(placements)
    }

    fn object_identity(&self, layer: usize, frame_start: usize) -> Result<HostObject, ReadError> {
        if let Some(cached) = self.identities.borrow().get(&(layer, frame_start)) {
            return Ok(cached.clone());
        }
        let object = self.inner.reader().object_identity(layer, frame_start)?;
        self.identities
            .borrow_mut()
            .insert((layer, frame_start), object.clone());
        Ok(object)
    }

    fn object_detail(
        &self,
        layer: usize,
        frame_start: usize,
    ) -> Result<HostObjectDetail, ReadError> {
        if let Some(cached) = self.details.borrow().get(&(layer, frame_start)) {
            return Ok(cached.clone());
        }
        let detail = self.inner.reader().object_detail(layer, frame_start)?;
        // 同一性の材料は詳細にも含まれる。詳細を読んだあとに同一性だけを読み
        // 直せば、同じ対象へ 2 度問い合わせることになる。
        self.identities
            .borrow_mut()
            .insert((layer, frame_start), detail.object.clone());
        self.details
            .borrow_mut()
            .insert((layer, frame_start), detail.clone());
        Ok(detail)
    }

    fn effect_track_values(
        &self,
        layer: usize,
        frame_start: usize,
        effect_position: usize,
        item_name: &str,
        frames: &[f64],
    ) -> Result<Vec<FiniteF64>, ReadError> {
        self.inner.reader().effect_track_values(
            layer,
            frame_start,
            effect_position,
            item_name,
            frames,
        )
    }

    fn effect_check_values(
        &self,
        layer: usize,
        frame_start: usize,
        effect_position: usize,
        item_name: &str,
        frames: &[usize],
    ) -> Result<Vec<bool>, ReadError> {
        self.inner.reader().effect_check_values(
            layer,
            frame_start,
            effect_position,
            item_name,
            frames,
        )
    }

    fn track_group_item_names(
        &self,
        layer: usize,
        frame_start: usize,
        effect_name: &str,
        effect_index: usize,
        group_name: &str,
    ) -> Result<Vec<String>, ReadError> {
        self.inner.reader().track_group_item_names(
            layer,
            frame_start,
            effect_name,
            effect_index,
            group_name,
        )
    }
}

/// 読み取り以外は素通しする。
///
/// 変更 API まで備えるのは trait の要求であって、事前解決相がこれらを呼ぶ
/// ことは無い——[`plan`] は変更の権利を作れないためである。
impl SceneEditor for CachingEditor<'_> {
    fn reader(&self) -> &dyn SceneReader {
        self
    }

    fn entry_edit_info(&self) -> &HostEditInfo {
        self.inner.entry_edit_info()
    }

    fn occupied_layer_max(&self) -> Result<usize, EditError> {
        self.inner.occupied_layer_max()
    }

    fn bind_object(
        &self,
        layer: usize,
        frame_start: usize,
    ) -> Result<crate::edit::host::ObjectSlot, EditError> {
        self.inner.bind_object(layer, frame_start)
    }

    fn bind_effect(
        &self,
        object: crate::edit::host::ObjectSlot,
        position: usize,
    ) -> Result<crate::edit::host::EffectSlot, EditError> {
        self.inner.bind_effect(object, position)
    }

    fn effect_items(
        &self,
        effect: &ResolvedEffect<'_>,
    ) -> Result<Vec<AvailableEffectItem>, EditError> {
        self.inner.effect_items(effect)
    }

    fn effect_item_value(
        &self,
        effect: &ResolvedEffect<'_>,
        item: &str,
    ) -> Result<String, EditError> {
        self.inner.effect_item_value(effect, item)
    }

    fn supports_media_file(&self, path: &str) -> Result<bool, EditError> {
        self.inner.supports_media_file(path)
    }

    fn create_object_from_alias(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        alias: &str,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.inner
            .create_object_from_alias(ticket, alias, layer, frame)
    }

    fn create_object_from_media_file(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        path: &str,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.inner
            .create_object_from_media_file(ticket, path, layer, frame)
    }

    fn object_position(&self, object: &ResolvedObject<'_>) -> Result<ObjectPosition, EditError> {
        self.inner.object_position(object)
    }

    fn move_object(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.inner.move_object(ticket, object, layer, frame)
    }

    fn delete_object(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        object: &ResolvedObject<'_>,
    ) -> Result<(), EditError> {
        self.inner.delete_object(ticket, object)
    }

    fn object_sections(
        &self,
        object: &ResolvedObject<'_>,
    ) -> Result<Vec<aviutl2_mcp_core::SectionRange>, EditError> {
        self.inner.object_sections(object)
    }

    fn create_object_section(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        frame: usize,
    ) -> Result<(), EditError> {
        self.inner.create_object_section(ticket, object, frame)
    }

    fn delete_object_section(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        section: usize,
    ) -> Result<(), EditError> {
        self.inner.delete_object_section(ticket, object, section)
    }

    fn move_object_section(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        section: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.inner
            .move_object_section(ticket, object, section, frame)
    }

    fn set_object_name(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        name: Option<&str>,
    ) -> Result<(), EditError> {
        self.inner.set_object_name(ticket, object, name)
    }

    fn create_effect(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        effect_name: &str,
    ) -> Result<(), EditError> {
        self.inner.create_effect(ticket, object, effect_name)
    }

    fn delete_effect(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        effect: &ResolvedEffect<'_>,
    ) -> Result<(), EditError> {
        self.inner.delete_effect(ticket, object, effect)
    }

    fn set_effect_enabled(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        effect: &ResolvedEffect<'_>,
        enabled: bool,
    ) -> Result<(), EditError> {
        self.inner.set_effect_enabled(ticket, effect, enabled)
    }

    fn set_effect_item(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        effect: &ResolvedEffect<'_>,
        item: &str,
        value: &str,
    ) -> Result<(), EditError> {
        self.inner.set_effect_item(ticket, effect, item, value)
    }

    fn set_layer_name(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        layer: usize,
        name: Option<&str>,
    ) -> Result<(), EditError> {
        self.inner.set_layer_name(ticket, layer, name)
    }

    fn set_layer_enabled(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        layer: usize,
        enabled: bool,
    ) -> Result<(), EditError> {
        self.inner.set_layer_enabled(ticket, layer, enabled)
    }

    fn set_layer_locked(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        layer: usize,
        locked: bool,
    ) -> Result<(), EditError> {
        self.inner.set_layer_locked(ticket, layer, locked)
    }

    fn set_cursor(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.inner.set_cursor(ticket, layer, frame)
    }

    fn set_select_range(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        range: Option<FrameRange>,
    ) -> Result<(), EditError> {
        self.inner.set_select_range(ticket, range)
    }

    fn set_focus_object(
        &self,
        ticket: crate::edit::precondition::MutationTicket<'_>,
        object: Option<&ResolvedObject<'_>>,
    ) -> Result<(), EditError> {
        self.inner.set_focus_object(ticket, object)
    }
}
