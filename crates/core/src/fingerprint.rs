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
    /// 本 crate が生成する fingerprint の方式。
    ///
    /// 生成関数は方式を引数に取らない。alias を正規化しないまま
    /// [`FingerprintAlgorithm::NormalizedAliasV1`] を名乗るダイジェストを
    /// 作れないよう、方式を型で固定している。
    pub const GENERATED: Self = FingerprintAlgorithm::RawV1;

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

    /// 浮動小数点をビット表現で書く。
    ///
    /// 十進整形を経由すると標準ライブラリの整形規則の変更でダイジェストが
    /// 変わってしまうため、整形に依存しないビット列を用いる。負のゼロは
    /// 値として正のゼロと等しいので、ビット表現も正のゼロへ寄せる。
    fn number(&mut self, name: &str, value: FiniteF64) {
        let raw = value.get();
        let normalized = if raw == 0.0 { 0.0 } else { raw };
        self.field(name, &normalized.to_bits().to_le_bytes());
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

    /// 省略可能な件数を、存在フラグを伴って書く。
    fn optional_count(&mut self, name: &str, value: Option<usize>) {
        self.boolean(&format!("{name}.present"), value.is_some());
        if let Some(value) = value {
            self.count(name, value);
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

/// 設定値を、種別タグと種別ごとの値フィールドとして書く。
///
/// 種別を独立したフィールドとして先に書くため、異なる種別の同じ内容が
/// 同じバイト列になることがない。
fn write_item_value(input: &mut FingerprintInput, value: &ItemValue) {
    match value {
        ItemValue::Number { value } => {
            input.text("item_value.kind", "number");
            input.number("item_value.number", *value);
        }
        ItemValue::Integer { value } => {
            input.text("item_value.kind", "integer");
            input.integer("item_value.integer", *value);
        }
        ItemValue::Bool { value } => {
            input.text("item_value.kind", "bool");
            input.boolean("item_value.bool", *value);
        }
        ItemValue::Color { value } => {
            input.text("item_value.kind", "color");
            input.text("item_value.color", value);
        }
        ItemValue::Choice { value, index } => {
            input.text("item_value.kind", "choice");
            input.text("item_value.choice", value);
            input.optional_count("item_value.choice_index", *index);
        }
        ItemValue::File { path } => {
            input.text("item_value.kind", "file");
            input.text("item_value.file", path);
        }
        ItemValue::Folder { path } => {
            input.text("item_value.kind", "folder");
            input.text("item_value.folder", path);
        }
        ItemValue::Font { name } => {
            input.text("item_value.kind", "font");
            input.text("item_value.font", name);
        }
        ItemValue::Text { value } => {
            input.text("item_value.kind", "text");
            input.text("item_value.text", value);
        }
        ItemValue::Unknown { raw } => {
            input.text("item_value.kind", "unknown");
            input.text("item_value.unknown", raw);
        }
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

/// [`object_fingerprint`] の入力。
///
/// 同じ型の引数を並べて取り違えると、症状が「セレクターが解決できない」と
/// いう診断しづらい形で現れるため、名前付きの構造体で受け取る。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectFingerprintInput<'a> {
    /// シーン ID。
    pub scene_id: i32,
    /// 0 始まりのレイヤー番号。
    pub layer: usize,
    /// 0 始まりの開始フレーム番号。
    pub frame_start: usize,
    /// 0 始まりの終了フレーム番号。
    pub frame_end: usize,
    /// オブジェクト名。標準名のままなら None。
    pub name: Option<&'a str>,
    /// 正規化前の alias。
    pub alias: &'a str,
}

/// [`effect_fingerprint`] の入力。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectFingerprintInput<'a> {
    /// effect 名。
    pub effect_name: &'a str,
    /// 同名 effect のうち何番目か。0 始まり。
    pub effect_index: usize,
    /// effect が有効か。
    pub enabled: bool,
    /// effect がロックされているか。
    pub locked: bool,
    /// 設定項目と値。
    pub items: &'a [EffectItem],
}

/// オブジェクトの fingerprint を算出する。
///
/// 方式は [`FingerprintAlgorithm::GENERATED`] に固定される。同一入力に対して
/// 常に同一のダイジェストを返す。
///
/// ```
/// # use aviutl2_mcp_core::{ObjectFingerprintInput, object_fingerprint};
/// let input = ObjectFingerprintInput {
///     scene_id: 0,
///     layer: 2,
///     frame_start: 120,
///     frame_end: 240,
///     name: Some("立ち絵"),
///     alias: "alias",
/// };
/// let fingerprint = object_fingerprint(input);
/// assert!(fingerprint.as_str().starts_with("sha256:"));
/// assert_eq!(fingerprint, object_fingerprint(input));
/// ```
pub fn object_fingerprint(input: ObjectFingerprintInput<'_>) -> Fingerprint {
    let mut bytes = FingerprintInput::new();
    bytes.text("algorithm", FingerprintAlgorithm::GENERATED.as_str());
    bytes.integer("scene_id", i64::from(input.scene_id));
    bytes.count("layer", input.layer);
    bytes.count("frame_start", input.frame_start);
    bytes.count("frame_end", input.frame_end);
    bytes.optional_text("name", input.name);
    bytes.text("alias", input.alias);
    bytes.finish()
}

/// effect の fingerprint を算出する。
///
/// 方式は [`FingerprintAlgorithm::GENERATED`] に固定される。同一入力に対して
/// 常に同一のダイジェストを返す。
pub fn effect_fingerprint(input: EffectFingerprintInput<'_>) -> Fingerprint {
    let mut bytes = FingerprintInput::new();
    bytes.text("algorithm", FingerprintAlgorithm::GENERATED.as_str());
    bytes.text("effect_name", input.effect_name);
    bytes.count("effect_index", input.effect_index);
    bytes.boolean("enabled", input.enabled);
    bytes.boolean("locked", input.locked);
    bytes.count("item_count", input.items.len());
    for item in input.items {
        bytes.text("item_name", &item.name);
        bytes.integer("item_type", i64::from(item.item_type.as_raw()));
        write_item_value(&mut bytes, &item.value);
        write_track(&mut bytes, item.track.as_ref());
    }
    bytes.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::EffectItemType;

    /// 既定の位置情報で object fingerprint を算出する。
    fn object_at(name: Option<&str>, alias: &str) -> Fingerprint {
        object_fingerprint(ObjectFingerprintInput {
            scene_id: 0,
            layer: 2,
            frame_start: 120,
            frame_end: 240,
            name,
            alias,
        })
    }

    fn sample_items() -> Vec<EffectItem> {
        vec![EffectItem {
            name: "X".to_string(),
            item_type: EffectItemType::Number,
            value: ItemValue::Number {
                value: FiniteF64::try_new(12.5).unwrap(),
            },
            track: Some(sample_track("座標")),
        }]
    }

    fn sample_track(group_name: &str) -> TrackInfo {
        TrackInfo {
            mode: "直線移動".to_string(),
            params: vec![FiniteF64::try_new(0.5).unwrap()],
            accelerate: true,
            decelerate: false,
            twopoint: false,
            timecontrol: false,
            group_num: 2,
            group_index: 0,
            group_name: Some(group_name.to_string()),
        }
    }

    /// 既定の effect 属性で effect fingerprint を算出する。
    fn effect_with(items: &[EffectItem]) -> Fingerprint {
        effect_fingerprint(EffectFingerprintInput {
            effect_name: "動画ファイル",
            effect_index: 0,
            enabled: true,
            locked: false,
            items,
        })
    }

    #[test]
    fn fingerprint_has_canonical_form() {
        let fingerprint = object_at(Some("立ち絵"), "alias");
        let hex = fingerprint.as_str().strip_prefix("sha256:").unwrap();
        assert_eq!(hex.len(), 64);
        assert!(hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    }

    #[test]
    fn fingerprint_is_deterministic() {
        assert_eq!(
            object_at(Some("立ち絵"), "alias"),
            object_at(Some("立ち絵"), "alias")
        );
        assert_eq!(effect_with(&sample_items()), effect_with(&sample_items()));
    }

    #[test]
    fn object_fingerprint_distinguishes_none_from_empty_name() {
        assert_ne!(object_at(None, "alias"), object_at(Some(""), "alias"));
    }

    #[test]
    fn object_fingerprint_survives_field_name_absorption() {
        // name の直後に書く alias は、フィールド名が値の先頭と一致し得る。
        // 長さを前置しなければ次の 2 つはどちらも
        // "name" "x" "alias" "alias" "y" と並び、同じバイト列になる。
        assert_ne!(
            object_at(Some("x"), "aliasy"),
            object_at(Some("xalias"), "y")
        );
    }

    #[test]
    fn effect_fingerprint_survives_field_name_absorption() {
        // 1 つ目の項目の track.group_name の直後には 2 つ目の項目の
        // item_name が続く。長さを前置しなければ次の 2 つはどちらも
        // "track.group_name" "g" "item_name" "item_name" "n" と並ぶ。
        let items = |group_name: &str, second_name: &str| {
            vec![
                EffectItem {
                    name: "a".to_string(),
                    item_type: EffectItemType::Text,
                    value: ItemValue::Text {
                        value: "v".to_string(),
                    },
                    track: Some(sample_track(group_name)),
                },
                EffectItem {
                    name: second_name.to_string(),
                    item_type: EffectItemType::Text,
                    value: ItemValue::Text {
                        value: "w".to_string(),
                    },
                    track: None,
                },
            ]
        };
        assert_ne!(
            effect_with(&items("g", "item_namen")),
            effect_with(&items("gitem_name", "n"))
        );
    }

    #[test]
    fn object_fingerprint_depends_on_every_input() {
        let base = ObjectFingerprintInput {
            scene_id: 0,
            layer: 2,
            frame_start: 120,
            frame_end: 240,
            name: Some("立ち絵"),
            alias: "alias",
        };
        for changed in [
            ObjectFingerprintInput {
                scene_id: 1,
                ..base
            },
            ObjectFingerprintInput { layer: 3, ..base },
            ObjectFingerprintInput {
                frame_start: 121,
                ..base
            },
            ObjectFingerprintInput {
                frame_end: 241,
                ..base
            },
            ObjectFingerprintInput {
                name: Some("座り絵"),
                ..base
            },
            ObjectFingerprintInput {
                alias: "alias2",
                ..base
            },
        ] {
            assert_ne!(object_fingerprint(base), object_fingerprint(changed));
        }
    }

    #[test]
    fn effect_fingerprint_depends_on_every_input() {
        let items = sample_items();
        let base = EffectFingerprintInput {
            effect_name: "動画ファイル",
            effect_index: 0,
            enabled: true,
            locked: false,
            items: &items,
        };
        for changed in [
            EffectFingerprintInput {
                effect_name: "別の effect",
                ..base
            },
            EffectFingerprintInput {
                effect_index: 1,
                ..base
            },
            EffectFingerprintInput {
                enabled: false,
                ..base
            },
            EffectFingerprintInput {
                locked: true,
                ..base
            },
            EffectFingerprintInput { items: &[], ..base },
        ] {
            assert_ne!(effect_fingerprint(base), effect_fingerprint(changed));
        }
    }

    #[test]
    fn effect_fingerprint_distinguishes_missing_track() {
        let mut without_track = sample_items();
        without_track[0].track = None;
        assert_ne!(effect_with(&sample_items()), effect_with(&without_track));
    }

    #[test]
    fn effect_fingerprint_distinguishes_item_value_variants() {
        let make = |value: ItemValue| {
            effect_with(&[EffectItem {
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
    fn effect_fingerprint_treats_signed_zero_as_equal() {
        // FiniteF64 の等値判定と一致させるため、正負のゼロは同じ扱いにする。
        let make = |value: f64| {
            effect_with(&[EffectItem {
                name: "項目".to_string(),
                item_type: EffectItemType::Number,
                value: ItemValue::Number {
                    value: FiniteF64::try_new(value).unwrap(),
                },
                track: None,
            }])
        };
        assert_eq!(make(0.0), make(-0.0));
        assert_ne!(make(0.0), make(1.0));
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
        let fingerprint = object_at(Some("立ち絵"), "alias");
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
    fn fingerprint_algorithm_known_names_never_become_unknown() {
        // 既知の方式名は Unknown に落ちないため、名前が一致するのに
        // variant が食い違う値が逆直列化から生まれることはない。
        for algorithm in [
            FingerprintAlgorithm::NormalizedAliasV1,
            FingerprintAlgorithm::RawV1,
        ] {
            let restored: FingerprintAlgorithm =
                serde_json::from_str(&format!("\"{}\"", algorithm.as_str())).unwrap();
            assert_eq!(restored, algorithm);
            assert_ne!(
                restored,
                FingerprintAlgorithm::Unknown(algorithm.as_str().to_string())
            );
        }
    }

    #[test]
    fn generated_algorithm_is_raw_v1() {
        // 生成関数は方式を引数に取らず、常にこの方式で算出する。
        assert_eq!(FingerprintAlgorithm::GENERATED, FingerprintAlgorithm::RawV1);
        assert_eq!(FingerprintAlgorithm::GENERATED.as_str(), "sha256-raw-v1");
    }

    #[test]
    fn to_hex_is_lowercase_and_padded() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }
}
