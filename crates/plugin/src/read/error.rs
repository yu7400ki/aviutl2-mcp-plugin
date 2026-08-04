//! 読み取りの失敗を表す型と、応答へ載せる安全な補助情報。

use crate::read::host::EditState;
use aviutl2_mcp_core::{ErrorCode, ObjectSummary};
use serde_json::{Value, json};

/// 編集ハンドルが読み取りを受け付けられない場合に案内する再試行間隔（ミリ秒）。
///
/// 準備完了はプロジェクトの初回ロードで成立するため、待ち時間は短く採る。
const NOT_READY_RETRY_AFTER_MS: u64 = 500;

/// 再生・出力中に案内する再試行間隔（ミリ秒）。
///
/// 再生や出力は利用者の操作が終わるまで続くため、準備待ちより長く採る。
const EDIT_BLOCKED_RETRY_AFTER_MS: u64 = 2_000;

/// 編集情報を取得する SDK 関数の名前。
///
/// 呼び出しの失敗と値の範囲外は同じ関数から来る。同じ名前を 2 か所に書くと、
/// 片方だけを変えたときに両者が別の関数から来たように見える。
pub(crate) const EDIT_INFO_OPERATION: &str = "get_edit_info";

/// 編集情報の値が受け渡せる範囲を超えていたことを表す名前。
pub(crate) const REASON_EDIT_INFO_OUT_OF_RANGE: &str = "edit_info_out_of_range";

/// 要求が名指しした対象そのものが存在しないことを表す名前。
pub(crate) const REASON_TARGET_MISSING: &str = "target_missing";

/// 対象の中に、要求された設定項目が無いことを表す名前。
///
/// 対象そのものの不在（[`REASON_TARGET_MISSING`]）と分ける。**要求元が次に取る
/// 行動が違う**——対象が無ければセレクターを取り直し、項目が無ければ項目名を
/// 直す。
pub(crate) const REASON_ITEM_NOT_FOUND: &str = "item_not_found";

/// 設定項目が任意フレームでの評価に対応しないことを表す名前。
pub(crate) const REASON_ITEM_NOT_EVALUATABLE: &str = "item_not_evaluatable";

/// 要求されたフレームが対象の範囲外であることを表す名前。
pub(crate) const REASON_FRAME_OUT_OF_RANGE: &str = "frame_out_of_range";

