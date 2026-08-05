//! read operation の名前と params / result、および編集・render operation の名前。
//!
//! 編集 operation と render operation の params / result 型は本モジュールでは
//! 定義しない。

use crate::budget::RequestBudgetKind;
use crate::edit_info::SceneInfo;
use crate::effect::{AvailableEffect, EffectType};
use crate::module::{ModuleEntry, ModuleType};
use crate::number::FiniteF64;
use crate::object::{LayerInfo, ObjectSummary};
use crate::page::{PageMeta, PageRequest};
use crate::palette::PaletteEntry;
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

/// 選択中・フォーカス中のオブジェクトを取得する operation 名。
pub const OPERATION_GET_SELECTION: &str = "get_selection";

/// 登録済みフォント名を列挙する operation 名。
pub const OPERATION_LIST_FONTS: &str = "list_fonts";

/// 登録済みパレットを列挙する operation 名。
pub const OPERATION_LIST_PALETTES: &str = "list_palettes";

/// 登録済みモジュールを列挙する operation 名。
pub const OPERATION_LIST_MODULES: &str = "list_modules";

/// 登録済みオブジェクトエイリアスを列挙する operation 名。
pub const OPERATION_LIST_OBJECT_ALIASES: &str = "list_object_aliases";

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

/// BPM グリッドの一覧を置き換える operation 名。
pub const OPERATION_SET_GRID_BPM: &str = "set_grid_bpm";

/// シーンの名前・解像度・サンプリングレートを変更する operation 名。
pub const OPERATION_SET_SCENE_SETTINGS: &str = "set_scene_settings";

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
    /// [`OPERATION_GET_SELECTION`]。
    GetSelection,
    /// [`OPERATION_LIST_FONTS`]。
    ListFonts,
    /// [`OPERATION_LIST_PALETTES`]。
    ListPalettes,
    /// [`OPERATION_LIST_MODULES`]。
    ListModules,
    /// [`OPERATION_LIST_OBJECT_ALIASES`]。
    ListObjectAliases,
}

impl ReadOperation {
    /// 全 variant。
    ///
    /// 要素数と内容は `read_operation_all_is_exhaustive` テストで固定する。
    pub const ALL: [ReadOperation; 12] = [
        ReadOperation::GetEditInfo,
        ReadOperation::GetCurrentScene,
        ReadOperation::ListLayers,
        ReadOperation::ListObjects,
        ReadOperation::GetObject,
        ReadOperation::ListAvailableEffects,
        ReadOperation::GetEffectItemValues,
        ReadOperation::GetSelection,
        ReadOperation::ListFonts,
        ReadOperation::ListPalettes,
        ReadOperation::ListModules,
        ReadOperation::ListObjectAliases,
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
            ReadOperation::GetSelection => OPERATION_GET_SELECTION,
            ReadOperation::ListFonts => OPERATION_LIST_FONTS,
            ReadOperation::ListPalettes => OPERATION_LIST_PALETTES,
            ReadOperation::ListModules => OPERATION_LIST_MODULES,
            ReadOperation::ListObjectAliases => OPERATION_LIST_OBJECT_ALIASES,
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
    /// [`OPERATION_SET_GRID_BPM`]。
    SetGridBpm,
    /// [`OPERATION_SET_SCENE_SETTINGS`]。
    ///
    /// 取り消せない変更である。ここに並ぶのは、区間の入り方も失敗の写し方も
    /// 他の編集 operation と同じであるためであり、取り消せることを意味しない。
    /// 取り消せない性質は応答と tool の annotation が運ぶ。
    SetSceneSettings,
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
    pub const ALL: [EditOperation; 16] = [
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
        EditOperation::SetGridBpm,
        EditOperation::SetSceneSettings,
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
            EditOperation::SetGridBpm => OPERATION_SET_GRID_BPM,
            EditOperation::SetSceneSettings => OPERATION_SET_SCENE_SETTINGS,
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
                | ReadOperation::GetEffectItemValues
                | ReadOperation::GetSelection
                | ReadOperation::ListFonts
                | ReadOperation::ListPalettes
                | ReadOperation::ListModules
                | ReadOperation::ListObjectAliases => RequestBudgetKind::Read,
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
                | EditOperation::MoveObjectSection
                | EditOperation::SetGridBpm
                | EditOperation::SetSceneSettings => RequestBudgetKind::Edit,
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

/// `get_selection` の params。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetSelectionParams {
    /// 取得対象が現在シーンのままであることを確認するための guard。
    ///
    /// 選択は現在シーンの内側の概念であり、シーンが変わった後の選択を前の
    /// シーンのものとして受け取らないよう必須とする。
    pub expected_scene_id: i32,
    /// ページ指定。要求では offset / limit / snapshot_revision として展開される。
    ///
    /// 掛かるのは [`SelectionSnapshot::selected`] だけである。
    #[serde(flatten)]
    pub page: PageRequest,
}

/// `list_fonts` の params。
///
/// シーン ID の guard を持たない。フォントはシーンに紐づく値ではなく、
/// 何も守らない値を必須にすると要求元は意味の無い値を用意することになる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListFontsParams {
    /// ページ指定。要求では offset / limit / snapshot_revision として展開される。
    #[serde(flatten)]
    pub page: PageRequest,
}

/// `list_palettes` の params。
///
/// シーン ID の guard を持たない理由は [`ListFontsParams`] と同じである。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListPalettesParams {
    /// ページ指定。要求では offset / limit / snapshot_revision として展開される。
    #[serde(flatten)]
    pub page: PageRequest,
}

