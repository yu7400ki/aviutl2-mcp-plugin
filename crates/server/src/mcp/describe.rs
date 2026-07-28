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
    text.push_line(format!("利用可能 effect {}", page_line(&result.page)));
    for effect in &result.items {
        text.push_line(format!(
            "- {} type={} items={}",
            clamp_chars(&effect.name, MAX_NAME_CHARS),
            effect_type_label(&effect.effect_type),
            effect.items.len(),
        ));
    }
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
    use crate::mcp::summary::MAX_TEXT_CHARS;
    use aviutl2_mcp_core::{
        AvailableEffect, EffectFlags, EffectType, InstanceId, InstanceState, LayerInfo,
        ObjectFingerprintInput, ObjectSummary,
    };

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
    fn layers_text_is_bounded_for_long_names() {
        let items: Vec<LayerInfo> = (0..200)
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
            page: page(1_000, 200),
        };
        let text = layers(&result);
        assert!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "上限を超えています: {}",
            text.chars().count()
        );
    }

    #[test]
    fn objects_text_is_bounded_for_long_names() {
        let items: Vec<ObjectSummary> = (0..200)
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
            page: page(10_000, 200),
        };
        let text = objects(&result);
        assert!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "上限を超えています: {}",
            text.chars().count()
        );
    }

    #[test]
    fn available_effects_text_is_bounded_for_long_names() {
        let items: Vec<AvailableEffect> = (0..200)
            .map(|_| AvailableEffect {
                name: long_name(),
                effect_type: EffectType::Filter,
                flags: EffectFlags::from_raw(1),
                items: Vec::new(),
            })
            .collect();
        let result = ListAvailableEffectsResult {
            items,
            page: page(200, 200),
        };
        let text = available_effects(&result);
        assert!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "上限を超えています: {}",
            text.chars().count()
        );
    }

    #[test]
    fn page_line_shows_next_offset_when_more_pages_exist() {
        let line = page_line(&page(1_000, 200));
        assert!(line.contains("offset=0"));
        assert!(line.contains("snapshot_revision=42"));
        assert!(line.contains("続きは offset=200"));
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
