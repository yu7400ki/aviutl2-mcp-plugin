//! tool の text content を組み立てる。
//!
//! 主要結果・ID・revision・次に必要な操作だけを短く示す。完全な機械可読値は
//! `structuredContent` が運ぶため、ここでは列挙の全項目を書き出さない。
//!
//! **object alias・設定項目の値・パスを text へ載せない。** これらは利用者の
//! 内容そのものであり、応答へ反響させない。オブジェクト名とレイヤー / フレーム
//! 番号は対象を見分けるのに要るため載せる。

use crate::api::ListInstancesResponse;
use crate::mcp::summary::{TextBuilder, clamp_chars};
use aviutl2_mcp_core::{
    EditInfo, EditOutcome, GetCurrentSceneResult, InstanceInfo, ListAvailableEffectsResult,
    ListLayersResult, ListObjectsResult, ObjectDetail, ObjectSummary, PageMeta, SelectionField,
    SelectionState,
};

/// 名前をそのまま行に載せるときの最大文字数。
const MAX_NAME_CHARS: usize = 60;

/// 編集の応答に共通して添える次の操作の案内。
const EDIT_NEXT_STEP: &str = "続けて編集する場合は structuredContent の selector と project_revision をそのまま使えます。前提条件が合わない場合は対象を読み直してください";

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
        .and_then(|p| p.display_name.as_deref())
        .map(|name| clamp_chars(name, MAX_NAME_CHARS))
        .unwrap_or_else(|| "-".to_string());
    // 未保存の変更の有無は保存を促すかどうかを分ける。structuredContent を
    // 読まない呼び出し側にも届くよう、行に載せる。
    let modified = info
        .project
        .as_ref()
        .and_then(|p| p.modified)
        .map(|modified| format!("modified={modified}"))
        .unwrap_or_else(|| "modified=未取得".to_string());
    let scene = info
        .scene
        .as_ref()
        .map(|s| format!("scene_id={}", s.id))
        .unwrap_or_else(|| "scene=未取得".to_string());
    format!(
        "- {} state={} pid={} project={project} {modified} {scene}",
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

/// `aviutl2_create_object` の text content。
pub fn create_object(outcome: &EditOutcome) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!(
        "オブジェクトを {} 件作成しました",
        outcome.created.len()
    ));
    if let Some(object) = &outcome.object {
        text.push_line(format!("先頭 {}", object_line(object)));
    }
    text.push_line(
        "長さと挿入位置はホストが決めるため、要求した位置と異なることがあります。上の位置が実際の配置です",
    );
    finish_edit(text, outcome.project_revision)
}

/// `aviutl2_move_object` の text content。
pub fn move_object(outcome: &EditOutcome) -> String {
    changed_object("移動しました", outcome)
}

/// `aviutl2_set_object_name` の text content。
pub fn set_object_name(outcome: &EditOutcome) -> String {
    changed_object("名前を変更しました", outcome)
}

/// `aviutl2_set_object_item` の text content。
pub fn set_object_item(outcome: &EditOutcome) -> String {
    changed_effect("設定項目を変更しました", outcome)
}

/// `aviutl2_add_effect` の text content。
pub fn add_effect(outcome: &EditOutcome) -> String {
    changed_effect("effect を付与しました", outcome)
}

/// `aviutl2_set_effect_state` の text content。
pub fn set_effect_state(outcome: &EditOutcome) -> String {
    changed_effect("effect の状態を変更しました", outcome)
}

/// `aviutl2_delete_effect` の text content。
pub fn delete_effect(outcome: &EditOutcome) -> String {
    changed_object("effect を削除しました", outcome)
}

/// `aviutl2_delete_object` の text content。
pub fn delete_object(outcome: &EditOutcome) -> String {
    let mut text = TextBuilder::new();
    text.push_line("オブジェクトを削除しました");
    text.push_line(format!("project_revision={}", outcome.project_revision));
    text.push_line(
        "削除した対象の selector は以後使えません。別の対象を編集する場合は読み直してください",
    );
    text.finish()
}

