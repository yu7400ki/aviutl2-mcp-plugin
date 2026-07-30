//! レンダリングの失敗を表す型と、応答へ載せる安全な補助情報。

use crate::edit::error::RetryRequires;
use crate::read::ReadError;
use aviutl2_mcp_core::ErrorCode;
use serde_json::{Map, Value, json};

/// 放棄済みタスクが上限に達しているときに案内する再試行間隔（ミリ秒）。
///
/// 件数が減るのは、忘れたタスクのコールバックが遅れて届いたときだけである。
/// 届くとしても数秒では期待できないため、準備待ちより長く採る。
const TOO_MANY_ABANDONED_RETRY_AFTER_MS: u64 = 5_000;

/// 停止要求で待機を打ち切ったときに案内する再試行間隔（ミリ秒）。
///
/// 停止が始まったインスタンスはまもなく消える。再試行の前に、そのインスタンスが
/// まだ居るかどうかを確かめ直せるだけの間隔を採る。
const SHUTTING_DOWN_RETRY_AFTER_MS: u64 = 2_000;

/// pixel buffer の検証で破れた規則。
///
/// ホストが渡す寸法は符号の検査を経ておらず、負値が 2^31 以上の `u32` として
/// 届く。長さだけは算出時に破綻すると空スライスへ縮退するため、寸法と長さが
/// 整合しない組が渡り得る。どの規則が破れたかを持つのは、失敗の原因を
/// 要求元と運用者が切り分けられるようにするためである。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferRule {
    /// 規則 1: コールバックが返したフレームが要求したフレームと異なる。
    FrameMismatch,
    /// 規則 2: 幅・高さ・pitch のいずれかが `i32::MAX` を超える。
    DimensionOutOfRange,
    /// 規則 3: 幅または高さが 0。
    EmptyDimension,
    /// 規則 4: 幅から求めた 1 行のバイト数が `u32` に収まらない。
    RowBytesOverflow,
    /// 規則 5: pitch が 1 行のバイト数に満たない。
    PitchTooSmall,
    /// 規則 6: buffer が空スライスへ縮退している。
    EmptyBuffer,
    /// 規則 6: `pitch * height` と buffer の長さが一致しない。
    BufferLengthMismatch,
    /// 規則 7: 詰め物を除いた大きさが上限を超える。
    FrameTooLarge,
}

impl BufferRule {
    /// 応答へ載せる機械可読な名前。
    ///
    /// 寸法にまつわる 3 つの規則は 1 つの名前へまとめる。要求元が取れる行動は
    /// どれでも同じであり、名前を分けても訂正の役に立たない。
    pub fn as_str(self) -> &'static str {
        match self {
            BufferRule::FrameMismatch => "frame_mismatch",
            BufferRule::DimensionOutOfRange
            | BufferRule::EmptyDimension
            | BufferRule::RowBytesOverflow => "dimension_out_of_range",
            BufferRule::PitchTooSmall => "pitch_too_small",
            BufferRule::EmptyBuffer => "empty_buffer",
            BufferRule::BufferLengthMismatch => "buffer_length_mismatch",
            BufferRule::FrameTooLarge => "frame_too_large",
        }
    }
}

impl std::fmt::Display for BufferRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            BufferRule::FrameMismatch => "レンダリング結果のフレームが要求と一致しません",
            BufferRule::DimensionOutOfRange => "レンダリング結果の寸法が範囲外です",
            BufferRule::EmptyDimension => "レンダリング結果の寸法が 0 です",
            BufferRule::RowBytesOverflow => "レンダリング結果の 1 行のバイト数が範囲外です",
            BufferRule::PitchTooSmall => "レンダリング結果の pitch が 1 行のバイト数に足りません",
            BufferRule::EmptyBuffer => "レンダリング結果の画像データが空です",
            BufferRule::BufferLengthMismatch => {
                "レンダリング結果の寸法と画像データの長さが一致しません"
            }
            BufferRule::FrameTooLarge => "レンダリング結果が上限を超えています",
        };
        f.write_str(text)
    }
}

/// 成果物を作る段のうち、どこで失敗したか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactStage {
    /// PNG への符号化。
    Encode,
    /// 引き渡し用ファイルへの書き込み。
    Write,
}

impl ArtifactStage {
    /// 応答へ載せる機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactStage::Encode => "encode",
            ArtifactStage::Write => "handoff",
        }
    }
}

