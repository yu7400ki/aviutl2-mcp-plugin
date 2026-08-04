//! read operation の名前と params / result、および編集・render operation の名前。
//!
//! 編集 operation と render operation の params / result 型は本モジュールでは
//! 定義しない。

use crate::budget::RequestBudgetKind;
use crate::edit_info::SceneInfo;
use crate::effect::{AvailableEffect, EffectType};
use crate::number::FiniteF64;
use crate::object::{LayerInfo, ObjectSummary};
use crate::page::{PageMeta, PageRequest};
use crate::selector::{EffectSelector, ObjectSelector};
use crate::validation::{TextSyntaxError, validate_name};
use serde::{Deserialize, Serialize};

/// 現在の編集情報を取得する operation 名。
pub const OPERATION_GET_EDIT_INFO: &str = "get_edit_info";

/// 現在シーンを取得する operation 名。
pub const OPERATION_GET_CURRENT_SCENE: &str = "get_current_scene";

/// 現在シーンのレイヤーを列挙する operation 名。
pub const OPERATION_LIST_LAYERS: &str = "list_layers";

/// 現在シーンのオブジェクトを列挙する operation 名。
pub const OPERATION_LIST_OBJECTS: &str = "list_objects";

/// オブジェクトの詳細を取得する operation 名。
pub const OPERATION_GET_OBJECT: &str = "get_object";

/// 利用可能な effect を列挙する operation 名。
pub const OPERATION_LIST_AVAILABLE_EFFECTS: &str = "list_available_effects";

/// effect の設定項目を任意フレームで評価する operation 名。
pub const OPERATION_GET_EFFECT_ITEM_VALUES: &str = "get_effect_item_values";

/// media file / alias からオブジェクトを作成する operation 名。
pub const OPERATION_CREATE_OBJECT: &str = "create_object";

/// オブジェクトのレイヤーと開始フレームを変更する operation 名。
pub const OPERATION_MOVE_OBJECT: &str = "move_object";

/// オブジェクトを削除する operation 名。
pub const OPERATION_DELETE_OBJECT: &str = "delete_object";

/// オブジェクト名を変更する operation 名。
pub const OPERATION_SET_OBJECT_NAME: &str = "set_object_name";

/// オブジェクトの設定項目・track 値を変更する operation 名。
pub const OPERATION_SET_OBJECT_ITEM: &str = "set_object_item";

/// オブジェクトへ effect を付与する operation 名。
pub const OPERATION_ADD_EFFECT: &str = "add_effect";

/// オブジェクトから effect を削除する operation 名。
pub const OPERATION_DELETE_EFFECT: &str = "delete_effect";

/// effect の有効・無効を変更する operation 名。
pub const OPERATION_SET_EFFECT_ENABLED: &str = "set_effect_enabled";

/// レイヤーの名前・表示・ロックを変更する operation 名。
pub const OPERATION_SET_LAYER_STATE: &str = "set_layer_state";

/// カーソル・選択範囲・フォーカスを変更する operation 名。
pub const OPERATION_SET_SELECTION: &str = "set_selection";

/// オブジェクトへ中間点を追加する operation 名。
pub const OPERATION_CREATE_OBJECT_SECTION: &str = "create_object_section";

/// オブジェクトの中間点を削除する operation 名。
pub const OPERATION_DELETE_OBJECT_SECTION: &str = "delete_object_section";

/// オブジェクトの中間点を移動する operation 名。
pub const OPERATION_MOVE_OBJECT_SECTION: &str = "move_object_section";

/// 複数の変更を 1 つの取り消し単位で適用する operation 名。
pub const OPERATION_APPLY_BATCH: &str = "apply_batch";

/// 現在シーンの 1 フレームを描画する operation 名。
pub const OPERATION_RENDER_FRAME: &str = "render_frame";

/// read operation の種別。
///
/// 役割は [`EditOperation`] と同じで、read operation の名前一覧をこの型へ
/// 一本化する。名前を定数の列としてだけ持つと、束ねる型が無いために
/// 「全 read operation を漏れなく数える」テストが書けず、追加した名前が
/// 一部の判定処理から抜け落ちても気付けない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOperation {
    /// [`OPERATION_GET_EDIT_INFO`]。
    GetEditInfo,
    /// [`OPERATION_GET_CURRENT_SCENE`]。
    GetCurrentScene,
    /// [`OPERATION_LIST_LAYERS`]。
    ListLayers,
    /// [`OPERATION_LIST_OBJECTS`]。
    ListObjects,
    /// [`OPERATION_GET_OBJECT`]。
    GetObject,
    /// [`OPERATION_LIST_AVAILABLE_EFFECTS`]。
    ListAvailableEffects,
    /// [`OPERATION_GET_EFFECT_ITEM_VALUES`]。
    GetEffectItemValues,
}

impl ReadOperation {
    /// 全 variant。
    ///
    /// 要素数と内容は `read_operation_all_is_exhaustive` テストで固定する。
    pub const ALL: [ReadOperation; 7] = [
        ReadOperation::GetEditInfo,
        ReadOperation::GetCurrentScene,
        ReadOperation::ListLayers,
        ReadOperation::ListObjects,
        ReadOperation::GetObject,
        ReadOperation::ListAvailableEffects,
        ReadOperation::GetEffectItemValues,
    ];

    /// operation 名の文字列表現を返す。
    pub const fn as_str(self) -> &'static str {
        match self {
            ReadOperation::GetEditInfo => OPERATION_GET_EDIT_INFO,
            ReadOperation::GetCurrentScene => OPERATION_GET_CURRENT_SCENE,
            ReadOperation::ListLayers => OPERATION_LIST_LAYERS,
            ReadOperation::ListObjects => OPERATION_LIST_OBJECTS,
            ReadOperation::GetObject => OPERATION_GET_OBJECT,
            ReadOperation::ListAvailableEffects => OPERATION_LIST_AVAILABLE_EFFECTS,
            ReadOperation::GetEffectItemValues => OPERATION_GET_EFFECT_ITEM_VALUES,
        }
    }

    /// operation 名から variant を引く。read operation でなければ `None`。
    ///
    /// [`ReadOperation::ALL`] を線形探索するだけであり、一覧を別に持たない。
    pub fn from_operation_name(name: &str) -> Option<Self> {
        ReadOperation::ALL
            .into_iter()
            .find(|op| op.as_str() == name)
    }
}

