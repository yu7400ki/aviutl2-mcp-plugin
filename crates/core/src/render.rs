//! render operation の params / result と、要求内容だけで決まる入力検証。
//!
//! 描画はプロジェクトを変更しないため、要求はプロジェクト境界の照合を求め
//! ない。前提条件が食い違っても損害が起きず、要求元へ描画のためだけに epoch
//! を用意させる理由が無い。**現在シーンの一致だけは必須とする** — シーンは
//! 「どれを描いたか」を左右する唯一の軸であり、切り替えた直後の要求が別の
//! シーンの絵を成功として受け取ることを防ぐ。
//!
//! # 大きさの上限をここへ置く理由
//!
//! 描画の成果物は、画を描く側（非圧縮 RGBA を抱える）と、成果物を引き取る側
//! （符号化後のファイルを読む）の 2 か所で大きさを判定される。判定する値が
//! 異なるため上限は 2 つ要るが、**別々の crate に別々に置くと、片方だけを
//! 通る帯ができる**。描く側の上限しか見ない実装は、引き取る側が捨てると
//! 決まっている大きさの成果物を、符号化と書き出しまで済ませてから失う。
//!
//! そこで [`MAX_RENDER_FRAME_BYTES`] と [`ARTIFACT_MAX_BYTES`] を要求と応答の
//! 語彙と同じ場所へ置き、両側が同じ値を引くようにする。2 つの関係は本
//! モジュールのテストで固定する。

use crate::error::ErrorCode;
use serde::{Deserialize, Serialize};

/// 1 フレーム分の非圧縮 RGBA8 が占めてよいバイト数の上限。
///
/// 描く側が投入前に、シーンの解像度から求めた `width * height * 4` をこれと
/// 比べる。超える解像度は、確保そのものがホストのメモリを圧迫するため描かない。
pub const MAX_RENDER_FRAME_BYTES: u64 = 256 * 1024 * 1024;

/// 符号化後の描画成果物が占めてよいバイト数の上限。
///
/// 引き取る側はこれを超えるファイルを読まずに捨てる。**符号化後の大きさは
/// 非圧縮の大きさから決まらない** — 情報量の多い画では PNG がほとんど縮まず、
/// [`MAX_RENDER_FRAME_BYTES`] を通った描画でもこの上限を超え得る。したがって
/// 描く側も、書き出す前に符号化の結果をこの値と比べる必要がある。
pub const ARTIFACT_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// 描画するフレーム番号の上限。
///
/// フレーム番号は `i32` で受け渡され、0 以上しか意味を持たないため、符号なし
/// で受けたうえで `i32` に収まることだけを課す。シーンの実際の長さとの比較は
/// 編集情報を要するため、ここでは行わない。
const MAX_FRAME: u32 = i32::MAX as u32;

/// 描画結果の出力形式。
///
/// 現時点では 1 種類しか無いが、要求のフィールドとして残す。形式を増やす際に
/// 要求の形を変えずに済む。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderFormat {
    /// PNG。
    #[default]
    Png,
}

/// `render_frame` の params。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderFrameParams {
    /// 現在シーンの一致確認に使う guard。
    pub expected_scene_id: i32,
    /// 0 始まりのフレーム番号。
    pub frame: u32,
    /// 出力形式。省略時は [`RenderFormat::Png`]。
    #[serde(default)]
    pub format: RenderFormat,
}

impl RenderFrameParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// 見るのはフレーム番号が受け渡せる範囲に収まることだけである。シーンの
    /// 長さを超えるかは編集情報を読める層が判定する。
    pub fn validate(&self) -> Result<(), RenderInputError> {
        if self.frame > MAX_FRAME {
            return Err(RenderInputError::FrameOutOfRange {
                value: self.frame,
                max: MAX_FRAME,
            });
        }
        Ok(())
    }
}

/// 要求内容だけで決まる検証の失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RenderInputError {
    /// フレーム番号が受け付けられる範囲を超えている。
    #[error("frame は {max} 以下である必要があります: {value}")]
    FrameOutOfRange {
        /// 指定された値。
        value: u32,
        /// 許容する最大値。
        max: u32,
    },
}

impl RenderInputError {
    /// 対応するエラーコードを返す。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            RenderInputError::FrameOutOfRange { .. } => ErrorCode::InvalidArgument,
        }
    }
}

