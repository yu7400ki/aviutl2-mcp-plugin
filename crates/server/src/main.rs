use aviutl2_mcp_server::api::{ListInstancesRequest, aviutl2_list_instances};
use aviutl2_mcp_server::discovery::default_registry_dir;
use aviutl2_mcp_server::init_logging;
use std::io::Write;

fn main() {
    init_logging();

    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "list-instances" {
        run_list_instances_cli();
        return;
    }

    tracing::info!("aviutl2-mcp-server started");
}

/// テスト用 CLI: `aviutl2_list_instances` を実行し、結果を stderr に JSON で出力する。
///
/// stdout は MCP プロトコル専用として汚染しない。
fn run_list_instances_cli() {
    let registry_dir = match default_registry_dir() {
        Some(dir) => dir,
        None => {
            tracing::error!("LOCALAPPDATA 環境変数が取得できませんでした");
            std::process::exit(1);
        }
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
        }
        Err(e) => {
            tracing::error!(error = %e, "aviutl2_list_instances failed");
            std::process::exit(1);
        }
    }
}