/// 編集 operation の種別。
///
/// 編集 operation の名前一覧はこの型へ一本化する。文字列表現は
/// [`EditOperation::as_str`]、名前からの解決は
/// [`EditOperation::from_operation_name`]、全 variant は [`EditOperation::ALL`]
/// で得られる。read/edit を分岐する必要がある処理（要求予算の選択、
/// operation の dispatch など）は、operation 名の一覧を個別に持たず、この型を
/// 経由して判定する。そうすることで、新しい編集 operation を追加する際に
/// 一部の判定処理だけへ足し忘れても、他の判定処理は追加前のまま動き続けて
/// しまう、という食い違いを構造的に防ぐ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOperation {
    /// [`OPERATION_CREATE_OBJECT`]。
    CreateObject,
    /// [`OPERATION_MOVE_OBJECT`]。
    MoveObject,
    /// [`OPERATION_DELETE_OBJECT`]。
    DeleteObject,
    /// [`OPERATION_SET_OBJECT_NAME`]。
    SetObjectName,
    /// [`OPERATION_SET_OBJECT_ITEM`]。
    SetObjectItem,
    /// [`OPERATION_ADD_EFFECT`]。
    AddEffect,
    /// [`OPERATION_DELETE_EFFECT`]。
    DeleteEffect,
    /// [`OPERATION_SET_EFFECT_ENABLED`]。
    SetEffectEnabled,
    /// [`OPERATION_SET_LAYER_STATE`]。
    SetLayerState,
    /// [`OPERATION_SET_SELECTION`]。
    SetSelection,
    /// [`OPERATION_CREATE_OBJECT_SECTION`]。
    CreateObjectSection,
    /// [`OPERATION_DELETE_OBJECT_SECTION`]。
    DeleteObjectSection,
    /// [`OPERATION_MOVE_OBJECT_SECTION`]。
    MoveObjectSection,
    /// [`OPERATION_APPLY_BATCH`]。
    ///
    /// 複数の変更をまとめて発行するが、区間の入り方も失敗の写し方も他の編集
    /// operation と同じであるため、別の族を作らずここへ並べる。要求予算だけは
    /// 他の編集より長い区分を持つ（[`KnownOperation::budget_kind`]）。
    ApplyBatch,
}

impl EditOperation {
    /// 全 variant。
    ///
    /// 要素数と内容は `edit_operation_all_is_exhaustive` テストで固定する。
    pub const ALL: [EditOperation; 14] = [
        EditOperation::CreateObject,
        EditOperation::MoveObject,
        EditOperation::DeleteObject,
        EditOperation::SetObjectName,
        EditOperation::SetObjectItem,
        EditOperation::AddEffect,
        EditOperation::DeleteEffect,
        EditOperation::SetEffectEnabled,
        EditOperation::SetLayerState,
        EditOperation::SetSelection,
        EditOperation::CreateObjectSection,
        EditOperation::DeleteObjectSection,
        EditOperation::MoveObjectSection,
        EditOperation::ApplyBatch,
    ];

    /// operation 名の文字列表現を返す。
    pub const fn as_str(self) -> &'static str {
        match self {
            EditOperation::CreateObject => OPERATION_CREATE_OBJECT,
            EditOperation::MoveObject => OPERATION_MOVE_OBJECT,
            EditOperation::DeleteObject => OPERATION_DELETE_OBJECT,
            EditOperation::SetObjectName => OPERATION_SET_OBJECT_NAME,
            EditOperation::SetObjectItem => OPERATION_SET_OBJECT_ITEM,
            EditOperation::AddEffect => OPERATION_ADD_EFFECT,
            EditOperation::DeleteEffect => OPERATION_DELETE_EFFECT,
            EditOperation::SetEffectEnabled => OPERATION_SET_EFFECT_ENABLED,
            EditOperation::SetLayerState => OPERATION_SET_LAYER_STATE,
            EditOperation::SetSelection => OPERATION_SET_SELECTION,
            EditOperation::CreateObjectSection => OPERATION_CREATE_OBJECT_SECTION,
            EditOperation::DeleteObjectSection => OPERATION_DELETE_OBJECT_SECTION,
            EditOperation::MoveObjectSection => OPERATION_MOVE_OBJECT_SECTION,
            EditOperation::ApplyBatch => OPERATION_APPLY_BATCH,
        }
    }

    /// operation 名から variant を引く。編集 operation でなければ `None`。
    ///
    /// [`EditOperation::ALL`] を線形探索するだけであり、一覧を別に持たない。
    pub fn from_operation_name(name: &str) -> Option<Self> {
        EditOperation::ALL
            .into_iter()
            .find(|op| op.as_str() == name)
    }
}

/// render operation の種別。
///
/// 役割は [`EditOperation`] と同じである。variant が 1 つしか無い段階から型を
/// 置くのは、後から名前の一覧を定数の列として増やし始めると、束ねる型を持つ
/// 他の族と扱いが分かれてしまうためである。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderOperation {
    /// [`OPERATION_RENDER_FRAME`]。
    RenderFrame,
}

impl RenderOperation {
    /// 全 variant。
    ///
    /// 要素数と内容は `render_operation_all_is_exhaustive` テストで固定する。
    pub const ALL: [RenderOperation; 1] = [RenderOperation::RenderFrame];

    /// operation 名の文字列表現を返す。
    pub const fn as_str(self) -> &'static str {
        match self {
            RenderOperation::RenderFrame => OPERATION_RENDER_FRAME,
        }
    }

    /// operation 名から variant を引く。render operation でなければ `None`。
    ///
    /// [`RenderOperation::ALL`] を線形探索するだけであり、一覧を別に持たない。
    pub fn from_operation_name(name: &str) -> Option<Self> {
        RenderOperation::ALL
            .into_iter()
            .find(|op| op.as_str() == name)
    }
}

/// 実行できる operation の全体。
///
/// read・編集・render の 3 族を 1 つの型で束ねる。族ごとに分かれた判定を持つと、
/// **どの族にも当たらなかった名前**と**まだ束ねていない族の名前**が同じ既定へ
/// 落ちる。既定は最も短い予算である [`RequestBudgetKind::Read`] であり、
/// 落ちたことはコンパイルエラーにもテストの失敗にもならないまま、実行時に
/// 「投入した瞬間に予算が尽きる operation」として現れる。
///
/// この型を経由すれば、族を増やしたときに [`KnownOperation::budget_kind`] の
/// 網羅 `match` が腕の不足でコンパイルを止める。塞ぎたいのは
/// **実行する operation の分類漏れ**だけであり、未知の名前が既定へ落ちること
/// 自体は無害である（受理する前に拒否されるため、予算を使う処理へ進まない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownOperation {
    /// read operation。
    Read(ReadOperation),
    /// 編集 operation。
    Edit(EditOperation),
    /// render operation。
    Render(RenderOperation),
}

