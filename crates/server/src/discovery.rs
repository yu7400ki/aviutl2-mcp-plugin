//! instance discovery pipeline と stale cleanup。
//!
//! registry ディレクトリを列挙し、descriptor 検証 → PID 作成時刻 → pipe 接続 →
//! handshake → ping の順で生存確認を行う。

use crate::identity::get_process_identity;
use crate::pipe_client::{PipeClient, PipeClientError};
use aviutl2_mcp_core::{
    InstanceDescriptor, InstanceId, InstanceInfo, InstanceProject, InstanceState, ProtocolVersion,
    pipe_name_for,
};

#[cfg(test)]
use aviutl2_mcp_core::DescriptorProject;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{debug, instrument, warn};

/// 1 候補の discovery 結果。
#[derive(Debug, Clone)]
pub enum DiscoveryResult {
    /// 生存確認が成功した。
    Alive(InstanceInfo),
    /// 一覧から除外する（stale または無効）。
    Excluded {
        instance_id: Option<InstanceId>,
        reason: ExclusionReason,
    },
}

/// 除外理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionReason {
    /// descriptor の検証に失敗した。
    InvalidDescriptor,
    /// プロトコルバージョンが互換ではない。
    ProtocolMismatch,
    /// PID / プロセス作成時刻が一致しない。
    ProcessIdentityMismatch,
    /// pipe 接続不能または切断。
    PipeUnreachable,
    /// handshake 失敗。
    AuthenticationFailed,
    /// ping 失敗または状態が draining/gone。
    PingFailed,
    /// 予期しない内部エラー（panic 捕捉）。
    InternalError,
}

impl ExclusionReason {
    /// 安全な理由コード文字列を返す。
    pub fn as_code(&self) -> &'static str {
        match self {
            ExclusionReason::InvalidDescriptor => "invalid_descriptor",
            ExclusionReason::ProtocolMismatch => "protocol_mismatch",
            ExclusionReason::ProcessIdentityMismatch => "process_identity_mismatch",
            ExclusionReason::PipeUnreachable => "pipe_unreachable",
            ExclusionReason::AuthenticationFailed => "authentication_failed",
            ExclusionReason::PingFailed => "ping_failed",
            ExclusionReason::InternalError => "internal_error",
        }
    }
}

/// discovery の設定。
#[derive(Debug, Clone, Copy)]
pub struct DiscoveryConfig {
    /// 1 候補あたりの discovery 期限。
    pub per_candidate_deadline: Duration,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            per_candidate_deadline: Duration::from_secs(5),
        }
    }
}

/// registry ディレクトリを既定の場所から決定する。
pub fn default_registry_dir() -> Option<PathBuf> {
    let local_app_data = std::env::var("LOCALAPPDATA").ok()?;
    Some(
        PathBuf::from(local_app_data)
            .join("AviUtl2Mcp")
            .join("instances"),
    )
}

/// registry ディレクトリ内の生存インスタンスを発見する。
///
/// 1 候補の失敗は全体を失敗させず、他候補の検証を継続する。
/// `cleanup` が true の場合、安全条件を満たす stale descriptor を削除する。
#[instrument(skip(config), fields(registry_dir = %registry_dir.display()))]
pub fn find_instances(
    registry_dir: &Path,
    config: DiscoveryConfig,
    cleanup: bool,
) -> Vec<InstanceInfo> {
    let files = match list_descriptor_files(registry_dir) {
        Ok(files) => files,
        Err(e) => {
            warn!(error = %e, "registry ディレクトリの列挙に失敗しました");
            return Vec::new();
        }
    };

    let mut results = Vec::new();
    for path in files {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            discover_candidate(&path, config)
        }));

        match result {
            Ok(DiscoveryResult::Alive(info)) => {
                debug!(instance_id = %info.instance_id, "instance is alive");
                results.push(info);
            }
            Ok(DiscoveryResult::Excluded {
                instance_id,
                reason,
            }) => {
                let id_short = instance_id.map(|id| id.to_string());
                warn!(instance_id = ?id_short, reason = reason.as_code(), "instance excluded");
                if cleanup
                    && should_attempt_cleanup(reason)
                    && let Err(e) = try_cleanup_stale_descriptor(&path)
                {
                    warn!(error = %e, "stale descriptor cleanup failed");
                }
            }
            Err(_) => {
                warn!("candidate discovery panicked; isolating");
                let id_short = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string());
                warn!(instance_id = ?id_short, reason = ExclusionReason::InternalError.as_code(), "instance excluded");
                if cleanup {
                    let _ = try_cleanup_stale_descriptor(&path);
                }
            }
        }
    }

    results
}

