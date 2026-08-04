//! 編集 operation の params / result と、要求内容だけで決まる入力検証。
//!
//! params は未知フィールドを拒否する。ただし内側の
//! [`ObjectSelector`] / [`EffectSelector`] は応答が返した値をそのまま送り返す
//! 往復型であり、未知フィールドを拒否しない（応答へ optional field が増えた
//! ときに往復が壊れるため）。
//!
//! プロジェクト境界の照合は、対象を指す [`ObjectSelector`] が運ぶ
//! `project_epoch` で行う。同じ意味の値を 1 要求の 2 か所へ置くと不整合な組を
//! 作れてしまうため、selector を持つ operation は境界の照合用に epoch を別途
//! 受け取らない。selector を持たない [`CreateObjectParams`]（対象がまだ無い）と
//! [`SetSelectionParams`]（`focus` を省略できる）だけが
//! `expected_project_epoch` を持つ。
//!
//! 応答は読み取りの DTO（[`ObjectSummary`] / [`EffectInfo`] / [`Cursor`] /
//! [`FrameRange`]）を再利用する。編集専用の対称型を作ると、クライアントが
//! 読み取りと編集の結果を同じ経路で扱えなくなる。
//!
//! opaque handle は params にも result にも現れない。

use crate::edit_info::{Cursor, DisplayRange, FrameRange};
use crate::effect::EffectInfo;
use crate::error::ErrorCode;
use crate::item_value::{ItemValue, ItemWriteError, validate_item_value};
use crate::object::{LayerInfo, ObjectSummary, SectionRange};
use crate::selector::{EffectSelector, ObjectSelector};
use crate::validation::{
    PathSyntaxError, TextSyntaxError, validate_alias, validate_control_free, validate_name,
    validate_path,
};
use serde::{Deserialize, Serialize};

/// 作成の配置先。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    /// 現在シーンの一致確認に使う guard。
    pub scene_id: i32,
    /// 0 始まりのレイヤー番号。
    pub layer: u32,
    /// 0 始まりの開始フレーム番号。
    pub frame: u32,
}

impl Placement {
    /// 位置指定の範囲を検証する。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_position(FIELD_LAYER, self.layer)?;
        validate_position(FIELD_FRAME, self.frame)
    }
}

/// 移動の宛先。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    /// 0 始まりのレイヤー番号。
    pub layer: u32,
    /// 0 始まりの開始フレーム番号。
    pub frame: u32,
}

impl Destination {
    /// 位置指定の範囲を検証する。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_position(FIELD_LAYER, self.layer)?;
        validate_position(FIELD_FRAME, self.frame)
    }
}

/// 作成元。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObjectSource {
    /// メディアファイルから作成する。
    MediaFile {
        /// 絶対パス。相対パス・device path・代替データストリーム・
        /// ネットワークパス（UNC）は受け付けない。
        path: String,
    },
    /// object alias から作成する。
    ObjectAlias {
        /// alias ファイルと同じ形式の文字列。
        ///
        /// 複数のオブジェクトを含む alias は全てが作成される。
        alias: String,
    },
}

impl ObjectSource {
    /// 作成元の構文と大きさを検証する。
    pub fn validate(&self) -> Result<(), EditInputError> {
        match self {
            ObjectSource::MediaFile { path } => validate_path_field(FIELD_PATH, path),
            ObjectSource::ObjectAlias { alias } => {
                validate_alias(alias).map_err(|source| EditInputError::Text {
                    field: FIELD_ALIAS,
                    source,
                })
            }
        }
    }
}

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
        validate_position(FIELD_LAYER, self.layer)?;
        validate_position(FIELD_FRAME, self.frame)
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
        validate_position(FIELD_LAYER, self.layer)?;
        validate_position(FIELD_FRAME, self.frame)
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

/// レイヤー名の変更。
///
/// 標準名へ戻す指定は値を持たないが、判別子だけを持つ**構造体 variant**として
/// 表す。理由は [`RangeChange`] と同じで、unit variant は判別子以外の
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

/// `create_object` の params。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateObjectParams {
    /// 作成元。
    pub source: ObjectSource,
    /// 配置先。
    pub placement: Placement,
    /// 応答が返した `project_epoch`。
    ///
    /// 作成は対象を指すセレクターを持たないため、プロジェクト境界を照合する
    /// 唯一の材料である。
    pub expected_project_epoch: String,
}

impl CreateObjectParams {
    /// 要求内容だけで決まる検証を行う。
    pub fn validate(&self) -> Result<(), EditInputError> {
        self.source.validate()?;
        self.placement.validate()
    }
}

/// `move_object` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveObjectParams {
    /// 対象オブジェクト。
    pub selector: ObjectSelector,
    /// 移動先。
    pub destination: Destination,
}

impl MoveObjectParams {
    /// 要求内容だけで決まる検証を行う。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_selector_position(&self.selector)?;
        self.destination.validate()
    }
}

/// `delete_object` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteObjectParams {
    /// 対象オブジェクト。
    pub selector: ObjectSelector,
}

impl DeleteObjectParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// 対象の解決と前提条件の照合は変更を適用する側が行う。ここで見るのは
    /// セレクターが持つ位置指定の範囲だけである。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_selector_position(&self.selector)
    }
}

/// `set_object_name` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetObjectNameParams {
    /// 対象オブジェクト。
    pub selector: ObjectSelector,
    /// 新しい名前。`null` と省略はどちらも標準名へ戻すことを意味する。
    #[serde(default)]
    pub name: Option<String>,
}

impl SetObjectNameParams {
    /// 要求内容だけで決まる検証を行う。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_selector_position(&self.selector)?;
        match &self.name {
            Some(name) => validate_name(name).map_err(|source| EditInputError::Text {
                field: FIELD_NAME,
                source,
            }),
            None => Ok(()),
        }
    }
}

/// `set_object_item` の params。
///
/// オブジェクトの設定項目は必ずいずれかの effect に属するため、対象は
/// [`EffectSelector`] で指す。トラックバー項目の値も同じ経路を通る。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetObjectItemParams {
    /// 設定項目を持つ effect。
    pub selector: EffectSelector,
    /// 設定項目名。
    pub item: String,
    /// 設定する値。
    pub value: ItemValue,
}

impl SetObjectItemParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// 設定項目の実在と種別との対応は、対象 effect の設定項目一覧を持つ層が
    /// 判定する。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_effect_selector_position(&self.selector)?;
        validate_name(&self.item).map_err(|source| EditInputError::Text {
            field: FIELD_ITEM,
            source,
        })?;
        validate_item_value(&self.value)?;
        Ok(())
    }
}

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
    /// 見るのは [`DeleteObjectParams::validate`] と同じく、セレクターが持つ
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

/// 構造を変更する編集の結果。
///
/// [`ObjectSummary`] / [`EffectInfo`] は selector と fingerprint を内包する
/// ため、応答だけで次の編集を組み立てられる。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditOutcome {
    /// 変更後のプロジェクトの epoch。
    pub project_epoch: String,
    /// 変更を反映したあとの revision。
    pub project_revision: u64,
    /// 変更後の対象オブジェクト。削除では null。
    pub object: Option<ObjectSummary>,
    /// effect を対象とする operation でのみ設定する。削除では null。
    pub effect: Option<EffectInfo>,
    /// 作成で生まれた全てのオブジェクト。作成以外では空。
    ///
    /// 複数オブジェクトを含む alias では 2 件以上になる。`object` はその
    /// 先頭を指す。
    #[serde(default)]
    pub created: Vec<ObjectSummary>,
}

impl EditOutcome {
    /// 作成の結果を組み立てる（`create_object`）。
    ///
    /// `created` に作成された全件を、`object` にその先頭を載せる。
    pub fn created(
        project_epoch: impl Into<String>,
        project_revision: u64,
        created: Vec<ObjectSummary>,
    ) -> Self {
        Self {
            project_epoch: project_epoch.into(),
            project_revision,
            object: created.first().cloned(),
            effect: None,
            created,
        }
    }

