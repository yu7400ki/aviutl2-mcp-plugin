//! 編集 tool の入力型。
//!
//! **セレクターは読み取り側と共有する**（[`crate::mcp::input`]）。同じ値を
//! 読み取りの応答から受け取って編集へ送り返すため、族ごとに別の型を持たない。
//!
//! 往復型以外は未知フィールドを拒否する。[`ItemValueInput`] は応答が返した値を
//! そのまま送り返す往復型であり、応答へ optional field が増えたときに往復が
//! 壊れるため拒否しない。読み取りが返した値をそのまま書き戻せることは設定項目の
//! 契約であり、入口で拒否すると読める値が書けなくなる。
//!
//! schema の制約は宣言であり、要求がそれを満たすかどうかは検証されない。
//! 宣言した制約は接続前に実際へ確かめ、違反を `invalid_argument` として返す。
//! 対応は次のとおり。
//!
//! - `instance_id` の長さと書式: [`parse_instance_id`](crate::mcp::input::parse_instance_id)
//! - selector の文字列長と fingerprint の書式:
//!   [`ObjectSelectorInput::to_selector`](crate::mcp::input::ObjectSelectorInput) /
//!   [`EffectSelectorInput::to_selector`](crate::mcp::input::EffectSelectorInput)
//! - `expected_project_epoch` の長さ: [`expected_project_epoch`]
//! - `layer` / `frame` の範囲、名前・パス・alias・設定値の長さと文字種:
//!   各 `to_params` が呼ぶ core の検証（要求元と実行側が同じ実装を共有する）
//!
//! 文字列長の宣言は JSON Schema の `maxLength`（文字数）で表すのに対し、core の
//! 検証はバイト数または UTF-16 code unit 数で数える。どちらも文字数以上を数える
//! ため、core を通った値は宣言した文字数上限を必ず満たす。宣言だけがあって
//! 検証されない制約は生じない。

use crate::mcp::failure::{from_code, invalid_argument};
use crate::mcp::input::{
    EffectSelectorInput, MAX_EPOCH_CHARS, MAX_NAME_CHARS, ObjectSelectorInput, UUID_PATTERN,
    ensure_length,
};
use aviutl2_mcp_core::{
    AddEffectParams, ApplyBatchParams, BatchInputError, BatchOperation, CreateObjectParams,
    CreateObjectSectionParams, CursorPosition, DeleteEffectParams, DeleteObjectParams,
    DeleteObjectSectionParams, Destination, DisplayStart, EditInputError, ErrorObject, FiniteF64,
    FocusChange, GridBpm, ItemValue, LayerNameChange, MAX_ALIAS_BYTES, MAX_BATCH_OPERATIONS,
    MAX_GRID_BPM_ENTRIES, MAX_ITEM_VALUE_BYTES, MAX_PATH_UTF16_UNITS, MAX_POSITION,
    MoveEffectParams, MoveObjectParams, MoveObjectSectionParams, ObjectSource, Placement,
    RangeChange, SceneSize, SetEffectEnabledParams, SetGridBpmParams, SetLayerStateParams,
    SetObjectItemParams, SetObjectNameParams, SetSceneSettingsParams, SetSelectionParams,
    TrackValue,
};
use schemars::JsonSchema;
use serde::Deserialize;

/// 区間番号に許す最小値。
///
/// 区間 0 の開始位置はオブジェクトの開始フレームであって中間点ではないため、
/// 削除も移動もできない。
const MIN_SECTION: u32 = 1;

/// object alias に許す最大文字数。
const MAX_ALIAS_CHARS: u32 = MAX_ALIAS_BYTES as u32;

/// パスに許す最大文字数。
const MAX_PATH_CHARS: u32 = MAX_PATH_UTF16_UNITS as u32;

/// 設定項目の文字列値に許す最大文字数。
const MAX_ITEM_VALUE_CHARS: u32 = MAX_ITEM_VALUE_BYTES as u32;

/// 作成の配置先。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlacementInput {
    /// 現在シーンの一致確認に使うシーン ID。
    pub scene_id: i32,
    /// 0 始まりのレイヤー番号。
    #[schemars(range(max = MAX_POSITION))]
    pub layer: u32,
    /// 0 始まりの開始フレーム番号。
    #[schemars(range(max = MAX_POSITION))]
    pub frame: u32,
}

impl PlacementInput {
    fn to_placement(self) -> Placement {
        Placement {
            scene_id: self.scene_id,
            layer: self.layer,
            frame: self.frame,
        }
    }
}

/// 移動の宛先。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DestinationInput {
    /// 0 始まりのレイヤー番号。
    #[schemars(range(max = MAX_POSITION))]
    pub layer: u32,
    /// 0 始まりの開始フレーム番号。
    #[schemars(range(max = MAX_POSITION))]
    pub frame: u32,
}

impl DestinationInput {
    fn to_destination(self) -> Destination {
        Destination {
            layer: self.layer,
            frame: self.frame,
        }
    }
}

/// 作成元。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObjectSourceInput {
    /// メディアファイルから作成する。
    MediaFile {
        /// 絶対パス。相対パス・device path・代替データストリーム・
        /// ネットワークパス（UNC）は受け付けない。
        /// ネットワーク上の素材はドライブレターを割り当てて指定する。
        #[schemars(length(min = 1, max = MAX_PATH_CHARS))]
        path: String,
    },
    /// object alias から作成する。
    ObjectAlias {
        /// alias ファイルと同じ形式の文字列。複数オブジェクトを含む場合は全てが作成される。
        #[schemars(length(max = MAX_ALIAS_CHARS))]
        alias: String,
    },
    /// エフェクト名から作成する。
    Effect {
        /// エイリアスファイルの effect.name の値。list_available_effects が返す名前をそのまま指定する。
        #[schemars(length(max = MAX_NAME_CHARS))]
        name: String,
    },
    /// 登録済みオブジェクトエイリアスの名前から作成する。
    AliasName {
        /// list_object_aliases が返した名前。エイリアスファイルの中身を読む必要は無い。
        /// `\ / : * ? " ' < > | % = , .` は含められない。これは AviUtl2 の UI が登録時に課す制約である。
        #[schemars(length(min = 1, max = MAX_NAME_CHARS))]
        name: String,
    },
}

impl ObjectSourceInput {
    fn to_source(&self) -> ObjectSource {
        match self {
            ObjectSourceInput::MediaFile { path } => ObjectSource::MediaFile { path: path.clone() },
            ObjectSourceInput::ObjectAlias { alias } => ObjectSource::ObjectAlias {
                alias: alias.clone(),
            },
            ObjectSourceInput::Effect { name } => ObjectSource::Effect { name: name.clone() },
            ObjectSourceInput::AliasName { name } => ObjectSource::AliasName { name: name.clone() },
        }
    }
}

/// カーソルの移動先。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CursorPositionInput {
    /// 0 始まりのレイヤー番号。
    #[schemars(range(max = MAX_POSITION))]
    pub layer: u32,
    /// 0 始まりのフレーム番号。
    #[schemars(range(max = MAX_POSITION))]
    pub frame: u32,
}

impl CursorPositionInput {
    fn to_position(self) -> CursorPosition {
        CursorPosition {
            layer: self.layer,
            frame: self.frame,
        }
    }
}

/// レイヤー編集の表示開始位置。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DisplayStartInput {
    /// 0 始まりの表示開始レイヤー番号。
    #[schemars(range(max = MAX_POSITION))]
    pub layer: u32,
    /// 0 始まりの表示開始フレーム番号。
    #[schemars(range(max = MAX_POSITION))]
    pub frame: u32,
}

impl DisplayStartInput {
    fn to_start(self) -> DisplayStart {
        DisplayStart {
            layer: self.layer,
            frame: self.frame,
        }
    }
}

/// 選択範囲の変更。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RangeChangeInput {
    /// 範囲を設定する。
    Set {
        /// 0 始まりの開始フレーム番号。
        #[schemars(range(max = MAX_POSITION))]
        start: u32,
        /// 0 始まりの終了フレーム番号。
        #[schemars(range(max = MAX_POSITION))]
        end: u32,
    },
    /// 範囲を解除する。
    Clear {},
}

impl RangeChangeInput {
    fn to_change(self) -> RangeChange {
        match self {
            RangeChangeInput::Set { start, end } => RangeChange::Set { start, end },
            RangeChangeInput::Clear {} => RangeChange::Clear {},
        }
    }
}

/// フォーカス対象の変更。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FocusChangeInput {
    /// 対象を選択する。
    Set {
        /// フォーカスするオブジェクト。
        object: ObjectSelectorInput,
    },
    /// 選択を解除する。解決できない対象を指定しても黙って解除されることはない。
    Clear {},
}

impl FocusChangeInput {
    fn to_change(&self) -> Result<FocusChange, ErrorObject> {
        Ok(match self {
            FocusChangeInput::Set { object } => FocusChange::Set {
                object: object.to_selector()?,
            },
            FocusChangeInput::Clear {} => FocusChange::Clear {},
        })
    }
}

