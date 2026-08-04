//! artifact store と handoff の引き取りの単体テスト。

use super::*;
use crate::test_support::capture_logs;
use aviutl2_mcp_core::HANDOFF_DIR;
use std::sync::atomic::{AtomicI64, Ordering};

/// 任意の時刻を指す時計。
struct FixedClock(AtomicI64);

impl FixedClock {
    fn new() -> Arc<Self> {
        Arc::new(Self(AtomicI64::new(0)))
    }

    /// 時刻を `seconds` 秒進める。
    fn advance(&self, seconds: i64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl ArtifactClock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.0.load(Ordering::SeqCst), 0).expect("表現できる時刻")
    }
}

/// テスト用の基底ディレクトリ。
fn temp_base_dir() -> PathBuf {
    std::env::temp_dir().join(format!("aviutl2-mcp-artifact-test-{}", Uuid::new_v4()))
}

/// 既定の設定を配る供給元。
fn default_settings() -> Arc<SettingsSource> {
    SettingsSource::fixed(Settings::default())
}

/// 既定の上限。store が設定から引く値と同じである。
fn limits() -> ArtifactLimits {
    ArtifactLimits::default()
}

/// 設定ファイルの内容から解決済みの設定を作る。
fn settings_from(text: &str) -> Settings {
    aviutl2_mcp_core::settings::SettingsDocument::parse(text)
        .unwrap()
        .resolve(&Settings::default())
        .0
}

/// store と、その基底・時計を束ねた試験環境。
struct Fixture {
    base_dir: PathBuf,
    clock: Arc<FixedClock>,
    /// 後始末で基底を消す前に閉じる必要があるため、取り出せる形で持つ。
    store: Option<ArtifactStore>,
    instance_id: InstanceId,
}

impl Fixture {
    fn new() -> Self {
        let base_dir = temp_base_dir();
        let clock = FixedClock::new();
        let store = ArtifactStore::open_with(base_dir.clone(), default_settings(), clock.clone())
            .expect("store を開けます");
        Self {
            base_dir,
            clock,
            store: Some(store),
            instance_id: InstanceId::new_v4(),
        }
    }

    /// 試験対象の store。
    fn store(&self) -> &ArtifactStore {
        self.store.as_ref().expect("store は後始末まで生きています")
    }

    /// handoff ファイルを書き、そのパスと token を返す。
    fn write_handoff(&self, token: &str, bytes: &[u8]) -> PathBuf {
        let dir = self
            .base_dir
            .join(HANDOFF_DIR)
            .join(self.instance_id.to_string());
        std::fs::create_dir_all(&dir).expect("handoff ディレクトリを作成できます");
        let path = dir.join(format!("{token}.{ARTIFACT_EXTENSION}"));
        std::fs::write(&path, bytes).expect("handoff ファイルを書けます");
        path
    }

    /// 申告どおりの handoff を引き取る。
    fn ingest_valid(&self, token: &str, bytes: &[u8]) -> Artifact {
        self.write_handoff(token, bytes);
        self.store()
            .ingest(
                &self.instance_id,
                token,
                bytes.len() as u64,
                &sha256_of(bytes),
            )
            .expect("申告と一致する handoff は引き取れます")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // store が session.lock を共有無しで掴んでいる間は基底を消せない。
        // 先に閉じないと、走査がそのファイルで打ち切られて handoff 側が残る。
        drop(self.store.take());
        let _ = std::fs::remove_dir_all(&self.base_dir);
    }
}

/// 有効な token を `seed` から作る。
fn token(seed: u8) -> String {
    format!("{seed:02x}").repeat(16)
}

// ============================================================================
// パスの組み立て
// ============================================================================

#[test]
fn handoff_path_is_built_from_the_own_base_and_the_resolved_instance() {
    // 組み立てに使えるのは検証済みの token だけであり、要求元が与えた文字列は
    // ここへ到達しない。
    //
    // 期待は取り決めそのものを書き下す。書き出す側は自分の基底から同じ形を
    // 組み立てるため、共有の定義だけが変われば引き取る側もここで落ちる。
    let fixture = Fixture::new();
    let value = token(0x5a);
    let parsed = HandoffToken::parse(&value).unwrap();
    let path = fixture.store().handoff_path(&fixture.instance_id, &parsed);

    assert_eq!(
        path,
        fixture
            .base_dir
            .join("render")
            .join(fixture.instance_id.to_string())
            .join(format!("{value}.png")),
    );
}