    /// オブジェクトだけを変更した結果を組み立てる
    /// （`move_object` / `set_object_name` / `delete_effect`）。
    pub fn object_changed(
        project_epoch: impl Into<String>,
        project_revision: u64,
        object: ObjectSummary,
    ) -> Self {
        Self {
            project_epoch: project_epoch.into(),
            project_revision,
            object: Some(object),
            effect: None,
            created: Vec::new(),
        }
    }

    /// effect を伴う変更の結果を組み立てる
    /// （`set_object_item` / `add_effect` / `set_effect_enabled`）。
    ///
    /// `effect` には読み直した値を載せる。ホスト側の正規化により要求値と
    /// 異なり得るが、それは失敗ではない。
    pub fn effect_changed(
        project_epoch: impl Into<String>,
        project_revision: u64,
        object: ObjectSummary,
        effect: EffectInfo,
    ) -> Self {
        Self {
            project_epoch: project_epoch.into(),
            project_revision,
            object: Some(object),
            effect: Some(effect),
            created: Vec::new(),
        }
    }

    /// オブジェクト削除の結果を組み立てる（`delete_object`）。
    pub fn deleted(project_epoch: impl Into<String>, project_revision: u64) -> Self {
        Self {
            project_epoch: project_epoch.into(),
            project_revision,
            object: None,
            effect: None,
            created: Vec::new(),
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

/// カーソル・選択範囲・フォーカスの状態。
///
/// `set_selection` だけが返す。プロジェクトの内容を変えないため
/// [`EditOutcome`] とは別の型である。
///
/// **この変更は取り消し単位を作らない。** 実行後に取り消し操作を行うと、
/// カーソルや選択範囲ではなく、その前に行った編集が取り消される。カーソルを
/// 動かしたあとに取り消した利用者は、直前の編集を失う。
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
    pub applied: Vec<SelectionField>,
    /// 要求されたが適用できなかった項目。
    ///
    /// `applied` の補集合をクライアントに求めない。補集合は自身が送った要求と
    /// 突き合わせなければ出せず、突き合わせを誤れば「反映されたと思い込んだ
    /// まま次の編集を組み立てる」ことになる。適用の可否は必ずこの 2 つで
    /// 完結して伝える。
    #[serde(default)]
    pub not_applied: Vec<SelectionField>,
    /// 反映値が編集と原子的に観測されたものではないことを示す。
    ///
    /// 常に `true` である。反映値は編集の区間を抜けたあとの読み取りで得る
    /// ため、観測までの間に他所からの変更が入り得る。将来、区間内での
    /// 再読み取りが可能になったときに原子的な観測へ切り替えられるよう、
    /// 値の意味をクライアントが解釈できる形で残す。
    pub observed_after_edit: bool,
}

/// 編集の区間を抜けたあとに読み取った選択状態の値。
///
/// [`SelectionState`] の反映値はいずれも同じ 1 回の読み取りから来る。組にして
/// 渡すことで、別々の時点で読んだ値を混ぜて組み立てられない形にする。
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
            observed_after_edit: true,
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

/// `layer` フィールド名。
const FIELD_LAYER: &str = "layer";
/// `frame` フィールド名。
const FIELD_FRAME: &str = "frame";
/// 選択範囲の開始フレームのフィールド名。
const FIELD_RANGE_START: &str = "selected_range.start";
/// 選択範囲の終了フレームのフィールド名。
const FIELD_RANGE_END: &str = "selected_range.end";
/// `path` フィールド名。
const FIELD_PATH: &str = "path";
/// `alias` フィールド名。
const FIELD_ALIAS: &str = "alias";
/// `name` フィールド名。
const FIELD_NAME: &str = "name";
/// `item` フィールド名。
const FIELD_ITEM: &str = "item";
/// `effect_name` フィールド名。
const FIELD_EFFECT_NAME: &str = "effect_name";
/// `enabled` フィールド名。
const FIELD_ENABLED: &str = "enabled";
/// `locked` フィールド名。
const FIELD_LOCKED: &str = "locked";
/// `cursor` フィールド名。
const FIELD_CURSOR: &str = "cursor";
/// `selected_range` フィールド名。
const FIELD_SELECTED_RANGE: &str = "selected_range";
/// `focus` フィールド名。
const FIELD_FOCUS: &str = "focus";
/// `display` フィールド名。
const FIELD_DISPLAY: &str = "display";
/// `section` フィールド名。
const FIELD_SECTION: &str = "section";
/// セレクターのレイヤー番号のフィールド名。
const FIELD_SELECTOR_LAYER: &str = "selector.layer";
/// セレクターの開始フレーム番号のフィールド名。
const FIELD_SELECTOR_FRAME: &str = "selector.frame";

/// 要求内容だけで決まる検証の失敗。
///
/// 呼び出し側は [`EditInputError::error_code`] でエラーコードへ写す。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditInputError {
    /// 位置指定が受け付けられる範囲を超えている。
    #[error("{field} は {max} 以下である必要があります: {value}")]
    PositionOutOfRange {
        /// 対象フィールド名。
        field: &'static str,
        /// 指定された値。
        value: u32,
        /// 許容する最大値。
        max: u32,
    },
    /// セレクターが持つ位置指定が受け付けられる範囲を超えている。
    #[error("{field} は {max} 以下である必要があります: {value}")]
    IndexOutOfRange {
        /// 対象フィールド名。
        field: &'static str,
        /// 指定された値。
        value: usize,
        /// 許容する最大値。
        max: usize,
    },
    /// 区間番号が中間点を開始位置に持つ区間を指していない。
    ///
    /// 区間 0 の開始位置はオブジェクトの開始フレームであって中間点ではない。
    /// 対象の状態に依らず常に誤りであり、読み直しても 0 が有効になることは
    /// 無いため、前提条件の不整合ではなく要求の誤りとして扱う。
    #[error("{field} は 1 以上である必要があります: {value}")]
    SectionIndexOutOfRange {
        /// 対象フィールド名。
        field: &'static str,
        /// 指定された値。
        value: u32,
    },
    /// 変更内容が 1 つも指定されていない。
    #[error("{} のいずれかを指定する必要があります", fields.join(" / "))]
    NoChangeRequested {
        /// いずれかの指定が要るフィールド名。
        fields: &'static [&'static str],
    },
    /// 文字列の検証に失敗した。
    #[error("{field}: {source}")]
    Text {
        /// 対象フィールド名。
        field: &'static str,
        /// 失敗の内容。
        #[source]
        source: TextSyntaxError,
    },
    /// パスの検証に失敗した。
    #[error("{field}: {source}")]
    Path {
        /// 対象フィールド名。
        field: &'static str,
        /// 失敗の内容。
        #[source]
        source: PathSyntaxError,
    },
    /// 設定項目の値の検証に失敗した。
    #[error(transparent)]
    ItemValue(#[from] ItemWriteError),
}

impl EditInputError {
    /// 全 variant の代表値。
    ///
    /// [`EditInputError::reason`] が返し得る名前を数え上げるために用いる。
    /// `const` にできないのは、包む失敗が所有文字列を含むためである。
    /// 構文検証と設定値の検証を包む variant は、包む側の全種別を並べる。
    pub fn all() -> Vec<EditInputError> {
        let mut all = vec![
            EditInputError::PositionOutOfRange {
                field: FIELD_LAYER,
                value: 0,
                max: MAX_POSITION,
            },
            EditInputError::IndexOutOfRange {
                field: FIELD_SELECTOR_LAYER,
                value: 0,
                max: MAX_POSITION as usize,
            },
            EditInputError::SectionIndexOutOfRange {
                field: FIELD_SECTION,
                value: 0,
            },
            EditInputError::NoChangeRequested {
                fields: &[FIELD_NAME, FIELD_ENABLED, FIELD_LOCKED],
            },
        ];
        all.extend(
            TextSyntaxError::ALL
                .iter()
                .map(|source| EditInputError::Text {
                    field: FIELD_NAME,
                    source: *source,
                }),
        );
        all.extend(
            PathSyntaxError::ALL
                .iter()
                .map(|source| EditInputError::Path {
                    field: FIELD_PATH,
                    source: *source,
                }),
        );
        all.extend(
            ItemWriteError::all()
                .into_iter()
                .map(EditInputError::ItemValue),
        );
        all
    }