/// 設定項目へ書き込む値。
///
/// 読み取りが返す値と同じ形で受け取り、そのまま書き戻せるようにする。
/// selector と同じ往復型であるため、未知フィールドを拒否しない。応答へ
/// optional field が増えたとき、読み取りが返した値を書き戻せなくなる非対称を
/// 作らないためである。必須フィールドの欠落と型不一致は従来どおり拒否する。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ItemValueInput {
    /// 実数。有限値のみ。item_type が number の項目に書ける。
    /// item_type が integer・scene・range の項目へ書くと invalid_argument となる。
    /// トラックバーへ書くと全区間へ同じ値が入る。
    /// 移動を持つ項目へは指定できず、unsupported_operation となる。
    /// 移動を消したい場合は mode を null にした track を送る。
    Number {
        /// 値。
        value: f64,
    },
    /// 整数。item_type が integer・scene・range の項目に書ける。
    /// item_type が number の項目へ書くと invalid_argument となる。
    /// トラックバーへ書くと全区間へ同じ値が入る。
    /// 移動を持つ項目へは指定できず、unsupported_operation となる。
    /// 移動を消したい場合は mode を null にした track を送る。
    /// item_type: scene が指す先のシーンが実在するかをホストは確かめず、
    /// 書き込み直後の読み直しも整数が入ったことしか言えない。存在しないシーンを
    /// 指す値も成功として返る。シーン ID を引く tool は無い。
    Integer {
        /// 値。
        value: i64,
    },
    /// 真偽値。
    Bool {
        /// 値。
        value: bool,
    },
    /// 色。16 進 6 桁（例 `ff8800`）で指定する。読み直すと小文字で返る。
    /// `#` を付けた表記と 3 桁の省略形は受け付けられず、指定した色にならない
    /// だけでなく元の色も失われて白（`ffffff`）になる。
    /// 受け付けられなかったことは書き込みの応答が
    /// unsupported_operation で伝える。
    Color {
        /// 値。
        #[schemars(length(max = MAX_ITEM_VALUE_CHARS))]
        value: String,
    },
    /// 一覧からの選択。
    Choice {
        /// 選択する表示文字列。
        #[schemars(length(max = MAX_ITEM_VALUE_CHARS))]
        value: String,
    },
    /// ファイルパス。
    File {
        /// 絶対パス。相対パス・device path・代替データストリーム・
        /// ネットワークパス（UNC）は受け付けない。
        /// ネットワーク上の素材はドライブレターを割り当てて指定する。
        #[schemars(length(max = MAX_ITEM_VALUE_CHARS))]
        path: String,
    },
    /// フォルダパス。
    Folder {
        /// 絶対パス。相対パス・device path・代替データストリーム・
        /// ネットワークパス（UNC）は受け付けない。
        /// ネットワーク上の場所はドライブレターを割り当てて指定する。
        #[schemars(length(max = MAX_ITEM_VALUE_CHARS))]
        path: String,
    },
    /// フォント名。list_fonts が返す登録済みの名前をそのまま指定する。
    /// 登録されていない名前は書き込みが unsupported_operation となり、
    /// 設定項目は変更前の値のまま残る。
    Font {
        /// フォント名。
        #[schemars(length(max = MAX_ITEM_VALUE_CHARS))]
        name: String,
    },
    /// テキスト。改行とタブを含めて書き込め、読み直すと書いたとおりに返る。
    /// バックスラッシュも書いたとおりに保たれるため、Windows パス・正規表現・
    /// LaTeX をそのまま指定できる。CRLF は LF として保存される。単独の CR は
    /// 受け付けない——保存はされるが描画では行が分かれず、意図を推測できない。
    /// 長さの上限は保存される表記に掛かり、`\` と改行はそれぞれ 2 バイトを
    /// 占める。
    Text {
        /// 値。
        #[schemars(length(max = MAX_ITEM_VALUE_CHARS))]
        value: String,
    },
    /// トラックバーの移動（キーフレーム）。区間ごとに違う値を書く唯一の形である。
    /// トラックバーに数値を書くと全区間へ同じ値が入るため、中間点で値を変えるには
    /// この形を使う。
    /// values は区間の境界ごとの値で、区間数 + 1 個を指定する。中間点が 2 個なら
    /// 3 区間となり 4 個である。区間の数は get_object が返す sections の件数である。
    /// 個数が合わない指定は invalid_argument となる。
    /// 移動を持たないトラックバーへ書けば新しく移動が付く。
    /// **アニメーションを作るのはこの経路である。**
    /// mode を null にし values を 1 要素にすると移動が消えて静的な値になる。
    /// **移動を消す手段はこれだけである。** 移動を持つ項目へ number や integer を
    /// 書く要求は unsupported_operation となり、移動は消えない。
    /// トラックバー以外の設定項目にはこの形を書けず invalid_argument となる。
    /// そのとき details.item_type と details.value_kind が種別と値の形を返す。
    /// 項目が現在移動を持つかは get_object が返す track が null かどうかで分かり、
    /// 値も移動を持つ項目でだけ track の形で返る。
    /// mode には AviUtl2 が持つ移動方法の名前を指定する。
    /// 一覧に無い名前は受け付けず track_mode_unknown を返す。
    /// 一覧に在る名前でも書けないものがあり track_mode_not_writable となる。
    /// 可否は details.known_movements の要素ごとの writable が名乗る。
    /// 書けない名前で移動を消そうとせず、mode を null にする。
    /// 時間制御はフラグではなく移動方法の名前の変種が担うため、
    /// 時間制御を使うにはその変種の名前を mode に指定する。
    /// get_object の track が返す timecontrol はホストの報告であり、ここで
    /// 指定する先は無い。
    /// params を空にすると移動方法ごとの既定値が入る。
    Track {
        /// 区間の境界ごとの値。
        values: Vec<f64>,
        /// 移動方法の名前。null は移動を持たないことを表す。
        #[schemars(length(max = MAX_ITEM_VALUE_CHARS))]
        mode: Option<String>,
        /// 移動方法のパラメータ。空にすると既定値が入る。
        params: Vec<f64>,
        /// 加速を有効にするか。
        accelerate: bool,
        /// 減速を有効にするか。
        decelerate: bool,
        /// 中間点を無視するか。
        twopoint: bool,
        /// 読み取りが返した値をそのまま書き戻すためのフラグ。省略すると 0 になる。
        /// 表せない値を綴ると invalid_argument となり track_flags_not_representable を返す。
        #[serde(default)]
        reserved_flags: u32,
    },
    /// 未対応種別の生値。読み取りは返すが、書き込みには指定できない。
    Unknown {
        /// 生文字列。
        raw: String,
    },
}

impl ItemValueInput {
    /// 設定値へ変換する。書き込みの可否は変換後の検証が判定する。
    fn to_value(&self) -> Result<ItemValue, ErrorObject> {
        Ok(match self {
            ItemValueInput::Number { value } => ItemValue::Number {
                value: FiniteF64::try_new(*value)
                    .ok_or_else(|| invalid_argument("value は有限の数値である必要があります"))?,
            },
            ItemValueInput::Integer { value } => ItemValue::Integer { value: *value },
            ItemValueInput::Bool { value } => ItemValue::Bool { value: *value },
            ItemValueInput::Color { value } => ItemValue::Color {
                value: value.clone(),
            },
            ItemValueInput::Choice { value } => ItemValue::Choice {
                value: value.clone(),
            },
            ItemValueInput::File { path } => ItemValue::File { path: path.clone() },
            ItemValueInput::Folder { path } => ItemValue::Folder { path: path.clone() },
            ItemValueInput::Font { name } => ItemValue::Font { name: name.clone() },
            ItemValueInput::Text { value } => ItemValue::Text {
                value: value.clone(),
            },
            ItemValueInput::Track {
                values,
                mode,
                params,
                accelerate,
                decelerate,
                twopoint,
                reserved_flags,
            } => ItemValue::Track(TrackValue {
                values: finite_values("values", values)?,
                mode: mode.clone(),
                params: finite_values("params", params)?,
                accelerate: *accelerate,
                decelerate: *decelerate,
                twopoint: *twopoint,
                reserved_flags: *reserved_flags,
            }),
            ItemValueInput::Unknown { raw } => ItemValue::Unknown { raw: raw.clone() },
        })
    }
}

/// 実数の並びを有限値の並びへ写す。
fn finite_values(field: &str, values: &[f64]) -> Result<Vec<FiniteF64>, ErrorObject> {
    values
        .iter()
        .map(|value| {
            FiniteF64::try_new(*value).ok_or_else(|| {
                invalid_argument(format!("{field} には有限の数値を指定してください"))
            })
        })
        .collect()
}

/// `create_object` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateObjectInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 作成元。
    pub source: ObjectSourceInput,
    /// 配置先。
    pub placement: PlacementInput,
    /// 直前の読み取りまたは編集の応答が返した project_epoch。省略はできない。作成は対象を指す selector を持たないため、これがプロジェクト境界を照合する唯一の材料である。
    #[schemars(length(min = 1, max = MAX_EPOCH_CHARS))]
    pub expected_project_epoch: String,
}