#[test]
fn base_dir_is_the_parent_only_when_the_registry_dir_has_our_shape() {
    let base = Path::new(r"C:\Users\someone\AppData\Local\AviUtl2Mcp");
    assert_eq!(base_dir_for_registry(&base.join("instances")), base);

    // 我々が置く形でなければ辿らない。辿ると、registry の場所を指す値ひとつで
    // 基底が上のディレクトリごと保護の適用対象になる。
    assert_eq!(
        base_dir_for_registry(&base.join("other")),
        base.join("other")
    );

    // 一致は完全一致で見る。大小違いは辿らない側へ倒れる。
    assert_eq!(
        base_dir_for_registry(&base.join("Instances")),
        base.join("Instances")
    );

    // 親を取れない場合も基底を作れないままにはしない。
    assert_eq!(
        base_dir_for_registry(Path::new("instances")),
        PathBuf::from("instances")
    );
}

// ============================================================================
// 引き取り
// ============================================================================

#[test]
fn ingest_moves_the_handoff_file_into_the_store() {
    let fixture = Fixture::new();
    let bytes = b"rendered image".to_vec();
    let value = token(0x11);
    let handoff = fixture.write_handoff(&value, &bytes);

    let artifact = fixture
        .store()
        .ingest(
            &fixture.instance_id,
            &value,
            bytes.len() as u64,
            &sha256_of(&bytes),
        )
        .expect("引き取りは成功します");

    assert_eq!(artifact.media_type, ARTIFACT_MEDIA_TYPE);
    assert_eq!(artifact.byte_length, bytes.len() as u64);
    assert_eq!(artifact.sha256, sha256_of(&bytes));
    assert_eq!(
        artifact.expires_at - artifact.created_at,
        TimeDelta::minutes(10)
    );
    assert!(
        Uuid::parse_str(&artifact.artifact_id).is_ok(),
        "artifact_id は UUID である"
    );
    assert_ne!(
        artifact.artifact_id, value,
        "handoff token とは別の値である"
    );

    assert!(!handoff.exists(), "引き取り後に handoff ファイルは残らない");
    let content = fixture
        .store()
        .read(&artifact.artifact_id)
        .expect("登録直後は読み出せます");
    assert_eq!(content.bytes, bytes);
}

