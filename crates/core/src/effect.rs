//! effect の読み取り DTO と種別列挙。

use crate::fingerprint::{EffectFingerprintInput, effect_fingerprint};
use crate::item_value::ItemValue;
use crate::kind::{kind_name, serialize_kind, visit_unknown_kind};
use crate::number::FiniteF64;
use crate::selector::{EffectSelector, ObjectSelector};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// オブジェクトに付与された effect。
///
/// **fingerprint はセレクターの中だけに持つ。** トップレベルへ併記すると、
/// 構造体リテラルからも逆直列化からも食い違う組を作れてしまう。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectInfo {
    /// effect 名。
    pub name: String,
    /// 同名 effect のうち何番目か。0 始まり。
    pub index: usize,
    /// effect が有効か。
    pub enabled: bool,
    /// effect がロックされているか。
    ///
    /// ロックは入力項目と出力項目をまとめた単位で掛かるが、SDK は出力項目
    /// （標準描画等）について常に偽を返す。**出力項目についてはこの値は実態を
    /// 反映しない。** 誤り方は一貫しているため fingerprint の決定性は保たれる。
    pub locked: bool,
    /// 設定項目と値。
    pub items: Vec<EffectItem>,
    /// 再指定用のセレクター。同一性検証用の fingerprint はこの中にある。
    pub selector: EffectSelector,
}

impl EffectInfo {
    /// effect 情報とセレクターを組み立てる。
    pub fn new(object: ObjectSelector, input: EffectFingerprintInput<'_>) -> Self {
        Self {
            name: input.effect_name.to_string(),
            index: input.effect_index,
            enabled: input.enabled,
            locked: input.locked,
            items: input.items.to_vec(),
            selector: EffectSelector {
                object,
                effect_name: input.effect_name.to_string(),
                effect_index: input.effect_index,
                fingerprint: effect_fingerprint(input),
            },
        }
    }
}

/// effect の設定項目 1 件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectItem {
    /// 設定項目名。
    pub name: String,
    /// 設定項目の種別。
    pub item_type: EffectItemType,
    /// 現在値。
    pub value: ItemValue,
    /// トラックバー項目のみが持つ移動情報。
    pub track: Option<TrackInfo>,
}

/// トラックバー項目の移動情報。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackInfo {
    /// 移動方法の名前。
    pub mode: String,
    /// 移動方法のパラメータ。
    pub params: Vec<FiniteF64>,
    /// 加速が有効か。
    pub accelerate: bool,
    /// 減速が有効か。
    pub decelerate: bool,
    /// 中間点を無視するか。
    pub twopoint: bool,
    /// 時間制御が有効か。
    pub timecontrol: bool,
    /// 所属グループの要素数。
    pub group_num: usize,
    /// 所属グループ内での 0 始まりの位置。
    pub group_index: usize,
    /// グループ名。無名は null。
    pub group_name: Option<String>,
}

/// 利用可能な effect 種別のメタ情報。
///
/// **設定項目の一覧は持たない。** この型が答えるのは「どの effect が在るか」で
/// あり、項目名の列挙はその判断に寄与しない一方で応答量の大半を占める。既存
/// オブジェクトの項目名は `get_object` が現在値付きで返すため、編集の経路は
/// この型を経由しない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvailableEffect {
    /// effect 名。
    pub name: String,
    /// effect の種別。
    pub effect_type: EffectType,
    /// 対応内容を表すフラグ。
    pub flags: EffectFlags,
    /// 設定項目の数。
    pub item_count: usize,
    /// 効果の説明。ホストが説明を持たない effect は null。
    ///
    /// 文言はホストが同梱するものをそのまま運ぶ。**説明が無いことを推測で
    /// 埋めない。** 検証できない説明は、無い場合より悪い——受け取った側はそれを
    /// 信じて使う。
    pub description: Option<String>,
}

/// 利用可能な effect の設定項目定義。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvailableEffectItem {
    /// 設定項目名。
    pub name: String,
    /// 設定項目の種別。
    pub item_type: EffectItemType,
}

/// effect 1 件の中身。
///
/// **設定項目を持つのがこの型と [`EffectInfo`] の違いである。** [`EffectInfo`] は
/// 特定のオブジェクトに付与された effect の現在値を運ぶ。こちらはオブジェクトを
/// 持たない effect 種別そのものの顔ぶれであり、値を 1 つも含まない。
///
/// 名前の似た effect の使い分けは、散文ではなく設定項目の顔ぶれで解ける。
/// 項目の一覧はホストの列挙から実時に得られるため、供給源の記述と食い違わない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectDescription {
    /// effect 名。
    pub name: String,
    /// 効果の説明。ホストが説明を持たない effect は null。
    ///
    /// 文言はホストが同梱するものをそのまま運ぶ。**説明が無いことを推測で
    /// 埋めない。** 検証できない説明は、無い場合より悪い——受け取った側はそれを
    /// 信じて使う。
    pub description: Option<String>,
    /// 設定項目を、ホストが列挙した順に並べたもの。
    pub items: Vec<EffectItemDescription>,
}