impl CreateObjectInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<CreateObjectParams, ErrorObject> {
        let params = CreateObjectParams {
            source: self.source.to_source(),
            placement: self.placement.to_placement(),
            expected_project_epoch: expected_project_epoch(&self.expected_project_epoch)?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `move_object` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveObjectInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象オブジェクトのセレクター。
    pub selector: ObjectSelectorInput,
    /// 移動先。
    pub destination: DestinationInput,
}

impl MoveObjectInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<MoveObjectParams, ErrorObject> {
        let params = MoveObjectParams {
            selector: self.selector.to_selector()?,
            destination: self.destination.to_destination(),
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `delete_object` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteObjectInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象オブジェクトのセレクター。
    pub selector: ObjectSelectorInput,
}

impl DeleteObjectInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<DeleteObjectParams, ErrorObject> {
        let params = DeleteObjectParams {
            selector: self.selector.to_selector()?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `set_object_name` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetObjectNameInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象オブジェクトのセレクター。
    pub selector: ObjectSelectorInput,
    /// 新しい名前。null と省略はどちらも標準名へ戻すことを意味する。
    #[serde(default)]
    #[schemars(length(max = MAX_NAME_CHARS))]
    pub name: Option<String>,
}

impl SetObjectNameInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<SetObjectNameParams, ErrorObject> {
        let params = SetObjectNameParams {
            selector: self.selector.to_selector()?,
            name: self.name.clone(),
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `set_object_item` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetObjectItemInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 設定項目を持つ effect のセレクター。設定項目はいずれかの effect に属するため、対象は effect の selector で指す。
    pub selector: EffectSelectorInput,
    /// 設定項目名。
    #[schemars(length(max = MAX_NAME_CHARS))]
    pub item: String,
    /// 設定する値。
    pub value: ItemValueInput,
}

impl SetObjectItemInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<SetObjectItemParams, ErrorObject> {
        let params = SetObjectItemParams {
            selector: self.selector.to_selector()?,
            item: self.item.clone(),
            value: self.value.to_value()?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `add_effect` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddEffectInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 付与先オブジェクトのセレクター。
    pub object: ObjectSelectorInput,
    /// 付与する effect 名。list_available_effects が返す名前を指定する。
    #[schemars(length(max = MAX_NAME_CHARS))]
    pub effect_name: String,
}

impl AddEffectInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<AddEffectParams, ErrorObject> {
        let params = AddEffectParams {
            object: self.object.to_selector()?,
            effect_name: self.effect_name.clone(),
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `delete_effect` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteEffectInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象 effect のセレクター。
    pub selector: EffectSelectorInput,
}

impl DeleteEffectInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<DeleteEffectParams, ErrorObject> {
        let params = DeleteEffectParams {
            selector: self.selector.to_selector()?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `set_effect_enabled` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetEffectEnabledInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象 effect のセレクター。
    pub selector: EffectSelectorInput,
    /// 有効・無効。
    pub enabled: bool,
}

impl SetEffectEnabledInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<SetEffectEnabledParams, ErrorObject> {
        let params = SetEffectEnabledParams {
            selector: self.selector.to_selector()?,
            enabled: self.enabled,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `move_effect` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveEffectInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 動かす effect のセレクター。
    pub selector: EffectSelectorInput,
    /// 移動先の、effect 列全体での位置。0 始まり。
    /// get_object の effects 配列の添字と同じ数え方であり、同名 effect の順序を表す effect_index とは別の値である。
    #[schemars(range(max = MAX_POSITION))]
    pub position: usize,
}

impl MoveEffectInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<MoveEffectParams, ErrorObject> {
        let params = MoveEffectParams {
            selector: self.selector.to_selector()?,
            position: self.position,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// レイヤー名の変更。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LayerNameChangeInput {
    /// 指定した名前にする。
    Set {
        /// 新しいレイヤー名。空文字列は指定できない。標準名へ戻すには reset を使う。
        #[schemars(length(min = 1, max = MAX_NAME_CHARS))]
        name: String,
    },
    /// 標準の名前へ戻す。
    Reset {},
}

impl LayerNameChangeInput {
    fn to_change(&self) -> LayerNameChange {
        match self {
            LayerNameChangeInput::Set { name } => LayerNameChange::Set { name: name.clone() },
            LayerNameChangeInput::Reset {} => LayerNameChange::Reset {},
        }
    }
}

/// `create_object_section` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateObjectSectionInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象オブジェクトのセレクター。
    pub selector: ObjectSelectorInput,
    /// 中間点を追加するフレーム番号。0 始まりのシーンの絶対フレーム番号。
    #[schemars(range(max = MAX_POSITION))]
    pub frame: u32,
}

impl CreateObjectSectionInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<CreateObjectSectionParams, ErrorObject> {
        let params = CreateObjectSectionParams {
            selector: self.selector.to_selector()?,
            frame: self.frame,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `delete_object_section` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteObjectSectionInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象オブジェクトのセレクター。
    pub selector: ObjectSelectorInput,
    /// 削除する中間点を開始位置に持つ区間の番号。1 以上。
    /// 区間の番号と中間点の番号は 1 つずれる。sections[i] が区間番号 i であり、i が 1 以上のとき sections[i].start が i 番目の中間点のフレームである。
    /// sections[0].start はオブジェクトの開始フレームであって中間点ではないため、区間 0 は指定できない。
    ///
    /// 宣言した下限は [`DeleteObjectSectionParams::validate`] が実際に確かめる。
    /// 宣言だけがあって検証されない制約は残さない。
    #[schemars(range(min = MIN_SECTION, max = MAX_POSITION))]
    pub section: u32,
}

impl DeleteObjectSectionInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<DeleteObjectSectionParams, ErrorObject> {
        let params = DeleteObjectSectionParams {
            selector: self.selector.to_selector()?,
            section: self.section,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `move_object_section` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveObjectSectionInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 対象オブジェクトのセレクター。
    pub selector: ObjectSelectorInput,
    /// 移動する中間点を開始位置に持つ区間の番号。1 以上。
    /// 区間の番号と中間点の番号は 1 つずれる。sections[i] が区間番号 i であり、i が 1 以上のとき sections[i].start が i 番目の中間点のフレームである。
    /// sections[0].start はオブジェクトの開始フレームであって中間点ではないため、区間 0 は指定できない。
    ///
    /// 宣言した下限は [`MoveObjectSectionParams::validate`] が実際に確かめる。
    #[schemars(range(min = MIN_SECTION, max = MAX_POSITION))]
    pub section: u32,
    /// 移動先のフレーム番号。0 始まりのシーンの絶対フレーム番号。
    #[schemars(range(max = MAX_POSITION))]
    pub frame: u32,
}

impl MoveObjectSectionInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<MoveObjectSectionParams, ErrorObject> {
        let params = MoveObjectSectionParams {
            selector: self.selector.to_selector()?,
            section: self.section,
            frame: self.frame,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `set_layer_state` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetLayerStateInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 現在シーンの一致確認に使うシーン ID。
    pub expected_scene_id: i32,
    /// 0 始まりのレイヤー番号。
    #[schemars(range(max = MAX_POSITION))]
    pub layer: u32,
    /// レイヤー名。省略時は変更しない。標準名へ戻すには {"type": "reset"} を指定する。
    #[serde(default)]
    pub name: Option<LayerNameChangeInput>,
    /// 表示の有効・無効。省略時は変更しない。
    #[serde(default)]
    pub enabled: Option<bool>,
    /// ロックの有無。省略時は変更しない。
    #[serde(default)]
    pub locked: Option<bool>,
    /// 直前の読み取りまたは編集の応答が返した project_epoch。省略はできない。レイヤーは selector を持たないため、これがプロジェクト境界を照合する唯一の材料である。
    #[schemars(length(min = 1, max = MAX_EPOCH_CHARS))]
    pub expected_project_epoch: String,
}

impl SetLayerStateInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<SetLayerStateParams, ErrorObject> {
        let params = SetLayerStateParams {
            expected_scene_id: self.expected_scene_id,
            layer: self.layer,
            name: self.name.as_ref().map(LayerNameChangeInput::to_change),
            enabled: self.enabled,
            locked: self.locked,
            expected_project_epoch: expected_project_epoch(&self.expected_project_epoch)?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `set_selection` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetSelectionInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 現在シーンの一致確認に使うシーン ID。
    pub expected_scene_id: i32,
    /// カーソル位置。省略時は変更しない。
    #[serde(default)]
    pub cursor: Option<CursorPositionInput>,
    /// 選択範囲。省略時は変更しない。
    #[serde(default)]
    pub selected_range: Option<RangeChangeInput>,
    /// フォーカス対象。省略時は変更しない。
    #[serde(default)]
    pub focus: Option<FocusChangeInput>,
    /// レイヤー編集の表示開始位置。省略時は変更しない。設定できる範囲へ調整される。
    #[serde(default)]
    pub display: Option<DisplayStartInput>,
    /// 直前の読み取りまたは編集の応答が返した project_epoch。省略はできない。focus を省略した要求は selector を 1 つも持たないため、これがプロジェクト境界を照合する材料である。
    #[schemars(length(min = 1, max = MAX_EPOCH_CHARS))]
    pub expected_project_epoch: String,
}

impl SetSelectionInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<SetSelectionParams, ErrorObject> {
        let focus = self
            .focus
            .as_ref()
            .map(FocusChangeInput::to_change)
            .transpose()?;
        let params = SetSelectionParams {
            expected_scene_id: self.expected_scene_id,
            cursor: self.cursor.map(CursorPositionInput::to_position),
            selected_range: self.selected_range.map(RangeChangeInput::to_change),
            focus,
            display: self.display.map(DisplayStartInput::to_start),
            expected_project_epoch: expected_project_epoch(&self.expected_project_epoch)?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// BPM グリッドの 1 件。
///
/// **未知フィールドを拒否しない。** `get_edit_info` が返した要素をそのまま
/// 送り返す往復型である。応答を組み立てるのは接続先の別プロセスであり、版が
/// 揃うとは限らない。新しい接続先が足した field をここで拒むと、応答をそのまま
/// 送り返す往復が壊れる。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
pub struct GridBpmInput {
    /// テンポ。0 より大きい値を指定する。
    pub tempo: f64,
    /// 拍子。1 以上を指定する。
    ///
    /// 宣言した範囲は [`SetGridBpmParams::validate`] が実際に確かめる。
    #[schemars(range(min = MIN_BEAT, max = MAX_BEAT))]
    pub beat: i64,
    /// 開始位置。秒であり、フレーム番号ではない。0 以上を指定する。
    pub start: f64,
    /// 拍子オフセット。秒であり、フレーム番号ではない。
    pub offset: f64,
}

/// 拍子に許す最小値。
const MIN_BEAT: i64 = 1;

/// 拍子に許す最大値。
///
/// ホストは拍子を 32bit 符号付き整数で受け渡す。
const MAX_BEAT: i64 = i32::MAX as i64;

/// BPM 情報の一覧に指定できる最大件数。
const MAX_GRID_BPM_COUNT: u32 = MAX_GRID_BPM_ENTRIES as u32;

impl GridBpmInput {
    /// IPC の DTO へ変換する。
    ///
    /// 有限であることだけを型が課す。値の範囲と重複は core の検証が見る。
    fn to_grid_bpm(self) -> Result<GridBpm, ErrorObject> {
        let finite = |field: &str, value: f64| {
            FiniteF64::try_new(value).ok_or_else(|| {
                invalid_argument(format!(
                    "entries の {field} には有限の数値を指定してください"
                ))
            })
        };
        Ok(GridBpm {
            tempo: finite("tempo", self.tempo)?,
            beat: self.beat,
            start: finite("start", self.start)?,
            offset: finite("offset", self.offset)?,
        })
    }
}

/// `set_grid_bpm` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetGridBpmInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 現在シーンの一致確認に使うシーン ID。
    pub expected_scene_id: i32,
    /// 置き換える BPM 情報の一覧。指定した一覧がそのまま現在の一覧になる。空配列でグリッドを消す。
    ///
    /// 宣言した件数は [`SetGridBpmParams::validate`] が実際に確かめる。
    #[schemars(length(max = MAX_GRID_BPM_COUNT))]
    pub entries: Vec<GridBpmInput>,
    /// 直前の読み取りまたは編集の応答が返した project_epoch。省略はできない。BPM グリッドは selector を持たないため、これがプロジェクト境界を照合する唯一の材料である。
    #[schemars(length(min = 1, max = MAX_EPOCH_CHARS))]
    pub expected_project_epoch: String,
}

impl SetGridBpmInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<SetGridBpmParams, ErrorObject> {
        let entries = self
            .entries
            .iter()
            .map(|entry| entry.to_grid_bpm())
            .collect::<Result<Vec<_>, _>>()?;
        let params = SetGridBpmParams {
            expected_scene_id: self.expected_scene_id,
            entries,
            expected_project_epoch: expected_project_epoch(&self.expected_project_epoch)?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// シーンの解像度。
///
/// **`width` と `height` を上位の型へ平坦化しない。** ホストは解像度を 1 回の
/// 呼び出しで受け取り、片方だけを変える手段を持たない。平坦化すると「横幅だけ
/// 指定」が綴れてしまうため、組にして片方だけの指定を必須欠落として落とす。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SceneSizeInput {
    /// 画像の横幅。
    ///
    /// 宣言した範囲は [`SetSceneSettingsParams::validate`] が実際に確かめる。
    #[schemars(range(min = MIN_SCENE_VALUE, max = MAX_POSITION))]
    pub width: u32,
    /// 画像の高さ。
    ///
    /// 宣言した範囲は [`SetSceneSettingsParams::validate`] が実際に確かめる。
    #[schemars(range(min = MIN_SCENE_VALUE, max = MAX_POSITION))]
    pub height: u32,
}

impl SceneSizeInput {
    fn to_size(self) -> SceneSize {
        SceneSize {
            width: self.width,
            height: self.height,
        }
    }
}

/// シーンの解像度とサンプリングレートに許す最小値。
const MIN_SCENE_VALUE: u32 = 1;

/// `set_scene_settings` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetSceneSettingsInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 現在シーンの一致確認に使うシーン ID。
    pub expected_scene_id: i32,
    /// シーン名。省略時は変更しない。空の名前は受け付けない。
    #[serde(default)]
    #[schemars(length(max = MAX_NAME_CHARS))]
    pub name: Option<String>,
    /// 解像度。省略時は変更しない。width と height は組で指定する。
    #[serde(default)]
    pub size: Option<SceneSizeInput>,
    /// 音声のサンプリングレート。省略時は変更しない。
    ///
    /// 宣言した範囲は [`SetSceneSettingsParams::validate`] が実際に確かめる。
    #[serde(default)]
    #[schemars(range(min = MIN_SCENE_VALUE, max = MAX_POSITION))]
    pub sample_rate: Option<u32>,
    /// 直前の読み取りまたは編集の応答が返した project_epoch。省略はできない。シーンは selector を持たないため、これがプロジェクト境界を照合する唯一の材料である。
    #[schemars(length(min = 1, max = MAX_EPOCH_CHARS))]
    pub expected_project_epoch: String,
}

impl SetSceneSettingsInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<SetSceneSettingsParams, ErrorObject> {
        let params = SetSceneSettingsParams {
            expected_scene_id: self.expected_scene_id,
            name: self.name.clone(),
            size: self.size.map(SceneSizeInput::to_size),
            sample_rate: self.sample_rate,
            expected_project_epoch: expected_project_epoch(&self.expected_project_epoch)?,
        };
        params.validate().map_err(from_input_error)?;
        Ok(params)
    }
}

/// `operations` に指定できる sub-operation の最大件数。
const MAX_BATCH_OPERATION_COUNT: u32 = MAX_BATCH_OPERATIONS as u32;

/// 一括適用の 1 要素。
///
/// **variant は 2 つしか無く、それが除外した編集 operation の拒否を兼ねる。**
/// 他の `type` は未知 variant として復号の段で落ちるため、「一括適用に
/// 入れられない operation か」を実行時に判定する分岐を持たない。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum BatchOperationInput {
    /// オブジェクトのレイヤーと開始フレームを変更する。
    MoveObject {
        /// 対象オブジェクトのセレクター。
        selector: ObjectSelectorInput,
        /// 移動先。
        destination: DestinationInput,
    },
    /// effect の設定項目またはトラックバーの値を変更する。
    SetObjectItem {
        /// 設定項目を持つ effect のセレクター。
        selector: EffectSelectorInput,
        /// 設定項目名。
        #[schemars(length(max = MAX_NAME_CHARS))]
        item: String,
        /// 設定する値。
        value: ItemValueInput,
    },
}

impl BatchOperationInput {
    /// core の sub-operation へ変換する。
    fn to_operation(&self) -> Result<BatchOperation, ErrorObject> {
        Ok(match self {
            BatchOperationInput::MoveObject {
                selector,
                destination,
            } => BatchOperation::MoveObject {
                selector: selector.to_selector()?,
                destination: destination.to_destination(),
            },
            BatchOperationInput::SetObjectItem {
                selector,
                item,
                value,
            } => BatchOperation::SetObjectItem {
                selector: selector.to_selector()?,
                item: item.clone(),
                value: value.to_value()?,
            },
        })
    }
}

/// `apply_batch` の入力。
///
/// 前提条件のフィールドを 1 つも持たない。全 sub-operation が selector を持つ
/// ため、プロジェクト境界も現在シーンも対象の同一性も selector が運ぶ。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplyBatchInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 配列順に適用する sub-operation。
    #[schemars(length(min = 1, max = MAX_BATCH_OPERATION_COUNT))]
    pub operations: Vec<BatchOperationInput>,
}

impl ApplyBatchInput {
    /// IPC の params へ変換する。
    ///
    /// 件数・シーンの一致・重複・各 sub-operation の内容は core の検証がまとめて
    /// 判定する。一括適用のために別の規則を書かない。
    pub fn to_params(&self) -> Result<ApplyBatchParams, ErrorObject> {
        let operations = self
            .operations
            .iter()
            .map(BatchOperationInput::to_operation)
            .collect::<Result<Vec<_>, _>>()?;
        let params = ApplyBatchParams { operations };
        params.validate().map_err(from_batch_input_error)?;
        Ok(params)
    }
}

/// core の一括適用の検証の失敗を tool result のエラーへ写す。
///
/// **何番目の sub-operation で落ちたかを添える。** 100 件の要求に対して位置の
/// 分からない `invalid_argument` は、訂正の手掛かりとして足りない。要求全体の
/// 誤り（件数）は位置を持たないため添えない。
///
/// **失敗の種別も単独編集と同じ名前で添える。** 同じ入力が経路によって違う
/// 応答になれば、要求元は一括適用のためだけの分岐を持つことになる。
fn from_batch_input_error(error: BatchInputError) -> ErrorObject {
    with_input_details(
        from_code(error.error_code(), error.to_string()),
        error.reason(),
        error.failed_index(),
    )
}

/// 入力検証の失敗へ、種別の名前と落ちた位置を添える。
///
/// どちらも持たない失敗には `details` を付けない。載せるのは名前と 0 始まりの
/// 整数だけであり、検証に落ちた値そのものは含まない。
fn with_input_details(
    mapped: ErrorObject,
    reason: Option<&str>,
    failed_index: Option<usize>,
) -> ErrorObject {
    let mut details = serde_json::Map::new();
    if let Some(reason) = reason {
        details.insert("reason".to_string(), serde_json::json!(reason));
    }
    if let Some(index) = failed_index {
        details.insert("failed_index".to_string(), serde_json::json!(index));
    }
    if details.is_empty() {
        return mapped;
    }
    mapped.with_details(serde_json::Value::Object(details))
}

/// 前提の epoch が schema で宣言した文字数の範囲に収まることを確かめる。
fn expected_project_epoch(value: &str) -> Result<String, ErrorObject> {
    ensure_length("expected_project_epoch", value, 1, MAX_EPOCH_CHARS)?;
    Ok(value.to_string())
}

/// core の入力検証の失敗を tool result のエラーへ写す。
///
/// **どの規則で落ちたかを機械可読な形で添える。** パスの構文検証は 7 種、
/// 文字列の構文検証は 4 種の失敗を持ち、要求元が取れる行動はそれぞれ異なる。
/// 名前が無ければ、要求元は説明の文面を解析するほかない。
///
/// 説明にも補助情報にも、検証に失敗した値そのものを含めない。過大な入力を
/// そのまま応答へ写すと、入力の誤りを伝える応答自体が過大になる。
fn from_input_error(error: EditInputError) -> ErrorObject {
    with_input_details(
        from_code(error.error_code(), error.to_string()),
        error.reason(),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::{EditOperation, ErrorCode, ObjectFingerprintInput, ObjectSummary};
    use serde_json::{Value, json};

    const SAMPLE_ID: &str = "8df98c04-e7c2-4f98-b3ce-fc1c39d76414";
    const SAMPLE_EPOCH: &str = "78be92d1-c8c9-44c6-ae52-387548971468";

    fn sample_summary() -> ObjectSummary {
        ObjectSummary::new(
            SAMPLE_EPOCH,
            ObjectFingerprintInput {
                scene_id: 3,
                layer: 2,
                frame_start: 120,
                frame_end: 240,
                name: Some("立ち絵"),
                alias: "alias",
            },
        )
    }

    fn object_selector_json() -> Value {
        serde_json::to_value(sample_summary().selector).expect("直列化できる")
    }

    fn effect_selector_json() -> Value {
        json!({
            "object": object_selector_json(),
            "effect_name": "動画ファイル",
            "effect_index": 0,
            "fingerprint": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        })
    }

    /// tool ごとの、現在の形の入力を引く。入力型を持たない operation は `None`。
    ///
    /// **`_` を使わない網羅 match で書く。** 編集 operation を足すとここが
    /// コンパイルエラーになるため、入力の形を確かめる一連のテストから漏れる
    /// ことがない。手書きの一覧にすると、足し忘れても全て緑のまま通ってしまう。
    fn current_input(operation: EditOperation) -> Option<Value> {
        Some(match operation {
            EditOperation::CreateObject => json!({
                "instance_id": SAMPLE_ID,
                "source": { "type": "object_alias", "alias": "a" },
                "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
                "expected_project_epoch": SAMPLE_EPOCH,
            }),
            EditOperation::MoveObject => json!({
                "instance_id": SAMPLE_ID,
                "selector": object_selector_json(),
                "destination": { "layer": 1, "frame": 0 },
            }),
            EditOperation::DeleteObject => json!({
                "instance_id": SAMPLE_ID,
                "selector": object_selector_json(),
            }),
            EditOperation::SetObjectName => json!({
                "instance_id": SAMPLE_ID,
                "selector": object_selector_json(),
                "name": "名前",
            }),
            EditOperation::SetObjectItem => json!({
                "instance_id": SAMPLE_ID,
                "selector": effect_selector_json(),
                "item": "X",
                "value": { "type": "integer", "value": 1 },
            }),
            EditOperation::AddEffect => json!({
                "instance_id": SAMPLE_ID,
                "object": object_selector_json(),
                "effect_name": "ぼかし",
            }),
            EditOperation::DeleteEffect => json!({
                "instance_id": SAMPLE_ID,
                "selector": effect_selector_json(),
            }),
            EditOperation::SetEffectEnabled => json!({
                "instance_id": SAMPLE_ID,
                "selector": effect_selector_json(),
                "enabled": true,
            }),
            EditOperation::MoveEffect => json!({
                "instance_id": SAMPLE_ID,
                "selector": effect_selector_json(),
                "position": 1,
            }),
            EditOperation::SetLayerState => json!({
                "instance_id": SAMPLE_ID,
                "expected_scene_id": 3,
                "layer": 1,
                "name": { "type": "set", "name": "背景" },
                "expected_project_epoch": SAMPLE_EPOCH,
            }),
            EditOperation::SetSelection => json!({
                "instance_id": SAMPLE_ID,
                "expected_scene_id": 3,
                "cursor": { "layer": 1, "frame": 2 },
                "expected_project_epoch": SAMPLE_EPOCH,
            }),
            EditOperation::CreateObjectSection => json!({
                "instance_id": SAMPLE_ID,
                "selector": object_selector_json(),
                "frame": 150,
            }),
            EditOperation::DeleteObjectSection => json!({
                "instance_id": SAMPLE_ID,
                "selector": object_selector_json(),
                "section": 1,
            }),
            EditOperation::MoveObjectSection => json!({
                "instance_id": SAMPLE_ID,
                "selector": object_selector_json(),
                "section": 1,
                "frame": 160,
            }),
            EditOperation::SetGridBpm => json!({
                "instance_id": SAMPLE_ID,
                "expected_scene_id": 3,
                "entries": [{ "tempo": 120.0, "beat": 4, "start": 0.0, "offset": 0.0 }],
                "expected_project_epoch": SAMPLE_EPOCH,
            }),
            EditOperation::SetSceneSettings => json!({
                "instance_id": SAMPLE_ID,
                "expected_scene_id": 3,
                "name": "本編",
                "size": { "width": 1920, "height": 1080 },
                "sample_rate": 48000,
                "expected_project_epoch": SAMPLE_EPOCH,
            }),
            EditOperation::ApplyBatch => json!({
                "instance_id": SAMPLE_ID,
                "operations": [batch_move_json()],
            }),
        })
    }

    /// 一括適用の sub-operation 1 件分の入力。
    fn batch_move_json() -> Value {
        json!({
            "type": "move_object",
            "selector": object_selector_json(),
            "destination": { "layer": 1, "frame": 0 },
        })
    }

    /// 入力を復元し、IPC の params へ写して返す。
    fn decode_input(operation: EditOperation, input: &Value) -> Result<Value, ErrorCode> {
        /// 復元と変換を済ませて params を JSON へ写す。
        macro_rules! decoded {
            ($ty:ty) => {{
                let input: $ty = serde_json::from_value(input.clone())
                    // 復元の失敗は tool router が invalid_argument へ写す。
                    .map_err(|_| ErrorCode::InvalidArgument)?;
                let params = input.to_params().map_err(|error| error.code)?;
                serde_json::to_value(&params).expect("params は直列化できる")
            }};
        }
        Ok(match operation {
            EditOperation::CreateObject => decoded!(CreateObjectInput),
            EditOperation::MoveObject => decoded!(MoveObjectInput),
            EditOperation::DeleteObject => decoded!(DeleteObjectInput),
            EditOperation::SetObjectName => decoded!(SetObjectNameInput),
            EditOperation::SetObjectItem => decoded!(SetObjectItemInput),
            EditOperation::AddEffect => decoded!(AddEffectInput),
            EditOperation::DeleteEffect => decoded!(DeleteEffectInput),
            EditOperation::SetEffectEnabled => decoded!(SetEffectEnabledInput),
            EditOperation::MoveEffect => decoded!(MoveEffectInput),
            EditOperation::SetLayerState => decoded!(SetLayerStateInput),
            EditOperation::SetSelection => decoded!(SetSelectionInput),
            EditOperation::CreateObjectSection => decoded!(CreateObjectSectionInput),
            EditOperation::DeleteObjectSection => decoded!(DeleteObjectSectionInput),
            EditOperation::MoveObjectSection => decoded!(MoveObjectSectionInput),
            EditOperation::SetGridBpm => decoded!(SetGridBpmInput),
            EditOperation::SetSceneSettings => decoded!(SetSceneSettingsInput),
            EditOperation::ApplyBatch => decoded!(ApplyBatchInput),
        })
    }

    #[test]
    fn every_edit_input_rejects_unknown_fields() {
        // 1 つの tool で通しても、他が受理のままなら気付けないため、全 tool を
        // 網羅 match から引いて同じ表に掛ける。
        for operation in EditOperation::ALL {
            let name = operation.as_str();
            let Some(current) = current_input(operation) else {
                continue;
            };
            decode_input(operation, &current)
                .unwrap_or_else(|code| panic!("{name} の現在の形が拒否されました: {code:?}"));

            let mut unknown = current.clone();
            unknown
                .as_object_mut()
                .unwrap()
                .insert("unknown_field".to_string(), json!(1));
            assert_eq!(
                decode_input(operation, &unknown),
                Err(ErrorCode::InvalidArgument),
                "{name} が未知フィールドを受理しました"
            );

            // 入れ子の未知フィールドも拒否する。往復型は対象から外す。
            for key in current.as_object().expect("入力は object").keys() {
                if is_round_trip_field(key) {
                    continue;
                }
                let mut nested = current.clone();
                let Some(inner) = nested[key].as_object_mut() else {
                    continue;
                };
                inner.insert("unknown_field".to_string(), json!(1));
                assert_eq!(
                    decode_input(operation, &nested),
                    Err(ErrorCode::InvalidArgument),
                    "{name} の {key} が未知フィールドを受理しました"
                );
            }
        }
    }

    #[test]
    fn every_edit_operation_has_an_input_type() {
        // 網羅 match は operation の追加を止めるが、既存の枝を除外へ書き換えても
        // 止まらない。表から外れているものを固定することで、除外を増やしても
        // 減らしてもここが落ちる。
        let excluded: Vec<&str> = EditOperation::ALL
            .into_iter()
            .filter(|operation| current_input(*operation).is_none())
            .map(EditOperation::as_str)
            .collect();

        assert_eq!(excluded, Vec::<&str>::new());
    }

    #[test]
    fn batch_sub_operations_reject_unknown_fields() {
        // 入れ子の未知フィールドは、上の表が配列の要素まで辿らないため個別に
        // 固定する。sub-operation 自身と宛先は拒否し、往復型の selector は
        // 拒否しない。
        let mut unknown = batch_move_json();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("unknown_field".to_string(), json!(1));
        let mut destination = batch_move_json();
        destination["destination"]
            .as_object_mut()
            .unwrap()
            .insert("unknown_field".to_string(), json!(1));
        for (label, operation) in [("sub-operation", unknown), ("destination", destination)] {
            assert_eq!(
                decode_input(
                    EditOperation::ApplyBatch,
                    &json!({ "instance_id": SAMPLE_ID, "operations": [operation] }),
                ),
                Err(ErrorCode::InvalidArgument),
                "{label} が未知フィールドを受理しました"
            );
        }

        let mut selector = batch_move_json();
        selector["selector"]
            .as_object_mut()
            .unwrap()
            .insert("unknown_field".to_string(), json!(1));
        assert!(
            decode_input(
                EditOperation::ApplyBatch,
                &json!({ "instance_id": SAMPLE_ID, "operations": [selector] }),
            )
            .is_ok(),
            "往復型の selector が未知フィールドで拒否されました"
        );
    }

    #[test]
    fn only_two_operation_types_can_be_sub_operations() {
        // union に 2 つしか variant が無いこと自体が、除外した編集 operation の
        // 拒否を兼ねる。実行時に「一括適用へ入れられるか」を判定しない。
        for operation in EditOperation::ALL {
            if matches!(
                operation,
                EditOperation::MoveObject | EditOperation::SetObjectItem
            ) {
                continue;
            }
            let name = operation.as_str();
            let mut sub = batch_move_json();
            sub["type"] = json!(name);
            assert_eq!(
                decode_input(
                    EditOperation::ApplyBatch,
                    &json!({ "instance_id": SAMPLE_ID, "operations": [sub] }),
                ),
                Err(ErrorCode::InvalidArgument),
                "{name} が sub-operation として受理されました"
            );
        }

        // 受け付ける 2 種はそれぞれの形で通る。
        for sub in [
            batch_move_json(),
            json!({
                "type": "set_object_item",
                "selector": effect_selector_json(),
                "item": "X",
                "value": { "type": "integer", "value": 1 },
            }),
        ] {
            assert!(
                decode_input(
                    EditOperation::ApplyBatch,
                    &json!({ "instance_id": SAMPLE_ID, "operations": [sub] }),
                )
                .is_ok(),
                "受け付けるはずの sub-operation が拒否されました"
            );
        }
    }

    #[test]
    fn batch_input_rejects_a_count_outside_the_declared_range() {
        for count in [0, aviutl2_mcp_core::MAX_BATCH_OPERATIONS + 1] {
            let operations: Vec<Value> = (0..count).map(|_| batch_move_json()).collect();
            assert_eq!(
                decode_input(
                    EditOperation::ApplyBatch,
                    &json!({ "instance_id": SAMPLE_ID, "operations": operations }),
                ),
                Err(ErrorCode::InvalidArgument),
                "{count} 件が受理されました"
            );
        }
    }

    #[test]
    fn batch_validation_failures_name_the_operation_that_failed() {
        // 100 件の要求に対して位置の分からない invalid_argument は、訂正の
        // 手掛かりとして足りない。
        let mut second = batch_move_json();
        second["selector"]["scene_id"] = json!(99);
        let input: ApplyBatchInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "operations": [batch_move_json(), second],
        }))
        .expect("入力型としては受理される");