/// `render_frame` の result。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderFrameResult {
    /// 描画時点のプロジェクトの epoch。
    pub project_epoch: String,
    /// 描画時点のプロジェクトの revision。
    pub project_revision: u64,
    /// 描画したシーン。要求の `expected_scene_id` と一致する。
    ///
    /// 何を描いた絵かを応答だけで確定できるようにするために返す。成果物の
    /// 識別子からは辿れない。
    pub scene_id: i32,
    /// 描画したフレーム。要求の `frame` と一致する。
    pub frame: u32,
    /// 画像の幅（ピクセル）。
    pub width: u32,
    /// 画像の高さ（ピクセル）。
    pub height: u32,
    /// 画像の MIME type。
    pub media_type: String,
    /// 画像のバイト数。
    pub byte_length: u64,
    /// `"sha256:"` と小文字十六進のダイジェスト。
    pub sha256: String,
    /// 受け渡しファイルの識別子。
    ///
    /// **要求元へ渡す値ではない。** 受け取った側が成果物を引き取るまでの間だけ
    /// 用いる内部の識別子であり、応答・補助情報・ログのいずれへも出さない。
    /// 出せば、要求元が成果物のファイル名を推測できる経路が生まれる。
    pub handoff_token: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn params() -> RenderFrameParams {
        RenderFrameParams {
            expected_scene_id: 0,
            frame: 120,
            format: RenderFormat::Png,
        }
    }

    #[test]
    fn size_caps_are_fixed() {
        assert_eq!(MAX_RENDER_FRAME_BYTES, 256 * 1024 * 1024);
        assert_eq!(ARTIFACT_MAX_BYTES, 32 * 1024 * 1024);
    }

    #[test]
    fn the_encoded_cap_is_smaller_than_the_uncompressed_cap() {
        // 符号化後の上限が非圧縮の上限以上になると、引き取る側の上限は
        // 描く側の判定に覆われることになり、書き出す前の判定が要らないという
        // 誤読を招く。2 つの上限は「非圧縮を通っても符号化後で落ち得る」
        // 関係にあることを、値そのもので固定する。
        let encoded = ARTIFACT_MAX_BYTES;
        let uncompressed = MAX_RENDER_FRAME_BYTES;
        assert!(
            encoded < uncompressed,
            "符号化後の上限 {encoded} が非圧縮の上限 {uncompressed} を下回りません"
        );
    }

    #[test]
    fn params_roundtrip() {
        let s = serde_json::to_string(&params()).unwrap();
        assert_eq!(
            serde_json::from_str::<RenderFrameParams>(&s).unwrap(),
            params()
        );
    }

    #[test]
    fn the_format_defaults_to_png() {
        let decoded: RenderFrameParams =
            serde_json::from_value(json!({"expected_scene_id": 0, "frame": 120})).unwrap();
        assert_eq!(decoded, params());
        assert_eq!(RenderFormat::default(), RenderFormat::Png);
        assert_eq!(
            serde_json::to_value(RenderFormat::Png).unwrap(),
            json!("png")
        );
    }

    #[test]
    fn params_require_the_scene_guard_and_the_frame() {
        for value in [json!({"frame": 0}), json!({"expected_scene_id": 0})] {
            assert!(serde_json::from_value::<RenderFrameParams>(value).is_err());
        }
    }

    #[test]
    fn params_reject_unknown_fields() {
        let mut value = serde_json::to_value(params()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(1));
        assert!(serde_json::from_value::<RenderFrameParams>(value).is_err());
    }

    #[test]
    fn the_frame_must_fit_in_the_representable_range() {
        assert_eq!(MAX_FRAME, 2_147_483_647);
        assert_eq!(params().validate(), Ok(()));
        assert_eq!(
            RenderFrameParams {
                frame: MAX_FRAME,
                ..params()
            }
            .validate(),
            Ok(())
        );

        let error = RenderFrameParams {
            frame: MAX_FRAME + 1,
            ..params()
        }
        .validate()
        .unwrap_err();
        assert_eq!(
            error,
            RenderInputError::FrameOutOfRange {
                value: MAX_FRAME + 1,
                max: MAX_FRAME,
            }
        );
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    }

    #[test]
    fn the_result_roundtrips_and_accepts_unknown_optional_fields() {
        let result = RenderFrameResult {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            project_revision: 42,
            scene_id: 0,
            frame: 120,
            width: 1920,
            height: 1080,
            media_type: "image/png".to_string(),
            byte_length: 4096,
            sha256: format!("sha256:{}", "0".repeat(64)),
            handoff_token: "0123456789abcdef0123456789abcdef".to_string(),
        };
        let s = serde_json::to_string(&result).unwrap();
        assert_eq!(
            serde_json::from_str::<RenderFrameResult>(&s).unwrap(),
            result
        );

        let mut value = serde_json::to_value(&result).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(1));
        assert_eq!(
            serde_json::from_value::<RenderFrameResult>(value).unwrap(),
            result
        );
    }
}