/// 1 候補を discovery pipeline で検証する。
#[instrument(skip(path, config), fields(path = %path.display()))]
fn discover_candidate(path: &Path, config: DiscoveryConfig) -> DiscoveryResult {
    let deadline = Instant::now() + config.per_candidate_deadline;

    // descriptor 検証。
    let descriptor = match validate_descriptor_file(path) {
        Ok(d) => d,
        Err(reason) => {
            return DiscoveryResult::Excluded {
                instance_id: None,
                reason,
            };
        }
    };

    // PID とプロセス作成時刻。
    let process_identity = match get_process_identity(descriptor.pid) {
        Some(id) => id,
        None => {
            return DiscoveryResult::Excluded {
                instance_id: Some(descriptor.instance_id),
                reason: ExclusionReason::ProcessIdentityMismatch,
            };
        }
    };
    if !process_created_at_matches(&descriptor.process_created_at, process_identity.created_at) {
        return DiscoveryResult::Excluded {
            instance_id: Some(descriptor.instance_id),
            reason: ExclusionReason::ProcessIdentityMismatch,
        };
    }

    // pipe 接続、handshake、ping。
    let state = match run_pipe_handshake_and_ping(&descriptor, deadline) {
        Ok(state) => state,
        Err(reason) => {
            return DiscoveryResult::Excluded {
                instance_id: Some(descriptor.instance_id),
                reason,
            };
        }
    };

    if matches!(state, InstanceState::Draining | InstanceState::Gone) {
        return DiscoveryResult::Excluded {
            instance_id: Some(descriptor.instance_id),
            reason: ExclusionReason::PingFailed,
        };
    }

    // InstanceInfo 生成。
    DiscoveryResult::Alive(build_instance_info(descriptor, state))
}

/// descriptor ファイルを検証する。
fn validate_descriptor_file(path: &Path) -> Result<InstanceDescriptor, ExclusionReason> {
    let content = std::fs::read_to_string(path).map_err(|_| ExclusionReason::InvalidDescriptor)?;
    let descriptor: InstanceDescriptor =
        serde_json::from_str(&content).map_err(|_| ExclusionReason::InvalidDescriptor)?;

    if descriptor.schema_version != 1 {
        return Err(ExclusionReason::InvalidDescriptor);
    }

    let file_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or(ExclusionReason::InvalidDescriptor)?;
    if file_stem != descriptor.instance_id.to_string() {
        return Err(ExclusionReason::InvalidDescriptor);
    }

    if descriptor.protocol_version.major != ProtocolVersion::CURRENT.major {
        return Err(ExclusionReason::ProtocolMismatch);
    }

    let expected_pipe_name = pipe_name_for(&descriptor.instance_id);
    if descriptor.pipe_name != expected_pipe_name {
        return Err(ExclusionReason::InvalidDescriptor);
    }

    Ok(descriptor)
}

/// pipe 接続、handshake、ping を実行する。
fn run_pipe_handshake_and_ping(
    descriptor: &InstanceDescriptor,
    deadline: Instant,
) -> Result<InstanceState, ExclusionReason> {
    let client = PipeClient::connect_and_handshake(
        descriptor.instance_id,
        descriptor.pid,
        &descriptor.process_created_at,
        &descriptor.auth_secret,
        deadline,
    )
    .map_err(map_pipe_error)?;

    client.ping(deadline).map_err(map_pipe_error)
}

/// `PipeClientError` を `ExclusionReason` へ対応付ける。
fn map_pipe_error(err: PipeClientError) -> ExclusionReason {
    match err {
        PipeClientError::ConnectFailed | PipeClientError::Timeout | PipeClientError::Io(_) => {
            ExclusionReason::PipeUnreachable
        }
        PipeClientError::Framing | PipeClientError::Json | PipeClientError::InvalidResponse => {
            ExclusionReason::PingFailed
        }
        PipeClientError::AuthenticationFailed => ExclusionReason::AuthenticationFailed,
        PipeClientError::ProtocolMismatch => ExclusionReason::ProtocolMismatch,
        PipeClientError::InstanceStale => ExclusionReason::ProcessIdentityMismatch,
    }
}

/// プロセス作成時刻が descriptor 記載値と一致するか判定する（1 秒の許容）。
fn process_created_at_matches(descriptor_value: &str, actual: DateTime<Utc>) -> bool {
    let parsed = match DateTime::parse_from_rfc3339(descriptor_value) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(_) => return false,
    };
    let diff = (actual - parsed).num_seconds().abs();
    diff <= 1
}

/// `InstanceDescriptor` と ping 応答から `InstanceInfo` を生成する。
fn build_instance_info(descriptor: InstanceDescriptor, state: InstanceState) -> InstanceInfo {
    InstanceInfo {
        instance_id: descriptor.instance_id,
        state,
        pid: descriptor.pid,
        started_at: descriptor.started_at,
        project: descriptor.project.map(|p| InstanceProject {
            display_name: p.display_name,
            path: p.path,
        }),
    }
}