#[test]
fn ingest_rejects_a_malformed_token_before_touching_the_filesystem() {
    let fixture = Fixture::new();
    // 検証を通らない値でパスが作られていれば、この名前のファイルが読まれる。
    let dir = fixture
        .base_dir
        .join(HANDOFF_DIR)
        .join(fixture.instance_id.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let decoy = dir.join("DEADBEEF.png");
    std::fs::write(&decoy, b"decoy").unwrap();

    let error = fixture
        .store()
        .ingest(&fixture.instance_id, "DEADBEEF", 5, &sha256_of(b"decoy"))
        .expect_err("書式違反の token では引き取れない");
    assert_eq!(error, IngestError::InvalidToken);
    assert!(
        decoy.exists(),
        "パスを組み立てていないため、別名のファイルには触れない"
    );
    assert!(fixture.store().is_empty());
}

#[test]
fn ingest_discards_the_handoff_when_the_byte_length_disagrees() {
    let fixture = Fixture::new();
    let bytes = b"rendered image".to_vec();
    let value = token(0x22);
    let handoff = fixture.write_handoff(&value, &bytes);

    let error = fixture
        .store()
        .ingest(
            &fixture.instance_id,
            &value,
            bytes.len() as u64 + 1,
            &sha256_of(&bytes),
        )
        .expect_err("長さが申告と食い違えば引き取らない");
    assert_eq!(error, IngestError::ContentMismatch);
    assert!(!handoff.exists(), "失敗時も handoff ファイルは削除される");
    assert!(fixture.store().is_empty(), "artifact は作られない");
}

#[test]
fn ingest_discards_the_handoff_when_the_digest_disagrees() {
    let fixture = Fixture::new();
    let bytes = b"rendered image".to_vec();
    let value = token(0x33);
    let handoff = fixture.write_handoff(&value, &bytes);

    let error = fixture
        .store()
        .ingest(
            &fixture.instance_id,
            &value,
            bytes.len() as u64,
            &sha256_of(b"another image"),
        )
        .expect_err("ダイジェストが申告と食い違えば引き取らない");
    assert_eq!(error, IngestError::ContentMismatch);
    assert!(!handoff.exists());
    assert!(fixture.store().is_empty());
}

#[test]
fn ingest_reports_a_missing_handoff_file() {
    let fixture = Fixture::new();
    let error = fixture
        .store()
        .ingest(&fixture.instance_id, &token(0x44), 0, &sha256_of(b""))
        .expect_err("ファイルが無ければ引き取れない");
    assert_eq!(error, IngestError::Unreadable);
    assert!(fixture.store().is_empty());
}

#[test]
fn ingest_rejects_an_oversized_file_before_reading_it() {
    // 上限のすぐ上ではなく桁違いに大きな長さを取る。上限の直上だと、全体を
    // 読んでから長さを判定する実装でも一瞬で同じ結果になってしまい、判定が
    // 読み込みより前にあることを確かめられない。
    //
    // 実体は書かずスパースファイルの長さだけを伸ばすため、ディスクは
    // 消費しない。読み込んでから判定する実装であれば、この長さの確保に
    // 失敗するか、読み切るだけで下の期限を桁違いに超える。大きさで判定して
    // いれば、読む量は 0 であり時間は長さに依らない。
    const HUGE: u64 = 256 * 1024 * 1024 * 1024;
    /// 実体を読まずに戻ることが分かる余裕。読み込む実装では到底収まらない。
    const BUDGET: Duration = Duration::from_secs(5);

    let fixture = Fixture::new();
    let value = token(0x55);
    let handoff = fixture.write_handoff(&value, b"");
    extend_as_sparse(&handoff, HUGE);
    assert_eq!(
        std::fs::metadata(&handoff).unwrap().len(),
        HUGE,
        "長さだけが伸びている"
    );

    let started = std::time::Instant::now();
    let error = fixture
        .store()
        .ingest(&fixture.instance_id, &value, HUGE, &sha256_of(b""))
        .expect_err("上限を超えるファイルは引き取らない");
    let elapsed = started.elapsed();

    assert_eq!(error, IngestError::TooLarge);
    assert!(
        elapsed < BUDGET,
        "実体を読んでから判定しています: {}ms",
        elapsed.as_millis()
    );
    assert!(!handoff.exists());
    assert!(fixture.store().is_empty());
}

/// ファイルをスパースにしたうえで、長さだけを `length` へ伸ばす。
///
/// スパースにしてから伸ばすため、ディスクの使用量は増えない。
fn extend_as_sparse(path: &Path, length: u64) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::Ioctl::FSCTL_SET_SPARSE;

    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("handoff ファイルを開けます");
    let mut returned = 0u32;
    // SAFETY: `file` は本関数が所有する有効なハンドルであり、入出力バッファを
    // 使わない制御コードのため、渡すのは戻りバイト数の書き込み先だけである。
    unsafe {
        DeviceIoControl(
            HANDLE(file.as_raw_handle()),
            FSCTL_SET_SPARSE,
            None,
            0,
            None,
            0,
            Some(&mut returned),
            None,
        )
    }
    .expect("スパースファイルにできます");
    file.set_len(length).expect("長さを伸ばせます");
}

#[test]
fn ingest_does_not_allocate_for_a_declared_length() {
    // 申告値を信じて確保すると、申告と実体が食い違ったときに過大な確保が起きる。
    // 確保は実体の大きさだけで決まるため、この申告でも失敗するのは照合である。
    let fixture = Fixture::new();
    let value = token(0x66);
    fixture.write_handoff(&value, b"small");

    let error = fixture
        .store()
        .ingest(&fixture.instance_id, &value, u64::MAX, &sha256_of(b"small"))
        .expect_err("申告された長さと実体が食い違う");
    assert_eq!(error, IngestError::ContentMismatch);
}

