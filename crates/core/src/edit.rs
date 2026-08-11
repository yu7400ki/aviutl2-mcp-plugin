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

use crate::edit_info::{Cursor, DisplayRange, FrameRange, GridBpm, SceneInfo};
use crate::effect::EffectInfo;
use crate::error::ErrorCode;
use crate::item_value::{ItemValue, ItemWriteError, validate_item_value};
use crate::number::FiniteF64;
use crate::object::{LayerInfo, ObjectSummary, SectionRange};
use crate::render::MAX_RENDER_FRAME_BYTES;
use crate::selector::{EffectSelector, ObjectSelector};
use crate::validation::{
    PathSyntaxError, TextSyntaxError, validate_alias, validate_control_free, validate_name,
    validate_object_alias_name, validate_path,
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
        validate_layer_frame(self.layer, self.frame)
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
        validate_layer_frame(self.layer, self.frame)
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
    /// エフェクト名から作成する。
    Effect {
        /// エイリアスファイルの effect.name の値。
        ///
        /// 検証の規則は `add_effect` が受ける effect 名と同じである。
        name: String,
    },
    /// 登録済みオブジェクトエイリアスの名前から作成する。
    AliasName {
        /// 一覧が返した名前。
        name: String,
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
            ObjectSource::Effect { name } => {
                validate_name(name).map_err(|source| EditInputError::Text {
                    field: FIELD_NAME,
                    source,
                })
            }
            // 名前はファイル名の一部になる。禁止文字の判定を連結より先に置く
            // ため、規則は要求元が与えた文字列そのものに対して掛ける。
            ObjectSource::AliasName { name } => {
                validate_object_alias_name(name).map_err(|source| EditInputError::Text {
                    field: FIELD_NAME,
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

/// `set_grid_bpm` の params。
///
/// BPM グリッドはシーンに属し、対象を指す selector を持たない。守れるのは
/// プロジェクト境界と現在シーンだけであり、「読み取った時点と同じ一覧か」は
/// 確かめられない。応答は read-back で得た実際の一覧を返すため、要求元は
/// それを見て判断する。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetGridBpmParams {
    /// 現在シーンの一致確認に使う guard。
    pub expected_scene_id: i32,
    /// 置き換える BPM 情報の一覧。
    ///
    /// 部分更新ではない。指定した一覧がそのまま現在の一覧になる。0 件は
    /// グリッドを消す指定であり、[`MAX_GRID_BPM_ENTRIES`] 件までを受け付ける。
    pub entries: Vec<GridBpm>,
    /// 応答が返した `project_epoch`。
    ///
    /// BPM グリッドは selector を持たないため、プロジェクト境界を照合する
    /// 唯一の材料である。
    pub expected_project_epoch: String,
}

impl SetGridBpmParams {
    /// 要求内容だけで決まる検証を行う。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_grid_bpm_entries(&self.entries)
    }
}

/// シーンの解像度。
///
/// **横幅と高さを別々のフィールドへ平坦化しない。** ホストは解像度を 1 回の
/// 呼び出しで受け取り、片方だけを変える手段を持たない。平坦化すると「横幅だけ
/// 指定」が綴れてしまい、綴れるのに実現できない要求を受け付けることになる。
/// 組にしておけば、片方だけの指定は必須フィールドの欠落として復号の段で落ちる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSize {
    /// 画像の横幅。1 以上。
    pub width: u32,
    /// 画像の高さ。1 以上。
    pub height: u32,
}

impl SceneSize {
    /// 解像度が受け渡せる範囲と、1 フレームを描ける大きさに収まることを確認する。
    pub fn validate(&self) -> Result<(), EditInputError> {
        validate_scene_value(FIELD_SIZE_WIDTH, self.width)?;
        validate_scene_value(FIELD_SIZE_HEIGHT, self.height)?;
        // 上限は描画の側と共有する。描けない大きさのシーンを作れてしまうと、
        // 作った本人がそのシーンを 1 度も描けない。
        //
        // 積は必ず 64bit で取る。`u32` 同士の積は容易に溢れ、溢れた値は上限を
        // 下回るため、判定が通ってしまう。
        let frame_bytes = u64::from(self.width) * u64::from(self.height) * 4;
        if frame_bytes > MAX_RENDER_FRAME_BYTES {
            return Err(EditInputError::SceneFrameTooLarge {
                bytes: frame_bytes,
                max: MAX_RENDER_FRAME_BYTES,
            });
        }
        Ok(())
    }
}

/// `set_scene_settings` の params。
///
/// シーンは selector も fingerprint も持たない。守れるのはプロジェクト境界と
/// 現在シーンと値の範囲だけであり、「読み取った時点と同じ状態のシーンか」は
/// 確かめられない。応答は変更後に観測した実際の状態を返すため、要求元はそれを
/// 見て判断する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetSceneSettingsParams {
    /// 現在シーンの一致確認に使う guard。
    ///
    /// 変更は常に現在シーンへ掛かる。非現在シーンを操作する手段は無く、この値は
    /// 探索先ではなく guard である。
    pub expected_scene_id: i32,
    /// シーン名。省略時は変更しない。
    ///
    /// 空文字は受け付けない。ホストは空文字と未指定をどちらも「変更しない」と
    /// して無視するため、受け付ければ何も起きなかった要求を成功として返す
    /// ことになる。オブジェクト名やレイヤー名と違い、シーン名には「標準へ戻す」
    /// 意味も戻す先も無いため、取り消しを表す指定も持たない。
    #[serde(default)]
    pub name: Option<String>,
    /// 解像度。省略時は変更しない。
    #[serde(default)]
    pub size: Option<SceneSize>,
    /// 音声のサンプリングレート。省略時は変更しない。
    #[serde(default)]
    pub sample_rate: Option<u32>,
    /// 応答が返した `project_epoch`。
    ///
    /// シーンは selector を持たないため、プロジェクト境界を照合する唯一の
    /// 材料である。
    pub expected_project_epoch: String,
}

impl SetSceneSettingsParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// 3 つ全ての省略は拒否する。何も変更しない編集要求は、成功したのか
    /// 無視されたのかをクライアントが区別できない。
    pub fn validate(&self) -> Result<(), EditInputError> {
        if self.name.is_none() && self.size.is_none() && self.sample_rate.is_none() {
            return Err(EditInputError::NoChangeRequested {
                fields: &[FIELD_NAME, FIELD_SIZE, FIELD_SAMPLE_RATE],
            });
        }
        if let Some(name) = &self.name {
            if name.is_empty() {
                return Err(EditInputError::Text {
                    field: FIELD_NAME,
                    source: TextSyntaxError::Empty,
                });
            }
            validate_name(name).map_err(|source| EditInputError::Text {
                field: FIELD_NAME,
                source,
            })?;
        }
        if let Some(size) = &self.size {
            size.validate()?;
        }
        if let Some(sample_rate) = self.sample_rate {
            // 受理値の一覧は作らない。SDK にも文書にも記述が無く、我々が列挙
            // すると、ホストが受け付ける値を我々の側で拒むことになる。
            validate_scene_value(FIELD_SAMPLE_RATE, sample_rate)?;
        }
        Ok(())
    }
}

