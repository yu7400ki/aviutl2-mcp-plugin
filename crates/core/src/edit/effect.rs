//! effect を対象とする編集の params。

use super::{
    EditInputError, FIELD_EFFECT_NAME, FIELD_POSITION, validate_effect_selector_position,
    validate_index, validate_selector_position,
};
use crate::selector::{EffectSelector, ObjectSelector};
use crate::validation::validate_name;
use serde::{Deserialize, Serialize};

/// `add_effect` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddEffectParams {
    /// 付与先のオブジェクト。
    pub object: ObjectSelector,
    /// 付与する effect 名。
    pub effect_name: String,
}

impl AddEffectParams {
    /// 要求内容だけで決まる検証を行う。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_selector_position(&self.object)?;
        validate_name(&self.effect_name).map_err(|source| EditInputError::Text {
            field: FIELD_EFFECT_NAME,
            source,
        })
    }
}

/// `delete_effect` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteEffectParams {
    /// 対象 effect。
    pub selector: EffectSelector,
}

impl DeleteEffectParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// 見るのは [`DeleteObjectParams::validate`](super::DeleteObjectParams::validate) と同じく、セレクターが持つ
    /// 位置指定の範囲だけである。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_effect_selector_position(&self.selector)
    }
}

/// `set_effect_enabled` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetEffectEnabledParams {
    /// 対象 effect。
    pub selector: EffectSelector,
    /// 有効・無効。
    pub enabled: bool,
}

impl SetEffectEnabledParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// 見るのは [`DeleteEffectParams::validate`] と同じく、セレクターが持つ
    /// 位置指定の範囲だけである。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_effect_selector_position(&self.selector)
    }
}

/// `move_effect` の params。
///
/// 移動先はセレクターの外の引数として受け取る。[`EffectSelector`] は対象を指す
/// 材料だけを運び、列の中での位置を含まない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveEffectParams {
    /// 動かす effect。
    pub selector: EffectSelector,
    /// 移動先の、列全体での 0 始まりの位置。
    pub position: usize,
}

impl MoveEffectParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// 見るのはセレクターが持つ位置指定と、移動先が受け渡せる範囲に収まることだけ
    /// である。列の長さとの比較は対象の現在の状態を要するため、変更を適用する側が
    /// 行う。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_effect_selector_position(&self.selector)?;
        validate_index(FIELD_POSITION, self.position)
    }
}
