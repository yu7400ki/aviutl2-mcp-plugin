//! IPC 要求・応答の Envelope。

use crate::error::ErrorObject;
use crate::identifier::{InstanceId, ProtocolVersion, RequestId};
use crate::state::InstanceState;
use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashSet;
use std::fmt;

/// 要求 Envelope の種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Request,
}

impl Serialize for RequestKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            RequestKind::Request => serializer.serialize_str("request"),
        }
    }
}

impl<'de> Deserialize<'de> for RequestKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s == "request" {
            Ok(RequestKind::Request)
        } else {
            Err(de::Error::custom(format!(
                "kind は \"request\" である必要があります: 実際は {:?}",
                s
            )))
        }
    }
}

/// 応答 Envelope の種別。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseKind {
    Response,
    /// 未知の kind 値を raw 保持。
    Unknown(String),
}

impl Serialize for ResponseKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            ResponseKind::Response => serializer.serialize_str("response"),
            ResponseKind::Unknown(s) => serializer.serialize_str(s),
        }
    }
}

impl<'de> Deserialize<'de> for ResponseKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s == "response" {
            Ok(ResponseKind::Response)
        } else {
            Ok(ResponseKind::Unknown(s))
        }
    }
}

/// 要求 Envelope。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    pub kind: RequestKind,
    /// 採用プロトコルバージョン。
    pub protocol_version: ProtocolVersion,
    pub request_id: RequestId,
    /// 接続先インスタンス ID。
    pub instance_id: InstanceId,
    /// deadline（Unix 時刻ミリ秒）。
    pub deadline_unix_ms: Option<u64>,
    pub operation: String,
    /// operation 依存のパラメータ。
    pub params: serde_json::Value,
}

impl RequestEnvelope {
    /// ping 要求を組み立てる。
    ///
    /// deadline は未指定で作られる。期限を付ける場合は
    /// [`RequestEnvelope::with_deadline`] を続けて呼ぶ。
    pub fn ping(
        protocol_version: ProtocolVersion,
        request_id: RequestId,
        instance_id: InstanceId,
    ) -> Self {
        Self {
            kind: RequestKind::Request,
            protocol_version,
            request_id,
            instance_id,
            deadline_unix_ms: None,
            operation: "ping".to_string(),
            params: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    /// deadline（Unix 時刻ミリ秒）を設定する。
    ///
    /// `None` は期限未指定を表し、受信側は自身の上限だけを適用する。
    pub fn with_deadline(mut self, deadline_unix_ms: Option<u64>) -> Self {
        self.deadline_unix_ms = deadline_unix_ms;
        self
    }
}

/// 応答結果。
#[derive(Debug, Clone, PartialEq)]
pub enum ResponseResult {
    Ok { result: serde_json::Value },
    Err { error: ErrorObject },
}

impl Serialize for ResponseResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(2))?;
        match self {
            ResponseResult::Ok { result } => {
                map.serialize_entry("ok", &true)?;
                map.serialize_entry("result", result)?;
            }
            ResponseResult::Err { error } => {
                map.serialize_entry("ok", &false)?;
                map.serialize_entry("error", error)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ResponseResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ResponseResultVisitor;
        impl<'de> Visitor<'de> for ResponseResultVisitor {
            type Value = ResponseResult;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("ok boolean と result/error フィールドを持つオブジェクト")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut ok = None;
                let mut result = None;
                let mut error = None;
                let mut seen = HashSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom(format!("重複した key です: {}", key)));
                    }
                    match key.as_str() {
                        "ok" => ok = Some(map.next_value::<bool>()?),
                        "result" => result = Some(map.next_value::<serde_json::Value>()?),
                        "error" => error = Some(map.next_value::<ErrorObject>()?),
                        _ => {
                            // 前方互換のため、将来の MINOR で追加される未知フィールドは
                            // 読み飛ばして許容する。
                            map.next_value::<serde_json::Value>()?;
                        }
                    }
                }
                let ok = ok.ok_or_else(|| de::Error::missing_field("ok"))?;
                if ok {
                    let result = result.ok_or_else(|| de::Error::missing_field("result"))?;
                    Ok(ResponseResult::Ok { result })
                } else {
                    let error = error.ok_or_else(|| de::Error::missing_field("error"))?;
                    Ok(ResponseResult::Err { error })
                }
            }
        }
        deserializer.deserialize_map(ResponseResultVisitor)
    }
}

