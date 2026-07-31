use aviutl2_mcp_server::api::{ListInstancesRequest, aviutl2_list_instances};
use aviutl2_mcp_server::discovery::default_registry_dir;
use aviutl2_mcp_server::init_logging;
use aviutl2_mcp_server::mcp::{AviUtl2McpServer, REGISTRY_DIR_ENV};
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

/// MCP サーバーは 1 本の stdio 接続だけを扱うため、単一スレッドの実行器で足りる。
/// tool call の同期処理は `spawn_blocking` の専用スレッドで走る。
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    init_logging();

    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "list-instances" {
        return run_list_instances_cli();
    }

    let Some(registry_dir) = registry_dir() else {
        tracing::error!("registry ディレクトリを決定できませんでした");
        return ExitCode::FAILURE;
    };

    // 描画成果物の保管庫は起動時に開く。保管庫はこのサービスが破棄されるときに
    // ディレクトリごと消えるため、寿命はプロセスの寿命と一致する。
    let server = match AviUtl2McpServer::new(registry_dir) {
        Ok(server) => server,
        Err(e) => {
            tracing::error!(error = %e, "描画成果物の保管庫を開けませんでした");
            return ExitCode::FAILURE;
        }
    };

    let service = match server.serve(stdio()).await {
        Ok(service) => service,
        Err(e) => {
            tracing::error!(error = %e, "MCP サーバーを開始できませんでした");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!("aviutl2-mcp-server started");

    match service.waiting().await {
        Ok(reason) => {
            tracing::info!(reason = ?reason, "aviutl2-mcp-server stopped");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!(error = %e, "MCP サーバーが異常終了しました");
            ExitCode::FAILURE
        }
    }
}

/// registry ディレクトリを決定する。環境変数の指定を既定より優先する。
fn registry_dir() -> Option<PathBuf> {
    match std::env::var(REGISTRY_DIR_ENV) {
        Ok(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => default_registry_dir(),
    }
}

/// テスト用 CLI: `aviutl2_list_instances` を実行し、結果を stderr に JSON で出力する。
///
/// stdout は MCP プロトコル専用として汚染しない。
fn run_list_instances_cli() -> ExitCode {
    let Some(registry_dir) = registry_dir() else {
        tracing::error!("registry ディレクトリを決定できませんでした");
        return ExitCode::FAILURE;
    };

    let request = ListInstancesRequest {
        offset: 0,
        limit: 50,
    };

    match aviutl2_list_instances(&registry_dir, request) {
        Ok(response) => {
            let json = serde_json::to_string_pretty(&response).unwrap_or_else(|_| "{}".to_string());
            // テスト用出力は stderr へ。
            let _ = writeln!(std::io::stderr(), "{}", json);
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!(error = %e, "aviutl2_list_instances failed");
            ExitCode::FAILURE
        }
    }
}
