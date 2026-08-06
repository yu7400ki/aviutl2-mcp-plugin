//! tool の text content を組み立てる。
//!
//! 主要結果・ID・revision・次に必要な操作だけを短く示す。完全な機械可読値は
//! `structuredContent` が運ぶため、ここでは列挙の全項目を書き出さない。
//!
//! **object alias・設定項目の値・パスを text へ載せない。** これらは利用者の
//! 内容そのものであり、応答へ反響させない。オブジェクト名とレイヤー / フレーム
//! 番号は対象を見分けるのに要るため載せる。

use crate::api::ListInstancesResponse;
use crate::mcp::render::RenderFrameOutput;
use crate::mcp::summary::{TextBuilder, clamp_chars};
use aviutl2_mcp_core::{
    BatchOutcome, BatchStepOutcome, EditInfo, EditOutcome, EffectItemValues, EvaluatedItem,
    GetCurrentSceneResult, GridBpmOutcome, InstanceInfo, LayerStateOutcome,
    ListAvailableEffectsResult, ListFontsResult, ListLayersResult, ListModulesResult,
    ListObjectAliasesResult, ListObjectsResult, ListPalettesResult, ObjectDetail,
    ObjectSectionsOutcome, ObjectSummary, PageMeta, SceneSettingsOutcome, SelectionField,
    SelectionSnapshot, SelectionState,
};

/// 名前をそのまま行に載せるときの最大文字数。
const MAX_NAME_CHARS: usize = 60;

/// 一括適用の text content に書き出す sub-operation の件数。
///
/// **上限に触れてから切り詰めるのではなく、構造として届かないようにする。**
/// 1 行は 200 文字で切り詰められるため、100 件を全て書けば 20,000 文字に達し、
/// text content の上限へ危険なほど近づく。完全な機械可読値は
/// `structuredContent` が運ぶ。
const MAX_BATCH_LINES: usize = 10;

/// 編集の応答に共通して添える次の操作の案内。
const EDIT_NEXT_STEP: &str = "続けて編集する場合は structuredContent の selector をそのまま使えます。project_revision は要求には指定しません。前提条件が合わない場合は対象を読み直してください";

/// `list_instances` の text content。
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
        .display_name
        .as_deref()
        .map(|name| clamp_chars(name, MAX_NAME_CHARS))
        .unwrap_or_else(|| "-".to_string());
    // 未保存の変更があり得るかどうかは保存を促すかどうかを分ける。真は
    // 「変更があり得る」ことを、偽だけが「変更が無い」ことを表す。structuredContent
    // を読まない呼び出し側にも届くよう、行に載せる。
    let modified = info.project.modified;
    format!(
        "- {} state={} pid={} project={project} modified={modified}",
        info.instance_id,
        info.state.as_snake_case(),
        info.pid,
    )
}

/// `get_edit_info` の text content。
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

/// `get_current_scene` の text content。
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
        "list_layers / list_objects には expected_scene_id にこの scene_id を指定します",
    );
    text.finish()
}

/// `list_layers` の text content。
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

/// `list_objects` の text content。
pub fn objects(result: &ListObjectsResult) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!("オブジェクト {}", page_line(&result.page)));
    for object in &result.items {
        text.push_line(format!("- {}", object_position_line(object)));
    }
    text.push_line(
        "frame / layer は 0 始まりです。詳細は get_object に structuredContent の selector をそのまま渡します",
    );
    text.finish()
}

/// `get_object` の text content。
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

/// `list_available_effects` の text content。
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
            effect.item_count,
        ));
    }
    text.push_line(
        "effect_type を指定すると種別で絞り込めます。設定項目の名前は、対象へ付与したあと get_object が現在値付きで返します",
    );
    text.finish()
}

/// `list_fonts` の text content。
pub fn fonts(result: &ListFontsResult) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!("フォント {}", catalog_page_line(&result.page)));
    for name in &result.items {
        text.push_line(format!("- {}", clamp_chars(name, MAX_NAME_CHARS)));
    }
    text.push_line("いずれも font 種別の設定項目へそのまま指定できます");
    text.finish()
}

/// `list_palettes` の text content。
///
/// **色そのものは載せない。** 64 件の組を行へ並べても読めるものにならず、完全な
/// 機械可読値は `structuredContent` が運ぶ。
pub fn palettes(result: &ListPalettesResult) -> String {
    let mut text = TextBuilder::new();
    text.push_line(match &result.current {
        Some(name) => format!("現在のパレット {}", clamp_chars(name, MAX_NAME_CHARS)),
        None => "現在のパレット 取得できませんでした".to_string(),
    });
    text.push_line(format!("パレット {}", catalog_page_line(&result.page)));
    for palette in &result.items {
        text.push_line(format!(
            "- {} colors={}",
            clamp_chars(&palette.name, MAX_NAME_CHARS),
            palette.colors.len(),
        ));
    }
    text.push_line("色は structuredContent を参照してください");
    text.finish()
}

