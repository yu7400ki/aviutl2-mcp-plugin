//! ライフサイクル状態。

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// インスタンスのライフサイクル状態。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InstanceState {
    /// plugin 登録 / IPC 初期化中。新規要求は host_busy。
    Starting,
    /// read/edit 受付可能。
    Ready,
    /// 再生・保存・キュー飽和など。種別に応じ受付/拒否。
    Busy,
    /// 終了処理中。新規要求は拒否。
    Draining,
    /// pipe 切断 / 生存確認失敗。instance_stale。
    Gone,
    /// 未知の状態値を raw 保持。
    Unknown(String),
}

impl InstanceState {
    pub fn as_snake_case(&self) -> String {
        match self {
            InstanceState::Starting => "starting".to_string(),
            InstanceState::Ready => "ready".to_string(),
            InstanceState::Busy => "busy".to_string(),
            InstanceState::Draining => "draining".to_string(),
            InstanceState::Gone => "gone".to_string(),
            InstanceState::Unknown(s) => s.clone(),
        }
    }
}

impl Serialize for InstanceState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.as_snake_case())
    }
}

impl<'de> Deserialize<'de> for InstanceState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "starting" => InstanceState::Starting,
            "ready" => InstanceState::Ready,
            "busy" => InstanceState::Busy,
            "draining" => InstanceState::Draining,
            "gone" => InstanceState::Gone,
            _ => InstanceState::Unknown(s),
        })
    }
}

impl fmt::Display for InstanceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_snake_case())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_state_roundtrip() {
        for state in [
            InstanceState::Starting,
            InstanceState::Ready,
            InstanceState::Busy,
            InstanceState::Draining,
            InstanceState::Gone,
        ] {
            let s = serde_json::to_string(&state).unwrap();
            let state2: InstanceState = serde_json::from_str(&s).unwrap();
            assert_eq!(state, state2);
        }
    }

    #[test]
    fn instance_state_unknown_preserved() {
        let s = "\"future_state\"";
        let state: InstanceState = serde_json::from_str(s).unwrap();
        assert_eq!(state, InstanceState::Unknown("future_state".to_string()));
        let s2 = serde_json::to_string(&state).unwrap();
        assert_eq!(s2, "\"future_state\"");
    }
}
