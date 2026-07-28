//! instance discovery pipeline と stale cleanup。
//!
//! registry ディレクトリを列挙し、descriptor 検証 → PID 作成時刻 → pipe 接続 →
//! handshake → ping の順で生存確認を行う。

use crate::identity::{ProcessLookup, lookup_process};
use crate::pipe_client::{PipeClient, PipeClientError};
use crate::redact;
use aviutl2_mcp_core::{
    ErrorCode, ErrorObject, InstanceDescriptor, InstanceId, InstanceInfo, InstanceProject,
    InstanceState, PongResult, ProtocolVersion, SERVER_RESOLVE_BUDGET, deserialize_json,
    parse_utc_timestamp, pipe_name_for,
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

/// `instance_id` 単体でのインスタンス解決の失敗。
#[derive(Debug, thiserror::Error)]
pub enum ResolveInstanceError {
    /// 指定された `instance_id` の descriptor が登録されていない。
    #[error("指定された instance_id は登録されていません")]
    NotRegistered,
    /// descriptor は見つかったが、生存確認に失敗した。
    #[error("インスタンスの生存確認に失敗しました: {}", .0.as_code())]
    Excluded(ExclusionReason),
    /// インスタンスは応答したが、今は要求に応じられない。
    #[error("インスタンスが要求を拒否しました: {}", .0.code)]
    Rejected(Box<ErrorObject>),
}

impl ResolveInstanceError {
    /// 応答へ載せるエラーコードを返す。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            // 候補が 1 件も見つからない場合と、見つかったが検証に落ちた場合を
            // 区別する。前者は指定 ID の instance が存在しないことを意味する。
            ResolveInstanceError::NotRegistered => ErrorCode::InstanceNotFound,
            ResolveInstanceError::Excluded(reason) => reason.error_code(),
            ResolveInstanceError::Rejected(error) => error.code.clone(),
        }
    }

    /// インスタンスが返したエラー応答。保持していない場合は `None`。
    ///
    /// `retry_after_ms` のような補助情報を呼び出し側がそのまま使えるようにする。
    pub fn remote_error(&self) -> Option<&ErrorObject> {
        match self {
            ResolveInstanceError::Rejected(error) => Some(error),
            ResolveInstanceError::NotRegistered | ResolveInstanceError::Excluded(_) => None,
        }
    }

    /// 除外された候補を解決失敗へ変換する。
    ///
    /// `host_busy` を返したインスタンスは生きており、待てば使えるようになる。
    /// 一覧を取り直しても同じ ID が返るだけなので、この場合はエラー応答を
    /// そのまま伝えて待ち直しへ誘導する。それ以外の拒否は、そのインスタンスを
    /// 使えることを確認できていない点で生存確認の失敗と変わらない。
    fn from_excluded(excluded: ExcludedCandidate) -> Self {
        match excluded.rejection {
            Some(error) if error.code == ErrorCode::HostBusy => Self::Rejected(error),
            _ => Self::Excluded(excluded.reason),
        }
    }
}

/// 生存確認を通過し、接続を保持したインスタンス。
///
/// `client` は handshake と ping を通過した認証済み接続であり、drop すると
/// pipe が閉じる。以降の operation を送る呼び出し側が必要な間だけ保持する。
///
/// [`PipeClient`] は生の pipe ハンドルを持つため `!Send` かつ `!Sync` であり、
/// 非同期タスクの await をまたいで持ち越せない。解決から要求送信、drop までを
/// 単一のブロッキング実行へ閉じ込めるか、接続を所有する専任スレッドを立てて
/// チャネル越しに要求を渡すこと。
pub struct ResolvedInstance {
    /// 認証済みの pipe 接続。
    pub client: PipeClient,
    /// 生存確認済みのインスタンス情報。
    pub info: InstanceInfo,
}

