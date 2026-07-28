//! instance discovery pipeline と stale cleanup。
//!
//! registry ディレクトリを列挙し、descriptor 検証 → PID 作成時刻 → pipe 接続 →
//! handshake → ping の順で生存確認を行う。

use crate::identity::{ProcessLookup, lookup_process};
use crate::pipe_client::{PipeClient, PipeClientError};
use aviutl2_mcp_core::{
    InstanceDescriptor, InstanceId, InstanceInfo, InstanceProject, InstanceState, ProtocolVersion,
    deserialize_json, pipe_name_for,
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

/// discovery 全体を失敗させるエラー。
///
/// 個々の候補の失敗は [`DiscoveryResult::Excluded`] として一覧から除外するにとどめ、
/// ここには含めない。
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// registry ディレクトリ自体を列挙できなかった。
    #[error("registry ディレクトリを列挙できませんでした: {0}")]
    RegistryUnreadable(#[source] std::io::Error),
}

impl DiscoveryError {
    /// 原因となった I/O エラーの種別を返す。
    pub fn io_error_kind(&self) -> std::io::ErrorKind {
        match self {
            DiscoveryError::RegistryUnreadable(e) => e.kind(),
        }
    }
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

/// 除外した候補の descriptor ファイルをどう扱うか。
///
/// 除外は「一覧に出さない」だけの可逆な措置であるのに対し、削除は不可逆であり、
/// 他プロセスが所有するファイルを消す。両者を型で分離し、既定を除外側に置く。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CleanupEligibility {
    /// 一覧から除外するのみで、descriptor ファイルは残す。
    ExcludeOnly,
    /// descriptor が指すインスタンスの不在を示せるため、再検証のうえ削除してよい。
    RemovalAllowed,
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

    /// この除外理由が descriptor ファイルの削除まで許すかを返す。
    ///
    /// 削除を許すのは「descriptor が指すインスタンスがもう存在しない」ことを
    /// 積極的に示せる理由に限る。判断できない理由は除外にとどめる。
    pub(crate) fn cleanup_eligibility(&self) -> CleanupEligibility {
        match self {
            // descriptor を解釈できていないため PID すら取り出せず、対応する
            // インスタンスの不在を示せない。将来 schema の descriptor（稼働中の
            // 新版インスタンスが書いたもの）や、一時的な read 失敗もここに落ちる。
            ExclusionReason::InvalidDescriptor => CleanupEligibility::ExcludeOnly,
            // 互換しないプロトコルで稼働中のインスタンスがあり得る。
            ExclusionReason::ProtocolMismatch => CleanupEligibility::ExcludeOnly,
            // panic により何も判定できていない。
            ExclusionReason::InternalError => CleanupEligibility::ExcludeOnly,
            // descriptor は解釈できているため、削除直前の再検証でプロセスの
            // 不在を確定できる。稼働中や判定不能であればそこで削除を取りやめる。
            ExclusionReason::ProcessIdentityMismatch => CleanupEligibility::RemovalAllowed,
            ExclusionReason::PipeUnreachable
            | ExclusionReason::AuthenticationFailed
            | ExclusionReason::PingFailed => CleanupEligibility::RemovalAllowed,
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
/// registry ディレクトリを読み取れなかった場合も 0 件として畳み込むため、
/// 「読み取れなかった」と「本当に 0 件」を区別できない。
///
/// 既存の呼び出し元との互換のために残している暫定 API であり、新しい呼び出し元を
/// 作ってはならない。[`try_find_instances`] を使うこと。
pub fn find_instances(
    registry_dir: &Path,
    config: DiscoveryConfig,
    cleanup: bool,
) -> Vec<InstanceInfo> {
    try_find_instances(registry_dir, config, cleanup).unwrap_or_default()
}

/// registry ディレクトリ内の生存インスタンスを発見する。
///
/// 1 候補の失敗は全体を失敗させず、他候補の検証を継続する。
/// registry ディレクトリ自体を列挙できない場合のみ [`DiscoveryError`] を返す。
/// `cleanup` が true の場合、安全条件を満たす stale descriptor を削除する。
#[instrument(skip(config), fields(registry_dir = %registry_dir.display()))]
pub fn try_find_instances(
    registry_dir: &Path,
    config: DiscoveryConfig,
    cleanup: bool,
) -> Result<Vec<InstanceInfo>, DiscoveryError> {
    let files = list_descriptor_files(registry_dir).map_err(|e| {
        warn!(error = %e, "registry ディレクトリの列挙に失敗しました");
        DiscoveryError::RegistryUnreadable(e)
    })?;

    let mut results = Vec::new();
    for path in files {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            discover_candidate(&path, config)
        }));

        // 除外は panic 経路も含めて 1 箇所へ集約し、cleanup 判定を迂回させない。
        let (instance_id, reason) = match result {
            Ok(DiscoveryResult::Alive(info)) => {
                debug!(instance_id = %info.instance_id, "instance is alive");
                results.push(info);
                continue;
            }
            Ok(DiscoveryResult::Excluded {
                instance_id,
                reason,
            }) => (instance_id.map(|id| id.to_string()), reason),
            Err(_) => {
                warn!("candidate discovery panicked; isolating");
                let id_short = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string());
                (id_short, ExclusionReason::InternalError)
            }
        };

        warn!(instance_id = ?instance_id, reason = reason.as_code(), "instance excluded");
        if cleanup
            && should_attempt_cleanup(reason)
            && let Err(e) = try_cleanup_stale_descriptor(&path)
        {
            warn!(error = %e, "stale descriptor cleanup failed");
        }
    }

    Ok(results)
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
    let process_identity = match lookup_process(descriptor.pid) {
        ProcessLookup::Found(identity) => identity,
        // 生存を確認できなければ一覧には出さない。不在か判定不能かは
        // descriptor を削除してよいかの判断にのみ影響する。
        ProcessLookup::Absent | ProcessLookup::Undetermined => {
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
///
/// JSON は不正 UTF-8・重複 key・非有限数を拒否する strict 規則で解釈する。
/// IPC の両端が同一の拒否規則を共有し、片側の検証漏れが他方の防御で塞がれるようにする。
fn validate_descriptor_file(path: &Path) -> Result<InstanceDescriptor, ExclusionReason> {
    let content = std::fs::read(path).map_err(|_| ExclusionReason::InvalidDescriptor)?;
    let descriptor: InstanceDescriptor =
        deserialize_json(&content).map_err(|_| ExclusionReason::InvalidDescriptor)?;

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
///
/// ディレクトリが存在しないことは「インスタンスが 1 件も登録されていない」を意味するため
/// エラーではなく空の一覧を返す。権限拒否や I/O エラーのように、読めるはずのものを
/// 読めなかった場合のみエラーとする。
fn list_descriptor_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut files = Vec::new();
    for entry in entries {
        // 列挙途中の失敗はディレクトリ自体を読み切れていないことを意味するため、
        // 候補単位の除外ではなく全体エラーとして伝播させる。
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
        reason.cleanup_eligibility(),
        CleanupEligibility::RemovalAllowed
    )
}

/// stale descriptor を安全に削除する（best-effort）。
///
/// 削除直前に再読み込み・再検証し、対応するインスタンスの不在を確認できた場合のみ削除する。
fn try_cleanup_stale_descriptor(path: &Path) -> std::io::Result<()> {
    if !instance_proven_absent(path) {
        debug!(path = %path.display(), "descriptor revalidated; skipped cleanup");
        return Ok(());
    }

    std::fs::remove_file(path)?;
    debug!(path = %path.display(), "stale descriptor removed");
    Ok(())
}

/// descriptor が指すインスタンスがもう存在しないことを積極的に示せるか。
///
/// 再読み込みや再検証に失敗した場合は判断できないものとして `false` を返し、
/// 削除ではなく除外にとどめる。
fn instance_proven_absent(path: &Path) -> bool {
    let Ok(descriptor) = validate_descriptor_file(path) else {
        return false;
    };
    absence_confirmed(
        lookup_process(descriptor.pid),
        &descriptor.process_created_at,
    )
}

/// プロセス照会結果から、descriptor のインスタンスの不在を確定できるか判定する。
fn absence_confirmed(lookup: ProcessLookup, descriptor_created_at: &str) -> bool {
    match lookup {
        // PID に対応するプロセスが存在しないことを確定できた。
        ProcessLookup::Absent => true,
        // 存在の有無を判定できていないため、不在の根拠にはならない。
        ProcessLookup::Undetermined => false,
        // プロセスは存在するが、作成時刻が一致しなければ PID 再利用であり
        // descriptor のインスタンスではない。一致するなら稼働中の可能性がある。
        ProcessLookup::Found(identity) => {
            !process_created_at_matches(descriptor_created_at, identity.created_at)
        }
    }
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

    /// 自プロセスの識別情報。
    fn self_identity() -> crate::identity::ProcessIdentity {
        match lookup_process(std::process::id()) {
            ProcessLookup::Found(identity) => identity,
            other => panic!("自身の PID は照会できる: {other:?}"),
        }
    }

    fn sample_descriptor(id: InstanceId) -> InstanceDescriptor {
        let identity = self_identity();
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
        let identity = self_identity();
        assert!(process_created_at_matches(
            &identity.created_at.to_rfc3339(),
            identity.created_at
        ));
    }

    #[test]
    fn process_created_at_mismatch_outside_tolerance() {
        let identity = self_identity();
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

        try_find_instances(&dir, DiscoveryConfig::default(), true).unwrap();
        assert!(!path.exists(), "stale descriptor should be cleaned up");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_registry_dir_is_zero_instances() {
        let dir = temp_registry_dir();
        assert!(!dir.exists());

        let instances = try_find_instances(&dir, DiscoveryConfig::default(), true).unwrap();
        assert!(
            instances.is_empty(),
            "ディレクトリ不在はインスタンス 0 件として扱う"
        );
    }

    #[test]
    fn unreadable_registry_dir_is_whole_error() {
        // ディレクトリとして開けない対象を registry として渡し、列挙自体の失敗を再現する。
        let path = std::env::temp_dir().join(format!(
            "aviutl2-mcp-discovery-not-a-dir-{}",
            InstanceId::new_v4()
        ));
        std::fs::write(&path, b"not a directory").unwrap();

        let result = try_find_instances(&path, DiscoveryConfig::default(), true);
        assert!(
            matches!(result, Err(DiscoveryError::RegistryUnreadable(_))),
            "ディレクトリを列挙できない場合は全体エラーになる"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn invalid_utf8_descriptor_excluded_and_kept() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        let path = dir.join(format!("{}.json", id));
        // JSON として妥当な形だが UTF-8 として不正なバイトを含む。
        let mut bytes = br#"{"schema_version":1,"instance_id":""#.to_vec();
        bytes.extend_from_slice(&[0x80, 0x81, 0x82]);
        bytes.extend_from_slice(br#""}"#);
        std::fs::write(&path, &bytes).unwrap();

        let result = discover_candidate(&path, DiscoveryConfig::default());
        assert!(
            matches!(
                result,
                DiscoveryResult::Excluded {
                    reason: ExclusionReason::InvalidDescriptor,
                    ..
                }
            ),
            "不正 UTF-8 の descriptor は除外される"
        );

        try_find_instances(&dir, DiscoveryConfig::default(), true).unwrap();
        assert!(path.exists(), "不正 UTF-8 の descriptor は削除されない");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicate_json_key_descriptor_excluded() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        let descriptor = sample_descriptor(id);
        let serialized = serde_json::to_string(&descriptor).unwrap();
        // 末尾の `}` の直前に既出の key を追加して重複させる。
        let duplicated = format!("{},\"pid\":1{}", &serialized[..serialized.len() - 1], "}");

        let path = dir.join(format!("{}.json", id));
        std::fs::write(&path, duplicated).unwrap();

        let result = discover_candidate(&path, DiscoveryConfig::default());
        assert!(
            matches!(
                result,
                DiscoveryResult::Excluded {
                    reason: ExclusionReason::InvalidDescriptor,
                    ..
                }
            ),
            "重複 JSON key を含む descriptor は除外される"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_eligibility_excludes_undecidable_reasons() {
        for reason in [
            ExclusionReason::InvalidDescriptor,
            ExclusionReason::ProtocolMismatch,
            ExclusionReason::InternalError,
        ] {
            assert_eq!(
                reason.cleanup_eligibility(),
                CleanupEligibility::ExcludeOnly,
                "生存を判断できない理由では削除しない: {}",
                reason.as_code()
            );
            assert!(!should_attempt_cleanup(reason));
        }

        for reason in [
            ExclusionReason::ProcessIdentityMismatch,
            ExclusionReason::PipeUnreachable,
            ExclusionReason::AuthenticationFailed,
            ExclusionReason::PingFailed,
        ] {
            assert_eq!(
                reason.cleanup_eligibility(),
                CleanupEligibility::RemovalAllowed,
                "不在を示せる理由では削除を許す: {}",
                reason.as_code()
            );
            assert!(should_attempt_cleanup(reason));
        }
    }

    #[test]
    fn unparsable_descriptor_is_not_removed() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        let path = dir.join(format!("{}.json", id));
        std::fs::write(&path, b"{ broken").unwrap();

        try_find_instances(&dir, DiscoveryConfig::default(), true).unwrap();
        assert!(
            path.exists(),
            "パース不能な descriptor は削除せず除外にとどめる"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn future_schema_version_descriptor_is_not_removed() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        let mut descriptor = sample_descriptor(id);
        descriptor.schema_version = 2;

        let path = dir.join(format!("{}.json", id));
        std::fs::write(&path, serde_json::to_string(&descriptor).unwrap()).unwrap();

        let instances = try_find_instances(&dir, DiscoveryConfig::default(), true).unwrap();
        assert!(
            instances.is_empty(),
            "未知 schema の descriptor は除外される"
        );
        assert!(
            path.exists(),
            "未知 schema の descriptor は稼働中インスタンスのものであり得るため削除しない"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn protocol_mismatch_descriptor_is_not_removed() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        let mut descriptor = sample_descriptor(id);
        descriptor.protocol_version = ProtocolVersion {
            major: ProtocolVersion::CURRENT.major + 1,
            minor: 0,
        };

        let path = dir.join(format!("{}.json", id));
        std::fs::write(&path, serde_json::to_string(&descriptor).unwrap()).unwrap();

        let instances = try_find_instances(&dir, DiscoveryConfig::default(), true).unwrap();
        assert!(instances.is_empty(), "MAJOR 不一致の候補は除外される");
        assert!(
            path.exists(),
            "MAJOR 不一致は別版で稼働中のインスタンスであり得るため削除しない"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_but_unreachable_instance_is_excluded_without_removal() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        // 自 PID を指す descriptor。pipe は待ち受けていないため接続に失敗する。
        let descriptor = sample_descriptor(id);
        let path = dir.join(format!("{}.json", id));
        std::fs::write(&path, serde_json::to_string(&descriptor).unwrap()).unwrap();

        let instances = try_find_instances(&dir, DiscoveryConfig::default(), true).unwrap();
        assert!(
            instances.is_empty(),
            "pipe に接続できない候補は一覧に含まれない"
        );
        assert!(
            path.exists(),
            "プロセスが稼働中である限り descriptor は削除されない"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undetermined_process_lookup_is_not_absence() {
        let identity = self_identity();
        let created_at = identity.created_at.to_rfc3339();

        assert!(
            !absence_confirmed(ProcessLookup::Undetermined, &created_at),
            "存在を判定できない場合は不在と扱わない"
        );
        assert!(absence_confirmed(ProcessLookup::Absent, &created_at));
        assert!(
            !absence_confirmed(ProcessLookup::Found(identity), &created_at),
            "作成時刻が一致するプロセスは稼働中とみなす"
        );

        let reused = crate::identity::ProcessIdentity {
            created_at: identity.created_at + chrono::Duration::seconds(10),
        };
        assert!(
            absence_confirmed(ProcessLookup::Found(reused), &created_at),
            "作成時刻が一致しないプロセスは PID 再利用であり不在を確定できる"
        );
    }

    #[test]
    fn cleanup_skips_descriptor_that_cannot_be_revalidated() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.json", InstanceId::new_v4()));
        std::fs::write(&path, b"{ broken").unwrap();

        assert!(!instance_proven_absent(&path));
        try_cleanup_stale_descriptor(&path).unwrap();
        assert!(path.exists(), "再検証できない descriptor は削除されない");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