impl std::fmt::Display for ArtifactStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            ArtifactStage::Encode => "レンダリング結果を符号化できませんでした",
            ArtifactStage::Write => "レンダリング結果を書き出せませんでした",
        };
        f.write_str(text)
    }
}

/// レンダリングが落ちた段。
///
/// レンダリングはプロジェクトを変更しないため、失敗しても「変更が入ったか
/// 不明」という警戒は要らない。代わりにどの段で落ちたかを伝え、要求元が
/// 再送してよいかを判断できるようにする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStage {
    /// 完了コールバックの待ち。
    Wait,
    /// PNG への符号化。
    Encode,
    /// 引き渡し用ファイルへの書き込み。
    Handoff,
}

impl RenderStage {
    /// 応答へ載せる機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            RenderStage::Wait => "wait",
            RenderStage::Encode => "encode",
            RenderStage::Handoff => "handoff",
        }
    }
}

/// レンダリングの失敗。
///
/// 補助情報には引き渡し用の識別子・パス・画像を含めない。画像には利用者の
/// プロジェクトの内容が写るため、応答にも表示にも断片を残さない。
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// 受付判定・編集情報の取得で生じた失敗。
    ///
    /// これらは読み取り経路とまったく同じ失敗分類を持つ。別の列挙へ写し替えると
    /// 対応の取り違えを招くため、そのまま埋め込む。
    #[error(transparent)]
    Read(#[from] ReadError),
    /// 現在シーンが要求の前提と異なる。
    ///
    /// 投入前と完了後の 2 回照合するため、どちらでもここへ来る。
    #[error("現在のシーンが要求の前提と一致しません")]
    SceneMismatch {
        /// 要求が前提としたシーン ID。
        expected: i32,
        /// 実際の現在シーン ID。
        current: i32,
    },
    /// フレームがシーンの範囲外。
    #[error("要求されたフレームはシーンの範囲外です")]
    FrameOutOfRange,
    /// 想定される非圧縮サイズが上限を超える。
    #[error("シーンの解像度に対する非圧縮サイズが上限を超えています")]
    FrameTooLarge,
    /// 完了コールバックが期限内に来なかった。
    #[error("レンダリングの完了を期限内に確認できませんでした")]
    WaitTimeout,
    /// 停止要求により待機を打ち切った。
    #[error("停止処理が始まったためレンダリングを打ち切りました")]
    ShuttingDown,
    /// 未完了のまま放棄された要求が多すぎる。
    #[error("未完了のレンダリングが多いため新しい要求を受け付けられません")]
    TooManyAbandoned,
    /// コールバックが返した buffer が矛盾している。
    #[error("{rule}")]
    InvalidBuffer {
        /// 破れた規則。
        rule: BufferRule,
    },
    /// 符号化または引き渡し用ファイルへの書き込みに失敗した。
    #[error("{stage}")]
    Artifact {
        /// 失敗した段。
        stage: ArtifactStage,
    },
    /// SDK の呼び出しが失敗した。
    #[error("SDK の呼び出しに失敗しました: {operation}")]
    Sdk {
        /// 失敗した SDK 関数の名前。
        operation: &'static str,
    },
    /// 要求処理または完了コールバックで panic を捕捉した。
    #[error("レンダリング処理で panic を捕捉しました")]
    Panicked,
}