impl ExclusionReason {
    /// 応答へ載せるエラーコードを返す。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            // panic 捕捉のみが server 自身の不具合であり、呼び出し側に取れる手が無い。
            ExclusionReason::InternalError => ErrorCode::InternalError,
            // 残りは descriptor が指す対象の状態を表す。読み取れない descriptor には
            // 書き換え途中の共有違反のような一過性の失敗も含まれるため、
            // 一覧の取り直しで解消し得る stale として扱う。
            ExclusionReason::InvalidDescriptor
            | ExclusionReason::ProcessIdentityMismatch
            | ExclusionReason::PipeUnreachable
            | ExclusionReason::PingFailed => ErrorCode::InstanceStale,
            ExclusionReason::ProtocolMismatch => ErrorCode::ProtocolMismatch,
            ExclusionReason::AuthenticationFailed => ErrorCode::AuthenticationFailed,
        }
    }

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
    ///
    /// pipe 接続・handshake・ping 往復をこの 1 つの期限で束ねる。接続先は
    /// 自身の各段の上限をこの予算の内側に収める。
    pub per_candidate_deadline: Duration,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            per_candidate_deadline: SERVER_RESOLVE_BUDGET,
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
/// registry ディレクトリ自体を列挙できない場合のみ [`DiscoveryError`] を返す。
/// `cleanup` が true の場合、安全条件を満たす stale descriptor を削除する。
#[instrument(skip_all)]
pub fn find_instances(
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
        let (instance, reason) = match result {
            Ok(DiscoveryResult::Alive(info)) => {
                debug!(instance = %redact::instance_id(&info.instance_id), "instance is alive");
                results.push(info);
                continue;
            }
            Ok(DiscoveryResult::Excluded {
                instance_id,
                reason,
            }) => (
                instance_id
                    .as_ref()
                    .map(redact::instance_id)
                    .unwrap_or_else(|| redact::descriptor_file(&path)),
                reason,
            ),
            Err(_) => {
                warn!("candidate discovery panicked; isolating");
                (
                    redact::descriptor_file(&path),
                    ExclusionReason::InternalError,
                )
            }
        };

        warn!(instance = %instance, reason = reason.as_code(), "instance excluded");
        if cleanup
            && should_attempt_cleanup(reason)
            && let Err(e) = try_cleanup_stale_descriptor(&path)
        {
            warn!(error = %e, "stale descriptor cleanup failed");
        }
    }

    Ok(results)
}

/// registry に登録されている `instance_id` を列挙する（生存確認なし）。
///
/// descriptor の検証とプロセス同一性の確認までを行い、pipe 接続・handshake・
/// ping は行わない。plugin の pipe は同時 1 接続しか受け付けないため、一覧の
/// たびに接続すると実行中の要求と競合し、双方を失敗させてしまう。
/// したがって返る ID のインスタンスが要求に応じられるとは限らず、生存確認は
/// 実際に要求を送る [`resolve_instance`] が行う。
///
/// 並びは descriptor のファイル名順で安定しており、ページ分割の基準にできる。
#[instrument(skip_all)]
pub fn list_registered_instances(registry_dir: &Path) -> Result<Vec<InstanceId>, DiscoveryError> {
    let files = list_descriptor_files(registry_dir).map_err(|e| {
        warn!(error = %e, "registry ディレクトリの列挙に失敗しました");
        DiscoveryError::RegistryUnreadable(e)
    })?;

    let mut instances = Vec::new();
    for path in files {
        let Ok(descriptor) = validate_descriptor_file(&path) else {
            debug!(instance = %redact::descriptor_file(&path), "descriptor を解釈できません");
            continue;
        };
        if !process_identity_matches(&descriptor) {
            debug!(
                instance = %redact::instance_id(&descriptor.instance_id),
                "descriptor のプロセスが存在しません",
            );
            continue;
        }
        instances.push(descriptor.instance_id);
    }
    Ok(instances)
}

/// descriptor が指すプロセスが今も同一であるかを、pipe に触れずに判定する。
fn process_identity_matches(descriptor: &InstanceDescriptor) -> bool {
    match lookup_process(descriptor.pid) {
        ProcessLookup::Found(identity) => {
            process_created_at_matches(&descriptor.process_created_at, identity.created_at)
        }
        // 生存を確認できなければ同一とは扱わない。不在か判定不能かは
        // descriptor を削除してよいかの判断にのみ影響する。
        ProcessLookup::Absent | ProcessLookup::Undetermined => false,
    }
}

/// 検証を通過した候補と、確立済みの接続。
struct VerifiedCandidate {
    info: InstanceInfo,
    client: PipeClient,
}

/// 検証に落ちた候補。
struct ExcludedCandidate {
    instance_id: Option<InstanceId>,
    reason: ExclusionReason,
    /// インスタンスが返したエラー応答。応答があった場合のみ入る。
    rejection: Option<Box<ErrorObject>>,
}

