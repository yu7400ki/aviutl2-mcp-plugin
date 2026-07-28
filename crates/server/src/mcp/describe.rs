//! read tool の text content を組み立てる。
//!
//! 主要結果・ID・revision・次に必要な操作だけを短く示す。完全な機械可読値は
//! `structuredContent` が運ぶため、ここでは列挙の全項目を書き出さない。

use crate::api::ListInstancesResponse;
use crate::mcp::summary::{TextBuilder, clamp_chars};
use aviutl2_mcp_core::{
    EditInfo, GetCurrentSceneResult, InstanceInfo, ListAvailableEffectsResult, ListLayersResult,
    ListObjectsResult, ObjectDetail, PageMeta,
};

/// 名前をそのまま行に載せるときの最大文字数。
const MAX_NAME_CHARS: usize = 60;

/// `aviutl2_list_instances` の text content。
pub fn instances(response: &ListInstancesResponse) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!(
        "生存確認済みインスタンス {} 件中 {} 件（offset={}{}）",
        response.total_count,
        response.count,
        response.offset,
        next_offset_hint(response.has_more, response.next_offset),
    ));
    for info in &response.instances {
        text.push_line(instance_line(info));
    }
    if response.instances.is_empty() {
        text.push_line("インスタンスが見つかりません。AviUtl2 が起動しているか確認してください");
    } else {
        text.push_line("以降の tool には instance_id を必ず指定します");
    }
    text.finish()
}

fn instance_line(info: &InstanceInfo) -> String {
    let project = info
        .project
        .as_ref()
        .map(|p| clamp_chars(&p.display_name, MAX_NAME_CHARS))
        .unwrap_or_else(|| "-".to_string());
    let scene = info
        .scene
        .as_ref()
        .map(|s| format!("scene_id={}", s.id))
        .unwrap_or_else(|| "scene=未取得".to_string());
    format!(
        "- {} state={} pid={} project={project} {scene}",
        info.instance_id,
        info.state.as_snake_case(),
        info.pid,
    )
}

/// `aviutl2_get_edit_info` の text content。
pub fn edit_info(info: &EditInfo) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!(
        "scene_id={} name={} {}x{} fps={}/{} sample_rate={}",
        info.scene.id,
        optional_name(info.scene.name.as_deref()),
        info.scene.width,
        info.scene.height,
        info.scene.fps_rate,
        info.scene.fps_scale,
        info.scene.sample_rate,
    ));
    text.push_line(format!(
        "cursor frame={} layer={}（frame / layer は 0 始まり）",
        info.cursor.frame, info.cursor.layer,
    ));
    text.push_line(format!(
        "オブジェクト存在範囲 frame_max={} layer_max={}",
        info.extent.frame_max, info.extent.layer_max,
    ));
    text.push_line(match &info.selected_range {
        Some(range) => format!("選択範囲 frame {}..{}", range.start, range.end),
        None => "選択範囲なし".to_string(),
    });
    text.push_line(format!(
        "project_epoch={} project_revision={}",
        info.project_epoch, info.project_revision,
    ));
    text.finish()
}

/// `aviutl2_get_current_scene` の text content。
pub fn current_scene(result: &GetCurrentSceneResult) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!(
        "scene_id={} name={} {}x{} fps={}/{} sample_rate={}",
        result.scene.id,
        optional_name(result.scene.name.as_deref()),
        result.scene.width,
        result.scene.height,
        result.scene.fps_rate,
        result.scene.fps_scale,
        result.scene.sample_rate,
    ));
    text.push_line(format!("project_revision={}", result.project_revision));
    text.push_line(
        "aviutl2_list_layers / aviutl2_list_objects には expected_scene_id にこの scene_id を指定します",
    );
    text.finish()
}

/// `aviutl2_list_layers` の text content。
pub fn layers(result: &ListLayersResult) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!("レイヤー {}", page_line(&result.page)));
    for layer in &result.items {
        text.push_line(format!(
            "- layer={} name={} enabled={} locked={} objects={}",
            layer.index,
            optional_name(layer.name.as_deref()),
            layer.enabled,
            layer.locked,
            layer.object_count,
        ));
    }
    text.push_line("layer は 0 始まりです");
    text.finish()
}

