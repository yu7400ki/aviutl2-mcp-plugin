//! ライフサイクル状態。

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;

/// インスタンスのライフサイクル状態。
///
/// 値を作るのは plugin の状態機械だけであり、ここに並ぶ 5 つ以外は名乗らない。
/// 一覧に無い値を運ぶ descriptor や応答は、状態が読めないものとして拒否する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
}

impl InstanceState {
    pub fn as_snake_case(&self) -> &'static str {
        match self {
            InstanceState::Starting => "starting",
            InstanceState::Ready => "ready",
            InstanceState::Busy => "busy",
            InstanceState::Draining => "draining",
            InstanceState::Gone => "gone",
        }
    }
}

impl Serialize for InstanceState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_snake_case())
    }
}

impl<'de> Deserialize<'de> for InstanceState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "starting" => Ok(InstanceState::Starting),
            "ready" => Ok(InstanceState::Ready),
            "busy" => Ok(InstanceState::Busy),
            "draining" => Ok(InstanceState::Draining),
            "gone" => Ok(InstanceState::Gone),
            _ => Err(de::Error::custom(format!(
                "state が既知の値ではありません: 実際は {:?}",
                s
            ))),
        }
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
    fn instance_state_outside_the_set_is_rejected() {
        // 状態を書くのは plugin の状態機械だけである。読めない状態を推測で
        // 埋めると、descriptor も pong も「どの状態か分からない」ことを
        // 伝えられなくなる。
        let s = "\"future_state\"";
        let result: Result<InstanceState, _> = serde_json::from_str(s);
        assert!(result.is_err());
    }
}
