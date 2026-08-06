//! property-based テスト。

use crate::batch::{ApplyBatchParams, BatchOperation, MAX_BATCH_OPERATIONS};
use crate::edit::{
    AddEffectParams, CreateObjectParams, DeleteEffectParams, DeleteObjectParams, Destination,
    MoveObjectParams, SetEffectEnabledParams, SetObjectItemParams, SetObjectNameParams,
    SetSelectionParams,
};
use crate::effect::{EffectItem, EffectItemType, TrackInfo};
use crate::fingerprint::{
    EffectFingerprintInput, Fingerprint, ObjectFingerprintInput, effect_fingerprint,
    object_fingerprint,
};
use crate::handoff::{
    HANDOFF_TOKEN_LEN, HandoffToken, HandoffTokenFormatError, handoff_dir, handoff_file,
};
use crate::handshake::{Mac, Nonce, compute_client_mac, compute_server_mac, verify_mac};
use crate::identifier::{InstanceId, ProtocolVersion};
use crate::item_value::{ItemValue, ItemWriteError, encode_item_value, validate_item_value};
use crate::json::{JsonStrictError, parse_json};
use crate::number::FiniteF64;
use crate::render::RenderFrameParams;
use crate::selector::ObjectSelector;
use crate::text_codec::{decode_host_text, encode_host_text};
use crate::validation::{
    MAX_PATH_UTF16_UNITS, PathSyntaxError, validate_object_alias_name, validate_path,
};
use proptest::prelude::*;
use proptest::string::string_regex;
use proptest::test_runner::TestCaseError;
use std::path::{Component, Path};

// ============================================================================
// json
// ============================================================================

fn json_value_strategy() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::Bool),
        any::<i64>().prop_map(|i| serde_json::Value::Number(i.into())),
        any::<u64>().prop_map(|i| serde_json::Value::Number(i.into())),
        ".*".prop_map(serde_json::Value::String),
    ];

    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(serde_json::Value::Array),
            prop::collection::hash_map(".*", inner, 0..8).prop_map(|m| {
                serde_json::Value::Object(m.into_iter().collect::<serde_json::Map<_, _>>())
            }),
        ]
    })
}

