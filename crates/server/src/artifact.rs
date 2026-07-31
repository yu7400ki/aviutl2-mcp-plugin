//! server が所有する一時成果物（artifact）の store。
//!
//! レンダリング結果の画像は AviUtl2 のプロセスで生まれ、ファイルとして
//! 引き渡される。本モジュールはその引き取りと、以降 MCP resource として
//! 配り終えるまでの所有を引き受ける。
//!
//! **パスを組み立てる材料は要求経路から入らない。** 要求の応答が運ぶのは
//! handoff token だけであり、基底と `instance_id` は server が自分で決めた
//! 値を使う。token は [`HandoffToken`] へ通した場合にのみパスの組み立てへ
//! 渡せるため、構文検証を経ていない文字列がファイル名になる経路が無い。
//!
//! 識別子からの引き当ても同様に、in-memory の一覧に対してのみ行う。
//! `artifact_id` をパスへ連結しないため、どのような文字列を与えても
//! 「見つからない」で終わる。

mod protected_dir;

use aviutl2_mcp_core::{ARTIFACT_MAX_BYTES, InstanceId};
use chrono::{DateTime, TimeDelta, Utc};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tracing::{debug, warn};
use uuid::Uuid;

/// artifact の有効期限。
pub const ARTIFACT_TTL: Duration = Duration::from_secs(10 * 60);

/// 同時に保持する artifact の件数の上限。
pub const ARTIFACT_MAX_COUNT: usize = 16;

/// 同時に保持する artifact の総量の上限。
pub const ARTIFACT_MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;

/// artifact の MIME type。
pub const ARTIFACT_MEDIA_TYPE: &str = "image/png";

/// 起動時に他プロセスの残骸とみなす session ディレクトリの古さ。
///
/// 異常終了した server の store は次の起動まで残る。一方で、同時に稼働する
/// 別の server の store を消してはならない。所有者の生存は
/// [`SESSION_LOCK_FILE`] で判定し、この時間は判定が付かない場合に備えた
/// 二重の余裕である。
const SESSION_STALE_AFTER: Duration = Duration::from_secs(60 * 60);

/// session ディレクトリの所有者が生きていることを示すファイルの名前。
///
/// **最終更新時刻だけでは所有者の生存を判定できない。** ディレクトリの
/// 最終更新時刻が動くのは中身が増減したときだけであり、しばらく成果物を
/// 作っていない稼働中の store は、いくらでも古く見える。それを消すと、
/// 消された側は書き込み先を失ったまま動き続ける。
///
/// そこで store は自分の session ディレクトリの中にこのファイルを作り、
/// 共有を一切許さずに開いたまま保持する。他プロセスはこのファイルを
/// 同じ方法で開けるかどうかで所有者の生存を判定でき、開けている間は
/// ディレクトリの削除自体も OS が拒む。
const SESSION_LOCK_FILE: &str = "session.lock";

/// handoff ファイルを置く、基底直下のディレクトリ名。
const HANDOFF_DIR: &str = "render";

/// artifact store を置く、基底直下のディレクトリ名。
const ARTIFACTS_DIR: &str = "artifacts";

/// 成果物ファイルの拡張子。
const ARTIFACT_EXTENSION: &str = "png";

/// handoff token の文字数（128 bit を小文字十六進で表した長さ）。
const HANDOFF_TOKEN_LEN: usize = 32;

/// ダイジェストの前置文字列。
const SHA256_PREFIX: &str = "sha256:";

/// 構文検証を通した handoff token。
///
/// 小文字十六進ちょうど [`HANDOFF_TOKEN_LEN`] 文字だけがこの型になる。
/// handoff ファイルのパスを組み立てる経路はこの型しか受け取らないため、
/// 検証を経ていない値が経路長・区切り文字・大小文字の違いを持ち込めない。
///
/// `Debug` は値を出さない。token は応答にもログにも現れてはならず、
/// これを含む構造体をそのまま記録した場合にも漏れないようにする。
#[derive(Clone, PartialEq, Eq)]
pub struct HandoffToken(String);

