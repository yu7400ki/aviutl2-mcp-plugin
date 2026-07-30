//! descriptor DTO と InstanceInfo。

use crate::edit_info::SceneRef;
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
    /// registry ファイルの形式版。現行 1。
    ///
    /// [`ProtocolVersion`] とは独立に上げる。IPC の版を変えないまま
    /// フィールドの意味だけを変える場合、形式の違いを表せるのはこの値だけである。
    /// 既知でない値の descriptor は解釈せず、削除もしない。
    pub schema_version: u32,
    /// 対応プロトコルバージョン。
    pub protocol_version: ProtocolVersion,
    pub instance_id: InstanceId,
    pub pipe_name: String,
    pub auth_secret: AuthSecret,
    pub pid: u32,
    /// プロセス作成時刻。書式は [`crate::format_utc_timestamp`]。
    pub process_created_at: String,
    /// HWND。書式は [`crate::format_hwnd`]。取得不能時は None。
    pub hwnd: Option<String>,
    /// 起動時刻。書式は [`crate::format_utc_timestamp`]。
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
    /// 起動時刻。書式は [`crate::format_utc_timestamp`]。
    pub started_at: String,
    pub project: Option<InstanceProject>,
    /// 現在シーンの参照。取得不能時は None。
    pub scene: Option<SceneRef>,
}