proptest! {
    #[test]
    fn rejects_invalid_utf8(bytes in prop::collection::vec(any::<u8>(), 0..=256)) {
        if std::str::from_utf8(&bytes).is_err() {
            let result = parse_json(&bytes);
            prop_assert!(matches!(result, Err(JsonStrictError::InvalidUtf8)));
        }
    }

    #[test]
    fn rejects_duplicate_keys(
        (key, v1, v2) in (
            string_regex("[a-zA-Z_][a-zA-Z0-9_]*").unwrap(),
            any::<i64>(),
            any::<i64>(),
        ),
    ) {
        let json = format!(r#"{{"{key}":{v1},"{key}":{v2}}}"#);
        let result = parse_json(json.as_bytes());
        prop_assert!(matches!(result, Err(JsonStrictError::DuplicateKey(k)) if k == key));
    }

    #[test]
    fn rejects_non_finite_float_literals(
        nonfinite in prop_oneof![Just("NaN"), Just("Infinity"), Just("-Infinity")],
    ) {
        let result = parse_json(nonfinite.as_bytes());
        prop_assert!(result.is_err());
    }

    #[test]
    fn finite_float_parse_does_not_panic(
        f in any::<f64>().prop_filter("有限数", |v| v.is_finite()),
    ) {
        let s = serde_json::to_string(&f).unwrap();
        let result = parse_json(s.as_bytes());
        prop_assert!(result.is_ok());
    }

    #[test]
    fn arbitrary_value_roundtrip(v in json_value_strategy()) {
        let bytes = serde_json::to_vec(&v).unwrap();
        let parsed = parse_json(&bytes).unwrap();
        prop_assert_eq!(parsed, v);
    }
}

// ============================================================================
// handshake
// ============================================================================

fn fixed_auth_secret() -> [u8; 32] {
    [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
        0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D,
        0x1E, 0x1F,
    ]
}

fn fixed_client_nonce() -> Nonce {
    Nonce::from_bytes([
        0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E,
        0x2F, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D,
        0x3E, 0x3F,
    ])
}

fn fixed_server_nonce() -> Nonce {
    Nonce::from_bytes([
        0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E,
        0x4F, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D,
        0x5E, 0x5F,
    ])
}

fn fixed_instance_id() -> InstanceId {
    InstanceId::from_bytes([
        0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E,
        0x6F,
    ])
}

// .NET の HMACSHA256 で計算した既知ベクタ。
const EXPECTED_SERVER_MAC: [u8; 32] = [
    0x4a, 0x10, 0x84, 0x57, 0x7d, 0xb5, 0x38, 0x02, 0xea, 0xaa, 0xff, 0x89, 0x12, 0x21, 0x42, 0x65,
    0xd7, 0x60, 0x15, 0x51, 0x16, 0x1e, 0x31, 0x07, 0xcb, 0xae, 0x05, 0x85, 0x20, 0x21, 0x04, 0x58,
];

const EXPECTED_CLIENT_MAC: [u8; 32] = [
    0xfa, 0xf6, 0xf9, 0x90, 0xbd, 0x0c, 0x81, 0x19, 0x0e, 0x72, 0xce, 0x6d, 0x0c, 0x03, 0x04, 0x85,
    0x43, 0xb6, 0xab, 0xc4, 0x44, 0xbb, 0xba, 0xf0, 0xe9, 0xcd, 0xf6, 0xd5, 0x9f, 0xed, 0x1f, 0xef,
];

#[test]
fn server_mac_known_vector() {
    let mac = compute_server_mac(
        &fixed_auth_secret(),
        &fixed_client_nonce(),
        &fixed_server_nonce(),
        &fixed_instance_id(),
        &ProtocolVersion { major: 1, minor: 0 },
    );
    assert_eq!(mac.as_bytes(), &EXPECTED_SERVER_MAC);
}

#[test]
fn client_mac_known_vector() {
    let mac = compute_client_mac(
        &fixed_auth_secret(),
        &fixed_server_nonce(),
        &fixed_client_nonce(),
    );
    assert_eq!(mac.as_bytes(), &EXPECTED_CLIENT_MAC);
}

#[derive(Debug, Clone)]
enum TamperTarget {
    AuthSecret,
    ClientNonce,
    ServerNonce,
    InstanceId,
    VersionMajor,
    VersionMinor,
}

fn tamper_target_strategy() -> impl Strategy<Value = TamperTarget> {
    prop_oneof![
        Just(TamperTarget::AuthSecret),
        Just(TamperTarget::ClientNonce),
        Just(TamperTarget::ServerNonce),
        Just(TamperTarget::InstanceId),
        Just(TamperTarget::VersionMajor),
        Just(TamperTarget::VersionMinor),
    ]
}

fn flip_bit(buf: &mut [u8], bit: usize) {
    let bit = bit % (buf.len() * 8);
    buf[bit / 8] ^= 1 << (bit % 8);
}

proptest! {
    #[test]
    fn server_mac_one_bit_tamper_fails(
        (target, bit) in (tamper_target_strategy(), 0..256usize),
    ) {
        let secret = fixed_auth_secret();
        let client_nonce = fixed_client_nonce();
        let server_nonce = fixed_server_nonce();
        let instance_id = fixed_instance_id();
        let version = ProtocolVersion { major: 1, minor: 0 };

        let expected = compute_server_mac(
            &secret,
            &client_nonce,
            &server_nonce,
            &instance_id,
            &version,
        );

        let actual = match target {
            TamperTarget::AuthSecret => {
                let mut s = secret;
                flip_bit(&mut s, bit);
                compute_server_mac(&s, &client_nonce, &server_nonce, &instance_id, &version)
            }
            TamperTarget::ClientNonce => {
                let mut n = *client_nonce.as_bytes();
                flip_bit(&mut n, bit);
                compute_server_mac(&secret, &Nonce::from_bytes(n), &server_nonce, &instance_id, &version)
            }
            TamperTarget::ServerNonce => {
                let mut n = *server_nonce.as_bytes();
                flip_bit(&mut n, bit);
                compute_server_mac(&secret, &client_nonce, &Nonce::from_bytes(n), &instance_id, &version)
            }
            TamperTarget::InstanceId => {
                let mut id = *instance_id.as_bytes();
                flip_bit(&mut id, bit);
                compute_server_mac(&secret, &client_nonce, &server_nonce, &InstanceId::from_bytes(id), &version)
            }
            TamperTarget::VersionMajor => {
                let mut v = version;
                v.major ^= 1 << (bit % 16);
                compute_server_mac(&secret, &client_nonce, &server_nonce, &instance_id, &v)
            }
            TamperTarget::VersionMinor => {
                let mut v = version;
                v.minor ^= 1 << (bit % 16);
                compute_server_mac(&secret, &client_nonce, &server_nonce, &instance_id, &v)
            }
        };

        prop_assert!(!verify_mac(&expected, &actual));
    }

    #[test]
    fn client_mac_one_bit_tamper_fails(
        (target, bit) in (tamper_target_strategy(), 0..256usize),
    ) {
        let secret = fixed_auth_secret();
        let client_nonce = fixed_client_nonce();
        let server_nonce = fixed_server_nonce();

        let expected = compute_client_mac(&secret, &server_nonce, &client_nonce);

        let actual = match target {
            TamperTarget::AuthSecret => {
                let mut s = secret;
                flip_bit(&mut s, bit);
                compute_client_mac(&s, &server_nonce, &client_nonce)
            }
            TamperTarget::ClientNonce => {
                let mut n = *client_nonce.as_bytes();
                flip_bit(&mut n, bit);
                compute_client_mac(&secret, &server_nonce, &Nonce::from_bytes(n))
            }
            TamperTarget::ServerNonce => {
                let mut n = *server_nonce.as_bytes();
                flip_bit(&mut n, bit);
                compute_client_mac(&secret, &Nonce::from_bytes(n), &client_nonce)
            }
            // client_mac 計算には instance_id / version は含まれない。
            TamperTarget::InstanceId
            | TamperTarget::VersionMajor
            | TamperTarget::VersionMinor => {
                return Ok(());
            }
        };

        prop_assert!(!verify_mac(&expected, &actual));
    }

    #[test]
    fn mac_one_bit_tamper_fails(bit in 0..256usize) {
        let secret = fixed_auth_secret();
        let client_nonce = fixed_client_nonce();
        let server_nonce = fixed_server_nonce();
        let instance_id = fixed_instance_id();
        let version = ProtocolVersion { major: 1, minor: 0 };

        let expected = compute_server_mac(
            &secret,
            &client_nonce,
            &server_nonce,
            &instance_id,
            &version,
        );

        let mut actual_bytes = *expected.as_bytes();
        flip_bit(&mut actual_bytes, bit);
        let actual = Mac::from_bytes(actual_bytes);

        prop_assert!(!verify_mac(&expected, &actual));
    }
}

// ============================================================================
// BPM グリッド
// ============================================================================

proptest! {
    #[test]
    fn a_single_precision_value_survives_the_round_trip_through_the_dto(
        bits in any::<u32>(),
    ) {
        // BPM 情報の tempo と offset は SDK では単精度である。DTO は倍精度で
        // 運ぶため、往復で値が変わらないことが「読み取った一覧をそのまま
        // 書き戻せる」ことの前提になる。
        let value = f32::from_bits(bits);
        prop_assume!(value.is_finite());

        let carried = FiniteF64::try_new(f64::from(value)).expect("単精度の有限値は有限である");
        let json = serde_json::to_string(&carried).expect("直列化できる");
        let restored: FiniteF64 = serde_json::from_str(&json).expect("逆直列化できる");

        prop_assert_eq!((restored.get() as f32).to_bits(), value.to_bits());
    }
}

// ============================================================================
// fingerprint
// ============================================================================

/// `object_fingerprint` の入力を所有する形。
#[derive(Debug, Clone, PartialEq)]
struct OwnedObjectInput {
    scene_id: i32,
    layer: usize,
    frame_start: usize,
    frame_end: usize,
    name: Option<String>,
    alias: String,
}

impl OwnedObjectInput {
    fn compute(&self) -> Fingerprint {
        object_fingerprint(ObjectFingerprintInput {
            scene_id: self.scene_id,
            layer: self.layer,
            frame_start: self.frame_start,
            frame_end: self.frame_end,
            name: self.name.as_deref(),
            alias: &self.alias,
        })
    }
}

fn object_input_strategy() -> impl Strategy<Value = OwnedObjectInput> {
    (
        any::<i32>(),
        0..1_000usize,
        0..1_000_000usize,
        0..1_000_000usize,
        prop::option::of(".*"),
        ".*",
    )
        .prop_map(
            |(scene_id, layer, frame_start, frame_end, name, alias)| OwnedObjectInput {
                scene_id,
                layer,
                frame_start,
                frame_end,
                name,
                alias,
            },
        )
}

fn item_value_strategy() -> impl Strategy<Value = ItemValue> {
    prop_oneof![
        any::<f64>()
            .prop_filter("有限数", |v| v.is_finite())
            .prop_map(|v| ItemValue::Number {
                value: FiniteF64::try_new(v).expect("有限数のみを生成する"),
            }),
        any::<i64>().prop_map(|value| ItemValue::Integer { value }),
        any::<bool>().prop_map(|value| ItemValue::Bool { value }),
        ".*".prop_map(|value| ItemValue::Color { value }),
        ".*".prop_map(|value| ItemValue::Choice { value }),
        ".*".prop_map(|path| ItemValue::File { path }),
        ".*".prop_map(|path| ItemValue::Folder { path }),
        ".*".prop_map(|name| ItemValue::Font { name }),
        ".*".prop_map(|value| ItemValue::Text { value }),
        ".*".prop_map(|raw| ItemValue::Unknown { raw }),
    ]
}

fn track_info_strategy() -> impl Strategy<Value = TrackInfo> {
    (
        ".*",
        prop::collection::vec(
            any::<f64>()
                .prop_filter("有限数", |v| v.is_finite())
                .prop_map(|v| FiniteF64::try_new(v).expect("有限数のみを生成する")),
            0..4,
        ),
        any::<(bool, bool, bool, bool)>(),
        (0..8usize, 0..8usize),
        prop::option::of(".*"),
    )
        .prop_map(
            |(
                mode,
                params,
                (accelerate, decelerate, twopoint, timecontrol),
                (group_num, group_index),
                group_name,
            )| {
                TrackInfo {
                    mode,
                    params,
                    accelerate,
                    decelerate,
                    twopoint,
                    timecontrol,
                    group_num,
                    group_index,
                    group_name,
                }
            },
        )
}

fn effect_item_strategy() -> impl Strategy<Value = EffectItem> {
    (
        ".*",
        any::<i32>().prop_map(EffectItemType::from_raw),
        item_value_strategy(),
        prop::option::of(track_info_strategy()),
    )
        .prop_map(|(name, item_type, value, track)| EffectItem {
            name,
            item_type,
            value,
            track,
        })
}

/// `effect_fingerprint` の入力を所有する形。
#[derive(Debug, Clone, PartialEq)]
struct OwnedEffectInput {
    effect_name: String,
    effect_index: usize,
    position: usize,
    effect_count: usize,
    enabled: bool,
    locked: bool,
    items: Vec<EffectItem>,
}

impl OwnedEffectInput {
    fn compute(&self) -> Fingerprint {
        effect_fingerprint(EffectFingerprintInput {
            effect_name: &self.effect_name,
            effect_index: self.effect_index,
            position: self.position,
            effect_count: self.effect_count,
            enabled: self.enabled,
            locked: self.locked,
            items: &self.items,
        })
    }
}

fn effect_input_strategy() -> impl Strategy<Value = OwnedEffectInput> {
    (
        ".*",
        0..8usize,
        0..8usize,
        1..16usize,
        any::<bool>(),
        any::<bool>(),
        prop::collection::vec(effect_item_strategy(), 0..4),
    )
        .prop_map(
            |(effect_name, effect_index, position, effect_count, enabled, locked, items)| {
                OwnedEffectInput {
                    effect_name,
                    effect_index,
                    position,
                    effect_count,
                    enabled,
                    locked,
                    items,
                }
            },
        )
}

/// 正準表現かどうかを判定する。
fn is_canonical_fingerprint(fingerprint: &Fingerprint) -> bool {
    fingerprint
        .as_str()
        .strip_prefix("sha256:")
        .is_some_and(|hex| {
            hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}

proptest! {
    #[test]
    fn object_fingerprint_is_deterministic(input in object_input_strategy()) {
        prop_assert_eq!(input.compute(), input.compute());
    }

    #[test]
    fn object_fingerprint_has_canonical_form(input in object_input_strategy()) {
        let fingerprint = input.compute();
        prop_assert!(is_canonical_fingerprint(&fingerprint));
        prop_assert!(fingerprint.as_str().parse::<Fingerprint>().is_ok());
    }

    #[test]
    fn object_fingerprint_differs_for_distinct_inputs(
        (a, b) in (object_input_strategy(), object_input_strategy()),
    ) {
        if a != b {
            prop_assert_ne!(a.compute(), b.compute());
        }
    }

    #[test]
    fn object_fingerprint_distinguishes_absent_name_from_empty(
        input in object_input_strategy(),
    ) {
        let absent = OwnedObjectInput { name: None, ..input.clone() };
        let empty = OwnedObjectInput { name: Some(String::new()), ..input };
        prop_assert_ne!(absent.compute(), empty.compute());
    }

    /// alias だけが違うオブジェクトは別物である。
    ///
    /// alias は配下 effect の設定値とロック状態を含むため、effect の変更はこの
    /// 経路でオブジェクトの fingerprint へ伝わる。
    #[test]
    fn object_fingerprint_depends_on_the_alias(
        (input, alias) in (object_input_strategy(), ".*"),
    ) {
        if input.alias != alias {
            let changed = OwnedObjectInput { alias, ..input.clone() };
            prop_assert_ne!(input.compute(), changed.compute());
        }
    }

    #[test]
    fn effect_fingerprint_is_deterministic(input in effect_input_strategy()) {
        prop_assert_eq!(input.compute(), input.compute());
    }

    #[test]
    fn effect_fingerprint_has_canonical_form(input in effect_input_strategy()) {
        prop_assert!(is_canonical_fingerprint(&input.compute()));
    }

    #[test]
    fn effect_fingerprint_differs_for_distinct_inputs(
        (a, b) in (effect_input_strategy(), effect_input_strategy()),
    ) {
        if a != b {
            prop_assert_ne!(a.compute(), b.compute());
        }
    }

    /// 列の絶対位置だけが違う effect は別物である。
    #[test]
    fn effect_fingerprint_depends_on_the_position_in_the_list(
        (input, position) in (effect_input_strategy(), 0..8usize),
    ) {
        if input.position != position {
            let moved = OwnedEffectInput { position, ..input.clone() };
            prop_assert_ne!(input.compute(), moved.compute());
        }
    }

    /// 列の総数だけが違う effect は別物である。
    ///
    /// 前方の同名 effect が取り除かれた場合、残った側は同名内の番号が繰り上がって
    /// 取り除く前の先頭と一致し得る。総数は必ず変わるため、ここで区別される。
    #[test]
    fn effect_fingerprint_depends_on_the_length_of_the_list(
        (input, effect_count) in (effect_input_strategy(), 1..16usize),
    ) {
        if input.effect_count != effect_count {
            let shifted = OwnedEffectInput { effect_count, ..input.clone() };
            prop_assert_ne!(input.compute(), shifted.compute());
        }
    }

}

// ============================================================================
// 編集入力の検証
// ============================================================================

/// パスの構成要素に使う文字。区切り・ドライブ・拡張子・多バイト文字を含む。
fn path_fragment_strategy() -> impl Strategy<Value = String> {
    string_regex(r"[a-zA-Z0-9_. \\/あ:?]{0,64}").unwrap()
}

/// パスらしい文字列。
///
/// 一様乱数の文字列は拒否規則に当たる形をほとんど生成しないため、規則ごとの
/// 接頭辞と、区切り・ドライブ・ストリーム・NUL を含む断片を組み合わせる。
fn path_like_strategy() -> impl Strategy<Value = String> {
    let prefix = prop_oneof![
        Just(String::new()),
        Just(r"C:\".to_string()),
        Just("C:".to_string()),
        Just(r"\".to_string()),
        Just(r"\\".to_string()),
        Just(r"\\.\".to_string()),
        Just(r"\\?\".to_string()),
        Just("//./".to_string()),
        Just("//?/".to_string()),
    ];
    let segment = prop_oneof![
        string_regex(r"[a-zA-Z0-9_.あ]{0,12}").unwrap(),
        Just(":stream".to_string()),
        Just("C:".to_string()),
        Just("\0".to_string()),
        Just("..".to_string()),
    ];
    (prefix, prop::collection::vec(segment, 0..4)).prop_map(|(prefix, segments)| {
        let mut path = prefix;
        path.push_str(&segments.join("\\"));
        path
    })
}

/// パス検証が規則どおりに答えることを確かめる。
///
/// 受理したなら拒否規則のいずれにも当たらないこと、空と NUL は理由まで
/// 一致することを見る。拒否規則を 1 つでも通してしまう実装は必ずここで
/// 落ちる。
fn assert_path_validation_follows_the_rules(path: &str) -> Result<(), TestCaseError> {
    if path.is_empty() {
        prop_assert_eq!(validate_path(path), Err(PathSyntaxError::Empty));
        return Ok(());
    }
    if path.contains('\0') {
        prop_assert_eq!(validate_path(path), Err(PathSyntaxError::ContainsNul));
        return Ok(());
    }
    if validate_path(path).is_err() {
        return Ok(());
    }
    prop_assert!(!path.is_empty());
    prop_assert!(!path.contains('\0'));
    prop_assert!(path.encode_utf16().count() <= MAX_PATH_UTF16_UNITS);
    let normalized = path.replace('/', "\\");
    prop_assert!(!normalized.starts_with(r"\\.\"));
    prop_assert!(!normalized.starts_with(r"\\?\"));
    prop_assert!(normalized != r"\\." && normalized != r"\\?");
    prop_assert!(normalized.matches(':').count() <= 1);
    // 受理されるのはドライブレター起点だけである。ネットワーク上の場所を指す
    // 形（`\\` 始まり）はここへ残らない。
    prop_assert!(!normalized.starts_with(r"\\"));
    prop_assert!(normalized.starts_with(|c: char| c.is_ascii_alphabetic()));
    prop_assert_eq!(normalized.get(1..3), Some(r":\"));
    Ok(())
}

proptest! {
    #[test]
    fn path_validation_answers_for_any_string(
        path in prop_oneof![".*", path_like_strategy()],
    ) {
        // 任意の文字列に対して panic せず、可否のいずれかを返す。
        assert_path_validation_follows_the_rules(&path)?;
    }

    #[test]
    fn path_validation_answers_for_arbitrary_bytes(
        bytes in prop_oneof![
            prop::collection::vec(any::<u8>(), 0..=512),
            path_like_strategy().prop_map(String::into_bytes),
        ],
    ) {
        // 不正な UTF-8 を含む入力から作った文字列でも規則は変わらない。
        let path = String::from_utf8_lossy(&bytes);
        assert_path_validation_follows_the_rules(&path)?;
    }

    #[test]
    fn device_namespace_is_always_rejected(
        (prefix, rest) in (
            prop_oneof![Just(r"\\.\"), Just(r"\\?\"), Just("//./"), Just("//?/")],
            path_fragment_strategy(),
        ),
    ) {
        let path = format!("{prefix}{rest}");
        prop_assert_eq!(validate_path(&path), Err(PathSyntaxError::DeviceNamespace));
    }

    #[test]
    fn alternate_data_stream_is_always_rejected(
        (name, stream) in (
            string_regex(r"[a-zA-Z0-9_.]{1,32}").unwrap(),
            string_regex(r"[a-zA-Z0-9_.$]{0,32}").unwrap(),
        ),
    ) {
        let path = format!(r"C:\{name}:{stream}");
        prop_assert_eq!(
            validate_path(&path),
            Err(PathSyntaxError::AlternateDataStream)
        );
    }

    #[test]
    fn unc_is_always_rejected(
        (prefix, server, rest) in (
            prop_oneof![Just(r"\\"), Just("//")],
            string_regex(r"[a-zA-Z0-9_あ]{1,12}").unwrap(),
            string_regex(r"[a-zA-Z0-9_.あ\\/]{0,24}").unwrap(),
        ),
    ) {
        // 共有名の有無にも区切りの種類にも依らず、同じ理由で拒否する。
        let path = format!(r"{prefix}{server}\{rest}");
        prop_assert_eq!(validate_path(&path), Err(PathSyntaxError::UncPath));
    }

    #[test]
    fn nul_and_oversized_paths_are_always_rejected(
        (head, tail) in (path_fragment_strategy(), path_fragment_strategy()),
    ) {
        prop_assert_eq!(
            validate_path(&format!("{head}\0{tail}")),
            Err(PathSyntaxError::ContainsNul)
        );

        let path = format!(r"C:\{}{head}", "a".repeat(MAX_PATH_UTF16_UNITS));
        prop_assert_eq!(
            validate_path(&path),
            Err(PathSyntaxError::TooLong { units: path.encode_utf16().count() })
        );
    }

    #[test]
    fn item_value_write_answers_for_any_value(
        (value, raw) in (item_value_strategy(), -1..=17i32),
    ) {
        // 種別と値のどの組み合わせでも panic せず、可否を返す。
        let item_type = EffectItemType::from_raw(raw);
        let encoded = encode_item_value(&item_type, &value);
        match &value {
            // 未対応種別の生値は種別によらず必ず拒否する。
            ItemValue::Unknown { .. } => {
                prop_assert_eq!(encoded, Err(ItemWriteError::UnknownValue));
                prop_assert_eq!(validate_item_value(&value), Err(ItemWriteError::UnknownValue));
            }
            _ => {
                if let Ok(encoded) = encoded {
                    // 書き込む文字列に NUL は残らない。
                    prop_assert!(!encoded.contains('\0'));
                    // 水平タブは複数行を取り得る値でのみ残る。改行はエスケープ
                    // 表記へ包まれるため、どの値でも制御文字としては残らない。
                    let multiline = matches!(value, ItemValue::Text { .. });
                    let allowed = |c: char| multiline && c == '\t';
                    let unexpected = encoded.chars().any(|c| c.is_control() && !allowed(c));
                    prop_assert!(!unexpected);
                    prop_assert!(validate_item_value(&value).is_ok());
                }
            }
        }
    }

    #[test]
    fn edit_params_decoders_answer_for_arbitrary_bytes(
        bytes in prop::collection::vec(any::<u8>(), 0..=512),
    ) {
        // 任意の byte 列に対して panic せず、型付きの decode error になる。
        macro_rules! assert_decodes_or_errors {
            ($type:ty) => {{
                match serde_json::from_slice::<$type>(&bytes) {
                    // 受理できた場合も、要求内容の検証まで panic せずに進む。
                    Ok(params) => {
                        let _ = params.validate();
                    }
                    Err(error) => {
                        prop_assert!(!error.to_string().is_empty());
                    }
                }
            }};
        }

        assert_decodes_or_errors!(CreateObjectParams);
        assert_decodes_or_errors!(MoveObjectParams);
        assert_decodes_or_errors!(DeleteObjectParams);
        assert_decodes_or_errors!(SetObjectNameParams);
        assert_decodes_or_errors!(SetObjectItemParams);
        assert_decodes_or_errors!(AddEffectParams);
        assert_decodes_or_errors!(DeleteEffectParams);
        assert_decodes_or_errors!(SetEffectEnabledParams);
        assert_decodes_or_errors!(SetSelectionParams);
        assert_decodes_or_errors!(ApplyBatchParams);
        assert_decodes_or_errors!(RenderFrameParams);
    }

    #[test]
    fn edit_params_decoders_answer_for_arbitrary_json(value in json_value_strategy()) {
        let bytes = serde_json::to_vec(&value).unwrap();
        macro_rules! assert_decodes_or_errors {
            ($type:ty) => {{
                if let Ok(params) = serde_json::from_slice::<$type>(&bytes) {
                    let _ = params.validate();
                }
            }};
        }

        assert_decodes_or_errors!(CreateObjectParams);
        assert_decodes_or_errors!(MoveObjectParams);
        assert_decodes_or_errors!(DeleteObjectParams);
        assert_decodes_or_errors!(SetObjectNameParams);
        assert_decodes_or_errors!(SetObjectItemParams);
        assert_decodes_or_errors!(AddEffectParams);
        assert_decodes_or_errors!(DeleteEffectParams);
        assert_decodes_or_errors!(SetEffectEnabledParams);
        assert_decodes_or_errors!(SetSelectionParams);
        assert_decodes_or_errors!(ApplyBatchParams);
        assert_decodes_or_errors!(RenderFrameParams);
    }

    /// 長大な `operations` を運ぶ要求でも、確保する要素数が実際に現れた件数を
    /// 大きく超えないことを確かめる。
    ///
    /// JSON は件数を前置きしないため、復号器は読み進めた分だけを確保する。
    /// 件数を名乗る値を信用して先に確保する実装へ変わると、上限を超える件数を
    /// 名乗るだけの短い要求で大きな確保を起こせるようになる。
    #[test]
    fn batch_params_decoder_does_not_over_allocate_for_long_arrays(
        // 一様乱数では上限の前後にほとんど当たらないため、0 の近く・上限の
        // 前後・上限の 10 倍を混ぜる。
        count in prop_oneof![0..4usize, 95..106usize, 990..1_010usize],
    ) {
        let elements: Vec<String> = (0..count)
            .map(|layer| serde_json::to_string(&batch_move_operation(layer)).unwrap())
            .collect();
        let body = format!("{{\"operations\":[{}]}}", elements.join(","));

        let params: ApplyBatchParams = serde_json::from_str(&body).unwrap();
        prop_assert_eq!(params.operations.len(), count);
        prop_assert!(
            params.operations.capacity() <= count.saturating_mul(2) + 8,
            "件数 {} に対して確保が {} まで膨らんでいる",
            count,
            params.operations.capacity()
        );

        // 上限を超える件数は、復号できても検証で落ちる。
        prop_assert_eq!(
            params.validate().is_ok(),
            (1..=MAX_BATCH_OPERATIONS).contains(&count)
        );
    }
}

/// 与えたレイヤーのオブジェクトを動かす sub-operation を作る。
///
/// レイヤーごとにセレクターが変わるため、並べても同じ状態を指さない。
fn batch_move_operation(layer: usize) -> BatchOperation {
    let input = ObjectFingerprintInput {
        scene_id: 0,
        layer,
        frame_start: 120,
        frame_end: 240,
        name: None,
        alias: "alias",
    };
    BatchOperation::MoveObject {
        selector: ObjectSelector {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            scene_id: 0,
            layer,
            frame: 120,
            name: None,
            fingerprint: object_fingerprint(input),
        },
        destination: Destination { layer: 0, frame: 0 },
    }
}

// ============================================================================
// handoff
// ============================================================================

proptest! {
    /// 任意の文字列は引き渡し用ファイルのパスの材料にならない。
    ///
    /// 構文検証を通らない値はパスを組み立てる関数へ渡せず、通った値も基底の
    /// 下へ 3 要素を足すだけである。区切り文字や相対参照が場所を動かす余地が
    /// 無いことを、任意の入力に対して固定する。
    #[test]
    fn an_arbitrary_string_never_builds_a_handoff_path(value in ".*") {
        let base = Path::new("base");
        let instance_id = InstanceId::from_bytes([0x11; 16]);
        let Ok(token) = HandoffToken::parse(&value) else {
            // 組み立てる材料が得られない。ここで止まる。
            return Ok(());
        };

        prop_assert_eq!(token.as_str(), value.as_str());
        prop_assert!(value.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        prop_assert_eq!(value.len(), HANDOFF_TOKEN_LEN);

        let path = handoff_file(base, &instance_id, &token);
        prop_assert!(path.starts_with(handoff_dir(base, &instance_id)));
        prop_assert_eq!(path.components().count(), 4);
    }

    /// 十六進でない文字を混ぜた 32 文字は必ず拒否される。
    #[test]
    fn a_token_with_a_non_hex_character_is_rejected(
        prefix in "[0-9a-f]{0,31}",
        intruder in "[^0-9a-f]",
    ) {
        let mut value = prefix;
        value.push_str(&intruder);
        while value.chars().count() < HANDOFF_TOKEN_LEN {
            value.push('0');
        }
        prop_assert_eq!(HandoffToken::parse(&value), Err(HandoffTokenFormatError));
    }
}

// ============================================================================
// オブジェクトエイリアス名
// ============================================================================

/// エイリアス名らしい文字列。
///
/// 一様乱数の文字列は区切りも相対参照もほとんど生成しないため、規則の境界に
/// 当たる断片を組み合わせる。
///
/// 通る名前へ重みを寄せる。断片を等確率で混ぜると生成のほとんどが検証で落ち、
/// 性質そのものを評価する回数が数えるほどしか残らない。落ちる入力は規則が
/// 緩んだ瞬間に性質へ到達させるために要るのであって、多いほど良いものでは
/// ない。
fn object_alias_name_like_strategy() -> impl Strategy<Value = String> {
    let prefix = prop_oneof![
        6 => Just(String::new()),
        1 => Just("..".to_string()),
        1 => Just(r"..\".to_string()),
        1 => Just("../".to_string()),
        1 => Just("C:".to_string()),
        1 => Just(".".to_string()),
    ];
    let segment = prop_oneof![
        8 => string_regex(r"[a-zA-Z0-9_ あ\u{a5}]{0,12}").unwrap(),
        1 => string_regex(r#"[\\/:*?"'<>|%=,.]{1,3}"#).unwrap(),
        1 => Just("\0".to_string()),
    ];
    (prefix, prop::collection::vec(segment, 0..4))
        .prop_map(|(prefix, segments)| format!("{prefix}{}", segments.concat()))
}

proptest! {
    /// 受理されたエイリアス名は、エイリアスディレクトリの外を指すパスを
    /// 組み立てない。
    ///
    /// 名前の判定はパスの組み立てより先に行うため、ファイル名の一部になるのは
    /// 通った名前だけである。区切りも相対参照も残らないことを、任意の入力に
    /// 対して固定する。
    ///
    /// 落ちた名前は `prop_assume!` で捨てる。黙って `Ok` を返す形にすると、
    /// 生成器が偏って通る名前を 1 つも作らなくなっても緑のままになる。
    /// 捨てた数は proptest が数えており、通る入力が枯れれば落ちる。
    #[test]
    fn an_accepted_object_alias_name_never_builds_a_path_outside_its_directory(
        name in prop_oneof![1 => ".*", 9 => object_alias_name_like_strategy()],
    ) {
        prop_assume!(validate_object_alias_name(&name).is_ok());

        // 名前は 1 つの構成要素にしかならない。区切りが残っていれば親が
        // 変わり、相対参照が残っていれば構成要素の数が変わる。
        let dir = Path::new(r"C:\ProgramData\aviutl2\Alias");
        let path = dir.join(format!("{name}.object"));
        prop_assert_eq!(path.parent(), Some(dir));
        prop_assert_eq!(path.components().count(), dir.components().count() + 1);
        prop_assert!(path.components().all(|component| !matches!(
            component,
            Component::CurDir | Component::ParentDir
        )));
    }
}

// ============================================================================
// テキスト設定値の codec
// ============================================================================

/// エスケープの規則に当たる断片を混ぜた文字列。
///
/// 一様乱数の文字列はバックスラッシュも改行もほとんど生成しないため、規則の
/// 境界に当たる断片を組み合わせる。
fn host_text_strategy() -> impl Strategy<Value = String> {
    let fragment = prop_oneof![
        6 => string_regex(r"[a-zA-Z0-9_ 字幕]{0,8}").unwrap(),
        2 => Just(r"\".to_string()),
        2 => Just(r"\\".to_string()),
        2 => Just(r"\n".to_string()),
        2 => Just("\n".to_string()),
        1 => Just("\t".to_string()),
        1 => Just(r"C:\temp\note".to_string()),
    ];
    prop::collection::vec(fragment, 0..8).prop_map(|fragments| fragments.concat())
}

proptest! {
    /// 符号化した文字列は必ず元へ復号できる。
    #[test]
    fn the_text_codec_round_trips_any_value(
        value in prop_oneof![1 => ".*", 9 => host_text_strategy()],
    ) {
        prop_assert_eq!(decode_host_text(&encode_host_text(&value)), value);
    }

    /// 符号化と復号を繰り返しても包みは育たない。
    #[test]
    fn the_text_codec_does_not_grow_the_escapes(
        value in prop_oneof![1 => ".*", 9 => host_text_strategy()],
    ) {
        let once = encode_host_text(&value);
        let mut current = once.clone();
        for _ in 0..3 {
            current = encode_host_text(&decode_host_text(&current));
            prop_assert_eq!(&current, &once);
        }
    }

    /// 符号化した文字列にはバックスラッシュを伴わない `n` の綴りしか残らない。
    ///
    /// 改行がエスケープ表記へ包まれ、バックスラッシュが必ず 2 つ組になる。
    /// タブはどちらの規則にも当たらず素通しする。
    #[test]
    fn the_encoded_form_has_no_bare_line_feed_and_pairs_every_backslash(
        value in prop_oneof![1 => ".*", 9 => host_text_strategy()],
    ) {
        let encoded = encode_host_text(&value);
        prop_assert!(!encoded.contains('\n'));
        prop_assert_eq!(
            encoded.matches('\t').count(),
            value.matches('\t').count()
        );

        // 先頭から 2 文字ずつ食えば、`\` は必ず `\\` か `\n` の一部になる。
        let chars: Vec<char> = encoded.chars().collect();
        let mut index = 0;
        while index < chars.len() {
            if chars[index] == '\\' {
                prop_assert!(matches!(chars.get(index + 1), Some('\\') | Some('n')));
                index += 2;
            } else {
                index += 1;
            }
        }
    }

    /// 書き込みが渡す文字列は、ホストが解いた時点で要求の値そのものになる。
    ///
    /// ホストの解釈を模した関数を通す。描画に使われるのは解いた後の値であり、
    /// ここが一致しなければ Windows パスも正規表現も崩れる。
    #[test]
    fn the_value_the_host_stores_is_the_value_that_was_requested(
        value in prop_oneof![1 => ".*", 9 => host_text_strategy()],
    ) {
        let requested = ItemValue::Text { value: value.clone() };
        prop_assume!(validate_item_value(&requested).is_ok());

        let encoded = encode_item_value(&EffectItemType::Text, &requested)
            .expect("検証を通った値は符号化できる");
        prop_assert_eq!(host_store(&encoded), value.replace("\r\n", "\n"));
    }
}

/// ホストが書き込まれた表記を解いて保存する規則を模す。
///
/// `\\` を `\` へ、`\n` を LF へ戻し、それ以外の `\` は次の文字ごとそのまま
/// 保つ。**codec を呼ばずに書く。** 呼ぶと、codec が規則からずれたときに
/// 検査も一緒にずれる。
fn host_store(written: &str) -> String {
    let chars: Vec<char> = written.chars().collect();
    let mut stored = String::new();
    let mut index = 0;
    while index < chars.len() {
        match (chars[index], chars.get(index + 1)) {
            ('\\', Some('\\')) => {
                stored.push('\\');
                index += 2;
            }
            ('\\', Some('n')) => {
                stored.push('\n');
                index += 2;
            }
            (c, _) => {
                stored.push(c);
                index += 1;
            }
        }
    }
    stored
}