impl ExcludedCandidate {
    fn new(instance_id: Option<InstanceId>, reason: ExclusionReason) -> Self {
        Self {
            instance_id,
            reason,
            rejection: None,
        }
    }
}

/// `instance_id` を指定して 1 件のインスタンスを解決する。
///
/// レジストリ全体を列挙せず、`instance_id` に対応する descriptor だけを読む。
/// 検証は一覧取得と同一の pipeline を通し、成功時は認証済み接続を返す。
#[instrument(skip_all, fields(instance = %redact::instance_id(&instance_id)))]
pub fn resolve_instance(
    registry_dir: &Path,
    instance_id: InstanceId,
    config: DiscoveryConfig,
) -> Result<ResolvedInstance, ResolveInstanceError> {
    let path = descriptor_path(registry_dir, &instance_id);
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => {}
        // 通常ファイルでない、または存在しない場合はこの ID の登録が無い。
        Ok(_) => return Err(ResolveInstanceError::NotRegistered),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ResolveInstanceError::NotRegistered);
        }
        // 存在の有無を判定できないため、登録が無いとは断定しない。
        Err(_) => {
            return Err(ResolveInstanceError::Excluded(
                ExclusionReason::InvalidDescriptor,
            ));
        }
    }

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        verify_candidate(&path, config)
    }));
    match result {
        Ok(Ok(candidate)) => Ok(ResolvedInstance {
            client: candidate.client,
            info: candidate.info,
        }),
        Ok(Err(excluded)) => {
            warn!(reason = excluded.reason.as_code(), "instance excluded");
            Err(ResolveInstanceError::from_excluded(excluded))
        }
        Err(_) => {
            warn!("instance resolution panicked; isolating");
            Err(ResolveInstanceError::Excluded(
                ExclusionReason::InternalError,
            ))
        }
    }
}

/// registry ディレクトリ内で `instance_id` に対応する descriptor のパス。
///
/// descriptor は `{instance_id}.json` として登録されるため、ファイル名は
/// UUID の正準書式のみで構成される。
fn descriptor_path(registry_dir: &Path, instance_id: &InstanceId) -> PathBuf {
    registry_dir.join(format!("{instance_id}.json"))
}

/// 1 候補を discovery pipeline で検証する。
#[instrument(skip_all, fields(instance = %redact::descriptor_file(path)))]
fn discover_candidate(path: &Path, config: DiscoveryConfig) -> DiscoveryResult {
    // 一覧取得では以降の要求を送らないため、接続はここで閉じる。
    match verify_candidate(path, config) {
        Ok(candidate) => DiscoveryResult::Alive(candidate.info),
        Err(excluded) => DiscoveryResult::Excluded {
            instance_id: excluded.instance_id,
            reason: excluded.reason,
        },
    }
}