impl KnownOperation {
    /// operation 名から variant を引く。いずれの族にも属さなければ `None`。
    pub fn from_operation_name(name: &str) -> Option<Self> {
        if let Some(operation) = ReadOperation::from_operation_name(name) {
            return Some(KnownOperation::Read(operation));
        }
        if let Some(operation) = EditOperation::from_operation_name(name) {
            return Some(KnownOperation::Edit(operation));
        }
        RenderOperation::from_operation_name(name).map(KnownOperation::Render)
    }

    /// operation 名の文字列表現を返す。
    pub const fn as_str(self) -> &'static str {
        match self {
            KnownOperation::Read(operation) => operation.as_str(),
            KnownOperation::Edit(operation) => operation.as_str(),
            KnownOperation::Render(operation) => operation.as_str(),
        }
    }

    /// 要求予算の区分を返す。
    ///
    /// **`_` を使わない網羅 `match` である。** variant を足すと腕が足りず
    /// コンパイルが落ちるため、予算区分を決めないまま新しい operation を
    /// 受け付ける状態にはならない。族と区分は 1 対 1 ではなく、編集 operation
    /// のうち一括適用だけが別の区分を持つ。
    pub const fn budget_kind(self) -> RequestBudgetKind {
        match self {
            KnownOperation::Read(operation) => match operation {
                ReadOperation::GetEditInfo
                | ReadOperation::GetCurrentScene
                | ReadOperation::ListLayers
                | ReadOperation::ListObjects
                | ReadOperation::GetObject
                | ReadOperation::ListAvailableEffects
                | ReadOperation::GetEffectItemValues => RequestBudgetKind::Read,
            },
            KnownOperation::Edit(operation) => match operation {
                EditOperation::CreateObject
                | EditOperation::MoveObject
                | EditOperation::DeleteObject
                | EditOperation::SetObjectName
                | EditOperation::SetObjectItem
                | EditOperation::AddEffect
                | EditOperation::DeleteEffect
                | EditOperation::SetEffectEnabled
                | EditOperation::SetLayerState
                | EditOperation::SetSelection
                | EditOperation::CreateObjectSection
                | EditOperation::DeleteObjectSection
                | EditOperation::MoveObjectSection => RequestBudgetKind::Edit,
                EditOperation::ApplyBatch => RequestBudgetKind::Batch,
            },
            KnownOperation::Render(operation) => match operation {
                RenderOperation::RenderFrame => RequestBudgetKind::Render,
            },
        }
    }
}

/// `get_edit_info` の params。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetEditInfoParams {}

/// `get_current_scene` の params。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetCurrentSceneParams {}

/// `list_layers` の params。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListLayersParams {
    /// 列挙時と同じシーンかを確認するための guard。
    pub expected_scene_id: i32,
    /// ページ指定。要求では offset / limit / snapshot_revision として展開される。
    #[serde(flatten)]
    pub page: PageRequest,
}

/// `list_objects` の params。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListObjectsParams {
    /// 列挙時と同じシーンかを確認するための guard。
    pub expected_scene_id: i32,
    /// 絞り込み条件。
    #[serde(default)]
    pub filter: Option<ObjectFilter>,
    /// ページ指定。要求では offset / limit / snapshot_revision として展開される。
    #[serde(flatten)]
    pub page: PageRequest,
}

/// オブジェクト列挙の絞り込み条件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectFilter {
    /// 対象とする最小レイヤー番号。0 始まり。
    #[serde(default)]
    pub layer_min: Option<usize>,
    /// 対象とする最大レイヤー番号。0 始まり。
    #[serde(default)]
    pub layer_max: Option<usize>,
}

impl ObjectFilter {
    /// レイヤー範囲の整合を検証する。
    ///
    /// 空集合になる指定は、結果 0 件と区別できるよう要求の誤りとして扱う。
    pub fn validate(&self) -> Result<(), ObjectFilterError> {
        if let (Some(min), Some(max)) = (self.layer_min, self.layer_max)
            && min > max
        {
            return Err(ObjectFilterError::InvertedLayerRange { min, max });
        }
        Ok(())
    }
}

/// 絞り込み条件の検証失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ObjectFilterError {
    /// `layer_min` が `layer_max` を上回っている。
    #[error("layer_min は layer_max 以下である必要があります: {min} > {max}")]
    InvertedLayerRange {
        /// 指定された最小レイヤー番号。
        min: usize,
        /// 指定された最大レイヤー番号。
        max: usize,
    },
}

/// `get_object` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetObjectParams {
    /// 対象オブジェクトのセレクター。
    pub selector: ObjectSelector,
}

/// `list_available_effects` の params。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListAvailableEffectsParams {
    /// 種別で絞り込む。
    #[serde(default)]
    pub effect_type: Option<EffectType>,
    /// ページ指定。要求では offset / limit / snapshot_revision として展開される。
    #[serde(flatten)]
    pub page: PageRequest,
}

/// 1 度の要求で評価できるフレームの最大件数。
pub const MAX_EVALUATED_FRAMES: usize = 16;

/// 1 度の要求で評価できる設定項目の最大件数。
pub const MAX_EVALUATED_ITEMS: usize = 32;

/// `get_effect_item_values` の params。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetEffectItemValuesParams {
    /// 評価対象の effect。
    ///
    /// 前提条件のフィールドを持たない。内側の [`ObjectSelector`] が
    /// project epoch・シーン ID・fingerprint を運ぶ。
    pub effect: EffectSelector,
    /// 評価するフレーム番号。シーンの絶対フレームで 0 始まり。1 件以上
    /// [`MAX_EVALUATED_FRAMES`] 件以下。
    ///
    /// トラックバー項目は小数部をそのまま使い、チェックボックス項目は整数部を
    /// 使う。
    pub frames: Vec<FiniteF64>,
    /// 評価する設定項目名。省略時は effect のトラックバー項目とチェックボックス
    /// 項目すべてを対象とする。明示するときは 1 件以上 [`MAX_EVALUATED_ITEMS`]
    /// 件以下。
    #[serde(default)]
    pub items: Option<Vec<String>>,
}

impl GetEffectItemValuesParams {
    /// 要求内容だけで決まる件数と項目名を検証する。
    ///
    /// 0 件のフレーム指定は「何を聞いているのか」が定まらないため受け付けない。
    pub fn validate(&self) -> Result<(), EffectItemValuesInputError> {
        if self.frames.is_empty() || self.frames.len() > MAX_EVALUATED_FRAMES {
            return Err(EffectItemValuesInputError::FrameCountOutOfRange {
                count: self.frames.len(),
            });
        }
        let Some(items) = &self.items else {
            return Ok(());
        };
        if items.is_empty() || items.len() > MAX_EVALUATED_ITEMS {
            return Err(EffectItemValuesInputError::ItemCountOutOfRange { count: items.len() });
        }
        for name in items {
            validate_name(name)
                .map_err(|source| EffectItemValuesInputError::ItemName { source })?;
        }
        Ok(())
    }
}

