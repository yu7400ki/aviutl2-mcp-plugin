use super::*;
use crate::budget::RequestBudgetKind;

/// テスト用の一時ファイルのパス。同名の衝突を避けるため uuid を挟む。
fn temp_settings_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "aviutl2-mcp-settings-test-{}.json",
        uuid::Uuid::new_v4()
    ))
}

/// `path` へ内容を書く。
fn write_settings(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
}

fn document(text: &str) -> SettingsDocument {
    SettingsDocument::parse(text).unwrap()
}

fn resolve(text: &str) -> (Settings, Vec<SettingsIssue>) {
    document(text).resolve(&Settings::default())
}

#[test]
fn an_empty_file_yields_every_default() {
    let (settings, issues) = resolve("{}");

    assert!(
        issues.is_empty(),
        "既定値の解決で不整合が出ました: {issues:?}"
    );
    assert_eq!(settings, Settings::default());
    assert_eq!(settings.log_level(), None);
    assert_eq!(settings.effective_log_level(), default_log_level());
    assert_eq!(settings.budgets().percent(), DEFAULT_BUDGET_SCALE_PERCENT);
    assert!(settings.disabled_tools().is_empty());
}

#[test]
fn a_file_without_the_new_fields_still_reads_as_defaults() {
    // 旧い形（`schema_version` と `disabled_tools` だけ）のファイル。
    let (settings, issues) = resolve(r#"{"schema_version":1,"disabled_tools":["aviutl2_x"]}"#);

    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(
        settings
            .disabled_tools()
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["aviutl2_x".to_string()]
    );
    assert_eq!(settings.artifact_ttl(), Settings::default().artifact_ttl());
    assert_eq!(settings.budgets(), Settings::default().budgets());
}

#[test]
fn every_field_round_trips_through_the_document() {
    let text = r#"{
        "schema_version": 1,
        "disabled_tools": ["aviutl2_delete_object"],
        "log_level": "debug",
        "budget_scale_percent": 200,
        "artifact": { "ttl_seconds": 900, "max_count": 8, "max_total_bytes": 268435456 },
        "handoff": { "ttl_seconds": 300 },
        "render": { "drain_timeout_ms": 5000 },
        "session": { "stale_after_seconds": 7200 }
    }"#;
    let parsed = document(text);
    let reparsed = SettingsDocument::parse(&parsed.to_json()).unwrap();
    assert_eq!(parsed, reparsed);

    let (settings, issues) = reparsed.resolve(&Settings::default());
    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(settings.log_level(), Some("debug"));
    assert_eq!(settings.budgets().percent(), 200);
    assert_eq!(settings.artifact_ttl(), Duration::from_secs(900));
    assert_eq!(settings.artifact_max_count(), 8);
    assert_eq!(settings.artifact_max_total_bytes(), 256 * 1024 * 1024);
    assert_eq!(settings.handoff_ttl(), Duration::from_secs(300));
    assert_eq!(settings.render_drain_timeout(), Duration::from_millis(5000));
    assert_eq!(settings.session_stale_after(), Duration::from_secs(7200));
}

#[test]
fn unknown_top_level_fields_are_ignored_and_preserved() {
    let text = r#"{"schema_version":1,"future_field":{"a":1},"log_level":"warn"}"#;
    let mut parsed = document(text);
    let (settings, issues) = parsed.resolve(&Settings::default());
    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(settings.log_level(), Some("warn"));

    // 書き戻しても未知の項目は残る。書き手が読み手より新しい build であり得る。
    parsed.apply(&SettingsChange {
        log_level: Some("error".to_string()),
        ..SettingsChange::default()
    });
    let written = parsed.to_json();
    assert!(written.contains("future_field"), "{written}");
    assert!(written.contains("\"error\""), "{written}");
}