/// handoff token の書式違反。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("handoff token は 32 桁の小文字十六進である必要があります")]
pub struct HandoffTokenFormatError;

impl HandoffToken {
    /// 構文を検証して token を作る。
    ///
    /// 受け付けるのは `0-9` と `a-f` だけからなるちょうど 32 文字である。
    /// 長さ違い・大文字・区切り文字・`..`・空文字・十六進でない Unicode は
    /// いずれも拒否する。バイト単位で判定するため、非 ASCII の文字は
    /// 長さの一致にかかわらず十六進でないバイトとして落ちる。
    pub fn parse(value: &str) -> Result<Self, HandoffTokenFormatError> {
        let is_lower_hex = value.len() == HANDOFF_TOKEN_LEN
            && value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if is_lower_hex {
            Ok(Self(value.to_owned()))
        } else {
            Err(HandoffTokenFormatError)
        }
    }

    /// handoff ファイルの名前を返す。
    fn file_name(&self) -> String {
        format!("{}.{ARTIFACT_EXTENSION}", self.0)
    }
}

impl fmt::Debug for HandoffToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HandoffToken(<redacted>)")
    }
}

/// server が所有する一時成果物。
///
/// 実体のパスは公開しない。要求元へ渡すのは識別子と metadata だけであり、
/// `Debug` にもパスは現れない。
#[derive(Clone)]
pub struct Artifact {
    /// UUID v4。handoff token とは別の値であり、これを見てもレンダリング元の
    /// プロセスが書いたファイルの名前は導けない。
    pub artifact_id: String,
    /// MIME type。本 Phase は PNG のみを公開する。
    pub media_type: &'static str,
    /// 実体のバイト数。
    pub byte_length: u64,
    /// `"sha256:" + 64 桁の小文字十六進`。
    pub sha256: String,
    /// 引き取りが完了した時刻。
    pub created_at: DateTime<Utc>,
    /// この時刻以降は引き当てられない。
    pub expires_at: DateTime<Utc>,
    /// store 内の実体。外へ出さない。
    path: PathBuf,
}

impl fmt::Debug for Artifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Artifact")
            .field("artifact_id", &self.artifact_id)
            .field("media_type", &self.media_type)
            .field("byte_length", &self.byte_length)
            .field("sha256", &self.sha256)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// artifact の metadata と実体。
///
/// `Debug` に実体を出さない。画像には利用者のプロジェクトの内容が写る。
#[derive(Clone)]
pub struct ArtifactContent {
    /// 対応する artifact の metadata。
    pub artifact: Artifact,
    /// 実体のバイト列。
    pub bytes: Vec<u8>,
}

impl fmt::Debug for ArtifactContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArtifactContent")
            .field("artifact", &self.artifact)
            .finish_non_exhaustive()
    }
}

/// 引き取りの失敗。
///
/// いずれの場合も artifact は作られない。handoff ファイルは、パスを組み立てた
/// うえで失敗した場合に限り削除されている（[`IngestError::InvalidToken`] は
/// パスを組み立てないため、何も削除しない）。
/// 呼び出し側は要求元へ内部エラーとして返す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IngestError {
    /// handoff token の構文が不正である。パスは組み立てておらず、
    /// ファイルへは一切触れていない。
    #[error("handoff token の書式が不正です")]
    InvalidToken,
    /// handoff ファイルを開けなかった、または読み切れなかった。
    #[error("handoff ファイルを読み取れませんでした")]
    Unreadable,
    /// handoff ファイルが [`ARTIFACT_MAX_BYTES`] を超えている。
    #[error("handoff ファイルが上限を超えています")]
    TooLarge,
    /// 実体が申告された長さ・ダイジェストと一致しない。
    #[error("handoff ファイルの内容が申告と一致しません")]
    ContentMismatch,
    /// store へ書き込めなかった。
    #[error("artifact を保存できませんでした")]
    StoreUnavailable,
}

