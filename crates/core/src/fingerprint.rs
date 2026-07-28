//! 対象の同一性を検証する fingerprint。
//!
//! 入力を曖昧さのない正準バイト列へ組み立て、SHA-256 ダイジェストを
//! `"sha256:" + 64 桁小文字十六進` として表現する。

use crate::effect::{EffectItem, TrackInfo};
use crate::item_value::ItemValue;
use crate::number::FiniteF64;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

/// ダイジェストの前置文字列。
const FINGERPRINT_PREFIX: &str = "sha256:";

/// 十六進表現の桁数。
const DIGEST_HEX_LEN: usize = 64;

/// 対象の同一性検証に用いるダイジェスト。
///
/// 表現は `"sha256:" + 64 桁の小文字十六進`。逆直列化ではこの形式を検証する。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fingerprint(String);

impl Fingerprint {
    /// 文字列表現を返す。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// fingerprint の書式違反。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("fingerprint は \"sha256:\" と 64 桁の小文字十六進で構成される必要があります")]
pub struct FingerprintFormatError;

impl FromStr for Fingerprint {
    type Err = FingerprintFormatError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if is_canonical(value) {
            Ok(Fingerprint(value.to_string()))
        } else {
            Err(FingerprintFormatError)
        }
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Fingerprint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Fingerprint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// 正準表現かどうかを判定する。
fn is_canonical(value: &str) -> bool {
    let Some(hex) = value.strip_prefix(FINGERPRINT_PREFIX) else {
        return false;
    };
    hex.len() == DIGEST_HEX_LEN && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// バイト列を小文字十六進へ変換する。
fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// fingerprint の算出方式。
///
/// 応答へ含めて算出方式を明示し、再計算時に同じ方式を選べるようにする。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FingerprintAlgorithm {
    /// alias を正規化した構造から算出する。
    NormalizedAliasV1,
    /// alias の生文字列と位置情報から算出する。
    RawV1,
    /// 未知の方式名を破棄せず raw 保持。
    Unknown(String),
}

impl FingerprintAlgorithm {
    /// 方式名を返す。
    pub fn as_str(&self) -> &str {
        match self {
            FingerprintAlgorithm::NormalizedAliasV1 => "sha256-alias-v1",
            FingerprintAlgorithm::RawV1 => "sha256-raw-v1",
            FingerprintAlgorithm::Unknown(name) => name,
        }
    }
}

impl Serialize for FingerprintAlgorithm {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FingerprintAlgorithm {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let name = String::deserialize(deserializer)?;
        Ok(match name.as_str() {
            "sha256-alias-v1" => FingerprintAlgorithm::NormalizedAliasV1,
            "sha256-raw-v1" => FingerprintAlgorithm::RawV1,
            _ => FingerprintAlgorithm::Unknown(name),
        })
    }
}

impl fmt::Display for FingerprintAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 正準バイト列を組み立てる。
///
/// 各フィールドは `u64 LE の名前長 || 名前 || u64 LE の値長 || 値` として
/// 追加順に連結する。長さを前置するため、区切り文字の混入や隣接フィールドの
/// 境界の曖昧さでダイジェストが衝突することがない。
struct FingerprintInput {
    buffer: Vec<u8>,
}

impl FingerprintInput {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    fn field(&mut self, name: &str, value: &[u8]) {
        self.buffer
            .extend_from_slice(&(name.len() as u64).to_le_bytes());
        self.buffer.extend_from_slice(name.as_bytes());
        self.buffer
            .extend_from_slice(&(value.len() as u64).to_le_bytes());
        self.buffer.extend_from_slice(value);
    }

    fn text(&mut self, name: &str, value: &str) {
        self.field(name, value.as_bytes());
    }

    fn integer(&mut self, name: &str, value: i64) {
        self.text(name, &value.to_string());
    }

    fn count(&mut self, name: &str, value: usize) {
        self.text(name, &value.to_string());
    }

    fn boolean(&mut self, name: &str, value: bool) {
        self.text(name, if value { "true" } else { "false" });
    }

    fn number(&mut self, name: &str, value: FiniteF64) {
        self.text(name, &value.to_string());
    }

