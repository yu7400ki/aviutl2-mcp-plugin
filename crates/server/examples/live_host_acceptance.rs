//! AviUtl2 実機を用いた完了条件の受け入れ確認。
//!
//! 「3 プロセスを異なる `instance_id` で区別できること」および
//! 「1 プロセス終了後に stale entry を返さないこと」を、実際に稼働する AviUtl2 が
//! 書いた descriptor に対して検証する。mock で代替できない部分（実プロセスが書いた
//! descriptor を読めるか、実プロセスの終了で登録が無効化されるか）だけを対象とする。
//!
//! # 分離方式
//!
//! 実機を要するため、テストターゲットではなく example ターゲットとして定義する。
//! example は `cargo test` ではビルドのみ行われ実行されない。したがって
//! `cargo test --workspace --all-features` が AviUtl2 を起動することはなく、
//! 一方で `cargo clippy --workspace --all-targets --all-features` の検査対象には
//! 含まれるため、型検査と lint は常に働く。
//!
//! # 実行方法
//!
//! ```text
//! cargo run -p aviutl2-mcp-server --example live_host_acceptance
//! ```
//!
//! 実行者は表示される指示に従って AviUtl2 の起動・終了を行う。判定は本ターゲットが行い、
//! 不合格なら終了コード 1 で終了する。registry ディレクトリは環境変数
//! `AVIUTL2_MCP_REGISTRY_DIR` で上書きできる。
//!
//! 本ターゲットは MCP server ではないため、対話用の出力は stdout へ書く。

use aviutl2_mcp_core::InstanceInfo;
use aviutl2_mcp_server::api::{
    ListInstancesRequest, ListInstancesResponse, aviutl2_list_instances,
};
use aviutl2_mcp_server::discovery::default_registry_dir;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// registry ディレクトリを上書きする環境変数。
const REGISTRY_DIR_ENV: &str = "AVIUTL2_MCP_REGISTRY_DIR";
/// 完了条件が要求する同時起動プロセス数。
const EXPECTED_INSTANCES: usize = 3;
/// 期待件数に達するまで待つ上限。
const READY_TIMEOUT: Duration = Duration::from_secs(120);
/// 一覧の再試行間隔。
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// 一覧要求の取得件数。
const LIST_LIMIT: u32 = 50;