#[test]
fn ingest_ignores_the_handoff_files_of_other_instances() {
    let fixture = Fixture::new();
    let value = token(0x77);
    let bytes = b"rendered image".to_vec();
    // 別 instance のディレクトリにだけファイルを置く。
    let other = fixture
        .base_dir
        .join(HANDOFF_DIR)
        .join(InstanceId::new_v4().to_string());
    std::fs::create_dir_all(&other).unwrap();
    let path = other.join(format!("{value}.{ARTIFACT_EXTENSION}"));
    std::fs::write(&path, &bytes).unwrap();

    let error = fixture
        .store()
        .ingest(
            &fixture.instance_id,
            &value,
            bytes.len() as u64,
            &sha256_of(&bytes),
        )
        .expect_err("自 instance のディレクトリにしか触れない");
    assert_eq!(error, IngestError::Unreadable);
    assert!(path.exists(), "他 instance のファイルは消さない");
}

// ============================================================================
// 上限と有効期限
// ============================================================================

#[test]
fn the_oldest_artifact_is_dropped_when_the_count_limit_is_exceeded() {
    let fixture = Fixture::new();
    let mut ids = Vec::new();
    for seed in 0..=limits().max_count {
        let artifact = fixture.ingest_valid(&token(seed as u8), format!("image {seed}").as_bytes());
        ids.push(artifact.artifact_id);
        fixture.clock.advance(1);
    }

    assert_eq!(fixture.store().len(), limits().max_count);
    assert!(
        fixture.store().get(&ids[0]).is_none(),
        "最も古い artifact が落ちる"
    );
    assert!(
        fixture.store().get(ids.last().unwrap()).is_some(),
        "新しい artifact は残る"
    );
    let remaining: Vec<_> = fixture
        .store()
        .list()
        .into_iter()
        .map(|artifact| artifact.artifact_id)
        .collect();
    assert_eq!(remaining, ids[1..], "残るのは古い順の並びのままである");
}

#[test]
fn eviction_respects_the_total_byte_limit() {
    // 総量の上限は 128 MiB であり、実ファイルで再現すると試験が重くなる。
    // 判定そのものを直接確かめる。
    let entry = |byte_length: u64| Artifact {
        artifact_id: Uuid::new_v4().to_string(),
        media_type: ARTIFACT_MEDIA_TYPE,
        byte_length,
        sha256: sha256_of(b""),
        created_at: DateTime::from_timestamp(0, 0).unwrap(),
        expires_at: DateTime::from_timestamp(0, 0).unwrap(),
        path: PathBuf::new(),
    };

    let quarter = limits().max_total_bytes / 4;
    let mut entries: Vec<_> = (0..4).map(|_| entry(quarter)).collect();
    let expected_survivors: Vec<_> = entries[1..]
        .iter()
        .map(|artifact| artifact.artifact_id.clone())
        .collect();

    assert!(
        !fits(&entries, 1, limits()),
        "総量の上限を超える追加は許さない"
    );
    make_room(&mut entries, quarter, limits());
    let survivors: Vec<_> = entries
        .iter()
        .map(|artifact| artifact.artifact_id.clone())
        .collect();
    assert_eq!(survivors, expected_survivors, "古い順に落とす");

    // 件数に余裕があっても総量で落ちる。逆に総量に余裕があっても件数で落ちる。
    let mut small: Vec<_> = (0..limits().max_count).map(|_| entry(1)).collect();
    assert!(
        !fits(&small, 1, limits()),
        "件数の上限を超える追加は許さない"
    );
    make_room(&mut small, 1, limits());
    assert_eq!(small.len(), limits().max_count - 1);
}

#[test]
fn an_expired_artifact_is_indistinguishable_from_an_unknown_one() {
    let fixture = Fixture::new();
    let artifact = fixture.ingest_valid(&token(0x88), b"rendered image");

    fixture.clock.advance(limits().ttl.as_secs() as i64);

    // 期限切れと未知の識別子はどちらも同じ `Option` の `None` へ落ちる。
    // 引き当ての戻り値には理由を運ぶ余地が無く、両者を区別する型が存在しない。
    // 区別できると、過去に存在した識別子を総当たりで調べられる。
    let expired: Option<Artifact> = fixture.store().get(&artifact.artifact_id);
    let unknown: Option<Artifact> = fixture.store().get(&Uuid::new_v4().to_string());
    assert!(expired.is_none());
    assert!(unknown.is_none());

    assert!(fixture.store().read(&artifact.artifact_id).is_none());
    assert!(fixture.store().list().is_empty(), "一覧にも現れない");
}

