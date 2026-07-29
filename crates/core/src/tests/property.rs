//! property-based テスト。

use crate::edit::{
    AddEffectParams, CreateObjectParams, DeleteEffectParams, DeleteObjectParams, MoveObjectParams,
    SetEffectStateParams, SetObjectItemParams, SetObjectNameParams, SetSelectionParams,
};
use crate::effect::{EffectItem, EffectItemType, TrackInfo};
use crate::fingerprint::{
    EffectFingerprintInput, Fingerprint, ObjectFingerprintInput, effect_fingerprint,
    object_fingerprint,
};
use crate::handshake::{Mac, Nonce, compute_client_mac, compute_server_mac, verify_mac};
use crate::identifier::{InstanceId, ProtocolVersion};
use crate::item_value::{ItemValue, ItemWriteError, encode_item_value, validate_item_value};
use crate::json::{JsonStrictError, parse_json};
use crate::number::FiniteF64;
use crate::validation::{MAX_PATH_UTF16_UNITS, PathSyntaxError, validate_path};
use proptest::prelude::*;
use proptest::string::string_regex;
use proptest::test_runner::TestCaseError;

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
    effect_fingerprints: Vec<Fingerprint>,
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
            effect_fingerprints: &self.effect_fingerprints,
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
        prop::collection::vec(effect_input_strategy(), 0..3),
    )
        .prop_map(
            |(scene_id, layer, frame_start, frame_end, name, alias, effects)| OwnedObjectInput {
                scene_id,
                layer,
                frame_start,
                frame_end,
                name,
                alias,
                effect_fingerprints: effects.iter().map(OwnedEffectInput::compute).collect(),
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
        (".*", prop::option::of(0..100usize))
            .prop_map(|(value, index)| ItemValue::Choice { value, index }),
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

    /// 配下 effect の fingerprint 列だけが違うオブジェクトは別物である。
    #[test]
    fn object_fingerprint_depends_on_the_effects(
        (input, effects) in (
            object_input_strategy(),
            prop::collection::vec(effect_input_strategy(), 0..3),
        ),
    ) {
        let effect_fingerprints: Vec<Fingerprint> =
            effects.iter().map(OwnedEffectInput::compute).collect();
        if input.effect_fingerprints != effect_fingerprints {
            let changed = OwnedObjectInput { effect_fingerprints, ..input.clone() };
            prop_assert_ne!(input.compute(), changed.compute());
        }
    }

    /// 配下 effect が 0 件のオブジェクトは、1 件持つオブジェクトと区別される。
    #[test]
    fn object_fingerprint_distinguishes_no_effect_from_one(
        (input, effect) in (object_input_strategy(), effect_input_strategy()),
    ) {
        let none = OwnedObjectInput {
            effect_fingerprints: Vec::new(),
            ..input.clone()
        };
        let one = OwnedObjectInput {
            effect_fingerprints: vec![effect.compute()],
            ..input
        };
        prop_assert_ne!(none.compute(), one.compute());
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
    prop_assert!(normalized.starts_with(r"\\") || normalized.contains(r":\"));
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
                    // 改行・復帰・水平タブは複数行を取り得る値でのみ残る。
                    // 他の値ではどの制御文字も残らない。
                    let multiline = matches!(value, ItemValue::Text { .. });
                    let allowed = |c: char| multiline && matches!(c, '\n' | '\r' | '\t');
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
        assert_decodes_or_errors!(SetEffectStateParams);
        assert_decodes_or_errors!(SetSelectionParams);
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
        assert_decodes_or_errors!(SetEffectStateParams);
        assert_decodes_or_errors!(SetSelectionParams);
    }
}
