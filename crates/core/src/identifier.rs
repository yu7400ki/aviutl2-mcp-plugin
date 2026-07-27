//! 識別子型と pipe_name 生成規則。

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// プロセス寿命ごとの操作対象主キー（UUID v4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstanceId(Uuid);

impl InstanceId {
    pub fn new_v4() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(Uuid::from_bytes(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.as_hyphenated())
    }
}

/// IPC 要求ごとの相関 ID（UUID）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(Uuid);

impl RequestId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

/// IPC プロトコルバージョン MAJOR.MINOR。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    /// 現行プロトコルバージョン。
    pub const CURRENT: Self = Self { major: 1, minor: 0 };

    /// 文字列表現を返す（例: "1.0"）。
    pub fn as_str(&self) -> String {
        format!("{}.{}", self.major, self.minor)
    }

    /// ASCII バイト列表現を返す（例: b"1.0"）。
    pub fn as_bytes(&self) -> Vec<u8> {
        self.as_str().into_bytes()
    }
}

impl Serialize for ProtocolVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProtocolVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_protocol_version(&s).map_err(serde::de::Error::custom)
    }
}

fn parse_protocol_version(s: &str) -> Result<ProtocolVersion, String> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 2 {
        return Err(format!(
            "プロトコルバージョンは MAJOR.MINOR 形式である必要があります: {}",
            s
        ));
    }
    let major = parts[0]
        .parse::<u16>()
        .map_err(|e| format!("major の解析に失敗しました: {} ({e})", parts[0]))?;
    let minor = parts[1]
        .parse::<u16>()
        .map_err(|e| format!("minor の解析に失敗しました: {} ({e})", parts[1]))?;
    Ok(ProtocolVersion { major, minor })
}

/// 指定したインスタンスに対する named pipe 名を生成する。
///
/// 形式: `\\.\pipe\aviutl2-mcp\v1\{instance_id}`
/// `v1` は pipe 名前空間の版であり、protocol_version の MAJOR とは独立に扱う。
pub fn pipe_name_for(instance_id: &InstanceId) -> String {
    format!(r"\\.\pipe\aviutl2-mcp\v1\{}", instance_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_id_new_v4_is_valid_uuid() {
        let id = InstanceId::new_v4();
        assert_eq!(id.0.get_version_num(), 4);
    }

    #[test]
    fn instance_id_roundtrip_bytes() {
        let id = InstanceId::new_v4();
        let bytes = *id.as_bytes();
        let id2 = InstanceId::from_bytes(bytes);
        assert_eq!(id, id2);
    }

    #[test]
    fn instance_id_json_string() {
        let id = InstanceId::new_v4();
        let s = serde_json::to_string(&id).unwrap();
        let id2: InstanceId = serde_json::from_str(&s).unwrap();
        assert_eq!(id, id2);
    }

    #[test]
    fn request_id_new_unique() {
        let a = RequestId::new();
        let b = RequestId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn protocol_version_json_string() {
        let v = ProtocolVersion { major: 1, minor: 2 };
        let s = serde_json::to_string(&v).unwrap();
        assert_eq!(s, "\"1.2\"");
        let v2: ProtocolVersion = serde_json::from_str(&s).unwrap();
        assert_eq!(v, v2);
    }

    #[test]
    fn protocol_version_invalid_format() {
        let err = parse_protocol_version("1").unwrap_err();
        assert!(err.contains("MAJOR.MINOR"));
    }

    #[test]
    fn pipe_name_format() {
        let id = InstanceId::from_bytes([
            0x8d, 0xf9, 0x8c, 0x04, 0xe7, 0xc2, 0x4f, 0x98, 0xb3, 0xce, 0xfc, 0x1c, 0x39, 0xd7,
            0x64, 0x14,
        ]);
        let name = pipe_name_for(&id);
        assert_eq!(
            name,
            r"\\.\pipe\aviutl2-mcp\v1\8df98c04-e7c2-4f98-b3ce-fc1c39d76414"
        );
    }
}
