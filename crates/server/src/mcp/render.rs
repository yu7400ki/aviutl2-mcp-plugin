//! 描画 tool の入出力型。
//!
//! **応答の型を IPC の result と分ける。** 接続先が返す
//! [`RenderFrameResult`] は引き渡しファイルの
//! 識別子を持ち、そのまま `structuredContent` へ流すと要求元がそのファイル名を
//! 知ることになる。[`RenderFrameOutput`] に対応するフィールドが無いため、
//! 素通しはコンパイルエラーになる。
//!
//! 型を分けるのは規律のためではなく、**漏れる経路を型として存在させない**
//! ためである。

use crate::artifact::Artifact;
use crate::mcp::failure::from_code;
use crate::mcp::input::UUID_PATTERN;
use crate::mcp::server::artifact_resource_uri;
use aviutl2_mcp_core::{ErrorObject, RenderFormat, RenderFrameParams, RenderFrameResult};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// フレーム番号に許す最大値。
///
/// ホストはフレーム番号を 32bit 符号付き整数で受け渡す。シーンの実際の長さとの
/// 比較は編集情報を要するため接続先が行う。
const MAX_FRAME: u32 = i32::MAX as u32;

/// 描画結果の出力形式。
///
/// 現時点では 1 種類しか無いが、形式を増やす際に要求の形を変えずに済むよう
/// フィールドとして残す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RenderFormatInput {
    /// PNG。可逆であり、描画結果を欠損なく運べる。
    #[default]
    Png,
}

impl From<RenderFormatInput> for RenderFormat {
    fn from(value: RenderFormatInput) -> Self {
        match value {
            RenderFormatInput::Png => RenderFormat::Png,
        }
    }
}

/// `aviutl2_render_frame` の入力。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenderFrameInput {
    /// 対象インスタンスの ID。
    #[schemars(length(min = 36, max = 36), pattern(UUID_PATTERN))]
    pub instance_id: String,
    /// 現在シーンの一致確認に使うシーン ID。aviutl2_get_edit_info などが返した値を指定する。
    pub expected_scene_id: i32,
    /// 0 始まりのフレーム番号。
    #[schemars(range(max = MAX_FRAME))]
    pub frame: u32,
    /// 出力形式。省略時は png。
    #[serde(default)]
    pub format: RenderFormatInput,
}

impl RenderFrameInput {
    /// IPC の params へ変換する。
    pub fn to_params(&self) -> Result<RenderFrameParams, ErrorObject> {
        let params = RenderFrameParams {
            expected_scene_id: self.expected_scene_id,
            frame: self.frame,
            format: self.format.into(),
        };
        params
            .validate()
            .map_err(|error| from_code(error.error_code(), error.to_string()))?;
        Ok(params)
    }
}

/// 要求元へ渡す成果物の参照。
///
/// 実体のパスも、接続先が書いた引き渡しファイルの識別子も持たない。要求元へ
/// 渡す識別子から他プロセスのファイル名を導けないようにするため、
/// `artifact_id` は引き渡しの識別子とは別に採番された値である。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactRef {
    /// server が採番した UUID v4。
    pub artifact_id: String,
    /// `aviutl2://artifacts/{artifact_id}`。
    pub uri: String,
    /// MIME type。
    pub media_type: String,
    /// 実体のバイト数。
    pub byte_length: u64,
    /// `"sha256:"` と小文字十六進のダイジェスト。
    pub sha256: String,
    /// RFC 3339 の失効時刻。これ以降は not_found になる。
    pub expires_at: String,
}

impl ArtifactRef {
    /// 引き取った成果物から参照を組み立てる。
    fn new(artifact: &Artifact) -> Self {
        Self {
            uri: artifact_resource_uri(&artifact.artifact_id),
            artifact_id: artifact.artifact_id.clone(),
            media_type: artifact.media_type.to_string(),
            byte_length: artifact.byte_length,
            sha256: artifact.sha256.clone(),
            expires_at: artifact.expires_at.to_rfc3339(),
        }
    }
}

/// `aviutl2_render_frame` の structuredContent。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderFrameOutput {
    /// 描画時点のプロジェクトの epoch。
    pub project_epoch: String,
    /// 描画時点のプロジェクトの revision。
    pub project_revision: u64,
    /// 描画したシーン。
    pub scene_id: i32,
    /// 描画したフレーム。
    pub frame: u32,
    /// 画像の幅（ピクセル）。
    pub width: u32,
    /// 画像の高さ（ピクセル）。
    pub height: u32,
    /// 成果物の参照。
    pub artifact: ArtifactRef,
}

impl RenderFrameOutput {
    /// 接続先の応答と、引き取った成果物から応答を組み立てる。
    ///
    /// **フィールドを 1 つずつ写す。** 応答の残りをまとめて写す形にすると、
    /// 接続先の result へフィールドが増えたときに黙って通り抜ける。
    pub fn new(result: &RenderFrameResult, artifact: &Artifact) -> Self {
        Self {
            project_epoch: result.project_epoch.clone(),
            project_revision: result.project_revision,
            scene_id: result.scene_id,
            frame: result.frame,
            width: result.width,
            height: result.height,
            artifact: ArtifactRef::new(artifact),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::ErrorCode;
    use serde_json::{Value, json};

    const SAMPLE_ID: &str = "8df98c04-e7c2-4f98-b3ce-fc1c39d76414";

    fn input_json() -> Value {
        json!({
            "instance_id": SAMPLE_ID,
            "expected_scene_id": 3,
            "frame": 120,
        })
    }

    #[test]
    fn the_format_defaults_to_png() {
        let input: RenderFrameInput =
            serde_json::from_value(input_json()).expect("形式の省略を受理する");
        assert_eq!(input.format, RenderFormatInput::Png);
        let params = input.to_params().expect("params へ変換できる");
        assert_eq!(params.format, RenderFormat::Png);
        assert_eq!(params.expected_scene_id, 3);
        assert_eq!(params.frame, 120);
    }

    #[test]
    fn the_scene_guard_and_the_frame_are_required() {
        for key in ["expected_scene_id", "frame"] {
            let mut value = input_json();
            value.as_object_mut().expect("object").remove(key);
            assert!(
                serde_json::from_value::<RenderFrameInput>(value).is_err(),
                "{key} の欠落が受理されました"
            );
        }
    }

    #[test]
    fn unknown_fields_and_unknown_formats_are_rejected() {
        let mut value = input_json();
        value
            .as_object_mut()
            .expect("object")
            .insert("future".to_string(), json!(1));
        assert!(serde_json::from_value::<RenderFrameInput>(value).is_err());

        let mut value = input_json();
        value["format"] = json!("webp");
        assert!(serde_json::from_value::<RenderFrameInput>(value).is_err());
    }

    #[test]
    fn a_frame_outside_the_representable_range_is_rejected() {
        // schema が宣言する上限は接続前に実際へ確かめる。
        let input = RenderFrameInput {
            instance_id: SAMPLE_ID.to_string(),
            expected_scene_id: 3,
            frame: MAX_FRAME + 1,
            format: RenderFormatInput::Png,
        };
        let error = input.to_params().expect_err("範囲外は拒否される");
        assert_eq!(error.code, ErrorCode::InvalidArgument);

        let input = RenderFrameInput {
            frame: MAX_FRAME,
            ..input
        };
        assert!(input.to_params().is_ok(), "上限ちょうどが拒否されました");
    }
}
