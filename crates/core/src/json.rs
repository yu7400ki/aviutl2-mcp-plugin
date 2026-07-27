//! strict JSON パース規則。
//!
//! 重複 JSON key、非有限数、不正 UTF-8、必須フィールド欠落を拒否する。

use serde::de::{self, Deserialize, Deserializer, Error as DeError, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, Serializer};
use std::collections::HashSet;
use std::fmt;

/// strict JSON パースエラー。
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum JsonStrictError {
    /// 不正な UTF-8。
    #[error("不正な UTF-8 です")]
    InvalidUtf8,
    /// 重複 JSON key。
    #[error("重複した JSON key です: {0}")]
    DuplicateKey(String),
    /// 非有限数。
    #[error("非有限数 (NaN / Infinity) は許可されません")]
    NonFiniteFloat,
    /// JSON パースエラー。
    #[error("JSON パースエラー: {0}")]
    ParseError(String),
    /// 構造エラー。
    #[error("JSON 構造エラー: {0}")]
    StructureError(String),
}

const DUPLICATE_KEY_PREFIX: &str = "__DUPLICATE_KEY__:";
const NON_FINITE_FLOAT_MARKER: &str = "__NON_FINITE_FLOAT__";

impl From<serde_json::Error> for JsonStrictError {
    fn from(e: serde_json::Error) -> Self {
        let s = e.to_string();
        if let Some(idx) = s.find(DUPLICATE_KEY_PREFIX) {
            let rest = &s[idx + DUPLICATE_KEY_PREFIX.len()..];
            let key = rest.split(" at line ").next().unwrap_or(rest);
            return JsonStrictError::DuplicateKey(key.to_string());
        }
        if s.contains(NON_FINITE_FLOAT_MARKER) {
            return JsonStrictError::NonFiniteFloat;
        }
        JsonStrictError::ParseError(s)
    }
}

impl From<JsonStrictError> for serde_json::Error {
    fn from(e: JsonStrictError) -> Self {
        serde::de::Error::custom(e.to_string())
    }
}

/// UTF-8 バイト列を strict JSON として検証し、`serde_json::Value` を返す。
///
/// 以下を拒否する:
/// - 不正 UTF-8
/// - 重複 JSON key
/// - 非有限数 (NaN / Infinity)
///
/// 未知フィールドは `Value` レベルでは保持する。struct への逆直列化時に
/// `#[serde(deny_unknown_fields)]` で制御すること。
pub fn parse_json(bytes: &[u8]) -> Result<serde_json::Value, JsonStrictError> {
    let s = std::str::from_utf8(bytes).map_err(|_| JsonStrictError::InvalidUtf8)?;
    let mut de = serde_json::Deserializer::from_str(s);
    let value = StrictValue::deserialize(&mut de)?;
    de.end()
        .map_err(|e| JsonStrictError::ParseError(e.to_string()))?;
    Ok(value.0)
}

/// バイト列を strict JSON として検証し、指定の型へ逆直列化する。
pub fn deserialize_json<T>(bytes: &[u8]) -> Result<T, JsonStrictError>
where
    T: for<'de> Deserialize<'de>,
{
    let value = parse_json(bytes)?;
    serde_json::from_value(value).map_err(|e| JsonStrictError::StructureError(e.to_string()))
}

struct StrictValue(serde_json::Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = deserializer.deserialize_any(StrictVisitor)?;
        Ok(StrictValue(value))
    }
}

impl Serialize for StrictValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = serde_json::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("JSON 値")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(v.into()))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if !v.is_finite() {
            return Err(E::custom(NON_FINITE_FLOAT_MARKER));
        }
        Ok(serde_json::Value::Number(
            serde_json::Number::from_f64(v).ok_or_else(|| E::custom(NON_FINITE_FLOAT_MARKER))?,
        ))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(serde_json::Value::String(v.to_string()))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        Deserialize::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut arr = Vec::new();
        while let Some(v) = seq.next_element::<StrictValue>()? {
            arr.push(v.0);
        }
        Ok(serde_json::Value::Array(arr))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut obj = serde_json::Map::new();
        let mut seen = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(A::Error::custom(format!("{DUPLICATE_KEY_PREFIX}{key}")));
            }
            let value = map.next_value::<StrictValue>()?;
            obj.insert(key, value.0);
        }
        Ok(serde_json::Value::Object(obj))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct Sample {
        a: i32,
        b: i32,
    }

    #[test]
    fn parse_json_valid() {
        let v = parse_json(br#"{"a":1,"b":2}"#).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], 2);
    }

    #[test]
    fn parse_json_rejects_duplicate_key() {
        let result = parse_json(br#"{"a":1,"a":2}"#);
        assert!(matches!(result, Err(JsonStrictError::DuplicateKey(_))));
    }

    #[test]
    fn parse_json_rejects_nested_duplicate_key() {
        let result = parse_json(br#"{"outer":{"a":1,"a":2}}"#);
        assert!(matches!(result, Err(JsonStrictError::DuplicateKey(_))));
    }

    #[test]
    fn parse_json_rejects_nan() {
        let result = parse_json(b"NaN");
        assert!(matches!(result, Err(JsonStrictError::ParseError(_))));
    }

    #[test]
    fn parse_json_rejects_infinity() {
        let result = parse_json(b"Infinity");
        assert!(matches!(result, Err(JsonStrictError::ParseError(_))));
    }

    #[test]
    fn parse_json_rejects_out_of_range_float() {
        // 表現可能範囲を超える指数は非有限数として拒否される。
        let result = parse_json(br#"{"x":1e309}"#);
        assert!(result.is_err());
    }

    #[test]
    fn parse_json_rejects_invalid_utf8() {
        let result = parse_json(&[0x80, 0x81, 0x82]);
        assert!(matches!(result, Err(JsonStrictError::InvalidUtf8)));
    }

    #[test]
    fn deserialize_json_roundtrip() {
        let sample: Sample = deserialize_json(br#"{"a":1,"b":2}"#).unwrap();
        assert_eq!(sample.a, 1);
        assert_eq!(sample.b, 2);
    }

    #[test]
    fn deserialize_json_rejects_duplicate_key() {
        let result: Result<Sample, _> = deserialize_json(br#"{"a":1,"a":2,"b":3}"#);
        assert!(matches!(result, Err(JsonStrictError::DuplicateKey(_))));
    }

    #[test]
    fn deserialize_json_rejects_missing_field() {
        let result: Result<Sample, _> = deserialize_json(br#"{"a":1}"#);
        assert!(matches!(result, Err(JsonStrictError::StructureError(_))));
    }
}