/// 事前確認を通した要求に対して値が得られなかったことを表す名前。
pub(crate) const REASON_TRACK_VALUE_UNAVAILABLE: &str = "track_value_unavailable";

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
    /// 候補の fingerprint が要求と一致しない。
    #[error("対象の fingerprint が要求と一致しません")]
    FingerprintMismatch {
        /// 解決済み対象を読み直した現在の概要。
        ///
        /// 食い違いを判定する時点で対象は既に読み直されており、載せるのに
        /// 追加の SDK 呼び出しは要らない。概要はセレクターと fingerprint を
        /// 内包するため、要求元はこれだけで次の要求を組み立てられる。
        ///
        /// 概要は alias も設定値もパスも持たない。載せても秘匿の方針は変わら
        /// ない。
        current_object: Box<ObjectSummary>,
    },
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
    /// セレクターが指す effect が存在しない。
    ///
    /// 所属オブジェクトの照合は既に通っている。要求元は effect の一覧を読み
    /// 直す必要がある。
    #[error("セレクターに一致する effect がありません")]
    EffectNotFound,
    /// 解決した effect の fingerprint がセレクターの値と一致しない。
    ///
    /// 現在の姿を名乗らない。オブジェクトの概要は要求元が送ってきた値と同じで
    /// あり、復帰の手掛かりを増やさない。
    #[error("対象 effect の fingerprint が要求と一致しません")]
    EffectFingerprintMismatch,
    /// 要求された設定項目が effect の項目一覧に無い。
    ///
    /// effect そのものの不在と分ける。要求元が次に取る行動が違う——前者は
    /// セレクターを取り直し、こちらは項目名を直す。
    #[error("指定された設定項目が effect にありません")]
    ItemNotFound,
    /// 設定項目は在るが、任意フレームでの値を持つ種別ではない。
    ///
    /// 項目名が誤っている場合と区別する。名前は正しいのに種別が違う要求に対して
    /// 要求元が取る行動は「別の項目を選ぶ」であり、「名前を直す」ではない。
    #[error("指定された設定項目は任意フレームでの値を持ちません")]
    ItemNotEvaluatable,
    /// 要求されたフレームが対象オブジェクトの範囲外である。
    #[error("要求されたフレームがオブジェクトの範囲外です")]
    FrameOutOfRange,
    /// 事前確認を通した要求に対して SDK が値を返さなかった。
    ///
    /// 項目の存在と種別、フレームの範囲はいずれも呼び出し前に確かめている。
    /// それらを通った失敗に対して要求元が打てる手は無い。
    #[error("補間後の値を取得できませんでした: {operation}")]
    TrackValueUnavailable {
        /// 値を返さなかった SDK 関数の名前。
        operation: &'static str,
    },
    /// SDK の呼び出しが失敗した。
    #[error("SDK の呼び出しに失敗しました: {operation}")]
    Sdk {
        /// 失敗した SDK 関数の名前。
        operation: &'static str,
    },
    /// 編集情報は取得できたが、返ってきた値が受け渡せる範囲に収まらない。
    ///
    /// 呼び出しは成功しており、失敗したのは値の検証である。[`ReadError::Sdk`]
    /// と同じコードで返るのは、どちらもホスト側の異常であって要求元に打つ手が
    /// 無いためである。区別は補助情報の `reason` が担う。
    #[error("編集情報の値が受け渡せる範囲を超えています")]
    EditInfoOutOfRange,
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
            | ReadError::FingerprintMismatch { .. }
            | ReadError::EffectFingerprintMismatch
            | ReadError::FrameOutOfRange => ErrorCode::PreconditionFailed,
            ReadError::ObjectNotFound { .. }
            | ReadError::EffectNotFound
            | ReadError::ItemNotFound => ErrorCode::NotFound,
            ReadError::ItemNotEvaluatable => ErrorCode::UnsupportedOperation,
            ReadError::AmbiguousObject { .. } => ErrorCode::AmbiguousSelector,
            ReadError::Sdk { .. }
            | ReadError::EditInfoOutOfRange
            | ReadError::TrackValueUnavailable { .. } => ErrorCode::SdkError,
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
    /// 含めるのは要求の前提と実際の食い違い、再試行の案内、失敗した SDK 関数名、
    /// 読み直した対象の概要のいずれかであり、ハンドル・生ポインタ・秘匿値は
    /// 含めない。
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
            ReadError::FingerprintMismatch { current_object } => {
                json!({ "current_object": current_object })
            }
            ReadError::ObjectNotFound { .. } => json!({}),
            ReadError::EffectFingerprintMismatch => json!({}),
            ReadError::EffectNotFound => json!({ "reason": REASON_TARGET_MISSING }),
            ReadError::ItemNotFound => json!({ "reason": REASON_ITEM_NOT_FOUND }),
            ReadError::ItemNotEvaluatable => json!({ "reason": REASON_ITEM_NOT_EVALUATABLE }),
            ReadError::FrameOutOfRange => json!({ "reason": REASON_FRAME_OUT_OF_RANGE }),
            ReadError::AmbiguousObject { candidate_count } => {
                json!({ "candidate_count": candidate_count })
            }
            ReadError::Sdk { operation } => json!({ "sdk_operation": operation }),
            ReadError::TrackValueUnavailable { operation } => json!({
                "sdk_operation": operation,
                "reason": REASON_TRACK_VALUE_UNAVAILABLE,
            }),
            ReadError::EditInfoOutOfRange => json!({
                "sdk_operation": EDIT_INFO_OPERATION,
                "reason": REASON_EDIT_INFO_OUT_OF_RANGE,
            }),
            ReadError::Panicked => json!({}),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::sample_object_summary;

    /// 全 variant の代表値。新しい variant を足したらここへも足す。
    pub(crate) fn all_errors() -> Vec<ReadError> {
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
            ReadError::FingerprintMismatch {
                current_object: Box::new(sample_object_summary()),
            },
            ReadError::ObjectNotFound {
                detected_by: "find_object",
            },
            ReadError::AmbiguousObject { candidate_count: 2 },
            ReadError::EffectNotFound,
            ReadError::EffectFingerprintMismatch,
            ReadError::ItemNotFound,
            ReadError::ItemNotEvaluatable,
            ReadError::FrameOutOfRange,
            ReadError::Sdk {
                operation: "get_object_alias",
            },
            ReadError::EditInfoOutOfRange,
            ReadError::TrackValueUnavailable {
                operation: "get_effect_track_value",
            },
            ReadError::Panicked,
        ]
    }

    /// variant を表す名前を返す。
    ///
    /// 網羅 match で書く。variant を足すとここがコンパイルエラーになり、
    /// すぐ下の一覧と [`all_errors`] へ足す必要があることが分かる。
    fn variant_name(error: &ReadError) -> &'static str {
        match error {
            ReadError::NotReady => "NotReady",
            ReadError::EditBlocked { .. } => "EditBlocked",
            ReadError::SceneMismatch { .. } => "SceneMismatch",
            ReadError::EpochMismatch => "EpochMismatch",
            ReadError::FingerprintMismatch { .. } => "FingerprintMismatch",
            ReadError::ObjectNotFound { .. } => "ObjectNotFound",
            ReadError::AmbiguousObject { .. } => "AmbiguousObject",
            ReadError::EffectNotFound => "EffectNotFound",
            ReadError::EffectFingerprintMismatch => "EffectFingerprintMismatch",
            ReadError::ItemNotFound => "ItemNotFound",
            ReadError::ItemNotEvaluatable => "ItemNotEvaluatable",
            ReadError::FrameOutOfRange => "FrameOutOfRange",
            ReadError::Sdk { .. } => "Sdk",
            ReadError::EditInfoOutOfRange => "EditInfoOutOfRange",
            ReadError::TrackValueUnavailable { .. } => "TrackValueUnavailable",
            ReadError::Panicked => "Panicked",
        }
    }

    #[test]
    fn all_errors_covers_every_variant() {
        const VARIANTS: &[&str] = &[
            "NotReady",
            "EditBlocked",
            "SceneMismatch",
            "EpochMismatch",
            "FingerprintMismatch",
            "ObjectNotFound",
            "AmbiguousObject",
            "EffectNotFound",
            "EffectFingerprintMismatch",
            "ItemNotFound",
            "ItemNotEvaluatable",
            "FrameOutOfRange",
            "Sdk",
            "EditInfoOutOfRange",
            "TrackValueUnavailable",
            "Panicked",
        ];
        let covered: Vec<&str> = all_errors().iter().map(variant_name).collect();
        for variant in VARIANTS {
            assert!(
                covered.contains(variant),
                "{variant} の代表値が一覧にありません"
            );
        }
        for variant in &covered {
            assert!(
                VARIANTS.contains(variant),
                "{variant} が網羅すべき variant の一覧にありません"
            );
        }
    }

    #[test]
    fn a_failed_call_and_an_out_of_range_value_are_told_apart() {
        // どちらも同じ関数から来る同じコードの失敗である。区別が付かないと、
        // 要求元も運用者も「ホストが壊れているのか、呼び出しに失敗したのか」
        // を切り分けられない。
        let called = ReadError::Sdk {
            operation: EDIT_INFO_OPERATION,
        };
        let out_of_range = ReadError::EditInfoOutOfRange;

        assert_eq!(called.error_code(), out_of_range.error_code());
        assert_eq!(
            called.details()["sdk_operation"],
            out_of_range.details()["sdk_operation"]
        );
        assert!(
            called.details().get("reason").is_none(),
            "呼び出しの失敗に名前が付きました"
        );
        assert_eq!(
            out_of_range.details()["reason"],
            json!(REASON_EDIT_INFO_OUT_OF_RANGE)
        );
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
                ErrorCode::NotFound,
                ErrorCode::AmbiguousSelector,
                ErrorCode::NotFound,
                ErrorCode::PreconditionFailed,
                ErrorCode::NotFound,
                // 項目は在るが種別が違う。名前が誤っている場合と畳まない。
                ErrorCode::UnsupportedOperation,
                ErrorCode::PreconditionFailed,
                ErrorCode::SdkError,
                // 取得は成功したが値が範囲外だった。要求元に打つ手が無い点は
                // 呼び出しの失敗と同じであり、コードは分けない。
                ErrorCode::SdkError,
                ErrorCode::SdkError,
                ErrorCode::InternalError,
            ]
        );
    }

    #[test]
    fn the_failures_of_evaluating_an_item_are_not_folded_together() {
        // effect が無い・項目名が誤っている・種別が違う・フレームが範囲外・値が
        // 返らないは、要求元が次に取る行動がそれぞれ違う。1 つでも同じ応答に
        // なると切り分けられない。
        let mapped: Vec<(ErrorCode, Value)> = [
            ReadError::EffectNotFound,
            ReadError::ItemNotFound,
            ReadError::ItemNotEvaluatable,
            ReadError::FrameOutOfRange,
            ReadError::TrackValueUnavailable {
                operation: "get_effect_track_value",
            },
        ]
        .into_iter()
        .map(|error| (error.error_code(), error.details()["reason"].clone()))
        .collect();

        assert_eq!(
            mapped,
            vec![
                (ErrorCode::NotFound, json!(REASON_TARGET_MISSING)),
                (ErrorCode::NotFound, json!(REASON_ITEM_NOT_FOUND)),
                (
                    ErrorCode::UnsupportedOperation,
                    json!(REASON_ITEM_NOT_EVALUATABLE)
                ),
                (
                    ErrorCode::PreconditionFailed,
                    json!(REASON_FRAME_OUT_OF_RANGE)
                ),
                (ErrorCode::SdkError, json!(REASON_TRACK_VALUE_UNAVAILABLE)),
            ]
        );
        let distinct: std::collections::BTreeSet<String> =
            mapped.iter().map(|pair| format!("{pair:?}")).collect();
        assert_eq!(
            distinct.len(),
            mapped.len(),
            "同じ応答になった失敗があります"
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
        // 補助情報のキーはここで列挙したものに限る。入れ子の内側まで見る。
        // トップレベルだけを見ると、値をオブジェクトで包んだ瞬間に検査が
        // 素通りする。新しいキーを足す際はハンドル・生ポインタ・秘匿値で
        // ないことを確かめた上で追加する。
        const ALLOWED: &[&str] = &[
            "retry_after_ms",
            "edit_state",
            "expected_scene_id",
            "current_scene_id",
            "candidate_count",
            "sdk_operation",
            "reason",
            // 読み直した対象の概要と、それが内包するセレクター。
            "current_object",
            "layer",
            "frame_start",
            "frame_end",
            "name",
            "selector",
            "fingerprint",
            "project_epoch",
            "scene_id",
            "frame",
        ];
        for error in all_errors() {
            let details = error.details();
            assert!(
                details.is_object(),
                "{error} の補助情報がオブジェクトではありません"
            );
            for key in nested_keys(&details) {
                assert!(
                    ALLOWED.contains(&key.as_str()),
                    "{error} の補助情報に未許可のキー {key} が含まれています"
                );
            }
        }
    }

    /// 入れ子を含む全てのキーを集める。
    fn nested_keys(value: &Value) -> Vec<String> {
        let mut found = Vec::new();
        collect_keys(value, &mut found);
        found
    }

    fn collect_keys(value: &Value, into: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    into.push(key.clone());
                    collect_keys(value, into);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect_keys(item, into)),
            _ => {}
        }
    }

    #[test]
    fn only_a_content_mismatch_carries_the_current_object() {
        // 現在の姿を載せられるのは、対象を読み直したうえで内容が食い違った
        // 場合だけである。他の失敗は対象を読み直していない。
        for error in all_errors() {
            let carried = error.details().get("current_object").is_some();
            let expected = matches!(error, ReadError::FingerprintMismatch { .. });
            assert_eq!(carried, expected, "{error}");
        }
    }

    #[test]
    fn the_current_object_carries_a_selector_that_can_be_sent_back() {
        // 概要はセレクターと fingerprint を内包する。要求元はこれだけで次の
        // 要求を組み立てられる。
        let summary = sample_object_summary();
        let error = ReadError::FingerprintMismatch {
            current_object: Box::new(summary.clone()),
        };
        let details = error.details();
        let current = &details["current_object"];
        assert_eq!(
            current["selector"],
            serde_json::to_value(&summary.selector).unwrap()
        );
        assert_eq!(
            current["fingerprint"],
            serde_json::to_value(&summary.fingerprint).unwrap()
        );
    }

    #[test]
    fn the_current_object_does_not_carry_an_alias_or_settings() {
        // 概要は要約であり alias も設定値もパスも持たない。秘匿の方針は
        // 補助情報へ載せても変わらない。
        let details = ReadError::FingerprintMismatch {
            current_object: Box::new(sample_object_summary()),
        }
        .details();
        for forbidden in ["alias", "path", "value", "item"] {
            assert!(
                !nested_keys(&details).iter().any(|key| key == forbidden),
                "補助情報に {forbidden} が現れました: {details}"
            );
        }
        assert!(!details.to_string().contains(r"C:\"), "{details}");
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
