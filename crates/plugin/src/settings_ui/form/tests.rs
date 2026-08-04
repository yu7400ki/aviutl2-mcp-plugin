use super::*;
use aviutl2_mcp_core::settings::{SettingsDocument, SettingsIssue};
use aviutl2_mcp_core::tool::{ALWAYS_ENABLED_TOOL, all_tool_names};
use std::collections::BTreeSet;

/// JSON から設定を解決する。丸めが起きた場合は試験の前提が崩れているため落とす。
fn settings_from(json: &str) -> Settings {
    let document = SettingsDocument::parse(json).expect("設定を解析できません");
    let (settings, issues) = document.resolve(&Settings::default());
    assert_eq!(issues, Vec::<SettingsIssue>::new(), "丸めが起きています");
    settings
}

/// 変更点を重ねた結果の設定。設定画面が保存した後に効く値である。
fn applied(json: &str, change: &SettingsChange) -> (Settings, SettingsDocument) {
    let mut document = SettingsDocument::parse(json).expect("設定を解析できません");
    document.apply(change);
    let (settings, _) = document.resolve(&Settings::default());
    (settings, document)
}

fn find(form: &SettingsForm, setting: NumericSetting) -> &NumericInput {
    form.numbers_in(setting.group())
        .find(|input| input.setting() == setting)
        .expect("項目が画面にありません")
}

/// 並べる tool 名が、切替の対象の全体と一致すること。
///
/// **tool が増えたときに載せ忘れる経路が構造的に無いこと**の確認である。
/// 一覧を書き写す実装にすると、operation を足した時点でここが落ちる。
#[test]
fn the_form_offers_exactly_the_togglable_tools() {
    let form = SettingsForm::new(&Settings::default());

    let offered: BTreeSet<&str> = form.tools().iter().map(ToolToggle::name).collect();
    let expected: BTreeSet<String> = all_tool_names()
        .filter(|name| name != ALWAYS_ENABLED_TOOL)
        .collect();
    let expected: BTreeSet<&str> = expected.iter().map(String::as_str).collect();

    assert_eq!(offered, expected);
    assert_eq!(
        form.tools().len(),
        all_tool_names().count() - 1,
        "重複または欠落があります"
    );
}

/// 常時有効な tool を無効化の候補として提示しないこと。
#[test]
fn the_always_enabled_tool_is_not_offered() {
    let form = SettingsForm::new(&Settings::default());

    assert!(
        form.tools()
            .iter()
            .all(|tool| tool.name() != ALWAYS_ENABLED_TOOL),
        "{ALWAYS_ENABLED_TOOL} が操作対象に現れています"
    );
}

/// 並びが族の導出と一致すること。
#[test]
fn the_tools_are_ordered_by_family() {
    let form = SettingsForm::new(&Settings::default());

    let offered: Vec<&str> = form.tools().iter().map(ToolToggle::name).collect();
    let expected: Vec<String> = ToolFamily::ALL
        .into_iter()
        .flat_map(ToolFamily::tool_names)
        .collect();
    assert_eq!(
        offered,
        expected.iter().map(String::as_str).collect::<Vec<_>>()
    );

    for family in ToolFamily::ALL {
        let in_family: Vec<&str> = form.tools_in(family).map(ToolToggle::name).collect();
        let expected: Vec<String> = family.tool_names().collect();
        assert_eq!(
            in_family,
            expected.iter().map(String::as_str).collect::<Vec<_>>(),
            "{family:?} の並びが導出と一致しません"
        );
    }
}