/// effect の設定項目 1 件の定義と説明。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectItemDescription {
    /// 設定項目名。
    pub name: String,
    /// 設定項目の種別。
    pub item_type: EffectItemType,
    /// 設定項目の説明。ホストが説明を持たない項目は null。
    ///
    /// 効果の説明と同じ供給源から来るが、別のキーである。**効果の説明をここへ
    /// 写さない**——項目の説明として効果の説明が出れば、受け取った側は誤った
    /// 文言を確信を持って使う。
    pub description: Option<String>,
    /// 選択肢の候補。表に項目が無ければ null。
    ///
    /// 候補が null であることは「値を選べない項目である」ことを意味しない。
    /// 表に無いだけであり、書き込みは従来どおり通る。
    ///
    /// **種別では絞らない。** 引くのは項目名だけであり、`integer` の項目でも表に
    /// 載っていれば候補が付く。種別で絞ると、表が書いた記述を我々の判断で黙って
    /// 落とすことになる。落とした側に「候補が無い」と「候補を出さないことにした」
    /// の区別は届かない。
    pub choices: Option<ItemChoices>,
    /// 値域と小数桁。表に項目が無ければ null。
    ///
    /// 値域が null であることは「値の範囲が無い項目である」ことを意味しない。
    /// 表に無いだけであり、書き込みは従来どおり通る。**`range` が null であること
    /// と `range.max` が null であることは別の事実である**——前者は表がその項目の
    /// 値域を述べていないことを、後者は上限を測れなかったことを言う。
    ///
    /// **種別では絞らない。** [`Self::choices`] と揃える——表に載っていれば `text`
    /// の項目でも値域が付く。種別で絞ると、表が書いた記述を我々の判断で黙って
    /// 落とすことになる。
    pub range: Option<ItemRange>,
}

/// 設定項目が取り得る値の候補。
///
/// **ヒントであってゲートではない。** ここに無い値でも書き込みは通り、ここに
/// ある値が必ず通るとも限らない。可否を決めるのはホストであり、書き込みの経路は
/// 書いた値を読み直して照合する。版ずれやプラグインの追加で表が実態から外れた
/// とき、事前検証を掛けていれば「正しい値なのに通らない」へ退化する。候補を
/// 知らないまま総当たりになる状態より悪い。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemChoices {
    /// 候補の値。表に書かれた順で並ぶ。
    pub values: Vec<String>,
    /// 候補の由来。
    pub source: TableSource,
}

/// 設定項目が取り得る値の範囲と小数桁。
///
/// **測れた側だけを持つ。** 供給源は候補と同じ表であり、値は極端な値を書いて
/// ホストが倒した先を読むことで起こす。探りの値が範囲の内側へ収まってしまった
/// 項目については、その側を記録しない。**表にこの項目が無いこと（`range` 自体が
/// null）と、上限を測れなかったこと（`max` だけが null）は別の事実である。**
///
/// **候補と同じくヒントであってゲートではない。** ここが述べる範囲を外れる値でも
/// 書き込みは通す。**値域は候補より外れやすい**——候補の陳腐化は「足りなくなる」
/// だが、値域の陳腐化は「狭くなる」であり、版が上がって上限が広がれば、表は
/// 正しい値を範囲外だと言う。事前検証を掛けていれば、そこで通るはずの値を
/// こちら側が拒む。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ItemRange {
    /// 下限。測れていなければ null。
    pub min: Option<FiniteF64>,
    /// 上限。測れていなければ null。
    pub max: Option<FiniteF64>,
    /// 小数点以下の桁数。測れていなければ null。
    pub decimals: Option<u32>,
    /// 値域の由来。
    pub source: TableSource,
}

/// 設定項目 1 件について表が述べたことの組。
///
/// **面ごとに独立して欠ける。** 候補だけを持つ項目も、値域だけを持つ項目も、
/// どちらも持たない項目もある。持たないことは「その項目に候補や値域が無い」
/// ことではなく、表がそれを述べていないことだけを意味する。
///
/// **応答には現れない。** 表を引く経路が (効果, 項目) から 1 件を取り出すために
/// 使う組であり、[`EffectItemDescription`] は面をそのままフィールドへ展開する。
/// 直列化を持たないのはそのためである。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemFacets {
    /// 選択肢の候補。表が述べていなければ `None`。
    pub choices: Option<ItemChoices>,
    /// 値域と小数桁。表が述べていなければ `None`。
    pub range: Option<ItemRange>,
}