/// 応答 Envelope。
#[derive(Debug, Clone, PartialEq)]
pub struct ResponseEnvelope {
    /// "response" または未知の kind。
    pub kind: ResponseKind,
    /// 採用プロトコルバージョン。
    pub protocol_version: ProtocolVersion,
    pub request_id: RequestId,
    /// 接続先インスタンス ID。
    pub instance_id: InstanceId,
    pub result: ResponseResult,
}

/// ping 応答が運ぶプロジェクトの状態。
///
/// 応答型の内側であるため未知フィールドを拒否しない。将来の MINOR で追加された
/// フィールドを含む応答を、旧版の受信側が受理できるようにする。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PongProject {
    /// プロジェクトの epoch。
    pub epoch: String,
    /// プロジェクトの revision。
    pub revision: u64,
    /// 最後の保存以降に変更があるか。
    pub modified: bool,
}

/// ping 応答の `result`。
///
/// `project` は接続先が載せなかった場合に `None` となる。既定値で埋めると
/// 「未取得」と実測値が区別できなくなるため、欠落のまま扱う。
///
/// 未知フィールドを拒否せず、`project` を持たない縮約形も受理する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PongResult {
    /// 接続先のライフサイクル状態。
    pub state: InstanceState,
    /// 接続先のインスタンス ID。
    pub instance_id: InstanceId,
    /// プロジェクトの状態。取得できない場合は None。
    #[serde(default)]
    pub project: Option<PongProject>,
}

impl PongResult {
    /// プロジェクトの状態を伴わない ping 応答を作る。
    pub fn new(instance_id: InstanceId, state: InstanceState) -> Self {
        Self {
            state,
            instance_id,
            project: None,
        }
    }

    /// プロジェクトの状態を添える。
    pub fn with_project(mut self, project: PongProject) -> Self {
        self.project = Some(project);
        self
    }
}

impl ResponseEnvelope {
    pub fn pong(
        protocol_version: ProtocolVersion,
        request_id: RequestId,
        result: &PongResult,
    ) -> Self {
        Self {
            kind: ResponseKind::Response,
            protocol_version,
            request_id,
            instance_id: result.instance_id,
            result: ResponseResult::Ok {
                result: serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
            },
        }
    }
}