/// descriptor ファイル一覧を取得する（ファイル名順）。
fn list_descriptor_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !dir.exists() {
        return Ok(files);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// この除外理由に対して cleanup を試みるべきか。
fn should_attempt_cleanup(reason: ExclusionReason) -> bool {
    matches!(
        reason,
        ExclusionReason::InvalidDescriptor
            | ExclusionReason::ProcessIdentityMismatch
            | ExclusionReason::PipeUnreachable
            | ExclusionReason::AuthenticationFailed
            | ExclusionReason::PingFailed
    )
}

/// stale descriptor を安全に削除する。
///
/// 削除直前に再読み込み・再検証し、依然として stale であれば削除する。
/// 削除失敗や判断に迷う場合は無視する。
fn try_cleanup_stale_descriptor(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    // 削除直前の再検証。
    let should_delete = match validate_descriptor_file(path) {
        Ok(descriptor) => {
            // プロセスが存在し、作成時刻も一致すれば稼働中の可能性がある。削除しない。
            if let Some(identity) = get_process_identity(descriptor.pid) {
                !process_created_at_matches(&descriptor.process_created_at, identity.created_at)
            } else {
                true
            }
        }
        Err(_) => true,
    };

    if should_delete {
        std::fs::remove_file(path)?;
        debug!(path = %path.display(), "stale descriptor removed");
    } else {
        debug!(path = %path.display(), "descriptor revalidated as alive; skipped cleanup");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::{AuthSecret, ProtocolVersion};

    fn temp_registry_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aviutl2-mcp-discovery-test-{}",
            InstanceId::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn sample_descriptor(id: InstanceId) -> InstanceDescriptor {
        let identity = get_process_identity(std::process::id()).unwrap();
        InstanceDescriptor {
            schema_version: 1,
            protocol_version: ProtocolVersion::CURRENT,
            instance_id: id,
            pipe_name: pipe_name_for(&id),
            auth_secret: AuthSecret::generate(),
            pid: std::process::id(),
            process_created_at: identity.created_at.to_rfc3339(),
            hwnd: None,
            started_at: identity.created_at.to_rfc3339(),
            state: InstanceState::Ready,
            project: Some(DescriptorProject {
                display_name: "Test".to_string(),
                path: r"C:\test.aup".to_string(),
            }),
        }
    }

    #[test]
    fn process_created_at_matches_self() {
        let identity = get_process_identity(std::process::id()).unwrap();
        assert!(process_created_at_matches(
            &identity.created_at.to_rfc3339(),
            identity.created_at
        ));
    }

    #[test]
    fn process_created_at_mismatch_outside_tolerance() {
        let identity = get_process_identity(std::process::id()).unwrap();
        let different = identity.created_at + chrono::Duration::seconds(10);
        assert!(!process_created_at_matches(
            &identity.created_at.to_rfc3339(),
            different
        ));
    }

    #[test]
    fn invalid_descriptor_excluded() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not-a-uuid.json");
        std::fs::write(&path, b"{}").unwrap();

        let result = discover_candidate(&path, DiscoveryConfig::default());
        assert!(
            matches!(
                result,
                DiscoveryResult::Excluded {
                    reason: ExclusionReason::InvalidDescriptor,
                    ..
                }
            ),
            "無効なファイルは除外される"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn descriptor_with_wrong_pipe_name_excluded() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        let mut descriptor = sample_descriptor(id);
        descriptor.pipe_name = r"\\.\pipe\wrong".to_string();

        let path = dir.join(format!("{}.json", id));
        std::fs::write(&path, serde_json::to_string(&descriptor).unwrap()).unwrap();

        let result = discover_candidate(&path, DiscoveryConfig::default());
        assert!(
            matches!(
                result,
                DiscoveryResult::Excluded {
                    reason: ExclusionReason::InvalidDescriptor,
                    ..
                }
            ),
            "pipe_name 不一致は除外される"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_preserves_descriptor_for_live_process() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        let descriptor = sample_descriptor(id);
        let path = dir.join(format!("{}.json", id));
        std::fs::write(&path, serde_json::to_string(&descriptor).unwrap()).unwrap();

        try_cleanup_stale_descriptor(&path).unwrap();
        assert!(path.exists(), "生存中の descriptor は削除されない");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_pid_excluded_and_cleaned() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        let mut descriptor = sample_descriptor(id);
        descriptor.pid = 0xFFFF_FFFF; // 存在しない PID

        let path = dir.join(format!("{}.json", id));
        std::fs::write(&path, serde_json::to_string(&descriptor).unwrap()).unwrap();

        let result = discover_candidate(&path, DiscoveryConfig::default());
        assert!(
            matches!(
                result,
                DiscoveryResult::Excluded {
                    reason: ExclusionReason::ProcessIdentityMismatch,
                    ..
                }
            ),
            "存在しない PID は除外される"
        );

        find_instances(&dir, DiscoveryConfig::default(), true);
        assert!(!path.exists(), "stale descriptor should be cleaned up");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
