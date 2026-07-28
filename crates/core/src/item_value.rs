//! effect 設定項目の値。

use crate::number::FiniteF64;
use serde::{Deserialize, Serialize};

/// effect 設定項目の値。
///
/// 種別ごとに異なる形を持つため `type` を判別子とする tagged union で表す。
/// 読み取りでは未対応種別も破棄せず [`ItemValue::Unknown`] として生文字列を保持する。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ItemValue {
    /// 実数。
    #[serde(rename = "number")]
    Number {
        /// 値。
        value: FiniteF64,
    },
    /// 整数。
    #[serde(rename = "integer")]
    Integer {
        /// 値。
        value: i64,
    },
    /// 真偽値。
    #[serde(rename = "bool")]
    Bool {
        /// 値。
        value: bool,
    },
    /// 正規化済みの色表現。
    #[serde(rename = "color")]
    Color {
        /// 値。
        value: String,
    },
    /// 一覧からの選択。
    #[serde(rename = "choice")]
    Choice {
        /// 選択された表示文字列。
        value: String,
        /// 選択肢の 0 始まりインデックス。特定できない場合は null。
        index: Option<usize>,
    },
    /// ファイルパス。
    #[serde(rename = "file")]
    File {
        /// パス。
        path: String,
    },
    /// フォルダパス。
    #[serde(rename = "folder")]
    Folder {
        /// パス。
        path: String,
    },
    /// フォント名。
    #[serde(rename = "font")]
    Font {
        /// フォント名。
        name: String,
    },
    /// テキスト。
    #[serde(rename = "text")]
    Text {
        /// 値。
        value: String,
    },
    /// 未対応種別の生値。
    #[serde(rename = "unknown")]
    Unknown {
        /// 生文字列。
        raw: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_values() -> Vec<ItemValue> {
        vec![
            ItemValue::Number {
                value: FiniteF64::try_new(1.5).unwrap(),
            },
            ItemValue::Integer { value: -3 },
            ItemValue::Bool { value: true },
            ItemValue::Color {
                value: "#ff8800".to_string(),
            },
            ItemValue::Choice {
                value: "通常".to_string(),
                index: Some(0),
            },
            ItemValue::Choice {
                value: "通常".to_string(),
                index: None,
            },
            ItemValue::File {
                path: r"C:\movie.mp4".to_string(),
            },
            ItemValue::Folder {
                path: r"C:\assets".to_string(),
            },
            ItemValue::Font {
                name: "Meiryo".to_string(),
            },
            ItemValue::Text {
                value: "字幕".to_string(),
            },
            ItemValue::Unknown {
                raw: "future=1".to_string(),
            },
        ]
    }

    #[test]
    fn item_value_roundtrip() {
        for value in sample_values() {
            let s = serde_json::to_string(&value).unwrap();
            let restored: ItemValue = serde_json::from_str(&s).unwrap();
            assert_eq!(restored, value);
        }
    }

    #[test]
    fn item_value_tag_is_snake_case() {
        let value = ItemValue::Integer { value: 1 };
        assert_eq!(
            serde_json::to_value(value).unwrap(),
            serde_json::json!({"type": "integer", "value": 1})
        );
    }

    #[test]
    fn item_value_unknown_preserves_raw() {
        let s = r#"{"type":"unknown","raw":"opaque"}"#;
        let value: ItemValue = serde_json::from_str(s).unwrap();
        assert_eq!(
            value,
            ItemValue::Unknown {
                raw: "opaque".to_string()
            }
        );
        assert_eq!(serde_json::to_string(&value).unwrap(), s);
    }

    #[test]
    fn item_value_choice_index_defaults_to_none() {
        let value: ItemValue = serde_json::from_str(r#"{"type":"choice","value":"通常"}"#).unwrap();
        assert_eq!(
            value,
            ItemValue::Choice {
                value: "通常".to_string(),
                index: None,
            }
        );
    }

    #[test]
    fn item_value_number_rejects_non_finite_json_literals() {
        // NaN / Infinity は JSON の字句として存在しないため、
        // FiniteF64 の検証へ到達する前にパーサが拒否する。
        for literal in ["NaN", "Infinity", "-Infinity"] {
            let s = format!(r#"{{"type":"number","value":{literal}}}"#);
            let result: Result<ItemValue, _> = serde_json::from_str(&s);
            assert!(result.is_err(), "{literal} が受理された");
        }
    }

    #[test]
    fn item_value_number_rejects_out_of_range_exponent() {
        // 表現範囲を超える指数も数値へ変換できないため拒否される。
        let result: Result<ItemValue, _> =
            serde_json::from_str(r#"{"type":"number","value":1e309}"#);
        assert!(result.is_err());
    }

    #[test]
    fn item_value_rejects_unknown_tag() {
        let result: Result<ItemValue, _> = serde_json::from_str(r#"{"type":"vector","x":1}"#);
        assert!(result.is_err());
    }
}
