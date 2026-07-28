//! strict JSON パース規則。
//!
//! 重複 JSON key、非有限数、不正 UTF-8、必須フィールド欠落を拒否する。

use serde::de::{self, Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use std::cell::Cell;
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

impl From<JsonStrictError> for serde_json::Error {
    fn from(e: JsonStrictError) -> Self {
        serde::de::Error::custom(e.to_string())
    }
}

/// strict 規則で拒否した理由。
///
/// serde の `Error::custom` は文字列しか運べず、`serde_json::Error` は独自の
/// エラー種別を保持できない。そのため拒否を検出した visitor が理由をこの型で
/// [`RejectSlot`] へ記録し、呼び出し側はそれだけを見て種別を決める。
/// 判定は `serde_json` のメッセージ書式に一切依存しない。
#[derive(Debug, Clone, PartialEq, Eq)]
enum StrictReject {
    DuplicateKey(String),
    NonFiniteFloat,
}

/// 拒否理由の記録先。パース 1 回ごとに新規作成し、visitor と共有する。
type RejectSlot = Cell<Option<StrictReject>>;

/// 最初に検出した拒否理由を記録する。
fn record_reject(slot: &RejectSlot, reject: StrictReject) {
    let current = slot.take();
    slot.set(current.or(Some(reject)));
}

/// パース失敗を、記録された拒否理由に基づいて分類する。
fn classify_error(slot: &RejectSlot, error: serde_json::Error) -> JsonStrictError {
    match slot.take() {
        Some(StrictReject::DuplicateKey(key)) => JsonStrictError::DuplicateKey(key),
        Some(StrictReject::NonFiniteFloat) => JsonStrictError::NonFiniteFloat,
        None => JsonStrictError::ParseError(error.to_string()),
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
    let reject = RejectSlot::new(None);
    let mut de = serde_json::Deserializer::from_str(s);
    let value = StrictValueSeed { reject: &reject }
        .deserialize(&mut de)
        .map_err(|e| classify_error(&reject, e))?;
    de.end().map_err(|e| classify_error(&reject, e))?;
    Ok(value)
}

/// バイト列を strict JSON として検証し、指定の型へ逆直列化する。
pub fn deserialize_json<T>(bytes: &[u8]) -> Result<T, JsonStrictError>
where
    T: for<'de> Deserialize<'de>,
{
    let value = parse_json(bytes)?;
    serde_json::from_value(value).map_err(|e| JsonStrictError::StructureError(e.to_string()))
}

/// strict 規則で 1 個の JSON 値を読む seed。
///
/// 拒否理由の記録先を入れ子の値へ伝播させるため、`Deserialize` ではなく
/// `DeserializeSeed` として実装する。
struct StrictValueSeed<'a> {
    reject: &'a RejectSlot,
}

impl<'de> DeserializeSeed<'de> for StrictValueSeed<'_> {
    type Value = serde_json::Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictVisitor {
            reject: self.reject,
        })
    }
}

struct StrictVisitor<'a> {
    reject: &'a RejectSlot,
}