impl RenderError {
    /// 応答へ載せるエラーコードを返す。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            RenderError::Read(error) => error.error_code(),
            RenderError::SceneMismatch { .. } => ErrorCode::PreconditionFailed,
            RenderError::FrameOutOfRange => ErrorCode::InvalidArgument,
            RenderError::FrameTooLarge => ErrorCode::UnsupportedOperation,
            RenderError::WaitTimeout => ErrorCode::Timeout,
            RenderError::ShuttingDown | RenderError::TooManyAbandoned => ErrorCode::HostBusy,
            RenderError::InvalidBuffer { .. } | RenderError::Sdk { .. } => ErrorCode::SdkError,
            RenderError::Artifact { .. } | RenderError::Panicked => ErrorCode::InternalError,
        }
    }

    /// 同じ要求をそのまま再送して成功し得るか。
    pub fn retryable(&self) -> bool {
        self.error_code().default_retryable()
    }

    /// 再試行までに空けるべき時間（ミリ秒）。案内できない場合は `None`。
    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            RenderError::Read(error) => error.retry_after_ms(),
            RenderError::TooManyAbandoned => Some(TOO_MANY_ABANDONED_RETRY_AFTER_MS),
            RenderError::ShuttingDown => Some(SHUTTING_DOWN_RETRY_AFTER_MS),
            _ => None,
        }
    }

    /// レンダリングが落ちた段を返す。段を持たない失敗では `None`。
    fn render_stage(&self) -> Option<RenderStage> {
        match self {
            RenderError::WaitTimeout => Some(RenderStage::Wait),
            RenderError::Artifact {
                stage: ArtifactStage::Encode,
            } => Some(RenderStage::Encode),
            RenderError::Artifact {
                stage: ArtifactStage::Write,
            } => Some(RenderStage::Handoff),
            _ => None,
        }
    }

    /// 要求元が取るべき再試行のしかたを返す。
    fn retry_requires(&self) -> RetryRequires {
        match self {
            RenderError::Read(ReadError::NotReady)
            | RenderError::Read(ReadError::EditBlocked { .. })
            | RenderError::WaitTimeout
            | RenderError::ShuttingDown
            | RenderError::TooManyAbandoned => RetryRequires::Resend,
            other => match other.error_code() {
                ErrorCode::PreconditionFailed => RetryRequires::Refetch,
                _ => RetryRequires::None,
            },
        }
    }

    /// 応答へ載せる補助情報を組み立てる。
    ///
    /// 引き渡し用の識別子・パス・画像は含めない。含めるのは要求元が次の行動を
    /// 決められる値だけである。
    ///
    /// 期限超過に「変更が入った可能性」を伝えるキーを付けないのは、
    /// レンダリングがプロジェクトを一切変更しないためである。付けると要求元が
    /// 編集と同じ警戒（読み直してから再送）を要すると誤解する。
    pub fn details(&self) -> Value {
        let mut details = Map::new();
        self.fill_details(&mut details);
        if let RenderError::SceneMismatch { .. } = self {
            details.insert("mismatch".to_string(), json!("scene_id"));
        }
        if let Some(stage) = self.render_stage() {
            details.insert("render_stage".to_string(), json!(stage.as_str()));
        }
        if let Some(retry_after_ms) = self.retry_after_ms() {
            details.insert("retry_after_ms".to_string(), json!(retry_after_ms));
        }
        details.insert(
            "retry_requires".to_string(),
            json!(self.retry_requires().as_str()),
        );
        Value::Object(details)
    }

    /// 失敗の種類ごとの補助情報を書き込む。
    fn fill_details(&self, details: &mut Map<String, Value>) {
        match self {
            RenderError::Read(error) => {
                if let Value::Object(source) = error.details() {
                    details.extend(source);
                }
            }
            RenderError::SceneMismatch { expected, current } => {
                details.insert("expected_scene_id".to_string(), json!(expected));
                details.insert("current_scene_id".to_string(), json!(current));
            }
            RenderError::FrameOutOfRange => {
                details.insert("reason".to_string(), json!("frame_out_of_range"));
            }
            RenderError::FrameTooLarge => {
                details.insert("reason".to_string(), json!("frame_too_large"));
            }
            RenderError::InvalidBuffer { rule } => {
                details.insert("reason".to_string(), json!(rule.as_str()));
            }
            RenderError::Sdk { operation } => {
                details.insert("sdk_operation".to_string(), json!(operation));
            }
            RenderError::WaitTimeout
            | RenderError::ShuttingDown
            | RenderError::TooManyAbandoned
            | RenderError::Artifact { .. }
            | RenderError::Panicked => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read::EditState;

    /// 全 variant の代表値。新しい variant を足したらここへも足す。
    fn all_errors() -> Vec<RenderError> {
        vec![
            RenderError::Read(ReadError::NotReady),
            RenderError::Read(ReadError::EditBlocked {
                state: EditState::Save,
            }),
            RenderError::Read(ReadError::Sdk {
                operation: "get_edit_info",
            }),
            RenderError::Read(ReadError::Panicked),
            RenderError::SceneMismatch {
                expected: 0,
                current: 3,
            },
            RenderError::FrameOutOfRange,
            RenderError::FrameTooLarge,
            RenderError::WaitTimeout,
            RenderError::ShuttingDown,
            RenderError::TooManyAbandoned,
            RenderError::InvalidBuffer {
                rule: BufferRule::PitchTooSmall,
            },
            RenderError::Artifact {
                stage: ArtifactStage::Encode,
            },
            RenderError::Artifact {
                stage: ArtifactStage::Write,
            },
            RenderError::Sdk {
                operation: "rendering_scene_video",
            },
            RenderError::Panicked,
        ]
    }

    /// variant を表す名前を返す。
    ///
    /// 網羅 match で書く。variant を足すとここがコンパイルエラーになり、すぐ下の
    /// 一覧と [`all_errors`] へ足す必要があることが分かる。
    fn variant_name(error: &RenderError) -> &'static str {
        match error {
            RenderError::Read(_) => "Read",
            RenderError::SceneMismatch { .. } => "SceneMismatch",
            RenderError::FrameOutOfRange => "FrameOutOfRange",
            RenderError::FrameTooLarge => "FrameTooLarge",
            RenderError::WaitTimeout => "WaitTimeout",
            RenderError::ShuttingDown => "ShuttingDown",
            RenderError::TooManyAbandoned => "TooManyAbandoned",
            RenderError::InvalidBuffer { .. } => "InvalidBuffer",
            RenderError::Artifact { .. } => "Artifact",
            RenderError::Sdk { .. } => "Sdk",
            RenderError::Panicked => "Panicked",
        }
    }

    /// 全 [`BufferRule`]。新しい規則を足したらここへも足す。
    fn all_buffer_rules() -> Vec<BufferRule> {
        let rules = vec![
            BufferRule::FrameMismatch,
            BufferRule::DimensionOutOfRange,
            BufferRule::EmptyDimension,
            BufferRule::RowBytesOverflow,
            BufferRule::PitchTooSmall,
            BufferRule::EmptyBuffer,
            BufferRule::BufferLengthMismatch,
            BufferRule::FrameTooLarge,
        ];
        for rule in &rules {
            // 網羅 match。規則を足すとここがコンパイルエラーになる。
            match rule {
                BufferRule::FrameMismatch
                | BufferRule::DimensionOutOfRange
                | BufferRule::EmptyDimension
                | BufferRule::RowBytesOverflow
                | BufferRule::PitchTooSmall
                | BufferRule::EmptyBuffer
                | BufferRule::BufferLengthMismatch
                | BufferRule::FrameTooLarge => {}
            }
        }
        rules
    }

    #[test]
    fn all_errors_covers_every_variant() {
        const VARIANTS: &[&str] = &[
            "Read",
            "SceneMismatch",
            "FrameOutOfRange",
            "FrameTooLarge",
            "WaitTimeout",
            "ShuttingDown",
            "TooManyAbandoned",
            "InvalidBuffer",
            "Artifact",
            "Sdk",
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
    fn error_codes_match_the_render_mapping() {
        let mapped: Vec<ErrorCode> = all_errors().iter().map(RenderError::error_code).collect();
        assert_eq!(
            mapped,
            vec![
                ErrorCode::HostBusy,
                ErrorCode::EditBlocked,
                ErrorCode::SdkError,
                ErrorCode::InternalError,
                ErrorCode::PreconditionFailed,
                // フレームは要求元が選ぶ値であり、直せば通る。
                ErrorCode::InvalidArgument,
                // 解像度は要求元が選んだ値ではない。直しても通らない。
                ErrorCode::UnsupportedOperation,
                ErrorCode::Timeout,
                ErrorCode::HostBusy,
                ErrorCode::HostBusy,
                ErrorCode::SdkError,
                ErrorCode::InternalError,
                ErrorCode::InternalError,
                ErrorCode::SdkError,
                ErrorCode::InternalError,
            ]
        );
    }

    #[test]
    fn cancelled_is_never_produced() {
        for error in all_errors() {
            assert_ne!(
                error.error_code(),
                ErrorCode::Cancelled,
                "{error} が cancelled になりました"
            );
        }
    }

    #[test]
    fn a_timeout_never_warns_about_an_applied_change() {
        // レンダリングはプロジェクトを変更しない。変更の有無を伝えるキーが
        // 現れると、要求元は編集と同じ警戒を要すると誤解する。
        for error in all_errors() {
            let details = error.details();
            assert!(
                details.get("change_applied").is_none(),
                "{error} が変更の適用を名乗りました"
            );
            assert!(
                details.get("mutation_issued").is_none(),
                "{error} が変更の発行を名乗りました"
            );
        }
        assert_eq!(
            RenderError::WaitTimeout.details()["render_stage"],
            json!("wait")
        );
    }

    #[test]
    fn artifact_failures_name_the_stage_they_failed_in() {
        assert_eq!(
            RenderError::Artifact {
                stage: ArtifactStage::Encode
            }
            .details()["render_stage"],
            json!("encode")
        );
        assert_eq!(
            RenderError::Artifact {
                stage: ArtifactStage::Write
            }
            .details()["render_stage"],
            json!("handoff")
        );
    }

    #[test]
    fn buffer_failures_name_the_rule_they_broke() {
        for rule in all_buffer_rules() {
            let error = RenderError::InvalidBuffer { rule };
            assert_eq!(error.error_code(), ErrorCode::SdkError, "{error}");
            assert_eq!(error.details()["reason"], json!(rule.as_str()), "{error}");
        }
    }

    #[test]
    fn scene_mismatch_asks_for_a_refetch() {
        let details = RenderError::SceneMismatch {
            expected: 0,
            current: 3,
        }
        .details();
        assert_eq!(details["mismatch"], json!("scene_id"));
        assert_eq!(details["expected_scene_id"], json!(0));
        assert_eq!(details["current_scene_id"], json!(3));
        assert_eq!(details["retry_requires"], json!("refetch"));
    }

    #[test]
    fn transient_states_allow_a_plain_resend_with_a_delay() {
        for error in [
            RenderError::WaitTimeout,
            RenderError::ShuttingDown,
            RenderError::TooManyAbandoned,
            RenderError::Read(ReadError::NotReady),
        ] {
            assert_eq!(
                error.details()["retry_requires"],
                json!("resend"),
                "{error} がそのままの再送を案内していません"
            );
            assert!(error.retryable(), "{error} が再試行不可になりました");
        }
        for error in [RenderError::ShuttingDown, RenderError::TooManyAbandoned] {
            assert!(
                error.details().get("retry_after_ms").is_some(),
                "{error} が再試行の間隔を案内していません"
            );
        }
    }

    #[test]
    fn permanent_failures_do_not_advise_a_retry() {
        for error in [
            RenderError::FrameOutOfRange,
            RenderError::FrameTooLarge,
            RenderError::InvalidBuffer {
                rule: BufferRule::EmptyBuffer,
            },
            RenderError::Artifact {
                stage: ArtifactStage::Write,
            },
            RenderError::Panicked,
        ] {
            assert_eq!(
                error.details()["retry_requires"],
                json!("none"),
                "{error} が再試行を案内しています"
            );
        }
    }

    #[test]
    fn failures_outside_preconditions_do_not_name_a_mismatch() {
        for error in all_errors() {
            if error.error_code() == ErrorCode::PreconditionFailed {
                continue;
            }
            assert!(
                error.details().get("mismatch").is_none(),
                "{error} が前提条件の食い違いを名乗りました"
            );
        }
    }

    #[test]
    fn details_only_use_allowed_keys() {
        // 補助情報のキーはここで列挙したものに限る。新しいキーを足す際は
        // 引き渡し用の識別子・パス・画像でないことを確かめた上で追加する。
        const ALLOWED: &[&str] = &[
            "retry_after_ms",
            "edit_state",
            "expected_scene_id",
            "current_scene_id",
            "mismatch",
            "reason",
            "render_stage",
            "sdk_operation",
            "retry_requires",
        ];
        for rule in all_buffer_rules() {
            let error = RenderError::InvalidBuffer { rule };
            for key in error.details().as_object().unwrap().keys() {
                assert!(
                    ALLOWED.contains(&key.as_str()),
                    "{error} の補助情報に未許可のキー {key} が含まれています"
                );
            }
        }
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
    fn details_and_messages_expose_neither_paths_nor_tokens() {
        // 画像には利用者のプロジェクトの内容が写る。引き渡し用の識別子を
        // 渡せば、それだけで成果物の在り処が分かる。どちらも応答へ出さない。
        for error in all_errors() {
            let text = format!("{} {}", error, error.details());
            assert!(!text.contains("0x"), "{text}");
            assert!(!text.to_lowercase().contains("handle"), "{text}");
            assert!(!text.to_lowercase().contains("pointer"), "{text}");
            assert!(!text.to_lowercase().contains("token"), "{text}");
            assert!(!text.contains(r"C:\"), "{text}");
            assert!(!text.contains(".png"), "{text}");
        }
    }
}