impl IngestError {
    /// ログへ残す安全な理由コードを返す。
    pub fn as_code(&self) -> &'static str {
        match self {
            IngestError::InvalidToken => "invalid_token",
            IngestError::Unreadable => "unreadable",
            IngestError::TooLarge => "too_large",
            IngestError::ContentMismatch => "content_mismatch",
            IngestError::StoreUnavailable => "store_unavailable",
        }
    }
}

/// store を開けなかった。
#[derive(Debug, thiserror::Error)]
pub enum ArtifactStoreError {
    /// store のディレクトリを用意できなかった。
    #[error("artifact store のディレクトリを用意できませんでした: {0}")]
    DirectoryUnavailable(#[source] std::io::Error),
}

/// 現在時刻の供給元。
///
/// 有効期限の判定を実時間から切り離し、期限切れを待たずに試験できるようにする。
/// 公開しないのは、有効期限が設定ではないためである。crate の外から差し替え
/// られる形にすると、上限と保存時間を定数と定めたことが読めなくなる。
pub(crate) trait ArtifactClock: Send + Sync {
    /// 現在の UTC 時刻を返す。
    fn now(&self) -> DateTime<Utc>;
}

/// システム時刻を返す既定の供給元。
pub(crate) struct SystemClock;

impl ArtifactClock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// discovery が使う registry ディレクトリから、handoff と store の基底を導く。
///
/// registry は基底直下の `instances` として作られるため、その親が基底である。
/// 親を取れない場合は registry 自身を基底として扱う。基底を独立に決めると
/// discovery と食い違う余地が生まれるため、常に registry から導く。
pub fn base_dir_for_registry(registry_dir: &Path) -> PathBuf {
    registry_dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(registry_dir)
        .to_path_buf()
}

/// server プロセスが所有する artifact の store。
///
/// 保持している artifact は `{base}\artifacts\{server_session_id}` の下にあり、
/// この store が drop される（= server が終了する）ときにディレクトリごと消える。
pub struct ArtifactStore {
    /// handoff と store に共通の基底。
    base_dir: PathBuf,
    /// この server プロセスの store ディレクトリ。
    session_dir: PathBuf,
    /// 稼働中であることを示す、開いたままのロックファイル。
    ///
    /// 掴んでいる間は他プロセスがこのディレクトリを消せない。取り直せる
    /// ようにするため、ハンドルは差し替え可能な形で持つ。
    lock: Mutex<Option<File>>,
    /// 登録から失効までの時間。
    ttl: Duration,
    clock: Arc<dyn ArtifactClock>,
    /// 登録済み artifact を `created_at` の昇順で保持する。
    ///
    /// 件数は [`ARTIFACT_MAX_COUNT`] で縛られるため、引き当ては線形走査で足りる。
    /// 順序を保つことで、上限超過時に落とす「最も古いもの」が先頭に定まる。
    entries: Mutex<Vec<Artifact>>,
}

impl ArtifactStore {
    /// 既定の有効期限とシステム時刻で store を開く。
    pub fn open(base_dir: PathBuf) -> Result<Self, ArtifactStoreError> {
        Self::open_with(base_dir, ARTIFACT_TTL, Arc::new(SystemClock))
    }

    /// 有効期限と時刻の供給元を指定して store を開く。
    ///
    /// `{base}\artifacts\{server_session_id}` を保護された DACL で作成し、
    /// [`SESSION_LOCK_FILE`] を掴んでから、同じ親にある放置された session
    /// ディレクトリを best effort で削除する。削除の対象は所有者が生きて
    /// いないことを確かめられ、かつ [`SESSION_STALE_AFTER`] より古いものだけ
    /// であり、稼働中の別 server の store には触れない。
    pub(crate) fn open_with(
        base_dir: PathBuf,
        ttl: Duration,
        clock: Arc<dyn ArtifactClock>,
    ) -> Result<Self, ArtifactStoreError> {
        // 基底も保護の対象に含める。ここが継承したままだと、その下を
        // いくら絞っても基底の一覧が他のユーザーへ開いたままになる。
        if let Some(parent) = base_dir
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(ArtifactStoreError::DirectoryUnavailable)?;
        }
        protected_dir::create_protected_directory(&base_dir)
            .map_err(ArtifactStoreError::DirectoryUnavailable)?;