/// `aviutl2_list_objects` の text content。
pub fn objects(result: &ListObjectsResult) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!("オブジェクト {}", page_line(&result.page)));
    for object in &result.items {
        text.push_line(format!(
            "- layer={} frame={}..{} name={}",
            object.layer,
            object.frame_start,
            object.frame_end,
            optional_name(object.name.as_deref()),
        ));
    }
    text.push_line(
        "frame / layer は 0 始まりです。詳細は aviutl2_get_object に structuredContent の selector をそのまま渡します",
    );
    text.finish()
}

/// `aviutl2_get_object` の text content。
pub fn object_detail(detail: &ObjectDetail) -> String {
    let mut text = TextBuilder::new();
    let summary = &detail.summary;
    text.push_line(format!(
        "layer={} frame={}..{} name={}（frame / layer は 0 始まり）",
        summary.layer,
        summary.frame_start,
        summary.frame_end,
        optional_name(summary.name.as_deref()),
    ));
    text.push_line(format!(
        "中間点区間 {} 件、effect {} 件、project_revision={}",
        detail.sections.len(),
        detail.effects.len(),
        detail.project_revision,
    ));
    for effect in &detail.effects {
        text.push_line(format!(
            "- effect {}:{} enabled={} locked={} items={}",
            clamp_chars(&effect.name, MAX_NAME_CHARS),
            effect.index,
            effect.enabled,
            effect.locked,
            effect.items.len(),
        ));
    }
    text.push_line("設定値と selector は structuredContent を参照してください");
    text.finish()
}

/// `aviutl2_list_available_effects` の text content。
pub fn available_effects(result: &ListAvailableEffectsResult) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!(
        "利用可能 effect {}",
        catalog_page_line(&result.page)
    ));
    for effect in &result.items {
        text.push_line(format!(
            "- {} type={} items={}",
            clamp_chars(&effect.name, MAX_NAME_CHARS),
            effect_type_label(&effect.effect_type),
            effect.items.len(),
        ));
    }
    text.push_line(
        "effect_type を指定すると種別で絞り込めます。設定項目の定義は structuredContent を参照してください",
    );
    text.finish()
}

/// ページ情報の 1 行表現。
fn page_line(page: &PageMeta) -> String {
    format!(
        "{} 件中 {} 件（offset={} snapshot_revision={}{}）",
        page.total_count,
        page.count,
        page.offset,
        page.snapshot_revision,
        next_offset_hint(page.has_more, page.next_offset),
    )
}

/// effect カタログのページ情報の 1 行表現。
///
/// `snapshot_revision` を示さない。effect カタログは登録済みプラグインの集合で
/// あり、プロジェクトの revision に連動しないためページ間の照合に使えない。
/// 照合されない値を示すと、次のページ要求へ添えるよう促してしまう。
fn catalog_page_line(page: &PageMeta) -> String {
    format!(
        "{} 件中 {} 件（offset={}{}）",
        page.total_count,
        page.count,
        page.offset,
        next_offset_hint(page.has_more, page.next_offset),
    )
}

/// 続きがある場合に次の offset を示す。
fn next_offset_hint(has_more: bool, next_offset: Option<u32>) -> String {
    match (has_more, next_offset) {
        (true, Some(next)) => format!(" 続きは offset={next}"),
        _ => String::new(),
    }
}

/// effect 種別の表示名。
fn effect_type_label(effect_type: &aviutl2_mcp_core::EffectType) -> String {
    match serde_json::to_value(effect_type) {
        Ok(serde_json::Value::String(name)) => name,
        _ => format!("unknown({})", effect_type.as_raw()),
    }
}