/// `list_modules` の params。
///
/// シーン ID の guard を持たない理由は [`ListFontsParams`] と同じである。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListModulesParams {
    /// 種別で絞り込む。省略時は全件。
    #[serde(default)]
    pub module_type: Option<ModuleType>,
    /// ページ指定。要求では offset / limit / snapshot_revision として展開される。
    #[serde(flatten)]
    pub page: PageRequest,
}

/// `list_object_aliases` の params。
///
/// シーン ID の guard を持たない理由は [`ListFontsParams`] と同じである。
/// オブジェクトエイリアスはシーンに紐づく値ではない。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListObjectAliasesParams {
    /// ページ指定。要求では offset / limit / snapshot_revision として展開される。
    #[serde(flatten)]
    pub page: PageRequest,
    /// 指定すると、この label を持つエントリだけを返す。
    #[serde(default)]
    pub label: Option<String>,
}

impl ListObjectAliasesParams {
    /// `label` の構文を検証する。
    ///
    /// [`validate_name`] と同じ規則を通す。禁止文字は掛けない — label は名前
    /// ではなく、パスの組み立てにも使わない。
    pub fn validate(&self) -> Result<(), TextSyntaxError> {
        match &self.label {
            Some(label) => validate_name(label),
            None => Ok(()),
        }
    }
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

/// `list_fonts` の result。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListFontsResult {
    /// 切り出されたページの要素。アプリケーション内の登録名であり、`Font` 種別の
    /// 設定項目が受け付ける名前と同じものである。
    pub items: Vec<String>,
    /// ページのメタ情報。
    pub page: PageMeta,
}

/// `list_palettes` の result。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListPalettesResult {
    /// 現在のパレット名。取得できない場合は null。
    ///
    /// ラベル付きの場合は `[ラベル名.パレット名]` の形式になる。分解せずに
    /// そのまま返す。分解の規則を定めると、ラベル名にドットを含む場合の扱いが
    /// 契約になる。
    pub current: Option<String>,
    /// 切り出されたページの要素。
    pub items: Vec<PaletteEntry>,
    /// ページのメタ情報。
    pub page: PageMeta,
}

/// `list_modules` の result。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListModulesResult {
    /// 切り出されたページの要素。
    pub items: Vec<ModuleEntry>,
    /// ページのメタ情報。
    pub page: PageMeta,
}