/// `aviutl2_set_selection` の text content。
pub fn selection_state(state: &SelectionState) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!(
        "cursor frame={} layer={}（frame / layer は 0 始まり）",
        state.cursor.frame, state.cursor.layer,
    ));
    text.push_line(match &state.selected_range {
        Some(range) => format!("選択範囲 frame {}..{}", range.start, range.end),
        None => "選択範囲なし".to_string(),
    });
    text.push_line(match &state.focus {
        Some(object) => format!("フォーカス {}", object_line(object)),
        None => "フォーカスなし".to_string(),
    });
    text.push_line(format!("適用できた項目: {}", applied_label(&state.applied)));
    if !state.not_applied.is_empty() {
        text.push_line(format!(
            "適用できなかった項目: {}",
            applied_label(&state.not_applied)
        ));
    }
    text.push_line(format!("project_revision={}", state.project_revision));
    text.push_line(
        "上の値はホストがクランプした結果であり、編集と同時に観測したものではありません。取り消し操作で元へ戻る保証もありません",
    );
    text.push_line(
        "not_applied の項目は反映されていません。確かめるには aviutl2_get_edit_info で読み直してください",
    );
    text.finish()
}

/// オブジェクトだけが変わった編集の text content。
fn changed_object(action: &str, outcome: &EditOutcome) -> String {
    let mut text = TextBuilder::new();
    text.push_line(target_line(action, outcome.object.as_ref()));
    finish_edit(text, outcome.project_revision)
}

/// effect を伴う編集の text content。
fn changed_effect(action: &str, outcome: &EditOutcome) -> String {
    let mut text = TextBuilder::new();
    text.push_line(target_line(action, outcome.object.as_ref()));
    if let Some(effect) = &outcome.effect {
        text.push_line(format!(
            "effect {}:{} enabled={} locked={} items={}",
            clamp_chars(&effect.name, MAX_NAME_CHARS),
            effect.index,
            effect.enabled,
            effect.locked,
            effect.items.len(),
        ));
    }
    text.push_line(
        "effect の変更でオブジェクトの fingerprint も変わるため、変更前の selector は使えません",
    );
    finish_edit(text, outcome.project_revision)
}

/// 変更後の対象を示す 1 行。
fn target_line(action: &str, object: Option<&ObjectSummary>) -> String {
    match object {
        Some(object) => format!("{action} {}", object_line(object)),
        None => format!("{action}（対象は応答に含まれません）"),
    }
}

/// オブジェクトの位置と名前の 1 行表現。
fn object_line(object: &ObjectSummary) -> String {
    format!(
        "layer={} frame={}..{} name={}（frame / layer は 0 始まり）",
        object.layer,
        object.frame_start,
        object.frame_end,
        optional_name(object.name.as_deref()),
    )
}

/// 適用できた項目の表示。
fn applied_label(applied: &[SelectionField]) -> String {
    if applied.is_empty() {
        return "なし".to_string();
    }
    applied
        .iter()
        .map(selection_field_label)
        .collect::<Vec<_>>()
        .join(" ")
}

/// 選択状態の項目名。
fn selection_field_label(field: &SelectionField) -> &'static str {
    match field {
        SelectionField::Cursor => "cursor",
        SelectionField::SelectedRange => "selected_range",
        SelectionField::Focus => "focus",
    }
}

