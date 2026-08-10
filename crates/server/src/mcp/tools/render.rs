//! 描画 tool。

use crate::mcp::input::parse_instance_id;
use crate::mcp::render::{RenderFrameInput, RenderFrameOutput};
use crate::mcp::server::{
    AviUtl2McpServer, CallBudgets, ToolSuccess, request_operation, to_structured,
};
use crate::mcp::{describe, failure};
use aviutl2_mcp_core::{OPERATION_RENDER_FRAME, RenderFrameResult};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};

#[tool_router(router = render_tools_router, vis = "pub(in crate::mcp)")]
impl AviUtl2McpServer {
    /// 現在シーンの 1 フレームを描画し、成果物を resource として返す。
    /// 描画できるのは現在シーンだけである。expected_scene_id には
    /// get_edit_info などが返した scene_id をそのまま指定する。
    /// 結果は画像そのものではなく resource URI で返る。内容は resources/read で
    /// 取得する。
    /// 成果物は既定で 10 分後に失効し、失効後の resources/read は not_found となる。
    /// 呼ぶたびに新しい成果物が生まれ、古いものは件数と総量の上限で押し出され得る。
    /// 出力形式は PNG のみである。
    /// プロジェクトは変更しないが、一時ファイルを作りホストの計算資源を使う。
    /// 出力（ファイル書き出し）中は edit_blocked となる。プレビュー再生中は成功し得る。
    /// 描画の途中でシーンを切り替えると precondition_failed となる。ただし
    /// 切り替えて戻した場合は検出できない。
    /// シーンの解像度が大きすぎる場合、および描いた結果が大きすぎる場合は
    /// unsupported_operation となる。どちらも要求を直しても通らない。
    /// timeout は描画されなかったことを意味する。プロジェクトは変更されていない
    /// ため、そのまま再送してよい。
    #[tool(
        name = "render_frame",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        output_schema = crate::mcp::output_schema::as_tool_schema(
            crate::mcp::output_schema::render_frame()
        )
    )]
    pub async fn render_frame(
        &self,
        Parameters(input): Parameters<RenderFrameInput>,
    ) -> CallToolResult {
        let registry_dir = self.registry_dir();
        let CallBudgets { limits, discovery } = self.call_budgets();
        let artifacts = self.artifacts.clone();
        self.run("render_frame", move || {
            let instance_id = parse_instance_id(&input.instance_id)?;
            let params = input.to_params()?;
            let artifacts = artifacts
                .ok_or_else(|| failure::internal_error("描画成果物の保管庫が利用できません"))?;

            // 応答を受けたあと、同じブロッキングタスクの中で成果物を引き取る。
            // 引き渡しの識別子はここで消費して終わり、以降のどの経路にも
            // 現れない。
            let result: RenderFrameResult = request_operation(
                &registry_dir,
                instance_id,
                limits,
                discovery,
                OPERATION_RENDER_FRAME,
                &params,
            )?;
            let artifact = artifacts
                .ingest(
                    &instance_id,
                    &result.handoff_token,
                    result.byte_length,
                    &result.sha256,
                )
                .map_err(|error| {
                    // 理由は分類名だけを残す。引き渡しの識別子もパスも記録しない。
                    tracing::warn!(reason = error.as_code(), "描画成果物を引き取れませんでした",);
                    failure::internal_error("描画成果物を引き取れませんでした")
                })?;

            let output = RenderFrameOutput::new(&result, &artifact);
            Ok(ToolSuccess {
                text: describe::render_frame(&output),
                structured: to_structured(&output)?,
            })
        })
        .await
    }
}