/// 表が述べたことの由来。
///
/// **候補にも値域にも同じ由来が付く。** 面ごとに別の enum を持つ理由が無い——
/// 「実行ファイルへ埋め込んだ表から来た」「走査で見つけたファイルから来た」の
/// 2 値は、面が何であるかに依らず同じ意味を持つ。面の名前を型名へ入れると、
/// 3 つ目の面を足すたびに同じ 2 値の enum が 1 つ増える。
///
/// **由来そのもので決まる。** ファイル名や中身から見分けるものではない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableSource {
    /// 実行ファイルへ埋め込まれた基底の表。
    BuiltinTable,
    /// 走査で見つけたサイドカーファイル。
    ///
    /// **書き手は特定できない。** プラグインの作者が配布物へ同梱したのか、
    /// 利用者が自分で置いたのかを区別する手段が我々には無い。
    Sidecar,
}

/// effect が対応する内容を表すフラグ。
///
/// 既知ビットを bool で展開する。**ビット列そのものは載せない**——生成元が
/// 復元できるのは既知ビットだけであり、未知ビットを運ぶ手段が無いためである。
///
/// **製品コードは 4 つとも読まない。** 読むのは `list_available_effects` の応答を
/// 受け取った要求元であり、どの effect を使うかを選ぶ材料そのものである。
/// 4 つとも実態と食い違わないため、読み手が製品に無いことは落とす理由にならない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectFlags {
    /// 画像をサポートする。
    pub video: bool,
    /// 音声をサポートする。
    pub audio: bool,
    /// フィルタオブジェクトをサポートする。
    pub filter: bool,
    /// カメラ効果をサポートする。
    pub camera: bool,
}

/// 画像対応ビット。
const EFFECT_FLAG_VIDEO: u32 = 1;
/// 音声対応ビット。
const EFFECT_FLAG_AUDIO: u32 = 2;
/// フィルタオブジェクト対応ビット。
const EFFECT_FLAG_FILTER: u32 = 4;
/// カメラ効果対応ビット。
const EFFECT_FLAG_CAMERA: u32 = 8;

impl EffectFlags {
    /// 生のビット列から既知フラグを展開する。未知ビットは落ちる。
    pub fn from_raw(raw: u32) -> Self {
        Self {
            video: raw & EFFECT_FLAG_VIDEO != 0,
            audio: raw & EFFECT_FLAG_AUDIO != 0,
            filter: raw & EFFECT_FLAG_FILTER != 0,
            camera: raw & EFFECT_FLAG_CAMERA != 0,
        }
    }
}

/// effect の種別。
///
/// 既知の種別は snake_case 文字列、未知の種別は
/// `{"type":"unknown","raw":<i32>}` として表現する。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EffectType {
    Filter,
    Input,
    Transition,
    Control,
    Output,
    /// 未知の種別値を破棄せず raw 保持。
    Unknown(i32),
}

impl EffectType {
    /// 種別値を返す。
    pub fn as_raw(&self) -> i32 {
        match self {
            EffectType::Filter => 1,
            EffectType::Input => 2,
            EffectType::Transition => 3,
            EffectType::Control => 4,
            EffectType::Output => 5,
            EffectType::Unknown(raw) => *raw,
        }
    }

    /// 種別値から復元する。既知でない値は [`EffectType::Unknown`] とする。
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            1 => EffectType::Filter,
            2 => EffectType::Input,
            3 => EffectType::Transition,
            4 => EffectType::Control,
            5 => EffectType::Output,
            other => EffectType::Unknown(other),
        }
    }

    fn name(&self) -> Option<&'static str> {
        match self {
            EffectType::Filter => Some("filter"),
            EffectType::Input => Some("input"),
            EffectType::Transition => Some("transition"),
            EffectType::Control => Some("control"),
            EffectType::Output => Some("output"),
            EffectType::Unknown(_) => None,
        }
    }

    /// 種別を一意に表す名前を返す。
    ///
    /// 表現は [`fmt::Display`] と同じで、既知の種別は snake_case 名、未知の種別は
    /// raw 値を含む別形式になる。raw 値そのものではなく名前で識別するため、
    /// 既知の種別と同じ raw を持つ [`EffectType::Unknown`] が既知の種別と
    /// 同じ表現になることはない。
    pub fn kind_name(&self) -> String {
        kind_name(self.name(), self.as_raw())
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "filter" => Some(EffectType::Filter),
            "input" => Some(EffectType::Input),
            "transition" => Some(EffectType::Transition),
            "control" => Some(EffectType::Control),
            "output" => Some(EffectType::Output),
            _ => None,
        }
    }
}

/// 任意フレームでの値を評価できる設定項目の種別。
///
/// 種別ごとにフレームの取り方も値の型も違う。トラックバーは小数部でフレーム間の
/// 位置を指せて値は数値であり、チェックボックスは整数フレームだけを取って値は
/// 真偽である。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluatedItemKind {
    /// トラックバー。
    Track,
    /// チェックボックス。
    Check,
}