/// BPM グリッドの一覧の置き換えの結果。
///
/// 一覧そのものが read-back であり、要求した値がどう正規化されたかはこの一覧が
/// 答える。
///
/// BPM グリッドはプロジェクトへ保存される内容であるため、この変更は revision を
/// 進める。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridBpmOutcome {
    /// 変更後のプロジェクトの epoch。
    pub project_epoch: String,
    /// 変更を反映したあとの revision。
    pub project_revision: u64,
    /// read-back で得た変更後の一覧。
    ///
    /// 要求した値と一致するとは限らない。ホストは単精度へ丸め、並べ替えもし得る。
    pub entries: Vec<GridBpm>,
}

/// シーン設定の変更の結果。
///
/// [`SceneInfo`] は読み取りの DTO をそのまま用いる。`get_current_scene` が返す
/// ものと同じ型であるため、要求元は読みと書きで別の形を覚えなくてよい。
///
/// シーンの名前・解像度・サンプリングレートはプロジェクトへ保存される内容で
/// あるため、この変更は revision を進める。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneSettingsOutcome {
    /// 変更後のプロジェクトの epoch。
    pub project_epoch: String,
    /// 変更を反映したあとの revision。
    pub project_revision: u64,
    /// 変更後に観測したシーンの状態。
    ///
    /// 要求した値と一致するとは限らない。ホストが値を調整し得るうえ、観測は
    /// 編集と原子的でない（[`Self::observed_after_edit`]）。差異そのものは
    /// 失敗ではない。
    pub scene: SceneInfo,
    /// 解像度とサンプリングレートが編集の区間の外で観測されたことを示す。
    ///
    /// 常に `true` である。反映値は編集情報にしか現れず、区間の内側から読み
    /// 直す手段が無いため、観測までの間に他所からの変更が入り得る。シーン名
    /// だけは区間の内側で照合済みである。
    ///
    /// **このフィールドを応答へ載せ続けるかは判断していない。** 常に同じ値を
    /// 返すうえ、同じことを tool の説明と text content も述べており、応答値として
    /// 持つ必要があるかは確かめていない。**判断していないことをここに書き残す**
    /// ——書かなければ、次に読む者は理由が在ると考えて探す。
    pub observed_after_edit: bool,
    /// この変更が取り消せないことを示す。
    ///
    /// 常に `true` である。AviUtl2 の取り消し操作ではシーン設定は元へ戻らず、
    /// 取り消すとその前に行った編集が取り消される。**成功したあとにも読める
    /// 唯一の口である** — tool の説明と annotation は要求を出す前にしか効かず、
    /// 応答だけを見る経路はそこから性質を拾えない。
    pub non_undoable: bool,
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
/// `size` フィールド名。
const FIELD_SIZE: &str = "size";
/// 解像度の横幅のフィールド名。
const FIELD_SIZE_WIDTH: &str = "size.width";
/// 解像度の高さのフィールド名。
const FIELD_SIZE_HEIGHT: &str = "size.height";
/// `sample_rate` フィールド名。
const FIELD_SAMPLE_RATE: &str = "sample_rate";
/// `item` フィールド名。
const FIELD_ITEM: &str = "item";
/// `effect_name` フィールド名。
const FIELD_EFFECT_NAME: &str = "effect_name";
/// `position` フィールド名。
const FIELD_POSITION: &str = "position";
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
/// `entries` フィールド名。
const FIELD_ENTRIES: &str = "entries";
/// BPM 情報のテンポのフィールド名。
const FIELD_TEMPO: &str = "tempo";
/// BPM 情報の拍子のフィールド名。
const FIELD_BEAT: &str = "beat";
/// BPM 情報の開始位置のフィールド名。
const FIELD_START: &str = "start";
/// BPM 情報の拍子オフセットのフィールド名。
const FIELD_OFFSET: &str = "offset";
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
    /// シーン設定の値が受け付けられる範囲の外にある。
    ///
    /// 0 は「変更しない」の意味を持たない——省略がその役目を担う。0 以下と
    /// 受け渡せない大きさだけを落とし、それ以外の値の当否はホストが決める。
    #[error("{field} は 1 以上 {max} 以下である必要があります: {value}")]
    SceneValueOutOfRange {
        /// 対象フィールド名。
        field: &'static str,
        /// 指定された値。
        value: u32,
        /// 許容する最大値。
        max: u32,
    },
    /// シーンの解像度が 1 フレームで描ける大きさを超えている。
    ///
    /// 上限は描画と共有する。描けない大きさのシーンを作れてしまうと、作った
    /// 本人がそのシーンを 1 度も描けない。
    #[error("size は 1 フレームが {max} バイト以下に収まる必要があります: {bytes} バイト")]
    SceneFrameTooLarge {
        /// 指定された解像度が要する 1 フレームのバイト数。
        bytes: u64,
        /// 許容する最大バイト数。
        max: u64,
    },
    /// 一覧の要素数が受け付けられる上限を超えている。
    #[error("{field} は {max} 件以下である必要があります: {count}")]
    TooManyEntries {
        /// 対象フィールド名。
        field: &'static str,
        /// 指定された件数。
        count: usize,
        /// 許容する最大件数。
        max: usize,
    },
    /// BPM 情報の値が受け付けられる範囲の外にある。
    #[error("entries[{index}].{field} は{expectation}必要があります")]
    GridBpmOutOfRange {
        /// 一覧の中での位置。
        index: usize,
        /// 対象フィールド名。
        field: &'static str,
        /// 満たすべき条件。
        expectation: &'static str,
    },
    /// BPM 情報の拍子を SDK の型へ写せない。
    ///
    /// 範囲の誤りとは別に扱う。要求元が取る行動が違い、前者は意図した値を
    /// 選び直すのに対し、こちらは値そのものが受け渡せない。
    #[error("entries[{index}].beat を受け渡せません: {value}")]
    GridBpmBeatNotRepresentable {
        /// 一覧の中での位置。
        index: usize,
        /// 指定された値。
        value: i64,
    },
    /// BPM 情報の開始位置が一覧の中で重複している。
    #[error("entries[{index}].start が一覧の中で重複しています")]
    DuplicateGridBpmStart {
        /// 重複した側の、一覧の中での位置。
        index: usize,
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
            EditInputError::SceneValueOutOfRange {
                field: FIELD_SIZE_WIDTH,
                value: 0,
                max: MAX_POSITION,
            },
            EditInputError::SceneFrameTooLarge {
                bytes: 0,
                max: MAX_RENDER_FRAME_BYTES,
            },
            EditInputError::TooManyEntries {
                field: FIELD_ENTRIES,
                count: 0,
                max: MAX_GRID_BPM_ENTRIES,
            },
            EditInputError::GridBpmOutOfRange {
                index: 0,
                field: FIELD_TEMPO,
                expectation: "0 より大きい",
            },
            EditInputError::GridBpmBeatNotRepresentable { index: 0, value: 0 },
            EditInputError::DuplicateGridBpmStart { index: 0 },
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
            EditInputError::GridBpmOutOfRange { .. } => Some("grid_bpm_out_of_range"),
            // 指すのは「引数を SDK の型へ写せない」という事実であり、同じ事実を
            // 変更 API の入口で見つけた場合の失敗と同じ名前を名乗る。
            EditInputError::GridBpmBeatNotRepresentable { .. } => {
                Some("argument_not_representable")
            }
            // 指すのは「同じ対象を 2 度指定した」という事実であり、一括適用が
            // 同じ対象を 2 度変更する要求へ与える名前と同じである。
            EditInputError::DuplicateGridBpmStart { .. } => Some("duplicate_target"),
            EditInputError::PositionOutOfRange { .. }
            | EditInputError::IndexOutOfRange { .. }
            | EditInputError::TooManyEntries { .. }
            | EditInputError::SceneValueOutOfRange { .. }
            | EditInputError::SceneFrameTooLarge { .. }
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
            | EditInputError::SceneValueOutOfRange { .. }
            | EditInputError::SceneFrameTooLarge { .. }
            | EditInputError::TooManyEntries { .. }
            | EditInputError::GridBpmOutOfRange { .. }
            | EditInputError::GridBpmBeatNotRepresentable { .. }
            | EditInputError::DuplicateGridBpmStart { .. }
            | EditInputError::Text { .. }
            | EditInputError::Path { .. } => ErrorCode::InvalidArgument,
        }
    }
}