/// descriptor 検証 → プロセス同一性 → pipe 接続 → handshake → ping を順に実行する。
fn verify_candidate(
    path: &Path,
    config: DiscoveryConfig,
) -> Result<VerifiedCandidate, ExcludedCandidate> {
    let deadline = Instant::now() + config.per_candidate_deadline;

    // descriptor 検証。
    let descriptor =
        validate_descriptor_file(path).map_err(|reason| ExcludedCandidate::new(None, reason))?;
    let excluded = |reason| ExcludedCandidate::new(Some(descriptor.instance_id), reason);

    // PID とプロセス作成時刻。
    if !process_identity_matches(&descriptor) {
        return Err(excluded(ExclusionReason::ProcessIdentityMismatch));
    }

    // pipe 接続、handshake、ping。
    let (client, pong) =
        run_pipe_handshake_and_ping(&descriptor, deadline).map_err(|err| ExcludedCandidate {
            instance_id: Some(descriptor.instance_id),
            reason: map_pipe_error(&err),
            // インスタンスが応答した場合のみ、その内容を上位の判断へ残す。
            rejection: match err {
                PipeClientError::Remote(error) => Some(error),
                _ => None,
            },
        })?;

    if matches!(pong.state, InstanceState::Draining | InstanceState::Gone) {
        return Err(excluded(ExclusionReason::PingFailed));
    }

    Ok(VerifiedCandidate {
        info: build_instance_info(descriptor, pong),
        client,
    })
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

/// pipe 接続、handshake、ping を実行し、認証済み接続と ping 応答を返す。
fn run_pipe_handshake_and_ping(
    descriptor: &InstanceDescriptor,
    deadline: Instant,
) -> Result<(PipeClient, PongResult), PipeClientError> {
    let client = PipeClient::connect_and_handshake(
        descriptor.instance_id,
        descriptor.pid,
        &descriptor.process_created_at,
        &descriptor.auth_secret,
        deadline,
    )?;

    let pong = client.ping(deadline)?;
    Ok((client, pong))
}

/// `PipeClientError` を `ExclusionReason` へ対応付ける。
fn map_pipe_error(err: &PipeClientError) -> ExclusionReason {
    match err {
        PipeClientError::ConnectFailed
        | PipeClientError::Timeout
        | PipeClientError::Io(_)
        | PipeClientError::Desynced => ExclusionReason::PipeUnreachable,
        // 接続先は応答したが契約どおりの内容ではなく、生存を確認できていない。
        PipeClientError::Framing
        | PipeClientError::Json
        | PipeClientError::InvalidResponse
        | PipeClientError::Remote(_) => ExclusionReason::PingFailed,
        PipeClientError::AuthenticationFailed => ExclusionReason::AuthenticationFailed,
        PipeClientError::ProtocolMismatch => ExclusionReason::ProtocolMismatch,
        PipeClientError::InstanceStale => ExclusionReason::ProcessIdentityMismatch,
    }
}

/// プロセス作成時刻が descriptor 記載値と厳密に一致するか判定する。
///
/// descriptor へは 100 ナノ秒粒度の `FILETIME` を欠落なく記録するため、
/// OS から取得した時刻と時点として完全に一致する。許容幅を設けると、その幅の
/// 間に再利用された PID を同一プロセスと誤認する余地が残る。
fn process_created_at_matches(descriptor_value: &str, actual: DateTime<Utc>) -> bool {
    parse_utc_timestamp(descriptor_value).is_ok_and(|parsed| parsed == actual)
}

/// `InstanceDescriptor` と ping 応答から `InstanceInfo` を生成する。
///
/// registry の descriptor は表示名とパスしか持たない。epoch / revision / modified は
/// ping 応答が運んだ場合にのみ入り、運ばれなければ欠落のままとする。既定値で埋めると
/// 「未取得」と実測値が区別できなくなる。
///
/// 現在シーンは ping 応答に含まれない。シーンは編集ハンドルを介してしか読めず、
/// 生存確認だけでは取得できないため `None` とする。
fn build_instance_info(descriptor: InstanceDescriptor, pong: PongResult) -> InstanceInfo {
    let project = pong.project;
    InstanceInfo {
        instance_id: descriptor.instance_id,
        state: pong.state,
        pid: descriptor.pid,
        started_at: descriptor.started_at,
        project: descriptor
            .project
            .map(|descriptor_project| InstanceProject {
                display_name: descriptor_project.display_name,
                path: Some(descriptor_project.path),
                epoch: project.as_ref().map(|p| p.epoch.clone()),
                revision: project.as_ref().map(|p| p.revision),
                modified: project.as_ref().map(|p| p.modified),
            }),
        scene: None,
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
        debug!(instance = %redact::descriptor_file(path), "descriptor revalidated; skipped cleanup");
        return Ok(());
    }

    std::fs::remove_file(path)?;
    debug!(instance = %redact::descriptor_file(path), "stale descriptor removed");
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
    use aviutl2_mcp_core::{AuthSecret, ProtocolVersion, format_utc_timestamp};

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
            process_created_at: format_utc_timestamp(identity.created_at),
            hwnd: None,
            started_at: format_utc_timestamp(identity.created_at),
            state: InstanceState::Ready,
            project: Some(DescriptorProject {
                display_name: "Test".to_string(),
                path: r"C:\test.aup".to_string(),
            }),
        }
    }

    #[test]
    fn instance_info_takes_the_project_state_from_the_ping_result() {
        let id = InstanceId::new_v4();
        let pong =
            PongResult::new(id, InstanceState::Ready).with_project(aviutl2_mcp_core::PongProject {
                epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
                revision: 42,
                modified: true,
            });

        let info = build_instance_info(sample_descriptor(id), pong);
        let project = info.project.expect("project が失われています");
        assert_eq!(project.display_name, "Test");
        assert_eq!(project.path.as_deref(), Some(r"C:\test.aup"));
        assert_eq!(
            project.epoch.as_deref(),
            Some("78be92d1-c8c9-44c6-ae52-387548971468")
        );
        assert_eq!(project.revision, Some(42));
        assert_eq!(project.modified, Some(true));
        assert_eq!(info.state, InstanceState::Ready);
        // シーンは生存確認からは取得できない。
        assert_eq!(info.scene, None);
    }

    #[test]
    fn instance_info_keeps_the_project_state_absent_when_the_ping_omits_it() {
        // 既定値で埋めると「未取得」と実測値が区別できなくなる。特に modified は
        // 「未保存の変更が無い」と読めてしまい、保存確認の要否を誤らせる。
        let id = InstanceId::new_v4();
        let info = build_instance_info(
            sample_descriptor(id),
            PongResult::new(id, InstanceState::Busy),
        );

        let project = info.project.expect("project が失われています");
        assert_eq!(project.epoch, None);
        assert_eq!(project.revision, None);
        assert_eq!(project.modified, None);
    }

    #[test]
    fn process_created_at_matches_self() {
        let identity = self_identity();
        assert!(process_created_at_matches(
            &format_utc_timestamp(identity.created_at),
            identity.created_at
        ));
    }

    #[test]
    fn process_created_at_rejects_sub_second_difference() {
        let identity = self_identity();
        let text = format_utc_timestamp(identity.created_at);
        for delta in [
            chrono::Duration::nanoseconds(100),
            chrono::Duration::milliseconds(1),
            chrono::Duration::seconds(1),
        ] {
            assert!(
                !process_created_at_matches(&text, identity.created_at + delta),
                "{delta} のずれは別プロセスとして扱う"
            );
        }
    }

    #[test]
    fn process_created_at_rejects_unparsable_value() {
        let identity = self_identity();
        assert!(!process_created_at_matches("", identity.created_at));
        assert!(!process_created_at_matches(
            "2026-01-01 00:00:00",
            identity.created_at
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

        find_instances(&dir, DiscoveryConfig::default(), true).unwrap();
        assert!(!path.exists(), "stale descriptor should be cleaned up");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_registry_dir_is_zero_instances() {
        let dir = temp_registry_dir();
        assert!(!dir.exists());

        let instances = find_instances(&dir, DiscoveryConfig::default(), true).unwrap();
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

        let result = find_instances(&path, DiscoveryConfig::default(), true);
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

        find_instances(&dir, DiscoveryConfig::default(), true).unwrap();
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
    fn exclusion_reason_maps_to_error_code() {
        let cases = [
            (ExclusionReason::InvalidDescriptor, ErrorCode::InstanceStale),
            (ExclusionReason::InternalError, ErrorCode::InternalError),
            (
                ExclusionReason::ProtocolMismatch,
                ErrorCode::ProtocolMismatch,
            ),
            (
                ExclusionReason::ProcessIdentityMismatch,
                ErrorCode::InstanceStale,
            ),
            (ExclusionReason::PipeUnreachable, ErrorCode::InstanceStale),
            (ExclusionReason::PingFailed, ErrorCode::InstanceStale),
            (
                ExclusionReason::AuthenticationFailed,
                ErrorCode::AuthenticationFailed,
            ),
        ];
        for (reason, expected) in cases {
            assert_eq!(
                reason.error_code(),
                expected,
                "{} の対応が誤り",
                reason.as_code()
            );
            assert_eq!(
                ResolveInstanceError::Excluded(reason).error_code(),
                expected
            );
        }

        // 候補が 1 件も見つからない場合だけが instance_not_found になる。
        assert_eq!(
            ResolveInstanceError::NotRegistered.error_code(),
            ErrorCode::InstanceNotFound
        );
    }

    #[test]
    fn resolve_instance_reports_not_registered_for_missing_descriptor() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();

        let error = resolve_instance(&dir, InstanceId::new_v4(), DiscoveryConfig::default())
            .err()
            .expect("登録の無い instance_id は解決できない");
        assert!(
            matches!(error, ResolveInstanceError::NotRegistered),
            "実際のエラー: {error:?}"
        );
        assert_eq!(error.error_code(), ErrorCode::InstanceNotFound);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_instance_ignores_descriptors_of_other_instances() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let other = InstanceId::new_v4();
        std::fs::write(
            dir.join(format!("{}.json", other)),
            serde_json::to_string(&sample_descriptor(other)).unwrap(),
        )
        .unwrap();

        // 他 instance の descriptor があっても、要求された ID は未登録のままである。
        let error = resolve_instance(&dir, InstanceId::new_v4(), DiscoveryConfig::default())
            .err()
            .expect("別 ID の descriptor では解決できない");
        assert_eq!(error.error_code(), ErrorCode::InstanceNotFound);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_instance_reports_stale_for_dead_process() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        let mut descriptor = sample_descriptor(id);
        descriptor.pid = 0xFFFF_FFFF; // 存在しない PID
        std::fs::write(
            dir.join(format!("{}.json", id)),
            serde_json::to_string(&descriptor).unwrap(),
        )
        .unwrap();

        let error = resolve_instance(&dir, id, DiscoveryConfig::default())
            .err()
            .expect("存在しない PID の descriptor では解決できない");
        assert!(
            matches!(
                error,
                ResolveInstanceError::Excluded(ExclusionReason::ProcessIdentityMismatch)
            ),
            "実際のエラー: {error:?}"
        );
        assert_eq!(error.error_code(), ErrorCode::InstanceStale);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_instance_reports_stale_for_unreachable_pipe() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        // 自 PID を指すため生存確認は通るが、pipe は待ち受けていない。
        std::fs::write(
            dir.join(format!("{}.json", id)),
            serde_json::to_string(&sample_descriptor(id)).unwrap(),
        )
        .unwrap();

        let error = resolve_instance(&dir, id, DiscoveryConfig::default())
            .err()
            .expect("接続できない pipe では解決できない");
        assert!(
            matches!(
                error,
                ResolveInstanceError::Excluded(ExclusionReason::PipeUnreachable)
            ),
            "実際のエラー: {error:?}"
        );
        assert_eq!(error.error_code(), ErrorCode::InstanceStale);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_instance_reports_protocol_mismatch() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        let mut descriptor = sample_descriptor(id);
        descriptor.protocol_version = ProtocolVersion {
            major: ProtocolVersion::CURRENT.major + 1,
            minor: 0,
        };
        std::fs::write(
            dir.join(format!("{}.json", id)),
            serde_json::to_string(&descriptor).unwrap(),
        )
        .unwrap();

        let error = resolve_instance(&dir, id, DiscoveryConfig::default())
            .err()
            .expect("MAJOR 不一致の descriptor では解決できない");
        assert_eq!(error.error_code(), ErrorCode::ProtocolMismatch);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_instance_reports_excluded_for_broken_descriptor() {
        let dir = temp_registry_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let id = InstanceId::new_v4();
        std::fs::write(dir.join(format!("{}.json", id)), b"{ broken").unwrap();

        let error = resolve_instance(&dir, id, DiscoveryConfig::default())
            .err()
            .expect("解釈できない descriptor では解決できない");
        // ファイルは存在するため未登録とは区別する。
        assert!(
            matches!(
                error,
                ResolveInstanceError::Excluded(ExclusionReason::InvalidDescriptor)
            ),
            "実際のエラー: {error:?}"
        );
        // 読み取れない descriptor は一覧の取り直しで解消し得るため stale とする。
        assert_eq!(error.error_code(), ErrorCode::InstanceStale);
        assert!(
            error.error_code().default_retryable(),
            "取り直しを促せるようリトライ可能なコードにする"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn descriptor_path_is_instance_id_json() {
        let id = InstanceId::new_v4();
        let dir = Path::new(r"C:\registry");
        assert_eq!(
            descriptor_path(dir, &id),
            dir.join(format!("{id}.json")),
            "descriptor は instance_id を名前とする JSON ファイル"
        );
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

        find_instances(&dir, DiscoveryConfig::default(), true).unwrap();
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

        let instances = find_instances(&dir, DiscoveryConfig::default(), true).unwrap();
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

        let instances = find_instances(&dir, DiscoveryConfig::default(), true).unwrap();
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

        let instances = find_instances(&dir, DiscoveryConfig::default(), true).unwrap();
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
        let created_at = format_utc_timestamp(identity.created_at);

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