/// `list_modules` の text content。
///
/// **説明文は載せない。** 秘匿の対象ではないが、1 件あたりの長さが定まらず、
/// 一覧の行が説明文の長さで決まってしまう。
pub fn modules(result: &ListModulesResult) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!("モジュール {}", catalog_page_line(&result.page)));
    for module in &result.items {
        text.push_line(format!(
            "- {} type={}",
            clamp_chars(&module.name, MAX_NAME_CHARS),
            module.module_type.kind_name(),
        ));
    }
    text.push_line(
        "module_type を指定すると種別で絞り込めます。説明文は structuredContent を参照してください",
    );
    text.finish()
}

/// `list_object_aliases` の text content。
///
/// **エイリアスの中身も要約も載せない。** 行に並べるのは名前とラベルだけであり、
/// オブジェクト数と effect 名は `structuredContent` が運ぶ。
pub fn object_aliases(result: &ListObjectAliasesResult) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!(
        "オブジェクトエイリアス {}",
        catalog_page_line(&result.page)
    ));
    for alias in &result.items {
        let name = clamp_chars(&alias.name, MAX_NAME_CHARS);
        text.push_line(match &alias.label {
            Some(label) => format!("- {name} label={}", clamp_chars(label, MAX_NAME_CHARS)),
            None => format!("- {name}"),
        });
    }
    text.push_line(
        "name は create_object の alias_name へそのまま指定できます。オブジェクト数と effect 名は structuredContent を参照してください",
    );
    text.finish()
}

/// `get_effect_item_values` の text content。
///
/// **評価した値そのものは載せない。** 値は利用者の内容であり、完全な機械可読値は
/// `structuredContent` が運ぶ。ここに書くのは何をどれだけ評価したかだけである。
pub fn effect_item_values(values: &EffectItemValues) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!(
        "{} 件のフレームで {} 件の設定項目を評価しました（frame は 0 始まりのシーン絶対フレーム番号）",
        values.frames.len(),
        values.items.len(),
    ));
    for item in &values.items {
        text.push_line(match item {
            EvaluatedItem::Track { name, group, .. } => format!(
                "- {} track group={}",
                clamp_chars(name, MAX_NAME_CHARS),
                match group {
                    Some(group) => clamp_chars(&group.name, MAX_NAME_CHARS),
                    None => "なし".to_string(),
                },
            ),
            EvaluatedItem::Check { name, .. } => {
                format!("- {} check", clamp_chars(name, MAX_NAME_CHARS))
            }
        });
    }
    if values.truncated {
        text.push_line(
            "評価できる項目が上限を超えたため打ち切りました。items に項目名を指定すると対象を選べます",
        );
    }
    text.push_line("評価した値は structuredContent を参照してください");
    text.finish()
}

/// `get_selection` の text content。
///
/// フォーカスと選択を別の行に分けて示す。2 つは別の概念であり、並べて書くと
/// 同じものと読まれる。
pub fn selection(snapshot: &SelectionSnapshot) -> String {
    let mut text = TextBuilder::new();
    // 番号の起点は末尾で 1 度だけ述べる。フォーカスと選択の双方が番号を持つ
    // ため、行内で注記すると同じ断り書きが繰り返される。
    text.push_line(match &snapshot.focus {
        Some(object) => format!(
            "フォーカス（オブジェクト設定ウィンドウの選択）{}",
            object_position_line(object)
        ),
        None => "フォーカス（オブジェクト設定ウィンドウの選択）なし".to_string(),
    });
    text.push_line(match snapshot.focus_section {
        Some(section) => format!("フォーカス対象の区間番号 {section}"),
        None => "フォーカス対象の区間番号なし".to_string(),
    });
    text.push_line(format!(
        "タイムライン上の選択 {}",
        page_line(&snapshot.page)
    ));
    for object in &snapshot.selected {
        text.push_line(format!("- {}", object_position_line(object)));
    }
    text.push_line(format!("project_revision={}", snapshot.project_revision));
    text.push_line(
        "frame / layer は 0 始まりです。詳細は get_object に structuredContent の selector をそのまま渡します",
    );
    text.finish()
}

/// `create_object` の text content。
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

/// `move_object` の text content。
pub fn move_object(outcome: &EditOutcome) -> String {
    changed_object("移動しました", outcome)
}

/// `set_object_name` の text content。
pub fn set_object_name(outcome: &EditOutcome) -> String {
    changed_object("名前を変更しました", outcome)
}

/// `set_object_item` の text content。
pub fn set_object_item(outcome: &EditOutcome) -> String {
    changed_effect("設定項目を変更しました", outcome)
}

