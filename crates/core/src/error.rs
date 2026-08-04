//! エラーモデル。

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// エラー情報。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorObject {
    /// エラーコード。
    pub code: ErrorCode,
    /// 安全で具体的な説明。秘匿値を含めない。
    pub message: String,
    /// リトライ可能かどうか。
    pub retryable: bool,
    /// 安全な補助情報のみを含む追加情報。
    ///
    /// 直列化では常に出力する。逆直列化では前方互換のため省略を許容し、
    /// 省略時は `null` として扱う。
    #[serde(default)]
    pub details: serde_json::Value,
    /// 相関 ID。server が UUID v7 で付与する。
    pub correlation_id: Option<String>,
}

impl ErrorObject {
    pub fn new(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            details: serde_json::Value::Object(serde_json::Map::new()),
            correlation_id: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }

    pub fn with_correlation_id(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }
}

/// エラーコード。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// 未知の instance_id。
    InstanceNotFound,
    /// descriptor はあるが pipe/identity 無効。
    InstanceStale,
    /// IPC MAJOR 不一致。
    ProtocolMismatch,
    /// ACL または handshake 検証失敗。
    AuthenticationFailed,
    /// 起動・終了処理中、キュー上限。
    HostBusy,
    /// deadline 超過。
    Timeout,
    /// 実行開始前に cancel。
    Cancelled,
    /// schema/範囲/型が不正。
    InvalidArgument,
    /// 想定外の内部失敗、panic 捕捉。
    InternalError,
    /// リソースが見つからない。
    NotFound,
    /// セレクターが曖昧。
    AmbiguousSelector,
    /// 前提条件の不整合。
    PreconditionFailed,
    /// 編集がブロックされた。
    EditBlocked,
    /// 未対応の operation。
    UnsupportedOperation,
    /// SDK エラー。
    SdkError,
    /// 未知のコードを破棄せず raw 保持。
    Unknown(String),
}

impl ErrorCode {
    pub fn as_snake_case(&self) -> String {
        match self {
            ErrorCode::InstanceNotFound => "instance_not_found".to_string(),
            ErrorCode::InstanceStale => "instance_stale".to_string(),
            ErrorCode::ProtocolMismatch => "protocol_mismatch".to_string(),
            ErrorCode::AuthenticationFailed => "authentication_failed".to_string(),
            ErrorCode::HostBusy => "host_busy".to_string(),
            ErrorCode::Timeout => "timeout".to_string(),
            ErrorCode::Cancelled => "cancelled".to_string(),
            ErrorCode::InvalidArgument => "invalid_argument".to_string(),
            ErrorCode::InternalError => "internal_error".to_string(),
            ErrorCode::NotFound => "not_found".to_string(),
            ErrorCode::AmbiguousSelector => "ambiguous_selector".to_string(),
            ErrorCode::PreconditionFailed => "precondition_failed".to_string(),
            ErrorCode::EditBlocked => "edit_blocked".to_string(),
            ErrorCode::UnsupportedOperation => "unsupported_operation".to_string(),
            ErrorCode::SdkError => "sdk_error".to_string(),
            ErrorCode::Unknown(s) => s.clone(),
        }
    }

    /// コードから導かれる既定のリトライ可否を返す。
    ///
    /// 時間経過や状態遷移で解消し得るものだけを true とする。要求内容そのものが
    /// 不正なものは再送しても同じ結果になるため false とする。
    /// [`ErrorCode::SdkError`] は実際には状態依存だが、成功する保証が無いため
    /// 既定は false とし、リトライ可能と判断できた呼び出し側が明示的に上書きする。
    pub fn default_retryable(&self) -> bool {
        match self {
            ErrorCode::InstanceStale
            | ErrorCode::HostBusy
            | ErrorCode::Timeout
            | ErrorCode::Cancelled
            | ErrorCode::PreconditionFailed
            | ErrorCode::EditBlocked => true,
            ErrorCode::InstanceNotFound
            | ErrorCode::ProtocolMismatch
            | ErrorCode::AuthenticationFailed
            | ErrorCode::InvalidArgument
            | ErrorCode::InternalError
            | ErrorCode::NotFound
            | ErrorCode::AmbiguousSelector
            | ErrorCode::UnsupportedOperation
            | ErrorCode::SdkError
            | ErrorCode::Unknown(_) => false,
        }
    }
}

impl Serialize for ErrorCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.as_snake_case())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "instance_not_found" => ErrorCode::InstanceNotFound,
            "instance_stale" => ErrorCode::InstanceStale,
            "protocol_mismatch" => ErrorCode::ProtocolMismatch,
            "authentication_failed" => ErrorCode::AuthenticationFailed,
            "host_busy" => ErrorCode::HostBusy,
            "timeout" => ErrorCode::Timeout,
            "cancelled" => ErrorCode::Cancelled,
            "invalid_argument" => ErrorCode::InvalidArgument,
            "internal_error" => ErrorCode::InternalError,
            "not_found" => ErrorCode::NotFound,
            "ambiguous_selector" => ErrorCode::AmbiguousSelector,
            "precondition_failed" => ErrorCode::PreconditionFailed,
            "edit_blocked" => ErrorCode::EditBlocked,
            "unsupported_operation" => ErrorCode::UnsupportedOperation,
            "sdk_error" => ErrorCode::SdkError,
            _ => ErrorCode::Unknown(s),
        })
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_snake_case())
    }
}

