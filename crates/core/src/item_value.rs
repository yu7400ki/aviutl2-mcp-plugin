//! effect 設定項目の値と、書き込み時の検証。

use crate::effect::{AvailableEffectItem, EffectItemType};
use crate::error::ErrorCode;
use crate::number::FiniteF64;
use crate::validation::{
    PathSyntaxError, TextSyntaxError, validate_item_text, validate_multiline_item_text,
    validate_path,
};
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

impl ItemValue {
    /// 値の形を表す名前を返す。JSON の判別子と同じ表記である。
    ///
    /// 値そのものを含まないため、エラー応答へ載せてよい。
    pub fn kind(&self) -> &'static str {
        match self {
            ItemValue::Number { .. } => "number",
            ItemValue::Integer { .. } => "integer",
            ItemValue::Bool { .. } => "bool",
            ItemValue::Color { .. } => "color",
            ItemValue::Choice { .. } => "choice",
            ItemValue::File { .. } => "file",
            ItemValue::Folder { .. } => "folder",
            ItemValue::Font { .. } => "font",
            ItemValue::Text { .. } => "text",
            ItemValue::Unknown { .. } => "unknown",
        }
    }
}

/// 設定項目への書き込みの検証失敗。
///
/// 要求を直せば通るもの（`invalid_argument` 相当）と、対象が対応しないため
/// 直しても通らないもの（`unsupported_operation` 相当）を別の variant で表す。
/// 対応は [`ItemWriteError::error_code`] が持つ。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ItemWriteError {
    /// 未対応種別の生値は書き込めない。
    #[error("未対応種別の値は書き込めません")]
    UnknownValue,
    /// 指定された設定項目が対象 effect に存在しない。
    #[error("設定項目が存在しません: {item}")]
    ItemNotFound {
        /// 要求された設定項目名。
        item: String,
    },
    /// 設定項目の種別と値の形が対応しない。
    #[error("種別 {item_type} の設定項目に {value_kind} の値は指定できません")]
    ValueKindMismatch {
        /// 設定項目の種別名。
        item_type: String,
        /// 与えられた値の形。
        value_kind: &'static str,
    },
    /// 書き込みを公開していない種別。
    #[error("種別 {item_type} の設定項目への書き込みには対応していません")]
    UnsupportedItemType {
        /// 設定項目の種別名。
        item_type: String,
    },
    /// 文字列値の検証に失敗した。
    #[error(transparent)]
    Text(#[from] TextSyntaxError),
    /// パス値の検証に失敗した。
    #[error(transparent)]
    Path(#[from] PathSyntaxError),
}

impl ItemWriteError {
    /// 全 variant の代表値。
    ///
    /// [`ItemWriteError::reason`] が返し得る名前を数え上げるために用いる。
    /// `const` にできないのは、値を持つ variant が所有文字列を含むためである。
    /// 構文検証を包む variant は、包む側の全種別を並べる。
    pub fn all() -> Vec<ItemWriteError> {
        let mut all = vec![
            ItemWriteError::UnknownValue,
            ItemWriteError::ItemNotFound {
                item: "範囲".to_string(),
            },
            ItemWriteError::ValueKindMismatch {
                item_type: "integer".to_string(),
                value_kind: "text",
            },
            ItemWriteError::UnsupportedItemType {
                item_type: "figure".to_string(),
            },
        ];
        all.extend(
            TextSyntaxError::ALL
                .iter()
                .copied()
                .map(ItemWriteError::Text),
        );
        all.extend(
            PathSyntaxError::ALL
                .iter()
                .copied()
                .map(ItemWriteError::Path),
        );
        all
    }

    /// 失敗の種別を表す機械可読な名前を返す。名前を持たない失敗では `None`。
    ///
    /// 名前は種別だけを表し、書き込もうとした値・パス・設定項目名を含まない。
    /// 値の形が種別と対応しないことと未対応種別の生値は、種別名と値の形を
    /// 別のキーで返せるため名前を持たない。
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            ItemWriteError::UnsupportedItemType { .. } => Some("item_type_not_writable"),
            ItemWriteError::Text(error) => Some(error.reason()),
            ItemWriteError::Path(error) => Some(error.reason()),
            ItemWriteError::UnknownValue
            | ItemWriteError::ItemNotFound { .. }
            | ItemWriteError::ValueKindMismatch { .. } => None,
        }
    }

    /// 対応するエラーコードを返す。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            ItemWriteError::ItemNotFound { .. } => ErrorCode::NotFound,
            ItemWriteError::UnsupportedItemType { .. } => ErrorCode::UnsupportedOperation,
            ItemWriteError::UnknownValue
            | ItemWriteError::ValueKindMismatch { .. }
            | ItemWriteError::Text(_)
            | ItemWriteError::Path(_) => ErrorCode::InvalidArgument,
        }
    }
}

