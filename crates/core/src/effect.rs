//! effect の読み取り DTO と種別列挙。

use crate::fingerprint::{
    EffectFingerprintInput, Fingerprint, FingerprintAlgorithm, effect_fingerprint,
};
use crate::item_value::ItemValue;
use crate::number::FiniteF64;
use crate::selector::{EffectSelector, ObjectSelector};
use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// 未知種別を表すオブジェクトの判別子。
const UNKNOWN_TAG: &str = "unknown";

/// 未知種別オブジェクトが持つフィールド。
const UNKNOWN_FIELDS: &[&str] = &["type", "raw"];

/// オブジェクトに付与された effect。
///
/// トップレベルとセレクターの fingerprint は同一でなければならない。
/// [`EffectInfo::new`] を用いると 1 度の算出結果が両方へ設定される。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectInfo {
    /// effect 名。
    pub name: String,
    /// 同名 effect のうち何番目か。0 始まり。
    pub index: usize,
    /// effect が有効か。
    pub enabled: bool,
    /// effect がロックされているか。
    pub locked: bool,
    /// 設定項目と値。
    pub items: Vec<EffectItem>,
    /// 再指定用のセレクター。
    pub selector: EffectSelector,
    /// 同一性検証用の fingerprint。
    pub fingerprint: Fingerprint,
    /// fingerprint の算出方式。
    pub fingerprint_algorithm: FingerprintAlgorithm,
}

impl EffectInfo {
    /// effect 情報とセレクターを、単一の fingerprint 算出結果から組み立てる。
    pub fn new(object: ObjectSelector, input: EffectFingerprintInput<'_>) -> Self {
        let fingerprint = effect_fingerprint(input);
        let algorithm = FingerprintAlgorithm::GENERATED;
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
                fingerprint: fingerprint.clone(),
                fingerprint_algorithm: algorithm.clone(),
            },
            fingerprint,
            fingerprint_algorithm: algorithm,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvailableEffect {
    /// effect 名。
    pub name: String,
    /// effect の種別。
    pub effect_type: EffectType,
    /// 対応内容を表すフラグ。
    pub flags: EffectFlags,
    /// 設定項目の定義。
    pub items: Vec<AvailableEffectItem>,
}

/// 利用可能な effect の設定項目定義。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvailableEffectItem {
    /// 設定項目名。
    pub name: String,
    /// 設定項目の種別。
    pub item_type: EffectItemType,
}

/// effect が対応する内容を表すフラグ。
///
/// 既知ビットを bool で展開しつつ、ビット列そのものを `raw` に併記する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectFlags {
    /// ビット列。生成元が既知ビットしか復元できない場合は未知ビットを含まない。
    pub raw: u32,
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
    /// 生のビット列から既知フラグを展開する。
    pub fn from_raw(raw: u32) -> Self {
        Self {
            raw,
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

/// 種別を一意に表す名前を組み立てる。
///
/// 既知の種別は snake_case 名をそのまま用い、未知の種別は raw 値を括弧で
/// 添えた形にする。既知の名前に括弧は現れないため、両者の表現が重なることはない。
fn kind_name(name: Option<&str>, raw: i32) -> String {
    match name {
        Some(name) => name.to_string(),
        None => format!("{UNKNOWN_TAG}({raw})"),
    }
}

/// 既知種別は文字列、未知種別は raw 付きオブジェクトとして書く。
fn serialize_kind<S>(name: Option<&str>, raw: i32, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match name {
        Some(name) => serializer.serialize_str(name),
        None => {
            let mut map = serializer.serialize_map(Some(2))?;
            map.serialize_entry("type", UNKNOWN_TAG)?;
            map.serialize_entry("raw", &raw)?;
            map.end()
        }
    }
}

/// 未知種別オブジェクトから raw 値を読む。
fn visit_unknown_kind<'de, A>(mut map: A) -> Result<i32, A::Error>
where
    A: MapAccess<'de>,
{
    let mut tag: Option<String> = None;
    let mut raw: Option<i32> = None;
    while let Some(key) = map.next_key::<String>()? {
        match key.as_str() {
            "type" => {
                if tag.is_some() {
                    return Err(de::Error::duplicate_field("type"));
                }
                tag = Some(map.next_value()?);
            }
            "raw" => {
                if raw.is_some() {
                    return Err(de::Error::duplicate_field("raw"));
                }
                raw = Some(map.next_value()?);
            }
            other => return Err(de::Error::unknown_field(other, UNKNOWN_FIELDS)),
        }
    }
    let tag = tag.ok_or_else(|| de::Error::missing_field("type"))?;
    if tag != UNKNOWN_TAG {
        return Err(de::Error::custom(format!(
            "種別オブジェクトの type は \"{UNKNOWN_TAG}\" である必要があります: {tag}"
        )));
    }
    raw.ok_or_else(|| de::Error::missing_field("raw"))
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

    fn known_item_types() -> Vec<EffectItemType> {
        vec![
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
            items: vec![AvailableEffectItem {
                name: "範囲".to_string(),
                item_type: EffectItemType::Integer,
            }],
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
        for item_type in known_item_types() {
            let s = serde_json::to_string(&item_type).unwrap();
            assert_eq!(s, format!("\"{item_type}\""));
            let restored: EffectItemType = serde_json::from_str(&s).unwrap();
            assert_eq!(restored, item_type);
        }
    }

    #[test]
    fn effect_item_type_raw_values_match_sdk_order() {
        // 既知種別は 1..=16 に連番で割り当てられる。
        let raws: Vec<i32> = known_item_types().iter().map(|t| t.as_raw()).collect();
        assert_eq!(raws, (1..=16).collect::<Vec<i32>>());
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

        for item_type in known_item_types() {
            let unknown = EffectItemType::Unknown(item_type.as_raw());
            assert_eq!(unknown.as_raw(), item_type.as_raw());
            assert_ne!(unknown, item_type);
            assert_ne!(
                unknown.kind_name(),
                item_type.kind_name(),
                "{item_type} と同じ名前になりました"
            );
        }
    }

    #[test]
    fn kind_name_is_unique_across_known_types() {
        let mut names: Vec<String> = known_item_types()
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
        for item_type in known_item_types() {
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
        assert_eq!(flags.raw, 6);
    }

    #[test]
    fn effect_flags_keeps_unknown_bits_in_raw() {
        let flags = EffectFlags::from_raw(0x8000_0000);
        assert!(!flags.video && !flags.audio && !flags.filter && !flags.camera);
        assert_eq!(flags.raw, 0x8000_0000);
    }

    #[test]
    fn effect_info_roundtrip() {
        let info = sample_effect_info();
        let s = serde_json::to_string(&info).unwrap();
        let restored: EffectInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, info);
    }

    #[test]
    fn effect_info_shares_one_fingerprint_with_selector() {
        let info = sample_effect_info();
        assert_eq!(info.fingerprint, info.selector.fingerprint);
        assert_eq!(
            info.fingerprint_algorithm,
            info.selector.fingerprint_algorithm
        );
        assert_eq!(info.fingerprint_algorithm, FingerprintAlgorithm::GENERATED);
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
    fn available_effect_keeps_unknown_types() {
        let effect = AvailableEffect {
            effect_type: EffectType::Unknown(42),
            items: vec![AvailableEffectItem {
                name: "未知".to_string(),
                item_type: EffectItemType::Unknown(99),
            }],
            ..sample_available_effect()
        };
        let s = serde_json::to_string(&effect).unwrap();
        let restored: AvailableEffect = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, effect);
    }
}