/// `i32` で受け渡される値の上限。
///
/// レイヤー番号・フレーム番号・シーンの解像度・サンプリングレートはいずれも
/// `i32` で受け渡され、0 以上しか意味を持たないため、符号なしで受けたうえで
/// `i32` に収まることだけを課す。
/// **上限をこれ以上狭めない。** レイヤーの実際の上限はホストが持ち、
/// 「オブジェクトが存在する最大レイヤー」は作成可能な上限ではない。空の
/// レイヤーへの作成を要求内容だけの推測で拒否しない。範囲外の指定は
/// ホストが失敗させる。
///
/// **入力 schema が宣言する上限もこの値である。** 別に定義すると、片方だけを
/// 動かしたときに schema へ適合する要求が検証で拒否される。
pub const MAX_POSITION: u32 = i32::MAX as u32;

/// BPM 情報の一覧に受け付ける最大件数。
///
/// SDK は上限を定めていない。上限が無い要求を受け付けないための、我々の側の
/// 制約である。数そのものに根拠は無い。
pub const MAX_GRID_BPM_ENTRIES: usize = 256;

/// レイヤー番号とフレーム番号の組が受け渡せる範囲に収まることを確認する。
///
/// タイムライン上の 1 点を指す型はいずれも同じ規則に従う。型ごとに書き分けると、
/// 一方だけを直したときに規則が分かれる。
fn validate_layer_frame(layer: u32, frame: u32) -> Result<(), EditInputError> {
    validate_position(FIELD_LAYER, layer)?;
    validate_position(FIELD_FRAME, frame)
}

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

