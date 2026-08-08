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
    /// 直列化では常に出力する。**逆直列化でのみ省略を許し、省略時は `null` と
    /// して扱う。** 省略形を作る経路は無いが、それでも許すのは、これが失敗を
    /// 伝える経路だからである。補助情報 1 つの欠落で応答全体を読めずに落とすと、
    /// 要求元は何が起きたかを知る手段ごと失う——失敗を伝える経路の失敗は、
    /// 他のどの失敗よりも診断が難しい。
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    ///
    /// **現在このコードを生成する経路は無い。** 生成できるのは、1 インスタンスが
    /// 複数の要求を同時に受け付ける形になり、実行開始前の取り消しが起き得るように
    /// なったときである。それまで `cancelled` が応答に現れることはなく、
    /// `cancelled_is_never_produced` に相当する検査がそれを固定している。
    ///
    /// **値域は最初から確定させる。** [`ErrorCode`] は MCP 境界へ露出するため、
    /// 後から足すと要求元が扱う値域が増える。
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
    /// 対象 tool がプラグイン設定で無効化されている。
    ToolDisabled,
    /// SDK エラー。
    SdkError,
}

impl ErrorCode {
    pub fn as_snake_case(&self) -> &'static str {
        match self {
            ErrorCode::InstanceNotFound => "instance_not_found",
            ErrorCode::InstanceStale => "instance_stale",
            ErrorCode::ProtocolMismatch => "protocol_mismatch",
            ErrorCode::AuthenticationFailed => "authentication_failed",
            ErrorCode::HostBusy => "host_busy",
            ErrorCode::Timeout => "timeout",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::InvalidArgument => "invalid_argument",
            ErrorCode::InternalError => "internal_error",
            ErrorCode::NotFound => "not_found",
            ErrorCode::AmbiguousSelector => "ambiguous_selector",
            ErrorCode::PreconditionFailed => "precondition_failed",
            ErrorCode::EditBlocked => "edit_blocked",
            ErrorCode::UnsupportedOperation => "unsupported_operation",
            ErrorCode::ToolDisabled => "tool_disabled",
            ErrorCode::SdkError => "sdk_error",
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
            // 再送しても設定が変わるまで同じ結果になる。有効化は利用者が
            // AviUtl2 のプラグイン設定で行う操作である。
            | ErrorCode::ToolDisabled
            | ErrorCode::SdkError => false,
        }
    }
}

impl Serialize for ErrorCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_snake_case())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    /// 一覧に無いコードは [`ErrorCode::InternalError`] として読む。
    ///
    /// このコードを送るのは我々自身であり、一覧に無い名前が届くのは互換の
    /// 証拠ではなく我々のバグである。raw を保持して素通しすると要求元は
    /// 分岐を書けない名前を受け取るため、バグが名乗るべきコードへ寄せる。
    /// 復号そのものは失敗させない——失敗を伝える応答を読めずに落とすと、
    /// 何が起きたかを伝える手段ごと失われる。
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
            "tool_disabled" => ErrorCode::ToolDisabled,
            "sdk_error" => ErrorCode::SdkError,
            _ => ErrorCode::InternalError,
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
    "alias_directory_unavailable",
    "alias_not_parsable",
    "alias_without_effect",
    "alternate_data_stream",
    "argument_not_representable",
    "buffer_length_mismatch",
    "change_not_applied",
    "contains_control",
    "contains_nul",
    "destination_occupied",
    "device_namespace",
    "dimension_out_of_range",
    "duplicate_effect_name",
    "duplicate_target",
    "edit_info_out_of_range",
    "effect_count_out_of_range",
    "effect_not_creatable",
    "effect_not_registered",
    "effect_state_immutable",
    "empty",
    "empty_buffer",
    "empty_path",
    "forbidden_character",
    "frame_mismatch",
    "frame_out_of_range",
    "frame_outside_object",
    "frame_too_large",
    "grid_bpm_out_of_range",
    "inverse_unavailable",
    "item_not_evaluatable",
    "item_not_found",
    "item_type_not_writable",
    "item_value_not_applied",
    "layer_locked",
    "lone_carriage_return",
    "media_not_supported",
    "not_absolute",
    "path_too_long",
    "pitch_too_small",
    "section_boundary_exists",
    "section_change_rejected",
    "section_index_out_of_range",
    "section_move_crosses_boundary",
    "target_missing",
    "too_long",
    "track_flags_not_representable",
    "track_mode_not_writable",
    "track_mode_reads_as_number",
    "track_mode_unknown",
    "track_movement_present",
    "track_movement_without_mode",
    "track_value_count",
    "track_value_unavailable",
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
            ErrorCode::ToolDisabled,
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
    fn a_code_outside_the_set_is_read_as_an_internal_error() {
        // 一覧に無いコードを送るのは我々自身の誤りであり、要求元が分岐を
        // 書けない名前を渡すよりバグとして名乗る方がよい。
        let s = "\"future_code\"";
        let code: ErrorCode = serde_json::from_str(s).unwrap();
        assert_eq!(code, ErrorCode::InternalError);
        let s2 = serde_json::to_string(&code).unwrap();
        assert_eq!(s2, "\"internal_error\"");
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
            ErrorCode::ToolDisabled,
            ErrorCode::SdkError,
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