        let artifacts_root = base_dir.join(ARTIFACTS_DIR);
        protected_dir::create_protected_directory(&artifacts_root)
            .map_err(ArtifactStoreError::DirectoryUnavailable)?;

        let session_dir = artifacts_root.join(Uuid::new_v4().to_string());
        protected_dir::create_protected_directory(&session_dir)
            .map_err(ArtifactStoreError::DirectoryUnavailable)?;
        let lock =
            open_session_lock(&session_dir).map_err(ArtifactStoreError::DirectoryUnavailable)?;

        // 自分のロックを掴んでから掃除する。順序が逆だと、掃除の最中に
        // 別 server から自分の store を放置扱いされる余地が残る。
        sweep_stale_sessions(&artifacts_root, &session_dir, SESSION_STALE_AFTER);

        Ok(Self {
            base_dir,
            session_dir,
            lock: Mutex::new(Some(lock)),
            ttl,
            clock,
            entries: Mutex::new(Vec::new()),
        })
    }

    /// plugin が書いた handoff ファイルを引き取り、artifact として登録する。
    ///
    /// 手順は次のとおりである。
    ///
    /// 1. token の構文を検証する
    /// 2. 自分の基底と解決済みの `instance_id` からパスを組み立てる
    /// 3. ファイルを開き、**全体を読む前に** 大きさが [`ARTIFACT_MAX_BYTES`]
    ///    以下であることを確認する
    /// 4. 読み込み、申告された長さとダイジェストに一致することを確認する
    /// 5. `artifact_id` を採番して store へ書き込む
    /// 6. handoff ファイルを削除する
    ///
    /// いずれかが失敗した場合も handoff ファイルは削除し、artifact を作らずに
    /// 失敗を返す。部分的に読めたものを配ることはない。
    ///
    /// `instance_id` は要求元が指定した値ではなく、discovery で解決済みの値を
    /// 渡すこと。`declared_sha256` は `"sha256:" + 64 桁の小文字十六進` である。
    pub fn ingest(
        &self,
        instance_id: &InstanceId,
        handoff_token: &str,
        declared_byte_length: u64,
        declared_sha256: &str,
    ) -> Result<Artifact, IngestError> {
        // 検証はパスの組み立てより先に行う。token はここでしか文字列から
        // 作られないため、以降の経路には構文を満たす値しか流れない。
        let token = HandoffToken::parse(handoff_token).map_err(|_| IngestError::InvalidToken)?;
        let path = self.handoff_path(instance_id, &token);

        let result = self.take_handoff(&path, declared_byte_length, declared_sha256);
        // 成否によらず引き渡し元のファイルは残さない。所有権は 1 か所ずつ移る。
        remove_file_best_effort(&path);
        result
    }

    /// 期限切れを落としたうえで、保持している artifact を古い順に返す。
    pub fn list(&self) -> Vec<Artifact> {
        let mut entries = self.lock_entries();
        self.sweep_expired(&mut entries);
        entries.clone()
    }

    /// `artifact_id` で artifact の metadata を引き当てる。
    ///
    /// 引き当ては in-memory の一覧に対してのみ行い、識別子をパスへ連結しない。
    /// 期限切れと未知の識別子はいずれも `None` になり、応答から区別できない。
    /// 区別すると、過去に存在した識別子を総当たりで調べられる。
    pub fn get(&self, artifact_id: &str) -> Option<Artifact> {
        let mut entries = self.lock_entries();
        self.sweep_expired(&mut entries);
        entries
            .iter()
            .find(|entry| entry.artifact_id == artifact_id)
            .cloned()
    }

    /// `artifact_id` の artifact を metadata と実体の組で読み出す。
    ///
    /// 引き当ての規則は [`ArtifactStore::get`] と同じである。実体を読めなかった
    /// 場合も `None` を返し、存在しない場合と区別しない。
    pub fn read(&self, artifact_id: &str) -> Option<ArtifactContent> {
        let artifact = self.get(artifact_id)?;
        match std::fs::read(&artifact.path) {
            Ok(bytes) => Some(ArtifactContent { artifact, bytes }),
            Err(e) => {
                warn!(
                    artifact_id = %artifact.artifact_id,
                    error = %e,
                    "artifact の実体を読み取れませんでした",
                );
                None
            }
        }
    }