/// シーン設定の値が受け渡せる範囲に収まることを確認する。
///
/// 解像度もサンプリングレートも `i32` で受け渡され、0 以下は意味を持たない。
/// 上限をこれ以上狭めない——ホストが受け付ける値の一覧は我々の側に無く、
/// 狭めれば実際には通る指定を我々が拒むことになる。
fn validate_scene_value(field: &'static str, value: u32) -> Result<(), EditInputError> {
    if value == 0 || value > MAX_POSITION {
        return Err(EditInputError::SceneValueOutOfRange {
            field,
            value,
            max: MAX_POSITION,
        });
    }
    Ok(())
}

/// BPM 情報の一覧を検証する。
///
/// 要求内容だけで決まる検証であり、server と plugin の双方がこれを呼ぶ。
/// 片方だけが検証すると、受理する要求の集合が経路ごとに分かれる。
///
/// **開始位置の昇順は求めない。** 並べ替えはホストの仕事であり、求めなかった
/// 順序を強制すると、read-back の順序と要求の順序が食い違ったときに説明が要る。
/// 順序が定まらない一覧だけを拒む——開始位置が等しい 2 件は前後を決められない。
fn validate_grid_bpm_entries(entries: &[GridBpm]) -> Result<(), EditInputError> {
    if entries.len() > MAX_GRID_BPM_ENTRIES {
        return Err(EditInputError::TooManyEntries {
            field: FIELD_ENTRIES,
            count: entries.len(),
            max: MAX_GRID_BPM_ENTRIES,
        });
    }
    for (index, entry) in entries.iter().enumerate() {
        validate_grid_bpm(index, entry)?;
    }
    for (index, entry) in entries.iter().enumerate() {
        let start = entry.start.get();
        if entries[..index]
            .iter()
            .any(|earlier| earlier.start.get() == start)
        {
            return Err(EditInputError::DuplicateGridBpmStart { index });
        }
    }
    Ok(())
}