        let error = input.to_params().expect_err("シーンの不一致は拒否される");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.details["failed_index"], json!(1));

        // 要求全体の誤りは位置を持たないため添えない。
        let input: ApplyBatchInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "operations": [],
        }))
        .expect("入力型としては受理される");
        let error = input.to_params().expect_err("空の要求は拒否される");
        assert!(
            error.details.get("failed_index").is_none(),
            "位置を持たない失敗に位置が付きました: {:?}",
            error.details
        );
    }

    #[test]
    fn batch_sub_operations_are_validated_like_the_standalone_edits() {
        // 同じ set_object_item が単独と一括で違う入力を受理すると、要求元は
        // 経路ごとに規則を持つことになる。
        let value = json!({ "type": "text", "value": "字\u{0}幕" });
        let standalone = decode_input(
            EditOperation::SetObjectItem,
            &json!({
                "instance_id": SAMPLE_ID,
                "selector": effect_selector_json(),
                "item": "X",
                "value": value,
            }),
        );
        let batched = decode_input(
            EditOperation::ApplyBatch,
            &json!({
                "instance_id": SAMPLE_ID,
                "operations": [{
                    "type": "set_object_item",
                    "selector": effect_selector_json(),
                    "item": "X",
                    "value": value,
                }],
            }),
        );
        assert_eq!(standalone, Err(ErrorCode::InvalidArgument));
        assert_eq!(batched, Err(ErrorCode::InvalidArgument));
    }