/// effect 設定項目の種別。
///
/// 既知の種別は snake_case 文字列、未知の種別は
/// `{"type":"unknown","raw":<i32>}` として表現する。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EffectItemType {
    Integer,
    Number,
    Check,
    Text,
    String,
    File,
    Color,
    /// リストからの選択。
    Select,
    Scene,
    /// レイヤー範囲。
    Range,
    /// リストと文字の複合。
    Combo,
    Mask,
    Font,
    /// 図形。
    Figure,
    Data,
    Folder,
    /// 未知の種別値を破棄せず raw 保持。
    Unknown(i32),
}

impl EffectItemType {
    /// 既知の全 variant。[`EffectItemType::Unknown`] は含まない。
    ///
    /// 要素数と種別値の連番は `effect_item_type_all_matches_sdk_order` テストで
    /// 固定する。種別値を辿って一覧を導くと、SDK が種別を連番でない値で足した
    /// 日に一覧が黙って短くなるため、列挙として書く。
    pub const ALL: &'static [EffectItemType] = &[
        EffectItemType::Integer,
        EffectItemType::Number,
        EffectItemType::Check,
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
    ];

    /// 種別値を返す。
    pub fn as_raw(&self) -> i32 {
        match self {
            EffectItemType::Integer => 1,
            EffectItemType::Number => 2,
            EffectItemType::Check => 3,
            EffectItemType::Text => 4,
            EffectItemType::String => 5,
            EffectItemType::File => 6,
            EffectItemType::Color => 7,
            EffectItemType::Select => 8,
            EffectItemType::Scene => 9,
            EffectItemType::Range => 10,
            EffectItemType::Combo => 11,
            EffectItemType::Mask => 12,
            EffectItemType::Font => 13,
            EffectItemType::Figure => 14,
            EffectItemType::Data => 15,
            EffectItemType::Folder => 16,
            EffectItemType::Unknown(raw) => *raw,
        }
    }

    /// 種別値から復元する。既知でない値は [`EffectItemType::Unknown`] とする。
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            1 => EffectItemType::Integer,
            2 => EffectItemType::Number,
            3 => EffectItemType::Check,
            4 => EffectItemType::Text,
            5 => EffectItemType::String,
            6 => EffectItemType::File,
            7 => EffectItemType::Color,
            8 => EffectItemType::Select,
            9 => EffectItemType::Scene,
            10 => EffectItemType::Range,
            11 => EffectItemType::Combo,
            12 => EffectItemType::Mask,
            13 => EffectItemType::Font,
            14 => EffectItemType::Figure,
            15 => EffectItemType::Data,
            16 => EffectItemType::Folder,
            other => EffectItemType::Unknown(other),
        }
    }

    fn name(&self) -> Option<&'static str> {
        match self {
            EffectItemType::Integer => Some("integer"),
            EffectItemType::Number => Some("number"),
            EffectItemType::Check => Some("check"),
            EffectItemType::Text => Some("text"),
            EffectItemType::String => Some("string"),
            EffectItemType::File => Some("file"),
            EffectItemType::Color => Some("color"),
            EffectItemType::Select => Some("select"),
            EffectItemType::Scene => Some("scene"),
            EffectItemType::Range => Some("range"),
            EffectItemType::Combo => Some("combo"),
            EffectItemType::Mask => Some("mask"),
            EffectItemType::Font => Some("font"),
            EffectItemType::Figure => Some("figure"),
            EffectItemType::Data => Some("data"),
            EffectItemType::Folder => Some("folder"),
            EffectItemType::Unknown(_) => None,
        }
    }

    /// 種別を一意に表す名前を返す。
    ///
    /// 表現は [`fmt::Display`] と同じで、既知の種別は snake_case 名、未知の種別は
    /// raw 値を含む別形式になる。raw 値そのものではなく名前で識別するため、
    /// 既知の種別と同じ raw を持つ [`EffectItemType::Unknown`] が既知の種別と
    /// 同じ表現になることはない。
    pub fn kind_name(&self) -> String {
        kind_name(self.name(), self.as_raw())
    }

    /// 任意フレームでの値を評価できる種別であれば、その評価の種別を返す。
    ///
    /// **`_` を使わない網羅 `match` である。** 種別を足したときに、評価できるか
    /// を決めないまま既定へ落ちることがない。
    pub fn evaluated_kind(&self) -> Option<EvaluatedItemKind> {
        match self {
            EffectItemType::Integer | EffectItemType::Number => Some(EvaluatedItemKind::Track),
            EffectItemType::Check => Some(EvaluatedItemKind::Check),
            EffectItemType::Text
            | EffectItemType::String
            | EffectItemType::File
            | EffectItemType::Color
            | EffectItemType::Select
            | EffectItemType::Scene
            | EffectItemType::Range
            | EffectItemType::Combo
            | EffectItemType::Mask
            | EffectItemType::Font
            | EffectItemType::Figure
            | EffectItemType::Data
            | EffectItemType::Folder
            | EffectItemType::Unknown(_) => None,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "integer" => Some(EffectItemType::Integer),
            "number" => Some(EffectItemType::Number),
            "check" => Some(EffectItemType::Check),
            "text" => Some(EffectItemType::Text),
            "string" => Some(EffectItemType::String),
            "file" => Some(EffectItemType::File),
            "color" => Some(EffectItemType::Color),
            "select" => Some(EffectItemType::Select),
            "scene" => Some(EffectItemType::Scene),
            "range" => Some(EffectItemType::Range),
            "combo" => Some(EffectItemType::Combo),
            "mask" => Some(EffectItemType::Mask),
            "font" => Some(EffectItemType::Font),
            "figure" => Some(EffectItemType::Figure),
            "data" => Some(EffectItemType::Data),
            "folder" => Some(EffectItemType::Folder),
            _ => None,
        }
    }
}