#[test]
fn an_expired_artifact_leaves_no_file_behind() {
    let fixture = Fixture::new();
    let artifact = fixture.ingest_valid(&token(0x99), b"rendered image");
    let path = artifact.path.clone();
    assert!(path.exists());

    fixture.clock.advance(limits().ttl.as_secs() as i64);
    let _ = fixture.store().list();
    assert!(!path.exists(), "期限切れの実体は掃除で消える");
}

#[test]
fn an_artifact_survives_until_its_expiry() {
    let fixture = Fixture::new();
    let artifact = fixture.ingest_valid(&token(0xaa), b"rendered image");

    fixture.clock.advance(limits().ttl.as_secs() as i64 - 1);
    assert!(
        fixture.store().get(&artifact.artifact_id).is_some(),
        "期限前は引き当てられる"
    );
}

// ============================================================================
// 識別子からの引き当て
// ============================================================================

#[test]
fn lookup_never_joins_the_identifier_to_a_path() {
    let fixture = Fixture::new();
    let artifact = fixture.ingest_valid(&token(0xbb), b"rendered image");
    // 引き当てで経路が解釈されるなら、この置き場所のファイルが読める。
    let outside = fixture
        .base_dir
        .join(format!("escape.{ARTIFACT_EXTENSION}"));
    std::fs::write(&outside, b"outside the store").unwrap();

    for identifier in [
        "..",
        "../escape",
        r"..\escape",
        r"..\..\escape",
        &format!("../{}", artifact.artifact_id),
        &format!("{}/..", artifact.artifact_id),
        &format!("{}\0", artifact.artifact_id),
        &format!("{} ", artifact.artifact_id),
        &artifact.artifact_id.to_uppercase(),
        "",
        "escape",
    ] {
        assert!(
            fixture.store().get(identifier).is_none(),
            "一覧に無い識別子は見つからない: {identifier:?}"
        );
        assert!(fixture.store().read(identifier).is_none());
    }

    assert!(outside.exists());
    assert!(
        fixture.store().get(&artifact.artifact_id).is_some(),
        "登録済みの識別子だけが引き当てられる"
    );
}

#[test]
fn artifact_debug_output_carries_no_path() {
    let fixture = Fixture::new();
    let artifact = fixture.ingest_valid(&token(0xcc), b"rendered image");
    let rendered = format!("{artifact:?}");

    assert!(
        !rendered.contains(&fixture.base_dir.display().to_string()),
        "パスが現れています: {rendered}"
    );
    assert!(
        !rendered.contains(&format!("{}.{ARTIFACT_EXTENSION}", artifact.artifact_id)),
        "実体のファイル名が現れています: {rendered}"
    );
    assert!(rendered.contains(&artifact.artifact_id));

    // 実体も記録されない。画像には利用者のプロジェクトの内容が写る。
    let content = fixture.store().read(&artifact.artifact_id).unwrap();
    let rendered = format!("{content:?}");
    assert!(!rendered.contains("rendered image"), "実体が現れています");
}

// ============================================================================
// store ディレクトリの寿命
// ============================================================================

#[test]
fn dropping_the_store_removes_its_directory() {
    let base_dir = temp_base_dir();
    let session_dir = {
        let store =
            ArtifactStore::open(base_dir.clone(), default_settings()).expect("store を開けます");
        let session_dir = store.session_dir.clone();
        assert!(session_dir.is_dir());
        session_dir
    };

    assert!(
        !session_dir.exists(),
        "server の終了時に store ディレクトリごと消える"
    );
    assert!(
        base_dir.join(ARTIFACTS_DIR).is_dir(),
        "親ディレクトリは他の server のために残す"
    );

    let _ = std::fs::remove_dir_all(&base_dir);
}