/// 省略可能な名前の表示。
fn optional_name(name: Option<&str>) -> String {
    match name {
        Some(name) => clamp_chars(name, MAX_NAME_CHARS),
        None => "-".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::summary::{MAX_TEXT_CHARS, TRUNCATION_NOTICE};
    use aviutl2_mcp_core::{
        AvailableEffect, EffectFingerprintInput, EffectFlags, EffectInfo, EffectType, FiniteF64,
        InstanceId, InstanceProject, InstanceState, LayerInfo, ObjectFingerprintInput,
        ObjectSummary, SceneInfo, SectionRange,
    };

    /// 上限を必ず超える件数。要求上限を無視した応答でも打ち切られることを確かめる。
    const OVERSIZED_COUNT: usize = 2_000;

    fn page(total: u32, count: u32) -> PageMeta {
        PageMeta {
            total_count: total,
            count,
            offset: 0,
            has_more: total > count,
            next_offset: (total > count).then_some(count),
            snapshot_revision: 42,
        }
    }

    fn long_name() -> String {
        "名".repeat(5_000)
    }

    /// 上限を超えず、超過分が打ち切られたことを示していること。
    fn assert_truncated_within_limit(text: &str) {
        assert!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "上限を超えています: {}",
            text.chars().count()
        );
        assert!(
            text.ends_with(TRUNCATION_NOTICE),
            "打ち切りが示されていません"
        );
    }

    #[test]
    fn instances_text_lists_ids_and_state() {
        let id = InstanceId::new_v4();
        let response = ListInstancesResponse {
            instances: vec![InstanceInfo {
                instance_id: id,
                state: InstanceState::Ready,
                pid: 1234,
                started_at: "2026-01-01T00:00:00.0000000Z".to_string(),
                project: None,
                scene: None,
            }],
            total_count: 1,
            count: 1,
            offset: 0,
            has_more: false,
            next_offset: None,
        };
        let text = instances(&response);
        assert!(text.contains(&id.to_string()));
        assert!(text.contains("ready"));
    }

    #[test]
    fn instances_text_is_bounded_for_oversized_results() {
        let listed: Vec<InstanceInfo> = (0..OVERSIZED_COUNT)
            .map(|_| InstanceInfo {
                instance_id: InstanceId::new_v4(),
                state: InstanceState::Ready,
                pid: 1234,
                started_at: "2026-01-01T00:00:00.0000000Z".to_string(),
                project: Some(InstanceProject {
                    display_name: long_name(),
                    path: None,
                    epoch: None,
                    revision: None,
                    modified: None,
                }),
                scene: None,
            })
            .collect();
        let response = ListInstancesResponse {
            total_count: listed.len() as u32,
            count: listed.len() as u32,
            instances: listed,
            offset: 0,
            has_more: false,
            next_offset: None,
        };
        assert_truncated_within_limit(&instances(&response));
    }

    #[test]
    fn layers_text_is_bounded_for_oversized_results() {
        let items: Vec<LayerInfo> = (0..OVERSIZED_COUNT)
            .map(|index| LayerInfo {
                index,
                name: Some(long_name()),
                enabled: true,
                locked: false,
                object_count: 3,
            })
            .collect();
        let result = ListLayersResult {
            items,
            page: page(10_000, OVERSIZED_COUNT as u32),
        };
        assert_truncated_within_limit(&layers(&result));
    }

    #[test]
    fn objects_text_is_bounded_for_oversized_results() {
        let items: Vec<ObjectSummary> = (0..OVERSIZED_COUNT)
            .map(|i| {
                ObjectSummary::new(
                    "78be92d1-c8c9-44c6-ae52-387548971468",
                    ObjectFingerprintInput {
                        scene_id: 0,
                        layer: i,
                        frame_start: 0,
                        frame_end: 10,
                        name: Some(&long_name()),
                        alias: "alias",
                    },
                )
            })
            .collect();
        let result = ListObjectsResult {
            items,
            page: page(100_000, OVERSIZED_COUNT as u32),
        };
        assert_truncated_within_limit(&objects(&result));
    }

    #[test]
    fn available_effects_text_is_bounded_for_oversized_results() {
        let items: Vec<AvailableEffect> = (0..OVERSIZED_COUNT)
            .map(|_| AvailableEffect {
                name: long_name(),
                effect_type: EffectType::Filter,
                flags: EffectFlags::from_raw(1),
                items: Vec::new(),
            })
            .collect();
        let result = ListAvailableEffectsResult {
            items,
            page: page(10_000, OVERSIZED_COUNT as u32),
        };
        assert_truncated_within_limit(&available_effects(&result));
    }

    #[test]
    fn object_detail_text_is_bounded_for_oversized_results() {
        let summary = ObjectSummary::new(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            ObjectFingerprintInput {
                scene_id: 0,
                layer: 2,
                frame_start: 0,
                frame_end: 10,
                name: Some(&long_name()),
                alias: "alias",
            },
        );
        let effects: Vec<EffectInfo> = (0..OVERSIZED_COUNT)
            .map(|index| {
                EffectInfo::new(
                    summary.selector.clone(),
                    EffectFingerprintInput {
                        effect_name: &long_name(),
                        effect_index: index,
                        enabled: true,
                        locked: false,
                        items: &[],
                    },
                )
            })
            .collect();
        let detail = ObjectDetail {
            summary,
            alias: long_name(),
            sections: vec![SectionRange { start: 0, end: 10 }],
            effects,
            project_revision: 42,
        };
        assert_truncated_within_limit(&object_detail(&detail));
    }

    #[test]
    fn every_text_content_guides_the_next_step() {
        let scene = SceneInfo {
            id: 3,
            name: Some("本編".to_string()),
            width: 1920,
            height: 1080,
            fps: FiniteF64::try_new(30.0),
            fps_rate: 30,
            fps_scale: 1,
            sample_rate: 48_000,
        };
        let summary = ObjectSummary::new(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            ObjectFingerprintInput {
                scene_id: 3,
                layer: 2,
                frame_start: 0,
                frame_end: 10,
                name: Some("立ち絵"),
                alias: "alias",
            },
        );

        let current_scene_text = current_scene(&GetCurrentSceneResult {
            scene,
            project_revision: 42,
        });
        assert!(current_scene_text.contains("expected_scene_id"));
        assert!(current_scene_text.contains("aviutl2_list_objects"));

        let objects_text = objects(&ListObjectsResult {
            items: vec![summary.clone()],
            page: page(1, 1),
        });
        assert!(objects_text.contains("aviutl2_get_object"));
        assert!(objects_text.contains("selector"));

        let object_detail_text = object_detail(&ObjectDetail {
            summary,
            alias: "alias".to_string(),
            sections: Vec::new(),
            effects: Vec::new(),
            project_revision: 42,
        });
        assert!(object_detail_text.contains("structuredContent"));

        let available_effects_text = available_effects(&ListAvailableEffectsResult {
            items: Vec::new(),
            page: page(0, 0),
        });
        assert!(available_effects_text.contains("effect_type"));
        assert!(available_effects_text.contains("structuredContent"));
    }

    #[test]
    fn short_results_are_not_truncated() {
        let result = ListLayersResult {
            items: vec![LayerInfo {
                index: 0,
                name: Some("背景".to_string()),
                enabled: true,
                locked: false,
                object_count: 2,
            }],
            page: page(1, 1),
        };
        let text = layers(&result);
        assert!(!text.contains(TRUNCATION_NOTICE));
        assert!(text.contains("layer=0"));
    }

    #[test]
    fn page_line_shows_next_offset_when_more_pages_exist() {
        let line = page_line(&page(1_000, 200));
        assert!(line.contains("offset=0"));
        assert!(line.contains("snapshot_revision=42"));
        assert!(line.contains("続きは offset=200"));
    }

    #[test]
    fn effect_catalog_text_does_not_show_snapshot_revision() {
        let text = available_effects(&ListAvailableEffectsResult {
            items: Vec::new(),
            page: page(1_000, 200),
        });
        assert!(
            !text.contains("snapshot_revision"),
            "照合に使えない値を次のページ要求へ促しています: {text}"
        );
        assert!(text.contains("続きは offset=200"), "{text}");

        // 照合する列挙は従来どおり値を示す。
        let layers_text = layers(&ListLayersResult {
            items: Vec::new(),
            page: page(1_000, 200),
        });
        assert!(
            layers_text.contains("snapshot_revision=42"),
            "{layers_text}"
        );
    }

    #[test]
    fn page_line_omits_next_offset_on_last_page() {
        let line = page_line(&page(5, 5));
        assert!(!line.contains("続きは"));
    }

    #[test]
    fn effect_type_label_keeps_unknown_raw() {
        assert_eq!(effect_type_label(&EffectType::Filter), "filter");
        assert_eq!(effect_type_label(&EffectType::Unknown(42)), "unknown(42)");
    }
}
