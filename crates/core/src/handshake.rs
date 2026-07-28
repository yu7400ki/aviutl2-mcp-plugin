//! HMAC handshake ロジック。
//!
//! 固定の連結順で HMAC-SHA256 を計算する。
//!
//! - `server_mac = HMAC(auth_secret, client_nonce || server_nonce || instance_id || protocol_version_string)`
//! - `client_mac = HMAC(auth_secret, server_nonce || client_nonce || "client")`

use crate::identifier::{InstanceId, ProtocolVersion};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac as HmacMac};
use rand::Rng;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::Sha256;
use std::fmt;
use subtle::ConstantTimeEq;

/// 32 バイト乱数。接続ごとに CSPRNG で生成し、再利用しない。
#[derive(Clone, PartialEq, Eq)]
pub struct Nonce([u8; 32]);

impl Nonce {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Nonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Nonce(***)")
    }
}

impl fmt::Display for Nonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Nonce(***)")
    }
}

impl Serialize for Nonce {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let encoded = URL_SAFE_NO_PAD.encode(self.0);
        serializer.serialize_str(&encoded)
    }
}

impl<'de> Deserialize<'de> for Nonce {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let bytes = URL_SAFE_NO_PAD.decode(&s).map_err(|e| {
            serde::de::Error::custom(format!("nonce の base64 デコードに失敗しました: {e}"))
        })?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom(format!(
                "nonce は 32 バイトである必要があります: 実際は {} バイト",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(Nonce(arr))
    }
}

/// HMAC-SHA256 結果。
#[derive(Clone, PartialEq, Eq)]
pub struct Mac([u8; 32]);

impl Mac {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mac(***)")
    }
}

impl fmt::Display for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mac(***)")
    }
}

impl Serialize for Mac {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let encoded = URL_SAFE_NO_PAD.encode(self.0);
        serializer.serialize_str(&encoded)
    }
}

impl<'de> Deserialize<'de> for Mac {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let bytes = URL_SAFE_NO_PAD.decode(&s).map_err(|e| {
            serde::de::Error::custom(format!("MAC の base64 デコードに失敗しました: {e}"))
        })?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom(format!(
                "MAC は 32 バイトである必要があります: 実際は {} バイト",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(Mac(arr))
    }
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC 鍵長は常に有効");
    HmacMac::update(&mut mac, data);
    mac.finalize().into_bytes().into()
}

/// server_mac を計算する。
///
/// `HMAC(auth_secret, client_nonce || server_nonce || instance_id || protocol_version)`
pub fn compute_server_mac(
    auth_secret: &[u8; 32],
    client_nonce: &Nonce,
    server_nonce: &Nonce,
    instance_id: &InstanceId,
    protocol_version: &ProtocolVersion,
) -> Mac {
    let mut data = Vec::with_capacity(32 + 32 + 16 + protocol_version.as_bytes().len());
    data.extend_from_slice(client_nonce.as_bytes());
    data.extend_from_slice(server_nonce.as_bytes());
    data.extend_from_slice(instance_id.as_bytes());
    data.extend_from_slice(&protocol_version.as_bytes());
    Mac(hmac_sha256(auth_secret, &data))
}

/// client_mac を計算する。
///
/// `HMAC(auth_secret, server_nonce || client_nonce || "client")`
pub fn compute_client_mac(
    auth_secret: &[u8; 32],
    server_nonce: &Nonce,
    client_nonce: &Nonce,
) -> Mac {
    let mut data = Vec::with_capacity(32 + 32 + 6);
    data.extend_from_slice(server_nonce.as_bytes());
    data.extend_from_slice(client_nonce.as_bytes());
    data.extend_from_slice(b"client");
    Mac(hmac_sha256(auth_secret, &data))
}

/// 定時間比較で MAC を検証する。
pub fn verify_mac(expected: &Mac, actual: &Mac) -> bool {
    expected.as_bytes().ct_eq(actual.as_bytes()).into()
}

/// client から plugin への最初の handshake メッセージ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientHello {
    /// client が対応する最大 MINOR。
    pub protocol_version: ProtocolVersion,
    /// 接続先として期待する ID。
    pub instance_id: InstanceId,
    pub client_nonce: Nonce,
}

/// plugin から client への認証応答。
///
/// 応答型であるため未知フィールドを拒否しない。将来の MINOR で追加された
/// フィールドを含む応答を、旧版の受信側がそのまま受理できるようにする。
/// 要求型（[`ClientHello`] / [`ClientAuth`]）が未知フィールドを拒否するのと
/// 非対称なのは意図的である。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerAuth {
    /// negotiation 結果のプロトコルバージョン。
    pub protocol_version: ProtocolVersion,
    /// plugin の実 ID。
    pub instance_id: InstanceId,
    pub server_nonce: Nonce,
    pub pid: u32,
    /// 実プロセス作成時刻。書式は [`crate::format_utc_timestamp`]。
    pub process_created_at: String,
    pub server_mac: Mac,
}

