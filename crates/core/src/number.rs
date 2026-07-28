//! 有限であることを保証する浮動小数点。

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// 有限（NaN でも無限大でもない）な `f64`。
///
/// JSON は非有限数を表現できず、`serde_json` は `f64::NAN` を静かに `null` へ
/// 落とす。DTO の浮動小数点フィールドをこの型で持てば、値が失われた JSON を
/// 出力する経路が型として存在しなくなる。逆直列化では非有限値を拒否する。
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct FiniteF64(f64);

impl FiniteF64 {
    /// 有限値のときだけ生成する。
    pub fn try_new(value: f64) -> Option<Self> {
        if value.is_finite() {
            Some(Self(value))
        } else {
            None
        }
    }

    /// 内部の値を返す。常に有限である。
    pub fn get(&self) -> f64 {
        self.0
    }
}

impl fmt::Display for FiniteF64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<FiniteF64> for f64 {
    fn from(value: FiniteF64) -> Self {
        value.0
    }
}

impl Serialize for FiniteF64 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for FiniteF64 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        FiniteF64::try_new(value).ok_or_else(|| {
            serde::de::Error::custom("非有限数 (NaN / Infinity) は許可されません".to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::IntoDeserializer;
    use serde::de::value::{Error as ValueError, F64Deserializer};

    #[test]
    fn finite_f64_accepts_finite() {
        assert_eq!(FiniteF64::try_new(1.5).map(|v| v.get()), Some(1.5));
        assert_eq!(FiniteF64::try_new(0.0).map(|v| v.get()), Some(0.0));
        assert_eq!(
            FiniteF64::try_new(f64::MAX).map(|v| v.get()),
            Some(f64::MAX)
        );
    }

    #[test]
    fn finite_f64_rejects_non_finite() {
        assert!(FiniteF64::try_new(f64::NAN).is_none());
        assert!(FiniteF64::try_new(f64::INFINITY).is_none());
        assert!(FiniteF64::try_new(f64::NEG_INFINITY).is_none());
    }

    #[test]
    fn finite_f64_roundtrip() {
        let value = FiniteF64::try_new(29.97).unwrap();
        let s = serde_json::to_string(&value).unwrap();
        assert_eq!(s, "29.97");
        let restored: FiniteF64 = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, value);
    }

    #[test]
    fn finite_f64_serialized_as_single_value() {
        // newtype ではなく素の数値として直列化する。
        let value = FiniteF64::try_new(-1.0).unwrap();
        assert_eq!(
            serde_json::to_value(value).unwrap(),
            serde_json::json!(-1.0)
        );
    }

    #[test]
    fn finite_f64_deserialize_rejects_non_finite() {
        // JSON テキストは NaN / Infinity を字句として持たないため、
        // 非有限値を運べる Deserializer から直接与えて検証する。
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let deserializer: F64Deserializer<ValueError> = value.into_deserializer();
            let result = FiniteF64::deserialize(deserializer);
            assert!(result.is_err());
        }
    }

    #[test]
    fn finite_f64_display_matches_inner() {
        assert_eq!(FiniteF64::try_new(1.5).unwrap().to_string(), "1.5");
        assert_eq!(FiniteF64::try_new(2.0).unwrap().to_string(), "2");
    }
}