/// 数値の範囲が共有の定数と一致すること。
///
/// **期待値の書き方を 2 つに分けてある。**
///
/// - 予算倍率だけはリテラルで書く。**定数の側だけを動かせばここが落ちる**ため、
///   画面の範囲が黙って変わらない。期待も実装も同じ定数から導くと、定数を
///   動かしたときに期待も一緒に動いて何も落ちない。
/// - 残りは定数から導く。**画面が定数を引いていること**を確かめたいためであり、
///   範囲を書き写した実装は定数を動かした時点でここが落ちる。
///
/// **同じ値のまま書き写した実装は、どちらの書き方でも捕まえられない**——値が
/// 一致している限り観測できる差が無い。捕まるのは定数が動いたときである。
#[test]
fn the_numeric_ranges_come_from_the_shared_constants() {
    let settings = Settings::default();
    let form = SettingsForm::new(&settings);

    for setting in NumericSetting::ALL {
        let input = find(&form, setting);
        assert_eq!(
            input.control().range_bounds(),
            Some(setting.range(settings.budgets())),
            "{setting:?} の範囲が指定されていません"
        );
    }

    let bounds = |setting: NumericSetting| setting.range(settings.budgets());
    assert_eq!(
        bounds(NumericSetting::BudgetScalePercent),
        (10, 400),
        "予算倍率の範囲が変わりました"
    );
    assert_eq!(
        bounds(NumericSetting::BudgetScalePercent),
        (
            MIN_BUDGET_SCALE_PERCENT as i32,
            MAX_BUDGET_SCALE_PERCENT as i32
        )
    );
    assert_eq!(
        bounds(NumericSetting::RenderDrainTimeoutMs),
        (
            MIN_RENDER_DRAIN_TIMEOUT_MS as i32,
            MAX_RENDER_DRAIN_TIMEOUT_MS as i32
        )
    );
    assert_eq!(
        bounds(NumericSetting::ArtifactTtlSeconds),
        (
            MIN_ARTIFACT_TTL_SECONDS as i32,
            MAX_ARTIFACT_TTL_SECONDS as i32
        )
    );
    assert_eq!(
        bounds(NumericSetting::ArtifactMaxCount),
        (MIN_ARTIFACT_MAX_COUNT as i32, MAX_ARTIFACT_MAX_COUNT as i32)
    );
    assert_eq!(
        bounds(NumericSetting::ArtifactMaxTotalMib),
        (
            MIN_ARTIFACT_MAX_TOTAL_BYTES.div_ceil(BYTES_PER_MIB) as i32,
            (MAX_ARTIFACT_MAX_TOTAL_BYTES / BYTES_PER_MIB) as i32
        )
    );
    assert_eq!(
        bounds(NumericSetting::SessionStaleAfterSeconds),
        (
            MIN_SESSION_STALE_AFTER_SECONDS as i32,
            MAX_SESSION_STALE_AFTER_SECONDS as i32
        )
    );
}

/// 引き渡しの保持時間の下限が、倍率を適用した**描画**の予算と連動すること。
///
/// 解決側と同じ規則であり、画面が解決側より緩い範囲を提示しない。
///
/// **期待値をリテラルで書く。** 同じ関数へ問い合わせて突き合わせると、どの予算
/// に結ばれているかを見ていないことになる——別の区分（一括適用など）へ差し
/// 替えても、それが 30 秒を超える限り通ってしまう。描画の要求フェーズ予算は
/// 30 秒であり、倍率 400% では 120 秒である。
#[test]
fn the_handoff_floor_follows_the_scaled_render_budget() {
    let scaled = settings_from(r#"{"budget_scale_percent":400}"#);

    let (min, max) = NumericSetting::HandoffTtlSeconds.range(scaled.budgets());

    assert_eq!(min, 120, "描画の予算（倍率 400%）と一致しません");
    assert_eq!(max, MAX_HANDOFF_TTL_SECONDS as i32);
    // 倍率が既定なら固定の下限のままである。
    assert_eq!(
        NumericSetting::HandoffTtlSeconds
            .range(Settings::default().budgets())
            .0,
        MIN_HANDOFF_TTL_SECONDS as i32
    );
}

/// 確定時の下限が、**入力済みの**倍率から引き直されること。
///
/// 倍率は同じ画面で変えられる。開いた時点の下限のまま通すと、解決側が丸める値
/// を「入力できた」ことにしてしまう——保存の後に WARN だけが残る。
#[test]
fn confirming_re_derives_the_handoff_floor_from_the_entered_scale() {
    let form = SettingsForm::new(&Settings::default());
    // 既定（100%）での下限は 30 秒であり、この入力は開いた時点では通る。
    find(&form, NumericSetting::HandoffTtlSeconds)
        .control()
        .set_value(MIN_HANDOFF_TTL_SECONDS);
    find(&form, NumericSetting::BudgetScalePercent)
        .control()
        .set_value(400);

    let errors = form.collect().unwrap_err();

    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains(NumericSetting::HandoffTtlSeconds.name()) && errors[0].contains("120"),
        "引き直した下限が伝わりません: {}",
        errors[0]
    );

    // 下限を満たす値なら通り、倍率と一緒に保存される。
    find(&form, NumericSetting::HandoffTtlSeconds)
        .control()
        .set_value(200);
    let change = form.collect().unwrap();
    assert_eq!(change.handoff_ttl_seconds, Some(200));
    assert_eq!(change.budget_scale_percent, Some(400));
}