/// 補間後の値の要求内容の検証失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EffectItemValuesInputError {
    /// フレームの件数が受け付ける範囲に無い。
    #[error("frames は 1 件以上 {MAX_EVALUATED_FRAMES} 件以下である必要があります: {count} 件")]
    FrameCountOutOfRange {
        /// 指定された件数。
        count: usize,
    },
    /// 設定項目の件数が受け付ける範囲に無い。
    #[error("items は 1 件以上 {MAX_EVALUATED_ITEMS} 件以下である必要があります: {count} 件")]
    ItemCountOutOfRange {
        /// 指定された件数。
        count: usize,
    },
    /// 設定項目名が名前の規則に反する。
    #[error("設定項目名が不正です: {source}")]
    ItemName {
        /// 反した規則。
        #[source]
        source: TextSyntaxError,
    },
}

/// `get_current_scene` の result。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetCurrentSceneResult {
    /// 現在シーンの情報。
    pub scene: SceneInfo,
    /// 取得時点のプロジェクト revision。
    pub project_revision: u64,
}

/// `list_layers` の result。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListLayersResult {
    /// 切り出されたページの要素。
    pub items: Vec<LayerInfo>,
    /// ページのメタ情報。
    pub page: PageMeta,
}

/// `list_objects` の result。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListObjectsResult {
    /// 切り出されたページの要素。
    pub items: Vec<ObjectSummary>,
    /// ページのメタ情報。
    pub page: PageMeta,
}

/// `list_available_effects` の result。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListAvailableEffectsResult {
    /// 切り出されたページの要素。
    pub items: Vec<AvailableEffect>,
    /// ページのメタ情報。
    pub page: PageMeta,
}

/// `get_effect_item_values` の result。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectItemValues {
    /// 取得時点のプロジェクト revision。
    pub project_revision: u64,
    /// 評価したフレーム番号。要求した順序をそのまま保つ。
    pub frames: Vec<FiniteF64>,
    /// 評価した設定項目。
    pub items: Vec<EvaluatedItem>,
    /// 設定項目を [`MAX_EVALUATED_ITEMS`] で打ち切った場合に true。
    pub truncated: bool,
}

/// 評価した設定項目 1 件。
///
/// 種別ごとに値の型が違うため、1 つの配列へ数値と真偽を混ぜず variant で分ける。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvaluatedItem {
    /// トラックバー項目。
    Track {
        /// 設定項目名。
        name: String,
        /// 評価した値。`frames` と同じ長さ・同じ順序。
        values: Vec<FiniteF64>,
        /// 所属するトラックバーグループ。属さない場合は null。
        group: Option<TrackGroup>,
    },
    /// チェックボックス項目。
    Check {
        /// 設定項目名。
        name: String,
        /// `frames` の整数部で評価した値。`frames` と同じ長さ・同じ順序。
        values: Vec<bool>,
    },
}

