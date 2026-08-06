//! 対象の同一性を検証する fingerprint。
//!
//! 入力を曖昧さのない正準バイト列へ組み立て、SHA-256 ダイジェストを
//! `"sha256:" + 64 桁小文字十六進` として表現する。

use crate::digest::{SHA256_HEX_LEN, SHA256_PREFIX, format_sha256};
use crate::effect::{EffectItem, TrackInfo};
use crate::item_value::ItemValue;
use crate::number::FiniteF64;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

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
///
/// 前置と桁数はダイジェストの表現に共通のものを引く。**同じ形を用いることは、
/// fingerprint と他のダイジェストを同じものとして扱うことではない。**
fn is_canonical(value: &str) -> bool {
    let Some(hex) = value.strip_prefix(SHA256_PREFIX) else {
        return false;
    };
    hex.len() == SHA256_HEX_LEN && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// fingerprint の算出方式。
///
/// ワイヤ表現を持たない。要求も応答も方式を運ばず、方式は
/// [`object_fingerprint`] / [`effect_fingerprint`] がダイジェストの材料として
/// 書き込むためだけに存在する。**方式が変われば同じ対象でもダイジェストが変わる
/// ため、方式の食い違いは fingerprint の照合が捕まえる。** 方式を運ぶ必要が
/// 無いのは、この性質による。
///
/// 生成関数は方式を引数に取らない。alias を正規化しないまま正規化を名乗る
/// ダイジェストを作れないよう、方式を型で固定している。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FingerprintAlgorithm {
    /// alias の生文字列と位置情報から算出する。
    RawV1,
}

impl FingerprintAlgorithm {
    /// 方式名を返す。ダイジェストの材料に書き込む値である。
    pub fn as_str(&self) -> &'static str {
        match self {
            FingerprintAlgorithm::RawV1 => "sha256-raw-v1",
        }
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

