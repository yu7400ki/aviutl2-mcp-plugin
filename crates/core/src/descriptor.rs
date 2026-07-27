//! descriptor DTO と InstanceInfo。

use crate::identifier::{InstanceId, ProtocolVersion};
use crate::state::InstanceState;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// 32 バイトの共有認証鍵。
///
/// `Debug` / `Display` はマスク表示。`Serialize` は descriptor 書き込み専用経路で
/// base64url 出力する。`InstanceInfo` や `ErrorObject` には含めない。
#[derive(Clone, PartialEq, Eq)]
pub struct AuthSecret([u8; 32]);

impl AuthSecret {
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

impl fmt::Debug for AuthSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AuthSecret(***)")
    }
}

impl fmt::Display for AuthSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AuthSecret(***)")
    }
}

impl Serialize for AuthSecret {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let encoded = URL_SAFE_NO_PAD.encode(self.0);
        serializer.serialize_str(&encoded)
    }
}

impl<'de> Deserialize<'de> for AuthSecret {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let bytes = URL_SAFE_NO_PAD.decode(&s).map_err(|e| {
            serde::de::Error::custom(format!("auth_secret の base64 デコードに失敗しました: {e}"))
        })?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom(format!(
                "auth_secret は 32 バイトである必要があります: 実際は {} バイト",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(AuthSecret(arr))
    }
}

/// registry に書かれるインスタンス登録情報。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceDescriptor {
    /// schema バージョン。現行 1。
    pub schema_version: u32,
    /// 対応プロトコルバージョン。
    pub protocol_version: ProtocolVersion,
    pub instance_id: InstanceId,
    pub pipe_name: String,
    pub auth_secret: AuthSecret,
    pub pid: u32,
    /// プロセス作成時刻（RFC3339 / ISO8601 UTC）。
    pub process_created_at: String,
    /// HWND（取得不能時は None）。
    pub hwnd: Option<String>,
    /// 起動時刻（RFC3339 UTC）。
    pub started_at: String,
    pub state: InstanceState,
    pub project: Option<DescriptorProject>,
}

/// descriptor 内のプロジェクト情報。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DescriptorProject {
    pub display_name: String,
    pub path: String,
}

/// registry から取得した公開可能なインスタンス情報。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub instance_id: InstanceId,
    pub state: InstanceState,
    pub pid: u32,
    /// 起動時刻（RFC3339 UTC）。
    pub started_at: String,
    pub project: Option<InstanceProject>,
}

/// `InstanceInfo` 内のプロジェクト情報。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceProject {
    pub display_name: String,
    pub path: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identifier::InstanceId;

    #[test]
    fn auth_secret_masked() {
        let secret = AuthSecret::from_bytes([0xDE; 32]);
        assert_eq!(format!("{secret:?}"), "AuthSecret(***)");
        assert_eq!(format!("{secret}"), "AuthSecret(***)");
    }

    #[test]
    fn auth_secret_json_roundtrip() {
        let secret = AuthSecret::from_bytes([0xAB; 32]);
        let s = serde_json::to_string(&secret).unwrap();
        assert!(!s.contains("AB"));
        let secret2: AuthSecret = serde_json::from_str(&s).unwrap();
        assert_eq!(secret.as_bytes(), secret2.as_bytes());
    }

    #[test]
    fn descriptor_roundtrip() {
        let descriptor = InstanceDescriptor {
            schema_version: 1,
            protocol_version: ProtocolVersion { major: 1, minor: 0 },
            instance_id: InstanceId::new_v4(),
            pipe_name: r"\\.\pipe\aviutl2-mcp\v1\test".to_string(),
            auth_secret: AuthSecret::generate(),
            pid: 1234,
            process_created_at: "2026-01-01T00:00:00Z".to_string(),
            hwnd: Some("0x12345678".to_string()),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            state: InstanceState::Ready,
            project: Some(DescriptorProject {
                display_name: "Project".to_string(),
                path: r"C:\project.aup".to_string(),
            }),
        };
        let s = serde_json::to_string(&descriptor).unwrap();
        // auth_secret は JSON には出ているが、デバッグ系ではマスクされている
        assert!(s.contains("auth_secret"));
        let descriptor2: InstanceDescriptor = serde_json::from_str(&s).unwrap();
        assert_eq!(descriptor, descriptor2);
    }

    #[test]
    fn descriptor_rejects_unknown_fields() {
        let s = r#"{"schema_version":1,"protocol_version":"1.0","instance_id":"8df98c04-e7c2-4f98-b3ce-fc1c39d76414","pipe_name":"x","auth_secret":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","pid":1,"process_created_at":"x","hwnd":null,"started_at":"x","state":"ready","project":null,"extra":1}"#;
        let result: Result<InstanceDescriptor, _> = serde_json::from_str(s);
        assert!(result.is_err());
    }

    #[test]
    fn instance_info_roundtrip() {
        let info = InstanceInfo {
            instance_id: InstanceId::new_v4(),
            state: InstanceState::Busy,
            pid: 5678,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            project: Some(InstanceProject {
                display_name: "Project".to_string(),
                path: r"C:\project.aup".to_string(),
            }),
        };
        let s = serde_json::to_string(&info).unwrap();
        // auth_secret は含まれない
        assert!(!s.contains("auth_secret"));
        let info2: InstanceInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(info, info2);
    }

    #[test]
    fn instance_info_allows_unknown_optional_fields() {
        let s = r#"{"instance_id":"8df98c04-e7c2-4f98-b3ce-fc1c39d76414","state":"ready","pid":1,"started_at":"x","project":null,"future":1}"#;
        let result: Result<InstanceInfo, _> = serde_json::from_str(s);
        assert!(result.is_ok());
    }
}