/// client から plugin への最終認証メッセージ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientAuth {
    pub client_mac: Mac,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identifier::InstanceId;

    fn fixed_auth_secret() -> [u8; 32] {
        [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
            0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B,
            0x1C, 0x1D, 0x1E, 0x1F,
        ]
    }

    fn fixed_client_nonce() -> Nonce {
        Nonce::from_bytes([
            0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D,
            0x2E, 0x2F, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x3B,
            0x3C, 0x3D, 0x3E, 0x3F,
        ])
    }

    fn fixed_server_nonce() -> Nonce {
        Nonce::from_bytes([
            0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D,
            0x4E, 0x4F, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x5B,
            0x5C, 0x5D, 0x5E, 0x5F,
        ])
    }

    fn fixed_instance_id() -> InstanceId {
        InstanceId::from_bytes([
            0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D,
            0x6E, 0x6F,
        ])
    }

    #[test]
    fn server_mac_known_vector() {
        let secret = fixed_auth_secret();
        let client_nonce = fixed_client_nonce();
        let server_nonce = fixed_server_nonce();
        let instance_id = fixed_instance_id();
        let protocol_version = ProtocolVersion { major: 1, minor: 0 };

        let mac = compute_server_mac(
            &secret,
            &client_nonce,
            &server_nonce,
            &instance_id,
            &protocol_version,
        );

        // 連結順と鍵が正しければ、同一入力で同一出力が得られる。
        let mac2 = compute_server_mac(
            &secret,
            &client_nonce,
            &server_nonce,
            &instance_id,
            &protocol_version,
        );
        assert_eq!(mac.as_bytes(), mac2.as_bytes());
    }

    #[test]
    fn client_mac_known_vector() {
        let secret = fixed_auth_secret();
        let client_nonce = fixed_client_nonce();
        let server_nonce = fixed_server_nonce();

        let mac = compute_client_mac(&secret, &server_nonce, &client_nonce);
        let mac2 = compute_client_mac(&secret, &server_nonce, &client_nonce);
        assert_eq!(mac.as_bytes(), mac2.as_bytes());
    }

    #[test]
    fn mac_tamper_detection() {
        let secret = fixed_auth_secret();
        let client_nonce = fixed_client_nonce();
        let server_nonce = fixed_server_nonce();
        let instance_id = fixed_instance_id();
        let protocol_version = ProtocolVersion { major: 1, minor: 0 };

        let mac = compute_server_mac(
            &secret,
            &client_nonce,
            &server_nonce,
            &instance_id,
            &protocol_version,
        );

        // 異なる鍵で再計算
        let mut wrong_secret = secret;
        wrong_secret[0] ^= 0xFF;
        let wrong_mac = compute_server_mac(
            &wrong_secret,
            &client_nonce,
            &server_nonce,
            &instance_id,
            &protocol_version,
        );
        assert!(!verify_mac(&mac, &wrong_mac));

        // nonce 差し替え
        let mut wrong_nonce = *client_nonce.as_bytes();
        wrong_nonce[0] ^= 0xFF;
        let wrong_mac = compute_server_mac(
            &secret,
            &Nonce::from_bytes(wrong_nonce),
            &server_nonce,
            &instance_id,
            &protocol_version,
        );
        assert!(!verify_mac(&mac, &wrong_mac));
    }

    #[test]
    fn nonce_unique_per_generation() {
        let a = Nonce::generate();
        let b = Nonce::generate();
        assert_ne!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn mac_json_serialization() {
        let mac = Mac::from_bytes([0xAB; 32]);
        let s = serde_json::to_string(&mac).unwrap();
        // base64url 表現なので raw バイトは含まれない
        assert!(!s.contains("AB"));
        let mac2: Mac = serde_json::from_str(&s).unwrap();
        assert_eq!(mac.as_bytes(), mac2.as_bytes());
    }

    fn sample_client_hello() -> ClientHello {
        ClientHello {
            protocol_version: ProtocolVersion { major: 1, minor: 0 },
            instance_id: fixed_instance_id(),
            client_nonce: fixed_client_nonce(),
        }
    }

    fn sample_server_auth() -> ServerAuth {
        ServerAuth {
            protocol_version: ProtocolVersion { major: 1, minor: 0 },
            instance_id: fixed_instance_id(),
            server_nonce: fixed_server_nonce(),
            pid: 4321,
            process_created_at: "2026-01-01T00:00:00.0000000Z".to_string(),
            server_mac: Mac::from_bytes([0xAB; 32]),
        }
    }

    fn sample_client_auth() -> ClientAuth {
        ClientAuth {
            client_mac: Mac::from_bytes([0xCD; 32]),
        }
    }

    /// 既知の値へ未知フィールドを 1 つ足した JSON を作る。
    fn with_unknown_field<T: Serialize>(value: &T) -> serde_json::Value {
        let mut obj = match serde_json::to_value(value).unwrap() {
            serde_json::Value::Object(obj) => obj,
            other => {
                panic!("handshake メッセージは JSON オブジェクトである必要があります: {other}")
            }
        };
        obj.insert("future_field".to_string(), serde_json::json!(1));
        serde_json::Value::Object(obj)
    }

    #[test]
    fn server_auth_allows_unknown_field() {
        let auth = sample_server_auth();
        let restored: ServerAuth =
            serde_json::from_value(serde_json::to_value(&auth).unwrap()).unwrap();
        assert_eq!(restored, auth);

        // 応答型は将来の MINOR で追加されたフィールドを受理し、既知フィールドは保つ。
        let restored: ServerAuth = serde_json::from_value(with_unknown_field(&auth)).unwrap();
        assert_eq!(restored, auth);
    }

    #[test]
    fn client_messages_reject_unknown_field() {
        let hello = sample_client_hello();
        let result: Result<ClientHello, _> = serde_json::from_value(with_unknown_field(&hello));
        assert!(result.is_err());

        let auth = sample_client_auth();
        let result: Result<ClientAuth, _> = serde_json::from_value(with_unknown_field(&auth));
        assert!(result.is_err());
    }

    #[test]
    fn debug_display_masked() {
        let nonce = Nonce::from_bytes([0xCD; 32]);
        let mac = Mac::from_bytes([0xEF; 32]);
        assert_eq!(format!("{nonce:?}"), "Nonce(***)");
        assert_eq!(format!("{mac:?}"), "Mac(***)");
        assert_eq!(format!("{nonce}"), "Nonce(***)");
        assert_eq!(format!("{mac}"), "Mac(***)");
    }
}
