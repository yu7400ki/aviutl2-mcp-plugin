//! 読み取りの失敗を表す型と、応答へ載せる安全な補助情報。

use crate::read::host::EditState;
use aviutl2_mcp_core::ErrorCode;
use serde_json::{Value, json};

/// 編集ハンドルが読み取りを受け付けられない場合に案内する再試行間隔（ミリ秒）。
///
/// 準備完了はプロジェクトの初回ロードで成立するため、待ち時間は短く採る。
const NOT_READY_RETRY_AFTER_MS: u64 = 500;

/// 再生・出力中に案内する再試行間隔（ミリ秒）。
///
/// 再生や出力は利用者の操作が終わるまで続くため、準備待ちより長く採る。
const EDIT_BLOCKED_RETRY_AFTER_MS: u64 = 2_000;

/// 読み取りの失敗。
///
/// 補助情報には SDK のハンドル・生ポインタ・秘匿値を含めない。含めるのは
/// 呼び出し側が次の行動を決められる値だけである。
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// 編集ハンドルがまだ読み取り API を受け付けられない。
    #[error("編集ハンドルの準備が完了していません")]
    NotReady,
    /// 再生中・出力中でプロジェクトデータを参照できない。
    #[error("{state}のため読み取りできません")]
    EditBlocked {
        /// 読み取りを妨げている編集状態。
        state: EditState,
    },
    /// 現在シーンが要求の前提と異なる。
    #[error("現在のシーンが要求の前提と一致しません")]
    SceneMismatch {
        /// 要求が前提としたシーン ID。
        expected: i32,
        /// 実際の現在シーン ID。
        current: i32,
    },
    /// プロジェクトの epoch が要求の前提と異なる。
    #[error("プロジェクトの epoch が要求の前提と一致しません")]
    EpochMismatch,
    /// fingerprint の算出方式が要求と異なる。
    #[error("fingerprint の算出方式が要求と一致しません")]
    FingerprintAlgorithmMismatch {
        /// 要求が指定した算出方式。
        requested: String,
        /// 現在生成できる算出方式。
        supported: String,
    },
    /// 候補の fingerprint が要求と一致しない。
    #[error("対象の fingerprint が要求と一致しません")]
    FingerprintMismatch,
    /// セレクターに一致する対象が存在しない。
    #[error("セレクターに一致するオブジェクトがありません")]
    ObjectNotFound {
        /// 不在を検出した SDK 関数の名前。
        ///
        /// 応答の補助情報には載せない。対象を 1 つも指定しない列挙では不在を
        /// そのまま返せず、列挙の失敗へ畳む必要がある。畳んだ後も、実際に
        /// 不在を検出した呼び出しを指せるようにここで引き継ぐ。
        detected_by: &'static str,
    },
    /// セレクターに一致する対象が複数ある。
    #[error("セレクターに一致するオブジェクトが複数あります")]
    AmbiguousObject {
        /// 一致した候補の件数。
        candidate_count: usize,
    },
    /// SDK の呼び出しが失敗した。
    #[error("SDK の呼び出しに失敗しました: {operation}")]
    Sdk {
        /// 失敗した SDK 関数の名前。
        operation: &'static str,
    },
    /// 参照区間の処理で panic を捕捉した。
    #[error("読み取り処理で panic を捕捉しました")]
    Panicked,
}