/// 倍率を下げた場合は固定の下限が効くこと。
#[test]
fn lowering_the_scale_keeps_the_fixed_handoff_floor() {
    let form = SettingsForm::new(&settings_from(r#"{"budget_scale_percent":400}"#));
    find(&form, NumericSetting::BudgetScalePercent)
        .control()
        .set_value(MIN_BUDGET_SCALE_PERCENT);
    find(&form, NumericSetting::HandoffTtlSeconds)
        .control()
        .set_value(MIN_HANDOFF_TTL_SECONDS);

    let change = form.collect().unwrap();

    assert_eq!(change.handoff_ttl_seconds, Some(MIN_HANDOFF_TTL_SECONDS));
}

/// 画面が現在の設定を初期値として映すこと。
#[test]
fn the_form_reflects_the_current_settings() {
    let settings = settings_from(
        r#"{
            "disabled_tools": ["delete_object"],
            "log_level": "warn",
            "budget_scale_percent": 200,
            "artifact": { "ttl_seconds": 900, "max_count": 8, "max_total_bytes": 268435456 },
            "handoff": { "ttl_seconds": 300 },
            "render": { "drain_timeout_ms": 5000 },
            "session": { "stale_after_seconds": 7200 }
        }"#,
    );

    let form = SettingsForm::new(&settings);

    for tool in form.tools() {
        assert_eq!(
            tool.control().is_checked(),
            tool.name() != "delete_object",
            "{} の初期状態が設定と一致しません",
            tool.name()
        );
    }
    assert_eq!(form.log_level().control().selected_text(), "warn");
    let value = |setting| find(&form, setting).control().validate().unwrap();
    assert_eq!(value(NumericSetting::BudgetScalePercent), 200);
    assert_eq!(value(NumericSetting::RenderDrainTimeoutMs), 5000);
    assert_eq!(value(NumericSetting::ArtifactTtlSeconds), 900);
    assert_eq!(value(NumericSetting::ArtifactMaxCount), 8);
    assert_eq!(value(NumericSetting::ArtifactMaxTotalMib), 256);
    assert_eq!(value(NumericSetting::HandoffTtlSeconds), 300);
    assert_eq!(value(NumericSetting::SessionStaleAfterSeconds), 7200);
}

/// 何も触らなければ変更点が 1 つも出ないこと。
///
/// **change set の意味論そのものである。** 全項目を載せる実装にすると、別の
/// プロセスが同時に変えた項目を上書きしてしまう。
#[test]
fn an_untouched_form_produces_no_change() {
    for json in [
        "{}",
        r#"{"log_level":"trace","budget_scale_percent":50,"artifact":{"max_count":3}}"#,
    ] {
        let form = SettingsForm::new(&settings_from(json));
        assert!(
            form.collect().unwrap().is_empty(),
            "触っていない画面が変更点を作りました: {json}"
        );
    }
}

/// 触った項目だけが変更点に載ること。
#[test]
fn only_the_touched_fields_appear_in_the_change() {
    let form = SettingsForm::new(&Settings::default());
    find(&form, NumericSetting::ArtifactMaxCount)
        .control()
        .set_value(5);

    let change = form.collect().unwrap();

    assert_eq!(
        change,
        SettingsChange {
            artifact_max_count: Some(5),
            ..SettingsChange::default()
        }
    );
}

/// 全項目を変えた場合に、値がそのまま設定へ戻ること。
#[test]
fn every_field_round_trips_through_the_change() {
    let form = SettingsForm::new(&Settings::default());

    for tool in form.tools() {
        tool.control().set_checked(false);
    }
    form.log_level().control().set_selected_index(
        form.log_level()
            .items()
            .iter()
            .position(|item| item == "error")
            .unwrap() as i32,
    );
    let set = |setting: NumericSetting, value: i32| find(&form, setting).control().set_value(value);
    set(NumericSetting::BudgetScalePercent, 250);
    set(NumericSetting::RenderDrainTimeoutMs, 1500);
    set(NumericSetting::ArtifactTtlSeconds, 1200);
    set(NumericSetting::ArtifactMaxCount, 32);
    set(NumericSetting::ArtifactMaxTotalMib, 512);
    set(NumericSetting::HandoffTtlSeconds, 600);
    set(NumericSetting::SessionStaleAfterSeconds, 4800);

    let change = form.collect().unwrap();
    let (settings, _) = applied("{}", &change);

    assert_eq!(
        settings.disabled_tools(),
        &togglable_tool_names().collect::<BTreeSet<String>>()
    );
    assert_eq!(settings.log_level(), Some("error"));
    assert_eq!(settings.budgets().percent(), 250);
    assert_eq!(settings.render_drain_timeout().as_millis(), 1500);
    assert_eq!(settings.artifact_ttl().as_secs(), 1200);
    assert_eq!(settings.artifact_max_count(), 32);
    assert_eq!(settings.artifact_max_total_bytes(), 512 * BYTES_PER_MIB);
    assert_eq!(settings.handoff_ttl().as_secs(), 600);
    assert_eq!(settings.session_stale_after().as_secs(), 4800);

    // 戻した設定で開き直すと、同じ画面が同じ値を映す。
    let reopened = SettingsForm::new(&settings);
    assert!(reopened.collect().unwrap().is_empty());
    assert!(reopened.tools().iter().all(|t| !t.control().is_checked()));
}

/// 未知の tool 名が画面を通しても保持されること。
///
/// 既知のカタログ分だけを操作対象とし、未知分は書き戻しで残る。
#[test]
fn unknown_tool_names_survive_the_form() {
    let json = r#"{"disabled_tools":["aviutl2_future_tool","delete_object"]}"#;
    let form = SettingsForm::new(&settings_from(json));

    assert!(
        form.tools()
            .iter()
            .all(|tool| tool.name() != "aviutl2_future_tool"),
        "未知の tool が操作対象に現れています"
    );
    form.tools()
        .iter()
        .find(|tool| tool.name() == "delete_object")
        .unwrap()
        .control()
        .set_checked(true);

    let (settings, document) = applied(json, &form.collect().unwrap());

    assert!(settings.disabled_tools().contains("aviutl2_future_tool"));
    assert!(!settings.disabled_tools().contains("delete_object"));
    assert!(document.to_json().contains("aviutl2_future_tool"));
}

/// 範囲外の入力を拒否し、どの項目かを伝えること。
///
/// 入力欄の範囲指定はスピンボタンとカーソルキーしか縛らないため、直接入力は
/// ここで初めて弾かれる。
#[test]
fn out_of_range_input_is_rejected_with_the_field_name() {
    let form = SettingsForm::new(&Settings::default());
    let input = find(&form, NumericSetting::BudgetScalePercent);
    input
        .control()
        .set_value(i64::from(MAX_BUDGET_SCALE_PERCENT) + 1);

    let errors = form.collect().unwrap_err();

    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].contains(NumericSetting::BudgetScalePercent.name()),
        "どの項目かが伝わりません: {}",
        errors[0]
    );
    assert!(errors[0].contains(&MAX_BUDGET_SCALE_PERCENT.to_string()));
}

