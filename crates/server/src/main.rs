use aviutl2_mcp_core::settings::{SettingsReader, SettingsRefresh, settings_location};
use aviutl2_mcp_server::api::{ListInstancesRequest, list_instances};
use aviutl2_mcp_server::artifact::base_dir_for_registry;
use aviutl2_mcp_server::discovery::default_registry_dir;
use aviutl2_mcp_server::init_logging;
use aviutl2_mcp_server::mcp::{AviUtl2McpServer, REGISTRY_DIR_ENV};
use aviutl2_mcp_server::settings::{ParentPolicy, SettingsWatcher};
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

/// MCP サーバーは 1 本の stdio 接続だけを扱うため、単一スレッドの実行器で足りる。
/// tool call の同期処理は `spawn_blocking` の専用スレッドで走る。
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "list-instances" {
        init_logging(&aviutl2_mcp_core::settings::Settings::default());
        return run_list_instances_cli();
    }

    let Some(registry_dir) = registry_dir() else {
        init_logging(&aviutl2_mcp_core::settings::Settings::default());
        tracing::error!("registry ディレクトリを決定できませんでした");
        return ExitCode::FAILURE;
    };

    // 設定は記録の準備より先に読む。ログレベルがそこから決まるためである。
    // 解決で生じた不整合は subscriber が立ってから流す。
    let location = settings_location(&base_dir_for_registry(&registry_dir));
    let mut reader = SettingsReader::new(location.path);
    let refresh = reader.refresh();
    init_logging(&reader.settings());
    match refresh {
        SettingsRefresh::Reloaded(issues) => {
            for issue in &issues {
                tracing::warn!("設定を補正しました: {issue}");
            }
        }
        SettingsRefresh::Unchanged => {}
        SettingsRefresh::Failed(e) => {
            tracing::warn!("設定を読み込めませんでした。既定値で続行します: {e}");
        }
    }

    // MCP の受付を始める前に初期 snapshot を作り、そこから監視を始める。
    // 外から指定された置き場所は作らない。存在しなければ起動しない。
    let parent_policy = if location.overridden {
        ParentPolicy::Require
    } else {
        ParentPolicy::Create
    };
    let watcher = match SettingsWatcher::start(reader, parent_policy) {
        Ok(watcher) => watcher,
        Err(e) => {
            tracing::error!(error = %e, "設定ファイルの監視を開始できませんでした");
            return ExitCode::FAILURE;
        }
    };

    // 描画成果物の保管庫は起動時に開く。保管庫はこのサービスが破棄されるときに
    // ディレクトリごと消えるため、寿命はプロセスの寿命と一致する。
    let server = match AviUtl2McpServer::new(registry_dir, watcher.source()) {
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

/// テスト用 CLI: `list_instances` を実行し、結果を stderr に JSON で出力する。
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

    match list_instances(&registry_dir, request) {
        Ok(response) => {
            let json = serde_json::to_string_pretty(&response).unwrap_or_else(|_| "{}".to_string());
            // テスト用出力は stderr へ。
            let _ = writeln!(std::io::stderr(), "{}", json);
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!(error = %e, "list_instances failed");
            ExitCode::FAILURE
        }
    }
}
