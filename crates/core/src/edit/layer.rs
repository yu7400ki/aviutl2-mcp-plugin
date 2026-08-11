//! レイヤーを対象とする編集の params と result。

use super::{
    EditInputError, FIELD_ENABLED, FIELD_LAYER, FIELD_LOCKED, FIELD_NAME, validate_position,
};
use crate::object::LayerInfo;
use crate::validation::{TextSyntaxError, validate_name};
use serde::{Deserialize, Serialize};

/// レイヤー名の変更。
///
/// 標準名へ戻す指定は値を持たないが、判別子だけを持つ**構造体 variant**として
/// 表す。理由は [`RangeChange`](super::RangeChange) と同じで、unit variant は判別子以外の
/// フィールドを黙って読み飛ばす。ワイヤ表現はどちらも `{"type":"reset"}` で
/// 変わらない。
///
/// 「省略」と「標準名へ戻す」を二重の [`Option`] で表さない。ワイヤ上では
/// どちらも同じ形になり、区別が付かなくなる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LayerNameChange {
    /// 指定した名前にする。
    ///
    /// 空文字列は受け付けない。ホストは空を標準名へ戻す指定として扱うため、
    /// 受け付ければ [`LayerNameChange::Reset`] を要求していない呼び出しに対して
    /// 標準名へ戻す変更を行い、それを成功として返すことになる。**取り消しの
    /// 指定は既にワイヤ上にある**ため、そこへ黙って合流させる理由が無い。
    Set {
        /// 新しいレイヤー名。空文字列は指定できない。
        name: String,
    },
    /// 標準の名前へ戻す。
    Reset {},
}

impl LayerNameChange {
    /// 名前の文字種と長さを検証する。
    pub fn validate(&self) -> Result<(), EditInputError> {
        match self {
            LayerNameChange::Set { name } => {
                if name.is_empty() {
                    return Err(EditInputError::Text {
                        field: FIELD_NAME,
                        source: TextSyntaxError::Empty,
                    });
                }
                validate_name(name).map_err(|source| EditInputError::Text {
                    field: FIELD_NAME,
                    source,
                })
            }
            LayerNameChange::Reset {} => Ok(()),
        }
    }

    /// 設定する名前。標準名へ戻す指定では `None`。
    ///
    /// [`LayerNameChange::validate`] を通った値では、`Some` の中身が空になる
    /// ことはない。呼び出し側は空を `None` へ寄せ直さずにそのまま渡す。
    pub fn requested(&self) -> Option<&str> {
        match self {
            LayerNameChange::Set { name } => Some(name),
            LayerNameChange::Reset {} => None,
        }
    }
}

/// `set_layer_state` の params。
///
/// レイヤーは selector も fingerprint も持たない。守れるのはプロジェクト境界と
/// 現在シーンとレイヤー番号の範囲だけであり、「読み取った時点と同じ状態の
/// レイヤーか」は確かめられない。応答は read-back で得た実際の状態を返すため、
/// 要求元はそれを見て判断する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetLayerStateParams {
    /// 現在シーンの一致確認に使う guard。
    ///
    /// レイヤーはシーンに属し、対象を指す selector を持たない。guard が無いと、
    /// 要求が想定したシーンと現在シーンの一致を確かめる手段が無い。
    pub expected_scene_id: i32,
    /// 0 始まりのレイヤー番号。
    pub layer: u32,
    /// レイヤー名。省略時は変更しない。
    #[serde(default)]
    pub name: Option<LayerNameChange>,
    /// 表示の有効・無効。省略時は変更しない。
    #[serde(default)]
    pub enabled: Option<bool>,
    /// ロックの有無。省略時は変更しない。
    #[serde(default)]
    pub locked: Option<bool>,
    /// 応答が返した `project_epoch`。
    ///
    /// レイヤーは selector を持たないため、プロジェクト境界を照合する唯一の
    /// 材料である。
    pub expected_project_epoch: String,
}

impl SetLayerStateParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// 3 つ全ての省略は拒否する。何も変更しない編集要求は、成功したのか
    /// 無視されたのかをクライアントが区別できない。
    pub fn validate(&self) -> Result<(), EditInputError> {
        if self.name.is_none() && self.enabled.is_none() && self.locked.is_none() {
            return Err(EditInputError::NoChangeRequested {
                fields: &[FIELD_NAME, FIELD_ENABLED, FIELD_LOCKED],
            });
        }
        validate_position(FIELD_LAYER, self.layer)?;
        match &self.name {
            Some(name) => name.validate(),
            None => Ok(()),
        }
    }
}

/// レイヤーの状態変更の結果。
///
/// [`LayerInfo`] は読み取りの DTO をそのまま用いる。編集専用の対称型を作ると、
/// クライアントが読み取りと編集の結果を同じ経路で扱えなくなる。
///
/// レイヤーの名前・表示・ロックはプロジェクトへ保存される内容であるため、
/// この変更は revision を進める。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerStateOutcome {
    /// 変更後のプロジェクトの epoch。
    pub project_epoch: String,
    /// 変更を反映したあとの revision。
    pub project_revision: u64,
    /// 変更後に読み直したレイヤーの状態。
    pub layer: LayerInfo,
}
