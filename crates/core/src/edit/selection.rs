//! カーソル・選択範囲・フォーカスの params と result。

use super::{
    EditInputError, FIELD_CURSOR, FIELD_DISPLAY, FIELD_FOCUS, FIELD_RANGE_END, FIELD_RANGE_START,
    FIELD_SELECTED_RANGE, validate_layer_frame, validate_position, validate_selector_position,
};
use crate::edit_info::{Cursor, DisplayRange, FrameRange};
use crate::object::ObjectSummary;
use crate::selector::ObjectSelector;
use serde::{Deserialize, Serialize};

/// カーソルの移動先。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorPosition {
    /// 0 始まりのレイヤー番号。
    pub layer: u32,
    /// 0 始まりのフレーム番号。
    pub frame: u32,
}

impl CursorPosition {
    /// 位置指定の範囲を検証する。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_layer_frame(self.layer, self.frame)
    }
}

/// レイヤー編集の表示開始位置。
///
/// カーソルと同じくホストが設定できる範囲へ調整するため、要求値がそのまま
/// 反映されるとは限らない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayStart {
    /// 表示開始レイヤー番号（0 始まり）。
    pub layer: u32,
    /// 表示開始フレーム番号（0 始まり）。
    pub frame: u32,
}

impl DisplayStart {
    /// 位置指定の範囲を検証する。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_layer_frame(self.layer, self.frame)
    }
}

/// 選択範囲の変更。
///
/// 解除は値を持たないが、判別子だけを持つ**構造体 variant**として表す。
/// unit variant は判別子以外のフィールドを黙って読み飛ばすため、未知
/// フィールドの拒否が効かない。ワイヤ表現はどちらも `{"type":"clear"}` で
/// 変わらない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RangeChange {
    /// 範囲を設定する。
    Set {
        /// 0 始まりの開始フレーム番号。
        start: u32,
        /// 0 始まりの終了フレーム番号。
        end: u32,
    },
    /// 範囲を解除する。
    Clear {},
}

impl RangeChange {
    /// フレーム番号の範囲を検証する。
    ///
    /// 開始と終了の前後関係は判定しない。ホストが範囲外の値をクランプする
    /// ため、要求値と反映値の差異そのものは失敗ではない。
    pub fn validate(&self) -> Result<(), EditInputError> {
        match self {
            RangeChange::Set { start, end } => {
                validate_position(FIELD_RANGE_START, *start)?;
                validate_position(FIELD_RANGE_END, *end)
            }
            RangeChange::Clear {} => Ok(()),
        }
    }
}

/// フォーカス対象の変更。
///
/// 解除を構造体 variant で表す理由は [`RangeChange`] と同じである。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FocusChange {
    /// 対象を選択する。
    Set {
        /// フォーカスするオブジェクト。
        object: ObjectSelector,
    },
    /// 選択を解除する。
    ///
    /// 解決できない対象を指定したときに黙って解除することはない。解除は
    /// この指定があるときだけ行う。
    Clear {},
}

/// `set_selection` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetSelectionParams {
    /// 現在シーンの一致確認に使う guard。
    ///
    /// カーソルと選択範囲はシーンに属する値であり、対象を指す selector を
    /// 持たない。guard が無いと、要求が想定したシーンと現在シーンの一致を
    /// 確かめる手段が無い。
    pub expected_scene_id: i32,
    /// カーソル位置。省略時は変更しない。
    #[serde(default)]
    pub cursor: Option<CursorPosition>,
    /// 選択範囲。省略時は変更しない。
    #[serde(default)]
    pub selected_range: Option<RangeChange>,
    /// フォーカス対象。省略時は変更しない。
    #[serde(default)]
    pub focus: Option<FocusChange>,
    /// レイヤー編集の表示開始位置。省略時は変更しない。
    #[serde(default)]
    pub display: Option<DisplayStart>,
    /// 応答が返した `project_epoch`。
    ///
    /// `focus` を省略した要求はセレクターを 1 つも持たないため、プロジェクト
    /// 境界を照合する材料が他に無い。
    pub expected_project_epoch: String,
}

impl SetSelectionParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// 4 つ全ての省略は拒否する。何も変更しない編集要求は、成功したのか
    /// 無視されたのかをクライアントが区別できない。
    pub fn validate(&self) -> Result<(), EditInputError> {
        if self.cursor.is_none()
            && self.selected_range.is_none()
            && self.focus.is_none()
            && self.display.is_none()
        {
            return Err(EditInputError::NoChangeRequested {
                fields: &[
                    FIELD_CURSOR,
                    FIELD_SELECTED_RANGE,
                    FIELD_FOCUS,
                    FIELD_DISPLAY,
                ],
            });
        }
        if let Some(cursor) = &self.cursor {
            cursor.validate()?;
        }
        if let Some(range) = &self.selected_range {
            range.validate()?;
        }
        if let Some(FocusChange::Set { object }) = &self.focus {
            validate_selector_position(object)?;
        }
        if let Some(display) = &self.display {
            display.validate()?;
        }
        Ok(())
    }
}

