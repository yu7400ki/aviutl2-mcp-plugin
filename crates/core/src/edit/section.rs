//! 中間点を対象とする編集の params と result。

use super::{
    EditInputError, FIELD_FRAME, validate_position, validate_section, validate_selector_position,
};
use crate::object::{ObjectSummary, SectionRange};
use crate::selector::ObjectSelector;
use serde::{Deserialize, Serialize};

/// `create_object_section` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateObjectSectionParams {
    /// 対象オブジェクト。
    pub selector: ObjectSelector,
    /// 中間点を追加するフレーム番号（0 始まり、シーンの絶対フレーム番号）。
    pub frame: u32,
}

impl CreateObjectSectionParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// フレームがオブジェクトの範囲に入るかは対象の現在の状態で決まるため、
    /// ここでは判定しない。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_selector_position(&self.selector)?;
        validate_position(FIELD_FRAME, self.frame)
    }
}

/// `delete_object_section` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteObjectSectionParams {
    /// 対象オブジェクト。
    pub selector: ObjectSelector,
    /// 削除する中間点を開始位置に持つ区間の番号。1 以上。
    pub section: u32,
}

impl DeleteObjectSectionParams {
    /// 要求内容だけで決まる検証を行う。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_selector_position(&self.selector)?;
        validate_section(self.section)
    }
}

/// `move_object_section` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveObjectSectionParams {
    /// 対象オブジェクト。
    pub selector: ObjectSelector,
    /// 移動する中間点を開始位置に持つ区間の番号。1 以上。
    pub section: u32,
    /// 移動先のフレーム番号（0 始まり、シーンの絶対フレーム番号）。
    pub frame: u32,
}

impl MoveObjectSectionParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// 隣の中間点を越えるかは対象の現在の状態で決まるため、ここでは判定しない。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_selector_position(&self.selector)?;
        validate_section(self.section)?;
        validate_position(FIELD_FRAME, self.frame)
    }
}

/// 中間点の変更の結果。
///
/// 3 つの operation が同じ型を返す。区間の一覧そのものが read-back であり、
/// 要求した中間点が実際にどこへ入ったかはこの一覧が答える。
///
/// 中間点はプロジェクトへ保存される内容であるため、この変更は revision を
/// 進める。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectSectionsOutcome {
    /// 変更後のプロジェクトの epoch。
    pub project_epoch: String,
    /// 変更を反映したあとの revision。
    pub project_revision: u64,
    /// read-back で得た変更後のオブジェクト。selector と fingerprint を含む。
    pub object: ObjectSummary,
    /// read-back で得た変更後の区間の一覧。
    ///
    /// 区間番号 `i` は `sections[i]` を指す。`sections[i].start`（i ≥ 1）が
    /// i 番目の中間点のフレームであり、`sections[0].start` はオブジェクトの
    /// 開始フレームであって中間点ではない。
    pub sections: Vec<SectionRange>,
}
