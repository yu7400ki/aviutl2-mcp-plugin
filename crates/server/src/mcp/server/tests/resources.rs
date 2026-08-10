//! resource の検査。

use super::*;

#[test]
fn resource_uri_for_instances_is_recognized() {
    assert_eq!(
        parse_resource_uri(INSTANCES_RESOURCE_URI),
        Some(ResourceTarget::Instances)
    );
}

#[test]
fn edit_info_resource_uri_round_trips() {
    let id = InstanceId::new_v4();
    let uri = edit_info_resource_uri(&id);
    assert_eq!(parse_resource_uri(&uri), Some(ResourceTarget::EditInfo(id)));
}

#[test]
fn unknown_resource_uri_is_rejected() {
    for uri in [
        "aviutl2://instances/not-a-uuid/edit-info",
        "aviutl2://instances//edit-info",
        "file:///etc/passwd",
        "aviutl2://instances/8df98c04-e7c2-4f98-b3ce-fc1c39d76414",
        // 識別子の無い成果物 URI は指す対象を持たない。
        "aviutl2://artifacts/",
        "aviutl2://artifacts",
    ] {
        assert_eq!(parse_resource_uri(uri), None, "{uri} を受理しています");
    }
}

#[test]
fn an_artifact_uri_is_resolved_by_lookup_alone() {
    // 識別子はパスへ連結しない。どのような文字列が来ても、引き当てに
    // 失敗すれば見つからないで終わる。書式を課す必要が無い。
    for id in [
        "5d0b6f7a-1f2e-4a3b-9c8d-7e6f5a4b3c2d",
        "..",
        "../../windows/system32/config/sam",
        r"..\..\secret.png",
        "a b c",
        "%2e%2e",
    ] {
        assert_eq!(
            parse_resource_uri(&artifact_resource_uri(id)),
            Some(ResourceTarget::Artifact(id.to_string())),
            "{id} を引き当ての対象として扱っていません"
        );
    }
}

/// 任意の時刻を指す時計。
struct FixedClock(std::sync::atomic::AtomicI64);

impl crate::artifact::ArtifactClock for FixedClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(self.0.load(std::sync::atomic::Ordering::SeqCst), 0)
            .expect("表現できる時刻")
    }
}

/// 成果物を持つ保管庫と、その基底を束ねた試験環境。
struct StoreFixture {
    base_dir: PathBuf,
    clock: Arc<FixedClock>,
    /// 後始末で基底を消す前に閉じる必要があるため、取り出せる形で持つ。
    store: Option<ArtifactStore>,
    instance_id: InstanceId,
}

impl StoreFixture {
    fn open(ttl: Duration) -> Self {
        let base_dir = std::env::temp_dir().join(format!(
            "aviutl2-mcp-resource-test-{}",
            uuid::Uuid::new_v4()
        ));
        let clock = Arc::new(FixedClock(std::sync::atomic::AtomicI64::new(0)));
        let settings = SettingsSource::fixed(settings_with_artifact_ttl(ttl));
        let store = ArtifactStore::open_with(base_dir.clone(), settings, clock.clone())
            .expect("保管庫を開ける");
        Self {
            base_dir,
            clock,
            store: Some(store),
            instance_id: InstanceId::new_v4(),
        }
    }

    fn store(&self) -> &ArtifactStore {
        self.store.as_ref().expect("保管庫は後始末まで生きています")
    }

    /// 引き渡しファイルを書いて成果物として引き取る。
    fn ingest(&self, token: &str, bytes: &[u8]) -> Artifact {
        let dir = self
            .base_dir
            .join("render")
            .join(self.instance_id.to_string());
        std::fs::create_dir_all(&dir).expect("引き渡しディレクトリを作れる");
        std::fs::write(dir.join(format!("{token}.png")), bytes).expect("引き渡しファイルを書ける");

        let mut sha256 = "sha256:".to_string();
        for byte in <sha2::Sha256 as sha2::Digest>::digest(bytes) {
            sha256.push_str(&format!("{byte:02x}"));
        }
        self.store()
            .ingest(&self.instance_id, token, bytes.len() as u64, &sha256)
            .expect("申告と一致する引き渡しは引き取れます")
    }