    /// 保持している artifact の件数（期限切れを落としてから数える）。
    pub fn len(&self) -> usize {
        self.list().len()
    }

    /// 保持している artifact が無いかどうか。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// handoff ファイルのパスを組み立てる。
    ///
    /// 材料は自分が保持する基底、discovery で解決済みの `instance_id`、
    /// 構文検証を通した [`HandoffToken`] だけである。要求元が与えた文字列は
    /// この関数へ到達しない。
    fn handoff_path(&self, instance_id: &InstanceId, token: &HandoffToken) -> PathBuf {
        self.base_dir
            .join(HANDOFF_DIR)
            .join(instance_id.to_string())
            .join(token.file_name())
    }

    /// handoff ファイルを読み、照合して store へ移す。
    fn take_handoff(
        &self,
        path: &Path,
        declared_byte_length: u64,
        declared_sha256: &str,
    ) -> Result<Artifact, IngestError> {
        let file = File::open(path).map_err(|e| {
            warn!(error = %e, "handoff ファイルを開けませんでした");
            IngestError::Unreadable
        })?;
        let length = file
            .metadata()
            .map_err(|e| {
                warn!(error = %e, "handoff ファイルの大きさを取得できませんでした");
                IngestError::Unreadable
            })?
            .len();
        // 申告値ではなく実体の大きさで判定し、読み込む前に打ち切る。
        // 申告を信じて確保すると、申告と実体が食い違ったときに過大な確保が起きる。
        //
        // 上限は書き出す側と同じ [`ARTIFACT_MAX_BYTES`] である。書き出す側も
        // 同じ値で落とすため、ここへ届く超過は書き出した相手が別の規則で
        // 動いている場合に限られる。それでも判定を残すのは、引き取りの確保を
        // 相手の振る舞いに委ねないためである。
        if length > ARTIFACT_MAX_BYTES {
            warn!(byte_length = length, "handoff ファイルが上限を超えています");
            return Err(IngestError::TooLarge);
        }

        let bytes = read_bounded(file, length)?;
        if bytes.len() as u64 != declared_byte_length {
            warn!("handoff ファイルの長さが申告と一致しません");
            return Err(IngestError::ContentMismatch);
        }
        let sha256 = sha256_of(&bytes);
        if sha256 != declared_sha256 {
            warn!("handoff ファイルのダイジェストが申告と一致しません");
            return Err(IngestError::ContentMismatch);
        }

        self.insert(bytes, sha256)
    }

    /// 照合済みの内容を store へ登録する。
    fn insert(&self, bytes: Vec<u8>, sha256: String) -> Result<Artifact, IngestError> {
        let now = self.clock.now();
        let artifact_id = Uuid::new_v4().to_string();
        let artifact = Artifact {
            path: self
                .session_dir
                .join(format!("{artifact_id}.{ARTIFACT_EXTENSION}")),
            artifact_id,
            media_type: ARTIFACT_MEDIA_TYPE,
            byte_length: bytes.len() as u64,
            sha256,
            created_at: now,
            expires_at: now + self.expiry_delta(),
        };

        // 新しい実体を書き終えてから古いものを落とす。先に落とすと、書き込みに
        // 失敗したときに「新しい artifact も無く、古い artifact も失われた」
        // 状態が残る。
        self.write_artifact(&artifact, &bytes)?;

        let mut entries = self.lock_entries();
        // 期限切れを先に落とし、それでも上限を超えるなら古い順に落とす。
        self.sweep_expired(&mut entries);
        make_room(&mut entries, artifact.byte_length);

        debug!(
            artifact_id = %artifact.artifact_id,
            byte_length = artifact.byte_length,
            "artifact を登録しました",
        );
        entries.push(artifact.clone());
        Ok(artifact)
    }