#[test]
fn unknown_tool_names_survive_a_change_to_a_different_tool() {
    let mut parsed =
        document(r#"{"disabled_tools":["aviutl2_future_tool","aviutl2_delete_object"]}"#);
    parsed.apply(&SettingsChange {
        tools: BTreeMap::from([("aviutl2_delete_object".to_string(), true)]),
        ..SettingsChange::default()
    });

    let (settings, _) = parsed.resolve(&Settings::default());
    assert_eq!(
        settings
            .disabled_tools()
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["aviutl2_future_tool".to_string()],
        "未知の tool 名が書き戻しで消えました"
    );
}

#[test]
fn out_of_range_values_are_clamped_to_the_bounds() {
    let below = r#"{
        "budget_scale_percent": 1,
        "artifact": { "ttl_seconds": 1, "max_count": 0, "max_total_bytes": 1 },
        "handoff": { "ttl_seconds": 1 },
        "session": { "stale_after_seconds": 1 }
    }"#;
    let (settings, issues) = resolve(below);
    assert_eq!(settings.budgets().percent(), MIN_BUDGET_SCALE_PERCENT);
    assert_eq!(
        settings.artifact_ttl(),
        Duration::from_secs(MIN_ARTIFACT_TTL_SECONDS)
    );
    assert_eq!(
        settings.artifact_max_count(),
        MIN_ARTIFACT_MAX_COUNT as usize
    );
    assert_eq!(
        settings.artifact_max_total_bytes(),
        MIN_ARTIFACT_MAX_TOTAL_BYTES
    );
    assert_eq!(
        settings.handoff_ttl(),
        Duration::from_secs(MIN_HANDOFF_TTL_SECONDS)
    );
    assert_eq!(
        settings.session_stale_after(),
        Duration::from_secs(MIN_SESSION_STALE_AFTER_SECONDS)
    );
    assert_eq!(issues.len(), 6, "丸めが記録されていません: {issues:?}");
    assert!(
        issues
            .iter()
            .all(|issue| matches!(issue.reason, SettingsIssueReason::Clamped { .. })),
        "{issues:?}"
    );

    let above = r#"{
        "budget_scale_percent": 100000,
        "artifact": { "ttl_seconds": 100000, "max_count": 100000, "max_total_bytes": 1099511627776 },
        "handoff": { "ttl_seconds": 100000 },
        "render": { "drain_timeout_ms": 100000 },
        "session": { "stale_after_seconds": 100000 }
    }"#;
    let (settings, issues) = resolve(above);
    assert_eq!(settings.budgets().percent(), MAX_BUDGET_SCALE_PERCENT);
    assert_eq!(
        settings.artifact_ttl(),
        Duration::from_secs(MAX_ARTIFACT_TTL_SECONDS)
    );
    assert_eq!(
        settings.artifact_max_count(),
        MAX_ARTIFACT_MAX_COUNT as usize
    );
    assert_eq!(
        settings.artifact_max_total_bytes(),
        MAX_ARTIFACT_MAX_TOTAL_BYTES
    );
    assert_eq!(
        settings.handoff_ttl(),
        Duration::from_secs(MAX_HANDOFF_TTL_SECONDS)
    );
    assert_eq!(
        settings.render_drain_timeout(),
        Duration::from_millis(MAX_RENDER_DRAIN_TIMEOUT_MS)
    );
    assert_eq!(
        settings.session_stale_after(),
        Duration::from_secs(MAX_SESSION_STALE_AFTER_SECONDS)
    );
    assert_eq!(issues.len(), 7, "{issues:?}");
}

#[test]
fn a_field_with_the_wrong_type_falls_back_alone() {
    // 1 項目のために全体を破損扱いにしない。設定画面から救えなくなる。
    let text = r#"{
        "log_level": 3,
        "budget_scale_percent": "fast",
        "artifact": { "ttl_seconds": "long", "max_count": 4 },
        "disabled_tools": "aviutl2_delete_object"
    }"#;
    let (settings, issues) = resolve(text);

    assert_eq!(settings.log_level(), None);
    assert_eq!(settings.effective_log_level(), default_log_level());
    assert_eq!(settings.budgets().percent(), DEFAULT_BUDGET_SCALE_PERCENT);
    assert_eq!(
        settings.artifact_ttl(),
        Duration::from_secs(DEFAULT_ARTIFACT_TTL_SECONDS)
    );
    assert!(settings.disabled_tools().is_empty());
    // 型が正しい隣の項目は生きている。
    assert_eq!(settings.artifact_max_count(), 4);
    assert_eq!(issues.len(), 4, "{issues:?}");
    assert!(
        issues
            .iter()
            .all(|issue| matches!(issue.reason, SettingsIssueReason::TypeMismatch)),
        "{issues:?}"
    );
}