    fn finish(self) -> Fingerprint {
        let mut hasher = Sha256::new();
        hasher.update(&self.buffer);
        Fingerprint(format_sha256(&hasher.finalize()))
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
        ItemValue::Choice { value } => {
            input.text("item_value.kind", "choice");
            input.text("item_value.choice", value);
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
        ItemValue::Track(track) => {
            input.text("item_value.kind", "track");
            input.count("item_value.track.value_count", track.values.len());
            for value in &track.values {
                input.number("item_value.track.value", *value);
            }
            input.optional_text("item_value.track.mode", track.mode.as_deref());
            input.count("item_value.track.param_count", track.params.len());
            for param in &track.params {
                input.number("item_value.track.param", *param);
            }
            input.boolean("item_value.track.accelerate", track.accelerate);
            input.boolean("item_value.track.decelerate", track.decelerate);
            input.boolean("item_value.track.twopoint", track.twopoint);
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
    ///
    /// alias は配下 effect の設定値を含む。effect だけを変えた場合も alias が
    /// 追随するため、effect を独立した材料として混ぜる必要はない。
    pub alias: &'a str,
}

/// [`effect_fingerprint`] の入力。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectFingerprintInput<'a> {
    /// effect 名。
    pub effect_name: &'a str,
    /// 同名 effect のうち何番目か。0 始まり。
    pub effect_index: usize,
    /// effect 列全体での位置。0 始まり。
    ///
    /// 同名 effect の何番目かとは別に、列の絶対位置も材料にする。
    pub position: usize,
    /// オブジェクトに付与された effect の総数。
    ///
    /// 前方の同名 effect が取り除かれると、後続の同名 effect は同名内の番号が
    /// 繰り上がる。繰り上がった側の設定が元と同じ場合、名前と同名内の番号だけ
    /// では取り除く前の値と一致してしまい、別の effect を同じものとして扱って
    /// しまう。取り除けば総数は必ず変わるため、総数を混ぜることで区別できる。
    pub effect_count: usize,
    /// effect が有効か。
    pub enabled: bool,
    /// effect がロックされているか。
    pub locked: bool,
    /// 設定項目と値。
    pub items: &'a [EffectItem],
}

/// オブジェクトの fingerprint を算出する。
///
/// 方式は [`FingerprintAlgorithm::RawV1`] に固定される。同一入力に対して
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
    bytes.text("algorithm", FingerprintAlgorithm::RawV1.as_str());
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
/// 方式は [`FingerprintAlgorithm::RawV1`] に固定される。同一入力に対して
/// 常に同一のダイジェストを返す。
pub fn effect_fingerprint(input: EffectFingerprintInput<'_>) -> Fingerprint {
    let mut bytes = FingerprintInput::new();
    bytes.text("algorithm", FingerprintAlgorithm::RawV1.as_str());
    bytes.text("effect_name", input.effect_name);
    bytes.count("effect_index", input.effect_index);
    bytes.count("position", input.position);
    bytes.count("effect_count", input.effect_count);
    bytes.boolean("enabled", input.enabled);
    bytes.boolean("locked", input.locked);
    bytes.count("item_count", input.items.len());
    for item in input.items {
        bytes.text("item_name", &item.name);
        // 種別値ではなく種別の名前を書く。値で書くと、既知値と同じ raw を持つ
        // 未知種別が既知種別と同じバイト列になり、等しくない値が同じ
        // ダイジェストへ潰れる。
        bytes.text("item_type", &item.item_type.kind_name());
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
            position: 0,
            effect_count: 1,
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
            position: 1,
            effect_count: 3,
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
                position: 2,
                ..base
            },
            EffectFingerprintInput {
                effect_count: 4,
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
    fn effect_fingerprint_detects_an_index_shift() {
        // 同名 effect が 2 つ並び、前方の 1 つが取り除かれた状況を作る。
        // 残った側は同名内の番号が 1 から 0 へ繰り上がり、設定が同じなら
        // 取り除く前の先頭と名前・番号・設定が全て一致する。列の総数と位置を
        // 材料に含めることで、両者を別の effect として区別する。
        let items = sample_items();
        let before = EffectFingerprintInput {
            effect_name: "ぼかし",
            effect_index: 0,
            position: 0,
            effect_count: 2,
            enabled: true,
            locked: false,
            items: &items,
        };
        let after = EffectFingerprintInput {
            effect_count: 1,
            ..before
        };
        assert_ne!(effect_fingerprint(before), effect_fingerprint(after));
    }

    #[test]
    fn effect_fingerprint_distinguishes_item_types() {
        let make = |item_type: EffectItemType| {
            effect_with(&[EffectItem {
                name: "項目".to_string(),
                item_type,
                value: ItemValue::Unknown {
                    raw: "v".to_string(),
                },
                track: None,
            }])
        };

        // 既知種別と、同じ raw 値を持つ未知種別は等しくない値であり、
        // ダイジェストも異なる。
        assert_ne!(EffectItemType::Unknown(2), EffectItemType::Number);
        assert_eq!(
            EffectItemType::Unknown(2).as_raw(),
            EffectItemType::Number.as_raw()
        );
        assert_ne!(
            make(EffectItemType::Unknown(2)),
            make(EffectItemType::Number)
        );

        assert_ne!(
            make(EffectItemType::Integer),
            make(EffectItemType::Number),
            "既知種別どうしが同じダイジェストになりました"
        );
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
    }

    #[test]
    fn effect_fingerprint_distinguishes_every_part_of_a_movement() {
        // 独立に生成した 2 つを比べる property test では、1 つのフィールドだけが
        // 違う組がほとんど現れない。材料から 1 つ落としても気付けないため、
        // 差分を 1 つずつ作って突き合わせる。
        let base = crate::track_value::TrackValue {
            values: vec![
                FiniteF64::try_new(0.0).unwrap(),
                FiniteF64::try_new(100.0).unwrap(),
            ],
            mode: Some("直線移動".to_string()),
            params: vec![FiniteF64::try_new(15.0).unwrap()],
            accelerate: false,
            decelerate: false,
            twopoint: false,
        };
        let make = |track: crate::track_value::TrackValue| {
            effect_with(&[EffectItem {
                name: "項目".to_string(),
                item_type: EffectItemType::Number,
                value: ItemValue::Track(track),
                track: None,
            }])
        };
        let variants = [
            crate::track_value::TrackValue {
                values: vec![
                    FiniteF64::try_new(0.0).unwrap(),
                    FiniteF64::try_new(50.0).unwrap(),
                ],
                ..base.clone()
            },
            crate::track_value::TrackValue {
                mode: Some("曲線移動".to_string()),
                ..base.clone()
            },
            crate::track_value::TrackValue {
                mode: None,
                ..base.clone()
            },
            crate::track_value::TrackValue {
                params: vec![FiniteF64::try_new(30.0).unwrap()],
                ..base.clone()
            },
            crate::track_value::TrackValue {
                accelerate: true,
                ..base.clone()
            },
            crate::track_value::TrackValue {
                decelerate: true,
                ..base.clone()
            },
            crate::track_value::TrackValue {
                twopoint: true,
                ..base.clone()
            },
            // 要素数だけが違う組。内容が違う組だけを並べると、並びの長さの
            // 違いが digest に現れるかを 1 度も試さないまま通る。
            crate::track_value::TrackValue {
                values: vec![
                    FiniteF64::try_new(0.0).unwrap(),
                    FiniteF64::try_new(100.0).unwrap(),
                    FiniteF64::try_new(100.0).unwrap(),
                ],
                ..base.clone()
            },
            crate::track_value::TrackValue {
                params: vec![
                    FiniteF64::try_new(15.0).unwrap(),
                    FiniteF64::try_new(15.0).unwrap(),
                ],
                ..base.clone()
            },
            crate::track_value::TrackValue {
                params: Vec::new(),
                ..base.clone()
            },
        ];
        let baseline = make(base.clone());
        for variant in variants {
            assert_ne!(
                make(variant.clone()),
                baseline,
                "{variant:?} が材料に入っていません"
            );
        }
        // 同じ値は同じ digest になる。差分の検出が「毎回違う」ではないことを示す。
        assert_eq!(make(base.clone()), baseline);
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
    fn fingerprint_uses_the_shared_digest_form() {
        let fingerprint = object_at(Some("立ち絵"), "alias");
        assert!(fingerprint.as_str().starts_with(SHA256_PREFIX));
        assert_eq!(
            fingerprint.as_str().len(),
            SHA256_PREFIX.len() + SHA256_HEX_LEN
        );
    }

    /// 方式だけを差し替えて object の材料を組み立て直す。
    fn object_digest_with(
        algorithm: Option<&str>,
        input: ObjectFingerprintInput<'_>,
    ) -> Fingerprint {
        let mut bytes = FingerprintInput::new();
        if let Some(algorithm) = algorithm {
            bytes.text("algorithm", algorithm);
        }
        bytes.integer("scene_id", i64::from(input.scene_id));
        bytes.count("layer", input.layer);
        bytes.count("frame_start", input.frame_start);
        bytes.count("frame_end", input.frame_end);
        bytes.optional_text("name", input.name);
        bytes.text("alias", input.alias);
        bytes.finish()
    }

    /// 方式だけを差し替えて effect の材料を組み立て直す。設定項目は持たせない。
    fn effect_digest_with(
        algorithm: Option<&str>,
        input: EffectFingerprintInput<'_>,
    ) -> Fingerprint {
        let mut bytes = FingerprintInput::new();
        if let Some(algorithm) = algorithm {
            bytes.text("algorithm", algorithm);
        }
        bytes.text("effect_name", input.effect_name);
        bytes.count("effect_index", input.effect_index);
        bytes.count("position", input.position);
        bytes.count("effect_count", input.effect_count);
        bytes.boolean("enabled", input.enabled);
        bytes.boolean("locked", input.locked);
        bytes.count("item_count", 0);
        bytes.finish()
    }

    #[test]
    fn the_algorithm_is_a_material_of_every_digest() {
        // 方式はワイヤに現れないが、ダイジェストの材料として書き込まれる。
        // 方式が変われば同じ対象でもダイジェストが変わるため、方式の食い違いは
        // fingerprint の照合が捕まえる。材料から外せば、この保護が消える。
        let object = ObjectFingerprintInput {
            scene_id: 0,
            layer: 2,
            frame_start: 120,
            frame_end: 240,
            name: Some("立ち絵"),
            alias: "alias",
        };
        let generated = FingerprintAlgorithm::RawV1.as_str();
        assert_eq!(
            object_fingerprint(object),
            object_digest_with(Some(generated), object)
        );
        assert_ne!(
            object_fingerprint(object),
            object_digest_with(Some("sha256-alias-v1"), object)
        );
        assert_ne!(object_fingerprint(object), object_digest_with(None, object));

        let effect = EffectFingerprintInput {
            effect_name: "動画ファイル",
            effect_index: 0,
            position: 0,
            effect_count: 1,
            enabled: true,
            locked: false,
            items: &[],
        };
        assert_eq!(
            effect_fingerprint(effect),
            effect_digest_with(Some(generated), effect)
        );
        assert_ne!(
            effect_fingerprint(effect),
            effect_digest_with(Some("sha256-alias-v1"), effect)
        );
        assert_ne!(effect_fingerprint(effect), effect_digest_with(None, effect));
    }
}