/// `list_object_aliases` の result。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListObjectAliasesResult {
    /// 切り出されたページの要素。
    pub items: Vec<ObjectAliasSummary>,
    /// ページのメタ情報。
    pub page: PageMeta,
}

/// 登録済みオブジェクトエイリアス 1 件の要約。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectAliasSummary {
    /// ファイル名（拡張子を除いたもの）。作成時に指定する名前である。
    pub name: String,
    /// history.ini 由来の UI ラベル。無ければ null。
    pub label: Option<String>,
    /// 作られるオブジェクト数。形式を判別できなければ null。
    pub object_count: Option<u32>,
    /// エイリアスが含む effect 名の並び。出現順で、重複を保つ。
    pub effects: Vec<String>,
}

/// `get_selection` の result。
///
/// 編集カーソルとフレーム範囲選択は載せない。どちらも [`crate::EditInfo`] が
/// 返しており、同じ値を 2 つの読み取りが返すと、要求元は「どちらが新しいか」を
/// 判断する規則を持つことになる。ここに載せるのは編集情報に無いものだけである。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectionSnapshot {
    /// 取得時点のプロジェクト revision。
    pub project_revision: u64,
    /// オブジェクト設定ウィンドウで選択されているオブジェクト。未選択は null。
    pub focus: Option<ObjectSummary>,
    /// フォーカス対象の区間番号。未選択は null。
    ///
    /// [`Self::focus`] が null のときは必ず null である。
    pub focus_section: Option<usize>,
    /// タイムライン上で選択されているオブジェクト。
    ///
    /// レイヤー番号・開始フレーム番号の昇順で並ぶ。
    pub selected: Vec<ObjectSummary>,
    /// [`Self::selected`] のページのメタ情報。
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
    use crate::palette::{PALETTE_COLOR_COUNT, Rgba};
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
        assert_eq!(OPERATION_GET_SELECTION, "get_selection");
        assert_eq!(OPERATION_LIST_FONTS, "list_fonts");
        assert_eq!(OPERATION_LIST_PALETTES, "list_palettes");
        assert_eq!(OPERATION_LIST_MODULES, "list_modules");
        assert_eq!(OPERATION_LIST_OBJECT_ALIASES, "list_object_aliases");
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
        assert_eq!(OPERATION_SET_SCENE_SETTINGS, "set_scene_settings");
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
        assert_eq!(
            EditOperation::SetSceneSettings.as_str(),
            OPERATION_SET_SCENE_SETTINGS
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
        assert_eq!(
            ReadOperation::GetSelection.as_str(),
            OPERATION_GET_SELECTION
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
                | EditOperation::SetGridBpm
                | EditOperation::SetSceneSettings
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
        assert_listed(EditOperation::SetGridBpm);
        assert_listed(EditOperation::SetSceneSettings);
        assert_listed(EditOperation::ApplyBatch);
        assert_eq!(EditOperation::ALL.len(), 16);
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
                | ReadOperation::GetEffectItemValues
                | ReadOperation::GetSelection
                | ReadOperation::ListFonts
                | ReadOperation::ListPalettes
                | ReadOperation::ListModules
                | ReadOperation::ListObjectAliases => {}
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
        assert_listed(ReadOperation::GetSelection);
        assert_listed(ReadOperation::ListFonts);
        assert_listed(ReadOperation::ListPalettes);
        assert_listed(ReadOperation::ListModules);
        assert_listed(ReadOperation::ListObjectAliases);
        assert_eq!(ReadOperation::ALL.len(), 12);
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
                | EditOperation::MoveObjectSection
                | EditOperation::SetGridBpm
                | EditOperation::SetSceneSettings => RequestBudgetKind::Edit,
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
    fn reading_the_selection_is_an_ordinary_read() {
        // 費用は list_objects と同じ形である（ページを切り出してから alias を
        // 読む）。新しい予算区分を作る理由が無い。
        let op = ReadOperation::GetSelection;
        assert_eq!(
            KnownOperation::Read(op).budget_kind(),
            RequestBudgetKind::Read
        );
        assert!(ReadOperation::ALL.contains(&op));
        assert_eq!(
            ReadOperation::from_operation_name(OPERATION_GET_SELECTION),
            Some(op)
        );
    }

    #[test]
    fn listing_the_catalogs_is_an_ordinary_read() {
        // 4 つとも費用は既存の列挙と同じ形（列挙、または列挙 + 窓の分だけの
        // 読み取り）であり、新しい予算区分を作る理由が無い。
        for (op, name) in [
            (ReadOperation::ListFonts, OPERATION_LIST_FONTS),
            (ReadOperation::ListPalettes, OPERATION_LIST_PALETTES),
            (ReadOperation::ListModules, OPERATION_LIST_MODULES),
            (
                ReadOperation::ListObjectAliases,
                OPERATION_LIST_OBJECT_ALIASES,
            ),
        ] {
            assert_eq!(
                KnownOperation::Read(op).budget_kind(),
                RequestBudgetKind::Read,
                "{op:?}"
            );
            assert!(ReadOperation::ALL.contains(&op), "{op:?}");
            assert_eq!(ReadOperation::from_operation_name(name), Some(op));
        }
    }

    #[test]
    fn replacing_the_grid_bpm_is_an_ordinary_edit() {
        // 費用は単一編集と同じ形である（1 回の変更 + 1 回の読み直し）。新しい
        // 予算区分を作る理由が無い。
        let op = EditOperation::SetGridBpm;
        assert_eq!(
            KnownOperation::Edit(op).budget_kind(),
            RequestBudgetKind::Edit
        );
        assert!(EditOperation::ALL.contains(&op));
        assert_eq!(
            EditOperation::from_operation_name(OPERATION_SET_GRID_BPM),
            Some(op)
        );
    }

    #[test]
    fn changing_the_scene_settings_is_an_ordinary_edit() {
        // SDK 呼び出しは最大 3 回、読み直しはシーン名と編集情報の 2 回であり、
        // 費用は単一編集の範囲に収まる。取り消せないことは費用と関係が無く、
        // 新しい予算区分を作る理由にはならない。
        let op = EditOperation::SetSceneSettings;
        assert_eq!(
            KnownOperation::Edit(op).budget_kind(),
            RequestBudgetKind::Edit
        );
        assert!(EditOperation::ALL.contains(&op));
        assert_eq!(
            EditOperation::from_operation_name(OPERATION_SET_SCENE_SETTINGS),
            Some(op)
        );
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
    fn catalog_params_accept_flat_page_fields() {
        let fonts: ListFontsParams = serde_json::from_str(r#"{"offset":2,"limit":5}"#).unwrap();
        assert_eq!((fonts.page.offset, fonts.page.limit), (2, 5));
        let palettes: ListPalettesParams =
            serde_json::from_str(r#"{"offset":3,"limit":6}"#).unwrap();
        assert_eq!((palettes.page.offset, palettes.page.limit), (3, 6));
        let modules: ListModulesParams =
            serde_json::from_str(r#"{"module_type":"plugin_input","offset":4,"limit":7}"#).unwrap();
        assert_eq!(modules.module_type, Some(ModuleType::PluginInput));
        assert_eq!((modules.page.offset, modules.page.limit), (4, 7));
        let aliases: ListObjectAliasesParams =
            serde_json::from_str(r#"{"label":"見出し","offset":8,"limit":9}"#).unwrap();
        assert_eq!(aliases.label, Some("見出し".to_string()));
        assert_eq!((aliases.page.offset, aliases.page.limit), (8, 9));
    }

    #[test]
    fn catalog_params_reject_unknown_field() {
        assert!(serde_json::from_str::<ListFontsParams>(r#"{"future":1}"#).is_err());
        assert!(serde_json::from_str::<ListPalettesParams>(r#"{"future":1}"#).is_err());
        assert!(serde_json::from_str::<ListModulesParams>(r#"{"future":1}"#).is_err());
        assert!(serde_json::from_str::<ListObjectAliasesParams>(r#"{"future":1}"#).is_err());
    }

    #[test]
    fn catalog_params_do_not_take_a_scene_id() {
        // シーンに紐づかない一覧であり、guard として何も守らない値を必須にしない。
        for value in [
            serde_json::to_value(ListFontsParams::default()).unwrap(),
            serde_json::to_value(ListPalettesParams::default()).unwrap(),
            serde_json::to_value(ListModulesParams::default()).unwrap(),
            serde_json::to_value(ListObjectAliasesParams::default()).unwrap(),
        ] {
            let keys: std::collections::BTreeSet<&str> = value
                .as_object()
                .expect("オブジェクト")
                .keys()
                .map(String::as_str)
                .collect();
            assert!(
                !keys.contains("expected_scene_id"),
                "シーン ID を要求しています: {keys:?}"
            );
        }
    }

    #[test]
    fn list_modules_params_default_the_filter_to_none() {
        let params: ListModulesParams = serde_json::from_str("{}").unwrap();
        assert_eq!(params.module_type, None);
    }

    #[test]
    fn list_object_aliases_params_default_the_label_to_none() {
        let params: ListObjectAliasesParams = serde_json::from_str("{}").unwrap();
        assert_eq!(params.label, None);
    }

    #[test]
    fn list_object_aliases_params_validate_accepts_missing_label() {
        assert_eq!(ListObjectAliasesParams::default().validate(), Ok(()));
    }

    #[test]
    fn list_object_aliases_params_label_rejects_nul_and_excess_length() {
        // label の検証は validate_name と同じ規則を通す。
        let params = ListObjectAliasesParams {
            page: PageRequest::default(),
            label: Some("名\0前".to_string()),
        };
        assert_eq!(params.validate(), Err(TextSyntaxError::ContainsNul));

        let params = ListObjectAliasesParams {
            page: PageRequest::default(),
            label: Some("あ".repeat(MAX_NAME_UTF16_UNITS + 1)),
        };
        assert_eq!(
            params.validate(),
            Err(TextSyntaxError::TooLongUtf16 {
                units: MAX_NAME_UTF16_UNITS + 1,
                max: MAX_NAME_UTF16_UNITS,
            })
        );
    }

    #[test]
    fn list_object_aliases_params_label_does_not_forbid_alias_name_characters() {
        // label は名前ではなく、パスの組み立てにも使わない。history.ini 由来の
        // ラベルには「テロップ.赤」のように `.` を含み得るものがあり、名前の
        // 禁止文字を掛けると弾いてしまう。
        let params = ListObjectAliasesParams {
            page: PageRequest::default(),
            label: Some("テロップ.赤".to_string()),
        };
        assert_eq!(params.validate(), Ok(()));
    }

    #[test]
    fn list_fonts_result_roundtrip() {
        let result = ListFontsResult {
            items: vec!["MS UI Gothic".to_string(), "游ゴシック".to_string()],
            page: sample_page_meta(),
        };
        let s = serde_json::to_string(&result).unwrap();
        assert_eq!(serde_json::from_str::<ListFontsResult>(&s).unwrap(), result);
    }

    #[test]
    fn list_palettes_result_roundtrip() {
        let result = ListPalettesResult {
            current: Some("[標準.既定]".to_string()),
            items: vec![PaletteEntry {
                name: "既定".to_string(),
                colors: vec![
                    Rgba {
                        r: 1,
                        g: 2,
                        b: 3,
                        a: 255
                    };
                    PALETTE_COLOR_COUNT
                ],
            }],
            page: sample_page_meta(),
        };
        let s = serde_json::to_string(&result).unwrap();
        let restored: ListPalettesResult = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, result);
        assert_eq!(restored.items[0].colors.len(), 64);
    }

    #[test]
    fn list_palettes_result_keeps_a_missing_current_name_as_null() {
        let result = ListPalettesResult {
            current: None,
            items: Vec::new(),
            page: sample_page_meta(),
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["current"], serde_json::Value::Null);
    }

    #[test]
    fn list_modules_result_roundtrip() {
        let result = ListModulesResult {
            items: vec![ModuleEntry {
                module_type: ModuleType::ScriptObject,
                name: "テキスト".to_string(),
                information: "標準搭載".to_string(),
            }],
            page: sample_page_meta(),
        };
        let s = serde_json::to_string(&result).unwrap();
        assert_eq!(
            serde_json::from_str::<ListModulesResult>(&s).unwrap(),
            result
        );
    }

    #[test]
    fn list_object_aliases_result_roundtrip() {
        let result = ListObjectAliasesResult {
            items: vec![ObjectAliasSummary {
                name: "立ち絵".to_string(),
                label: Some("テロップ".to_string()),
                object_count: Some(1),
                effects: vec!["標準描画".to_string(), "テキスト".to_string()],
            }],
            page: sample_page_meta(),
        };
        let s = serde_json::to_string(&result).unwrap();
        assert_eq!(
            serde_json::from_str::<ListObjectAliasesResult>(&s).unwrap(),
            result
        );
    }

    #[test]
    fn object_alias_summary_keeps_missing_label_and_object_count_as_null() {
        let summary = ObjectAliasSummary {
            name: "立ち絵".to_string(),
            label: None,
            object_count: None,
            effects: Vec::new(),
        };
        let value = serde_json::to_value(&summary).unwrap();
        assert_eq!(value["label"], serde_json::Value::Null);
        assert_eq!(value["object_count"], serde_json::Value::Null);
    }

    #[test]
    fn object_alias_summary_keeps_effect_order_and_duplicates() {
        // 平坦な並びであり、入れ子にしない（§5.3.1）。出現順で重複を保つ。
        let summary = ObjectAliasSummary {
            name: "立ち絵".to_string(),
            label: None,
            object_count: Some(2),
            effects: vec![
                "標準描画".to_string(),
                "テキスト".to_string(),
                "標準描画".to_string(),
            ],
        };
        let s = serde_json::to_string(&summary).unwrap();
        let restored: ObjectAliasSummary = serde_json::from_str(&s).unwrap();
        assert_eq!(restored.effects, summary.effects);
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
    fn get_selection_params_accept_flat_page_fields() {
        let params: GetSelectionParams =
            serde_json::from_str(r#"{"expected_scene_id":3,"offset":2,"limit":5}"#).unwrap();
        assert_eq!(params.expected_scene_id, 3);
        assert_eq!(params.page.offset, 2);
        assert_eq!(params.page.limit, 5);
    }

    #[test]
    fn get_selection_params_require_the_scene_guard() {
        assert!(serde_json::from_str::<GetSelectionParams>(r#"{"offset":0}"#).is_err());
    }

    #[test]
    fn get_selection_params_reject_unknown_field() {
        assert!(
            serde_json::from_str::<GetSelectionParams>(r#"{"expected_scene_id":0,"future":1}"#)
                .is_err()
        );
    }

    #[test]
    fn selection_snapshot_roundtrip() {
        let snapshot = SelectionSnapshot {
            project_revision: 42,
            focus: Some(sample_object_summary()),
            focus_section: Some(2),
            selected: vec![sample_object_summary()],
            page: sample_page_meta(),
        };
        let s = serde_json::to_string(&snapshot).unwrap();
        let restored: SelectionSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, snapshot);
    }

    #[test]
    fn selection_snapshot_carries_neither_the_cursor_nor_the_selected_range() {
        // どちらも編集情報が返す。同じ値を 2 つの読み取りが返すと、要求元は
        // 「どちらが新しいか」を判断する規則を持つことになる。
        let snapshot = SelectionSnapshot {
            project_revision: 0,
            focus: None,
            focus_section: None,
            selected: Vec::new(),
            page: sample_page_meta(),
        };
        let value = serde_json::to_value(&snapshot).unwrap();
        let keys: std::collections::BTreeSet<&str> = value
            .as_object()
            .expect("オブジェクト")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            std::collections::BTreeSet::from([
                "project_revision",
                "focus",
                "focus_section",
                "selected",
                "page",
            ])
        );
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