    /// 応答が返した値をそのまま送り返す往復型のフィールドか。
    ///
    /// 往復型は応答へ optional field が増えても往復が壊れないよう、未知
    /// フィールドを拒否しない。
    fn is_round_trip_field(key: &str) -> bool {
        matches!(key, "selector" | "object" | "value")
    }

    #[test]
    fn every_edit_input_ignores_a_fingerprint_algorithm_in_its_selectors() {
        // セレクターは算出方式を運ばないが、往復型なので名乗る指定も拒否せず、
        // 値を接続先へ渡さずに捨てる。
        for operation in EditOperation::ALL {
            let name = operation.as_str();
            let Some(mut input) = current_input(operation) else {
                continue;
            };
            add_algorithms(&mut input);
            let params = decode_input(operation, &input)
                .unwrap_or_else(|_| panic!("{name} がセレクターの算出方式を拒否しました"));
            assert!(
                !params.to_string().contains("fingerprint_algorithm"),
                "{name} が算出方式を接続先へ運びました: {params}"
            );
        }
    }

    /// JSON を辿り、全てのセレクターへ算出方式を足す。
    ///
    /// 要求の形ごとに selector の位置を知らずに済むよう、`fingerprint` を持つ
    /// オブジェクトをセレクターと見なす。
    fn add_algorithms(value: &mut Value) {
        match value {
            Value::Object(map) => {
                if map.contains_key("fingerprint") {
                    map.insert("fingerprint_algorithm".to_string(), json!("sha256-raw-v1"));
                }
                for nested in map.values_mut() {
                    add_algorithms(nested);
                }
            }
            Value::Array(items) => {
                for item in items {
                    add_algorithms(item);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn the_expected_epoch_is_required_where_there_is_no_selector() {
        // 対象を指す selector を持たない要求では、前提の epoch だけがプロジェクト
        // 境界を照合する材料である。省略を認めると、別のプロジェクトへ作成や
        // 選択の変更が通ってしまう。
        assert!(
            serde_json::from_value::<CreateObjectInput>(json!({
                "instance_id": SAMPLE_ID,
                "source": { "type": "object_alias", "alias": "a" },
                "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
            }))
            .is_err(),
            "作成が前提の epoch なしで受理されました"
        );
        assert!(
            serde_json::from_value::<SetSelectionInput>(json!({
                "instance_id": SAMPLE_ID,
                "expected_scene_id": 3,
                "cursor": { "layer": 1, "frame": 2 },
            }))
            .is_err(),
            "選択状態の変更が前提の epoch なしで受理されました"
        );
    }

    #[test]
    fn the_optional_members_of_selection_reject_unknown_fields() {
        // 表が引く現在の形は `cursor` だけを持つ。省略できる残りも同じ扱いで
        // あることを、種別ごとに確かめる。
        for member in [
            json!({ "selected_range": { "type": "set", "start": 0, "end": 1, "future": 1 } }),
            json!({ "selected_range": { "type": "clear", "future": 1 } }),
            json!({ "focus": { "type": "set", "object": object_selector_json(), "future": 1 } }),
            json!({ "focus": { "type": "clear", "future": 1 } }),
        ] {
            let mut selection = json!({
                "instance_id": SAMPLE_ID,
                "expected_scene_id": 3,
                "expected_project_epoch": SAMPLE_EPOCH,
            });
            let (key, value) = member
                .as_object()
                .and_then(|map| map.iter().next())
                .map(|(key, value)| (key.clone(), value.clone()))
                .expect("変更は 1 つ");
            selection[&key] = value;
            assert!(
                serde_json::from_value::<SetSelectionInput>(selection).is_err(),
                "{member} の未知フィールドが受理されました"
            );
        }
    }

    #[test]
    fn selectors_accept_unknown_fields() {
        // 応答へフィールドが増えても、返された selector をそのまま渡せる。
        let mut object = object_selector_json();
        object["future"] = json!(1);
        let mut selector = effect_selector_json();
        selector["object"] = object;
        selector["future"] = json!(1);

        let input: DeleteEffectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": selector,
        }))
        .expect("未知フィールドを含む selector を受理する");

        let params = input.to_params().expect("params へ変換できる");
        assert_eq!(params.selector.effect_name, "動画ファイル");
        assert_eq!(params.selector.effect_index, 0);
        assert_eq!(params.selector.object.layer, 2);
        assert_eq!(params.selector.object.frame, 120);
    }

    #[test]
    fn effect_selector_rejects_malformed_fingerprint() {
        let mut selector = effect_selector_json();
        selector["fingerprint"] = json!("sha256:zzzz");
        let input: DeleteEffectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": selector,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("書式違反は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn effect_selector_strings_are_bounded() {
        let mut selector = effect_selector_json();
        selector["effect_name"] = json!("あ".repeat(MAX_NAME_CHARS as usize + 1));
        let input: DeleteEffectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": selector,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input
                .to_params()
                .expect_err("effect_name の上限超過が受理されました")
                .code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn expected_epoch_is_bounded() {
        let over = "e".repeat(MAX_EPOCH_CHARS as usize + 1);
        let create: CreateObjectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "source": { "type": "object_alias", "alias": "a" },
            "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
            "expected_project_epoch": over,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            create.to_params().expect_err("上限超過は拒否される").code,
            ErrorCode::InvalidArgument
        );

        let selection: SetSelectionInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "cursor": { "layer": 1, "frame": 2 },
            "expected_project_epoch": over,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            selection
                .to_params()
                .expect_err("上限超過は拒否される")
                .code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn positions_beyond_the_declared_maximum_are_rejected() {
        // 宣言した上限は u32 の内側にあるため、入力型を通り抜けた値を検証で拒む。
        let over = MAX_POSITION + 1;

        let create: CreateObjectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "source": { "type": "object_alias", "alias": "a" },
            "placement": { "scene_id": 3, "layer": over, "frame": 0 },
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            create.to_params().expect_err("範囲外は拒否される").code,
            ErrorCode::InvalidArgument
        );

        let move_object: MoveObjectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": object_selector_json(),
            "destination": { "layer": 0, "frame": over },
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            move_object
                .to_params()
                .expect_err("範囲外は拒否される")
                .code,
            ErrorCode::InvalidArgument
        );

        let selection: SetSelectionInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "cursor": { "layer": 0, "frame": over },
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            selection.to_params().expect_err("範囲外は拒否される").code,
            ErrorCode::InvalidArgument
        );

        let range: SetSelectionInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "selected_range": { "type": "set", "start": 0, "end": over },
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            range.to_params().expect_err("範囲外は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn alias_over_the_limit_is_rejected() {
        let input: CreateObjectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "source": { "type": "object_alias", "alias": "a".repeat(MAX_ALIAS_CHARS as usize + 1) },
            "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("上限超過は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn an_effect_source_becomes_the_effect_variant_of_the_params() {
        let input: CreateObjectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "source": { "type": "effect", "name": "テキスト" },
            "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("入力型としては受理される");
        let params = input.to_params().expect("effect 名は受理される");
        assert_eq!(
            params.source,
            ObjectSource::Effect {
                name: "テキスト".to_string(),
            }
        );
    }

    #[test]
    fn an_effect_source_name_over_the_limit_is_rejected() {
        let input: CreateObjectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "source": { "type": "effect", "name": "a".repeat(MAX_NAME_CHARS as usize + 1) },
            "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("上限超過は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn the_path_rules_do_not_reach_an_effect_source() {
        // 作成元がパスを運ばない以上、パスの規則は掛からない。
        for name in [
            r"..\図形",
            r"\\.\図形",
            r"C:\図形:1",
            r"\\server\share\図形",
        ] {
            let input: CreateObjectInput = serde_json::from_value(json!({
                "instance_id": SAMPLE_ID,
                "source": { "type": "effect", "name": name },
                "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
                "expected_project_epoch": SAMPLE_EPOCH,
            }))
            .expect("入力型としては受理される");
            input
                .to_params()
                .unwrap_or_else(|error| panic!("{name} がパスの規則で拒否されました: {error:?}"));
        }
    }

    #[test]
    fn media_path_syntax_is_validated_before_the_request_is_sent() {
        for path in [
            "",
            r"..\movie.mp4",
            r"\\.\PhysicalDrive0",
            r"C:\movie.mp4:stream",
            "movie.mp4",
            r"\\server\share\movie.mp4",
            "//server/share/movie.mp4",
        ] {
            let input: CreateObjectInput = serde_json::from_value(json!({
                "instance_id": SAMPLE_ID,
                "source": { "type": "media_file", "path": path },
                "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
                "expected_project_epoch": SAMPLE_EPOCH,
            }))
            .expect("入力型としては受理される");
            let error = input
                .to_params()
                .err()
                .unwrap_or_else(|| panic!("{path} が受理されました"));
            assert_eq!(error.code, ErrorCode::InvalidArgument, "{path}");
        }
    }

    #[test]
    fn item_path_syntax_is_validated_before_the_request_is_sent() {
        // 設定値のパスも作成元と同じ規則を通る。
        for path in [
            "",
            r"..\image.png",
            r"\\.\PhysicalDrive0",
            r"C:\image.png:stream",
            r"\\server\share\image.png",
            "//server/share/image.png",
        ] {
            for value in [
                json!({ "type": "file", "path": path }),
                json!({ "type": "folder", "path": path }),
            ] {
                let input: SetObjectItemInput = serde_json::from_value(json!({
                    "instance_id": SAMPLE_ID,
                    "selector": effect_selector_json(),
                    "item": "ファイル",
                    "value": value,
                }))
                .expect("入力型としては受理される");
                let error = input
                    .to_params()
                    .err()
                    .unwrap_or_else(|| panic!("{value} が受理されました"));
                assert_eq!(error.code, ErrorCode::InvalidArgument, "{value}");
            }
        }
    }

    #[test]
    fn absolute_media_path_is_accepted() {
        let input: CreateObjectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "source": { "type": "media_file", "path": r"C:\movie.mp4" },
            "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("入力型としては受理される");
        assert!(input.to_params().is_ok());
    }

    #[test]
    fn unknown_item_value_is_rejected_for_write() {
        let input: SetObjectItemInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": effect_selector_json(),
            "item": "テキスト",
            "value": { "type": "unknown", "raw": "opaque" },
        }))
        .expect("読み取りが返した形をそのまま受理する");
        assert_eq!(
            input.to_params().expect_err("未対応種別は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn item_value_strings_are_checked_for_length_and_control_characters() {
        for value in [
            json!({ "type": "text", "value": "a".repeat(MAX_ITEM_VALUE_CHARS as usize + 1) }),
            json!({ "type": "color", "value": "赤\u{1b}" }),
            json!({ "type": "file", "path": r"relative\path.png" }),
        ] {
            let input: SetObjectItemInput = serde_json::from_value(json!({
                "instance_id": SAMPLE_ID,
                "selector": effect_selector_json(),
                "item": "項目",
                "value": value,
            }))
            .expect("入力型としては受理される");
            assert!(input.to_params().is_err(), "{value} が受理されました");
        }
    }

    #[test]
    fn multiline_text_item_value_is_accepted() {
        // 複数行のテキストを 1 回の書き込みで設定できる。
        let input: SetObjectItemInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": effect_selector_json(),
            "item": "テキスト",
            "value": { "type": "text", "value": "1 行目\n2 行目" },
        }))
        .expect("入力型としては受理される");
        assert!(input.to_params().is_ok());
    }