#[test]
fn an_unprotected_base_dir_stops_the_store_from_opening() {
    // DACL を保証できない場所へ利用者のプロジェクトの内容を書き出すより、
    // 開けないほうがよい。失敗させることと壊さないことを同じ試験で確かめる。
    let base_dir = temp_base_dir();
    std::fs::create_dir_all(&base_dir).unwrap();
    let before = aviutl2_mcp_win::test_support::security_descriptor_bytes(&base_dir);

    let opened = ArtifactStore::open_with(base_dir.clone(), default_settings(), FixedClock::new());
    let Err(error) = opened else {
        panic!("保護されていない基底で store を開けました");
    };
    assert!(
        matches!(error, ArtifactStoreError::DirectoryUnavailable(_)),
        "{error:?}"
    );

    assert_eq!(
        aviutl2_mcp_win::test_support::security_descriptor_bytes(&base_dir),
        before,
        "開けなかった基底の DACL が書き換わりました"
    );
    assert!(
        !base_dir.join(ARTIFACTS_DIR).exists(),
        "開けなかった基底の下にディレクトリが作られました"
    );

    let _ = std::fs::remove_dir_all(&base_dir);
}

#[test]
fn opening_the_store_removes_only_stale_sibling_sessions() {
    let base_dir = temp_base_dir();
    let artifacts_root = base_dir.join(ARTIFACTS_DIR);
    // 先に動いた server が残した状態を再現する。保護は作成時に与えられており、
    // 開き直す側はそれを検証するだけである。
    create_protected_directory(&base_dir).unwrap();
    create_protected_directory(&artifacts_root).unwrap();

    let stale = artifacts_root.join(Uuid::new_v4().to_string());
    let fresh = artifacts_root.join(Uuid::new_v4().to_string());
    for dir in [&stale, &fresh] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("leftover.{ARTIFACT_EXTENSION}")), b"x").unwrap();
    }
    backdate(
        &stale,
        limits().session_stale_after + Duration::from_secs(60),
    );

    let store =
        ArtifactStore::open(base_dir.clone(), default_settings()).expect("store を開けます");
    assert!(!stale.exists(), "十分古い session ディレクトリは削除される");
    assert!(
        fresh.exists(),
        "新しい session ディレクトリは稼働中の server のものであり得るため残す"
    );
    assert!(store.session_dir.is_dir(), "自分の store は削除しない");

    drop(store);
    let _ = std::fs::remove_dir_all(&base_dir);
}