/// カーソル・選択範囲・フォーカスの状態。
///
/// `set_selection` だけが返す。プロジェクトの内容を変えないため
/// [`EditOutcome`](super::EditOutcome) とは別の型である。
///
/// **この変更は取り消し単位を作らない。** 実行後に取り消し操作を行うと、
/// カーソルや選択範囲ではなく、その前に行った編集が取り消される。カーソルを
/// 動かしたあとに取り消した利用者は、直前の編集を失う。
///
/// **反映値は編集の区間を抜けたあとの読み取りで得る。** 観測までの間に他所からの
/// 変更が入り得ることは tool 説明と text content が述べる——応答ごとに変わる値では
/// なく、この tool の性質だからである。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectionState {
    /// プロジェクトの epoch。
    pub project_epoch: String,
    /// プロジェクトの revision。
    pub project_revision: u64,
    /// 反映後のカーソル位置。
    pub cursor: Cursor,
    /// 反映後の選択範囲。未選択は null。
    pub selected_range: Option<FrameRange>,
    /// 反映後のフォーカス対象。未選択は null。
    pub focus: Option<ObjectSummary>,
    /// 反映後のタイムライン表示範囲。
    pub display: DisplayRange,
    /// 実際に適用できた項目。部分適用を伝える唯一の手段である。
    ///
    /// **「適用できた」の意味は軸によって違う。**
    ///
    /// | 軸 | ここに入る条件 |
    /// |---|---|
    /// | [`SelectionField::Cursor`] | 変更が受け付けられたこと。**要求どおりの
    ///   値になったことではない**——カーソルはシーンの範囲へ丸められる |
    /// | [`SelectionField::SelectedRange`] | 同上 |
    /// | [`SelectionField::Focus`] | 同上 |
    /// | [`SelectionField::Display`] | 変更が受け付けられ、**かつ表示開始位置が
    ///   要求どおりであること**。範囲へ丸められた場合は入らない |
    ///
    /// **違いは反映値を伝える手段の違いから来る。** カーソル・選択範囲・
    /// フォーカスは反映値そのものが同じ応答に載るため、要求どおりかは受け取った
    /// 側が読めば分かる。表示範囲（[`DisplayRange`]）は開始位置以外が概数で
    /// あり、載せた値から要求との一致を判定できない。判定できない軸だけを
    /// この一覧が肩代わりする。
    pub applied: Vec<SelectionField>,
    /// 要求されたが適用できなかった項目。
    ///
    /// 「適用できなかった」の意味は `applied` の裏返しであり、同じく軸によって
    /// 違う。表示開始位置が範囲へ丸められた場合はここへ入るが、カーソルが
    /// 丸められた場合は入らない。
    ///
    /// `applied` の補集合をクライアントに求めない。補集合は自身が送った要求と
    /// 突き合わせなければ出せず、突き合わせを誤れば「反映されたと思い込んだ
    /// まま次の編集を組み立てる」ことになる。適用の可否は必ずこの 2 つで
    /// 完結して伝える。**そのため省略も許さない**——欠けていれば受信側は
    /// 「空だった」のか「送られなかった」のかを区別できず、補集合を求めない
    /// という目的そのものが崩れる。
    pub not_applied: Vec<SelectionField>,
}

/// 編集の区間を抜けたあとに読み取った選択状態の値。
///
/// [`SelectionState`] の反映値を 1 つの引数へまとめる。**値が同じ読み取りから
/// 来ることを型が確かめるわけではない**——それは組み立てる側の責務である。
/// この型が課すのは、反映値を 1 つでも埋め忘れれば組み立てられないことだけで
/// ある。
#[derive(Debug, Clone, PartialEq)]
pub struct ObservedSelection {
    /// 反映後のカーソル位置。
    pub cursor: Cursor,
    /// 反映後の選択範囲。未選択は `None`。
    pub selected_range: Option<FrameRange>,
    /// 反映後のフォーカス対象。未選択は `None`。
    pub focus: Option<ObjectSummary>,
    /// 反映後のタイムライン表示範囲。
    pub display: DisplayRange,
}

impl SelectionState {
    /// 編集の区間を抜けたあとに観測した状態として組み立てる。
    pub fn observed(
        project_epoch: impl Into<String>,
        project_revision: u64,
        observed: ObservedSelection,
        applied: Vec<SelectionField>,
        not_applied: Vec<SelectionField>,
    ) -> Self {
        Self {
            project_epoch: project_epoch.into(),
            project_revision,
            cursor: observed.cursor,
            selected_range: observed.selected_range,
            focus: observed.focus,
            display: observed.display,
            applied,
            not_applied,
        }
    }
}

/// 選択状態のうち適用できた項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionField {
    /// カーソル位置。
    Cursor,
    /// 選択範囲。
    SelectedRange,
    /// フォーカス対象。
    Focus,
    /// レイヤー編集の表示開始位置。
    Display,
}