    /// 読み取りが返す設定値の標本を、種別を 1 つずつ辿って組み立てる。
    ///
    /// 次の種別は [`following`] の網羅 `match` が決める。手で並べた配列では、
    /// 種別が増えたときに更新しなくても通ってしまい、増えた種別が書き戻せない
    /// ことに気づけない。
    fn every_item_value() -> Vec<ItemValue> {
        let mut values = vec![ItemValue::Number {
            value: FiniteF64::try_new(1.5).expect("有限値"),
        }];
        while let Some(next) = following(values.last().expect("先頭がある")) {
            assert!(
                !values.iter().any(|value| value.kind() == next.kind()),
                "種別 {} を二度辿っています",
                next.kind()
            );
            values.push(next);
        }
        values
    }

    /// 標本で `previous` の次に置く値を返す。末尾では `None`。
    ///
    /// `_` を使わない網羅 `match` であるため、設定値へ種別が増えるとここが
    /// コンパイルエラーになる。腕を足すには増えた種別を標本の連なりへ繋ぐ
    /// 必要があり、書き戻せるかを確かめないまま通過できない。
    fn following(previous: &ItemValue) -> Option<ItemValue> {
        Some(match previous {
            ItemValue::Number { .. } => ItemValue::Integer { value: -3 },
            ItemValue::Integer { .. } => ItemValue::Bool { value: true },
            ItemValue::Bool { .. } => ItemValue::Color {
                value: "#ff8800".to_string(),
            },
            ItemValue::Color { .. } => ItemValue::Choice {
                value: "通常".to_string(),
            },
            ItemValue::Choice { .. } => ItemValue::File {
                path: r"C:\movie.mp4".to_string(),
            },
            ItemValue::File { .. } => ItemValue::Folder {
                path: r"C:\assets".to_string(),
            },
            ItemValue::Folder { .. } => ItemValue::Font {
                name: "Meiryo".to_string(),
            },
            ItemValue::Font { .. } => ItemValue::Text {
                value: "字幕".to_string(),
            },
            ItemValue::Text { .. } => ItemValue::Track(TrackValue {
                values: [0.0, 100.0]
                    .into_iter()
                    .map(|value| FiniteF64::try_new(value).expect("有限値"))
                    .collect(),
                mode: Some("直線移動".to_string()),
                params: Vec::new(),
                accelerate: false,
                decelerate: false,
                twopoint: false,
                // 名前を持たないビットも往復の対象である。0 を標本に置くと、
                // 入力型がこのフィールドを持たなくても往復が成り立ってしまう。
                reserved_flags: 16,
            }),
            ItemValue::Track(_) => ItemValue::Unknown {
                raw: "future=1".to_string(),
            },
            ItemValue::Unknown { .. } => return None,
        })
    }

