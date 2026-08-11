//! 中間点の前提条件。

use crate::edit::error::{EditError, SectionPreconditionReason};
use aviutl2_mcp_core::SectionRange;
/// 中間点を置くフレームが、いま読み直した区間と両立することを確かめる。
///
/// 見るのはオブジェクトの範囲に入ることと、既存の境界と重ならないことである。
/// どちらも対象の現在の状態で決まるため、要求内容だけの検証では判定できない。
pub(super) fn ensure_section_can_be_created(
    sections: &[SectionRange],
    frame: usize,
) -> Result<(), EditError> {
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
pub(super) fn ensure_section_exists(
    sections: &[SectionRange],
    section: usize,
) -> Result<(), EditError> {
    if section < sections.len() {
        return Ok(());
    }
    Err(EditError::SectionPrecondition {
        reason: SectionPreconditionReason::SectionIndexOutOfRange,
    })
}

/// 中間点の移動先が隣の中間点を越えないことを確かめる。
///
/// 動かせるのは 1 つ前の区間の開始位置より後、1 つ後の区間の開始位置より前
/// である。1 つ後が無ければオブジェクトの終了フレームまでとなる。中間点の順序が
/// 入れ替わらないことは SDK の不変条件であり、崩す要求は届く前に落とす。
pub(super) fn ensure_section_move_stays_between_neighbours(
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