impl Serialize for EffectType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_kind(self.name(), self.as_raw(), serializer)
    }
}

impl<'de> Deserialize<'de> for EffectType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EffectTypeVisitor;

        impl<'de> Visitor<'de> for EffectTypeVisitor {
            type Value = EffectType;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("effect 種別の名前、または未知種別のオブジェクト")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                EffectType::from_name(value)
                    .ok_or_else(|| E::custom(format!("未知の effect 種別名です: {value}")))
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                visit_unknown_kind(map).map(EffectType::from_raw)
            }
        }

        deserializer.deserialize_any(EffectTypeVisitor)
    }
}

impl fmt::Display for EffectType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.kind_name())
    }
}

impl Serialize for EffectItemType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_kind(self.name(), self.as_raw(), serializer)
    }
}

impl<'de> Deserialize<'de> for EffectItemType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EffectItemTypeVisitor;

        impl<'de> Visitor<'de> for EffectItemTypeVisitor {
            type Value = EffectItemType;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("effect 設定項目種別の名前、または未知種別のオブジェクト")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                EffectItemType::from_name(value)
                    .ok_or_else(|| E::custom(format!("未知の effect 設定項目種別名です: {value}")))
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                visit_unknown_kind(map).map(EffectItemType::from_raw)
            }
        }

        deserializer.deserialize_any(EffectItemTypeVisitor)
    }
}