/// 編集の text content へ revision と次の操作を添えて仕上げる。
fn finish_edit(mut text: TextBuilder, project_revision: u64) -> String {
    text.push_line(format!("project_revision={project_revision}"));
    text.push_line(EDIT_NEXT_STEP);
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
        AvailableEffect, Cursor, EffectFingerprintInput, EffectFlags, EffectInfo, EffectItem,
        EffectItemType, EffectType, FiniteF64, FrameRange, InstanceId, InstanceProject,
        InstanceState, ItemValue, LayerInfo, ObjectFingerprintInput, ObjectSummary, SceneInfo,
        SectionRange,
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
        // project を持たない候補では未保存の変更の有無を判断できない。
        assert!(text.contains("modified=未取得"), "{text}");
    }

    #[test]
    fn instances_text_reports_unsaved_changes() {
        // 未保存の変更の有無は保存を促すかどうかを分ける。text だけを読む
        // 呼び出し側にも届かなければならない。
        for (modified, expected) in [
            (Some(true), "modified=true"),
            (Some(false), "modified=false"),
            (None, "modified=未取得"),
        ] {
            let response = ListInstancesResponse {
                instances: vec![InstanceInfo {
                    instance_id: InstanceId::new_v4(),
                    state: InstanceState::Ready,
                    pid: 1234,
                    started_at: "2026-01-01T00:00:00.0000000Z".to_string(),
                    project: Some(InstanceProject {
                        display_name: None,
                        path: None,
                        epoch: None,
                        revision: None,
                        modified,
                    }),
                    scene: None,
                }],
                total_count: 1,
                count: 1,
                offset: 0,
                has_more: false,
                next_offset: None,
            };
            let text = instances(&response);
            assert!(text.contains(expected), "{modified:?}: {text}");
        }
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
                    display_name: Some(long_name()),
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
                        effect_fingerprints: &[],
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
                effect_fingerprints: &[],
            },
        );
        let effects: Vec<EffectInfo> = (0..OVERSIZED_COUNT)
            .map(|index| {
                EffectInfo::new(
                    summary.selector.clone(),
                    EffectFingerprintInput {
                        effect_name: &long_name(),
                        effect_index: index,
                        position: index,
                        effect_count: OVERSIZED_COUNT,
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
                effect_fingerprints: &[],
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

    /// 秘匿すべき内容を全て含む effect。
    fn secretive_effect(summary: &ObjectSummary) -> EffectInfo {
        let items = vec![
            EffectItem {
                name: "テキスト".to_string(),
                item_type: EffectItemType::Text,
                value: ItemValue::Text {
                    value: SECRET_VALUE.to_string(),
                },
                track: None,
            },
            EffectItem {
                name: "ファイル".to_string(),
                item_type: EffectItemType::File,
                value: ItemValue::File {
                    path: SECRET_PATH.to_string(),
                },
                track: None,
            },
        ];
        EffectInfo::new(
            summary.selector.clone(),
            EffectFingerprintInput {
                effect_name: "テキスト",
                effect_index: 0,
                position: 0,
                effect_count: 1,
                enabled: true,
                locked: false,
                items: &items,
            },
        )
    }

    /// text へ現れてはならない設定値。
    const SECRET_VALUE: &str = "秘密の字幕";
    /// text へ現れてはならないパス。
    const SECRET_PATH: &str = r"C:\Users\tester\secret.png";

    fn sample_summary() -> ObjectSummary {
        ObjectSummary::new(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            ObjectFingerprintInput {
                scene_id: 3,
                layer: 2,
                frame_start: 120,
                frame_end: 240,
                name: Some("立ち絵"),
                alias: "[vo]\n_name=立ち絵\n",
                effect_fingerprints: &[],
            },
        )
    }

    /// 全編集 tool の text content を、秘匿すべき内容を含む応答から組み立てる。
    fn every_edit_text() -> Vec<(&'static str, String)> {
        let summary = sample_summary();
        let effect = secretive_effect(&summary);
        let object_changed = EditOutcome::object_changed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            summary.clone(),
        );
        let effect_changed = EditOutcome::effect_changed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            summary.clone(),
            effect,
        );
        let created = EditOutcome::created(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            vec![summary.clone(), summary.clone()],
        );
        let deleted = EditOutcome::deleted("78be92d1-c8c9-44c6-ae52-387548971468", 43);
        let selection = SelectionState::observed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            Cursor {
                frame: 120,
                layer: 2,
            },
            Some(FrameRange { start: 0, end: 10 }),
            Some(summary),
            vec![SelectionField::Cursor],
            vec![SelectionField::SelectedRange, SelectionField::Focus],
        );
        vec![
            ("aviutl2_create_object", create_object(&created)),
            ("aviutl2_move_object", move_object(&object_changed)),
            ("aviutl2_set_object_name", set_object_name(&object_changed)),
            ("aviutl2_set_object_item", set_object_item(&effect_changed)),
            ("aviutl2_add_effect", add_effect(&effect_changed)),
            (
                "aviutl2_set_effect_state",
                set_effect_state(&effect_changed),
            ),
            ("aviutl2_delete_effect", delete_effect(&object_changed)),
            ("aviutl2_delete_object", delete_object(&deleted)),
            ("aviutl2_set_selection", selection_state(&selection)),
        ]
    }

    #[test]
    fn edit_text_states_the_change_the_revision_and_the_next_step() {
        for (tool, text) in every_edit_text() {
            assert!(
                text.contains("project_revision=43"),
                "{tool} が変更後の revision を示していません: {text}"
            );
            assert!(
                text.contains("selector") || text.contains("読み直"),
                "{tool} が次の操作を示していません: {text}"
            );
        }
    }

    #[test]
    fn edit_text_locates_the_target() {
        for (tool, text) in every_edit_text() {
            if tool == "aviutl2_delete_object" {
                // 対象は消えているため位置を示さない。
                continue;
            }
            assert!(
                text.contains("layer=2") && text.contains("frame=120..240"),
                "{tool} が対象の位置を示していません: {text}"
            );
            assert!(
                text.contains("立ち絵"),
                "{tool} が対象の名前を示していません: {text}"
            );
        }
    }

    #[test]
    fn edit_text_never_carries_aliases_values_or_paths() {
        // 応答へ載せてよいのは対象の位置と名前だけである。alias・設定値・パスは
        // 利用者の内容そのものであり、text へ反響させない。
        for (tool, text) in every_edit_text() {
            for forbidden in [SECRET_VALUE, SECRET_PATH, "[vo]", "_name="] {
                assert!(
                    !text.contains(forbidden),
                    "{tool} の text に {forbidden} が含まれています: {text}"
                );
            }
        }
    }

    #[test]
    fn edit_text_is_bounded_for_oversized_results() {
        let summary = ObjectSummary::new(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            ObjectFingerprintInput {
                scene_id: 3,
                layer: 2,
                frame_start: 0,
                frame_end: 10,
                name: Some(&long_name()),
                alias: "alias",
                effect_fingerprints: &[],
            },
        );
        let created: Vec<ObjectSummary> = (0..OVERSIZED_COUNT).map(|_| summary.clone()).collect();
        let outcome = EditOutcome::created("78be92d1-c8c9-44c6-ae52-387548971468", 43, created);
        let text = create_object(&outcome);
        assert!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "上限を超えています: {}",
            text.chars().count()
        );
    }

    #[test]
    fn effect_changing_text_warns_that_the_object_fingerprint_moved() {
        let summary = sample_summary();
        let outcome = EditOutcome::effect_changed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            summary.clone(),
            secretive_effect(&summary),
        );
        for text in [
            set_object_item(&outcome),
            add_effect(&outcome),
            set_effect_state(&outcome),
        ] {
            assert!(text.contains("fingerprint"), "{text}");
        }
    }

    #[test]
    fn selection_text_separates_the_applied_and_the_not_applied_fields() {
        let state = SelectionState::observed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            Cursor { frame: 5, layer: 1 },
            None,
            None,
            Vec::new(),
            vec![SelectionField::Cursor],
        );
        let text = selection_state(&state);
        assert!(text.contains("適用できた項目: なし"), "{text}");
        assert!(text.contains("適用できなかった項目: cursor"), "{text}");
        assert!(text.contains("選択範囲なし"), "{text}");
        assert!(text.contains("フォーカスなし"), "{text}");
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