    #[test]
    fn the_track_input_states_that_a_listed_movement_may_still_be_unwritable() {
        // 一覧に載る名前が使えるという規律は、載らない名前を拒むだけでは
        // 成り立たない。**書けない名前が混じることと、そのときの持ち替え先を
        // 述べる。** 述べなければ、要求元は一覧から選び直すという通らない手を
        // 打ち続ける。
        let schema = serde_json::to_value(schemars::schema_for!(ItemValueInput))
            .expect("schema は直列化できる")
            .to_string();
        for phrase in [
            "一覧に無い名前は受け付けず track_mode_unknown を返す",
            "一覧に在る名前でも書けないものがあり track_mode_not_writable となる",
            "可否は details.known_movements の要素ごとの writable が名乗る",
            "書けない名前で移動を消そうとせず、mode を null にする",
        ] {
            assert!(
                schema.contains(phrase),
                "移動方法の説明に「{phrase}」がありません"
            );
        }
    }

    /// 入力 schema が受け付ける種別名。
    fn declared_item_value_kinds() -> std::collections::BTreeSet<String> {
        let schema = serde_json::to_value(schemars::schema_for!(ItemValueInput))
            .expect("schema は直列化できる");
        schema["oneOf"]
            .as_array()
            .expect("種別ごとの分岐がある")
            .iter()
            .map(|branch| {
                branch["properties"]["type"]["const"]
                    .as_str()
                    .expect("判別子がある")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn every_read_item_value_round_trips_through_the_input_type() {
        // 読み取りが返す値の形と入力型がずれると、読めた値を書き戻せなくなる。
        let values = every_item_value();

        // 標本が入力 schema の全分岐を覆っていること。覆えていない種別は、
        // 読み取りが返しても書き戻せないまま素通りする。
        let covered: std::collections::BTreeSet<String> = values
            .iter()
            .map(|value| value.kind().to_string())
            .collect();
        assert_eq!(covered, declared_item_value_kinds());

        for value in values {
            let encoded = serde_json::to_value(&value).expect("直列化できる");
            let input: ItemValueInput =
                serde_json::from_value(encoded.clone()).unwrap_or_else(|e| {
                    panic!("読み取りの値を入力型で受け取れません: {encoded} ({e})")
                });
            assert_eq!(input.to_value().expect("変換できる"), value);
        }
    }

    #[test]
    fn a_null_mode_with_one_value_removes_the_movement() {
        // 移動を消す手段はこの形だけである。他に消し方が無いことを、静的な値へ
        // 写ることで固定する。
        let input: ItemValueInput = serde_json::from_value(json!({
            "type": "track",
            "values": [50.0],
            "mode": null,
            "params": [],
            "accelerate": false,
            "decelerate": false,
            "twopoint": false,
        }))
        .expect("受理される");
        let value = input.to_value().expect("変換できる");
        assert_eq!(
            value,
            ItemValue::Track(TrackValue {
                values: vec![FiniteF64::try_new(50.0).expect("有限値")],
                mode: None,
                params: Vec::new(),
                accelerate: false,
                decelerate: false,
                twopoint: false,
                reserved_flags: 0,
            })
        );
        assert_eq!(aviutl2_mcp_core::validate_item_value(&value), Ok(()));
    }

    #[test]
    fn a_movement_that_spells_a_flag_the_host_cannot_evaluate_is_refused() {
        // 予約ビットを入力へ置いた以上、要求元は評価の死ぬ値を綴れる。黙って
        // 落とさず拒否する——無言で値を変えるのは、この予約ビットが在る理由その
        // ものである。
        let input: ItemValueInput = serde_json::from_value(json!({
            "type": "track",
            "values": [-600.0, 600.0],
            "mode": "直線移動",
            "params": [],
            "accelerate": false,
            "decelerate": false,
            "twopoint": false,
            "reserved_flags": 8,
        }))
        .expect("受理される");
        let value = input.to_value().expect("変換できる");
        let error = aviutl2_mcp_core::validate_item_value(&value).expect_err("拒否されます");
        assert_eq!(error.reason(), Some("track_flags_not_representable"));
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    }

    #[test]
    fn the_flags_without_a_name_survive_the_boundary() {
        // 読み取りが返した値をそのまま送り返す往復である。入力 schema が予約
        // ビットを持たないと、境界で 0 へ落ちて符号化から消える。
        let read = ItemValue::Track(TrackValue {
            values: [-600.0, 600.0]
                .into_iter()
                .map(|value| FiniteF64::try_new(value).expect("有限値"))
                .collect(),
            mode: Some("直線移動".to_string()),
            params: Vec::new(),
            accelerate: false,
            decelerate: false,
            twopoint: false,
            reserved_flags: 16,
        });
        let input: ItemValueInput =
            serde_json::from_value(serde_json::to_value(&read).expect("直列化できる"))
                .expect("入力型で受け取れる");
        let converted = input.to_value().expect("変換できる");
        assert_eq!(converted, read);

        let ItemValue::Track(track) = converted else {
            panic!("移動として受け取れません");
        };
        let movements = vec![aviutl2_mcp_core::Movement {
            name: "直線移動".to_string(),
            writable: true,
        }];
        assert_eq!(
            aviutl2_mcp_core::encode_track_value(
                &track,
                aviutl2_mcp_core::TrackWriteTarget {
                    section_count: 1,
                    movements: &movements,
                },
            ),
            Ok("-600,600,直線移動,16".to_string())
        );
    }

    #[test]
    fn a_movement_rejects_values_that_are_not_finite() {
        // JSON は非有限数を字句として持たないため、この経路は復号では踏めない。
        // それでも判定を置くのは、応答へ載せる型が有限値しか持てないからである。
        let track = |values: Vec<f64>, params: Vec<f64>| ItemValueInput::Track {
            values,
            mode: Some("直線移動".to_string()),
            params,
            accelerate: false,
            decelerate: false,
            twopoint: false,
            reserved_flags: 0,
        };
        for input in [
            track(vec![0.0, f64::INFINITY], Vec::new()),
            track(vec![0.0, 100.0], vec![f64::NAN]),
        ] {
            assert_eq!(
                input.to_value().map_err(|error| error.code),
                Err(ErrorCode::InvalidArgument)
            );
        }
    }

    #[test]
    fn item_value_input_accepts_unknown_fields() {
        // 設定値も selector と同じ往復型である。応答へ optional field が増えた
        // とき、読み取りが返した値をそのまま書き戻せなくなる非対称を作らない。
        let input: SetObjectItemInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": effect_selector_json(),
            "item": "種類",
            "value": { "type": "choice", "value": "通常", "future": 1 },
        }))
        .expect("未知フィールドを含む設定値を受理する");

        let params = input.to_params().expect("params へ変換できる");
        assert_eq!(
            params.value,
            ItemValue::Choice {
                value: "通常".to_string(),
            },
            "既知フィールドが失われています"
        );
    }