#[test]
fn a_running_store_is_never_swept_however_old_it_looks() {
    // ディレクトリの最終更新時刻は中身が増減したときにしか動かない。しばらく
    // 成果物を作っていない稼働中の store は、いくらでも古く見える。時刻だけで
    // 判定すると、その store が別の server に消される。
    let base_dir = temp_base_dir();
    let running =
        ArtifactStore::open(base_dir.clone(), default_settings()).expect("store を開けます");
    let running_dir = running.session_dir.clone();
    let instance_id = InstanceId::new_v4();
    backdate(&running_dir, limits().session_stale_after * 24);

    let later = ArtifactStore::open(base_dir.clone(), default_settings())
        .expect("2 つ目の store を開けます");
    assert!(
        running_dir.is_dir(),
        "稼働中の store は、どれだけ古く見えても削除されない"
    );

    // 消されていないだけでなく、そのまま使い続けられる。
    let bytes = b"rendered image";
    let value = token(0xd1);
    let dir = base_dir.join(HANDOFF_DIR).join(instance_id.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{value}.{ARTIFACT_EXTENSION}")), bytes).unwrap();
    running
        .ingest(&instance_id, &value, bytes.len() as u64, &sha256_of(bytes))
        .expect("稼働中の store は引き取りを続けられます");

    drop(later);
    drop(running);
    let _ = std::fs::remove_dir_all(&base_dir);
}

#[test]
fn the_store_directory_of_a_running_server_cannot_be_removed() {
    // 掴んだままのロックファイルにより、削除そのものが OS に拒まれる。
    let base_dir = temp_base_dir();
    let store =
        ArtifactStore::open(base_dir.clone(), default_settings()).expect("store を開けます");

    assert!(
        std::fs::remove_dir_all(&store.session_dir).is_err(),
        "稼働中の store は外から削除できない"
    );
    assert!(store.session_dir.is_dir());

    drop(store);
    let _ = std::fs::remove_dir_all(&base_dir);
}

#[test]
fn the_store_recreates_its_directory_when_it_disappears() {
    // ロックは他プロセスからの削除を拒むが、掃除の仕組みが増える・利用者が
    // 手で消すといった経路までは塞げない。消えたまま失敗し続ける恒久的な
    // 故障にしない。
    let fixture = Fixture::new();
    let first = fixture.ingest_valid(&token(0xd2), b"rendered image");

    // ロックごと消す。掴んでいるファイルは残るため、まず手放させる。
    drop(
        fixture
            .store()
            .lock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take(),
    );
    std::fs::remove_dir_all(&fixture.store().session_dir).expect("store を消せます");

    let second = fixture.ingest_valid(&token(0xd3), b"another image");
    assert!(
        fixture.store().session_dir.is_dir(),
        "引き取りの前にディレクトリを作り直す"
    );
    assert_eq!(
        fixture
            .store()
            .read(&second.artifact_id)
            .expect("作り直した store から読み出せます")
            .bytes,
        b"another image",
    );
    // 消えた実体は読み出せない。存在しない artifact として扱う。
    assert!(fixture.store().read(&first.artifact_id).is_none());
}

/// ログにパスも token も現れないことを確かめる。
///
/// ログは不具合の報告に添えて持ち出される。画像には利用者のプロジェクトの
/// 内容が写り、パスはディレクトリ構成を、token は他プロセスのファイル名を
/// 明かす。記録してよいのは artifact_id・byte length・結果コードだけである。
///
/// 成功・失敗・掃除・押し出し・終了の各経路をまとめて通す。ログへ値を渡して
/// いる箇所を目で追うだけでは、どこか 1 行の追加で静かに壊れる。
#[test]
fn logs_carry_neither_paths_nor_tokens() {
    let base_dir = temp_base_dir();
    let clock = FixedClock::new();
    let instance_id = InstanceId::new_v4();
    let tokens: Vec<String> = (0..=(limits().max_count as u8 + 1)).map(token).collect();
    let mut oversized = None;

    let (_, logs) = capture_logs(|| {
        let store = ArtifactStore::open_with(base_dir.clone(), default_settings(), clock.clone())
            .expect("store を開けます");
        let handoff_dir = base_dir.join(HANDOFF_DIR).join(instance_id.to_string());
        std::fs::create_dir_all(&handoff_dir).unwrap();
        let write = |value: &str, bytes: &[u8]| {
            let path = handoff_dir.join(format!("{value}.{ARTIFACT_EXTENSION}"));
            std::fs::write(&path, bytes).unwrap();
            path
        };

        // 成功と、件数の上限による押し出し。
        for value in &tokens {
            let bytes = format!("image {value}").into_bytes();
            write(value, &bytes);
            store
                .ingest(&instance_id, value, bytes.len() as u64, &sha256_of(&bytes))
                .expect("引き取りは成功します");
            clock.advance(1);
        }

        // 照合の失敗、上限超過、ファイル不在、書式違反。
        let mismatched = &tokens[0];
        write(mismatched, b"mismatched");
        let _ = store.ingest(&instance_id, mismatched, 1, &sha256_of(b""));

        let huge = &tokens[1];
        let path = write(huge, b"");
        extend_as_sparse(&path, ARTIFACT_MAX_BYTES + 1);
        oversized = Some(path);
        let _ = store.ingest(&instance_id, huge, ARTIFACT_MAX_BYTES + 1, &sha256_of(b""));

        let _ = store.ingest(&instance_id, &tokens[2], 0, &sha256_of(b""));
        let _ = store.ingest(&instance_id, "not-a-token", 0, &sha256_of(b""));

        // 期限切れの掃除と、終了時の削除。
        clock.advance(limits().ttl.as_secs() as i64);
        let _ = store.list();
        drop(store);
    });

    assert!(
        !logs.is_empty(),
        "何も記録されていなければ検査にならない: {logs:?}"
    );
    assert!(
        !logs.contains(&base_dir.display().to_string()),
        "パスが現れています: {logs}"
    );
    assert!(
        !logs.contains(ARTIFACTS_DIR) && !logs.contains(HANDOFF_DIR),
        "パスの一部が現れています: {logs}"
    );
    assert!(
        !logs.contains(&instance_id.to_string()),
        "instance_id が現れています: {logs}"
    );
    for value in &tokens {
        assert!(!logs.contains(value), "token が現れています: {logs}");
    }

    if let Some(path) = oversized {
        let _ = std::fs::remove_file(path);
    }
    let _ = std::fs::remove_dir_all(&base_dir);
}

/// ディレクトリの最終更新時刻を `age` だけ過去へずらす。
fn backdate(path: &Path, age: Duration) {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::time::{SystemTime, UNIX_EPOCH};
    use windows::Win32::Foundation::{FILETIME, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES, SetFileTime,
    };

    /// 1601-01-01 から 1970-01-01 までの 100 ナノ秒単位の間隔。
    const EPOCH_DIFFERENCE: u64 = 116_444_736_000_000_000;

    // ディレクトリを開くには backup semantics が要る。
    let dir = std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path)
        .expect("ディレクトリを開けます");

    let target = SystemTime::now() - age;
    let intervals = target
        .duration_since(UNIX_EPOCH)
        .expect("1970 年以降の時刻")
        .as_nanos() as u64
        / 100
        + EPOCH_DIFFERENCE;
    let filetime = FILETIME {
        dwLowDateTime: intervals as u32,
        dwHighDateTime: (intervals >> 32) as u32,
    };

    // SAFETY: `dir` は本関数が所有する有効なハンドルであり、`filetime` は
    // スタック上の有効な読み出し元である。
    unsafe { SetFileTime(HANDLE(dir.as_raw_handle()), None, None, Some(&filetime)) }
        .expect("最終更新時刻を設定できます");
}