    /// artifact の実体を store へ書き込む。
    fn write_artifact(&self, artifact: &Artifact, bytes: &[u8]) -> Result<(), IngestError> {
        self.ensure_session_dir().map_err(|e| {
            warn!(error = %e, "artifact store のディレクトリを用意できませんでした");
            IngestError::StoreUnavailable
        })?;
        if let Err(e) = std::fs::write(&artifact.path, bytes) {
            warn!(error = %e, "artifact を保存できませんでした");
            // 途中まで書けたものを配らない。
            remove_file_best_effort(&artifact.path);
            return Err(IngestError::StoreUnavailable);
        }
        Ok(())
    }

    /// store のディレクトリが無ければ作り直し、ロックを取り直す。
    ///
    /// ロックを掴んでいる限り他プロセスは消せないが、利用者が手で消す経路まで
    /// は塞げない。作り直さないと、一度消された store は以降の引き取りが
    /// すべて失敗し続ける恒久的な故障になる。
    fn ensure_session_dir(&self) -> std::io::Result<()> {
        if self.session_dir.is_dir() {
            return Ok(());
        }
        protected_dir::create_protected_directory(&self.session_dir)?;
        warn!("artifact store のディレクトリを作り直しました");
        *self.lock.lock().unwrap_or_else(|e| e.into_inner()) = open_session_lock(&self.session_dir)
            .map_err(|e| {
                warn!(error = %e, "artifact store のロックを取り直せませんでした");
                e
            })
            .ok();
        Ok(())
    }

    /// 期限切れの artifact を一覧と store の双方から落とす。
    ///
    /// 掃除の契機は新規登録時と一覧・読み取りの前だけである。専用のスレッドを
    /// 持たない。触られない store が期限切れを抱えたまま残るのは無害であり、
    /// スレッドを増やす方が停止時の面倒が増える。
    fn sweep_expired(&self, entries: &mut Vec<Artifact>) {
        let now = self.clock.now();
        entries.retain(|entry| {
            if entry.expires_at > now {
                return true;
            }
            debug!(artifact_id = %entry.artifact_id, "artifact が期限切れになりました");
            remove_file_best_effort(&entry.path);
            false
        });
    }

    /// 有効期限を `chrono` の差分へ変換する。
    ///
    /// 変換できない大きさは実装上あり得ないが、その場合も期限を無限にはせず
    /// 既定値へ落とす。
    fn expiry_delta(&self) -> TimeDelta {
        TimeDelta::from_std(self.ttl).unwrap_or_else(|_| {
            TimeDelta::from_std(ARTIFACT_TTL).expect("既定の有効期限は必ず変換できる")
        })
    }