    /// 失敗の種別を表す機械可読な名前を返す。名前を持たない失敗では `None`。
    ///
    /// 名前は種別だけを表し、検証に落ちたパスも文字列も含まない。どのフィールド
    /// で落ちたかは説明の文面が担う。
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            EditInputError::Text { source, .. } => Some(source.reason()),
            EditInputError::Path { source, .. } => Some(source.reason()),
            EditInputError::ItemValue(error) => error.reason(),
            // 名前が指すのは「区間番号が中間点を開始位置に持たない」という事実
            // であり、同じ事実を実態と照合して見つけた場合の失敗と同じ名前を
            // 名乗る。復帰できるかどうかはエラーコードの側が区別する。
            EditInputError::SectionIndexOutOfRange { .. } => Some("section_index_out_of_range"),
            EditInputError::PositionOutOfRange { .. }
            | EditInputError::IndexOutOfRange { .. }
            | EditInputError::NoChangeRequested { .. } => None,
        }
    }

    /// 対応するエラーコードを返す。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            EditInputError::ItemValue(error) => error.error_code(),
            EditInputError::PositionOutOfRange { .. }
            | EditInputError::IndexOutOfRange { .. }
            | EditInputError::SectionIndexOutOfRange { .. }
            | EditInputError::NoChangeRequested { .. }
            | EditInputError::Text { .. }
            | EditInputError::Path { .. } => ErrorCode::InvalidArgument,
        }
    }
}

/// 位置指定の上限。
///
/// レイヤー番号とフレーム番号は `i32` で受け渡され、0 以上しか意味を持た
/// ないため、符号なしで受けたうえで `i32` に収まることだけを課す。
/// **上限をこれ以上狭めない。** レイヤーの実際の上限はホストが持ち、
/// 「オブジェクトが存在する最大レイヤー」は作成可能な上限ではない。空の
/// レイヤーへの作成を要求内容だけの推測で拒否しない。範囲外の指定は
/// ホストが失敗させる。
const MAX_POSITION: u32 = i32::MAX as u32;

/// 位置指定が受け渡せる範囲に収まることを確認する。
fn validate_position(field: &'static str, value: u32) -> Result<(), EditInputError> {
    if value > MAX_POSITION {
        return Err(EditInputError::PositionOutOfRange {
            field,
            value,
            max: MAX_POSITION,
        });
    }
    Ok(())
}

/// 区間番号が中間点を指し得る範囲に収まることを確認する。
///
/// 見るのは 0 でないことと受け渡せる範囲に収まることだけである。区間の総数との
/// 比較は対象の現在の状態を要するため、変更を適用する側が行う。
fn validate_section(value: u32) -> Result<(), EditInputError> {
    if value == 0 {
        return Err(EditInputError::SectionIndexOutOfRange {
            field: FIELD_SECTION,
            value,
        });
    }
    validate_position(FIELD_SECTION, value)
}

/// セレクターの位置指定が受け渡せる範囲に収まることを確認する。
///
/// セレクターは応答が返した値をそのまま送り返す往復型であり、正常な値は必ず
/// 範囲内に収まる。それは**信頼の前提であって検証ではない**。範囲外の値をその
/// まま解決へ渡すと、対象の探索が整数変換で落ちて SDK の失敗として返る。範囲外は
/// 要求の誤りであって SDK の失敗ではないうえ、呼ばれてもいない SDK 関数を
/// 名指しする補助情報が付く。
fn validate_selector_position(selector: &ObjectSelector) -> Result<(), EditInputError> {
    validate_index(FIELD_SELECTOR_LAYER, selector.layer)?;
    validate_index(FIELD_SELECTOR_FRAME, selector.frame)
}

/// effect セレクターが含む位置指定を検証する。
fn validate_effect_selector_position(selector: &EffectSelector) -> Result<(), EditInputError> {
    validate_selector_position(&selector.object)
}

/// 添字が受け渡せる範囲に収まることを確認する。
fn validate_index(field: &'static str, value: usize) -> Result<(), EditInputError> {
    let max = MAX_POSITION as usize;
    if value > max {
        return Err(EditInputError::IndexOutOfRange { field, value, max });
    }
    Ok(())
}