/// `add_effect` の text content。
pub fn add_effect(outcome: &EditOutcome) -> String {
    changed_effect("effect を付与しました", outcome)
}

/// `set_effect_enabled` の text content。
pub fn set_effect_enabled(outcome: &EditOutcome) -> String {
    changed_effect("effect の有効・無効を変更しました", outcome)
}

/// `delete_effect` の text content。
pub fn delete_effect(outcome: &EditOutcome) -> String {
    changed_object("effect を削除しました", outcome)
}

/// `delete_object` の text content。
pub fn delete_object(outcome: &EditOutcome) -> String {
    let mut text = TextBuilder::new();
    text.push_line("オブジェクトを削除しました");
    text.push_line(format!("project_revision={}", outcome.project_revision));
    text.push_line(
        "削除した対象の selector は以後使えません。別の対象を編集する場合は読み直してください",
    );
    text.finish()
}

/// `apply_batch` の text content。
///
/// 書き出すのは先頭 10 件までで、残りは件数だけを示す。
pub fn apply_batch(outcome: &BatchOutcome) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!(
        "{} 件の操作を 1 つの取り消し単位として適用しました",
        outcome.results.len(),
    ));
    for (index, step) in outcome.results.iter().take(MAX_BATCH_LINES).enumerate() {
        text.push_line(batch_step_line(index, step));
    }
    if let Some(rest) = outcome
        .results
        .len()
        .checked_sub(MAX_BATCH_LINES)
        .filter(|rest| *rest > 0)
    {
        text.push_line(format!("他 {rest} 件"));
    }
    text.push_line(format!("project_revision={}", outcome.project_revision));
    text.push_line(
        "上の位置と selector は全 sub-operation の適用を終えたあとに読み直した値です。続けて編集する場合は structuredContent の selector をそのまま使えます",
    );
    text.finish()
}

/// 一括適用の 1 sub-operation の結果を示す行。
///
/// 設定値は載せない。何が変わったかは `structuredContent` が運ぶ。
fn batch_step_line(index: usize, step: &BatchStepOutcome) -> String {
    let action = match &step.effect {
        Some(effect) => format!(
            "設定項目を変更 effect={}:{}",
            clamp_chars(&effect.name, MAX_NAME_CHARS),
            effect.index,
        ),
        None => "移動".to_string(),
    };
    format!("- [{index}] {action} {}", object_line(&step.object))
}

/// `render_frame` の text content。
///
/// **URI は載せる。** 識別子であり、画像の内容を漏らさない。引き渡しの識別子・
/// 保存先のパス・画像そのものは載せない。
pub fn render_frame(output: &RenderFrameOutput) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!(
        "scene_id={} frame={} を {}x{} で描画しました（frame は 0 始まり）",
        output.scene_id, output.frame, output.width, output.height,
    ));
    text.push_line(format!(
        "成果物 {} media_type={} expires_at={}",
        output.artifact.uri, output.artifact.media_type, output.artifact.expires_at,
    ));
    text.push_line(
        "画像は応答に含まれません。内容は resources/read にこの URI を渡して取得します。失効後は not_found になります",
    );
    text.finish()
}

/// `set_layer_state` の text content。
pub fn layer_state(outcome: &LayerStateOutcome) -> String {
    let layer = &outcome.layer;
    let mut text = TextBuilder::new();
    text.push_line(format!(
        "layer={} name={} enabled={} locked={}（layer は 0 始まり）",
        layer.index,
        optional_name(layer.name.as_deref()),
        layer.enabled,
        layer.locked,
    ));
    text.push_line(format!("project_revision={}", outcome.project_revision));
    text.push_line(
        "上の値は変更後に読み直した実際の状態です。レイヤーは fingerprint を持たないため、読み取り時からの変化は検出できません",
    );
    text.finish()
}

/// 中間点を変える 3 つの tool に共通する text content。
///
/// 区間そのものは列挙しない。件数と番号の対応だけを示し、完全な一覧は
/// `structuredContent` が運ぶ。
pub fn object_sections(action: &str, outcome: &ObjectSectionsOutcome) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!(
        "{action}。{} 件の区間になりました",
        outcome.sections.len()
    ));
    text.push_line(object_line(&outcome.object));
    text.push_line(
        "区間番号 i は sections[i] を指します。sections[0].start はオブジェクトの開始フレームであって中間点ではないため、区間 0 は削除も移動もできません",
    );
    finish_edit(text, outcome.project_revision)
}