/// 整数として読めない入力を拒否すること。
#[test]
fn input_that_is_not_an_integer_is_rejected() {
    let form = SettingsForm::new(&Settings::default());
    find(&form, NumericSetting::ArtifactTtlSeconds)
        .control()
        .set_text("１２３");

    let errors = form.collect().unwrap_err();

    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains(NumericSetting::ArtifactTtlSeconds.name()));
}

/// 検証を通らない項目があるとき、他の項目の変更も保存しないこと。
#[test]
fn nothing_is_collected_while_any_field_is_invalid() {
    let form = SettingsForm::new(&Settings::default());
    find(&form, NumericSetting::ArtifactMaxCount)
        .control()
        .set_value(5);
    find(&form, NumericSetting::ArtifactTtlSeconds)
        .control()
        .set_value(0);

    assert!(form.collect().is_err());
}

/// 手で書かれたログレベルの指定を選択肢に残すこと。
#[test]
fn a_hand_written_log_level_stays_selectable() {
    let form = SettingsForm::new(&settings_from(
        r#"{"log_level":"aviutl2_mcp_plugin=trace"}"#,
    ));

    assert_eq!(
        form.log_level().control().selected_text(),
        "aviutl2_mcp_plugin=trace"
    );
    assert!(form.collect().unwrap().log_level.is_none());
}

/// 未記載のログレベルは実際に効いている水準を映すこと。
#[test]
fn an_unset_log_level_shows_the_effective_level() {
    let settings = Settings::default();
    let form = SettingsForm::new(&settings);

    assert_eq!(
        form.log_level().control().selected_text(),
        settings.effective_log_level()
    );
    assert!(form.collect().unwrap().log_level.is_none());
}

/// 「動作」ページの群がすべて中身を持つこと。
#[test]
fn every_behavior_group_carries_its_fields() {
    let form = SettingsForm::new(&Settings::default());

    assert_eq!(form.numbers_in(BehaviorGroup::Log).count(), 0);
    assert_eq!(form.numbers_in(BehaviorGroup::Timing).count(), 2);
    assert_eq!(form.numbers_in(BehaviorGroup::Retention).count(), 5);
    assert_eq!(
        BehaviorGroup::ALL
            .into_iter()
            .map(|group| form.numbers_in(group).count())
            .sum::<usize>(),
        NumericSetting::ALL.len()
    );
}

/// 見出しが単位と範囲を伝えること。
#[test]
fn the_labels_carry_the_unit_and_the_range() {
    let settings = Settings::default();
    let form = SettingsForm::new(&settings);

    for setting in NumericSetting::ALL {
        let (min, max) = setting.range(settings.budgets());
        let label = find(&form, setting).label();
        assert!(label.contains(setting.name()), "{label}");
        assert!(label.contains(setting.unit()), "{label}");
        assert!(label.contains(&min.to_string()), "{label}");
        assert!(label.contains(&max.to_string()), "{label}");
    }
}