impl fmt::Display for EffectItemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.kind_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::ObjectSummary;

    fn known_effect_types() -> Vec<EffectType> {
        vec![
            EffectType::Filter,
            EffectType::Input,
            EffectType::Transition,
            EffectType::Control,
            EffectType::Output,
        ]
    }

    fn sample_object_selector() -> ObjectSelector {
        ObjectSummary::new(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            crate::fingerprint::ObjectFingerprintInput {
                scene_id: 0,
                layer: 2,
                frame_start: 120,
                frame_end: 240,
                name: Some("立ち絵"),
                alias: "alias",
            },
        )
        .selector
    }

    fn sample_track_info() -> TrackInfo {
        TrackInfo {
            mode: "直線移動".to_string(),
            params: vec![FiniteF64::try_new(0.5).unwrap()],
            accelerate: true,
            decelerate: false,
            twopoint: false,
            timecontrol: false,
            group_num: 2,
            group_index: 0,
            group_name: Some("座標".to_string()),
        }
    }

    fn sample_effect_items() -> Vec<EffectItem> {
        vec![
            EffectItem {
                name: "X".to_string(),
                item_type: EffectItemType::Number,
                value: ItemValue::Number {
                    value: FiniteF64::try_new(12.5).unwrap(),
                },
                track: Some(sample_track_info()),
            },
            EffectItem {
                name: "ファイル".to_string(),
                item_type: EffectItemType::File,
                value: ItemValue::File {
                    path: r"C:\movie.mp4".to_string(),
                },
                track: None,
            },
        ]
    }

    fn sample_effect_info() -> EffectInfo {
        let items = sample_effect_items();
        EffectInfo::new(
            sample_object_selector(),
            EffectFingerprintInput {
                effect_name: "動画ファイル",
                effect_index: 0,
                position: 0,
                effect_count: 1,
                enabled: true,
                locked: false,
                items: &items,
            },
        )
    }

    fn sample_available_effect() -> AvailableEffect {
        AvailableEffect {
            name: "ぼかし".to_string(),
            effect_type: EffectType::Filter,
            flags: EffectFlags::from_raw(EFFECT_FLAG_VIDEO | EFFECT_FLAG_CAMERA),
            item_count: 1,
            description: Some("ぼかします".to_string()),
        }
    }

    #[test]
    fn effect_type_roundtrip() {
        for effect_type in known_effect_types() {
            let s = serde_json::to_string(&effect_type).unwrap();
            assert_eq!(s, format!("\"{effect_type}\""));
            let restored: EffectType = serde_json::from_str(&s).unwrap();
            assert_eq!(restored, effect_type);
        }
    }

    #[test]
    fn effect_type_unknown_preserved() {
        let effect_type = EffectType::Unknown(99);
        let s = serde_json::to_string(&effect_type).unwrap();
        assert_eq!(s, r#"{"type":"unknown","raw":99}"#);
        let restored: EffectType = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, EffectType::Unknown(99));
        assert_eq!(restored.as_raw(), 99);
    }

    #[test]
    fn effect_type_raw_roundtrip() {
        for effect_type in known_effect_types() {
            assert_eq!(EffectType::from_raw(effect_type.as_raw()), effect_type);
        }
        assert_eq!(EffectType::from_raw(-1), EffectType::Unknown(-1));
    }

    #[test]
    fn effect_type_rejects_unknown_name() {
        let result: Result<EffectType, _> = serde_json::from_str("\"future\"");
        assert!(result.is_err());
    }

    #[test]
    fn effect_item_type_roundtrip() {
        for item_type in EffectItemType::ALL {
            let s = serde_json::to_string(item_type).unwrap();
            assert_eq!(s, format!("\"{item_type}\""));
            let restored: EffectItemType = serde_json::from_str(&s).unwrap();
            assert_eq!(&restored, item_type);
        }
    }

    #[test]
    fn effect_item_type_all_matches_sdk_order() {
        // 既知種別は 1..=16 に連番で割り当てられる。`ALL` の並びが種別値の順で
        // あり、かつ 1 つも欠けていないことを、網羅 `match` を持つ `as_raw` と
        // 突き合わせて確かめる。variant を足して `ALL` へ足し忘れると、要素数か
        // 連番のどちらかが崩れる。
        assert_eq!(EffectItemType::ALL.len(), 16);
        let raws: Vec<i32> = EffectItemType::ALL.iter().map(|t| t.as_raw()).collect();
        assert_eq!(raws, (1..=16).collect::<Vec<i32>>());
        // 種別値からの復元も `ALL` と一致し、連番の次の値は未知のままである。
        // 一覧へ足さずに variant を足すと、その種別値が未知でなくなって落ちる。
        for item_type in EffectItemType::ALL {
            assert_eq!(&EffectItemType::from_raw(item_type.as_raw()), item_type);
        }
        assert_eq!(EffectItemType::from_raw(17), EffectItemType::Unknown(17));
    }

    #[test]
    fn effect_item_type_unknown_preserved() {
        let s = r#"{"type":"unknown","raw":17}"#;
        let item_type: EffectItemType = serde_json::from_str(s).unwrap();
        assert_eq!(item_type, EffectItemType::Unknown(17));
        assert_eq!(serde_json::to_string(&item_type).unwrap(), s);
    }

    #[test]
    fn unknown_kind_object_normalizes_known_raw_value() {
        // 既知値を持つ未知種別オブジェクトは既知の variant へ寄せる。
        // 往復は値としては保たれるが JSON 表現は正準形へ変わる。
        let effect_type: EffectType =
            serde_json::from_str(r#"{"type":"unknown","raw":1}"#).unwrap();
        assert_eq!(effect_type, EffectType::Filter);
        assert_eq!(serde_json::to_string(&effect_type).unwrap(), "\"filter\"");

        let item_type: EffectItemType =
            serde_json::from_str(r#"{"type":"unknown","raw":2}"#).unwrap();
        assert_eq!(item_type, EffectItemType::Number);
        assert_eq!(serde_json::to_string(&item_type).unwrap(), "\"number\"");
    }

    #[test]
    fn unknown_kind_object_rejects_bad_shape() {
        for s in [
            r#"{"type":"filter","raw":1}"#,
            r#"{"type":"unknown"}"#,
            r#"{"raw":99}"#,
            r#"{"type":"unknown","raw":99,"extra":1}"#,
        ] {
            let result: Result<EffectType, _> = serde_json::from_str(s);
            assert!(result.is_err(), "{s} が受理された");
        }
    }

    #[test]
    fn kind_name_separates_unknown_from_the_known_type_of_the_same_raw() {
        // `Unknown` は public な variant であり、既知値を持つ値も構築できる。
        // raw 値で識別すると既知の種別と区別が付かなくなる。
        for effect_type in known_effect_types() {
            let unknown = EffectType::Unknown(effect_type.as_raw());
            assert_eq!(unknown.as_raw(), effect_type.as_raw());
            assert_ne!(unknown, effect_type);
            assert_ne!(
                unknown.kind_name(),
                effect_type.kind_name(),
                "{effect_type} と同じ名前になりました"
            );
        }

        for item_type in EffectItemType::ALL {
            let unknown = EffectItemType::Unknown(item_type.as_raw());
            assert_eq!(unknown.as_raw(), item_type.as_raw());
            assert_ne!(&unknown, item_type);
            assert_ne!(
                unknown.kind_name(),
                item_type.kind_name(),
                "{item_type} と同じ名前になりました"
            );
        }
    }

    #[test]
    fn kind_name_is_unique_across_known_types() {
        let mut names: Vec<String> = EffectItemType::ALL
            .iter()
            .map(EffectItemType::kind_name)
            .collect();
        let total = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), total, "既知種別の名前が重複しています");
    }

    #[test]
    fn kind_name_matches_display() {
        assert_eq!(EffectType::Filter.kind_name(), "filter");
        assert_eq!(EffectType::Unknown(1).kind_name(), "unknown(1)");
        assert_eq!(EffectItemType::Number.kind_name(), "number");
        assert_eq!(EffectItemType::Unknown(2).kind_name(), "unknown(2)");
        for item_type in EffectItemType::ALL {
            assert_eq!(item_type.kind_name(), item_type.to_string());
        }
    }

    #[test]
    fn effect_flags_expands_known_bits() {
        let flags = EffectFlags::from_raw(EFFECT_FLAG_AUDIO | EFFECT_FLAG_FILTER);
        assert!(!flags.video);
        assert!(flags.audio);
        assert!(flags.filter);
        assert!(!flags.camera);
    }

    #[test]
    fn effect_flags_drop_unknown_bits() {
        let flags = EffectFlags::from_raw(0x8000_0000);
        assert!(!flags.video && !flags.audio && !flags.filter && !flags.camera);
    }

    #[test]
    fn effect_info_roundtrip() {
        let info = sample_effect_info();
        let s = serde_json::to_string(&info).unwrap();
        let restored: EffectInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, info);
    }

    #[test]
    fn effect_info_carries_the_fingerprint_only_in_the_selector() {
        let value = serde_json::to_value(sample_effect_info()).unwrap();
        assert!(
            value.get("fingerprint").is_none(),
            "{value} がトップレベルへ fingerprint を併記しています"
        );
        assert!(value["selector"]["fingerprint"].is_string());
    }

    #[test]
    fn effect_info_does_not_report_a_fingerprint_algorithm() {
        // 方式は digest の材料であって運ぶ値ではない。セレクターが受け取らない
        // 値を応答へ載せても、要求元には送り返す先が無い。
        let value = serde_json::to_value(sample_effect_info()).unwrap();
        assert!(
            value.get("fingerprint_algorithm").is_none(),
            "{value} が算出方式を返しています"
        );
    }

    #[test]
    fn effect_info_new_copies_input_into_selector() {
        let info = sample_effect_info();
        assert_eq!(info.name, info.selector.effect_name);
        assert_eq!(info.index, info.selector.effect_index);
    }

    #[test]
    fn effect_info_allows_unknown_optional_fields() {
        let mut value = serde_json::to_value(sample_effect_info()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::json!(1));
        let restored: EffectInfo = serde_json::from_value(value).unwrap();
        assert_eq!(restored, sample_effect_info());
    }

    #[test]
    fn track_info_roundtrip() {
        let track = sample_track_info();
        let s = serde_json::to_string(&track).unwrap();
        let restored: TrackInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, track);
    }

    #[test]
    fn available_effect_roundtrip() {
        let effect = sample_available_effect();
        let s = serde_json::to_string(&effect).unwrap();
        let restored: AvailableEffect = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, effect);
    }

    #[test]
    fn available_effect_without_a_description_carries_null() {
        // 説明の無い effect は null で名乗る。埋めた説明と区別が付かなくなる
        // 空文字列や既定の文言では代えない。
        let effect = AvailableEffect {
            description: None,
            ..sample_available_effect()
        };
        let value = serde_json::to_value(&effect).unwrap();
        assert!(value["description"].is_null(), "{value}");
        let restored: AvailableEffect = serde_json::from_value(value).unwrap();
        assert_eq!(restored, effect);
    }

    #[test]
    fn available_effect_keeps_unknown_types() {
        let effect = AvailableEffect {
            effect_type: EffectType::Unknown(42),
            ..sample_available_effect()
        };
        let s = serde_json::to_string(&effect).unwrap();
        let restored: AvailableEffect = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, effect);
    }

    #[test]
    fn available_effect_reports_the_item_count_without_listing_the_items() {
        // 一覧が答えるのは「どの effect が在るか」だけである。項目名の配列を
        // 載せると、応答量の大半をそれが占めたまま判断の材料は増えない。
        let value = serde_json::to_value(sample_available_effect()).unwrap();
        assert_eq!(value["item_count"], 1);
        assert_eq!(value["description"], "ぼかします");
        for forbidden in ["items", "item_names"] {
            assert!(
                value.get(forbidden).is_none(),
                "{forbidden} が応答に現れました: {value}"
            );
        }
    }

    fn sample_effect_description() -> EffectDescription {
        EffectDescription {
            name: "図形".to_string(),
            description: Some(
                "単色の図形を作成します\nsvgファイルから読み込むことも出来ます".to_string(),
            ),
            items: vec![
                EffectItemDescription {
                    name: "図形の種類".to_string(),
                    item_type: EffectItemType::Figure,
                    description: Some(
                        "図形の種類を選択します\nボタンクリックでsvgファイルを選択出来ます"
                            .to_string(),
                    ),
                    choices: Some(ItemChoices {
                        values: vec!["円".to_string(), "四角形".to_string()],
                        source: TableSource::BuiltinTable,
                    }),
                    range: None,
                },
                EffectItemDescription {
                    name: "ライン幅".to_string(),
                    item_type: EffectItemType::Integer,
                    description: None,
                    choices: None,
                    range: Some(ItemRange {
                        min: FiniteF64::try_new(1.0),
                        max: FiniteF64::try_new(4000.0),
                        decimals: Some(0),
                        source: TableSource::BuiltinTable,
                    }),
                },
            ],
        }
    }

    #[test]
    fn effect_description_roundtrip() {
        let effect = sample_effect_description();
        let s = serde_json::to_string(&effect).unwrap();
        let restored: EffectDescription = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, effect);
    }

    #[test]
    fn an_effect_description_keeps_every_line_of_the_text() {
        // 発見の鍵が 2 行目に置かれている説明が実在する。効果の説明も項目の
        // 説明も、先頭行だけに切ると載せる意味そのものが失われる。
        let value = serde_json::to_value(sample_effect_description()).unwrap();
        assert!(
            value["description"].as_str().unwrap().contains("svg"),
            "{value}"
        );
        assert!(
            value["items"][0]["description"]
                .as_str()
                .unwrap()
                .contains("svg"),
            "{value}"
        );
    }

    #[test]
    fn an_item_without_a_description_carries_null() {
        // 説明の無い項目は null で名乗る。空文字列で代えると、説明が空である
        // ことと説明を持たないことの区別が付かない。
        let value = serde_json::to_value(sample_effect_description()).unwrap();
        assert!(value["items"][1]["description"].is_null(), "{value}");
    }

    #[test]
    fn an_item_carries_its_choices_with_the_source_they_came_from() {
        // 由来は候補と同じ組で運ぶ。値だけを返すと、受け取った側は表の誤りを
        // どこへ報告すればよいかを判断できない。
        let value = serde_json::to_value(sample_effect_description()).unwrap();
        assert_eq!(value["items"][0]["choices"]["values"][0], "円");
        assert_eq!(value["items"][0]["choices"]["source"], "builtin_table");
        // 表に無い項目は null である。空の配列で代えると、「候補が 1 つも無い
        // 項目」と「表に載っていない項目」の区別が付かない。
        assert!(value["items"][1]["choices"].is_null(), "{value}");
    }

    #[test]
    fn an_item_carries_its_range_with_the_source_it_came_from() {
        // 値域も候補と同じ形で運ぶ。由来を落とすと、受け取った側は表の誤りを
        // どこへ報告すればよいかを判断できない。
        let value = serde_json::to_value(sample_effect_description()).unwrap();
        assert_eq!(value["items"][1]["range"]["max"], 4000.0);
        assert_eq!(value["items"][1]["range"]["source"], "builtin_table");
        // 表に無い項目は null である。
        assert!(value["items"][0]["range"].is_null(), "{value}");
    }

    #[test]
    fn the_parts_of_a_range_are_null_one_by_one() {
        // **測れた側だけを載せる。** 探りの値が範囲の内側へ収まった項目では、
        // その側を記録できない。**値域そのものが null であることと、上限だけが
        // null であることは、要求元にとって別の情報である**——前者は表が値域を
        // 述べていないことを、後者は上限を測れなかったことを言う。
        let item = EffectItemDescription {
            name: "縦横比".to_string(),
            item_type: EffectItemType::Number,
            description: None,
            choices: None,
            range: Some(ItemRange {
                min: None,
                max: FiniteF64::try_new(100.0),
                decimals: None,
                source: TableSource::Sidecar,
            }),
        };
        // 測れなかった側は欄ごと消さずに null で名乗る。欄が消えると、要求元は
        // 「上限を測れなかった」と「上限という概念が無い」を見分けられない。
        let value = serde_json::to_value(&item).unwrap();
        let range = value["range"].as_object().expect("値域がある");
        assert_eq!(range.get("min"), Some(&serde_json::Value::Null), "{value}");
        assert_eq!(range["max"], 100.0);
        assert_eq!(
            range.get("decimals"),
            Some(&serde_json::Value::Null),
            "{value}"
        );

        let restored: EffectItemDescription = serde_json::from_value(value).unwrap();
        assert_eq!(restored, item);
    }

    #[test]
    fn the_table_source_names_where_the_values_came_from() {
        for (source, name) in [
            (TableSource::BuiltinTable, "\"builtin_table\""),
            (TableSource::Sidecar, "\"sidecar\""),
        ] {
            let s = serde_json::to_string(&source).unwrap();
            assert_eq!(s, name);
            let restored: TableSource = serde_json::from_str(&s).unwrap();
            assert_eq!(restored, source);
        }
    }
}