// ============================================================================
// 設定との結び付き
// ============================================================================

#[test]
fn the_store_limits_come_from_the_shared_settings() {
    // 上限は設定の解決結果そのものである。**store 側に独自の範囲判定は無い。**
    let settings = settings_from(
        r#"{"artifact":{"ttl_seconds":900,"max_count":3,"max_total_bytes":268435456},
            "session":{"stale_after_seconds":1200}}"#,
    );
    let base_dir = temp_base_dir();
    let store = ArtifactStore::open(base_dir.clone(), SettingsSource::fixed(settings.clone()))
        .expect("store を開けます");

    assert_eq!(store.limits(), ArtifactLimits::from_settings(&settings));
    assert_eq!(store.limits().ttl, Duration::from_secs(900));
    assert_eq!(store.limits().max_count, 3);
    assert_eq!(store.limits().max_total_bytes, 256 * 1024 * 1024);
    assert_eq!(
        store.limits().session_stale_after,
        Duration::from_secs(1200)
    );
    assert_ne!(store.limits(), ArtifactLimits::default());

    drop(store);
    let _ = std::fs::remove_dir_all(&base_dir);
}

#[test]
fn a_configured_count_limit_actually_evicts() {
    // 設定した件数の上限が実際に効く。読み込んだだけで使われていなければ
    // ここで落ちる。
    let settings = settings_from(r#"{"artifact":{"max_count":2}}"#);
    let base_dir = temp_base_dir();
    let clock = FixedClock::new();
    let store = ArtifactStore::open_with(
        base_dir.clone(),
        SettingsSource::fixed(settings),
        clock.clone(),
    )
    .expect("store を開けます");
    let instance_id = InstanceId::new_v4();

    let dir = base_dir.join(HANDOFF_DIR).join(instance_id.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let mut ids = Vec::new();
    for seed in 0..3u8 {
        let value = token(seed);
        let bytes = format!("image {seed}").into_bytes();
        std::fs::write(dir.join(format!("{value}.{ARTIFACT_EXTENSION}")), &bytes).unwrap();
        ids.push(
            store
                .ingest(&instance_id, &value, bytes.len() as u64, &sha256_of(&bytes))
                .expect("引き取れます")
                .artifact_id,
        );
        clock.advance(1);
    }

    assert_eq!(store.len(), 2, "設定した件数の上限が効いていません");
    assert!(store.get(&ids[0]).is_none(), "最も古い成果物が残りました");

    drop(store);
    let _ = std::fs::remove_dir_all(&base_dir);
}

#[test]
fn a_total_byte_limit_below_a_single_artifact_cannot_be_configured() {
    // 総量の下限は 1 件分の上限と連動する。下回ると、上限内の成果物が 1 件も
    // 入らない store ができる。
    let settings = settings_from(r#"{"artifact":{"max_total_bytes":1}}"#);
    let limits = ArtifactLimits::from_settings(&settings);
    assert_eq!(limits.max_total_bytes, ARTIFACT_MAX_BYTES);
    assert!(fits(&[], ARTIFACT_MAX_BYTES, limits));
}