impl<'de> Visitor<'de> for StrictVisitor<'_> {
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
        // JSON テキスト経由では、NaN / Infinity は字句解析で、表現範囲外の指数は
        // 数値変換で拒否されるため、非有限値がここへ渡ることはない。
        // JSON 以外の Deserializer から値を受ける場合に備えた防御的分岐である。
        // `Number::from_f64` は NaN・無限大に対して `None` を返す。
        match serde_json::Number::from_f64(v) {
            Some(number) => Ok(serde_json::Value::Number(number)),
            None => {
                record_reject(self.reject, StrictReject::NonFiniteFloat);
                Err(E::custom("非有限数 (NaN / Infinity) は許可されません"))
            }
        }
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
        StrictValueSeed {
            reject: self.reject,
        }
        .deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut arr = Vec::new();
        while let Some(v) = seq.next_element_seed(StrictValueSeed {
            reject: self.reject,
        })? {
            arr.push(v);
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
                record_reject(self.reject, StrictReject::DuplicateKey(key.clone()));
                return Err(de::Error::custom(format!("重複した JSON key です: {key}")));
            }
            let value = map.next_value_seed(StrictValueSeed {
                reject: self.reject,
            })?;
            obj.insert(key, value);
        }
        Ok(serde_json::Value::Object(obj))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde::de::IntoDeserializer;
    use serde::de::value::{Error as ValueError, F64Deserializer};

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
    fn parse_json_rejects_duplicate_key_inside_array() {
        let result = parse_json(br#"[{"a":1,"a":2}]"#);
        assert_eq!(result, Err(JsonStrictError::DuplicateKey("a".to_string())));
    }

    #[test]
    fn parse_json_rejects_duplicate_key_nested_under_arrays() {
        let result = parse_json(br#"{"x":[1,{"y":[{"z":1,"z":2}]}]}"#);
        assert_eq!(result, Err(JsonStrictError::DuplicateKey("z".to_string())));
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
        // 表現可能範囲を超える指数は serde_json の数値パース自体が拒否するため、
        // visitor の非有限数チェックまでは到達しない。
        let result = parse_json(br#"{"x":1e309}"#);
        assert!(matches!(result, Err(JsonStrictError::ParseError(_))));
    }

    #[test]
    fn non_finite_float_is_recorded_as_reject() {
        // 非有限数を直接 visitor へ渡し、拒否理由が記録されることを確認する。
        let slot = RejectSlot::new(None);
        let deserializer: F64Deserializer<ValueError> = f64::INFINITY.into_deserializer();
        let result = StrictValueSeed { reject: &slot }.deserialize(deserializer);
        assert!(result.is_err());
        assert_eq!(slot.take(), Some(StrictReject::NonFiniteFloat));
    }

    #[test]
    fn parse_json_rejects_invalid_utf8() {
        let result = parse_json(&[0x80, 0x81, 0x82]);
        assert!(matches!(result, Err(JsonStrictError::InvalidUtf8)));
    }

    #[test]
    fn duplicate_key_is_reported_verbatim() {
        // key がエラーメッセージの断片に似ていても、記録した key がそのまま返る。
        // 種別も key もパーサのメッセージ書式から復元していないことを示す。
        let result = parse_json(br#"{"a at line 1 column 2":1,"a at line 1 column 2":2}"#);
        assert_eq!(
            result,
            Err(JsonStrictError::DuplicateKey(
                "a at line 1 column 2".to_string()
            ))
        );
    }

    #[test]
    fn classify_error_uses_recorded_reject() {
        let syntax_error = || serde_json::from_str::<serde_json::Value>("{").unwrap_err();

        let slot = RejectSlot::new(None);
        record_reject(&slot, StrictReject::DuplicateKey("k".to_string()));
        assert_eq!(
            classify_error(&slot, syntax_error()),
            JsonStrictError::DuplicateKey("k".to_string())
        );

        let slot = RejectSlot::new(None);
        record_reject(&slot, StrictReject::NonFiniteFloat);
        assert_eq!(
            classify_error(&slot, syntax_error()),
            JsonStrictError::NonFiniteFloat
        );

        // 記録が無い場合のみ、素の構文エラーとして扱う。
        let slot = RejectSlot::new(None);
        assert!(matches!(
            classify_error(&slot, syntax_error()),
            JsonStrictError::ParseError(_)
        ));
    }

    #[test]
    fn record_reject_keeps_first_reason() {
        let slot = RejectSlot::new(None);
        record_reject(&slot, StrictReject::DuplicateKey("first".to_string()));
        record_reject(&slot, StrictReject::NonFiniteFloat);
        assert_eq!(
            slot.take(),
            Some(StrictReject::DuplicateKey("first".to_string()))
        );
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
