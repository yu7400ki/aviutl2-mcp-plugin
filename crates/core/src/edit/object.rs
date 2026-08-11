//! オブジェクトを対象とする編集の params。

use super::{
    EditInputError, FIELD_ALIAS, FIELD_ITEM, FIELD_NAME, FIELD_PATH,
    validate_effect_selector_position, validate_layer_frame, validate_path_field,
    validate_selector_position,
};
use crate::item_value::{ItemValue, validate_item_value};
use crate::selector::{EffectSelector, ObjectSelector};
use crate::validation::{validate_alias, validate_name, validate_object_alias_name};
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