#[test]
fn a_group_with_the_wrong_type_falls_back_without_taking_the_rest() {
    let (settings, issues) =
        resolve(r#"{"artifact": 5, "session": {"stale_after_seconds": 1200}}"#);

    assert_eq!(
        settings.artifact_ttl(),
        Duration::from_secs(DEFAULT_ARTIFACT_TTL_SECONDS)
    );
    assert_eq!(settings.session_stale_after(), Duration::from_secs(1200));
    // 群の型違いは群につき 1 回だけ記録する。項目の数だけ積むと、同じ 1 行が
    // 3 回 WARN に並ぶ。
    assert_eq!(
        issues,
        vec![SettingsIssue {
            field: "artifact".to_string(),
            reason: SettingsIssueReason::TypeMismatch,
        }],
        "群の型違いが 1 回だけ記録されていません"
    );
}

#[test]
fn an_absent_log_level_is_distinguishable_from_a_written_one() {
    // 未記載のときに何を採るかはビルドで変わる。「書かれていない」と
    // 「`info` と書かれている」を区別できなければ、その選び分けができない。
    let (absent, _) = resolve("{}");
    assert_eq!(absent.log_level(), None);
    assert_eq!(absent.effective_log_level(), default_log_level());

    let (written, _) = resolve(r#"{"log_level":"info"}"#);
    assert_eq!(written.log_level(), Some("info"));
    assert_eq!(written.effective_log_level(), "info");

    // 既定値は 1 つのままである。選び分けているのは未記載のときだけ。
    assert!(matches!(
        default_log_level(),
        DEFAULT_LOG_LEVEL | DEVELOPMENT_LOG_LEVEL
    ));
    assert_ne!(absent, written, "未記載と明示が同じ設定になりました");
}

#[test]
fn an_empty_log_level_is_reported_and_falls_back() {
    let (settings, issues) = resolve(r#"{"log_level":"   "}"#);

    assert_eq!(settings.log_level(), None);
    assert_eq!(settings.effective_log_level(), default_log_level());
    assert_eq!(
        issues,
        vec![SettingsIssue {
            field: "log_level".to_string(),
            reason: SettingsIssueReason::Unparsable,
        }]
    );
}

#[test]
fn the_artifact_total_floor_follows_the_single_artifact_limit() {
    // 総量の下限が 1 件分の上限を下回ると、上限内の成果物が 1 件も入らない
    // store ができる。定数を直接引いていることを固定する。
    assert_eq!(MIN_ARTIFACT_MAX_TOTAL_BYTES, ARTIFACT_MAX_BYTES);

    let text = format!(
        r#"{{"artifact": {{"max_total_bytes": {}}}}}"#,
        ARTIFACT_MAX_BYTES - 1
    );
    let (settings, _) = resolve(&text);
    assert_eq!(settings.artifact_max_total_bytes(), ARTIFACT_MAX_BYTES);
}

#[test]
fn the_handoff_floor_follows_the_scaled_render_budget() {
    // 倍率を上げると引き取りに掛かり得る時間も伸びる。下限は 30 秒と
    // 倍率適用後の描画予算の長い方になる。
    let (settings, _) = resolve(r#"{"budget_scale_percent": 400, "handoff": {"ttl_seconds": 30}}"#);
    let render_budget = settings
        .budgets()
        .server_request_phase(RequestBudgetKind::Render);
    assert_eq!(render_budget, Duration::from_secs(120));
    assert_eq!(settings.handoff_ttl(), render_budget);

    // 倍率を下げても 30 秒は割らない。
    let (settings, _) = resolve(r#"{"budget_scale_percent": 10, "handoff": {"ttl_seconds": 1}}"#);
    assert_eq!(
        settings.handoff_ttl(),
        Duration::from_secs(MIN_HANDOFF_TTL_SECONDS)
    );
}

#[test]
fn fixed_limits_written_into_the_file_have_no_effect() {
    // 固定のまま残す項目は未知の top-level field として読み飛ばされる。
    let text = r#"{
        "artifact_max_bytes": 1,
        "max_render_frame_bytes": 1,
        "max_abandoned_renders": 999,
        "abandoned_entry_ttl_seconds": 1,
        "render_wait_tick_ms": 1
    }"#;
    let (settings, issues) = resolve(text);

    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(settings, Settings::default());
    // 書き戻しても消えないが、解決結果には現れない。
    assert!(document(text).to_json().contains("max_abandoned_renders"));
}

#[test]
fn a_budget_scale_below_the_floor_is_clamped_before_the_inequality_check() {
    // **丸めが先に効くため、不等式の検査へ届くのは範囲内の倍率だけである。**
    // 10〜400 のいずれも不等式を満たすことは `budget.rs` の全数検査が示して
    // おり、したがって「破れたら直前の値を維持する」経路は製品の入力からは
    // 到達しない。到達させるには定数そのものを動かす必要がある。
    //
    // ここで確かめるのは、範囲外の倍率が拒否ではなく丸めとして扱われること
    // である。直前の値への差し戻しは起きない。
    let previous = Settings {
        budgets: crate::budget::ScaledBudgets::checked(200).unwrap(),
        ..Settings::default()
    };
    let below = SettingsDocument {
        fields: Map::from_iter([(FIELD_BUDGET_SCALE_PERCENT.to_string(), Value::from(0))]),
    };

    let (settings, issues) = below.resolve(&previous);

    assert_eq!(settings.budgets().percent(), MIN_BUDGET_SCALE_PERCENT);
    assert_ne!(
        settings.budgets(),
        previous.budgets(),
        "丸めるべき値が直前の値へ差し戻されました"
    );
    assert_eq!(
        issues,
        vec![SettingsIssue {
            field: "budget_scale_percent".to_string(),
            reason: SettingsIssueReason::Clamped {
                requested: 0,
                applied: u64::from(MIN_BUDGET_SCALE_PERCENT),
            },
        }]
    );
}

#[test]
fn the_reader_starts_from_the_defaults_without_touching_the_disk() {
    let reader = SettingsReader::new(temp_settings_path());
    assert_eq!(*reader.settings(), Settings::default());
    assert_eq!(reader.loads(), 0);
}

#[test]
fn a_missing_file_resolves_to_the_defaults() {
    let mut reader = SettingsReader::new(temp_settings_path());
    let refresh = reader.refresh();
    assert!(
        matches!(refresh, SettingsRefresh::Reloaded(_)),
        "{refresh:?}"
    );
    assert_eq!(*reader.settings(), Settings::default());
}

#[test]
fn an_unchanged_stamp_does_not_reparse() {
    let path = temp_settings_path();
    write_settings(&path, r#"{"log_level":"debug"}"#);
    let mut reader = SettingsReader::new(path.clone());

    assert!(matches!(reader.refresh(), SettingsRefresh::Reloaded(_)));
    assert_eq!(reader.loads(), 1);
    for _ in 0..5 {
        assert!(matches!(reader.refresh(), SettingsRefresh::Unchanged));
    }
    assert_eq!(reader.loads(), 1, "印が同じなのに読み直しました");
    assert_eq!(reader.settings().log_level(), Some("debug"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_replaced_file_is_seen_on_the_next_refresh() {
    // 原子的置換はファイルの identity を差し替えるが、毎回パスを見に行く
    // 形は取りこぼさない。
    let path = temp_settings_path();
    write_settings(&path, r#"{"log_level":"debug"}"#);
    let mut reader = SettingsReader::new(path.clone());
    reader.refresh();

    let replacement = path.with_extension("tmp");
    std::fs::write(
        &replacement,
        r#"{"log_level":"trace","artifact":{"max_count":3}}"#,
    )
    .unwrap();
    std::fs::rename(&replacement, &path).unwrap();

    assert!(matches!(reader.refresh(), SettingsRefresh::Reloaded(_)));
    assert_eq!(reader.settings().log_level(), Some("trace"));
    assert_eq!(reader.settings().artifact_max_count(), 3);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn several_writes_between_refreshes_apply_once_as_the_last_state() {
    // 契機と契機の間に何度書き込まれても、読み直しは 1 回であり、見えるのは
    // 最後の状態である。debounce を持たなくても中間の状態は残らない。
    let path = temp_settings_path();
    write_settings(&path, r#"{"log_level":"debug"}"#);
    let mut reader = SettingsReader::new(path.clone());
    reader.refresh();
    let loads = reader.loads();

    write_settings(&path, r#"{"log_level":"warn"}"#);
    write_settings(&path, r#"{"log_level":"error"}"#);
    write_settings(&path, r#"{"log_level":"trace"}"#);

    assert!(matches!(reader.refresh(), SettingsRefresh::Reloaded(_)));
    assert_eq!(
        reader.loads(),
        loads + 1,
        "書き込みの回数だけ読み直しました"
    );
    assert_eq!(reader.settings().log_level(), Some("trace"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_corrupt_file_keeps_the_previous_snapshot() {
    let path = temp_settings_path();
    write_settings(&path, r#"{"log_level":"debug"}"#);
    let mut reader = SettingsReader::new(path.clone());
    reader.refresh();

    write_settings(&path, "{ this is not json");
    let refresh = reader.refresh();

    assert!(matches!(refresh, SettingsRefresh::Failed(_)), "{refresh:?}");
    assert_eq!(
        reader.settings().log_level().unwrap_or_default(),
        "debug",
        "破損で直前の設定が失われました"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_corrupt_file_at_startup_yields_the_defaults() {
    let path = temp_settings_path();
    write_settings(&path, "not json at all");
    let mut reader = SettingsReader::new(path.clone());

    assert!(matches!(reader.refresh(), SettingsRefresh::Failed(_)));
    assert_eq!(*reader.settings(), Settings::default());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn adopting_a_document_takes_effect_without_reading_the_file_again() {
    // 設定画面が保存に成功した直後は、書いた内容をそのまま snapshot にする。
    let path = temp_settings_path();
    write_settings(&path, r#"{"log_level":"debug"}"#);
    let mut reader = SettingsReader::new(path.clone());
    reader.refresh();

    let mut written = document(r#"{"log_level":"debug"}"#);
    written.apply(&SettingsChange {
        log_level: Some("trace".to_string()),
        ..SettingsChange::default()
    });
    std::fs::write(&path, written.to_json()).unwrap();
    reader.adopt(&written);

    assert_eq!(reader.settings().log_level(), Some("trace"));
    // 直後の契機では読み直さない。反映は既に済んでいる。
    assert!(matches!(reader.refresh(), SettingsRefresh::Unchanged));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn two_readers_of_the_same_file_reach_the_same_limits() {
    // plugin と server は別のプロセスで別の読み取り口を持つが、解決の手続きは
    // 1 つしか無い。**片方だけが丸める形を落とす。**
    let path = temp_settings_path();
    write_settings(
        &path,
        r#"{
            "log_level": "warn",
            "budget_scale_percent": 5,
            "artifact": { "ttl_seconds": 100000, "max_count": 0, "max_total_bytes": 1 },
            "handoff": { "ttl_seconds": 1 },
            "render": { "drain_timeout_ms": 999999 },
            "session": { "stale_after_seconds": 1 },
            "disabled_tools": ["aviutl2_delete_object"]
        }"#,
    );

    let mut plugin_side = SettingsReader::new(path.clone());
    let mut server_side = SettingsReader::new(path.clone());
    plugin_side.refresh();
    server_side.refresh();

    let plugin = plugin_side.settings();
    let server = server_side.settings();
    assert_eq!(plugin.log_level(), server.log_level());
    assert_eq!(plugin.budgets(), server.budgets());
    assert_eq!(plugin.disabled_tools(), server.disabled_tools());
    assert_eq!(plugin.artifact_ttl(), server.artifact_ttl());
    assert_eq!(plugin.artifact_max_count(), server.artifact_max_count());
    assert_eq!(
        plugin.artifact_max_total_bytes(),
        server.artifact_max_total_bytes()
    );
    assert_eq!(plugin.handoff_ttl(), server.handoff_ttl());
    assert_eq!(plugin.render_drain_timeout(), server.render_drain_timeout());
    assert_eq!(plugin.session_stale_after(), server.session_stale_after());
    assert_eq!(*plugin, *server);
    // 丸めが実際に効いていること（既定と一致していれば検査になっていない）。
    assert_eq!(plugin.budgets().percent(), MIN_BUDGET_SCALE_PERCENT);
    assert_ne!(*plugin, Settings::default());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_settings_path_follows_the_environment_override_or_the_base_dir() {
    // 環境変数はプロセス全体で共有されるため、上書き時の分岐は値を直接
    // 与えられる形で確かめる。既定側だけをここで固定する。
    let base = Path::new(r"C:\base");
    if std::env::var(SETTINGS_FILE_ENV).is_err() {
        assert_eq!(settings_path(base), base.join(SETTINGS_FILE_NAME));
        let location = settings_location(base);
        assert!(
            !location.overridden,
            "上書きが無いのに上書き扱いになりました"
        );
        assert_eq!(location.path, base.join(SETTINGS_FILE_NAME));
    }
    assert_eq!(SETTINGS_FILE_NAME, "settings.json");
    assert_eq!(SETTINGS_FILE_ENV, "AVIUTL2_MCP_SETTINGS_FILE");
}

#[test]
fn the_schema_version_stays_at_one() {
    // 版を上げるのは既存フィールドの意味が変わるときだけである。
    assert_eq!(SETTINGS_SCHEMA_VERSION, 1);
    assert!(
        SettingsDocument::default()
            .to_json()
            .contains(r#""schema_version": 1"#)
    );
}
