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
mod tests {
    use super::*;
    use crate::error::REASON_VALUES;
    use crate::fingerprint::{EffectFingerprintInput, ObjectFingerprintInput};
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
            EditInputError::SceneValueOutOfRange { .. } => "SceneValueOutOfRange",
            EditInputError::SceneFrameTooLarge { .. } => "SceneFrameTooLarge",
            EditInputError::TooManyEntries { .. } => "TooManyEntries",
            EditInputError::GridBpmOutOfRange { .. } => "GridBpmOutOfRange",
            EditInputError::GridBpmBeatNotRepresentable { .. } => "GridBpmBeatNotRepresentable",
            EditInputError::DuplicateGridBpmStart { .. } => "DuplicateGridBpmStart",
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
            "SceneValueOutOfRange",
            "SceneFrameTooLarge",
            "TooManyEntries",
            "GridBpmOutOfRange",
            "GridBpmBeatNotRepresentable",
            "DuplicateGridBpmStart",
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

    fn sample_move_effect() -> MoveEffectParams {
        MoveEffectParams {
            selector: sample_effect_selector(),
            position: 2,
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
        assert_roundtrip(CreateObjectParams {
            source: ObjectSource::AliasName {
                name: "テストエイリアス".to_string(),
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
        assert_roundtrip(sample_move_effect());
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
        assert_rejects_unknown!(MoveEffectParams, sample_move_effect());
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
            serde_json::to_value(ObjectSource::Effect {
                name: "テキスト".to_string(),
            })
            .unwrap(),
            json!({"type": "effect", "name": "テキスト"})
        );
        assert_eq!(
            serde_json::to_value(ObjectSource::AliasName {
                name: "テストエイリアス".to_string(),
            })
            .unwrap(),
            json!({"type": "alias_name", "name": "テストエイリアス"})
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

        for (cursor, field) in [
            (
                CursorPosition {
                    layer: 0,
                    frame: over,
                },
                FIELD_FRAME,
            ),
            (
                CursorPosition {
                    layer: over,
                    frame: 0,
                },
                FIELD_LAYER,
            ),
        ] {
            assert_eq!(
                SetSelectionParams {
                    cursor: Some(cursor),
                    selected_range: None,
                    focus: None,
                    display: None,
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
    fn create_validates_the_effect_name_by_the_same_rule_as_add_effect() {
        // 名前の規則が作成元と effect の付与で食い違うと、同じ名前が片方でだけ
        // 通る。上限は UTF-16 code unit で数える。
        let over = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2 + 1);
        let at_limit = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2);
        for name in [over.clone(), at_limit.clone(), "図形\0".to_string()] {
            assert_eq!(
                CreateObjectParams {
                    source: ObjectSource::Effect { name: name.clone() },
                    ..sample_create()
                }
                .validate()
                .map_err(|error| match error {
                    EditInputError::Text { source, .. } => source,
                    other => panic!("{other:?}"),
                }),
                AddEffectParams {
                    object: sample_object_selector(),
                    effect_name: name.clone(),
                }
                .validate()
                .map_err(|error| match error {
                    EditInputError::Text { source, .. } => source,
                    other => panic!("{other:?}"),
                }),
                "{name:?}"
            );
        }

        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::Effect {
                    name: "図形\0".to_string(),
                },
                ..sample_create()
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_NAME,
                source: TextSyntaxError::ContainsNul,
            })
        );
        assert!(matches!(
            CreateObjectParams {
                source: ObjectSource::Effect { name: over },
                ..sample_create()
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_NAME,
                source: TextSyntaxError::TooLongUtf16 { .. },
            })
        ));
        assert_eq!(
            CreateObjectParams {
                source: ObjectSource::Effect { name: at_limit },
                ..sample_create()
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn the_alias_name_source_goes_through_the_alias_name_rules() {
        // 名前はファイル名の一部になる。禁止文字を拒めばディレクトリの外を指す
        // 名前は残らないが、規則は連結より先に掛かっていなければならない。
        for (name, expected) in [
            ("テストエイリアス", None),
            ("", Some(TextSyntaxError::Empty)),
            ("..", Some(TextSyntaxError::ForbiddenCharacter)),
            (r"..\..\x", Some(TextSyntaxError::ForbiddenCharacter)),
            ("a/b", Some(TextSyntaxError::ForbiddenCharacter)),
            (r"C:\x", Some(TextSyntaxError::ForbiddenCharacter)),
            ("図形\0", Some(TextSyntaxError::ContainsNul)),
            ("図形\u{1}", Some(TextSyntaxError::ContainsControl)),
        ] {
            let result = CreateObjectParams {
                source: ObjectSource::AliasName {
                    name: name.to_string(),
                },
                ..sample_create()
            }
            .validate();
            match expected {
                None => assert_eq!(result, Ok(()), "{name:?}"),
                Some(source) => assert_eq!(
                    result,
                    Err(EditInputError::Text {
                        field: FIELD_NAME,
                        source,
                    }),
                    "{name:?}"
                ),
            }
        }

        // effect 名は 1,024 UTF-16 code units を上限とする。エイリアス名も同じ
        // 上限を共有する。
        assert!(matches!(
            CreateObjectParams {
                source: ObjectSource::AliasName {
                    name: "あ".repeat(MAX_NAME_UTF16_UNITS + 1),
                },
                ..sample_create()
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_NAME,
                source: TextSyntaxError::TooLongUtf16 { .. },
            })
        ));
    }

    #[test]
    fn the_alias_name_source_is_stricter_than_the_effect_name_source() {
        // 生テキストと effect 名は禁止文字を持たない。エイリアス名だけが追加の
        // 規則を負う。片方だけに規則が掛かっていることを 1 つの比較で残す。
        for name in [r"..\図形", r"C:\図形:1", "図形.1"] {
            assert_eq!(
                CreateObjectParams {
                    source: ObjectSource::Effect {
                        name: name.to_string(),
                    },
                    ..sample_create()
                }
                .validate(),
                Ok(()),
                "{name}"
            );
            assert_eq!(
                CreateObjectParams {
                    source: ObjectSource::AliasName {
                        name: name.to_string(),
                    },
                    ..sample_create()
                }
                .validate(),
                Err(EditInputError::Text {
                    field: FIELD_NAME,
                    source: TextSyntaxError::ForbiddenCharacter,
                }),
                "{name}"
            );
        }
    }

    #[test]
    fn the_effect_source_is_not_subject_to_the_path_rules() {
        // 作成元がパスを運ばない以上、パスの規則は掛からない。掛かると、
        // パスとしては不正な文字列を名前に持つ effect を作成元にできなくなる。
        for name in [
            r"..\図形",
            r"\\.\図形",
            r"C:\図形:1",
            r"\\server\share\図形",
            "図形",
        ] {
            assert_eq!(
                CreateObjectParams {
                    source: ObjectSource::Effect {
                        name: name.to_string(),
                    },
                    ..sample_create()
                }
                .validate(),
                Ok(()),
                "{name}"
            );
        }
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
                selector: effect.clone(),
                enabled: true,
            }
            .validate(),
            MoveEffectParams {
                selector: effect,
                position: 0,
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

    #[test]
    fn move_effect_params_only_bound_the_destination() {
        // 列の長さとの比較は対象の現在の状態を要する。要求内容だけの検証は、
        // 移動先が受け渡せる範囲に収まることまでしか見ない。
        sample_move_effect()
            .validate()
            .expect("移動先の位置が拒否されました");

        let error = MoveEffectParams {
            position: MAX_POSITION as usize + 1,
            ..sample_move_effect()
        }
        .validate()
        .expect_err("i32 に収まらない移動先が受理されました");
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
        assert!(
            matches!(
                error,
                EditInputError::IndexOutOfRange {
                    field: FIELD_POSITION,
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn move_effect_params_reject_a_negative_destination() {
        // 負値は usize へ復号できない。実行口へ届く前に落ちる。
        let mut value = serde_json::to_value(sample_move_effect()).unwrap();
        value["position"] = json!(-1);
        assert!(serde_json::from_value::<MoveEffectParams>(value).is_err());
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

    fn finite(value: f64) -> FiniteF64 {
        FiniteF64::try_new(value).expect("有限値")
    }

    fn bpm(tempo: f64, beat: i64, start: f64, offset: f64) -> GridBpm {
        GridBpm {
            tempo: finite(tempo),
            beat,
            start: finite(start),
            offset: finite(offset),
        }
    }

    fn set_grid_bpm(entries: Vec<GridBpm>) -> SetGridBpmParams {
        SetGridBpmParams {
            expected_scene_id: 0,
            entries,
            expected_project_epoch: EPOCH.to_string(),
        }
    }

    #[test]
    fn a_grid_bpm_list_is_accepted_when_every_value_is_in_range() {
        set_grid_bpm(vec![bpm(120.0, 4, 0.0, 0.0), bpm(140.0, 3, 12.5, 0.25)])
            .validate()
            .expect("正常な一覧が拒否されました");
    }

    #[test]
    fn an_empty_grid_bpm_list_is_accepted() {
        // グリッドを消す指定である。ホストが無視するなら read-back の件数照合が
        // 捕まえる。先回りして拒む理由が無い。
        set_grid_bpm(Vec::new())
            .validate()
            .expect("0 件の一覧が拒否されました");
    }

    #[test]
    fn a_grid_bpm_list_at_the_limit_is_accepted_and_one_more_is_not() {
        let entries = |count: usize| {
            (0..count)
                .map(|index| bpm(120.0, 4, index as f64, 0.0))
                .collect::<Vec<_>>()
        };
        set_grid_bpm(entries(MAX_GRID_BPM_ENTRIES))
            .validate()
            .expect("上限ちょうどの一覧が拒否されました");

        let error = set_grid_bpm(entries(MAX_GRID_BPM_ENTRIES + 1))
            .validate()
            .expect_err("上限を超えた一覧が受理されました");
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
        // 件数の上限は対象フィールド名と上限で説明が尽きる。名前を足しても
        // 要求元が取れる行動は変わらない。
        assert_eq!(error.reason(), None);
    }

    #[test]
    fn a_grid_bpm_value_that_merely_rounds_is_accepted() {
        // ホストは単精度で受け取るため、要求元が単精度で表せない値を送れば
        // 読み返した値は要求値と一致しない。それは失敗ではない。拒むのは、
        // 丸めた結果が課した範囲を外れる場合だけである。
        set_grid_bpm(vec![bpm(0.1, 4, 0.3, 0.7)])
            .validate()
            .expect("丸めが起きるだけの値が拒否されました");
        // 単精度の最小の正規化数より小さくても、0 へ潰れなければ通る。
        set_grid_bpm(vec![bpm(f64::from(f32::MIN_POSITIVE), 4, 0.0, 0.0)])
            .validate()
            .expect("単精度で表せる最小の正の値が拒否されました");
    }

    #[test]
    fn a_descending_grid_bpm_list_is_accepted() {
        // 並べ替えはホストの仕事である。求めなかった順序を強制すると、
        // read-back の順序と要求の順序が食い違ったときに説明が要る。
        set_grid_bpm(vec![
            bpm(120.0, 4, 30.0, 0.0),
            bpm(120.0, 4, 20.0, 0.0),
            bpm(120.0, 4, 10.0, 0.0),
        ])
        .validate()
        .expect("降順の一覧が拒否されました");
    }

    #[test]
    fn each_grid_bpm_rejection_names_its_own_reason() {
        // 5 種の検証が別の名前を名乗ることを固定する。畳むと、要求元は
        // 「値の直し方」と「そもそも受け渡せない」と「同じ位置を 2 度指した」を
        // 区別できない。
        let cases: &[(&str, Vec<GridBpm>, &str)] = &[
            (
                "単精度で無限大になる tempo",
                vec![bpm(1.0e300, 4, 0.0, 0.0)],
                "grid_bpm_out_of_range",
            ),
            (
                "単精度で 0 へ潰れる tempo",
                vec![bpm(1.0e-300, 4, 0.0, 0.0)],
                "grid_bpm_out_of_range",
            ),
            (
                "単精度で無限大になる offset",
                vec![bpm(120.0, 4, 0.0, 1.0e300)],
                "grid_bpm_out_of_range",
            ),
            (
                "0 以下の tempo",
                vec![bpm(0.0, 4, 0.0, 0.0)],
                "grid_bpm_out_of_range",
            ),
            (
                "1 未満の beat",
                vec![bpm(120.0, 0, 0.0, 0.0)],
                "grid_bpm_out_of_range",
            ),
            (
                "負の start",
                vec![bpm(120.0, 4, -1.0, 0.0)],
                "grid_bpm_out_of_range",
            ),
            (
                "重複した start",
                vec![bpm(120.0, 4, 5.0, 0.0), bpm(140.0, 3, 5.0, 0.0)],
                "duplicate_target",
            ),
            (
                "i32 に収まらない beat",
                vec![bpm(120.0, i64::from(i32::MAX) + 1, 0.0, 0.0)],
                "argument_not_representable",
            ),
        ];
        for (label, entries, reason) in cases {
            let error = set_grid_bpm(entries.clone()).validate().expect_err(label);
            assert_eq!(error.error_code(), ErrorCode::InvalidArgument, "{label}");
            assert_eq!(error.reason(), Some(*reason), "{label}");
            assert!(REASON_VALUES.contains(reason), "{label}");
        }
    }

    #[test]
    fn a_non_finite_grid_bpm_value_never_becomes_a_dto() {
        // 有限であることは型が担保する。JSON が非有限数を運べる唯一の経路は
        // 指数が範囲を超える表記であり、そこで拒否される。
        let json = format!(
            r#"{{"expected_scene_id":0,"entries":[{{"tempo":1e999,"beat":4,"start":0.0,"offset":0.0}}],"expected_project_epoch":"{EPOCH}"}}"#
        );
        assert!(serde_json::from_str::<SetGridBpmParams>(&json).is_err());
    }

    #[test]
    fn set_grid_bpm_params_roundtrip() {
        assert_roundtrip(set_grid_bpm(vec![bpm(120.0, 4, 1.5, 0.25)]));
    }

    #[test]
    fn set_grid_bpm_params_reject_unknown_fields() {
        let value = with_unknown_field(&set_grid_bpm(Vec::new()));
        assert!(serde_json::from_value::<SetGridBpmParams>(value).is_err());
    }

    #[test]
    fn grid_bpm_outcome_roundtrip() {
        let outcome = GridBpmOutcome {
            project_epoch: EPOCH.to_string(),
            project_revision: 43,
            entries: vec![bpm(120.0, 4, 0.0, 0.0)],
        };
        let s = serde_json::to_string(&outcome).unwrap();
        let restored: GridBpmOutcome = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, outcome);
    }

    fn sample_set_scene_settings() -> SetSceneSettingsParams {
        SetSceneSettingsParams {
            expected_scene_id: 0,
            name: Some("本編".to_string()),
            size: Some(SceneSize {
                width: 1920,
                height: 1080,
            }),
            sample_rate: Some(48_000),
            expected_project_epoch: EPOCH.to_string(),
        }
    }

    #[test]
    fn set_scene_settings_rejects_omitting_every_change() {
        let params = SetSceneSettingsParams {
            name: None,
            size: None,
            sample_rate: None,
            ..sample_set_scene_settings()
        };
        let error = params.validate().unwrap_err();
        assert_eq!(
            error,
            EditInputError::NoChangeRequested {
                fields: &["name", "size", "sample_rate"],
            }
        );
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
        assert_eq!(error.reason(), None);

        // 3 つの軸は個別にも組み合わせても指定できる。
        for params in [
            SetSceneSettingsParams {
                size: None,
                sample_rate: None,
                ..sample_set_scene_settings()
            },
            SetSceneSettingsParams {
                name: None,
                sample_rate: None,
                ..sample_set_scene_settings()
            },
            SetSceneSettingsParams {
                name: None,
                size: None,
                ..sample_set_scene_settings()
            },
            SetSceneSettingsParams {
                sample_rate: None,
                ..sample_set_scene_settings()
            },
            sample_set_scene_settings(),
        ] {
            assert_eq!(params.validate(), Ok(()));
        }
    }

    #[test]
    fn set_scene_settings_rejects_an_empty_scene_name() {
        // ホストは空文字を「変更しない」として無視する。受け付ければ、何も
        // 起きなかった要求を成功として返すことになる。
        let error = SetSceneSettingsParams {
            name: Some(String::new()),
            ..sample_set_scene_settings()
        }
        .validate()
        .expect_err("空のシーン名が受理されました");
        assert_eq!(
            error,
            EditInputError::Text {
                field: FIELD_NAME,
                source: TextSyntaxError::Empty,
            }
        );
        assert_eq!(error.reason(), Some("empty"));
        assert!(REASON_VALUES.contains(&"empty"));

        // オブジェクト名は空を標準名へ戻す指定として受け付け続ける。シーン名に
        // その意味が無いのは、戻す先が存在しないためである。
        assert_eq!(
            SetObjectNameParams {
                selector: sample_object_selector(),
                name: Some(String::new()),
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn set_scene_settings_applies_the_shared_name_rule() {
        // 名前の規則はオブジェクト名・レイヤー名と共有する。別の規則を書き
        // 起こすと、同じ名前が経路によって受理されたり拒否されたりする。
        assert_eq!(
            SetSceneSettingsParams {
                name: Some("本\0編".to_string()),
                ..sample_set_scene_settings()
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_NAME,
                source: TextSyntaxError::ContainsNul,
            })
        );

        let over = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2 + 1);
        assert!(matches!(
            SetSceneSettingsParams {
                name: Some(over),
                ..sample_set_scene_settings()
            }
            .validate(),
            Err(EditInputError::Text {
                field: FIELD_NAME,
                source: TextSyntaxError::TooLongUtf16 { .. },
            })
        ));

        // 制御文字は見ない。名前の規則が経路ごとに分かれる。
        assert_eq!(
            SetSceneSettingsParams {
                name: Some("本\t編".to_string()),
                ..sample_set_scene_settings()
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn the_scene_size_bound_comes_from_the_render_limit() {
        let scene_size = |width, height| SetSceneSettingsParams {
            name: None,
            size: Some(SceneSize { width, height }),
            sample_rate: None,
            ..sample_set_scene_settings()
        };
        let max_pixels = (MAX_RENDER_FRAME_BYTES / 4) as u32;

        // ちょうど上限の組は通る。形が違っても境界は画素数だけで決まる。
        for (width, height) in [(8192, 8192), (1, max_pixels)] {
            scene_size(width, height)
                .validate()
                .expect("上限ちょうどの解像度が拒否されました");
        }

        // 1 画素超えると落ちる。
        let error = scene_size(1, max_pixels + 1)
            .validate()
            .expect_err("上限を 1 画素超えた解像度が受理されました");
        assert_eq!(
            error,
            EditInputError::SceneFrameTooLarge {
                bytes: (u64::from(max_pixels) + 1) * 4,
                max: MAX_RENDER_FRAME_BYTES,
            }
        );
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);

        // 積は 64bit で取る。`u32` の掛け算では 0 へ折り返し、上限を下回る値
        // として通ってしまう組である。
        assert!(matches!(
            scene_size(65536, 65536).validate(),
            Err(EditInputError::SceneFrameTooLarge { .. })
        ));
    }

    #[test]
    fn set_scene_settings_rejects_values_outside_the_receivable_range() {
        let over = MAX_POSITION + 1;
        let cases: &[(&str, SetSceneSettingsParams, &'static str, u32)] = &[
            (
                "横幅 0",
                SetSceneSettingsParams {
                    size: Some(SceneSize {
                        width: 0,
                        height: 1080,
                    }),
                    ..sample_set_scene_settings()
                },
                FIELD_SIZE_WIDTH,
                0,
            ),
            (
                "高さ 0",
                SetSceneSettingsParams {
                    size: Some(SceneSize {
                        width: 1920,
                        height: 0,
                    }),
                    ..sample_set_scene_settings()
                },
                FIELD_SIZE_HEIGHT,
                0,
            ),
            (
                "i32 に収まらない横幅",
                SetSceneSettingsParams {
                    size: Some(SceneSize {
                        width: over,
                        height: 1,
                    }),
                    ..sample_set_scene_settings()
                },
                FIELD_SIZE_WIDTH,
                over,
            ),
            (
                "i32 に収まらない高さ",
                SetSceneSettingsParams {
                    size: Some(SceneSize {
                        width: 1,
                        height: over,
                    }),
                    ..sample_set_scene_settings()
                },
                FIELD_SIZE_HEIGHT,
                over,
            ),
            (
                "サンプリングレート 0",
                SetSceneSettingsParams {
                    name: None,
                    size: None,
                    sample_rate: Some(0),
                    ..sample_set_scene_settings()
                },
                FIELD_SAMPLE_RATE,
                0,
            ),
            (
                "i32 に収まらないサンプリングレート",
                SetSceneSettingsParams {
                    name: None,
                    size: None,
                    sample_rate: Some(over),
                    ..sample_set_scene_settings()
                },
                FIELD_SAMPLE_RATE,
                over,
            ),
        ];
        for (label, params, field, value) in cases {
            let error = params.clone().validate().expect_err(label);
            assert_eq!(
                error,
                EditInputError::SceneValueOutOfRange {
                    field,
                    value: *value,
                    max: MAX_POSITION,
                },
                "{label}"
            );
        }

        // 上限ちょうどのサンプリングレートは通る。受理値の一覧は我々の側に
        // 無く、受け渡せる範囲だけを課している。
        assert_eq!(
            SetSceneSettingsParams {
                name: None,
                size: None,
                sample_rate: Some(MAX_POSITION),
                ..sample_set_scene_settings()
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn scene_setting_range_failures_have_no_reason() {
        // 範囲外はフィールド名と上限の文面で説明が尽きる。機械可読な種別名を
        // 足しても要求元が取れる行動は変わらず、値域を広げる理由が無い。
        for error in [
            EditInputError::SceneValueOutOfRange {
                field: FIELD_SIZE_WIDTH,
                value: 0,
                max: MAX_POSITION,
            },
            EditInputError::SceneValueOutOfRange {
                field: FIELD_SAMPLE_RATE,
                value: 0,
                max: MAX_POSITION,
            },
            EditInputError::SceneFrameTooLarge {
                bytes: MAX_RENDER_FRAME_BYTES + 4,
                max: MAX_RENDER_FRAME_BYTES,
            },
        ] {
            assert_eq!(error.reason(), None, "{error}");
            assert_eq!(error.error_code(), ErrorCode::InvalidArgument, "{error}");
        }
    }

    #[test]
    fn set_scene_settings_params_roundtrip() {
        assert_roundtrip(sample_set_scene_settings());
        assert_roundtrip(SetSceneSettingsParams {
            name: None,
            size: None,
            ..sample_set_scene_settings()
        });
    }

    #[test]
    fn set_scene_settings_params_reject_unknown_fields() {
        assert!(
            serde_json::from_value::<SetSceneSettingsParams>(with_unknown_field(
                &sample_set_scene_settings()
            ))
            .is_err()
        );
        let mut value = serde_json::to_value(sample_set_scene_settings()).unwrap();
        value["size"] = json!({"width": 1920, "height": 1080, "future": 1});
        assert!(serde_json::from_value::<SetSceneSettingsParams>(value).is_err());
    }

    #[test]
    fn set_scene_settings_params_require_the_guards_and_the_whole_size() {
        for key in ["expected_scene_id", "expected_project_epoch"] {
            assert!(
                serde_json::from_value::<SetSceneSettingsParams>(without_field(
                    &sample_set_scene_settings(),
                    key
                ))
                .is_err(),
                "{key} の欠落が受理されました"
            );
        }

        // 3 つの軸は省略できる。
        let omitted: SetSceneSettingsParams = serde_json::from_value(json!({
            "expected_scene_id": 0,
            "sample_rate": 48_000,
            "expected_project_epoch": EPOCH,
        }))
        .unwrap();
        assert_eq!(omitted.name, None);
        assert_eq!(omitted.size, None);

        // 解像度は組であり、片方だけの指定は綴れない。ホストは片方だけを
        // 変える手段を持たない。
        let mut value = serde_json::to_value(sample_set_scene_settings()).unwrap();
        value["size"] = json!({"width": 1920});
        assert!(serde_json::from_value::<SetSceneSettingsParams>(value).is_err());
    }

    #[test]
    fn the_scene_settings_outcome_reuses_the_read_dto() {
        let outcome = SceneSettingsOutcome {
            project_epoch: EPOCH.to_string(),
            project_revision: 43,
            scene: SceneInfo {
                id: 0,
                name: Some("本編".to_string()),
                width: 1920,
                height: 1080,
                fps: Some(finite(60.0)),
                fps_rate: 60,
                fps_scale: 1,
                sample_rate: 48_000,
            },
            observed_after_edit: true,
            non_undoable: true,
        };
        let value = serde_json::to_value(&outcome).unwrap();
        assert_eq!(value["project_epoch"], json!(EPOCH));
        assert_eq!(value["project_revision"], json!(43));
        assert_eq!(
            value["scene"],
            serde_json::to_value(&outcome.scene).unwrap()
        );
        // 取り消せないことと、観測が編集と原子的でないことは、応答だけを見る
        // 経路が拾える唯一の口である。
        assert_eq!(value["non_undoable"], json!(true));
        assert_eq!(value["observed_after_edit"], json!(true));

        let s = serde_json::to_string(&outcome).unwrap();
        assert_eq!(
            serde_json::from_str::<SceneSettingsOutcome>(&s).unwrap(),
            outcome
        );
        // 応答型は将来の optional field を受け入れる。
        let restored: SceneSettingsOutcome =
            serde_json::from_value(with_unknown_field(&outcome)).unwrap();
        assert_eq!(restored, outcome);
    }
}