    /// 一覧のロックを取る。
    ///
    /// 保持中に panic した場合でも store 自体は使い続けられる。中身は artifact の
    /// 一覧だけであり、不整合が残るとすれば「実体の無い項目」または
    /// 「一覧に無い実体」でしかなく、いずれも掃除と終了時の削除で回収される。
    fn lock_entries(&self) -> MutexGuard<'_, Vec<Artifact>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Drop for ArtifactStore {
    /// server の終了時に store ディレクトリごと削除する。
    ///
    /// artifact は server プロセスだけが読むものであり、プロセスの終了後に
    /// 残す理由が無い。
    fn drop(&mut self) {
        // 自分で掴んでいるロックを先に手放す。開いたままでは自分の
        // ディレクトリも消せない。
        drop(self.lock.lock().unwrap_or_else(|e| e.into_inner()).take());
        match std::fs::remove_dir_all(&self.session_dir) {
            Ok(()) => debug!("artifact store を削除しました"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(error = %e, "artifact store を削除できませんでした"),
        }
    }
}

/// 新規登録のために古い順で落とし、件数と総量の上限を満たす。
///
/// 一覧は `created_at` の昇順であるため、先頭から落とすことが「古い順」になる。
fn make_room(entries: &mut Vec<Artifact>, incoming: u64) {
    while !entries.is_empty() && !fits(entries, incoming) {
        let evicted = entries.remove(0);
        debug!(
            artifact_id = %evicted.artifact_id,
            "上限を満たすため古い artifact を落としました",
        );
        remove_file_best_effort(&evicted.path);
    }
}

/// `incoming` バイトの artifact を追加しても件数・総量の上限を超えないか。
fn fits(entries: &[Artifact], incoming: u64) -> bool {
    let total: u64 = entries.iter().map(|entry| entry.byte_length).sum();
    entries.len() < ARTIFACT_MAX_COUNT && total.saturating_add(incoming) <= ARTIFACT_MAX_TOTAL_BYTES
}

/// ファイル全体を読み込む。
///
/// 読み取りの途中でファイルが伸びた場合にも上限を超えて確保しないよう、
/// 読み取り自体を [`ARTIFACT_MAX_BYTES`] + 1 バイトで区切る。
fn read_bounded(file: File, length: u64) -> Result<Vec<u8>, IngestError> {
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(ARTIFACT_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| {
            warn!(error = %e, "handoff ファイルを読み取れませんでした");
            IngestError::Unreadable
        })?;
    if bytes.len() as u64 > ARTIFACT_MAX_BYTES {
        warn!("handoff ファイルが読み取り中に上限を超えました");
        return Err(IngestError::TooLarge);
    }
    Ok(bytes)
}

/// バイト列の SHA-256 を `"sha256:" + 64 桁の小文字十六進` で表す。
fn sha256_of(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut value = String::with_capacity(SHA256_PREFIX.len() + digest.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in digest {
        value.push(DIGITS[usize::from(byte >> 4)] as char);
        value.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    value
}

/// ファイルを削除する。失敗はログへ残すだけで伝播させない。
///
/// 失敗の説明にパスを含めない。どの artifact かは呼び出し元の記録が示す。
fn remove_file_best_effort(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(error = %e, "ファイルを削除できませんでした"),
    }
}

/// 放置された session ディレクトリを削除する（best effort）。
///
/// 異常終了した server の store を回収するためのものであり、`current` と、
/// 所有者が生きているディレクトリには触れない。生存は [`SESSION_LOCK_FILE`] を
/// 排他で開けるかどうかで判定し、最終更新時刻はその上に重ねる二重の余裕として
/// 使う。**時刻だけで判定してはならない** — ディレクトリの最終更新時刻は中身が
/// 増減したときにしか動かないため、しばらく成果物を作っていない稼働中の store が
/// 消される。
fn sweep_stale_sessions(root: &Path, current: &Path, older_than: Duration) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == current || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        if session_is_owned(&path) {
            debug!("稼働中の artifact store には触れません");
            continue;
        }
        let is_old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| {
                modified
                    .elapsed()
                    .is_ok_and(|elapsed| elapsed >= older_than)
            });
        if !is_old {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => debug!("放置された artifact store を削除しました"),
            Err(e) => warn!(error = %e, "放置された artifact store を削除できませんでした"),
        }
    }
}

/// session ディレクトリの所有者が生きているかを判定する。
///
/// 判定は [`SESSION_LOCK_FILE`] を共有なしで開けるかどうかで行う。開けたなら
/// 誰も掴んでおらず所有者はいない。ロックファイル自体が無いのは、掴む前に
/// 落ちた store か、この方式を持たない版の残骸であり、いずれも所有者はいない。
/// それ以外の失敗は判定が付かないため、生きているものとして扱う。
fn session_is_owned(session_dir: &Path) -> bool {
    match open_exclusive(&session_dir.join(SESSION_LOCK_FILE)) {
        Ok(_) => false,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// session ディレクトリのロックファイルを作成し、掴んだまま返す。
///
/// 共有を許さないため、他プロセスが掴んでいるディレクトリに対しては失敗する。
/// 自分の session ディレクトリは採番したばかりであり、競合は起こらない。
fn open_session_lock(session_dir: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .share_mode(0)
        .open(session_dir.join(SESSION_LOCK_FILE))
}

/// 共有を一切許さずに既存のファイルを開く。
///
/// 開けている間、他プロセスはこのファイルを開くことも消すこともできず、
/// これを含むディレクトリの削除も失敗する。
fn open_exclusive(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
}

#[cfg(test)]
mod tests;
