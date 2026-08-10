//! 実行予算の検査。

use super::*;

#[test]
fn default_limits_come_from_the_shared_budget() {
    // 既定値を接続先と共有する配分から外すと、接続先が自身の上限まで使った
    // 段の途中で予算が尽き、応答しているインスタンスが期限超過になる。
    let unscaled = ScaledBudgets::unscaled();
    let limits = CallLimits::default();
    assert_eq!(
        limits.request,
        unscaled.server_request_phase(RequestBudgetKind::Read)
    );
    assert_eq!(
        limits.edit_request,
        unscaled.server_request_phase(RequestBudgetKind::Edit)
    );
    assert_eq!(
        limits.batch_request,
        unscaled.server_request_phase(RequestBudgetKind::Batch)
    );
    assert_eq!(
        limits.render_request,
        unscaled.server_request_phase(RequestBudgetKind::Render)
    );
    assert_eq!(limits.artifact_ingest, unscaled.server_artifact_ingest());
    assert_eq!(
        DiscoveryConfig::default(),
        DiscoveryConfig::from_budgets(unscaled)
    );
}

#[test]
fn the_discovery_config_follows_the_shared_settings() {
    // 解決フェーズの配分を倍率へ連動させないと、期限だけが縮んだ組が
    // discovery へ渡り、接続を 1 度も試みないまま到達不能として扱われる。
    let settings = settings_with_scale(10);
    let server = AviUtl2McpServer::from_settings_or_fixed(
        PathBuf::from("registry"),
        SettingsOrFixed::Settings(SettingsSource::fixed(settings.clone())),
    );

    let budgets = server.call_budgets();
    assert_eq!(
        budgets.discovery,
        DiscoveryConfig::from_budgets(settings.budgets())
    );
    assert_ne!(budgets.discovery, DiscoveryConfig::default());
    // 要求フェーズと解決フェーズは同じ snapshot から導く。
    assert_eq!(budgets.limits, CallLimits::from_budgets(settings.budgets()));
}

#[test]
fn the_limits_follow_the_shared_settings_without_a_second_judgement() {
    // 倍率の採否は core が不等式ごと決める。server 側で範囲を判定し直すと、
    // plugin と server が同じファイルから別の結論を得る形ができる。
    let settings = settings_with_scale(50);
    let source = SettingsSource::fixed(settings.clone());
    let server = AviUtl2McpServer::from_settings_or_fixed(
        PathBuf::from("registry"),
        SettingsOrFixed::Settings(source),
    );

    let budgets = settings.budgets();
    assert_eq!(server.limits(), CallLimits::from_budgets(budgets));
    assert_eq!(
        server.limits().render_request,
        budgets.server_request_phase(RequestBudgetKind::Render)
    );
    assert_ne!(server.limits(), CallLimits::default());
}

/// 倍率を適用した設定を作る。
fn settings_with_scale(percent: u64) -> aviutl2_mcp_core::settings::Settings {
    settings_from(&format!(r#"{{"budget_scale_percent":{percent}}}"#))
}

#[test]
fn call_limits_can_be_overridden() {
    let limits = CallLimits {
        request: Duration::from_millis(340),
        edit_request: Duration::from_millis(560),
        batch_request: Duration::from_millis(780),
        render_request: Duration::from_millis(910),
        artifact_ingest: Duration::from_millis(130),
    };
    let server = AviUtl2McpServer::without_artifact_store(PathBuf::from("registry"), limits);
    assert_eq!(server.limits().request, Duration::from_millis(340));
    assert_eq!(server.limits().edit_request, Duration::from_millis(560));
    assert_eq!(server.limits().batch_request, Duration::from_millis(780));
    assert_eq!(server.limits().render_request, Duration::from_millis(910));
    assert_eq!(server.limits().artifact_ingest, Duration::from_millis(130));
}

/// 区分ごとの取り違えが必ず落ちるよう、桁で離した予算。
fn probe_limits() -> CallLimits {
    CallLimits {
        request: Duration::from_millis(2),
        edit_request: Duration::from_millis(3),
        batch_request: Duration::from_millis(4),
        render_request: Duration::from_millis(50),
        artifact_ingest: Duration::from_millis(6),
    }
}

#[test]
fn request_budget_selects_the_limit_matching_the_operation_kind() {
    let limits = probe_limits();

    for name in aviutl2_mcp_core::ReadOperation::ALL
        .into_iter()
        .map(aviutl2_mcp_core::ReadOperation::as_str)
        .chain(["ping", "future_operation"])
    {
        assert_eq!(
            limits.request_phase_budget(name),
            limits.request,
            "{name} が read 予算を使っていません"
        );
    }

    for op in aviutl2_mcp_core::EditOperation::ALL {
        use aviutl2_mcp_core::EditOperation as Edit;
        // 一括適用は編集の族に属するが、費用の主項が違うため別の予算を持つ。
        //
        // **`_` を使わない網羅 match である。** `_` で受けると、新しい編集
        // operation が黙って編集予算へ落ち、「誤って分類した」を捕まえられない。
        let expected = match op {
            Edit::ApplyBatch => limits.batch_request,
            Edit::CreateObject
            | Edit::MoveObject
            | Edit::DeleteObject
            | Edit::SetObjectName
            | Edit::SetObjectItem
            | Edit::AddEffect
            | Edit::DeleteEffect
            | Edit::SetEffectEnabled
            | Edit::MoveEffect
            | Edit::SetLayerState
            | Edit::SetSelection
            | Edit::CreateObjectSection
            | Edit::DeleteObjectSection
            | Edit::MoveObjectSection
            | Edit::SetGridBpm
            | Edit::SetSceneSettings => limits.edit_request,
        };
        assert_eq!(
            limits.request_phase_budget(op.as_str()),
            expected,
            "{op:?} の予算が想定と異なります"
        );
    }

    for op in aviutl2_mcp_core::RenderOperation::ALL {
        assert_eq!(
            limits.request_phase_budget(op.as_str()),
            limits.render_request,
            "{op:?} が render 予算を使っていません"
        );
    }
}

#[test]
fn only_the_render_request_reserves_time_for_what_happens_after_the_response() {
    // 描画だけが応答を受けたあとに成果物の引き取りを行う。要求フェーズの
    // 予算をそのまま IPC へ渡すと、接続先が期限いっぱいまで使った直後に
    // 引き取りが始まり、どの層の期限にも捕まらないまま予算を超える。
    let limits = probe_limits();

    for op in aviutl2_mcp_core::RenderOperation::ALL {
        let name = op.as_str();
        assert_eq!(
            limits.ipc_request_budget(name),
            limits.render_request - limits.artifact_ingest,
            "{name} の期限が引き取りの取り分を残していません"
        );
        assert_ne!(
            limits.ipc_request_budget(name),
            limits.render_request,
            "{name} が要求フェーズの予算をそのまま渡しています"
        );
    }

    // 他の operation は応答後の段を持たないため、要求フェーズの予算がその
    // まま期限になる。
    for name in aviutl2_mcp_core::ReadOperation::ALL
        .into_iter()
        .map(aviutl2_mcp_core::ReadOperation::as_str)
        .map(str::to_string)
        .chain(
            aviutl2_mcp_core::EditOperation::ALL
                .into_iter()
                .map(|op| op.as_str().to_string()),
        )
        .chain(["ping".to_string(), "future_operation".to_string()])
    {
        assert_eq!(
            limits.ipc_request_budget(&name),
            limits.request_phase_budget(&name),
            "{name} が予算から時間を差し引いています"
        );
    }
}