/// 設定項目名の実在確認から書き込む文字列の組み立てまでを行う。
///
/// `items` は対象 effect が公開している設定項目の一覧である。判定は次の順で
/// 行う。
///
/// 1. [`ItemValue::Unknown`] を拒否する
/// 2. `item` が `items` に存在することを確認する
/// 3. 種別への書き込みが公開されているかを確認する
/// 4. 種別と値の形が対応するかを確認する
/// 5. 書き込む文字列へ変換する
pub fn prepare_item_write(
    items: &[AvailableEffectItem],
    item: &str,
    value: &ItemValue,
) -> Result<String, ItemWriteError> {
    if matches!(value, ItemValue::Unknown { .. }) {
        return Err(ItemWriteError::UnknownValue);
    }
    let entry = items
        .iter()
        .find(|candidate| candidate.name == item)
        .ok_or_else(|| ItemWriteError::ItemNotFound {
            item: item.to_string(),
        })?;
    encode_item_value(&entry.item_type, value)
}

/// 種別と値を照合し、書き込む文字列を組み立てる。
///
/// 書き込みを公開する種別かどうかを、種別と値の対応より**先に**判定する。
/// 公開しない種別は受け付ける値の形自体を定めていないため、値の形の照合が
/// 成立しないためである。
pub fn encode_item_value(
    item_type: &EffectItemType,
    value: &ItemValue,
) -> Result<String, ItemWriteError> {
    if matches!(value, ItemValue::Unknown { .. }) {
        return Err(ItemWriteError::UnknownValue);
    }
    if !is_writable(item_type) {
        return Err(ItemWriteError::UnsupportedItemType {
            item_type: item_type.kind_name(),
        });
    }
    if !accepts(item_type, value) {
        return Err(ItemWriteError::ValueKindMismatch {
            item_type: item_type.kind_name(),
            value_kind: value.kind(),
        });
    }
    encode_value(value)
}

/// 種別を伴わずに判定できる範囲だけを検証する。
///
/// 対象 effect の設定項目一覧を持たない層が、要求を受け付けた時点で
/// 呼ぶための入口である。種別との対応は [`encode_item_value`] が見る。
pub fn validate_item_value(value: &ItemValue) -> Result<(), ItemWriteError> {
    encode_value(value).map(|_| ())
}

/// 書き込みを公開している種別か。
///
/// 複合種別のうち `scene` / `range` / `mask` / `figure` / `data` と未知種別は、
/// 値の表記が確定していないため公開しない。推測した表記で書き込むと、検証を
/// 通ったのに意図と異なる値が入る。
///
/// `combo` は表記が確定しているため公開する。読み取りは `select` と同じ
/// [`ItemValue::Choice`] で返し、有効な値を知る手段も `select` と同じ——
/// 既存のオブジェクトから読む——である。
fn is_writable(item_type: &EffectItemType) -> bool {
    matches!(
        item_type,
        EffectItemType::Integer
            | EffectItemType::Number
            | EffectItemType::Check
            | EffectItemType::Text
            | EffectItemType::String
            | EffectItemType::File
            | EffectItemType::Folder
            | EffectItemType::Font
            | EffectItemType::Color
            | EffectItemType::Select
            | EffectItemType::Combo
    )
}

/// 種別が値の形を受け付けるか。
fn accepts(item_type: &EffectItemType, value: &ItemValue) -> bool {
    matches!(
        (item_type, value),
        (EffectItemType::Integer, ItemValue::Integer { .. })
            | (EffectItemType::Number, ItemValue::Number { .. })
            | (EffectItemType::Check, ItemValue::Bool { .. })
            | (
                EffectItemType::Text | EffectItemType::String,
                ItemValue::Text { .. }
            )
            | (EffectItemType::File, ItemValue::File { .. })
            | (EffectItemType::Folder, ItemValue::Folder { .. })
            | (EffectItemType::Font, ItemValue::Font { .. })
            | (EffectItemType::Color, ItemValue::Color { .. })
            | (
                EffectItemType::Select | EffectItemType::Combo,
                ItemValue::Choice { .. }
            )
    )
}

