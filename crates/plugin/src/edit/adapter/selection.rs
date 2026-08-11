//! カーソル・選択範囲・フォーカスの適用と応答の組み立て。

use super::index;
use crate::edit::host::{HostSelection, SceneEditor};
use crate::edit::precondition::{Boundary, MutationPermit};
use crate::edit::resolve::ResolvedObject;
use crate::read::resolve::object_summary;
use aviutl2_mcp_core::{
    Cursor, DisplayRange, DisplayStart, FocusChange, FrameRange, ObservedSelection, RangeChange,
    SelectionField, SelectionState, SetSelectionParams,
};

/// 適用を試みた結果。
///
/// ここが持つのは**変更 API を呼んだ結果だけ**であり、応答の `applied` とは
/// 別である。要求どおりに反映されたかまで見る軸があるため、応答の一覧は
/// [`selection_state`] が観測と突き合わせてから組み立てる。
pub(super) struct SelectionOutcome {
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
pub(super) fn apply_selection(
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
pub(super) fn selection_state(
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