/// BPM 情報 1 件の値を検証する。
fn validate_grid_bpm(index: usize, entry: &GridBpm) -> Result<(), EditInputError> {
    let out_of_range = |field, expectation| EditInputError::GridBpmOutOfRange {
        index,
        field,
        expectation,
    };
    if entry.tempo.get() <= 0.0 {
        return Err(out_of_range(FIELD_TEMPO, "0 より大きい"));
    }
    if entry.beat < 1 {
        return Err(out_of_range(FIELD_BEAT, "1 以上である"));
    }
    if entry.start.get() < 0.0 {
        return Err(out_of_range(FIELD_START, "0 以上である"));
    }
    // ホストは tempo と offset を単精度で受け取る。単精度で無限大になる値を
    // 書き込むと、以後の読み取りが非有限値として失敗する。
    //
    // tempo は 0 へ潰れる側も見る。単精度で 0 になる値は、上の判定を通ったのに
    // 0 のテンポとして書き込まれる。丸めそのものは受け入れる——拒むのは、
    // 丸めた結果がここで課した範囲を外れる場合だけである。
    let single_tempo = as_single(entry.tempo);
    if !single_tempo.is_finite() || single_tempo <= 0.0 {
        return Err(out_of_range(FIELD_TEMPO, "単精度で表しても 0 より大きい"));
    }
    // offset は 0 を許すため、見るのは無限大への溢れだけである。
    if !as_single(entry.offset).is_finite() {
        return Err(out_of_range(FIELD_OFFSET, "単精度で表せる"));
    }
    // 拍子は SDK の 32bit 符号付き整数へそのまま渡す。
    if i32::try_from(entry.beat).is_err() {
        return Err(EditInputError::GridBpmBeatNotRepresentable {
            index,
            value: entry.beat,
        });
    }
    Ok(())
}

/// ホストが受け取る単精度へ写した値。
fn as_single(value: FiniteF64) -> f32 {
    value.get() as f32
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
mod tests;