/// `InstanceInfo` 内のプロジェクト情報。
///
/// 応答型の内側であるため未知フィールドを拒否しない。将来の MINOR で
/// 追加されたフィールドを含む応答を、旧版の受信側が受理できるようにする。
///
/// `epoch` / `revision` / `modified` は取得できていない状態を `None` で表す。
/// 特に `modified` は「未保存の変更が無い」と「未取得」を混同すると保存確認の
/// 要否を誤らせるため、既定値で埋めずに欠落として表す。
///
/// `display_name` と `path` はプロジェクトファイルに由来するため、未保存の
/// プロジェクトではいずれも存在しない。名前を作って埋めると、実在するファイル名と
/// 区別が付かなくなる。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceProject {
    /// プロジェクトの表示名。未保存プロジェクトでは None。
    pub display_name: Option<String>,
    /// プロジェクトファイルのパス。未保存プロジェクトでは None。
    pub path: Option<String>,
    /// プロジェクトの epoch。未取得のときは None。
    pub epoch: Option<String>,
    /// プロジェクトの revision。未取得のときは None。
    pub revision: Option<u64>,
    /// 未保存の変更があるか。未取得のときは None。
    pub modified: Option<bool>,
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
            process_created_at: "2026-01-01T00:00:00.0000000Z".to_string(),
            hwnd: Some("0x0000000012345678".to_string()),
            started_at: "2026-01-01T00:00:00.0000000Z".to_string(),
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

    fn sample_instance_info() -> InstanceInfo {
        InstanceInfo {
            instance_id: InstanceId::new_v4(),
            state: InstanceState::Busy,
            pid: 5678,
            started_at: "2026-01-01T00:00:00.0000000Z".to_string(),
            project: Some(InstanceProject {
                display_name: Some("Project".to_string()),
                path: Some(r"C:\project.aup".to_string()),
                epoch: Some("78be92d1-c8c9-44c6-ae52-387548971468".to_string()),
                revision: Some(42),
                modified: Some(true),
            }),
            scene: Some(SceneRef {
                id: 0,
                name: Some("Scene 1".to_string()),
            }),
        }
    }

    #[test]
    fn instance_info_roundtrip() {
        let info = sample_instance_info();
        let s = serde_json::to_string(&info).unwrap();
        // auth_secret は含まれない
        assert!(!s.contains("auth_secret"));
        let info2: InstanceInfo = serde_json::from_str(&s).unwrap();
        assert_eq!(info, info2);
    }

    #[test]
    fn instance_info_allows_unknown_optional_fields() {
        let s = r#"{"instance_id":"8df98c04-e7c2-4f98-b3ce-fc1c39d76414","state":"ready","pid":1,"started_at":"x","project":{"display_name":"a","path":"b","epoch":"e","revision":1,"modified":false},"scene":{"id":0,"name":null},"future":1}"#;
        let result: Result<InstanceInfo, _> = serde_json::from_str(s);
        assert!(result.is_ok());
    }

    #[test]
    fn instance_info_accepts_reduced_project_shape() {
        // display_name と path だけを持つ旧来の形を、拡張フィールド未取得として受理する。
        let s = r#"{"instance_id":"8df98c04-e7c2-4f98-b3ce-fc1c39d76414","state":"ready","pid":1,"started_at":"x","project":{"display_name":"a","path":"b"}}"#;
        let info: InstanceInfo = serde_json::from_str(s).unwrap();
        let project = info.project.expect("project が読み取れる");
        assert_eq!(project.display_name.as_deref(), Some("a"));
        assert_eq!(project.path.as_deref(), Some("b"));
        assert_eq!(project.epoch, None);
        assert_eq!(project.revision, None);
        assert_eq!(project.modified, None);
        // scene を持たない応答も受理する。
        assert_eq!(info.scene, None);
    }

    #[test]
    fn instance_project_distinguishes_unknown_from_unmodified() {
        // 「未取得」と「未保存の変更なし」は別の値として表現される。
        let unknown: InstanceProject =
            serde_json::from_str(r#"{"display_name":"a","path":null}"#).unwrap();
        let unmodified: InstanceProject =
            serde_json::from_str(r#"{"display_name":"a","path":null,"modified":false}"#).unwrap();
        assert_eq!(unknown.modified, None);
        assert_eq!(unmodified.modified, Some(false));
        assert_ne!(unknown, unmodified);
    }

    #[test]
    fn instance_project_allows_unknown_optional_fields() {
        // 応答型の内側でも将来の MINOR 追加を受理する。
        let info = sample_instance_info();
        let mut value = serde_json::to_value(&info).unwrap();
        value["project"]
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::json!(1));
        let restored: InstanceInfo = serde_json::from_value(value).unwrap();
        assert_eq!(restored, info);
    }

    #[test]
    fn unsaved_project_has_neither_name_nor_path() {
        // 未保存プロジェクトはファイルに由来する値を持たない。名前を作って埋めると
        // 実在するファイル名と区別が付かなくなる。
        let info = InstanceInfo {
            project: Some(InstanceProject {
                display_name: None,
                path: None,
                epoch: Some("78be92d1-c8c9-44c6-ae52-387548971468".to_string()),
                revision: Some(0),
                modified: Some(true),
            }),
            ..sample_instance_info()
        };
        let value = serde_json::to_value(&info).unwrap();
        assert_eq!(value["project"]["display_name"], serde_json::Value::Null);
        assert_eq!(value["project"]["path"], serde_json::Value::Null);
        // ファイルに由来する値が無くても、実測した状態は運ばれる。
        assert_eq!(value["project"]["modified"], serde_json::json!(true));
        let restored: InstanceInfo = serde_json::from_value(value).unwrap();
        assert_eq!(restored, info);
    }

    #[test]
    fn instance_info_scene_can_be_absent() {
        let info = InstanceInfo {
            scene: None,
            ..sample_instance_info()
        };
        let value = serde_json::to_value(&info).unwrap();
        assert_eq!(value["scene"], serde_json::Value::Null);
        let restored: InstanceInfo = serde_json::from_value(value).unwrap();
        assert_eq!(restored.scene, None);
    }

    #[test]
    fn descriptor_project_still_rejects_unknown_fields() {
        // registry ファイルの入力型は strict のままとする。
        let result: Result<DescriptorProject, _> =
            serde_json::from_str(r#"{"display_name":"x","path":"y","future":1}"#);
        assert!(result.is_err());
    }
}