/// `set_grid_bpm` の text content。
///
/// 一覧そのものは列挙しない。件数と、返る値の読み方だけを示し、完全な一覧は
/// `structuredContent` が運ぶ。
pub fn grid_bpm(outcome: &GridBpmOutcome) -> String {
    let mut text = TextBuilder::new();
    text.push_line(format!(
        "BPM グリッドを {} 件の一覧へ置き換えました",
        outcome.entries.len()
    ));
    text.push_line(format!("project_revision={}", outcome.project_revision));
    text.push_line(
        "entries には置き換え後に読み直した一覧が入ります。ホストは単精度で受け取り並べ替えもするため、要求した値や順序と一致するとは限りません",
    );
    text.finish()
}

/// `set_scene_settings` の text content。
///
/// 取り消せないことを先に述べる。応答だけを読む経路にとっては、これが性質を
/// 知る最後の機会である。
pub fn scene_settings(outcome: &SceneSettingsOutcome) -> String {
    let scene = &outcome.scene;
    let mut text = TextBuilder::new();
    text.push_line(
        "シーン設定を変更しました。この変更は取り消せません。取り消し操作を行うと、その前に行った編集が取り消されます",
    );
    text.push_line(format!(
        "scene_id={} name={} {}x{} sample_rate={}",
        scene.id,
        optional_name(scene.name.as_deref()),
        scene.width,
        scene.height,
        scene.sample_rate,
    ));
    text.push_line(format!("project_revision={}", outcome.project_revision));
    text.push_line(
        "上の値は変更後に読み直した実際の状態です。シーンは fingerprint を持たないため、読み取り時からの変化は検出できません",
    );
    text.push_line(
        "解像度とサンプリングレートは編集の区間を抜けた後に観測した値であり、ホストが調整し得ます。シーン名だけは区間の内側で照合済みです",
    );
    text.finish()
}