/// パスの構文と、そのまま渡せる文字列かを確認する。
fn validate_path_field(field: &'static str, path: &str) -> Result<(), EditInputError> {
    validate_path(path).map_err(|source| EditInputError::Path { field, source })?;
    validate_control_free(path).map_err(|source| EditInputError::Text { field, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::REASON_VALUES;
    use crate::fingerprint::{EffectFingerprintInput, ObjectFingerprintInput};
    use crate::number::FiniteF64;
    use crate::validation::{MAX_ALIAS_BYTES, MAX_NAME_UTF16_UNITS, MAX_PATH_UTF16_UNITS};
    use serde_json::{Value, json};

    const EPOCH: &str = "78be92d1-c8c9-44c6-ae52-387548971468";

    /// variant を表す名前を返す。
    ///
    /// 網羅 match で書く。variant を足すとここがコンパイルエラーになり、
    /// すぐ下の一覧と [`EditInputError::all`] へ足す必要があることが分かる。
    fn input_variant_name(error: &EditInputError) -> &'static str {
        match error {
            EditInputError::PositionOutOfRange { .. } => "PositionOutOfRange",
            EditInputError::IndexOutOfRange { .. } => "IndexOutOfRange",
            EditInputError::SectionIndexOutOfRange { .. } => "SectionIndexOutOfRange",
            EditInputError::NoChangeRequested { .. } => "NoChangeRequested",
            EditInputError::Text { .. } => "Text",
            EditInputError::Path { .. } => "Path",
            EditInputError::ItemValue(_) => "ItemValue",
        }
    }

    #[test]
    fn all_input_failures_cover_every_variant() {
        const VARIANTS: &[&str] = &[
            "PositionOutOfRange",
            "IndexOutOfRange",
            "SectionIndexOutOfRange",
            "NoChangeRequested",
            "Text",
            "Path",
            "ItemValue",
        ];
        let covered: Vec<&str> = EditInputError::all()
            .iter()
            .map(input_variant_name)
            .collect();
        for variant in VARIANTS {
            assert!(
                covered.contains(variant),
                "{variant} の代表値が一覧にありません"
            );
        }
        for variant in &covered {
            assert!(
                VARIANTS.contains(variant),
                "{variant} が網羅すべき variant の一覧にありません"
            );
        }
    }

    #[test]
    fn all_input_failures_cover_every_reason() {
        // 名前は包む側の種別で決まる。包む側に種別が増えたとき、一覧が
        // 追随していなければここで落ちる。
        let reasons: Vec<Option<&str>> = EditInputError::all()
            .iter()
            .map(EditInputError::reason)
            .collect();
        for source in TextSyntaxError::ALL {
            assert!(reasons.contains(&Some(source.reason())), "{source}");
        }
        for source in PathSyntaxError::ALL {
            assert!(reasons.contains(&Some(source.reason())), "{source}");
        }
        for source in ItemWriteError::all() {
            assert!(reasons.contains(&source.reason()), "{source}");
        }
    }

    #[test]
    fn input_failures_carry_the_reason_of_the_syntax_error_they_wrap() {
        // 検証の実体は core にあり、失敗の種別名も core が持つ。要求元へ
        // 届けるのは既にある名前であって、経路ごとに付け直す名前ではない。
        for error in PathSyntaxError::ALL {
            let mapped = EditInputError::Path {
                field: FIELD_PATH,
                source: *error,
            };
            assert_eq!(mapped.reason(), Some(error.reason()), "{error}");
            assert!(REASON_VALUES.contains(&error.reason()));
        }
        for error in TextSyntaxError::ALL {
            let mapped = EditInputError::Text {
                field: FIELD_NAME,
                source: *error,
            };
            assert_eq!(mapped.reason(), Some(error.reason()), "{error}");
            assert!(REASON_VALUES.contains(&error.reason()));
        }
        assert_eq!(
            EditInputError::ItemValue(ItemWriteError::Path(PathSyntaxError::UncPath)).reason(),
            Some("unc_path")
        );
    }

    #[test]
    fn position_failures_have_no_reason() {
        // 範囲外の位置は対象フィールド名と上限で説明が尽きる。名前を足しても
        // 要求元が取れる行動は変わらない。
        for error in [
            EditInputError::PositionOutOfRange {
                field: FIELD_LAYER,
                value: 0,
                max: 0,
            },
            EditInputError::IndexOutOfRange {
                field: FIELD_SELECTOR_LAYER,
                value: 0,
                max: 0,
            },
            EditInputError::NoChangeRequested {
                fields: &["enabled"],
            },
        ] {
            assert_eq!(error.reason(), None, "{error}");
        }
    }

    fn sample_summary() -> ObjectSummary {
        ObjectSummary::new(
            EPOCH,
            ObjectFingerprintInput {
                scene_id: 0,
                layer: 2,
                frame_start: 120,
                frame_end: 240,
                name: Some("立ち絵"),
                alias: "alias",
            },
        )
    }

    fn sample_object_selector() -> ObjectSelector {
        sample_summary().selector
    }

    fn sample_effect_info() -> EffectInfo {
        EffectInfo::new(
            sample_object_selector(),
            EffectFingerprintInput {
                effect_name: "動画ファイル",
                effect_index: 0,
                position: 0,
                effect_count: 1,
                enabled: true,
                locked: false,
                items: &[],
            },
        )
    }

    fn sample_effect_selector() -> EffectSelector {
        sample_effect_info().selector
    }

    fn sample_create() -> CreateObjectParams {
        CreateObjectParams {
            source: ObjectSource::MediaFile {
                path: r"C:\movie.mp4".to_string(),
            },
            placement: Placement {
                scene_id: 0,
                layer: 2,
                frame: 120,
            },
            expected_project_epoch: EPOCH.to_string(),
        }
    }

    fn sample_move() -> MoveObjectParams {
        MoveObjectParams {
            selector: sample_object_selector(),
            destination: Destination {
                layer: 3,
                frame: 240,
            },
        }
    }

    fn sample_set_object_item() -> SetObjectItemParams {
        SetObjectItemParams {
            selector: sample_effect_selector(),
            item: "X".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new(12.5).unwrap(),
            },
        }
    }

    fn sample_set_layer_state() -> SetLayerStateParams {
        SetLayerStateParams {
            expected_scene_id: 0,
            layer: 2,
            name: Some(LayerNameChange::Set {
                name: "背景".to_string(),
            }),
            enabled: Some(false),
            locked: Some(true),
            expected_project_epoch: EPOCH.to_string(),
        }
    }

    fn sample_set_selection() -> SetSelectionParams {
        SetSelectionParams {
            expected_scene_id: 0,
            cursor: Some(CursorPosition {
                layer: 2,
                frame: 120,
            }),
            selected_range: Some(RangeChange::Set { start: 10, end: 20 }),
            focus: Some(FocusChange::Set {
                object: sample_object_selector(),
            }),
            display: Some(DisplayStart {
                layer: 1,
                frame: 60,
            }),
            expected_project_epoch: EPOCH.to_string(),
        }
    }

    fn sample_display_range() -> DisplayRange {
        DisplayRange {
            frame_start: 60,
            layer_start: 1,
            frame_num: 600,
            layer_num: 10,
        }
    }

    /// params を JSON へ写し、未知フィールドを足した値を返す。
    fn with_unknown_field(value: &impl Serialize) -> Value {
        let mut value = serde_json::to_value(value).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(1));
        value
    }

    /// JSON から 1 つのキーを取り除いた値を返す。
    fn without_field(value: &impl Serialize, key: &str) -> Value {
        let mut value = serde_json::to_value(value).unwrap();
        assert!(
            value.as_object_mut().unwrap().remove(key).is_some(),
            "{key} が存在しません"
        );
        value
    }

    /// JSON への往復で値が変わらないことを確かめる。
    fn assert_roundtrip<T>(params: T)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let s = serde_json::to_string(&params).unwrap();
        let restored: T = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, params);
    }

    #[test]
    fn params_roundtrip() {
        assert_roundtrip(sample_create());
        assert_roundtrip(CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "[vo]\n_name=立ち絵\n".to_string(),
            },
            ..sample_create()
        });
        assert_roundtrip(sample_move());
        assert_roundtrip(DeleteObjectParams {
            selector: sample_object_selector(),
        });
        assert_roundtrip(SetObjectNameParams {
            selector: sample_object_selector(),
            name: Some("立ち絵".to_string()),
        });
        assert_roundtrip(SetObjectNameParams {
            selector: sample_object_selector(),
            name: None,
        });
        assert_roundtrip(sample_set_object_item());
        assert_roundtrip(AddEffectParams {
            object: sample_object_selector(),
            effect_name: "ぼかし".to_string(),
        });
        assert_roundtrip(DeleteEffectParams {
            selector: sample_effect_selector(),
        });
        assert_roundtrip(SetEffectEnabledParams {
            selector: sample_effect_selector(),
            enabled: false,
        });
        assert_roundtrip(sample_set_selection());
        assert_roundtrip(SetSelectionParams {
            selected_range: Some(RangeChange::Clear {}),
            focus: Some(FocusChange::Clear {}),
            ..sample_set_selection()
        });
        assert_roundtrip(sample_set_layer_state());
        assert_roundtrip(SetLayerStateParams {
            name: Some(LayerNameChange::Reset {}),
            enabled: None,
            locked: None,
            ..sample_set_layer_state()
        });
    }

    #[test]
    fn params_reject_unknown_fields() {
        macro_rules! assert_rejects_unknown {
            ($type:ty, $params:expr) => {
                assert!(
                    serde_json::from_value::<$type>(with_unknown_field(&$params)).is_err(),
                    "{} が未知フィールドを受理しました",
                    stringify!($type)
                );
            };
        }

        assert_rejects_unknown!(CreateObjectParams, sample_create());
        assert_rejects_unknown!(MoveObjectParams, sample_move());
        assert_rejects_unknown!(
            DeleteObjectParams,
            DeleteObjectParams {
                selector: sample_object_selector(),
            }
        );
        assert_rejects_unknown!(
            SetObjectNameParams,
            SetObjectNameParams {
                selector: sample_object_selector(),
                name: None,
            }
        );
        assert_rejects_unknown!(SetObjectItemParams, sample_set_object_item());
        assert_rejects_unknown!(
            AddEffectParams,
            AddEffectParams {
                object: sample_object_selector(),
                effect_name: "ぼかし".to_string(),
            }
        );
        assert_rejects_unknown!(
            DeleteEffectParams,
            DeleteEffectParams {
                selector: sample_effect_selector(),
            }
        );
        assert_rejects_unknown!(
            SetEffectEnabledParams,
            SetEffectEnabledParams {
                selector: sample_effect_selector(),
                enabled: true,
            }
        );
        assert_rejects_unknown!(SetSelectionParams, sample_set_selection());
        assert_rejects_unknown!(SetLayerStateParams, sample_set_layer_state());
        assert_rejects_unknown!(
            LayerNameChange,
            LayerNameChange::Set {
                name: "背景".to_string(),
            }
        );
        assert_rejects_unknown!(LayerNameChange, LayerNameChange::Reset {});
        assert_rejects_unknown!(
            Placement,
            Placement {
                scene_id: 0,
                layer: 0,
                frame: 0,
            }
        );
        assert_rejects_unknown!(Destination, Destination { layer: 0, frame: 0 });
        assert_rejects_unknown!(CursorPosition, CursorPosition { layer: 0, frame: 0 });
        assert_rejects_unknown!(
            ObjectSource,
            ObjectSource::MediaFile {
                path: r"C:\movie.mp4".to_string(),
            }
        );
        assert_rejects_unknown!(RangeChange, RangeChange::Set { start: 0, end: 1 });
        assert_rejects_unknown!(RangeChange, RangeChange::Clear {});
        assert_rejects_unknown!(
            FocusChange,
            FocusChange::Set {
                object: sample_object_selector(),
            }
        );
    }

    #[test]
    fn params_reject_missing_required_fields() {
        for key in ["source", "placement", "expected_project_epoch"] {
            assert!(
                serde_json::from_value::<CreateObjectParams>(without_field(&sample_create(), key))
                    .is_err(),
                "{key} の欠落が受理されました"
            );
        }
        for key in ["selector", "destination"] {
            assert!(
                serde_json::from_value::<MoveObjectParams>(without_field(&sample_move(), key))
                    .is_err(),
                "{key} の欠落が受理されました"
            );
        }
        for key in ["selector", "item", "value"] {
            assert!(
                serde_json::from_value::<SetObjectItemParams>(without_field(
                    &sample_set_object_item(),
                    key
                ))
                .is_err(),
                "{key} の欠落が受理されました"
            );
        }
        let set_effect_enabled = SetEffectEnabledParams {
            selector: sample_effect_selector(),
            enabled: false,
        };
        for key in ["selector", "enabled"] {
            assert!(
                serde_json::from_value::<SetEffectEnabledParams>(without_field(
                    &set_effect_enabled,
                    key
                ))
                .is_err(),
                "{key} の欠落が受理されました"
            );
        }
        for key in ["expected_scene_id", "expected_project_epoch"] {
            assert!(
                serde_json::from_value::<SetSelectionParams>(without_field(
                    &sample_set_selection(),
                    key
                ))
                .is_err(),
                "{key} の欠落が受理されました"
            );
        }
        for key in ["expected_scene_id", "layer", "expected_project_epoch"] {
            assert!(
                serde_json::from_value::<SetLayerStateParams>(without_field(
                    &sample_set_layer_state(),
                    key
                ))
                .is_err(),
                "{key} の欠落が受理されました"
            );
        }
    }

    #[test]
    fn optional_fields_may_be_omitted() {
        // 省略と null の明示はどちらも標準名へ戻すことを意味する。
        let omitted: SetObjectNameParams = serde_json::from_value(json!({
            "selector": serde_json::to_value(sample_object_selector()).unwrap(),
        }))
        .unwrap();
        let explicit: SetObjectNameParams = serde_json::from_value(json!({
            "selector": serde_json::to_value(sample_object_selector()).unwrap(),
            "name": Value::Null,
        }))
        .unwrap();
        assert_eq!(omitted, explicit);
        assert_eq!(omitted.name, None);
    }

    #[test]
    fn nested_selectors_still_accept_unknown_fields() {
        // params が未知フィールドを拒否しても、往復型である selector の
        // 扱いは変わらない。応答へ optional field が増えても往復が壊れない。
        let mut value = serde_json::to_value(sample_move()).unwrap();
        value["selector"]
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(1));
        let restored: MoveObjectParams = serde_json::from_value(value).unwrap();
        assert_eq!(restored.selector, sample_object_selector());

        let mut value = serde_json::to_value(sample_set_object_item()).unwrap();
        value["selector"]
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(1));
        value["selector"]["object"]
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(2));
        let restored: SetObjectItemParams = serde_json::from_value(value).unwrap();
        assert_eq!(restored.selector, sample_effect_selector());

        let mut value = serde_json::to_value(sample_set_selection()).unwrap();
        value["focus"]["object"]
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(1));
        let restored: SetSelectionParams = serde_json::from_value(value).unwrap();
        assert_eq!(
            restored.focus,
            Some(FocusChange::Set {
                object: sample_object_selector(),
            })
        );
    }

    #[test]
    fn tagged_enums_use_snake_case_tags() {
        assert_eq!(
            serde_json::to_value(ObjectSource::MediaFile {
                path: r"C:\movie.mp4".to_string(),
            })
            .unwrap(),
            json!({"type": "media_file", "path": r"C:\movie.mp4"})
        );
        assert_eq!(
            serde_json::to_value(ObjectSource::ObjectAlias {
                alias: "[vo]".to_string(),
            })
            .unwrap(),
            json!({"type": "object_alias", "alias": "[vo]"})
        );
        assert_eq!(
            serde_json::to_value(RangeChange::Set { start: 1, end: 2 }).unwrap(),
            json!({"type": "set", "start": 1, "end": 2})
        );
        assert_eq!(
            serde_json::to_value(RangeChange::Clear {}).unwrap(),
            json!({"type": "clear"})
        );
        assert_eq!(
            serde_json::to_value(FocusChange::Clear {}).unwrap(),
            json!({"type": "clear"})
        );
        assert_eq!(
            serde_json::to_value(SelectionField::SelectedRange).unwrap(),
            json!("selected_range")
        );
    }

    #[test]
    fn positions_reject_values_outside_the_representable_range() {
        let over = MAX_POSITION + 1;
        assert_eq!(MAX_POSITION, 2_147_483_647);

        for (field, params) in [
            (
                FIELD_LAYER,
                CreateObjectParams {
                    placement: Placement {
                        scene_id: 0,
                        layer: over,
                        frame: 0,
                    },
                    ..sample_create()
                },
            ),
            (
                FIELD_FRAME,
                CreateObjectParams {
                    placement: Placement {
                        scene_id: 0,
                        layer: 0,
                        frame: over,
                    },
                    ..sample_create()
                },
            ),
        ] {
            assert_eq!(
                params.validate(),
                Err(EditInputError::PositionOutOfRange {
                    field,
                    value: over,
                    max: MAX_POSITION,
                })
            );
        }

        assert_eq!(
            MoveObjectParams {
                destination: Destination {
                    layer: over,
                    frame: 0,
                },
                ..sample_move()
            }
            .validate(),
            Err(EditInputError::PositionOutOfRange {
                field: FIELD_LAYER,
                value: over,
                max: MAX_POSITION,
            })
        );

        assert_eq!(
            SetSelectionParams {
                cursor: Some(CursorPosition {
                    layer: 0,
                    frame: over,
                }),
                selected_range: None,
                focus: None,
                ..sample_set_selection()
            }
            .validate(),
            Err(EditInputError::PositionOutOfRange {
                field: FIELD_FRAME,
                value: over,
                max: MAX_POSITION,
            })
        );

        assert_eq!(
            SetSelectionParams {
                cursor: None,
                selected_range: Some(RangeChange::Set {
                    start: 0,
                    end: over,
                }),
                focus: None,
                ..sample_set_selection()
            }
            .validate(),
            Err(EditInputError::PositionOutOfRange {
                field: FIELD_RANGE_END,
                value: over,
                max: MAX_POSITION,
            })
        );
    }

    #[test]
    fn positions_accept_the_upper_bound() {
        // 上限をこれ以上狭めない。
        assert_eq!(
            CreateObjectParams {
                placement: Placement {
                    scene_id: 0,
                    layer: MAX_POSITION,
                    frame: MAX_POSITION,
                },
                ..sample_create()
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn set_selection_rejects_omitting_every_change() {
        let params = SetSelectionParams {
            expected_scene_id: 0,
            cursor: None,
            selected_range: None,
            focus: None,
            display: None,
            expected_project_epoch: EPOCH.to_string(),
        };
        let error = params.validate().unwrap_err();
        assert_eq!(
            error,
            EditInputError::NoChangeRequested {
                fields: &["cursor", "selected_range", "focus", "display"],
            }
        );
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);

        assert_eq!(sample_set_selection().validate(), Ok(()));
        assert_eq!(
            SetSelectionParams {
                cursor: None,
                selected_range: None,
                focus: Some(FocusChange::Clear {}),
                display: None,
                ..sample_set_selection()
            }
            .validate(),
            Ok(())
        );
        // 表示開始位置だけの指定でも変更要求として成立する。
        assert_eq!(
            SetSelectionParams {
                cursor: None,
                selected_range: None,
                focus: None,
                display: Some(DisplayStart {
                    layer: 3,
                    frame: 90,
                }),
                ..sample_set_selection()
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn set_selection_rejects_a_display_start_outside_the_transferable_range() {
        let over = MAX_POSITION + 1;
        for (display, field) in [
            (
                DisplayStart {
                    layer: over,
                    frame: 0,
                },
                FIELD_LAYER,
            ),
            (
                DisplayStart {
                    layer: 0,
                    frame: over,
                },
                FIELD_FRAME,
            ),
        ] {
            assert_eq!(
                SetSelectionParams {
                    cursor: None,
                    selected_range: None,
                    focus: None,
                    display: Some(display),
                    ..sample_set_selection()
                }
                .validate(),
                Err(EditInputError::PositionOutOfRange {
                    field,
                    value: over,
                    max: MAX_POSITION,
                })
            );
        }
    }

    #[test]
    fn set_layer_state_rejects_omitting_every_change() {
        let params = SetLayerStateParams {
            name: None,
            enabled: None,
            locked: None,
            ..sample_set_layer_state()
        };
        let error = params.validate().unwrap_err();
        assert_eq!(
            error,
            EditInputError::NoChangeRequested {
                fields: &["name", "enabled", "locked"],
            }
        );
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);

        // 3 つの軸は個別にも組み合わせても指定できる。
        for params in [
            SetLayerStateParams {
                name: Some(LayerNameChange::Reset {}),
                enabled: None,
                locked: None,
                ..sample_set_layer_state()
            },
            SetLayerStateParams {
                name: None,
                enabled: Some(true),
                locked: None,
                ..sample_set_layer_state()
            },
            SetLayerStateParams {
                name: None,
                enabled: None,
                locked: Some(false),
                ..sample_set_layer_state()
            },
            sample_set_layer_state(),
        ] {
            assert_eq!(params.validate(), Ok(()));
        }
    }

    #[test]
    fn the_layer_name_change_is_a_struct_variant() {
        // internally tagged 表現では unit variant が deny_unknown_fields を
        // 無視し、未知フィールドを黙って読み飛ばす。
        assert_eq!(
            serde_json::to_value(LayerNameChange::Reset {}).unwrap(),
            json!({"type": "reset"})
        );
        assert_eq!(
            serde_json::to_value(LayerNameChange::Set {
                name: "背景".to_string(),
            })
            .unwrap(),
            json!({"type": "set", "name": "背景"})
        );
        assert!(
            serde_json::from_value::<LayerNameChange>(json!({"type": "reset", "name": "x"}))
                .is_err(),
            "標準名へ戻す指定が名前を読み飛ばしました"
        );
        assert!(
            serde_json::from_value::<LayerNameChange>(json!({"type": "set"})).is_err(),
            "名前を持たない設定が受理されました"
        );

        // params の内側でも同じ扱いになる。
        let mut value = serde_json::to_value(sample_set_layer_state()).unwrap();
        value["name"] = json!({"type": "reset", "name": "x"});
        assert!(serde_json::from_value::<SetLayerStateParams>(value).is_err());
    }

    #[test]
    fn set_layer_state_validates_the_layer_and_the_name() {
        let over = MAX_POSITION + 1;
        assert_eq!(
            SetLayerStateParams {
                layer: over,
                ..sample_set_layer_state()
            }
            .validate(),
            Err(EditInputError::PositionOutOfRange {
                field: FIELD_LAYER,
                value: over,
                max: MAX_POSITION,
            })
        );

        // 空の名前は標準名へ戻す指定と同じ結果になるため受け付けない。要求元が
        // 言っていない変更を、成功として返すことになる。
        assert_eq!(
            SetLayerStateParams {
                name: Some(LayerNameChange::Set {
                    name: String::new(),
                }),
                ..sample_set_layer_state()
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_NAME,
                source: TextSyntaxError::Empty,
            })
        );
        // オブジェクト名は空を標準名へ戻す指定として受け付け続ける。取り消しを
        // 表す別の指定を持たないためである。
        assert_eq!(
            SetObjectNameParams {
                selector: sample_object_selector(),
                name: Some(String::new()),
            }
            .validate(),
            Ok(())
        );

        // 名前の規則はオブジェクト名と共有する。
        assert_eq!(
            SetLayerStateParams {
                name: Some(LayerNameChange::Set {
                    name: "名\0前".to_string(),
                }),
                ..sample_set_layer_state()
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_NAME,
                source: TextSyntaxError::ContainsNul,
            })
        );

        let over = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2 + 1);
        assert!(matches!(
            SetLayerStateParams {
                name: Some(LayerNameChange::Set { name: over }),
                ..sample_set_layer_state()
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_NAME,
                source: TextSyntaxError::TooLongUtf16 { .. },
            })
        ));
    }

    #[test]
    fn the_layer_state_outcome_reuses_the_read_dto() {
        let outcome = LayerStateOutcome {
            project_epoch: EPOCH.to_string(),
            project_revision: 43,
            layer: LayerInfo {
                index: 2,
                name: Some("背景".to_string()),
                enabled: false,
                locked: true,
                object_count: 3,
            },
        };
        let value = serde_json::to_value(&outcome).unwrap();
        assert_eq!(value["project_epoch"], json!(EPOCH));
        assert_eq!(value["project_revision"], json!(43));
        assert_eq!(
            value["layer"],
            serde_json::to_value(&outcome.layer).unwrap()
        );

        let s = serde_json::to_string(&outcome).unwrap();
        assert_eq!(
            serde_json::from_str::<LayerStateOutcome>(&s).unwrap(),
            outcome
        );
        // 応答型は将来の optional field を受け入れる。
        let restored: LayerStateOutcome =
            serde_json::from_value(with_unknown_field(&outcome)).unwrap();
        assert_eq!(restored, outcome);
    }

    #[test]
    fn create_validates_the_source() {
        assert_eq!(sample_create().validate(), Ok(()));

        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::MediaFile {
                    path: r"..\movie.mp4".to_string(),
                },
                ..sample_create()
            }
            .validate(),
            Err(EditInputError::Path {
                field: FIELD_PATH,
                source: PathSyntaxError::NotAbsolute,
            })
        );

        let path = format!(r"C:\{}", "a".repeat(MAX_PATH_UTF16_UNITS));
        assert!(matches!(
            CreateObjectParams {
                source: ObjectSource::MediaFile { path },
                ..sample_create()
            }
            .validate(),
            Err(EditInputError::Path {
                source: PathSyntaxError::TooLong { .. },
                ..
            })
        ));

        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::ObjectAlias {
                    alias: "a".repeat(MAX_ALIAS_BYTES + 1),
                },
                ..sample_create()
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_ALIAS,
                source: TextSyntaxError::TooLongBytes {
                    bytes: MAX_ALIAS_BYTES + 1,
                    max: MAX_ALIAS_BYTES,
                },
            })
        );
        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::ObjectAlias {
                    alias: "a".repeat(MAX_ALIAS_BYTES),
                },
                ..sample_create()
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn path_rules_apply_to_every_field_that_carries_a_path() {
        // 作成元のパスと設定値のパスは別の型を通るため、規則が片方だけに
        // 掛かっていても個別のテストでは気付けない。
        for (path, expected) in [
            ("", PathSyntaxError::Empty),
            (r"..\movie.mp4", PathSyntaxError::NotAbsolute),
            (r"\\.\pipe\aviutl2", PathSyntaxError::DeviceNamespace),
            (r"C:\movie.mp4:stream", PathSyntaxError::AlternateDataStream),
            (r"\\server\share\movie.mp4", PathSyntaxError::UncPath),
            ("//server/share/movie.mp4", PathSyntaxError::UncPath),
        ] {
            assert_eq!(
                CreateObjectParams {
                    source: ObjectSource::MediaFile {
                        path: path.to_string(),
                    },
                    ..sample_create()
                }
                .validate(),
                Err(EditInputError::Path {
                    field: FIELD_PATH,
                    source: expected,
                }),
                "作成元の {path}"
            );

            for value in [
                ItemValue::File {
                    path: path.to_string(),
                },
                ItemValue::Folder {
                    path: path.to_string(),
                },
            ] {
                let kind = value.kind();
                assert_eq!(
                    SetObjectItemParams {
                        value,
                        ..sample_set_object_item()
                    }
                    .validate(),
                    Err(EditInputError::ItemValue(ItemWriteError::Path(expected))),
                    "{kind} の {path}"
                );
            }
        }
    }

    #[test]
    fn paths_reject_control_characters_on_top_of_the_syntax_rules() {
        // ファイル名に現れ得ない文字は、構文の規則を通っても渡さない。
        for control in ['\u{1}', '\u{1b}', '\n', '\t'] {
            let path = format!(r"C:\movie{control}.mp4");
            assert_eq!(
                validate_path(&path),
                Ok(()),
                "{control:?} が構文の規則で落ちています"
            );

            assert_eq!(
                CreateObjectParams {
                    source: ObjectSource::MediaFile { path: path.clone() },
                    ..sample_create()
                }
                .validate(),
                Err(EditInputError::Text {
                    field: FIELD_PATH,
                    source: TextSyntaxError::ContainsControl,
                }),
                "作成元の {control:?}"
            );

            assert_eq!(
                SetObjectItemParams {
                    value: ItemValue::File { path },
                    ..sample_set_object_item()
                }
                .validate(),
                Err(EditInputError::ItemValue(ItemWriteError::Text(
                    TextSyntaxError::ContainsControl
                ))),
                "設定値の {control:?}"
            );
        }
    }

    #[test]
    fn media_file_path_is_bounded_only_by_the_path_limit() {
        // 作成元のパスは設定項目の値ではないため、値としての上限は掛からない。
        let path = format!(r"C:\{}", "a".repeat(MAX_PATH_UTF16_UNITS - 3));
        assert_eq!(path.encode_utf16().count(), MAX_PATH_UTF16_UNITS);
        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::MediaFile { path },
                ..sample_create()
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn names_are_limited_in_utf16_code_units() {
        let name = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2 + 1);
        assert!(matches!(
            SetObjectNameParams {
                selector: sample_object_selector(),
                name: Some(name.clone()),
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_NAME,
                source: TextSyntaxError::TooLongUtf16 { .. },
            })
        ));
        assert!(matches!(
            AddEffectParams {
                object: sample_object_selector(),
                effect_name: name.clone(),
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_EFFECT_NAME,
                source: TextSyntaxError::TooLongUtf16 { .. },
            })
        ));
        assert!(matches!(
            SetObjectItemParams {
                item: name,
                ..sample_set_object_item()
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_ITEM,
                source: TextSyntaxError::TooLongUtf16 { .. },
            })
        ));

        let name = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2);
        assert_eq!(
            SetObjectNameParams {
                selector: sample_object_selector(),
                name: Some(name),
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn set_object_item_rejects_unknown_values() {
        let error = SetObjectItemParams {
            value: ItemValue::Unknown {
                raw: "future=1".to_string(),
            },
            ..sample_set_object_item()
        }
        .validate()
        .unwrap_err();
        assert_eq!(
            error,
            EditInputError::ItemValue(ItemWriteError::UnknownValue)
        );
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    }

    /// 各コンストラクタが埋めるフィールドの組み合わせを固定する。
    ///
    /// 固定するのは**コンストラクタの契約だけ**である。どの operation が
    /// どのコンストラクタを呼ぶかは応答を組み立てる側にあり、ここでは
    /// 検証できない。表の operation 名は、どの契約がどの用途に対応するかを
    /// 読み手へ示すための注記である。
    #[test]
    fn edit_outcome_matches_the_operation_table() {
        let created = vec![sample_summary(), sample_summary()];
        // operation ごとの object / effect / created の設定内容。
        let cases: Vec<(&str, EditOutcome, bool, bool, usize)> = vec![
            (
                "create_object",
                EditOutcome::created(EPOCH, 43, created.clone()),
                true,
                false,
                2,
            ),
            (
                "move_object",
                EditOutcome::object_changed(EPOCH, 43, sample_summary()),
                true,
                false,
                0,
            ),
            (
                "delete_object",
                EditOutcome::deleted(EPOCH, 43),
                false,
                false,
                0,
            ),
            (
                "set_object_name",
                EditOutcome::object_changed(EPOCH, 43, sample_summary()),
                true,
                false,
                0,
            ),
            (
                "set_object_item",
                EditOutcome::effect_changed(EPOCH, 43, sample_summary(), sample_effect_info()),
                true,
                true,
                0,
            ),
            (
                "add_effect",
                EditOutcome::effect_changed(EPOCH, 43, sample_summary(), sample_effect_info()),
                true,
                true,
                0,
            ),
            (
                "delete_effect",
                EditOutcome::object_changed(EPOCH, 43, sample_summary()),
                true,
                false,
                0,
            ),
            (
                "set_effect_enabled",
                EditOutcome::effect_changed(EPOCH, 43, sample_summary(), sample_effect_info()),
                true,
                true,
                0,
            ),
        ];

        for (operation, outcome, has_object, has_effect, created_count) in cases {
            assert_eq!(outcome.object.is_some(), has_object, "{operation}: object");
            assert_eq!(outcome.effect.is_some(), has_effect, "{operation}: effect");
            assert_eq!(outcome.created.len(), created_count, "{operation}: created");
            assert_eq!(outcome.project_epoch, EPOCH, "{operation}");
            assert_eq!(outcome.project_revision, 43, "{operation}");
        }
    }

    #[test]
    fn created_outcome_points_at_the_first_object() {
        let created = vec![sample_summary(), sample_summary()];
        let outcome = EditOutcome::created(EPOCH, 43, created.clone());
        assert_eq!(outcome.object.as_ref(), created.first());
        assert_eq!(outcome.created, created);

        // 作成された件数が 0 の場合は対象を名乗らない。
        let empty = EditOutcome::created(EPOCH, 43, Vec::new());
        assert_eq!(empty.object, None);
        assert!(empty.created.is_empty());
    }

    #[test]
    fn results_keep_reporting_the_project_generation() {
        // 要求から外した値でも、応答が返し続けるものはある。要求のフィールドは
        // 要求元へ組み立てを強いるが、応答のフィールドは強いない。revision は
        // `modified` の状態と変更の発生を要求元が観測する唯一の手段である。
        let outcome = serde_json::to_value(EditOutcome::effect_changed(
            EPOCH,
            43,
            sample_summary(),
            sample_effect_info(),
        ))
        .unwrap();
        assert_eq!(outcome["project_epoch"], json!(EPOCH));
        assert_eq!(outcome["project_revision"], json!(43));

        let state = serde_json::to_value(SelectionState::observed(
            EPOCH,
            42,
            ObservedSelection {
                cursor: Cursor { frame: 0, layer: 0 },
                selected_range: None,
                focus: Some(sample_summary()),
                display: sample_display_range(),
            },
            Vec::new(),
            Vec::new(),
        ))
        .unwrap();
        assert_eq!(state["project_epoch"], json!(EPOCH));
        assert_eq!(state["project_revision"], json!(42));
    }

    #[test]
    fn results_roundtrip() {
        let outcome =
            EditOutcome::effect_changed(EPOCH, 43, sample_summary(), sample_effect_info());
        let s = serde_json::to_string(&outcome).unwrap();
        assert_eq!(serde_json::from_str::<EditOutcome>(&s).unwrap(), outcome);

        let state = SelectionState::observed(
            EPOCH,
            42,
            ObservedSelection {
                cursor: Cursor {
                    frame: 120,
                    layer: 2,
                },
                selected_range: Some(FrameRange { start: 10, end: 20 }),
                focus: Some(sample_summary()),
                display: sample_display_range(),
            },
            vec![SelectionField::Cursor, SelectionField::Focus],
            Vec::new(),
        );
        let s = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<SelectionState>(&s).unwrap(), state);
    }

    #[test]
    fn results_allow_unknown_optional_fields() {
        // 応答型は将来の optional field を受け入れる。
        let outcome = EditOutcome::deleted(EPOCH, 43);
        let restored: EditOutcome = serde_json::from_value(with_unknown_field(&outcome)).unwrap();
        assert_eq!(restored, outcome);

        let state = SelectionState::observed(
            EPOCH,
            42,
            ObservedSelection {
                cursor: Cursor { frame: 0, layer: 0 },
                selected_range: None,
                focus: None,
                display: sample_display_range(),
            },
            Vec::new(),
            Vec::new(),
        );
        let restored: SelectionState = serde_json::from_value(with_unknown_field(&state)).unwrap();
        assert_eq!(restored, state);
    }

    #[test]
    fn selection_state_is_always_observed_after_the_edit() {
        let state = SelectionState::observed(
            EPOCH,
            42,
            ObservedSelection {
                cursor: Cursor { frame: 0, layer: 0 },
                selected_range: None,
                focus: None,
                display: sample_display_range(),
            },
            vec![SelectionField::Cursor],
            Vec::new(),
        );
        assert!(state.observed_after_edit);
    }

    #[test]
    fn results_do_not_expose_handles() {
        let documents = [
            serde_json::to_string(&EditOutcome::created(EPOCH, 43, vec![sample_summary()]))
                .unwrap(),
            serde_json::to_string(&EditOutcome::effect_changed(
                EPOCH,
                43,
                sample_summary(),
                sample_effect_info(),
            ))
            .unwrap(),
            serde_json::to_string(&EditOutcome::deleted(EPOCH, 43)).unwrap(),
            serde_json::to_string(&SelectionState::observed(
                EPOCH,
                42,
                ObservedSelection {
                    cursor: Cursor { frame: 0, layer: 0 },
                    selected_range: Some(FrameRange { start: 0, end: 1 }),
                    focus: Some(sample_summary()),
                    display: sample_display_range(),
                },
                vec![SelectionField::Focus],
                Vec::new(),
            ))
            .unwrap(),
        ];

        for document in documents {
            let lowered = document.to_lowercase();
            for forbidden in ["handle", "pointer", "0x", "secret", "nonce"] {
                assert!(
                    !lowered.contains(forbidden),
                    "{forbidden} が応答に含まれます: {document}"
                );
            }
        }
    }

    #[test]
    fn a_selector_position_out_of_range_is_an_invalid_argument() {
        // 往復型だから正常な値は範囲内に収まる、というのは信頼の前提であって
        // 検証ではない。範囲外をそのまま解決へ渡すと、対象の探索が整数変換で
        // 落ちて SDK の失敗として返り、呼ばれてもいない関数が名指しされる。
        let out_of_range = MAX_POSITION as usize + 1;

        let mut selector = sample_object_selector();
        selector.layer = out_of_range;
        let error = MoveObjectParams {
            selector: selector.clone(),
            destination: Destination {
                layer: 1,
                frame: 10,
            },
        }
        .validate()
        .expect_err("範囲外のレイヤー番号が受理されました");
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);

        let mut selector = sample_object_selector();
        selector.frame = out_of_range;
        let error = DeleteObjectParams { selector }
            .validate()
            .expect_err("範囲外の開始フレーム番号が受理されました");
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    }

    #[test]
    fn every_edit_input_checks_the_selectors_it_carries() {
        // ネストしたセレクターだけが検証を免れると、そこから範囲外の値が
        // 解決へ抜ける。
        let out_of_range = MAX_POSITION as usize + 1;
        let mut object = sample_object_selector();
        object.layer = out_of_range;
        let mut effect = sample_effect_selector();
        effect.object.layer = out_of_range;

        let failures: Vec<Result<(), EditInputError>> = vec![
            SetObjectNameParams {
                selector: object.clone(),
                name: None,
            }
            .validate(),
            SetObjectItemParams {
                selector: effect.clone(),
                item: "範囲".to_string(),
                value: ItemValue::Integer { value: 1 },
            }
            .validate(),
            AddEffectParams {
                object: object.clone(),
                effect_name: "ぼかし".to_string(),
            }
            .validate(),
            DeleteEffectParams {
                selector: effect.clone(),
            }
            .validate(),
            SetEffectEnabledParams {
                selector: effect,
                enabled: true,
            }
            .validate(),
            SetSelectionParams {
                expected_scene_id: 0,
                cursor: None,
                selected_range: None,
                focus: Some(FocusChange::Set { object }),
                display: None,
                expected_project_epoch: EPOCH.to_string(),
            }
            .validate(),
        ];

        for failure in failures {
            let error = failure.expect_err("範囲外のセレクターが受理されました");
            assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
        }
    }

    fn sample_create_section() -> CreateObjectSectionParams {
        CreateObjectSectionParams {
            selector: sample_object_selector(),
            frame: 180,
        }
    }

    fn sample_delete_section() -> DeleteObjectSectionParams {
        DeleteObjectSectionParams {
            selector: sample_object_selector(),
            section: 1,
        }
    }

    fn sample_move_section() -> MoveObjectSectionParams {
        MoveObjectSectionParams {
            selector: sample_object_selector(),
            section: 1,
            frame: 200,
        }
    }

    #[test]
    fn object_section_params_roundtrip() {
        assert_roundtrip(sample_create_section());
        assert_roundtrip(sample_delete_section());
        assert_roundtrip(sample_move_section());
    }

    #[test]
    fn object_section_params_reject_unknown_fields() {
        assert!(
            serde_json::from_value::<CreateObjectSectionParams>(with_unknown_field(
                &sample_create_section()
            ))
            .is_err()
        );
        assert!(
            serde_json::from_value::<DeleteObjectSectionParams>(with_unknown_field(
                &sample_delete_section()
            ))
            .is_err()
        );
        assert!(
            serde_json::from_value::<MoveObjectSectionParams>(with_unknown_field(
                &sample_move_section()
            ))
            .is_err()
        );
    }

    #[test]
    fn object_section_params_reject_a_negative_number() {
        // 負値は u32 へ復号できない。実行口へ届く前に落ちる。
        let mut value = serde_json::to_value(sample_move_section()).unwrap();
        value["frame"] = json!(-1);
        assert!(serde_json::from_value::<MoveObjectSectionParams>(value).is_err());

        let mut value = serde_json::to_value(sample_delete_section()).unwrap();
        value["section"] = json!(-1);
        assert!(serde_json::from_value::<DeleteObjectSectionParams>(value).is_err());
    }

    #[test]
    fn section_zero_is_rejected_as_an_invalid_argument() {
        // 区間 0 の開始位置はオブジェクトの開始フレームであって中間点ではない。
        // 対象を読み直しても 0 が有効になることはないため、前提条件の不整合では
        // なく要求の誤りとして返す。
        for error in [
            DeleteObjectSectionParams {
                section: 0,
                ..sample_delete_section()
            }
            .validate()
            .expect_err("区間番号 0 の削除が受理されました"),
            MoveObjectSectionParams {
                section: 0,
                ..sample_move_section()
            }
            .validate()
            .expect_err("区間番号 0 の移動が受理されました"),
        ] {
            assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
            assert_eq!(error.reason(), Some("section_index_out_of_range"));
            assert!(REASON_VALUES.contains(&"section_index_out_of_range"));
        }
    }

    #[test]
    fn section_one_is_accepted_without_knowing_the_object() {
        // 区間の総数との比較は対象の現在の状態を要する。要求内容だけの検証は
        // そこまで見ない。
        sample_delete_section()
            .validate()
            .expect("区間番号 1 の削除が拒否されました");
        sample_move_section()
            .validate()
            .expect("区間番号 1 の移動が拒否されました");
        sample_create_section()
            .validate()
            .expect("中間点の追加が拒否されました");
    }

    #[test]
    fn object_section_params_reject_values_beyond_i32() {
        for error in [
            CreateObjectSectionParams {
                frame: MAX_POSITION + 1,
                ..sample_create_section()
            }
            .validate()
            .expect_err("i32 に収まらないフレームが受理されました"),
            DeleteObjectSectionParams {
                section: MAX_POSITION + 1,
                ..sample_delete_section()
            }
            .validate()
            .expect_err("i32 に収まらない区間番号が受理されました"),
            MoveObjectSectionParams {
                frame: MAX_POSITION + 1,
                ..sample_move_section()
            }
            .validate()
            .expect_err("i32 に収まらないフレームが受理されました"),
        ] {
            assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn object_sections_outcome_roundtrip() {
        let outcome = ObjectSectionsOutcome {
            project_epoch: EPOCH.to_string(),
            project_revision: 43,
            object: sample_summary(),
            sections: vec![
                SectionRange {
                    start: 120,
                    end: 179,
                },
                SectionRange {
                    start: 180,
                    end: 240,
                },
            ],
        };
        let s = serde_json::to_string(&outcome).unwrap();
        let restored: ObjectSectionsOutcome = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, outcome);
    }

    #[test]
    fn object_sections_outcome_carries_no_alias() {
        // 応答が返すのは概要であり詳細ではない。alias も設定値も載らない。
        let value = serde_json::to_value(ObjectSectionsOutcome {
            project_epoch: EPOCH.to_string(),
            project_revision: 43,
            object: sample_summary(),
            sections: Vec::new(),
        })
        .unwrap();
        assert!(value.get("alias").is_none());
        assert!(value["object"].get("alias").is_none());
    }
}