/// `details.reason` に載せてよい機械可読名の全体。
///
/// 各エラー型の `reason()` が返す値は、すべてこの集合に属する。
/// 集合はワイヤ契約であり、値を足すことは契約の変更である。
///
/// 一覧は昇順に並べ、同じ値を 2 度置かない。名前が指すのは失敗の種別であり、
/// 同じ事実を表す失敗は種別が別でも同じ名前を名乗る。どのエラーコードで
/// 返るかは名前とは独立に決まる。
pub const REASON_VALUES: &[&str] = &[
    "alternate_data_stream",
    "argument_not_representable",
    "buffer_length_mismatch",
    "change_not_applied",
    "contains_control",
    "contains_nul",
    "destination_occupied",
    "device_namespace",
    "dimension_out_of_range",
    "duplicate_target",
    "effect_not_registered",
    "effect_state_immutable",
    "empty",
    "empty_buffer",
    "empty_path",
    "frame_mismatch",
    "frame_out_of_range",
    "frame_too_large",
    "inverse_unavailable",
    "item_type_not_writable",
    "layer_locked",
    "media_not_supported",
    "not_absolute",
    "path_too_long",
    "pitch_too_small",
    "target_missing",
    "too_long",
    "unc_path",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_roundtrip() {
        for code in [
            ErrorCode::InstanceNotFound,
            ErrorCode::InstanceStale,
            ErrorCode::ProtocolMismatch,
            ErrorCode::AuthenticationFailed,
            ErrorCode::HostBusy,
            ErrorCode::Timeout,
            ErrorCode::Cancelled,
            ErrorCode::InvalidArgument,
            ErrorCode::InternalError,
            ErrorCode::NotFound,
            ErrorCode::AmbiguousSelector,
            ErrorCode::PreconditionFailed,
            ErrorCode::EditBlocked,
            ErrorCode::UnsupportedOperation,
            ErrorCode::SdkError,
        ] {
            let s = serde_json::to_string(&code).unwrap();
            let code2: ErrorCode = serde_json::from_str(&s).unwrap();
            assert_eq!(code, code2);
        }
    }

    #[test]
    fn reason_values_are_sorted_and_unique() {
        // 昇順かつ一意であることを固定する。重複した名前は、片方を消しても
        // 誰も落ちないまま残り続ける。
        let mut sorted = REASON_VALUES.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, REASON_VALUES, "reason の一覧が昇順ではありません");
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            REASON_VALUES.len(),
            "reason の一覧に重複があります"
        );
    }

    #[test]
    fn reason_values_are_machine_readable_names() {
        // 応答で分岐に使う値であり、表示用の文言ではない。
        for reason in REASON_VALUES {
            assert!(!reason.is_empty());
            assert!(
                reason
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
                "{reason} は小文字の snake_case ではありません"
            );
        }
    }

    #[test]
    fn error_code_unknown_preserved() {
        let s = "\"future_code\"";
        let code: ErrorCode = serde_json::from_str(s).unwrap();
        assert_eq!(code, ErrorCode::Unknown("future_code".to_string()));
        let s2 = serde_json::to_string(&code).unwrap();
        assert_eq!(s2, "\"future_code\"");
    }

    #[test]
    fn default_retryable_matches_code() {
        for code in [
            ErrorCode::InstanceStale,
            ErrorCode::HostBusy,
            ErrorCode::Timeout,
            ErrorCode::Cancelled,
            ErrorCode::PreconditionFailed,
            ErrorCode::EditBlocked,
        ] {
            assert!(code.default_retryable(), "{code} はリトライ可能である");
        }

        for code in [
            ErrorCode::InstanceNotFound,
            ErrorCode::ProtocolMismatch,
            ErrorCode::AuthenticationFailed,
            ErrorCode::InvalidArgument,
            ErrorCode::InternalError,
            ErrorCode::NotFound,
            ErrorCode::AmbiguousSelector,
            ErrorCode::UnsupportedOperation,
            ErrorCode::SdkError,
            ErrorCode::Unknown("future_code".to_string()),
        ] {
            assert!(!code.default_retryable(), "{code} はリトライ不可である");
        }
    }

    #[test]
    fn error_object_keeps_explicit_retryable() {
        // 既定値の導出は ErrorObject::new の引数を置き換えない。
        let err = ErrorObject::new(ErrorCode::SdkError, "sdk failed", true);
        assert!(err.retryable);
        assert!(!err.code.default_retryable());
    }

    #[test]
    fn error_object_roundtrip() {
        let err = ErrorObject::new(ErrorCode::HostBusy, "host is busy", true)
            .with_correlation_id("0190abcd-1234-7def-1234-567890abcdef");
        let s = serde_json::to_string(&err).unwrap();
        let err2: ErrorObject = serde_json::from_str(&s).unwrap();
        assert_eq!(err, err2);
    }

    #[test]
    fn error_object_allows_omitted_details() {
        let s = r#"{"code":"host_busy","message":"busy","retryable":true}"#;
        let err: ErrorObject = serde_json::from_str(s).unwrap();
        assert_eq!(err.details, serde_json::Value::Null);
        assert_eq!(err.correlation_id, None);
    }

    #[test]
    fn error_object_always_serializes_details() {
        let err = ErrorObject::new(ErrorCode::HostBusy, "busy", true);
        let s = serde_json::to_string(&err).unwrap();
        assert!(s.contains("\"details\""));
    }

    #[test]
    fn error_object_allows_unknown_optional_fields() {
        let s =
            r#"{"code":"internal_error","message":"x","retryable":false,"details":{},"unknown":1}"#;
        let result: Result<ErrorObject, _> = serde_json::from_str(s);
        assert!(result.is_ok());
    }
}