    /// 省略可能な文字列を、存在フラグを伴って書く。
    ///
    /// 値が無い場合と空文字列の場合で異なるバイト列になる。
    fn optional_text(&mut self, name: &str, value: Option<&str>) {
        self.boolean(&format!("{name}.present"), value.is_some());
        if let Some(value) = value {
            self.text(name, value);
        }
    }

    fn finish(self) -> Fingerprint {
        let mut hasher = Sha256::new();
        hasher.update(&self.buffer);
        Fingerprint(format!(
            "{FINGERPRINT_PREFIX}{}",
            to_hex(&hasher.finalize())
        ))
    }
}

/// 設定値を曖昧さのない文字列へ正規化する。
fn canonical_item_value(value: &ItemValue) -> String {
    match value {
        ItemValue::Number { value } => format!("number:{value}"),
        ItemValue::Integer { value } => format!("integer:{value}"),
        ItemValue::Bool { value } => format!("bool:{value}"),
        ItemValue::Color { value } => format!("color:{value}"),
        // index は数字のみで構成されるため、最初の ":" までが index、
        // 空なら未特定であると一意に読み取れる。
        ItemValue::Choice { value, index } => match index {
            Some(index) => format!("choice:{index}:{value}"),
            None => format!("choice::{value}"),
        },
        ItemValue::File { path } => format!("file:{path}"),
        ItemValue::Folder { path } => format!("folder:{path}"),
        ItemValue::Font { name } => format!("font:{name}"),
        ItemValue::Text { value } => format!("text:{value}"),
        ItemValue::Unknown { raw } => format!("unknown:{raw}"),
    }
}

/// トラックバー情報を存在フラグ付きで書く。
fn write_track(input: &mut FingerprintInput, track: Option<&TrackInfo>) {
    input.boolean("track.present", track.is_some());
    let Some(track) = track else {
        return;
    };
    input.text("track.mode", &track.mode);
    input.count("track.param_count", track.params.len());
    for param in &track.params {
        input.number("track.param", *param);
    }
    input.boolean("track.accelerate", track.accelerate);
    input.boolean("track.decelerate", track.decelerate);
    input.boolean("track.twopoint", track.twopoint);
    input.boolean("track.timecontrol", track.timecontrol);
    input.count("track.group_num", track.group_num);
    input.count("track.group_index", track.group_index);
    input.optional_text("track.group_name", track.group_name.as_deref());
}

/// オブジェクトの fingerprint を算出する。
///
/// 同一入力に対して常に同一のダイジェストを返す。
///
/// ```
/// # use aviutl2_mcp_core::{FingerprintAlgorithm, object_fingerprint};
/// let algorithm = FingerprintAlgorithm::RawV1;
/// let fingerprint = object_fingerprint(&algorithm, 0, 2, 120, 240, Some("立ち絵"), "alias");
/// assert!(fingerprint.as_str().starts_with("sha256:"));
/// assert_eq!(
///     fingerprint,
///     object_fingerprint(&algorithm, 0, 2, 120, 240, Some("立ち絵"), "alias")
/// );
/// ```
pub fn object_fingerprint(
    algorithm: &FingerprintAlgorithm,
    scene_id: i32,
    layer: usize,
    frame_start: usize,
    frame_end: usize,
    name: Option<&str>,
    alias: &str,
) -> Fingerprint {
    let mut input = FingerprintInput::new();
    input.text("algorithm", algorithm.as_str());
    input.integer("scene_id", i64::from(scene_id));
    input.count("layer", layer);
    input.count("frame_start", frame_start);
    input.count("frame_end", frame_end);
    input.optional_text("name", name);
    input.text("alias", alias);
    input.finish()
}

/// effect の fingerprint を算出する。
///
/// 同一入力に対して常に同一のダイジェストを返す。
pub fn effect_fingerprint(
    algorithm: &FingerprintAlgorithm,
    effect_name: &str,
    effect_index: usize,
    enabled: bool,
    locked: bool,
    items: &[EffectItem],
) -> Fingerprint {
    let mut input = FingerprintInput::new();
    input.text("algorithm", algorithm.as_str());
    input.text("effect_name", effect_name);
    input.count("effect_index", effect_index);
    input.boolean("enabled", enabled);
    input.boolean("locked", locked);
    input.count("item_count", items.len());
    for item in items {
        input.text("item_name", &item.name);
        input.integer("item_type", i64::from(item.item_type.as_raw()));
        input.text("item_value", &canonical_item_value(&item.value));
        write_track(&mut input, item.track.as_ref());
    }
    input.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::EffectItemType;

    fn raw_object_fingerprint(name: Option<&str>, alias: &str) -> Fingerprint {
        object_fingerprint(&FingerprintAlgorithm::RawV1, 0, 2, 120, 240, name, alias)
    }

    fn sample_items() -> Vec<EffectItem> {
        vec![EffectItem {
            name: "X".to_string(),
            item_type: EffectItemType::Number,
            value: ItemValue::Number {
                value: FiniteF64::try_new(12.5).unwrap(),
            },
            track: Some(TrackInfo {
                mode: "直線移動".to_string(),
                params: vec![FiniteF64::try_new(0.5).unwrap()],
                accelerate: true,
                decelerate: false,
                twopoint: false,
                timecontrol: false,
                group_num: 2,
                group_index: 0,
                group_name: Some("座標".to_string()),
            }),
        }]
    }

    fn raw_effect_fingerprint(items: &[EffectItem]) -> Fingerprint {
        effect_fingerprint(
            &FingerprintAlgorithm::RawV1,
            "動画ファイル",
            0,
            true,
            false,
            items,
        )
    }

    #[test]
    fn fingerprint_has_canonical_form() {
        let fingerprint = raw_object_fingerprint(Some("立ち絵"), "alias");
        let hex = fingerprint.as_str().strip_prefix("sha256:").unwrap();
        assert_eq!(hex.len(), 64);
        assert!(hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    }

    #[test]
    fn fingerprint_is_deterministic() {
        assert_eq!(
            raw_object_fingerprint(Some("立ち絵"), "alias"),
            raw_object_fingerprint(Some("立ち絵"), "alias")
        );
        assert_eq!(
            raw_effect_fingerprint(&sample_items()),
            raw_effect_fingerprint(&sample_items())
        );
    }

    #[test]
    fn object_fingerprint_distinguishes_none_from_empty_name() {
        assert_ne!(
            raw_object_fingerprint(None, "alias"),
            raw_object_fingerprint(Some(""), "alias")
        );
    }

    #[test]
    fn object_fingerprint_distinguishes_field_boundaries() {
        // 隣接フィールドの内容を移し替えても、長さ前置により別のダイジェストになる。
        assert_ne!(
            raw_object_fingerprint(Some("ab"), "cd"),
            raw_object_fingerprint(Some("abc"), "d")
        );
    }

    #[test]
    fn object_fingerprint_depends_on_every_input() {
        let base = raw_object_fingerprint(Some("立ち絵"), "alias");
        assert_ne!(
            base,
            object_fingerprint(
                &FingerprintAlgorithm::NormalizedAliasV1,
                0,
                2,
                120,
                240,
                Some("立ち絵"),
                "alias"
            )
        );
        for changed in [
            object_fingerprint(
                &FingerprintAlgorithm::RawV1,
                1,
                2,
                120,
                240,
                Some("立ち絵"),
                "alias",
            ),
            object_fingerprint(
                &FingerprintAlgorithm::RawV1,
                0,
                3,
                120,
                240,
                Some("立ち絵"),
                "alias",
            ),
            object_fingerprint(
                &FingerprintAlgorithm::RawV1,
                0,
                2,
                121,
                240,
                Some("立ち絵"),
                "alias",
            ),
            object_fingerprint(
                &FingerprintAlgorithm::RawV1,
                0,
                2,
                120,
                241,
                Some("立ち絵"),
                "alias",
            ),
            raw_object_fingerprint(Some("座り絵"), "alias"),
            raw_object_fingerprint(Some("立ち絵"), "alias2"),
        ] {
            assert_ne!(base, changed);
        }
    }

    #[test]
    fn effect_fingerprint_depends_on_every_input() {
        let base = raw_effect_fingerprint(&sample_items());
        for changed in [
            effect_fingerprint(
                &FingerprintAlgorithm::RawV1,
                "別の effect",
                0,
                true,
                false,
                &sample_items(),
            ),
            effect_fingerprint(
                &FingerprintAlgorithm::RawV1,
                "動画ファイル",
                1,
                true,
                false,
                &sample_items(),
            ),
            effect_fingerprint(
                &FingerprintAlgorithm::RawV1,
                "動画ファイル",
                0,
                false,
                false,
                &sample_items(),
            ),
            effect_fingerprint(
                &FingerprintAlgorithm::RawV1,
                "動画ファイル",
                0,
                true,
                true,
                &sample_items(),
            ),
            raw_effect_fingerprint(&[]),
        ] {
            assert_ne!(base, changed);
        }
    }

    #[test]
    fn effect_fingerprint_distinguishes_missing_track() {
        let mut without_track = sample_items();
        without_track[0].track = None;
        assert_ne!(
            raw_effect_fingerprint(&sample_items()),
            raw_effect_fingerprint(&without_track)
        );
    }

    #[test]
    fn effect_fingerprint_distinguishes_item_value_variants() {
        let make = |value: ItemValue| {
            raw_effect_fingerprint(&[EffectItem {
                name: "項目".to_string(),
                item_type: EffectItemType::Text,
                value,
                track: None,
            }])
        };
        assert_ne!(
            make(ItemValue::Text {
                value: "x".to_string()
            }),
            make(ItemValue::Unknown {
                raw: "x".to_string()
            })
        );
        assert_ne!(
            make(ItemValue::Choice {
                value: "x".to_string(),
                index: None
            }),
            make(ItemValue::Choice {
                value: "x".to_string(),
                index: Some(0)
            })
        );
    }

    #[test]
    fn fingerprint_parses_canonical_form() {
        let text = format!("sha256:{}", "a1".repeat(32));
        let fingerprint: Fingerprint = text.parse().unwrap();
        assert_eq!(fingerprint.as_str(), text);
        assert_eq!(fingerprint.to_string(), text);
    }

    #[test]
    fn fingerprint_rejects_invalid_form() {
        for text in [
            "",
            "sha256:",
            &"a1".repeat(32),
            &format!("sha1:{}", "a1".repeat(32)),
            &format!("sha256:{}", "a1".repeat(31)),
            &format!("sha256:{}", "a1".repeat(33)),
            &format!("sha256:{}", "A1".repeat(32)),
            &format!("sha256:{}", "g1".repeat(32)),
        ] {
            assert_eq!(text.parse::<Fingerprint>(), Err(FingerprintFormatError));
        }
    }

    #[test]
    fn fingerprint_json_roundtrip() {
        let fingerprint = raw_object_fingerprint(Some("立ち絵"), "alias");
        let s = serde_json::to_string(&fingerprint).unwrap();
        assert_eq!(s, format!("\"{fingerprint}\""));
        let restored: Fingerprint = serde_json::from_str(&s).unwrap();
        assert_eq!(restored, fingerprint);
    }

    #[test]
    fn fingerprint_deserialize_rejects_invalid_form() {
        let result: Result<Fingerprint, _> = serde_json::from_str("\"sha256:zz\"");
        assert!(result.is_err());
    }

    #[test]
    fn fingerprint_algorithm_roundtrip() {
        for algorithm in [
            FingerprintAlgorithm::NormalizedAliasV1,
            FingerprintAlgorithm::RawV1,
        ] {
            let s = serde_json::to_string(&algorithm).unwrap();
            assert_eq!(s, format!("\"{algorithm}\""));
            let restored: FingerprintAlgorithm = serde_json::from_str(&s).unwrap();
            assert_eq!(restored, algorithm);
        }
    }

    #[test]
    fn fingerprint_algorithm_unknown_preserved() {
        let algorithm: FingerprintAlgorithm = serde_json::from_str("\"sha256-future-v9\"").unwrap();
        assert_eq!(
            algorithm,
            FingerprintAlgorithm::Unknown("sha256-future-v9".to_string())
        );
        assert_eq!(
            serde_json::to_string(&algorithm).unwrap(),
            "\"sha256-future-v9\""
        );
    }

    #[test]
    fn to_hex_is_lowercase_and_padded() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }
}