impl ReadError {
    /// 応答へ載せるエラーコードを返す。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            ReadError::NotReady => ErrorCode::HostBusy,
            ReadError::EditBlocked { .. } => ErrorCode::EditBlocked,
            ReadError::SceneMismatch { .. }
            | ReadError::EpochMismatch
            | ReadError::FingerprintAlgorithmMismatch { .. }
            | ReadError::FingerprintMismatch => ErrorCode::PreconditionFailed,
            ReadError::ObjectNotFound { .. } => ErrorCode::NotFound,
            ReadError::AmbiguousObject { .. } => ErrorCode::AmbiguousSelector,
            ReadError::Sdk { .. } => ErrorCode::SdkError,
            ReadError::Panicked => ErrorCode::InternalError,
        }
    }

    /// 再試行してよいかどうか。
    pub fn retryable(&self) -> bool {
        self.error_code().default_retryable()
    }

    /// 再試行までに空けるべき時間（ミリ秒）。案内できない場合は `None`。
    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            ReadError::NotReady => Some(NOT_READY_RETRY_AFTER_MS),
            ReadError::EditBlocked { .. } => Some(EDIT_BLOCKED_RETRY_AFTER_MS),
            _ => None,
        }
    }

    /// 応答へ載せる補助情報を組み立てる。
    ///
    /// 含めるのは要求の前提と実際の食い違い、再試行の案内、失敗した SDK 関数名の
    /// いずれかであり、ハンドル・生ポインタ・秘匿値は含めない。
    pub fn details(&self) -> Value {
        match self {
            ReadError::NotReady => json!({ "retry_after_ms": NOT_READY_RETRY_AFTER_MS }),
            ReadError::EditBlocked { state } => json!({
                "edit_state": state.as_str(),
                "retry_after_ms": EDIT_BLOCKED_RETRY_AFTER_MS,
            }),
            ReadError::SceneMismatch { expected, current } => json!({
                "expected_scene_id": expected,
                "current_scene_id": current,
            }),
            ReadError::EpochMismatch => json!({}),
            ReadError::FingerprintAlgorithmMismatch {
                requested,
                supported,
            } => json!({
                "requested_fingerprint_algorithm": requested,
                "supported_fingerprint_algorithm": supported,
            }),
            ReadError::FingerprintMismatch => json!({}),
            ReadError::ObjectNotFound { .. } => json!({}),
            ReadError::AmbiguousObject { candidate_count } => {
                json!({ "candidate_count": candidate_count })
            }
            ReadError::Sdk { operation } => json!({ "sdk_operation": operation }),
            ReadError::Panicked => json!({}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全 variant の代表値。新しい variant を足したらここへも足す。
    fn all_errors() -> Vec<ReadError> {
        vec![
            ReadError::NotReady,
            ReadError::EditBlocked {
                state: EditState::Preview,
            },
            ReadError::EditBlocked {
                state: EditState::Save,
            },
            ReadError::SceneMismatch {
                expected: 0,
                current: 3,
            },
            ReadError::EpochMismatch,
            ReadError::FingerprintAlgorithmMismatch {
                requested: "sha256-future-v9".to_string(),
                supported: "sha256-raw-v1".to_string(),
            },
            ReadError::FingerprintMismatch,
            ReadError::ObjectNotFound {
                detected_by: "find_object",
            },
            ReadError::AmbiguousObject { candidate_count: 2 },
            ReadError::Sdk {
                operation: "get_object_alias",
            },
            ReadError::Panicked,
        ]
    }

    #[test]
    fn error_codes_match_read_mapping() {
        let mapped: Vec<ErrorCode> = all_errors().iter().map(ReadError::error_code).collect();
        assert_eq!(
            mapped,
            vec![
                ErrorCode::HostBusy,
                ErrorCode::EditBlocked,
                ErrorCode::EditBlocked,
                ErrorCode::PreconditionFailed,
                ErrorCode::PreconditionFailed,
                ErrorCode::PreconditionFailed,
                ErrorCode::PreconditionFailed,
                ErrorCode::NotFound,
                ErrorCode::AmbiguousSelector,
                ErrorCode::SdkError,
                ErrorCode::InternalError,
            ]
        );
    }

    #[test]
    fn retryable_follows_error_code() {
        for error in all_errors() {
            assert_eq!(error.retryable(), error.error_code().default_retryable());
        }
        assert!(ReadError::NotReady.retryable());
        assert!(!ReadError::Panicked.retryable());
    }

    #[test]
    fn retry_after_is_advised_only_for_transient_states() {
        assert!(ReadError::NotReady.retry_after_ms().is_some());
        assert!(
            ReadError::EditBlocked {
                state: EditState::Preview
            }
            .retry_after_ms()
            .is_some()
        );
        assert_eq!(
            ReadError::ObjectNotFound {
                detected_by: "find_object"
            }
            .retry_after_ms(),
            None
        );
        assert_eq!(
            ReadError::Sdk {
                operation: "find_object"
            }
            .retry_after_ms(),
            None
        );
    }

    #[test]
    fn details_only_use_allowed_keys() {
        // 補助情報のキーはここで列挙したものに限る。新しいキーを足す際は
        // ハンドル・生ポインタ・秘匿値でないことを確かめた上で追加する。
        const ALLOWED: &[&str] = &[
            "retry_after_ms",
            "edit_state",
            "expected_scene_id",
            "current_scene_id",
            "requested_fingerprint_algorithm",
            "supported_fingerprint_algorithm",
            "candidate_count",
            "sdk_operation",
        ];
        for error in all_errors() {
            let details = error.details();
            let object = details
                .as_object()
                .unwrap_or_else(|| panic!("{error} の補助情報がオブジェクトではありません"));
            for key in object.keys() {
                assert!(
                    ALLOWED.contains(&key.as_str()),
                    "{error} の補助情報に未許可のキー {key} が含まれています"
                );
            }
        }
    }

    #[test]
    fn details_do_not_expose_pointers() {
        for error in all_errors() {
            let text = serde_json::to_string(&error.details()).unwrap();
            assert!(!text.contains("0x"), "{error} の補助情報: {text}");
            assert!(!text.to_lowercase().contains("handle"), "{error}: {text}");
            assert!(!text.to_lowercase().contains("pointer"), "{error}: {text}");
        }
    }

    #[test]
    fn messages_do_not_expose_pointers() {
        for error in all_errors() {
            let text = error.to_string();
            assert!(!text.contains("0x"), "{text}");
        }
    }
}