impl Serialize for ResponseEnvelope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("kind", &self.kind)?;
        map.serialize_entry("protocol_version", &self.protocol_version)?;
        map.serialize_entry("request_id", &self.request_id)?;
        map.serialize_entry("instance_id", &self.instance_id)?;
        match &self.result {
            ResponseResult::Ok { result } => {
                map.serialize_entry("ok", &true)?;
                map.serialize_entry("result", result)?;
            }
            ResponseResult::Err { error } => {
                map.serialize_entry("ok", &false)?;
                map.serialize_entry("error", error)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ResponseEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ResponseEnvelopeVisitor;
        impl<'de> Visitor<'de> for ResponseEnvelopeVisitor {
            type Value = ResponseEnvelope;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("response envelope")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut kind = None;
                let mut protocol_version = None;
                let mut request_id = None;
                let mut instance_id = None;
                let mut ok = None;
                let mut result = None;
                let mut error = None;
                let mut seen = HashSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom(format!("重複した key です: {}", key)));
                    }
                    match key.as_str() {
                        "kind" => kind = Some(map.next_value::<ResponseKind>()?),
                        "protocol_version" => {
                            protocol_version = Some(map.next_value::<ProtocolVersion>()?)
                        }
                        "request_id" => request_id = Some(map.next_value::<RequestId>()?),
                        "instance_id" => instance_id = Some(map.next_value::<InstanceId>()?),
                        "ok" => ok = Some(map.next_value::<bool>()?),
                        "result" => result = Some(map.next_value::<serde_json::Value>()?),
                        "error" => error = Some(map.next_value::<ErrorObject>()?),
                        _ => {
                            // 前方互換のため、将来の MINOR で追加される未知フィールドは
                            // 読み飛ばして許容する。
                            map.next_value::<serde_json::Value>()?;
                        }
                    }
                }
                let kind = kind.ok_or_else(|| de::Error::missing_field("kind"))?;
                let protocol_version =
                    protocol_version.ok_or_else(|| de::Error::missing_field("protocol_version"))?;
                let request_id =
                    request_id.ok_or_else(|| de::Error::missing_field("request_id"))?;
                let instance_id =
                    instance_id.ok_or_else(|| de::Error::missing_field("instance_id"))?;
                let ok = ok.ok_or_else(|| de::Error::missing_field("ok"))?;
                let result = if ok {
                    ResponseResult::Ok {
                        result: result.ok_or_else(|| de::Error::missing_field("result"))?,
                    }
                } else {
                    ResponseResult::Err {
                        error: error.ok_or_else(|| de::Error::missing_field("error"))?,
                    }
                };
                Ok(ResponseEnvelope {
                    kind,
                    protocol_version,
                    request_id,
                    instance_id,
                    result,
                })
            }
        }
        deserializer.deserialize_map(ResponseEnvelopeVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ErrorCode, ErrorObject};
    use crate::identifier::InstanceId;
    use crate::state::InstanceState;

    #[test]
    fn request_envelope_roundtrip() {
        let env = RequestEnvelope {
            kind: RequestKind::Request,
            protocol_version: ProtocolVersion { major: 1, minor: 0 },
            request_id: RequestId::new(),
            instance_id: InstanceId::new_v4(),
            deadline_unix_ms: Some(1234567890),
            operation: "ping".to_string(),
            params: serde_json::json!({}),
        };
        let s = serde_json::to_string(&env).unwrap();
        let env2: RequestEnvelope = serde_json::from_str(&s).unwrap();
        assert_eq!(env, env2);
    }

    #[test]
    fn request_envelope_rejects_unknown_field() {
        let s = r#"{"kind":"request","protocol_version":"1.0","request_id":"8df98c04-e7c2-4f98-b3ce-fc1c39d76414","instance_id":"8df98c04-e7c2-4f98-b3ce-fc1c39d76414","deadline_unix_ms":null,"operation":"ping","params":{},"unknown":1}"#;
        let result: Result<RequestEnvelope, _> = serde_json::from_str(s);
        assert!(result.is_err());
    }

    #[test]
    fn request_kind_invalid_rejected() {
        let s = "\"request_v2\"";
        let result: Result<RequestKind, _> = serde_json::from_str(s);
        assert!(result.is_err());
    }

    #[test]
    fn response_envelope_ok_roundtrip() {
        let env = ResponseEnvelope {
            kind: ResponseKind::Response,
            protocol_version: ProtocolVersion { major: 1, minor: 0 },
            request_id: RequestId::new(),
            instance_id: InstanceId::new_v4(),
            result: ResponseResult::Ok {
                result: serde_json::json!({"state": "ready"}),
            },
        };
        let s = serde_json::to_string(&env).unwrap();
        let env2: ResponseEnvelope = serde_json::from_str(&s).unwrap();
        assert_eq!(env, env2);
    }

    #[test]
    fn response_envelope_err_roundtrip() {
        let env = ResponseEnvelope {
            kind: ResponseKind::Response,
            protocol_version: ProtocolVersion { major: 1, minor: 0 },
            request_id: RequestId::new(),
            instance_id: InstanceId::new_v4(),
            result: ResponseResult::Err {
                error: ErrorObject::new(ErrorCode::HostBusy, "busy", true),
            },
        };
        let s = serde_json::to_string(&env).unwrap();
        let env2: ResponseEnvelope = serde_json::from_str(&s).unwrap();
        assert_eq!(env, env2);
    }

    #[test]
    fn response_envelope_allows_unknown_optional_fields() {
        let s = r#"{"kind":"response","protocol_version":"1.0","request_id":"8df98c04-e7c2-4f98-b3ce-fc1c39d76414","instance_id":"8df98c04-e7c2-4f98-b3ce-fc1c39d76414","ok":true,"result":{},"future_field":1}"#;
        let result: Result<ResponseEnvelope, _> = serde_json::from_str(s);
        assert!(result.is_ok());
    }

    #[test]
    fn response_kind_unknown_preserved() {
        let s = "\"response_v2\"";
        let kind: ResponseKind = serde_json::from_str(s).unwrap();
        assert_eq!(kind, ResponseKind::Unknown("response_v2".to_string()));
        let s2 = serde_json::to_string(&kind).unwrap();
        assert_eq!(s2, "\"response_v2\"");
    }

    #[test]
    fn response_result_json_structure() {
        let ok = ResponseResult::Ok {
            result: serde_json::json!({"x": 1}),
        };
        let s = serde_json::to_string(&ok).unwrap();
        assert!(s.contains("\"ok\":true"));
        assert!(s.contains("\"result\""));

        let err = ResponseResult::Err {
            error: ErrorObject::new(ErrorCode::InternalError, "oops", false),
        };
        let s = serde_json::to_string(&err).unwrap();
        assert!(s.contains("\"ok\":false"));
        assert!(s.contains("\"error\""));
    }

    #[test]
    fn ping_has_no_deadline_by_default() {
        let request = RequestEnvelope::ping(
            ProtocolVersion::CURRENT,
            RequestId::new(),
            InstanceId::new_v4(),
        );
        assert_eq!(request.deadline_unix_ms, None);
    }

    #[test]
    fn with_deadline_sets_and_clears_deadline() {
        let request = RequestEnvelope::ping(
            ProtocolVersion::CURRENT,
            RequestId::new(),
            InstanceId::new_v4(),
        );

        let with_deadline = request.clone().with_deadline(Some(1_785_144_000_000));
        assert_eq!(with_deadline.deadline_unix_ms, Some(1_785_144_000_000));

        let s = serde_json::to_string(&with_deadline).unwrap();
        let decoded: RequestEnvelope = serde_json::from_str(&s).unwrap();
        assert_eq!(decoded, with_deadline);

        assert_eq!(with_deadline.with_deadline(None).deadline_unix_ms, None);
    }

    #[test]
    fn ping_pong_roundtrip() {
        let instance_id = InstanceId::new_v4();
        let request_id = RequestId::new();
        let request = RequestEnvelope::ping(ProtocolVersion::CURRENT, request_id, instance_id);
        let s = serde_json::to_string(&request).unwrap();
        let request2: RequestEnvelope = serde_json::from_str(&s).unwrap();
        assert_eq!(request, request2);

        let response = ResponseEnvelope::pong(
            ProtocolVersion::CURRENT,
            request_id,
            &PongResult::new(instance_id, InstanceState::Ready),
        );
        let s = serde_json::to_string(&response).unwrap();
        let response2: ResponseEnvelope = serde_json::from_str(&s).unwrap();
        assert_eq!(response, response2);
    }

    #[test]
    fn pong_carries_the_project_state() {
        let instance_id = InstanceId::new_v4();
        let project = PongProject {
            epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            revision: 42,
            modified: true,
        };
        let result =
            PongResult::new(instance_id, InstanceState::Ready).with_project(project.clone());
        let response = ResponseEnvelope::pong(ProtocolVersion::CURRENT, RequestId::new(), &result);

        let ResponseResult::Ok { result: value } = &response.result else {
            panic!("pong が成功応答ではありません");
        };
        let restored: PongResult = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(restored, result);
        assert_eq!(restored.project, Some(project));
        assert_eq!(response.instance_id, instance_id);
    }

    #[test]
    fn pong_result_accepts_the_reduced_form() {
        // project を持たない応答は「未取得」として受理する。既定値で埋めると
        // 実測値と区別が付かなくなる。
        let instance_id = InstanceId::new_v4();
        let value = serde_json::json!({
            "state": "ready",
            "instance_id": instance_id,
        });
        let result: PongResult = serde_json::from_value(value).unwrap();
        assert_eq!(result.state, InstanceState::Ready);
        assert_eq!(result.instance_id, instance_id);
        assert_eq!(result.project, None);
    }

    #[test]
    fn pong_result_ignores_unknown_fields() {
        let instance_id = InstanceId::new_v4();
        let value = serde_json::json!({
            "state": "ready",
            "instance_id": instance_id,
            "project": {
                "epoch": "e",
                "revision": 1,
                "modified": false,
                "future": 1,
            },
            "future": 1,
        });
        let result: PongResult = serde_json::from_value(value).unwrap();
        assert_eq!(
            result.project,
            Some(PongProject {
                epoch: "e".to_string(),
                revision: 1,
                modified: false,
            })
        );
    }

    #[test]
    fn pong_result_requires_state_and_instance_id() {
        let instance_id = InstanceId::new_v4();
        for value in [
            serde_json::json!({ "state": "ready" }),
            serde_json::json!({ "instance_id": instance_id }),
            serde_json::json!({}),
        ] {
            assert!(
                serde_json::from_value::<PongResult>(value.clone()).is_err(),
                "{value} が受理されました"
            );
        }
    }
}