    #[test]
    fn item_value_input_still_rejects_missing_fields_and_type_mismatch() {
        // 寛容にするのは余分な field だけである。
        for value in [
            json!({ "type": "integer" }),
            json!({ "type": "integer", "value": "1" }),
            json!({ "type": "vector", "x": 1 }),
        ] {
            assert!(
                serde_json::from_value::<ItemValueInput>(value.clone()).is_err(),
                "{value} が受理されました"
            );
        }
    }

    #[test]
    fn set_effect_enabled_requires_the_enabled_field() {
        assert!(
            serde_json::from_value::<SetEffectEnabledInput>(json!({
                "instance_id": SAMPLE_ID,
                "selector": effect_selector_json(),
            }))
            .is_err(),
            "enabled の欠落が受理されました"
        );
    }

    #[test]
    fn layer_state_requires_at_least_one_change() {
        let input: SetLayerStateInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "layer": 1,
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("全省略は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn layer_state_reset_rejects_a_name() {
        // 構造体 variant であるため、未知フィールドの拒否が判別子つきの分岐でも
        // 効く。unit variant では黙って読み飛ばされる。
        assert!(
            serde_json::from_value::<SetLayerStateInput>(json!({
                "instance_id": SAMPLE_ID,
                "expected_scene_id": 3,
                "layer": 1,
                "name": { "type": "reset", "name": "x" },
                "expected_project_epoch": SAMPLE_EPOCH,
            }))
            .is_err(),
            "標準名へ戻す指定が名前を読み飛ばしました"
        );

        let input: SetLayerStateInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "layer": 1,
            "name": { "type": "reset" },
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("標準名へ戻す指定は受理される");
        assert_eq!(
            input.to_params().expect("params へ変換できる").name,
            Some(aviutl2_mcp_core::LayerNameChange::Reset {})
        );
    }

    #[test]
    fn an_empty_layer_name_is_rejected_instead_of_meaning_reset() {
        // ホストは空を標準名へ戻す指定として扱う。受け付ければ、reset を要求して
        // いない呼び出しに対して標準名へ戻す変更を行い、成功として返すことになる。
        let input: SetLayerStateInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "layer": 1,
            "name": { "type": "set", "name": "" },
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("空の名前は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn layer_state_rejects_a_layer_beyond_the_declared_maximum() {
        let input: SetLayerStateInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "layer": MAX_POSITION + 1,
            "locked": false,
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("範囲外は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn layer_name_over_the_limit_is_rejected() {
        let input: SetLayerStateInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "layer": 1,
            "name": { "type": "set", "name": "a".repeat(MAX_NAME_CHARS as usize + 1) },
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("上限超過は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    /// 3 つの軸を全て省略したシーン設定の要求。
    fn scene_settings_json() -> Value {
        json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "expected_project_epoch": SAMPLE_EPOCH,
        })
    }

    #[test]
    fn scene_settings_requires_at_least_one_change() {
        let input: SetSceneSettingsInput =
            serde_json::from_value(scene_settings_json()).expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("全省略は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn an_empty_scene_name_is_rejected_instead_of_being_ignored() {
        // ホストは空の名前を「変更しない」として無視する。受け付ければ、何も
        // 起きなかった要求を成功として返すことになる。
        let mut value = scene_settings_json();
        value["name"] = json!("");
        let input: SetSceneSettingsInput =
            serde_json::from_value(value).expect("入力型としては受理される");
        let error = input.to_params().expect_err("空の名前は拒否される");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.details["reason"], json!("empty"));
    }

    #[test]
    fn a_scene_size_needs_both_axes() {
        // 片方だけを変える手段がホストに無いため、組でしか綴れない形にする。
        //
        // 落ちた理由まで見る。`is_err` だけを見ると、`size` という入れ子その
        // ものが消えて未知フィールドとして落ちる形でも通ってしまう。
        for (size, missing) in [
            (json!({ "width": 1920 }), "height"),
            (json!({ "height": 1080 }), "width"),
        ] {
            let mut value = scene_settings_json();
            value["size"] = size.clone();
            let error = serde_json::from_value::<SetSceneSettingsInput>(value)
                .expect_err(&format!("{size} が受理されました"));
            assert!(
                error.to_string().contains(missing),
                "{size} が {missing} の欠落として落ちていません: {error}"
            );
        }
    }

    #[test]
    fn scene_values_outside_the_declared_range_are_rejected() {
        let cases = [
            json!({ "size": { "width": 0, "height": 1080 } }),
            json!({ "size": { "width": 1920, "height": MAX_POSITION + 1 } }),
            // 画素数の上限は描画の側と共有する。
            json!({ "size": { "width": 30_000, "height": 30_000 } }),
            json!({ "sample_rate": 0 }),
        ];
        for case in cases {
            let mut value = scene_settings_json();
            for (key, field) in case.as_object().expect("case は object") {
                value[key] = field.clone();
            }
            let input: SetSceneSettingsInput =
                serde_json::from_value(value).expect("入力型としては受理される");
            assert_eq!(
                input.to_params().expect_err("範囲外は拒否される").code,
                ErrorCode::InvalidArgument,
                "{case} が受理されました"
            );
        }
    }

    #[test]
    fn selection_requires_at_least_one_change() {
        let input: SetSelectionInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("全省略は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn selection_focus_selector_is_converted() {
        let input: SetSelectionInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "focus": { "type": "set", "object": object_selector_json() },
            "selected_range": { "type": "clear" },
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("入力型としては受理される");
        let params = input.to_params().expect("params へ変換できる");
        assert!(matches!(
            params.focus,
            Some(aviutl2_mcp_core::FocusChange::Set { .. })
        ));
        assert_eq!(params.selected_range, Some(RangeChange::Clear {}));
    }

    #[test]
    fn item_and_effect_names_over_the_limit_are_rejected() {
        let too_long = "a".repeat(MAX_NAME_CHARS as usize + 1);

        let item: SetObjectItemInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": effect_selector_json(),
            "item": too_long,
            "value": { "type": "integer", "value": 1 },
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            item.to_params().expect_err("上限超過は拒否される").code,
            ErrorCode::InvalidArgument
        );

        let effect: AddEffectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "object": object_selector_json(),
            "effect_name": too_long,
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            effect.to_params().expect_err("上限超過は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn object_name_over_the_limit_is_rejected() {
        let input: SetObjectNameInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "selector": object_selector_json(),
            "name": "a".repeat(MAX_NAME_CHARS as usize + 1),
        }))
        .expect("入力型としては受理される");
        assert_eq!(
            input.to_params().expect_err("上限超過は拒否される").code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn validation_errors_do_not_echo_the_input() {
        // 応答へ入力そのものを写すと、誤りを伝える応答自体が過大になる。
        let input: CreateObjectInput = serde_json::from_value(json!({
            "instance_id": SAMPLE_ID,
            "source": { "type": "media_file", "path": r"C:\secret\movie.mp4:stream" },
            "placement": { "scene_id": 3, "layer": 1, "frame": 0 },
            "expected_project_epoch": SAMPLE_EPOCH,
        }))
        .expect("入力型としては受理される");
        let error = input.to_params().expect_err("拒否される");
        assert!(!error.message.contains("secret"), "{}", error.message);
    }
}