    fn advance(&self, seconds: i64) {
        self.clock
            .0
            .fetch_add(seconds, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for StoreFixture {
    fn drop(&mut self) {
        drop(self.store.take());
        let _ = std::fs::remove_dir_all(&self.base_dir);
    }
}

/// 有効な引き渡しの識別子を種から作る。
fn handoff_token(seed: u8) -> String {
    format!("{seed:02x}").repeat(16)
}

#[test]
fn a_listing_without_artifacts_keeps_the_shape_it_had_before() {
    // 成果物を持たない場合、並びも cursor も成果物を導入する前と変わらない。
    let registered: Vec<InstanceId> = (0..RESOURCES_PAGE_SIZE + 5)
        .map(|_| InstanceId::new_v4())
        .collect();

    let (first, cursor) = resource_page(&registered, &[], 0);
    // 先頭ページはインスタンス一覧そのものを含む。
    assert_eq!(first.len(), RESOURCES_PAGE_SIZE + 1);
    assert_eq!(first[0].uri, INSTANCES_RESOURCE_URI);
    assert_eq!(
        first[1].uri,
        edit_info_resource_uri(&registered[0]),
        "instance 由来の項目が先に来ていません"
    );
    assert_eq!(cursor.as_deref(), Some("100"));

    let (second, cursor) = resource_page(&registered, &[], 100);
    assert_eq!(second.len(), 5);
    assert!(
        second.iter().all(|item| item.uri != INSTANCES_RESOURCE_URI),
        "2 ページ目に一覧そのものが現れています"
    );
    assert_eq!(cursor, None);

    // 範囲外の位置は空のページになる。
    let (empty, cursor) = resource_page(&registered, &[], 1_000);
    assert!(empty.is_empty());
    assert_eq!(cursor, None);
}

#[test]
fn artifacts_are_listed_after_the_instances() {
    let fixture = StoreFixture::open(Duration::from_secs(600));
    let first = fixture.ingest(&handoff_token(1), b"first");
    let second = fixture.ingest(&handoff_token(2), b"second");
    let registered = vec![InstanceId::new_v4()];

    let artifacts = fixture.store().list();
    let (page, cursor) = resource_page(&registered, &artifacts, 0);
    assert_eq!(cursor, None);
    let uris: Vec<&str> = page.iter().map(|item| item.uri.as_str()).collect();
    assert_eq!(
        uris,
        vec![
            INSTANCES_RESOURCE_URI,
            &edit_info_resource_uri(&registered[0]),
            &artifact_resource_uri(&first.artifact_id),
            &artifact_resource_uri(&second.artifact_id),
        ],
    );

    // cursor は連結した一覧への位置であり、成果物までまたぐ。
    let (page, cursor) = resource_page(&registered, &artifacts, 1);
    assert_eq!(cursor, None);
    assert_eq!(
        page.iter()
            .map(|item| item.uri.as_str())
            .collect::<Vec<_>>(),
        vec![
            artifact_resource_uri(&first.artifact_id).as_str(),
            artifact_resource_uri(&second.artifact_id).as_str(),
        ],
    );
}

#[test]
fn an_artifact_listing_says_nothing_about_what_the_image_shows() {
    let fixture = StoreFixture::open(Duration::from_secs(600));
    let artifact = fixture.ingest(&handoff_token(3), b"image");
    let listed = artifact_resource(&artifact);

    assert_eq!(listed.mime_type.as_deref(), Some("image/png"));
    assert!(
        listed.name.contains(&artifact.artifact_id),
        "{}",
        listed.name
    );
    let description = listed.description.clone().expect("説明がある");
    assert!(
        description.contains(&artifact.created_at.to_rfc3339()),
        "{description}"
    );
    assert!(
        description.contains(&artifact.expires_at.to_rfc3339()),
        "{description}"
    );
    // 引き当てに要らない値を漏らさない。
    for forbidden in [artifact.sha256.as_str(), "render", "png\\"] {
        assert!(
            !description.contains(forbidden),
            "{forbidden} が説明にあります: {description}"
        );
    }
}

#[test]
fn an_expired_artifact_and_an_unknown_id_are_both_simply_missing() {
    // 区別すると、過去に存在した識別子を総当たりで調べられる。
    let fixture = StoreFixture::open(Duration::from_secs(60));
    let artifact = fixture.ingest(&handoff_token(4), b"image");
    assert!(fixture.store().read(&artifact.artifact_id).is_some());

    fixture.advance(61);
    assert!(fixture.store().read(&artifact.artifact_id).is_none());
    assert!(fixture.store().read("unknown-artifact").is_none());
    assert!(fixture.store().list().is_empty(), "期限切れが残っています");

    // どちらも同じ失敗として返る。
    let error = to_mcp_error(&artifact_not_found());
    assert_eq!(error.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
}

#[test]
fn artifact_bytes_are_encoded_as_standard_base64() {
    assert_eq!(encode_base64(b""), "");
    assert_eq!(encode_base64(b"\x89PNG\r\n\x1a\n"), "iVBORw0KGgo=");
}

#[test]
fn correlation_ids_are_unique_uuids() {
    let first = new_correlation_id();
    let second = new_correlation_id();
    assert_ne!(first, second);
    assert_eq!(first.len(), 36);
}

#[test]
fn instance_not_found_becomes_resource_not_found() {
    let error = failure::from_resolve_error(&crate::discovery::ResolveInstanceError::NotRegistered);
    let mcp_error = to_mcp_error(&error);
    assert_eq!(mcp_error.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
}

#[test]
fn stale_instance_becomes_resource_not_found() {
    // 一覧を取り直せば解消し得るため、恒久的な内部エラーにはしない。
    let error = failure::from_resolve_error(&crate::discovery::ResolveInstanceError::Excluded(
        crate::discovery::ExclusionReason::PipeUnreachable,
    ));
    let mcp_error = to_mcp_error(&error);
    assert_eq!(mcp_error.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
}

#[test]
fn invalid_argument_becomes_invalid_params() {
    let mcp_error = to_mcp_error(&failure::invalid_argument("limit が範囲外です"));
    assert_eq!(mcp_error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn transient_failures_become_resource_not_found() {
    // 待てば取得し得る失敗を server の不具合と読ませない。
    for code in [
        ErrorCode::HostBusy,
        ErrorCode::EditBlocked,
        ErrorCode::Timeout,
    ] {
        let error = failure::from_code(code, "失敗");
        assert!(error.retryable, "{code}");
        let mcp_error = to_mcp_error(&error);
        assert_eq!(
            mcp_error.code,
            rmcp::model::ErrorCode::RESOURCE_NOT_FOUND,
            "{code}"
        );
        assert_eq!(
            mcp_error.data.expect("data がある")["retryable"],
            serde_json::json!(true),
            "{code}"
        );
    }
}

#[test]
fn other_errors_become_internal_error() {
    for code in [
        ErrorCode::SdkError,
        ErrorCode::InternalError,
        ErrorCode::UnsupportedOperation,
        ErrorCode::AuthenticationFailed,
    ] {
        let mcp_error = to_mcp_error(&failure::from_code(code, "失敗"));
        assert_eq!(
            mcp_error.code,
            rmcp::model::ErrorCode::INTERNAL_ERROR,
            "{code}"
        );
    }
}

#[test]
fn mcp_error_carries_structured_details() {
    let remote = aviutl2_mcp_core::ErrorObject::new(ErrorCode::HostBusy, "起動処理中です", true)
        .with_details(serde_json::json!({ "retry_after_ms": 500 }));
    let error = failure::with_correlation_id(
        failure::from_pipe_error(
            &crate::pipe_client::PipeClientError::Remote(Box::new(remote)),
            aviutl2_mcp_core::OPERATION_MOVE_OBJECT,
        ),
        "correlation",
    );
    let data = to_mcp_error(&error).data.expect("data がある");
    assert_eq!(data["code"], serde_json::json!("host_busy"));
    assert_eq!(data["retryable"], serde_json::json!(true));
    assert_eq!(data["details"]["retry_after_ms"], serde_json::json!(500));
    assert_eq!(data["correlation_id"], serde_json::json!("correlation"));
}

#[test]
fn cursor_round_trips() {
    assert_eq!(decode_cursor(None).expect("未指定は先頭"), 0);
    assert_eq!(
        decode_cursor(Some(&encode_cursor(100))).expect("符号化した位置を戻せる"),
        100
    );
}

#[test]
fn malformed_cursor_is_rejected() {
    for cursor in ["", "-1", "abc", "1.5"] {
        let error = decode_cursor(Some(cursor)).expect_err("解釈できない cursor は拒否する");
        assert_eq!(
            error.code,
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "{cursor}"
        );
    }
}

/// 表示名が長いインスタンスを指定件数だけ並べた一覧。
fn oversized_instances(count: usize) -> ListInstancesResponse {
    let instances: Vec<aviutl2_mcp_core::InstanceInfo> = (0..count)
        .map(|_| aviutl2_mcp_core::InstanceInfo {
            instance_id: InstanceId::new_v4(),
            state: aviutl2_mcp_core::InstanceState::Ready,
            pid: 1234,
            started_at: "2026-01-01T00:00:00.0000000Z".to_string(),
            project: aviutl2_mcp_core::InstanceProject {
                display_name: Some("名".repeat(500)),
                path: None,
                epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
                revision: 0,
                modified: false,
            },
        })
        .collect();
    ListInstancesResponse {
        total_count: instances.len() as u32,
        count: instances.len() as u32,
        instances,
        offset: 0,
        has_more: false,
        next_offset: None,
    }
}

#[test]
fn instances_resource_shrinks_until_it_fits() {
    let response = oversized_instances(MAX_PAGE_LIMIT as usize);
    let value = fitted_instances_value(response).expect("値へ変換できる");
    let text = pretty_json(&value).expect("直列化できる");
    assert!(
        text.chars().count() <= MAX_TEXT_CHARS,
        "上限を超えています: {}",
        text.chars().count()
    );
    // 落とした分は続きとして示され、黙って欠落しない。
    assert_eq!(value["has_more"], serde_json::json!(true));
    assert!(value["next_offset"].is_number());
    assert!(
        value["count"].as_u64().expect("count は数値") < MAX_PAGE_LIMIT as u64,
        "件数が絞られていません"
    );
}

#[test]
fn small_instances_resource_is_not_shrunk() {
    let response = oversized_instances(1);
    let value = fitted_instances_value(response).expect("値へ変換できる");
    assert_eq!(value["count"], serde_json::json!(1));
    assert_eq!(value["has_more"], serde_json::json!(false));
}

#[test]
fn resource_text_stays_within_limit() {
    let value = serde_json::json!({ "note": "え".repeat(MAX_TEXT_CHARS * 2) });
    let text = resource_text(&value).expect("代替内容を返せる");
    assert!(
        text.chars().count() <= MAX_TEXT_CHARS,
        "上限を超えています: {}",
        text.chars().count()
    );
    // 途中で切らず、読み取れる JSON のまま超過を伝える。
    let decoded: Value = serde_json::from_str(&text).expect("JSON として読める");
    assert_eq!(decoded["truncated"], serde_json::json!(true));
}

#[test]
fn resource_text_keeps_content_within_limit_intact() {
    let value = serde_json::json!({ "note": "短い" });
    let text = resource_text(&value).expect("直列化できる");
    let decoded: Value = serde_json::from_str(&text).expect("JSON として読める");
    assert_eq!(decoded, value);
}