/// 値を書き込む文字列へ変換する。
///
/// 読み取りが返した値をそのまま書き戻せるよう、表記を独自に整形しない。
/// 整数は十進整数、実数は指数表記を用いない十進小数、真偽値は `0` / `1` と
/// する。実数は元の値へ戻せる最短の桁数で書き出す。
///
/// [`ItemValue::Text`] だけは改行とタブを許す。これらを拒否すると、複数行の
/// テキストを書く直接の手段が無くなる。色・フォント名・選択肢の値に改行が
/// 現れる余地は無いため、緩和しない。
///
/// [`ItemValue::Choice`] の `index` は読み取りが付ける補助情報であり、
/// 書き込みでは無視する。選択肢の並びはホスト側の都合で変わり得るため、
/// index を正としない。
fn encode_value(value: &ItemValue) -> Result<String, ItemWriteError> {
    match value {
        ItemValue::Unknown { .. } => Err(ItemWriteError::UnknownValue),
        ItemValue::Integer { value } => Ok(value.to_string()),
        ItemValue::Number { value } => Ok(value.to_string()),
        ItemValue::Bool { value } => Ok(if *value { "1" } else { "0" }.to_string()),
        ItemValue::Text { value } => encode_multiline_text(value),
        ItemValue::Color { value } | ItemValue::Choice { value, .. } => encode_text(value),
        ItemValue::Font { name } => encode_text(name),
        ItemValue::File { path } | ItemValue::Folder { path } => encode_path(path),
    }
}

/// 単一行の文字列値をそのまま渡せる形か確認する。
fn encode_text(value: &str) -> Result<String, ItemWriteError> {
    validate_item_text(value)?;
    Ok(value.to_string())
}

/// 複数行を取り得る文字列値をそのまま渡せる形か確認する。
fn encode_multiline_text(value: &str) -> Result<String, ItemWriteError> {
    validate_multiline_item_text(value)?;
    Ok(value.to_string())
}