/// トラックバーのグループ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackGroup {
    /// グループ名。
    pub name: String,
    /// グループ内での 0 始まりの位置。
    pub index: usize,
    /// グループのトラック数。
    ///
    /// [`item_names`](Self::item_names) の件数と一致するとは限らない。前者は
    /// トラックバー情報が名乗るトラック数、後者は所属アイテム名の列挙結果で
    /// あり、同じ数であるとはどこにも定められていない。一致を強制せず両方を返す。
    pub count: usize,
    /// 所属アイテム名。
    pub item_names: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{AvailableEffectItem, EffectFlags, EffectItemType, EvaluatedItemKind};
    use crate::fingerprint::ObjectFingerprintInput;
    use crate::object::ObjectSummary;
    use crate::page::DEFAULT_PAGE_LIMIT;
    use crate::validation::MAX_NAME_UTF16_UNITS;

    fn sample_object_summary() -> ObjectSummary {
        ObjectSummary::new(
            "78be92d1-c8c9-44c6-ae52-387548971468",
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
        sample_object_summary().selector
    }

    fn sample_page_meta() -> PageMeta {
        PageMeta {
            total_count: 3,
            count: 1,
            offset: 0,
            has_more: true,
            next_offset: Some(1),
            snapshot_revision: 42,
        }
    }

    #[test]
    fn operation_names_are_snake_case() {
        assert_eq!(OPERATION_GET_EDIT_INFO, "get_edit_info");
        assert_eq!(OPERATION_GET_CURRENT_SCENE, "get_current_scene");
        assert_eq!(OPERATION_LIST_LAYERS, "list_layers");
        assert_eq!(OPERATION_LIST_OBJECTS, "list_objects");
        assert_eq!(OPERATION_GET_OBJECT, "get_object");
        assert_eq!(OPERATION_LIST_AVAILABLE_EFFECTS, "list_available_effects");
        assert_eq!(OPERATION_GET_EFFECT_ITEM_VALUES, "get_effect_item_values");
    }

    #[test]
    fn batch_and_render_operation_names_are_snake_case() {
        assert_eq!(OPERATION_APPLY_BATCH, "apply_batch");
        assert_eq!(OPERATION_RENDER_FRAME, "render_frame");
    }

    #[test]
    fn edit_operation_names_are_snake_case() {
        assert_eq!(OPERATION_CREATE_OBJECT, "create_object");
        assert_eq!(OPERATION_MOVE_OBJECT, "move_object");
        assert_eq!(OPERATION_DELETE_OBJECT, "delete_object");
        assert_eq!(OPERATION_SET_OBJECT_NAME, "set_object_name");
        assert_eq!(OPERATION_SET_OBJECT_ITEM, "set_object_item");
        assert_eq!(OPERATION_ADD_EFFECT, "add_effect");
        assert_eq!(OPERATION_DELETE_EFFECT, "delete_effect");
        assert_eq!(OPERATION_SET_EFFECT_ENABLED, "set_effect_enabled");
        assert_eq!(OPERATION_SET_LAYER_STATE, "set_layer_state");
        assert_eq!(OPERATION_SET_SELECTION, "set_selection");
        assert_eq!(OPERATION_CREATE_OBJECT_SECTION, "create_object_section");
        assert_eq!(OPERATION_DELETE_OBJECT_SECTION, "delete_object_section");
        assert_eq!(OPERATION_MOVE_OBJECT_SECTION, "move_object_section");
        assert_eq!(OPERATION_APPLY_BATCH, "apply_batch");
    }

    #[test]
    fn edit_operation_as_str_matches_the_operation_constants() {
        assert_eq!(
            EditOperation::CreateObject.as_str(),
            OPERATION_CREATE_OBJECT
        );
        assert_eq!(EditOperation::MoveObject.as_str(), OPERATION_MOVE_OBJECT);
        assert_eq!(
            EditOperation::DeleteObject.as_str(),
            OPERATION_DELETE_OBJECT
        );
        assert_eq!(
            EditOperation::SetObjectName.as_str(),
            OPERATION_SET_OBJECT_NAME
        );
        assert_eq!(
            EditOperation::SetObjectItem.as_str(),
            OPERATION_SET_OBJECT_ITEM
        );
        assert_eq!(EditOperation::AddEffect.as_str(), OPERATION_ADD_EFFECT);
        assert_eq!(
            EditOperation::DeleteEffect.as_str(),
            OPERATION_DELETE_EFFECT
        );
        assert_eq!(
            EditOperation::SetEffectEnabled.as_str(),
            OPERATION_SET_EFFECT_ENABLED
        );
        assert_eq!(
            EditOperation::SetLayerState.as_str(),
            OPERATION_SET_LAYER_STATE
        );
        assert_eq!(
            EditOperation::SetSelection.as_str(),
            OPERATION_SET_SELECTION
        );
        assert_eq!(
            EditOperation::CreateObjectSection.as_str(),
            OPERATION_CREATE_OBJECT_SECTION
        );
        assert_eq!(
            EditOperation::DeleteObjectSection.as_str(),
            OPERATION_DELETE_OBJECT_SECTION
        );
        assert_eq!(
            EditOperation::MoveObjectSection.as_str(),
            OPERATION_MOVE_OBJECT_SECTION
        );
        assert_eq!(EditOperation::ApplyBatch.as_str(), OPERATION_APPLY_BATCH);
    }

    #[test]
    fn read_operation_as_str_matches_the_operation_constants() {
        assert_eq!(ReadOperation::GetEditInfo.as_str(), OPERATION_GET_EDIT_INFO);
        assert_eq!(
            ReadOperation::GetCurrentScene.as_str(),
            OPERATION_GET_CURRENT_SCENE
        );
        assert_eq!(ReadOperation::ListLayers.as_str(), OPERATION_LIST_LAYERS);
        assert_eq!(ReadOperation::ListObjects.as_str(), OPERATION_LIST_OBJECTS);
        assert_eq!(ReadOperation::GetObject.as_str(), OPERATION_GET_OBJECT);
        assert_eq!(
            ReadOperation::ListAvailableEffects.as_str(),
            OPERATION_LIST_AVAILABLE_EFFECTS
        );
        assert_eq!(
            ReadOperation::GetEffectItemValues.as_str(),
            OPERATION_GET_EFFECT_ITEM_VALUES
        );
    }

    #[test]
    fn render_operation_as_str_matches_the_operation_constants() {
        assert_eq!(
            RenderOperation::RenderFrame.as_str(),
            OPERATION_RENDER_FRAME
        );
    }

    #[test]
    fn read_operation_from_operation_name_round_trips_through_all() {
        for op in ReadOperation::ALL {
            assert_eq!(ReadOperation::from_operation_name(op.as_str()), Some(op));
        }
        for name in ["", "ping", OPERATION_MOVE_OBJECT, OPERATION_RENDER_FRAME] {
            assert_eq!(ReadOperation::from_operation_name(name), None);
        }
    }

    #[test]
    fn render_operation_from_operation_name_round_trips_through_all() {
        for op in RenderOperation::ALL {
            assert_eq!(RenderOperation::from_operation_name(op.as_str()), Some(op));
        }
        for name in ["", "ping", OPERATION_GET_EDIT_INFO, OPERATION_APPLY_BATCH] {
            assert_eq!(RenderOperation::from_operation_name(name), None);
        }
    }

    #[test]
    fn edit_operation_from_operation_name_round_trips_through_all() {
        for op in EditOperation::ALL {
            assert_eq!(EditOperation::from_operation_name(op.as_str()), Some(op));
        }
        for name in ["", "ping", OPERATION_GET_EDIT_INFO, "future_operation"] {
            assert_eq!(EditOperation::from_operation_name(name), None);
        }
    }

    /// [`EditOperation::ALL`] が全 variant を含むことを固定する。
    ///
    /// `assert_listed` の中身は網羅 match であり、`EditOperation` へ variant を
    /// 追加すると腕が足りずコンパイルが落ちる。腕を追加した際に対応する
    /// `assert_listed(...)` 呼び出しを下の一覧へ足し忘れると、その variant は
    /// `ALL` に含まれるかを確認されないまま残る。呼び出し一覧と `ALL` は
    /// どちらも本テストの中でだけ手で書く 2 つの独立した表現であり、
    /// 一方だけを更新すると `assert!` が実行時に落ちて食い違いが分かる。
    #[test]
    fn edit_operation_all_is_exhaustive() {
        fn assert_listed(op: EditOperation) {
            match op {
                EditOperation::CreateObject
                | EditOperation::MoveObject
                | EditOperation::DeleteObject
                | EditOperation::SetObjectName
                | EditOperation::SetObjectItem
                | EditOperation::AddEffect
                | EditOperation::DeleteEffect
                | EditOperation::SetEffectEnabled
                | EditOperation::SetLayerState
                | EditOperation::SetSelection
                | EditOperation::CreateObjectSection
                | EditOperation::DeleteObjectSection
                | EditOperation::MoveObjectSection
                | EditOperation::ApplyBatch => {}
            }
            assert!(
                EditOperation::ALL.contains(&op),
                "{op:?} が EditOperation::ALL に含まれていません"
            );
        }

        assert_listed(EditOperation::CreateObject);
        assert_listed(EditOperation::MoveObject);
        assert_listed(EditOperation::DeleteObject);
        assert_listed(EditOperation::SetObjectName);
        assert_listed(EditOperation::SetObjectItem);
        assert_listed(EditOperation::AddEffect);
        assert_listed(EditOperation::DeleteEffect);
        assert_listed(EditOperation::SetEffectEnabled);
        assert_listed(EditOperation::SetLayerState);
        assert_listed(EditOperation::SetSelection);
        assert_listed(EditOperation::CreateObjectSection);
        assert_listed(EditOperation::DeleteObjectSection);
        assert_listed(EditOperation::MoveObjectSection);
        assert_listed(EditOperation::ApplyBatch);
        assert_eq!(EditOperation::ALL.len(), 14);
    }

    /// [`ReadOperation::ALL`] が全 variant を含むことを固定する。
    ///
    /// 仕組みは `edit_operation_all_is_exhaustive` と同じである。
    #[test]
    fn read_operation_all_is_exhaustive() {
        fn assert_listed(op: ReadOperation) {
            match op {
                ReadOperation::GetEditInfo
                | ReadOperation::GetCurrentScene
                | ReadOperation::ListLayers
                | ReadOperation::ListObjects
                | ReadOperation::GetObject
                | ReadOperation::ListAvailableEffects
                | ReadOperation::GetEffectItemValues => {}
            }
            assert!(
                ReadOperation::ALL.contains(&op),
                "{op:?} が ReadOperation::ALL に含まれていません"
            );
        }

        assert_listed(ReadOperation::GetEditInfo);
        assert_listed(ReadOperation::GetCurrentScene);
        assert_listed(ReadOperation::ListLayers);
        assert_listed(ReadOperation::ListObjects);
        assert_listed(ReadOperation::GetObject);
        assert_listed(ReadOperation::ListAvailableEffects);
        assert_listed(ReadOperation::GetEffectItemValues);
        assert_eq!(ReadOperation::ALL.len(), 7);
    }

    /// [`RenderOperation::ALL`] が全 variant を含むことを固定する。
    ///
    /// 仕組みは `edit_operation_all_is_exhaustive` と同じである。
    #[test]
    fn render_operation_all_is_exhaustive() {
        fn assert_listed(op: RenderOperation) {
            match op {
                RenderOperation::RenderFrame => {}
            }
            assert!(
                RenderOperation::ALL.contains(&op),
                "{op:?} が RenderOperation::ALL に含まれていません"
            );
        }

        assert_listed(RenderOperation::RenderFrame);
        assert_eq!(RenderOperation::ALL.len(), 1);
    }

    #[test]
    fn known_operation_classifies_every_operation_name() {
        for op in ReadOperation::ALL {
            assert_eq!(
                KnownOperation::from_operation_name(op.as_str()),
                Some(KnownOperation::Read(op))
            );
        }
        for op in EditOperation::ALL {
            assert_eq!(
                KnownOperation::from_operation_name(op.as_str()),
                Some(KnownOperation::Edit(op))
            );
        }
        for op in RenderOperation::ALL {
            assert_eq!(
                KnownOperation::from_operation_name(op.as_str()),
                Some(KnownOperation::Render(op))
            );
        }
        for name in ["", "ping", "future_operation"] {
            assert_eq!(KnownOperation::from_operation_name(name), None);
        }
    }

    #[test]
    fn known_operation_as_str_round_trips() {
        for name in ReadOperation::ALL
            .into_iter()
            .map(ReadOperation::as_str)
            .chain(EditOperation::ALL.into_iter().map(EditOperation::as_str))
            .chain(
                RenderOperation::ALL
                    .into_iter()
                    .map(RenderOperation::as_str),
            )
        {
            let operation = KnownOperation::from_operation_name(name).expect("分類できる名前");
            assert_eq!(operation.as_str(), name);
        }
    }

    #[test]
    fn known_operation_budget_kind_separates_batch_and_render() {
        for op in ReadOperation::ALL {
            assert_eq!(
                KnownOperation::Read(op).budget_kind(),
                RequestBudgetKind::Read,
                "{op:?} が read の予算区分になっていません"
            );
        }
        for op in EditOperation::ALL {
            // `_` を使わない網羅 match である。網羅 match は「分類し忘れ」を
            // 捕まえるが「誤って分類した」は捕まえない。期待値を variant ごとに
            // 書き並べることで、後者もここで落ちる。
            let expected = match op {
                EditOperation::ApplyBatch => RequestBudgetKind::Batch,
                EditOperation::CreateObject
                | EditOperation::MoveObject
                | EditOperation::DeleteObject
                | EditOperation::SetObjectName
                | EditOperation::SetObjectItem
                | EditOperation::AddEffect
                | EditOperation::DeleteEffect
                | EditOperation::SetEffectEnabled
                | EditOperation::SetLayerState
                | EditOperation::SetSelection
                | EditOperation::CreateObjectSection
                | EditOperation::DeleteObjectSection
                | EditOperation::MoveObjectSection => RequestBudgetKind::Edit,
            };
            assert_eq!(
                KnownOperation::Edit(op).budget_kind(),
                expected,
                "{op:?} の予算区分が想定と異なります"
            );
        }
        for op in RenderOperation::ALL {
            assert_eq!(
                KnownOperation::Render(op).budget_kind(),
                RequestBudgetKind::Render,
                "{op:?} が render の予算区分になっていません"
            );
        }
    }

    #[test]
    fn object_section_operations_are_ordinary_edits() {
        // 中間点の操作の費用は単一編集と同じ形である（1 対象の解決 + 1 回の
        // 変更）。新しい予算区分を作る理由が無い。
        for op in [
            EditOperation::CreateObjectSection,
            EditOperation::DeleteObjectSection,
            EditOperation::MoveObjectSection,
        ] {
            assert_eq!(
                KnownOperation::Edit(op).budget_kind(),
                RequestBudgetKind::Edit,
                "{op:?}"
            );
            assert!(EditOperation::ALL.contains(&op));
        }
    }

    #[test]
    fn evaluating_item_values_is_an_ordinary_read() {
        // 費用の形は 1 対象の解決と上限付きの値取得であり、既存の read と同じ桁
        // である。新しい予算区分を作る理由が無い。
        assert_eq!(
            KnownOperation::Read(ReadOperation::GetEffectItemValues).budget_kind(),
            RequestBudgetKind::Read
        );
        assert!(ReadOperation::ALL.contains(&ReadOperation::GetEffectItemValues));
    }

    fn sample_effect_selector() -> EffectSelector {
        let object = sample_object_selector();
        EffectSelector {
            fingerprint: object.fingerprint.clone(),
            object,
            effect_name: "動画ファイル".to_string(),
            effect_index: 0,
        }
    }

    fn sample_item_values_params(frames: usize, items: Option<usize>) -> GetEffectItemValuesParams {
        GetEffectItemValuesParams {
            effect: sample_effect_selector(),
            frames: (0..frames)
                .map(|i| FiniteF64::try_new(i as f64).expect("有限値"))
                .collect(),
            items: items.map(|count| (0..count).map(|i| format!("項目{i}")).collect::<Vec<_>>()),
        }
    }

    #[test]
    fn get_effect_item_values_params_roundtrip() {
        for items in [None, Some(1), Some(MAX_EVALUATED_ITEMS)] {
            let params = sample_item_values_params(2, items);
            let s = serde_json::to_string(&params).unwrap();
            let restored: GetEffectItemValuesParams = serde_json::from_str(&s).unwrap();
            assert_eq!(restored, params);
        }
    }

    #[test]
    fn get_effect_item_values_params_reject_unknown_field() {
        let mut value = serde_json::to_value(sample_item_values_params(1, None)).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::json!(1));
        assert!(serde_json::from_value::<GetEffectItemValuesParams>(value).is_err());
    }

    #[test]
    fn get_effect_item_values_params_bound_the_frame_count() {
        for count in [1, 2, MAX_EVALUATED_FRAMES] {
            assert_eq!(
                sample_item_values_params(count, None).validate(),
                Ok(()),
                "{count} 件のフレームが拒否されました"
            );
        }
        for count in [0, MAX_EVALUATED_FRAMES + 1] {
            assert_eq!(
                sample_item_values_params(count, None).validate(),
                Err(EffectItemValuesInputError::FrameCountOutOfRange { count }),
                "{count} 件のフレームが受理されました"
            );
        }
    }

    #[test]
    fn get_effect_item_values_params_bound_the_item_count() {
        for count in [1, MAX_EVALUATED_ITEMS] {
            assert_eq!(
                sample_item_values_params(1, Some(count)).validate(),
                Ok(()),
                "{count} 件の項目が拒否されました"
            );
        }
        for count in [0, MAX_EVALUATED_ITEMS + 1] {
            assert_eq!(
                sample_item_values_params(1, Some(count)).validate(),
                Err(EffectItemValuesInputError::ItemCountOutOfRange { count }),
                "{count} 件の項目が受理されました"
            );
        }
    }

    #[test]
    fn get_effect_item_values_params_apply_the_shared_name_rule() {
        // 項目名の規則は名前の検証を共有する。別の規則を書き起こすと、同じ名前が
        // 経路によって受理されたり拒否されたりする。
        for name in ["名\0前", &"あ".repeat(MAX_NAME_UTF16_UNITS + 1)] {
            let params = GetEffectItemValuesParams {
                items: Some(vec![name.to_string()]),
                ..sample_item_values_params(1, None)
            };
            let error = params
                .validate()
                .expect_err("規則違反の名前が受理されました");
            assert!(matches!(error, EffectItemValuesInputError::ItemName { .. }));
        }
    }

    #[test]
    fn effect_item_values_result_roundtrip() {
        let result = EffectItemValues {
            project_revision: 42,
            frames: vec![
                FiniteF64::try_new(120.0).unwrap(),
                FiniteF64::try_new(120.5).unwrap(),
            ],
            items: vec![
                EvaluatedItem::Track {
                    name: "X".to_string(),
                    values: vec![
                        FiniteF64::try_new(0.0).unwrap(),
                        FiniteF64::try_new(1.5).unwrap(),
                    ],
                    group: Some(TrackGroup {
                        name: "座標".to_string(),
                        index: 0,
                        count: 3,
                        item_names: vec!["X".to_string(), "Y".to_string()],
                    }),
                },
                EvaluatedItem::Check {
                    name: "反転".to_string(),
                    values: vec![true, false],
                },
            ],
            truncated: false,
        };
        let s = serde_json::to_string(&result).unwrap();
        let restored: EffectItemValues = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, result);
    }

    #[test]
    fn evaluated_items_are_told_apart_by_a_tag() {
        // 値の型が種別ごとに違うことを、判別子で読めるようにする。
        let value = serde_json::to_value(EvaluatedItem::Check {
            name: "反転".to_string(),
            values: vec![true],
        })
        .unwrap();
        assert_eq!(value["type"], serde_json::json!("check"));
        let value = serde_json::to_value(EvaluatedItem::Track {
            name: "X".to_string(),
            values: Vec::new(),
            group: None,
        })
        .unwrap();
        assert_eq!(value["type"], serde_json::json!("track"));
        assert_eq!(value["group"], serde_json::Value::Null);
    }

    #[test]
    fn only_track_and_check_items_can_be_evaluated() {
        assert_eq!(
            EffectItemType::Integer.evaluated_kind(),
            Some(EvaluatedItemKind::Track)
        );
        assert_eq!(
            EffectItemType::Number.evaluated_kind(),
            Some(EvaluatedItemKind::Track)
        );
        assert_eq!(
            EffectItemType::Check.evaluated_kind(),
            Some(EvaluatedItemKind::Check)
        );
        for item_type in [
            EffectItemType::Text,
            EffectItemType::String,
            EffectItemType::File,
            EffectItemType::Color,
            EffectItemType::Select,
            EffectItemType::Scene,
            EffectItemType::Range,
            EffectItemType::Combo,
            EffectItemType::Mask,
            EffectItemType::Font,
            EffectItemType::Figure,
            EffectItemType::Data,
            EffectItemType::Folder,
            EffectItemType::Unknown(99),
        ] {
            assert_eq!(
                item_type.evaluated_kind(),
                None,
                "{item_type} が評価できる種別として扱われました"
            );
        }
    }

    #[test]
    fn params_without_fields_accept_empty_object() {
        assert_eq!(
            serde_json::from_str::<GetEditInfoParams>("{}").unwrap(),
            GetEditInfoParams {}
        );
        assert_eq!(
            serde_json::from_str::<GetCurrentSceneParams>("{}").unwrap(),
            GetCurrentSceneParams {}
        );
        assert_eq!(serde_json::to_string(&GetEditInfoParams {}).unwrap(), "{}");
    }

    #[test]
    fn params_without_fields_reject_unknown_field() {
        assert!(serde_json::from_str::<GetEditInfoParams>(r#"{"future":1}"#).is_err());
        assert!(serde_json::from_str::<GetCurrentSceneParams>(r#"{"future":1}"#).is_err());
    }

    #[test]
    fn list_layers_params_roundtrip() {
        let params = ListLayersParams {
            expected_scene_id: 0,
            page: PageRequest {
                offset: 10,
                limit: 20,
                snapshot_revision: Some(42),
            },
        };
        let s = serde_json::to_string(&params).unwrap();
        let restored: ListLayersParams = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, params);
    }

    #[test]
    fn list_layers_params_wire_form_is_flat() {
        // ページ指定は入れ子にせず、他の一覧系 params と同じく #[serde(flatten)] で平坦に並べる。
        let params = ListLayersParams {
            expected_scene_id: 0,
            page: PageRequest {
                offset: 10,
                limit: 20,
                snapshot_revision: Some(42),
            },
        };
        assert_eq!(
            serde_json::to_value(params).unwrap(),
            serde_json::json!({
                "expected_scene_id": 0,
                "offset": 10,
                "limit": 20,
                "snapshot_revision": 42,
            })
        );
    }

    #[test]
    fn list_layers_params_defaults_page() {
        let params: ListLayersParams = serde_json::from_str(r#"{"expected_scene_id":0}"#).unwrap();
        assert_eq!(params.page.limit, DEFAULT_PAGE_LIMIT);
        assert_eq!(params.page.offset, 0);
        assert_eq!(params.page.snapshot_revision, None);
    }

    #[test]
    fn list_layers_params_accept_flat_page_fields() {
        let params: ListLayersParams =
            serde_json::from_str(r#"{"expected_scene_id":3,"offset":5,"limit":10}"#).unwrap();
        assert_eq!(params.expected_scene_id, 3);
        assert_eq!(params.page.offset, 5);
        assert_eq!(params.page.limit, 10);
    }

    #[test]
    fn list_layers_params_reject_unknown_field() {
        // 平坦に並べても未知フィールドは拒否する。
        assert!(
            serde_json::from_str::<ListLayersParams>(r#"{"expected_scene_id":0,"future":1}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<ListLayersParams>(
                r#"{"expected_scene_id":0,"limit":10,"future":1}"#
            )
            .is_err()
        );
    }

    #[test]
    fn list_layers_params_require_expected_scene_id() {
        assert!(serde_json::from_str::<ListLayersParams>("{}").is_err());
    }

    #[test]
    fn list_objects_params_roundtrip() {
        let params = ListObjectsParams {
            expected_scene_id: 3,
            filter: Some(ObjectFilter {
                layer_min: Some(1),
                layer_max: Some(8),
            }),
            page: PageRequest::default(),
        };
        let s = serde_json::to_string(&params).unwrap();
        let restored: ListObjectsParams = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, params);
    }

    #[test]
    fn list_objects_params_defaults_filter_to_none() {
        let params: ListObjectsParams = serde_json::from_str(r#"{"expected_scene_id":0}"#).unwrap();
        assert_eq!(params.filter, None);
    }

    #[test]
    fn object_filter_rejects_unknown_field() {
        assert!(serde_json::from_str::<ObjectFilter>(r#"{"name":"x"}"#).is_err());
    }

    #[test]
    fn object_filter_accepts_valid_layer_range() {
        for filter in [
            ObjectFilter::default(),
            ObjectFilter {
                layer_min: Some(3),
                layer_max: None,
            },
            ObjectFilter {
                layer_min: None,
                layer_max: Some(3),
            },
            ObjectFilter {
                layer_min: Some(3),
                layer_max: Some(3),
            },
            ObjectFilter {
                layer_min: Some(1),
                layer_max: Some(8),
            },
        ] {
            assert_eq!(filter.validate(), Ok(()));
        }
    }

    #[test]
    fn object_filter_rejects_inverted_layer_range() {
        let filter = ObjectFilter {
            layer_min: Some(8),
            layer_max: Some(1),
        };
        assert_eq!(
            filter.validate(),
            Err(ObjectFilterError::InvertedLayerRange { min: 8, max: 1 })
        );
    }

    #[test]
    fn list_available_effects_params_roundtrip() {
        for effect_type in [
            None,
            Some(EffectType::Filter),
            Some(EffectType::Unknown(42)),
        ] {
            let params = ListAvailableEffectsParams {
                effect_type,
                page: PageRequest::default(),
            };
            let s = serde_json::to_string(&params).unwrap();
            let restored: ListAvailableEffectsParams = serde_json::from_str(&s).unwrap();
            assert_eq!(restored, params);
        }
    }

    #[test]
    fn list_available_effects_params_accept_flat_page_fields() {
        let params: ListAvailableEffectsParams =
            serde_json::from_str(r#"{"effect_type":"filter","offset":2,"limit":5}"#).unwrap();
        assert_eq!(params.effect_type, Some(EffectType::Filter));
        assert_eq!(params.page.offset, 2);
        assert_eq!(params.page.limit, 5);
    }

    #[test]
    fn list_available_effects_params_reject_unknown_field() {
        assert!(serde_json::from_str::<ListAvailableEffectsParams>(r#"{"future":1}"#).is_err());
    }

    #[test]
    fn list_objects_params_reject_unknown_field() {
        assert!(
            serde_json::from_str::<ListObjectsParams>(r#"{"expected_scene_id":0,"future":1}"#)
                .is_err()
        );
    }

    #[test]
    fn get_object_params_roundtrip() {
        let params = GetObjectParams {
            selector: sample_object_selector(),
        };
        let s = serde_json::to_string(&params).unwrap();
        let restored: GetObjectParams = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, params);
    }

    #[test]
    fn get_object_params_reject_unknown_field() {
        let mut value = serde_json::to_value(GetObjectParams {
            selector: sample_object_selector(),
        })
        .unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::json!(1));
        assert!(serde_json::from_value::<GetObjectParams>(value).is_err());
    }

    #[test]
    fn get_current_scene_result_roundtrip() {
        let result = GetCurrentSceneResult {
            scene: SceneInfo {
                id: 0,
                name: Some("Scene 1".to_string()),
                width: 1920,
                height: 1080,
                fps: FiniteF64::try_new(60.0),
                fps_rate: 60,
                fps_scale: 1,
                sample_rate: 48000,
            },
            project_revision: 42,
        };
        let s = serde_json::to_string(&result).unwrap();
        let restored: GetCurrentSceneResult = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, result);
    }

    #[test]
    fn list_layers_result_roundtrip() {
        let result = ListLayersResult {
            items: vec![LayerInfo {
                index: 0,
                name: Some("背景".to_string()),
                enabled: true,
                locked: false,
                object_count: 2,
            }],
            page: sample_page_meta(),
        };
        let s = serde_json::to_string(&result).unwrap();
        let restored: ListLayersResult = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, result);
    }

    #[test]
    fn list_objects_result_roundtrip() {
        let result = ListObjectsResult {
            items: vec![sample_object_summary()],
            page: sample_page_meta(),
        };
        let s = serde_json::to_string(&result).unwrap();
        let restored: ListObjectsResult = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, result);
    }

    #[test]
    fn list_available_effects_result_roundtrip() {
        let result = ListAvailableEffectsResult {
            items: vec![AvailableEffect {
                name: "ぼかし".to_string(),
                effect_type: EffectType::Filter,
                flags: EffectFlags::from_raw(9),
                items: vec![AvailableEffectItem {
                    name: "範囲".to_string(),
                    item_type: EffectItemType::Integer,
                }],
            }],
            page: sample_page_meta(),
        };
        let s = serde_json::to_string(&result).unwrap();
        let restored: ListAvailableEffectsResult = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, result);
    }

    #[test]
    fn results_allow_unknown_optional_fields() {
        let mut value = serde_json::to_value(ListLayersResult {
            items: Vec::new(),
            page: sample_page_meta(),
        })
        .unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::json!(1));
        let restored: ListLayersResult = serde_json::from_value(value).unwrap();
        assert!(restored.items.is_empty());
    }
}
