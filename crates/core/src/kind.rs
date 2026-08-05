//! 未知値を捨てない種別列挙の共通表現。
//!
//! 既知の種別は snake_case 文字列、未知の種別は
//! `{"type":"unknown","raw":<i32>}` として書く。種別ごとに表現を組み直すと、
//! 片方だけが未知値を落とす形になり得るため、写し方をここへ一本化する。

use serde::Serializer;
use serde::de::{self, MapAccess};
use serde::ser::SerializeMap;

/// 未知種別を表すオブジェクトの判別子。
const UNKNOWN_TAG: &str = "unknown";

/// 未知種別オブジェクトが持つフィールド。
const UNKNOWN_FIELDS: &[&str] = &["type", "raw"];

/// 種別を一意に表す名前を組み立てる。
///
/// 既知の種別は snake_case 名をそのまま用い、未知の種別は raw 値を括弧で
/// 添えた形にする。既知の名前に括弧は現れないため、両者の表現が重なることはない。
pub(crate) fn kind_name(name: Option<&str>, raw: i32) -> String {
    match name {
        Some(name) => name.to_string(),
        None => format!("{UNKNOWN_TAG}({raw})"),
    }
}

/// 既知種別は文字列、未知種別は raw 付きオブジェクトとして書く。
pub(crate) fn serialize_kind<S>(
    name: Option<&str>,
    raw: i32,
    serializer: S,
) -> Result<S::Ok, S::Error>
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
pub(crate) fn visit_unknown_kind<'de, A>(mut map: A) -> Result<i32, A::Error>
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