/// `set_selection` の text content。
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
    text.push_line(format!(
        "表示開始 frame={} layer={} 表示数 frame={} layer={}（表示数は厳密な値ではありません）",
        state.display.frame_start,
        state.display.layer_start,
        state.display.frame_num,
        state.display.layer_num,
    ));
    text.push_line(format!("適用できた項目: {}", applied_label(&state.applied)));
    if !state.not_applied.is_empty() {
        text.push_line(format!(
            "適用できなかった項目: {}",
            applied_label(&state.not_applied)
        ));
    }
    text.push_line(format!("project_revision={}", state.project_revision));
    text.push_line(
        "上の値はホストがクランプした結果であり、編集と同時に観測したものではありません。この変更は取り消し単位を作らず、取り消し操作はその前に行った編集を取り消します",
    );
    text.push_line(
        "not_applied の項目は反映されていません。確かめるには get_edit_info で読み直してください",
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

/// オブジェクトの位置と名前の 1 行表現。番号の起点を行内で注記する。
///
/// 対象を 1 件だけ示す text が用いる。番号を持つ行が 1 つしか無いため、末尾に
/// まとめて注記する場所が無い。
fn object_line(object: &ObjectSummary) -> String {
    format!(
        "{}（frame / layer は 0 始まり）",
        object_position_line(object)
    )
}

/// オブジェクトの位置と名前の 1 行表現。番号の起点は注記しない。
///
/// 一覧を並べる text が用いる。行ごとに注記すると同じ断り書きが件数分
/// 繰り返されるため、起点は末尾で 1 度だけ述べる。
fn object_position_line(object: &ObjectSummary) -> String {
    format!(
        "layer={} frame={}..{} name={}",
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
        SelectionField::Display => "display",
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
        AvailableEffect, Cursor, DisplayRange, EffectFingerprintInput, EffectFlags, EffectInfo,
        EffectItem, EffectItemType, EffectType, FiniteF64, FrameRange, InstanceId, InstanceProject,
        InstanceState, ItemValue, LayerInfo, ModuleEntry, ModuleType, ObjectAliasSummary,
        ObjectFingerprintInput, ObjectSummary, ObservedSelection, PALETTE_COLOR_COUNT,
        PaletteEntry, Rgba, SceneInfo, SectionRange, TrackGroup,
    };

    /// 上限を必ず超える件数。要求上限を無視した応答でも打ち切られることを確かめる。
    const OVERSIZED_COUNT: usize = 2_000;

    fn sample_display_range() -> DisplayRange {
        DisplayRange {
            frame_start: 60,
            layer_start: 1,
            frame_num: 600,
            layer_num: 10,
        }
    }

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
                project: sample_instance_project(),
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
        // 未保存の変更の有無は生存確認が必ず運ぶ。
        assert!(text.contains("modified=false"), "{text}");
    }

    /// プロジェクトファイルを持たないインスタンスのプロジェクト状態。
    fn sample_instance_project() -> InstanceProject {
        InstanceProject {
            display_name: None,
            path: None,
            epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            revision: 0,
            modified: false,
        }
    }

    #[test]
    fn instances_text_reports_unsaved_changes() {
        // 未保存の変更の有無は保存を促すかどうかを分ける。text だけを読む
        // 呼び出し側にも届かなければならない。
        for (modified, expected) in [(true, "modified=true"), (false, "modified=false")] {
            let response = ListInstancesResponse {
                instances: vec![InstanceInfo {
                    instance_id: InstanceId::new_v4(),
                    state: InstanceState::Ready,
                    pid: 1234,
                    started_at: "2026-01-01T00:00:00.0000000Z".to_string(),
                    project: InstanceProject {
                        modified,
                        ..sample_instance_project()
                    },
                }],
                total_count: 1,
                count: 1,
                offset: 0,
                has_more: false,
                next_offset: None,
            };
            let text = instances(&response);
            assert!(text.contains(expected), "{modified}: {text}");
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
                project: InstanceProject {
                    display_name: Some(long_name()),
                    ..sample_instance_project()
                },
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

    /// 番号の起点の注記が 1 度しか現れないことを確かめる。
    ///
    /// 一覧を並べる text は起点を末尾でまとめて述べる。行内でも述べると、同じ
    /// 断り書きが対象の件数だけ繰り返される。
    #[test]
    fn a_listing_states_the_origin_of_the_numbering_once() {
        let summary = ObjectSummary::new(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            ObjectFingerprintInput {
                scene_id: 0,
                layer: 2,
                frame_start: 0,
                frame_end: 10,
                name: Some("立ち絵"),
                alias: "alias",
            },
        );
        let texts = [
            selection(&SelectionSnapshot {
                project_revision: 42,
                focus: Some(summary.clone()),
                focus_section: Some(1),
                selected: vec![summary.clone(), summary.clone()],
                page: page(2, 2),
            }),
            objects(&ListObjectsResult {
                items: vec![summary.clone(), summary],
                page: page(2, 2),
            }),
        ];
        for text in texts {
            assert_eq!(
                text.matches("0 始まり").count(),
                1,
                "番号の起点が繰り返されています: {text}"
            );
        }
    }

    #[test]
    fn selection_text_is_bounded_for_oversized_results() {
        let selected: Vec<ObjectSummary> = (0..OVERSIZED_COUNT)
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
        let snapshot = SelectionSnapshot {
            project_revision: 42,
            focus: selected.first().cloned(),
            focus_section: Some(1),
            selected,
            page: page(100_000, OVERSIZED_COUNT as u32),
        };
        assert_truncated_within_limit(&selection(&snapshot));
    }

    #[test]
    fn available_effects_text_is_bounded_for_oversized_results() {
        let items: Vec<AvailableEffect> = (0..OVERSIZED_COUNT)
            .map(|_| AvailableEffect {
                name: long_name(),
                effect_type: EffectType::Filter,
                flags: EffectFlags::from_raw(1),
                item_count: 0,
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
    fn effect_item_values_text_is_bounded_for_oversized_results() {
        let items: Vec<EvaluatedItem> = (0..OVERSIZED_COUNT)
            .map(|_| EvaluatedItem::Track {
                name: long_name(),
                values: Vec::new(),
                group: Some(TrackGroup {
                    name: long_name(),
                    index: 0,
                    count: 3,
                    item_names: Vec::new(),
                }),
            })
            .collect();
        let values = EffectItemValues {
            project_revision: 42,
            frames: Vec::new(),
            items,
            truncated: true,
        };
        assert_truncated_within_limit(&effect_item_values(&values));
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
        assert!(current_scene_text.contains("list_objects"));

        let objects_text = objects(&ListObjectsResult {
            items: vec![summary.clone()],
            page: page(1, 1),
        });
        assert!(objects_text.contains("get_object"));
        assert!(objects_text.contains("selector"));

        let object_detail_text = object_detail(&ObjectDetail {
            summary,
            alias: "alias".to_string(),
            sections: Vec::new(),
            effects: Vec::new(),
            project_revision: 42,
        });
        assert!(object_detail_text.contains("structuredContent"));

        let selection_text = selection(&SelectionSnapshot {
            project_revision: 42,
            focus: None,
            focus_section: None,
            selected: Vec::new(),
            page: page(0, 0),
        });
        assert!(selection_text.contains("get_object"));
        assert!(selection_text.contains("selector"));

        let available_effects_text = available_effects(&ListAvailableEffectsResult {
            items: Vec::new(),
            page: page(0, 0),
        });
        assert!(available_effects_text.contains("effect_type"));
        assert!(available_effects_text.contains("get_object"));

        let effect_item_values_text = effect_item_values(&EffectItemValues {
            project_revision: 42,
            frames: Vec::new(),
            items: Vec::new(),
            truncated: false,
        });
        assert!(effect_item_values_text.contains("structuredContent"));
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
            ObservedSelection {
                cursor: Cursor {
                    frame: 120,
                    layer: 2,
                },
                selected_range: Some(FrameRange { start: 0, end: 10 }),
                focus: Some(summary),
                display: sample_display_range(),
            },
            vec![SelectionField::Cursor],
            vec![
                SelectionField::SelectedRange,
                SelectionField::Focus,
                SelectionField::Display,
            ],
        );
        vec![
            ("create_object", create_object(&created)),
            ("move_object", move_object(&object_changed)),
            ("set_object_name", set_object_name(&object_changed)),
            ("set_object_item", set_object_item(&effect_changed)),
            ("add_effect", add_effect(&effect_changed)),
            ("set_effect_enabled", set_effect_enabled(&effect_changed)),
            ("delete_effect", delete_effect(&object_changed)),
            ("delete_object", delete_object(&deleted)),
            (
                "set_layer_state",
                layer_state(&LayerStateOutcome {
                    project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
                    project_revision: 43,
                    layer: LayerInfo {
                        index: 2,
                        name: Some("背景".to_string()),
                        enabled: false,
                        locked: true,
                        object_count: 3,
                    },
                }),
            ),
            ("set_selection", selection_state(&selection)),
            (
                "create_object_section",
                object_sections("中間点を追加しました", &sample_sections_outcome()),
            ),
            (
                "delete_object_section",
                object_sections("中間点を削除しました", &sample_sections_outcome()),
            ),
            (
                "move_object_section",
                object_sections("中間点を移動しました", &sample_sections_outcome()),
            ),
            (
                "set_grid_bpm",
                grid_bpm(&GridBpmOutcome {
                    project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
                    project_revision: 43,
                    entries: vec![aviutl2_mcp_core::GridBpm {
                        tempo: FiniteF64::try_new(120.0).expect("有限値"),
                        beat: 4,
                        start: FiniteF64::try_new(0.0).expect("有限値"),
                        offset: FiniteF64::try_new(0.25).expect("有限値"),
                    }],
                }),
            ),
            (
                "set_scene_settings",
                scene_settings(&sample_scene_settings_outcome()),
            ),
            ("apply_batch", apply_batch(&sample_batch_outcome())),
        ]
    }

    /// 3 軸を変更したあとに観測したシーンの状態。
    fn sample_scene_settings_outcome() -> SceneSettingsOutcome {
        SceneSettingsOutcome {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            project_revision: 43,
            scene: aviutl2_mcp_core::SceneInfo {
                id: 3,
                name: Some("本編".to_string()),
                width: 1920,
                height: 1080,
                fps: FiniteF64::try_new(60.0),
                fps_rate: 60,
                fps_scale: 1,
                sample_rate: 48_000,
            },
            observed_after_edit: true,
            non_undoable: true,
        }
    }

    #[test]
    fn scene_settings_text_states_that_the_change_cannot_be_undone() {
        // 応答だけを読む経路にとって、これが取り消せないことを知る最後の機会で
        // ある。観測が編集と原子的でないことも併せて述べる。
        let text = scene_settings(&sample_scene_settings_outcome());
        assert!(text.contains("この変更は取り消せません"), "{text}");
        assert!(
            text.contains("その前に行った編集が取り消されます"),
            "{text}"
        );
        assert!(text.contains("1920x1080"), "{text}");
        assert!(text.contains("sample_rate=48000"), "{text}");
        assert!(text.contains("区間を抜けた後に観測した値"), "{text}");
    }

    #[test]
    fn every_edit_text_covers_every_edit_tool() {
        // 表は手書きであり、載せ忘れた tool は 3 つの検査を素通りする。編集
        // operation の一覧と突き合わせて、載せ忘れをここで落とす。
        let covered: std::collections::BTreeSet<&str> =
            every_edit_text().iter().map(|(name, _)| *name).collect();
        let expected: std::collections::BTreeSet<&str> = aviutl2_mcp_core::EditOperation::ALL
            .iter()
            .map(|operation| operation.as_str())
            .collect();
        assert_eq!(covered, expected);
    }

    /// 中間点を 1 つ持つ対象の変更結果。
    fn sample_sections_outcome() -> ObjectSectionsOutcome {
        ObjectSectionsOutcome {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            project_revision: 43,
            object: sample_summary(),
            sections: vec![
                aviutl2_mcp_core::SectionRange {
                    start: 120,
                    end: 179,
                },
                aviutl2_mcp_core::SectionRange {
                    start: 180,
                    end: 240,
                },
            ],
        }
    }

    #[test]
    fn section_text_states_the_index_correspondence() {
        // 区間の番号と中間点の番号が 1 つずれることは、要求元が自力で気付ける
        // 情報ではない。
        let text = object_sections("中間点を追加しました", &sample_sections_outcome());
        assert!(text.contains("2 件の区間"), "{text}");
        assert!(text.contains("sections[0].start"), "{text}");
        assert!(text.contains("区間 0 は削除も移動もできません"), "{text}");
        assert!(text.contains("project_revision=43"), "{text}");
    }

    /// 移動と設定変更を 1 件ずつ含む一括適用の結果。
    fn sample_batch_outcome() -> BatchOutcome {
        let summary = sample_summary();
        let effect = secretive_effect(&summary);
        BatchOutcome {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            project_revision: 43,
            results: vec![
                BatchStepOutcome {
                    object: summary.clone(),
                    effect: None,
                },
                BatchStepOutcome {
                    object: summary,
                    effect: Some(effect),
                },
            ],
        }
    }

    #[test]
    fn render_text_points_at_the_resource_without_leaking_the_image_or_its_source() {
        use crate::mcp::render::{ArtifactRef, RenderFrameOutput};
        let output = RenderFrameOutput {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            project_revision: 42,
            scene_id: 3,
            frame: 120,
            width: 1920,
            height: 1080,
            artifact: ArtifactRef {
                artifact_id: "5d0b6f7a-1f2e-4a3b-9c8d-7e6f5a4b3c2d".to_string(),
                uri: "aviutl2://artifacts/5d0b6f7a-1f2e-4a3b-9c8d-7e6f5a4b3c2d".to_string(),
                media_type: "image/png".to_string(),
                byte_length: 4096,
                sha256: format!("sha256:{}", "0".repeat(64)),
                expires_at: "2026-01-01T00:10:00+00:00".to_string(),
            },
        };

        let text = render_frame(&output);
        assert!(text.contains("scene_id=3"), "{text}");
        assert!(text.contains("frame=120"), "{text}");
        assert!(text.contains("1920x1080"), "{text}");
        assert!(text.contains(&output.artifact.uri), "{text}");
        assert!(text.contains("2026-01-01T00:10:00+00:00"), "{text}");
        assert!(text.contains("resources/read"), "{text}");
        assert!(text.contains("0 始まり"), "{text}");
    }

    #[test]
    fn batch_text_distinguishes_the_two_kinds_of_sub_operation() {
        let text = apply_batch(&sample_batch_outcome());
        assert!(text.contains("2 件の操作"), "{text}");
        assert!(text.contains("[0] 移動"), "{text}");
        assert!(text.contains("[1] 設定項目を変更"), "{text}");
        assert!(text.contains("1 つの取り消し単位"), "{text}");
    }

    #[test]
    fn batch_text_stops_at_ten_lines_and_counts_the_rest() {
        // 100 件を全て書けば 1 行 200 文字の切り詰めでも 20,000 文字に達し、
        // 上限へ危険なほど近づく。上限に触れてから切り詰めるのではなく、
        // 構造として届かないようにする。
        let summary = ObjectSummary::new(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            ObjectFingerprintInput {
                scene_id: 3,
                layer: 2,
                frame_start: 0,
                frame_end: 10,
                name: Some(&long_name()),
                alias: "alias",
            },
        );
        let outcome = BatchOutcome {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            project_revision: 43,
            results: (0..aviutl2_mcp_core::MAX_BATCH_OPERATIONS)
                .map(|_| BatchStepOutcome {
                    object: summary.clone(),
                    effect: None,
                })
                .collect(),
        };

        let text = apply_batch(&outcome);
        assert!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "上限を超えています: {}",
            text.chars().count()
        );
        assert!(text.contains("- [9] "), "10 件目がありません: {text}");
        assert!(!text.contains("- [10] "), "11 件目が書かれています: {text}");
        assert!(text.contains("他 90 件"), "残りの件数がありません: {text}");
        // 打ち切りは行数で決まるため、上限に触れて捨てられたのではない。
        assert!(!text.contains(TRUNCATION_NOTICE), "{text}");
    }

    #[test]
    fn batch_text_of_ten_or_fewer_steps_counts_nothing_extra() {
        let summary = sample_summary();
        let outcome = BatchOutcome {
            project_epoch: "78be92d1-c8c9-44c6-ae52-387548971468".to_string(),
            project_revision: 43,
            results: (0..10)
                .map(|_| BatchStepOutcome {
                    object: summary.clone(),
                    effect: None,
                })
                .collect(),
        };
        let text = apply_batch(&outcome);
        assert!(text.contains("- [9] "), "{text}");
        assert!(!text.contains("他 "), "{text}");
    }

    /// 応答が対象オブジェクトの位置を運ばない tool。
    ///
    /// 削除では対象が消えており、レイヤーの状態変更・BPM グリッドの置き換え・
    /// シーン設定の変更ではそもそも対象がオブジェクトではない。
    const TOOLS_WITHOUT_AN_OBJECT: &[&str] = &[
        "delete_object",
        "set_layer_state",
        "set_grid_bpm",
        "set_scene_settings",
    ];

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
            if TOOLS_WITHOUT_AN_OBJECT.contains(&tool) {
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
            set_effect_enabled(&outcome),
        ] {
            assert!(text.contains("fingerprint"), "{text}");
        }
    }

    #[test]
    fn selection_text_separates_the_applied_and_the_not_applied_fields() {
        let state = SelectionState::observed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            ObservedSelection {
                cursor: Cursor { frame: 5, layer: 1 },
                selected_range: None,
                focus: None,
                display: sample_display_range(),
            },
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
    fn selection_text_names_what_an_undo_would_remove() {
        // 「戻る保証が無い」は「戻るかもしれない」と読める。実際は戻らないうえに
        // 取り消しが 1 つ前の編集まで飛ぶため、失うものを名指しする。
        let state = SelectionState::observed(
            "78be92d1-c8c9-44c6-ae52-387548971468",
            43,
            ObservedSelection {
                cursor: Cursor { frame: 5, layer: 1 },
                selected_range: None,
                focus: None,
                display: sample_display_range(),
            },
            vec![SelectionField::Cursor],
            Vec::new(),
        );
        let text = selection_state(&state);
        assert!(text.contains("取り消し単位を作らず"), "{text}");
        assert!(text.contains("その前に行った編集を取り消します"), "{text}");
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
    fn the_catalog_texts_do_not_show_snapshot_revision() {
        // 照合に使えない値を次のページ要求へ促さない。
        let texts = [
            (
                "list_fonts",
                fonts(&ListFontsResult {
                    items: Vec::new(),
                    page: page(1_000, 200),
                }),
            ),
            (
                "list_palettes",
                palettes(&ListPalettesResult {
                    current: None,
                    items: Vec::new(),
                    page: page(1_000, 200),
                }),
            ),
            (
                "list_modules",
                modules(&ListModulesResult {
                    items: Vec::new(),
                    page: page(1_000, 200),
                }),
            ),
            (
                "list_object_aliases",
                object_aliases(&ListObjectAliasesResult {
                    items: Vec::new(),
                    page: page(1_000, 200),
                }),
            ),
        ];
        for (tool, text) in texts {
            assert!(!text.contains("snapshot_revision"), "{tool}: {text}");
            assert!(text.contains("続きは offset=200"), "{tool}: {text}");
        }
    }

    #[test]
    fn object_alias_text_names_the_entries_without_their_contents() {
        // 名前とラベルは利用者が付けた名前であり載せてよい。中身と要約は載せない。
        let text = object_aliases(&ListObjectAliasesResult {
            items: vec![
                ObjectAliasSummary {
                    name: "立ち絵".to_string(),
                    label: Some("キャラ".to_string()),
                    object_count: Some(2),
                    effects: vec!["テキスト".to_string(), "標準描画".to_string()],
                },
                ObjectAliasSummary {
                    name: "手置き".to_string(),
                    label: None,
                    object_count: None,
                    effects: Vec::new(),
                },
            ],
            page: page(2, 2),
        });
        assert!(text.contains("- 立ち絵 label=キャラ"), "{text}");
        assert!(text.contains("- 手置き"), "{text}");
        assert!(!text.contains("標準描画"), "{text}");
        assert!(!text.contains("object_count"), "{text}");
    }

    #[test]
    fn palette_text_reports_the_colour_count_without_the_colours() {
        let text = palettes(&ListPalettesResult {
            current: Some("[標準.既定]".to_string()),
            items: vec![PaletteEntry {
                name: "既定".to_string(),
                colors: vec![
                    Rgba {
                        r: 18,
                        g: 52,
                        b: 86,
                        a: 255
                    };
                    PALETTE_COLOR_COUNT
                ],
            }],
            page: page(1, 1),
        });
        assert!(text.contains("[標準.既定]"), "{text}");
        assert!(text.contains("colors=64"), "{text}");
    }

    #[test]
    fn palette_text_says_the_current_name_is_missing() {
        let text = palettes(&ListPalettesResult {
            current: None,
            items: Vec::new(),
            page: page(0, 0),
        });
        assert!(text.contains("取得できませんでした"), "{text}");
    }

    #[test]
    fn module_text_does_not_carry_the_information() {
        // 秘匿の対象ではないが、1 件あたりの長さが定まらない。行に載せると
        // 一覧の大きさが説明文の長さで決まってしまう。
        let text = modules(&ListModulesResult {
            items: vec![ModuleEntry {
                module_type: ModuleType::PluginInput,
                name: "入力プラグイン".to_string(),
                information: SECRET_VALUE.to_string(),
            }],
            page: page(1, 1),
        });
        assert!(text.contains("入力プラグイン"), "{text}");
        assert!(text.contains("plugin_input"), "{text}");
        assert!(!text.contains(SECRET_VALUE), "{text}");
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