fn main() {
    match run() {
        Ok(()) => {
            println!();
            println!("合格: 3 件を区別でき、1 件終了後に stale entry を返しませんでした。");
        }
        Err(message) => {
            println!();
            eprintln!("不合格: {message}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<(), String> {
    let registry_dir = registry_dir()?;
    println!("registry ディレクトリ: {}", registry_dir.display());

    prepare(&registry_dir)?;
    let launched = launch_three(&registry_dir)?;
    let (closed, remaining) = close_one(&registry_dir, &launched)?;
    relaunch_closed(&registry_dir, &closed, &remaining)
}

/// 前提条件を整える。
///
/// 稼働中の AviUtl2 が無く、registry ディレクトリに descriptor が残っていない状態から始める。
/// ここで残留 descriptor が消えることは、実プロセスの終了が登録を無効化することの確認でもある。
fn prepare(registry_dir: &Path) -> Result<(), String> {
    prompt("すべての AviUtl2 を終了してから Enter を押してください。");

    let response = list(registry_dir)?;
    if response.total_count != 0 {
        return Err(format!(
            "AviUtl2 が {} 件稼働しています。すべて終了してからやり直してください。",
            response.total_count
        ));
    }

    let leftovers = descriptor_files(registry_dir)?;
    if !leftovers.is_empty() {
        return Err(format!(
            "終了済みインスタンスの descriptor が {} 件残っています。",
            leftovers.len()
        ));
    }

    println!("registry ディレクトリが空であることを確認しました。");
    Ok(())
}

/// 3 プロセスを起動し、異なる `instance_id` で区別できることを確認する。
fn launch_three(registry_dir: &Path) -> Result<Vec<InstanceInfo>, String> {
    prompt(&format!(
        "AviUtl2 を {EXPECTED_INSTANCES} プロセス起動し、それぞれで別のプロジェクトを開いて\n\
         plugin が ready になったら Enter を押してください。"
    ));

    let response = wait_for_instances(registry_dir, EXPECTED_INSTANCES)?;
    let instances = response.instances.clone();

    verify_distinct(&instances)?;
    verify_written_by_other_processes(&instances)?;
    verify_started_at_parsable(&instances)?;
    verify_descriptor_files_exist(registry_dir, &instances)?;
    verify_no_secret_leak(registry_dir, &response)?;

    print_instances(&instances);
    if !confirm("上記の PID / 起動時刻 / プロジェクトが起動した 3 プロセスと対応していますか。")
    {
        return Err("一覧の内容が起動したプロセスと対応していません。".to_string());
    }

    Ok(instances)
}

/// 1 プロセスを終了し、その descriptor を残したままでも一覧に返らないことを確認する。
///
/// 終了したプロセスの descriptor を退避して書き戻すことで、異常終了により登録が
/// 残置された状態を再現する。
fn close_one(
    registry_dir: &Path,
    instances: &[InstanceInfo],
) -> Result<(InstanceInfo, Vec<InstanceInfo>), String> {
    let closed = instances
        .first()
        .ok_or_else(|| "終了対象のインスタンスがありません。".to_string())?
        .clone();
    let remaining: Vec<InstanceInfo> = instances[1..].to_vec();

    let path = descriptor_path(registry_dir, &closed);
    let saved = std::fs::read(&path)
        .map_err(|e| format!("descriptor {} を退避できません: {e}", path.display()))?;

    prompt(&format!(
        "PID {} の AviUtl2 のみを終了し、Enter を押してください。",
        closed.pid
    ));

    if path.exists() {
        println!("終了したインスタンスの descriptor は残置されています。");
    } else {
        println!("終了したインスタンスの descriptor は終了処理で削除されました。");
    }

    // 異常終了の再現として、終了したプロセスが書いた descriptor をそのまま復元する。
    std::fs::write(&path, &saved)
        .map_err(|e| format!("descriptor {} を復元できません: {e}", path.display()))?;

    let response = list(registry_dir)?;
    let listed = response.instances.clone();

    if listed.len() != remaining.len() {
        return Err(format!(
            "終了後は {} 件を期待しましたが {} 件返りました。",
            remaining.len(),
            listed.len()
        ));
    }
    if listed
        .iter()
        .any(|info| info.instance_id == closed.instance_id)
    {
        return Err(format!(
            "終了したインスタンス {} が一覧に返りました。",
            closed.instance_id
        ));
    }
    for kept in &remaining {
        if !listed
            .iter()
            .any(|info| info.instance_id == kept.instance_id)
        {
            return Err(format!(
                "稼働中のインスタンス {} が一覧から欠落しました。",
                kept.instance_id
            ));
        }
    }
    verify_no_secret_leak(registry_dir, &response)?;

    if path.exists() {
        println!("注意: 残置 descriptor は一覧から除外されましたが削除されていません。");
    } else {
        println!("残置 descriptor は一覧から除外され、削除されました。");
    }

    print_instances(&listed);
    Ok((closed, remaining))
}

/// 終了したプロセスを再起動し、新しい `instance_id` で現れることを確認する。
fn relaunch_closed(
    registry_dir: &Path,
    closed: &InstanceInfo,
    remaining: &[InstanceInfo],
) -> Result<(), String> {
    prompt("終了した AviUtl2 を再度起動し、plugin が ready になったら Enter を押してください。");

    let response = wait_for_instances(registry_dir, EXPECTED_INSTANCES)?;
    let instances = response.instances.clone();

    verify_distinct(&instances)?;
    verify_no_secret_leak(registry_dir, &response)?;

    if instances
        .iter()
        .any(|info| info.instance_id == closed.instance_id)
    {
        return Err(format!(
            "再起動後に旧 instance_id {} が返りました。",
            closed.instance_id
        ));
    }
    for kept in remaining {
        if !instances
            .iter()
            .any(|info| info.instance_id == kept.instance_id)
        {
            return Err(format!(
                "稼働し続けているインスタンス {} が一覧から欠落しました。",
                kept.instance_id
            ));
        }
    }

    print_instances(&instances);
    Ok(())
}

/// `instance_id` と PID が互いに異なることを確認する。
fn verify_distinct(instances: &[InstanceInfo]) -> Result<(), String> {
    let ids: HashSet<_> = instances.iter().map(|info| info.instance_id).collect();
    if ids.len() != instances.len() {
        return Err(format!(
            "{} 件中 {} 種類の instance_id しかありません。",
            instances.len(),
            ids.len()
        ));
    }
    let pids: HashSet<_> = instances.iter().map(|info| info.pid).collect();
    if pids.len() != instances.len() {
        return Err(format!(
            "{} 件中 {} 種類の PID しかありません。",
            instances.len(),
            pids.len()
        ));
    }
    Ok(())
}

/// descriptor が本プロセス以外（実機の AviUtl2）によって書かれたことを確認する。
fn verify_written_by_other_processes(instances: &[InstanceInfo]) -> Result<(), String> {
    let own_pid = std::process::id();
    for info in instances {
        if info.pid == own_pid {
            return Err(format!(
                "instance {} の PID が本プロセスと同一です。実機の descriptor ではありません。",
                info.instance_id
            ));
        }
    }
    Ok(())
}

/// `started_at` が RFC3339 として解釈できることを確認する。
fn verify_started_at_parsable(instances: &[InstanceInfo]) -> Result<(), String> {
    for info in instances {
        chrono::DateTime::parse_from_rfc3339(&info.started_at).map_err(|e| {
            format!(
                "instance {} の started_at を解釈できません: {e}",
                info.instance_id
            )
        })?;
    }
    Ok(())
}

/// 一覧された各インスタンスに対応する descriptor ファイルが存在することを確認する。
fn verify_descriptor_files_exist(
    registry_dir: &Path,
    instances: &[InstanceInfo],
) -> Result<(), String> {
    for info in instances {
        let path = descriptor_path(registry_dir, info);
        if !path.exists() {
            return Err(format!(
                "instance {} の descriptor {} が存在しません。",
                info.instance_id,
                path.display()
            ));
        }
    }
    Ok(())
}

/// 応答に descriptor 由来の秘密情報が含まれないことを確認する。
///
/// 実機が生成した `auth_secret` と `pipe_name` の実値を descriptor から読み出し、
/// 応答 JSON に現れないことを照合する。
fn verify_no_secret_leak(
    registry_dir: &Path,
    response: &ListInstancesResponse,
) -> Result<(), String> {
    let json =
        serde_json::to_string(response).map_err(|e| format!("応答を直列化できません: {e}"))?;

    for field in ["auth_secret", "pipe_name", "hwnd"] {
        if json.contains(field) {
            return Err(format!("応答に {field} フィールドが含まれています。"));
        }
    }

    for path in descriptor_files(registry_dir)? {
        let content = match std::fs::read(&path) {
            Ok(content) => content,
            // 検証中に稼働インスタンスが descriptor を差し替えることがあるため、
            // 読めなかったものは照合対象から外す。
            Err(_) => continue,
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&content) else {
            continue;
        };
        for field in ["auth_secret", "pipe_name"] {
            let Some(secret) = value.get(field).and_then(|v| v.as_str()) else {
                continue;
            };
            if json.contains(secret) {
                return Err(format!(
                    "応答に descriptor の {field} の値が含まれています。"
                ));
            }
        }
    }

    Ok(())
}

/// 一覧が期待件数になるまで待つ。
fn wait_for_instances(
    registry_dir: &Path,
    expected: usize,
) -> Result<ListInstancesResponse, String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut response = list(registry_dir)?;
    while response.instances.len() != expected && Instant::now() < deadline {
        std::thread::sleep(POLL_INTERVAL);
        response = list(registry_dir)?;
    }
    if response.instances.len() != expected {
        return Err(format!(
            "{expected} 件を期待しましたが {} 件でした。",
            response.instances.len()
        ));
    }
    Ok(response)
}

/// `aviutl2_list_instances` を実行する。
fn list(registry_dir: &Path) -> Result<ListInstancesResponse, String> {
    aviutl2_list_instances(
        registry_dir,
        ListInstancesRequest {
            offset: 0,
            limit: LIST_LIMIT,
        },
    )
    .map_err(|e| format!("aviutl2_list_instances に失敗しました: {e}"))
}

/// registry ディレクトリ内の descriptor ファイル一覧を返す。
fn descriptor_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("registry ディレクトリを列挙できません: {e}")),
    };

    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("registry ディレクトリの列挙に失敗しました: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// インスタンスに対応する descriptor ファイルのパスを返す。
fn descriptor_path(registry_dir: &Path, info: &InstanceInfo) -> PathBuf {
    registry_dir.join(format!("{}.json", info.instance_id))
}

/// registry ディレクトリを決定する。
fn registry_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os(REGISTRY_DIR_ENV) {
        return Ok(PathBuf::from(dir));
    }
    default_registry_dir().ok_or_else(|| {
        format!("registry ディレクトリを決定できません。{REGISTRY_DIR_ENV} を設定してください。")
    })
}

/// 一覧結果を実行者向けに表示する。
fn print_instances(instances: &[InstanceInfo]) {
    println!();
    println!("列挙結果 {} 件:", instances.len());
    for info in instances {
        let project = match &info.project {
            Some(project) => format!("{} ({})", project.display_name, project.path),
            None => "（プロジェクトなし）".to_string(),
        };
        println!(
            "  instance_id={} state={:?} pid={} started_at={} project={}",
            info.instance_id, info.state, info.pid, info.started_at, project
        );
    }
}

/// 実行者へ操作を指示し、Enter を待つ。
fn prompt(message: &str) {
    println!();
    println!("{message}");
    print!("> ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

/// 実行者へ確認を求める。
fn confirm(message: &str) -> bool {
    println!();
    println!("{message}");
    print!("[y/N] > ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y")
}