/// パス値をそのまま渡せる形か確認する。
///
/// パスとしての構文に加えて、設定項目の値としての上限も課す。2 つは択一で
/// はなく、どちらも掛かる。パスの上限は UTF-16 code unit で数えるため、
/// どの文字集合でも設定値のバイト上限より緩い。パス側だけを見ると、単一の
/// 項目が応答サイズを圧迫しないという上限の目的が達成されない。
fn encode_path(path: &str) -> Result<String, ItemWriteError> {
    validate_path(path)?;
    validate_item_text(path)?;
    Ok(path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::REASON_VALUES;
    use crate::validation::{MAX_ITEM_VALUE_BYTES, MAX_PATH_UTF16_UNITS};

    /// variant を表す名前を返す。
    ///
    /// 網羅 match で書く。variant を足すとここがコンパイルエラーになり、
    /// すぐ下の一覧と [`ItemWriteError::all`] へ足す必要があることが分かる。
    fn write_variant_name(error: &ItemWriteError) -> &'static str {
        match error {
            ItemWriteError::UnknownValue => "UnknownValue",
            ItemWriteError::ItemNotFound { .. } => "ItemNotFound",
            ItemWriteError::ValueKindMismatch { .. } => "ValueKindMismatch",
            ItemWriteError::UnsupportedItemType { .. } => "UnsupportedItemType",
            ItemWriteError::Text(_) => "Text",
            ItemWriteError::Path(_) => "Path",
        }
    }

    #[test]
    fn all_write_failures_cover_every_variant() {
        const VARIANTS: &[&str] = &[
            "UnknownValue",
            "ItemNotFound",
            "ValueKindMismatch",
            "UnsupportedItemType",
            "Text",
            "Path",
        ];
        let covered: Vec<&str> = ItemWriteError::all()
            .iter()
            .map(write_variant_name)
            .collect();
        for variant in VARIANTS {
            assert!(
                covered.contains(variant),
                "{variant} の代表値が一覧にありません"
            );
        }
        for variant in &covered {
            assert!(
                VARIANTS.contains(variant),
                "{variant} が網羅すべき variant の一覧にありません"
            );
        }
    }

    #[test]
    fn all_write_failures_cover_every_syntax_kind() {
        // 名前は包む側の種別で決まる。variant を 1 つ挙げるだけでは、
        // 包む側に種別が増えたときに一覧が追随しない。
        let reasons: Vec<Option<&str>> = ItemWriteError::all()
            .iter()
            .map(ItemWriteError::reason)
            .collect();
        for source in TextSyntaxError::ALL {
            assert!(reasons.contains(&Some(source.reason())), "{source}");
        }
        for source in PathSyntaxError::ALL {
            assert!(reasons.contains(&Some(source.reason())), "{source}");
        }
    }

    #[test]
    fn write_failures_carry_the_reason_of_the_syntax_error_they_wrap() {
        // 検証の失敗種別をそのまま名乗る。写し替える層を挟むと、種別の
        // 取り違えが起きても誰も落ちない。
        for error in PathSyntaxError::ALL {
            assert_eq!(
                ItemWriteError::Path(*error).reason(),
                Some(error.reason()),
                "{error}"
            );
        }
        for error in TextSyntaxError::ALL {
            assert_eq!(
                ItemWriteError::Text(*error).reason(),
                Some(error.reason()),
                "{error}"
            );
        }
    }

    #[test]
    fn write_failures_only_name_reasons_from_the_shared_value_set() {
        let named = [
            ItemWriteError::UnsupportedItemType {
                item_type: "figure".to_string(),
            },
            ItemWriteError::Text(TextSyntaxError::ContainsNul),
            ItemWriteError::Path(PathSyntaxError::UncPath),
        ];
        for error in named {
            let reason = error.reason().expect("名前を持つ失敗です");
            assert!(
                REASON_VALUES.contains(&reason),
                "{reason} が reason の値域にありません"
            );
        }
        // 種別名と値の形を別のキーで返せる失敗は名前を持たない。
        for error in [
            ItemWriteError::UnknownValue,
            ItemWriteError::ItemNotFound {
                item: "範囲".to_string(),
            },
            ItemWriteError::ValueKindMismatch {
                item_type: "integer".to_string(),
                value_kind: "text",
            },
        ] {
            assert_eq!(error.reason(), None, "{error}");
        }
    }

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

    /// 書き込みを公開する種別と、受け付ける値の組。
    fn writable_pairs() -> Vec<(EffectItemType, ItemValue, &'static str)> {
        vec![
            (
                EffectItemType::Integer,
                ItemValue::Integer { value: -3 },
                "-3",
            ),
            (
                EffectItemType::Number,
                ItemValue::Number {
                    value: FiniteF64::try_new(12.5).unwrap(),
                },
                "12.5",
            ),
            (EffectItemType::Check, ItemValue::Bool { value: true }, "1"),
            (
                EffectItemType::Text,
                ItemValue::Text {
                    value: "字幕".to_string(),
                },
                "字幕",
            ),
            (
                EffectItemType::String,
                ItemValue::Text {
                    value: "文字列".to_string(),
                },
                "文字列",
            ),
            (
                EffectItemType::File,
                ItemValue::File {
                    path: r"C:\movie.mp4".to_string(),
                },
                r"C:\movie.mp4",
            ),
            (
                EffectItemType::Folder,
                ItemValue::Folder {
                    path: r"C:\assets".to_string(),
                },
                r"C:\assets",
            ),
            (
                EffectItemType::Font,
                ItemValue::Font {
                    name: "Meiryo".to_string(),
                },
                "Meiryo",
            ),
            (
                EffectItemType::Color,
                ItemValue::Color {
                    value: "#ff8800".to_string(),
                },
                "#ff8800",
            ),
            (
                EffectItemType::Select,
                ItemValue::Choice {
                    value: "通常".to_string(),
                    index: Some(2),
                },
                "通常",
            ),
            (
                EffectItemType::Combo,
                ItemValue::Choice {
                    value: "通常".to_string(),
                    index: Some(2),
                },
                "通常",
            ),
        ]
    }

    /// 書き込みを公開しない種別。
    fn non_writable_item_types() -> Vec<EffectItemType> {
        vec![
            EffectItemType::Scene,
            EffectItemType::Range,
            EffectItemType::Mask,
            EffectItemType::Figure,
            EffectItemType::Data,
            EffectItemType::Unknown(99),
        ]
    }

    /// 種別ごとに、書き込みを公開するかを述べる。
    ///
    /// [`EffectItemType`] に対する網羅 `match` であり `_` を使わない。**種別を
    /// 足すとここが落ち、公開するかを書くまでコンパイルできない。**
    fn expects_writable(item_type: &EffectItemType) -> bool {
        match item_type {
            EffectItemType::Integer
            | EffectItemType::Number
            | EffectItemType::Check
            | EffectItemType::Text
            | EffectItemType::String
            | EffectItemType::File
            | EffectItemType::Folder
            | EffectItemType::Font
            | EffectItemType::Color
            | EffectItemType::Select
            | EffectItemType::Combo => true,
            EffectItemType::Scene
            | EffectItemType::Range
            | EffectItemType::Mask
            | EffectItemType::Figure
            | EffectItemType::Data
            | EffectItemType::Unknown(_) => false,
        }
    }

    /// 既知の種別を列挙する。
    ///
    /// 未知を名乗る種別値に当たるまで辿るため、既知の種別が増えても一覧は
    /// 種別値が連続する限り自動で伸びる。
    fn known_item_types() -> Vec<EffectItemType> {
        let mut types = Vec::new();
        for raw in 1i32.. {
            let item_type = EffectItemType::from_raw(raw);
            if item_type == EffectItemType::Unknown(raw) {
                break;
            }
            types.push(item_type);
        }
        types
    }

    /// 書き込みが公開されているかを、変換の応答から判定する。
    ///
    /// 公開しない種別だけが [`ItemWriteError::UnsupportedItemType`] を返す。
    /// 値の形の照合は公開の判定より後に行われるため、形が合わない値を渡しても
    /// 判定は変わらない。
    fn is_exposed_for_write(item_type: &EffectItemType) -> bool {
        let probe = ItemValue::Text {
            value: "文字列".to_string(),
        };
        !matches!(
            encode_item_value(item_type, &probe),
            Err(ItemWriteError::UnsupportedItemType { .. })
        )
    }

    #[test]
    fn write_accepts_the_documented_pairs() {
        for (item_type, value, encoded) in writable_pairs() {
            assert_eq!(
                encode_item_value(&item_type, &value),
                Ok(encoded.to_string()),
                "{item_type}"
            );
        }
    }

    #[test]
    fn write_rejects_unknown_value() {
        let value = ItemValue::Unknown {
            raw: "future=1".to_string(),
        };
        for item_type in writable_pairs()
            .into_iter()
            .map(|(item_type, _, _)| item_type)
            .chain(non_writable_item_types())
        {
            assert_eq!(
                encode_item_value(&item_type, &value),
                Err(ItemWriteError::UnknownValue),
                "{item_type}"
            );
        }
        assert_eq!(
            validate_item_value(&value),
            Err(ItemWriteError::UnknownValue)
        );
        // 設定項目の実在確認より先に拒否する。
        assert_eq!(
            prepare_item_write(&[], "存在しない項目", &value),
            Err(ItemWriteError::UnknownValue)
        );
    }

    #[test]
    fn write_rejects_value_kind_mismatch() {
        let mismatched = ItemValue::Text {
            value: "文字列".to_string(),
        };
        for (item_type, _, _) in writable_pairs() {
            if matches!(item_type, EffectItemType::Text | EffectItemType::String) {
                continue;
            }
            assert_eq!(
                encode_item_value(&item_type, &mismatched),
                Err(ItemWriteError::ValueKindMismatch {
                    item_type: item_type.kind_name(),
                    value_kind: "text",
                }),
                "{item_type}"
            );
        }
    }

    #[test]
    fn the_exposed_types_are_the_ones_declared_writable() {
        // 公開の範囲を、種別を網羅した宣言と突き合わせる。実装だけを直した
        // 場合も、宣言だけを直した場合も落ちる。
        for item_type in known_item_types()
            .into_iter()
            .chain([EffectItemType::Unknown(99)])
        {
            assert_eq!(
                is_exposed_for_write(&item_type),
                expects_writable(&item_type),
                "{item_type} の公開の可否が宣言と異なります"
            );
        }
    }

    #[test]
    fn every_known_item_type_is_listed_as_writable_or_not() {
        // 既知の種別が公開・非公開のどちらの一覧にも現れないまま検査を素通り
        // することを防ぐ。
        let writable: Vec<EffectItemType> = writable_pairs()
            .into_iter()
            .map(|(item_type, _, _)| item_type)
            .collect();
        let non_writable = non_writable_item_types();
        for item_type in known_item_types() {
            assert_eq!(
                writable.contains(&item_type),
                expects_writable(&item_type),
                "{item_type} が公開する種別の一覧と宣言で食い違います"
            );
            assert_eq!(
                non_writable.contains(&item_type),
                !expects_writable(&item_type),
                "{item_type} が公開しない種別の一覧と宣言で食い違います"
            );
        }
    }

    #[test]
    fn write_rejects_non_writable_item_types() {
        // 複合種別と未知種別は、値の形にかかわらず未対応として拒否する。
        let value = ItemValue::Text {
            value: "文字列".to_string(),
        };
        for item_type in non_writable_item_types() {
            assert_eq!(
                encode_item_value(&item_type, &value),
                Err(ItemWriteError::UnsupportedItemType {
                    item_type: item_type.kind_name(),
                }),
                "{item_type}"
            );
        }
    }

    #[test]
    fn write_separates_invalid_argument_from_unsupported_operation() {
        // 要求を直せば通るものと、直しても通らないものを取り違えない。
        assert_eq!(
            ItemWriteError::UnknownValue.error_code(),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ItemWriteError::ValueKindMismatch {
                item_type: "integer".to_string(),
                value_kind: "text",
            }
            .error_code(),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ItemWriteError::Text(TextSyntaxError::ContainsNul).error_code(),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ItemWriteError::Path(PathSyntaxError::NotAbsolute).error_code(),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ItemWriteError::UnsupportedItemType {
                item_type: "scene".to_string(),
            }
            .error_code(),
            ErrorCode::UnsupportedOperation
        );
        assert_eq!(
            ItemWriteError::ItemNotFound {
                item: "X".to_string(),
            }
            .error_code(),
            ErrorCode::NotFound
        );
    }

    #[test]
    fn write_ignores_the_choice_index() {
        // 選択肢の並びはホスト側の都合で変わり得るため、index を正としない。
        for item_type in [EffectItemType::Select, EffectItemType::Combo] {
            let encoded: Vec<String> = [Some(0), Some(7), None]
                .into_iter()
                .map(|index| {
                    encode_item_value(
                        &item_type,
                        &ItemValue::Choice {
                            value: "通常".to_string(),
                            index,
                        },
                    )
                    .unwrap()
                })
                .collect();
            assert_eq!(encoded, vec!["通常".to_string(); 3], "{item_type}");
        }
    }

    #[test]
    fn combo_shares_the_select_write_path() {
        // 表記が同じであることを、専用の分岐を持たないことで示す。同じ値に
        // 対して受理・拒否・変換結果のすべてが一致する。
        let cases = [
            ItemValue::Choice {
                value: "左寄せ[上]".to_string(),
                index: None,
            },
            ItemValue::Choice {
                value: "通常".to_string(),
                index: Some(3),
            },
            // 形が対応しない値。
            ItemValue::Text {
                value: "通常".to_string(),
            },
            // 単一行の文字列として拒否される値。
            ItemValue::Choice {
                value: "通常\n".to_string(),
                index: None,
            },
        ];
        for value in cases {
            let select = encode_item_value(&EffectItemType::Select, &value);
            let combo = encode_item_value(&EffectItemType::Combo, &value);
            // 種別名だけは異なるため、エラーはその点を除いて比べる。
            match (select, combo) {
                (Ok(select), Ok(combo)) => assert_eq!(select, combo, "{}", value.kind()),
                (Err(select), Err(combo)) => {
                    assert_eq!(select.error_code(), combo.error_code(), "{}", value.kind());
                    assert_eq!(
                        std::mem::discriminant(&select),
                        std::mem::discriminant(&combo),
                        "{}",
                        value.kind()
                    );
                }
                (select, combo) => {
                    panic!(
                        "{} で結果が分かれました: {select:?} / {combo:?}",
                        value.kind()
                    )
                }
            }
        }
    }

    #[test]
    fn write_rejects_nul_and_control_characters_in_strings() {
        for value in [
            ItemValue::Text {
                value: "字幕\0".to_string(),
            },
            ItemValue::Color {
                value: "#ff8800\0".to_string(),
            },
            ItemValue::Font {
                name: "Meiryo\0".to_string(),
            },
            ItemValue::Choice {
                value: "通常\0".to_string(),
                index: None,
            },
            ItemValue::File {
                path: "C:\\movie\0.mp4".to_string(),
            },
        ] {
            assert_eq!(
                validate_item_value(&value).unwrap_err().error_code(),
                ErrorCode::InvalidArgument,
                "{}",
                value.kind()
            );
        }

        // テキスト以外は改行もタブも受け付けない。
        for value in [
            ItemValue::Color {
                value: "#ff8800\n".to_string(),
            },
            ItemValue::Font {
                name: "Meiryo\n".to_string(),
            },
            ItemValue::Choice {
                value: "通常\t".to_string(),
                index: None,
            },
        ] {
            assert_eq!(
                validate_item_value(&value),
                Err(ItemWriteError::Text(TextSyntaxError::ContainsControl)),
                "{}",
                value.kind()
            );
        }
    }

    #[test]
    fn text_values_may_span_multiple_lines() {
        // 複数行のテキストを 1 回の書き込みで設定できる。
        let value = "1 行目\r\n2 行目\n\t字下げ".to_string();
        assert_eq!(
            encode_item_value(
                &EffectItemType::Text,
                &ItemValue::Text {
                    value: value.clone()
                }
            ),
            Ok(value.clone())
        );
        assert_eq!(
            encode_item_value(
                &EffectItemType::String,
                &ItemValue::Text {
                    value: value.clone()
                }
            ),
            Ok(value.clone())
        );
        assert_eq!(validate_item_value(&ItemValue::Text { value }), Ok(()));
    }

    #[test]
    fn text_values_still_reject_other_control_characters() {
        // 緩和するのは行の折り返しと字下げだけで、他の制御文字は通さない。
        for control in ['\0', '\u{1}', '\u{b}', '\u{1b}', '\u{7f}', '\u{9b}'] {
            let value = ItemValue::Text {
                value: format!("字幕{control}"),
            };
            assert!(
                validate_item_value(&value).is_err(),
                "{control:?} が受理されました"
            );
        }
    }

    #[test]
    fn write_rejects_strings_over_the_limit() {
        let value = "a".repeat(MAX_ITEM_VALUE_BYTES + 1);
        assert_eq!(
            validate_item_value(&ItemValue::Text {
                value: value.clone()
            }),
            Err(ItemWriteError::Text(TextSyntaxError::TooLongBytes {
                bytes: MAX_ITEM_VALUE_BYTES + 1,
                max: MAX_ITEM_VALUE_BYTES,
            }))
        );
        assert_eq!(
            validate_item_value(&ItemValue::Text {
                value: value[..MAX_ITEM_VALUE_BYTES].to_string(),
            }),
            Ok(())
        );
    }

    #[test]
    fn write_rejects_invalid_paths() {
        for (path, expected) in [
            ("", PathSyntaxError::Empty),
            (r"..\movie.mp4", PathSyntaxError::NotAbsolute),
            (r"\\.\PhysicalDrive0", PathSyntaxError::DeviceNamespace),
            (r"C:\movie.mp4:stream", PathSyntaxError::AlternateDataStream),
            (r"\\server\share\movie.mp4", PathSyntaxError::UncPath),
        ] {
            for value in [
                ItemValue::File {
                    path: path.to_string(),
                },
                ItemValue::Folder {
                    path: path.to_string(),
                },
            ] {
                assert_eq!(
                    validate_item_value(&value),
                    Err(ItemWriteError::Path(expected)),
                    "{path}"
                );
            }
        }
    }

    #[test]
    fn write_rejects_paths_over_the_setting_value_limit() {
        // パスの上限は UTF-16 code unit で数えるため設定値のバイト上限より
        // 緩く、パス側だけを見ると設定値の上限が効かなくなる。両方を課す。
        for path in [
            // ASCII だけでも設定値の上限を超えられる。
            format!(r"C:\{}", "a".repeat(MAX_ITEM_VALUE_BYTES)),
            // 多バイト文字ではパス上限に達する前に大きく超える。
            format!(r"C:\{}", "あ".repeat(MAX_ITEM_VALUE_BYTES / 3)),
        ] {
            let bytes = path.len();
            assert!(path.encode_utf16().count() <= MAX_PATH_UTF16_UNITS);
            for value in [
                ItemValue::File { path: path.clone() },
                ItemValue::Folder { path: path.clone() },
            ] {
                assert_eq!(
                    validate_item_value(&value),
                    Err(ItemWriteError::Text(TextSyntaxError::TooLongBytes {
                        bytes,
                        max: MAX_ITEM_VALUE_BYTES,
                    })),
                    "{} が受理されました",
                    value.kind()
                );
            }
        }
    }

    #[test]
    fn write_accepts_paths_within_both_limits() {
        let path = format!(r"C:\{}", "a".repeat(MAX_ITEM_VALUE_BYTES - 3));
        assert_eq!(path.len(), MAX_ITEM_VALUE_BYTES);
        assert_eq!(validate_item_value(&ItemValue::File { path }), Ok(()));
    }

    #[test]
    fn write_encodes_numbers_without_losing_the_value() {
        // 読み取りは十進表記を f64 として解釈する。書き込みが元の値へ戻せる
        // 表記を出さなければ、読み取った値をそのまま書き戻せない。
        for raw in [
            0.0,
            -0.0,
            1.0,
            0.1,
            12.5,
            29.97,
            -1.0 / 3.0,
            1e300,
            1e-300,
            f64::MAX,
            f64::MIN_POSITIVE,
        ] {
            let value = ItemValue::Number {
                value: FiniteF64::try_new(raw).unwrap(),
            };
            let encoded = encode_item_value(&EffectItemType::Number, &value).unwrap();
            assert!(
                !encoded.contains('e') && !encoded.contains('E'),
                "指数表記になりました: {encoded}"
            );
            // 0.0 と -0.0 は等値比較では区別できないため、ビット列で比べる。
            assert_eq!(
                encoded.trim().parse::<f64>().unwrap().to_bits(),
                raw.to_bits(),
                "{raw} が {encoded} になりました"
            );
        }
    }

    #[test]
    fn write_encodes_check_as_zero_or_one() {
        assert_eq!(
            encode_item_value(&EffectItemType::Check, &ItemValue::Bool { value: false }),
            Ok("0".to_string())
        );
        assert_eq!(
            encode_item_value(&EffectItemType::Check, &ItemValue::Bool { value: true }),
            Ok("1".to_string())
        );
    }

    #[test]
    fn prepare_item_write_looks_up_the_item_type() {
        let items = vec![
            AvailableEffectItem {
                name: "X".to_string(),
                item_type: EffectItemType::Number,
            },
            AvailableEffectItem {
                name: "図形".to_string(),
                item_type: EffectItemType::Figure,
            },
        ];

        assert_eq!(
            prepare_item_write(
                &items,
                "X",
                &ItemValue::Number {
                    value: FiniteF64::try_new(1.5).unwrap(),
                },
            ),
            Ok("1.5".to_string())
        );
        assert_eq!(
            prepare_item_write(
                &items,
                "Y",
                &ItemValue::Number {
                    value: FiniteF64::try_new(1.5).unwrap(),
                },
            ),
            Err(ItemWriteError::ItemNotFound {
                item: "Y".to_string(),
            })
        );
        assert_eq!(
            prepare_item_write(
                &items,
                "図形",
                &ItemValue::Text {
                    value: "円".to_string(),
                },
            ),
            Err(ItemWriteError::UnsupportedItemType {
                item_type: "figure".to_string(),
            })
        );
    }

    #[test]
    fn write_errors_do_not_repeat_the_value() {
        // 設定値そのものは応答へ反響させない。
        let secret = "秘密の値";
        let errors = [
            encode_item_value(
                &EffectItemType::Integer,
                &ItemValue::Text {
                    value: secret.to_string(),
                },
            )
            .unwrap_err(),
            encode_item_value(
                &EffectItemType::Scene,
                &ItemValue::Text {
                    value: secret.to_string(),
                },
            )
            .unwrap_err(),
            validate_item_value(&ItemValue::Text {
                value: format!("{secret}\0"),
            })
            .unwrap_err(),
            validate_item_value(&ItemValue::File {
                path: format!(r"..\{secret}"),
            })
            .unwrap_err(),
            validate_item_value(&ItemValue::Unknown {
                raw: secret.to_string(),
            })
            .unwrap_err(),
        ];
        for error in errors {
            assert!(
                !error.to_string().contains(secret),
                "値が含まれます: {error}"
            );
        }
    }
}
