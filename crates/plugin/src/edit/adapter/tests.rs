//! 編集手順の統合テスト。
//!
//! フェイクは [`EditHost`] / [`SceneEditor`] の位置に差し込むため、検証の対象は
//! adapter の本番実装そのものになる。フェイクは呼び出しを順序ごと記録するので、
//! 順序自体を検証できる。

use super::*;
use crate::alias::tests::{TempDir, write_fixture};
use crate::edit::fake::{
    CHOICE_VALUES, CLOSURE_ESCAPED, COORDINATE, CREATE_FRAME_SHIFT, DEFAULT_COLOR, DEFAULT_FONT,
    EFFECT_LIST, FakeCatalogEntry, FakeEditHost, FakeLayer, FakeObject, FakeReadHost, Fault,
    ITEM_VALUE, Knobs, LAYER_ATTRIBUTES, LAYER_LOCK, LAYER_MAX, MAX_FRAME, MAX_ITEM_VALUE,
    MAX_LAYER, MAX_SCENE_HEIGHT, MAX_SCENE_SAMPLE_RATE, MAX_SCENE_WIDTH, MOVE_FRAME_SHIFT,
    MOVING_ITEM, MUTATIONS, OBSERVED_SCENE, PanicPoint, READ_SECTION, RENAMED_SCENE_NAME, SCENE_ID,
    SCENE_NAME, SECTION_RANGES, SHAPE, STATIC_ITEM, TRACK_MODES, coordinate,
    coordinate_catalog_entry, raw_item_value, shape, shape_catalog_entry,
};
use crate::read::{HostReadAdapter, ReadAdapter};
use crate::test_support::{default_page_request, default_page_window, with_silent_panic_hook};
use aviutl2_mcp_core::{
    ApplyBatchParams, BatchOperation, CreateObjectSectionParams, CursorPosition,
    DeleteObjectSectionParams, Destination, EditOperation, EffectFlags, EffectItem, EffectItemType,
    EffectSelector, EffectType, ErrorCode, Fingerprint, FiniteF64, GridBpm, ItemChoices,
    ItemFacets, ItemRange, ItemValue, LayerNameChange, MAX_GRID_BPM_ENTRIES,
    MoveObjectSectionParams, Movement, ObjectSectionsOutcome, ObjectSelector, Placement, SceneSize,
    TableSource,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::mpsc::channel;
use std::time::Duration;

/// BPM 情報 1 件を組み立てる。
fn grid_bpm(tempo: f64, beat: i64, start: f64, offset: f64) -> GridBpm {
    let finite = |value: f64| FiniteF64::try_new(value).expect("有限値");
    GridBpm {
        tempo: finite(tempo),
        beat,
        start: finite(start),
        offset: finite(offset),
    }
}

/// フェイクを組み込んだ編集口と読み取り口の一式。
struct Harness {
    host: Arc<FakeEditHost>,
    project: Arc<ProjectState>,
    edit: HostEditAdapter<Arc<FakeEditHost>>,
    read: HostReadAdapter<FakeReadHost>,
}

impl Harness {
    /// 既定の状態で一式を組む。
    fn new() -> Self {
        Self::with(|_| {})
    }

    /// フェイクの設定を変えて一式を組む。
    fn with(configure: impl FnOnce(&mut FakeEditHost)) -> Self {
        let project = Arc::new(ProjectState::new());
        let mut host = FakeEditHost::new();
        host.project = Some(project.clone());
        configure(&mut host);
        let host = Arc::new(host);
        Self {
            edit: HostEditAdapter::new(host.clone(), project.clone()),
            read: HostReadAdapter::new(FakeReadHost(host.clone()), project.clone()),
            host,
            project,
        }
    }

    /// 現在のプロジェクト epoch を返す。
    ///
    /// セレクターを持たない要求（作成・選択状態の変更）だけがこれを前提として
    /// 運ぶ。
    fn epoch(&self) -> String {
        self.project.epoch()
    }

    /// 仕込んだ失敗を止めた状態で読み取る。
    ///
    /// 編集へ渡すセレクターは健全な状態の読み取りから得る。失敗を仕込んだまま
    /// 読むと、要求を組み立てる段で先に落ちてしまい、編集の判定を試せない。
    fn healthy<T>(&self, read: impl FnOnce() -> T) -> T {
        let mut saved = Knobs::default();
        self.host.arm(|knobs| {
            saved = *knobs;
            *knobs = Knobs::default();
        });
        let value = read();
        self.host.arm(|knobs| *knobs = saved);
        value
    }

    /// 読み取り経路が返す概要を得る。
    ///
    /// 編集へ渡すセレクターは必ずここから取る。読み取りが返した値をそのまま
    /// 送り返せることが、往復の契約そのものである。
    fn summary(&self, layer: usize, frame: usize) -> ObjectSummary {
        let page = self
            .healthy(|| {
                self.read
                    .list_objects(SCENE_ID, None, &default_page_request())
            })
            .expect("列挙に失敗しました")
            .expect("ページ要求が拒否されました");
        page.items
            .into_iter()
            .find(|item| item.layer == layer && item.frame_start == frame)
            .unwrap_or_else(|| panic!("レイヤー {layer} フレーム {frame} の対象がありません"))
    }

    /// 読み取り経路が数えるシーンのオブジェクト数を得る。
    fn object_count(&self) -> usize {
        self.healthy(|| {
            self.read
                .list_objects(SCENE_ID, None, &default_page_request())
        })
        .expect("列挙に失敗しました")
        .expect("ページ要求が拒否されました")
        .items
        .len()
    }

    /// 読み取り経路が返すオブジェクトのセレクターを得る。
    fn selector(&self, layer: usize, frame: usize) -> ObjectSelector {
        self.summary(layer, frame).selector
    }

    /// 読み取り経路が返す effect のセレクターを得る。
    fn effect_selector(
        &self,
        layer: usize,
        frame: usize,
        effect_name: &str,
        effect_index: usize,
    ) -> EffectSelector {
        let selector = self.selector(layer, frame);
        let detail = self
            .healthy(|| self.read.get_object(&selector))
            .expect("対象の詳細を取得できませんでした");
        detail
            .effects
            .into_iter()
            .find(|effect| effect.name == effect_name && effect.index == effect_index)
            .unwrap_or_else(|| panic!("{effect_name}:{effect_index} がありません"))
            .selector
    }

    /// 対象の指定を差し替えた effect のセレクターを得る。
    ///
    /// effect 自体は与えられた指定が指す位置から読み直し、そのうえで所属
    /// オブジェクトの指定だけを与えられた値へ差し替える。食い違わせた指定の
    /// まま読むと、要求を組み立てる段で先に落ちて編集の判定を試せない。
    fn effect_selector_of(
        &self,
        object: ObjectSelector,
        effect_name: &str,
        effect_index: usize,
    ) -> EffectSelector {
        let mut selector =
            self.effect_selector(object.layer, object.frame, effect_name, effect_index);
        selector.object = object;
        selector
    }

    /// 変更 API が 1 度も呼ばれていないことを確かめる。
    fn assert_untouched(&self) {
        assert!(
            !self.host.mutated(),
            "判定を通らずに変更 API が呼ばれました: {:?}",
            self.host.calls()
        );
        assert_eq!(
            self.project.revision(),
            0,
            "変更していないのに revision が進みました"
        );
    }
}

/// 別の fingerprint へ差し替える。
fn tamper(fingerprint: &Fingerprint) -> Fingerprint {
    let text = fingerprint.to_string();
    let (algorithm, digest) = text.split_once(':').expect("fingerprint の書式");
    let flipped: String = digest
        .chars()
        .map(|c| if c == '0' { '1' } else { '0' })
        .collect();
    format!("{algorithm}:{flipped}")
        .parse()
        .expect("差し替えた fingerprint の書式")
}

/// 立ち絵オブジェクトの移動要求を組み立てる。
fn move_params(harness: &Harness) -> MoveObjectParams {
    MoveObjectParams {
        selector: harness.selector(1, 100),
        destination: Destination {
            layer: 1,
            frame: 500,
        },
    }
}

// ---------------------------------------------------------------- 受付判定

#[test]
fn a_starting_host_is_rejected_without_touching_the_sdk() {
    let harness = Harness::with(|host| host.arm(|knobs| knobs.ready = false));
    let error = harness
        .edit
        .move_object(&move_params(&harness))
        .expect_err("準備前の編集が受理されました");

    assert_eq!(error.error_code(), ErrorCode::HostBusy);
    assert_eq!(harness.host.enter_calls(), 0);
    harness.assert_untouched();
}

#[test]
fn playback_blocks_the_edit_before_the_section_is_entered() {
    for state in [EditState::Preview, EditState::Save] {
        let harness = Harness::with(|host| host.arm(|knobs| knobs.state = state));
        let params = move_params(&harness);
        let error = harness
            .edit
            .move_object(&params)
            .expect_err("{state} 中の編集が受理されました");

        assert_eq!(error.error_code(), ErrorCode::EditBlocked);
        assert_eq!(harness.host.enter_calls(), 0, "{state} で区間へ入りました");
        harness.assert_untouched();
    }
}

#[test]
fn a_section_failure_is_reclassified_by_rereading_the_edit_state() {
    let harness = Harness::with(|host| {
        host.arm(|knobs| {
            knobs.fault = Some(Fault::Section);
            knobs.later_state = Some(EditState::Save);
        });
    });
    let params = move_params(&harness);
    let error = harness
        .edit
        .move_object(&params)
        .expect_err("区間の失敗が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::EditBlocked);
    harness.assert_untouched();
}

#[test]
fn a_section_failure_while_editing_stays_an_sdk_error() {
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::Section)));
    let params = move_params(&harness);
    let error = harness.edit.move_object(&params).expect_err("区間の失敗");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
}

#[test]
fn one_request_enters_the_edit_section_exactly_once() {
    let harness = Harness::new();
    harness
        .edit
        .move_object(&move_params(&harness))
        .expect("移動に失敗しました");

    assert_eq!(
        harness.host.enter_calls(),
        1,
        "1 要求が複数の取り消し単位に分かれました"
    );
}

// ------------------------------------------------------------ 検証順序 1〜8

#[test]
fn the_selector_epoch_is_checked_first() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    // セレクター・シーン・fingerprint の全てを壊しても、最初の段で落ちる。
    params.selector.project_epoch = "別のプロジェクト".to_string();
    params.selector.scene_id = 9;
    params.selector.fingerprint = tamper(&params.selector.fingerprint);

    let error = harness.edit.move_object(&params).expect_err("epoch 不一致");
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));
    harness.assert_untouched();
}

#[test]
fn an_advanced_revision_is_accepted_when_the_fingerprint_matches() {
    let harness = Harness::new();
    let params = move_params(&harness);
    // 対象は変えずに revision だけを進める。fingerprint は一致したままである。
    harness.project.on_object_updated();

    harness
        .edit
        .move_object(&params)
        .expect("revision が進んだだけで編集が拒否されました");
    assert!(harness.host.mutated());
}

#[test]
fn a_scene_guard_mismatch_is_checked_before_the_resolution() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.scene_id = 9;
    // 解決できない座標を併せて指定しても、シーンの段で落ちる。
    params.selector.frame = 9_999;

    let error = harness.edit.move_object(&params).expect_err("シーン不一致");
    assert_eq!(error.details()["mismatch"], json!("scene_id"));
    assert_eq!(error.details()["expected_scene_id"], json!(9));
    harness.assert_untouched();
}

#[test]
fn a_tampered_fingerprint_is_rejected() {
    // 要求は算出方式を運ばない。対象が変化していれば fingerprint が捕まえる
    // ため、別対象への適用は起きない。
    let harness = Harness::new();
    let params = move_params(&harness);
    harness
        .edit
        .move_object(&params)
        .expect("現在の対象を指す指定が拒否されました");

    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.fingerprint = tamper(&params.selector.fingerprint);

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("fingerprint の食い違いが受理されました");
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["mismatch"], json!("fingerprint"));
    harness.assert_untouched();
}

#[test]
fn an_unresolvable_selector_is_not_found() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.frame = 9_999;

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("解決できない対象");
    assert_eq!(error.error_code(), ErrorCode::NotFound);
    harness.assert_untouched();
}

#[test]
fn an_ambiguous_selector_reports_the_candidate_count() {
    let harness = Harness::with(|host| {
        let mut scene = host.scene.lock().unwrap();
        // 同じ開始フレームに同名の対象を並べる。
        let duplicate = FakeObject {
            id: 42,
            placement: scene.layers[1].objects[0].placement.clone(),
            alias: "[1:100]".to_string(),
            effects: Vec::new(),
            section_points: Vec::new(),
        };
        scene.layers[1].objects.push(duplicate);
        drop(scene);
    });
    let mut params = MoveObjectParams {
        selector: harness.selector(0, 0),
        destination: Destination {
            layer: 1,
            frame: 500,
        },
    };
    params.selector.layer = 1;
    params.selector.frame = 100;
    params.selector.name = Some("立ち絵".to_string());

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("曖昧なセレクター");
    assert_eq!(error.error_code(), ErrorCode::AmbiguousSelector);
    assert_eq!(error.details()["candidate_count"], json!(2));
    harness.assert_untouched();
}

#[test]
fn a_fingerprint_mismatch_is_checked_before_the_operation_preconditions() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.fingerprint = tamper(&params.selector.fingerprint);
    // 宛先も埋めておく。fingerprint の段で落ちるので宛先重複にはならない。
    params.destination.frame = 300;

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("fingerprint 不一致");
    assert_eq!(error.details()["mismatch"], json!("fingerprint"));
    harness.assert_untouched();
}

/// 名前を変えられた対象への編集が、読み直せば作り直せる失敗として返ることを
/// 確かめる。
///
/// 名前で候補を絞ると、この状況は候補 0 件になり「再試行しても解消しない」
/// として返る。要求元は復帰できるのに停止する。
#[test]
fn a_renamed_target_is_rejected_as_a_content_mismatch() {
    let harness = Harness::new();
    let params = move_params(&harness);
    harness.host.scene.lock().unwrap().layers[1].objects[0]
        .placement
        .name = Some("改名後".to_string());

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("改名された対象への編集が受理されました");
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["mismatch"], json!("fingerprint"));
    assert_eq!(error.details()["retry_requires"], json!("refetch"));
    harness.assert_untouched();
}

/// 内容が食い違った応答が返した対象を、そのまま次の要求へ渡せることを確かめる。
///
/// 応答が現在の姿を返さなければ、要求元は列挙まで戻って対象を探し直すほかない。
/// 失敗と再要求の 2 呼び出しで済むことを、呼び出し回数ごと固定する。
#[test]
fn the_current_object_of_a_content_mismatch_is_accepted_as_is() {
    let harness = Harness::new();
    let params = move_params(&harness);
    harness.host.scene.lock().unwrap().layers[1].objects[0]
        .placement
        .name = Some("改名後".to_string());

    // 要求の組み立てに使った読み取りをここまでで数え、以降増えないことを見る。
    let reads_before = read_sections(&harness);

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("改名された対象への編集が受理されました");
    let details = error.details();
    assert_eq!(details["mismatch"], json!("fingerprint"));

    let selector: ObjectSelector =
        serde_json::from_value(details["current_object"]["selector"].clone())
            .expect("応答が返したセレクターを読み取れません");
    let outcome = harness
        .edit
        .move_object(&MoveObjectParams {
            selector,
            destination: params.destination,
        })
        .expect("応答が返したセレクターでの再要求が拒否されました");

    let object = outcome.object.expect("移動の応答が対象を返しませんでした");
    assert_eq!(object.frame_start, params.destination.frame as usize);
    assert_eq!(object.name.as_deref(), Some("改名後"));

    assert_eq!(
        read_sections(&harness),
        reads_before,
        "失敗と再要求の間に読み直しを挟みました"
    );
    assert_eq!(
        harness.host.enter_calls(),
        2,
        "失敗と再要求の 2 呼び出しで済んでいません"
    );
}

/// 読み取り経路が参照区間へ入った回数。
fn read_sections(harness: &Harness) -> usize {
    harness
        .host
        .calls()
        .iter()
        .filter(|call| **call == READ_SECTION)
        .count()
}

/// 名前を名乗らないセレクターでも対象が特定できることを確かめる。
#[test]
fn a_selector_without_a_name_still_resolves_the_target() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.name = None;

    harness
        .edit
        .move_object(&params)
        .expect("名前を持たない指定が拒否されました");
    assert!(harness.host.mutated());
}

#[test]
fn a_revision_change_during_the_resolution_does_not_stop_the_mutation() {
    // 対象の解決と fingerprint の再計算の間に revision が進む状況を作る。
    // 対象の内容は変わっていないので、変更はそのまま発行される。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.bump_on_detail = 1));
    let params = move_params(&harness);

    harness
        .edit
        .move_object(&params)
        .expect("解決中の revision の変化で変更が止まりました");
    assert!(harness.host.mutated());
}

#[test]
fn the_project_boundary_is_matched_only_before_the_resolution() {
    // 境界の照合は区間の先頭で 1 度だけ行う。区間へ入った後に境界が変わっても
    // 変更は止まらない——プロジェクト境界の更新は区間と同じスレッドで走るため、
    // 区間の内側で入れ替わる経路が存在しない。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.renew_on_detail = true));
    let params = move_params(&harness);

    harness
        .edit
        .move_object(&params)
        .expect("区間の内側の境界の変化で変更が止まりました");
    assert!(harness.host.mutated());

    // 区間へ入る前の境界の食い違いは従来どおり止める。
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.project_epoch = "別のプロジェクト".to_string();
    let error = harness
        .edit
        .move_object(&params)
        .expect_err("別プロジェクトのセレクターが受理されました");
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));
    assert!(!harness.host.mutated());
}

// ---------------------------------------------------- 読み取りとの解決の共有

#[test]
fn the_edit_path_accepts_the_selector_the_read_path_returned() {
    let harness = Harness::new();
    let detail = harness
        .read
        .get_object(&harness.selector(1, 100))
        .expect("詳細の取得");

    let outcome = harness
        .edit
        .set_object_name(&SetObjectNameParams {
            selector: detail.summary.selector.clone(),
            name: Some("新しい名前".to_string()),
        })
        .expect("読み取りが返したセレクターが編集で拒否されました");

    // 変更後の応答が返した概要を、読み取り経路がそのまま受け付ける。両者が
    // 同じ材料から同じ fingerprint を算出していなければ成立しない。
    let after = outcome.object.expect("変更後の対象");
    let reread = harness
        .read
        .get_object(&after.selector)
        .expect("編集の応答が返したセレクターを読み取りが拒否しました");
    assert_eq!(reread.summary, after);
}

#[test]
fn an_effect_edit_checks_both_the_object_and_the_effect_fingerprint() {
    let harness = Harness::new();
    let selector = harness.effect_selector(1, 100, "ぼかし", 0);

    let mut object_tampered = selector.clone();
    object_tampered.object.fingerprint = tamper(&object_tampered.object.fingerprint);
    let error = harness
        .edit
        .delete_effect(&DeleteEffectParams {
            selector: object_tampered,
        })
        .expect_err("オブジェクトの fingerprint 改竄が通りました");
    assert_eq!(error.details()["mismatch"], json!("fingerprint"));

    let mut effect_tampered = selector;
    effect_tampered.fingerprint = tamper(&effect_tampered.fingerprint);
    let error = harness
        .edit
        .delete_effect(&DeleteEffectParams {
            selector: effect_tampered,
        })
        .expect_err("effect の fingerprint 改竄が通りました");
    assert_eq!(error.details()["mismatch"], json!("fingerprint"));
    harness.assert_untouched();
}

/// effect の食い違いでは現在の対象を名乗らないことを確かめる。
///
/// ここへ到達する時点で所属オブジェクトの照合は通っている。オブジェクトの概要を
/// 添えても要求元が送ってきた値と同じであり、「そのまま次の要求へ渡せば通る」と
/// いう案内に従うと同じ失敗が返り続ける。読み直すべきは effect の一覧である。
#[test]
fn an_effect_mismatch_does_not_name_a_current_object() {
    let harness = Harness::new();
    let mut selector = harness.effect_selector(1, 100, "ぼかし", 0);
    selector.fingerprint = tamper(&selector.fingerprint);

    let error = harness
        .edit
        .delete_effect(&DeleteEffectParams { selector })
        .expect_err("effect の fingerprint 改竄が通りました");
    let details = error.details();
    assert_eq!(details["mismatch"], json!("fingerprint"));
    assert_eq!(details["retry_requires"], json!("refetch"));
    assert!(
        details.get("current_object").is_none(),
        "要求元が既に持っている値を現在の姿として返しました: {details}"
    );
    harness.assert_untouched();
}

/// 同じ effect の指定でも、食い違いが対象の側なら現在の対象を名乗ることを
/// 確かめる。
#[test]
fn an_object_mismatch_under_an_effect_selector_names_the_current_object() {
    let harness = Harness::new();
    let mut selector = harness.effect_selector(1, 100, "ぼかし", 0);
    selector.object.fingerprint = tamper(&selector.object.fingerprint);

    let error = harness
        .edit
        .delete_effect(&DeleteEffectParams { selector })
        .expect_err("オブジェクトの fingerprint 改竄が通りました");
    let details = error.details();
    assert_eq!(details["mismatch"], json!("fingerprint"));
    assert_eq!(details["current_object"]["frame_start"], json!(100));
    harness.assert_untouched();
}

#[test]
fn a_missing_effect_is_not_found() {
    let harness = Harness::new();
    let mut selector = harness.effect_selector(1, 100, "ぼかし", 0);
    selector.effect_index = 5;

    let error = harness
        .edit
        .delete_effect(&DeleteEffectParams { selector })
        .expect_err("存在しない effect が解決されました");
    assert_eq!(error.error_code(), ErrorCode::NotFound);
    assert_eq!(error.details()["effect_name"], json!("ぼかし"));
    assert_eq!(error.details()["effect_index"], json!(5));
    harness.assert_untouched();
}

// -------------------------------------------------- operation 固有の事前条件

#[test]
fn a_locked_layer_is_rejected() {
    let harness = Harness::new();
    let error = harness
        .edit
        .delete_object(&DeleteObjectParams {
            selector: harness.selector(2, 0),
        })
        .expect_err("ロックされたレイヤーの対象が削除されました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["reason"], json!("layer_locked"));
    assert_eq!(error.details()["layer"], json!(2));
    // ロックの解除は別の operation であり、読み直しても要求は通らない。
    assert_eq!(error.details()["retry_requires"], json!("none"));
    harness.assert_untouched();
}

#[test]
fn a_locked_destination_layer_is_rejected() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.destination.layer = 2;
    params.destination.frame = 500;

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("ロックされたレイヤーへ移動できました");
    assert_eq!(error.details()["reason"], json!("layer_locked"));
    harness.assert_untouched();
}

#[test]
fn an_occupied_destination_is_rejected() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.destination.frame = 350;

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("既存の対象へ重ねて移動できました");
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["reason"], json!("destination_occupied"));
    assert_eq!(error.details()["layer"], json!(1));
    assert_eq!(error.details()["frame"], json!(350));
    // 塞いでいる範囲が返るため、次の宛先を選ぶのに読み直しが要らない。
    assert_eq!(
        error.details()["occupied_by"],
        json!({"frame_start": 300, "frame_end": 400})
    );
    // 塞いでいる対象の名前と fingerprint は載せない。
    let text = error.details().to_string();
    assert!(!text.contains("字幕"), "{text}");
    assert!(!text.contains("fingerprint"), "{text}");
    harness.assert_untouched();
}

#[test]
fn moving_an_object_onto_itself_is_not_treated_as_occupied() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.destination.frame = 100;

    harness
        .edit
        .move_object(&params)
        .expect("自分自身の位置が塞がりとして扱われました");
}

#[test]
fn an_occupied_creation_target_is_rejected() {
    let harness = Harness::new();
    let error = harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "[obj]".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 150,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect_err("既存の対象へ重ねて作成できました");

    assert_eq!(error.details()["reason"], json!("destination_occupied"));
    harness.assert_untouched();
}

#[test]
fn an_unsupported_media_file_is_rejected_before_the_mutation() {
    let harness = Harness::new();
    let error = harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::MediaFile {
                path: r"C:\media\clip.xyz".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 600,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect_err("対応しないメディアから作成できました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("media_not_supported"));
    harness.assert_untouched();
}

/// effect 名を作成元とする要求を組み立てる。
fn create_from_effect(harness: &Harness, name: &str, layer: u32, frame: u32) -> CreateObjectParams {
    CreateObjectParams {
        source: ObjectSource::Effect {
            name: name.to_string(),
        },
        placement: Placement {
            scene_id: SCENE_ID,
            layer,
            frame,
        },
        expected_project_epoch: harness.epoch(),
    }
}

#[test]
fn an_effect_source_calls_the_creation_api_that_takes_an_effect_name() {
    let harness = Harness::new();
    harness.host.clear_calls();
    harness
        .edit
        .create_object(&create_from_effect(&harness, "ぼかし", 1, 600))
        .expect("effect 名から作成できませんでした");

    let calls = harness.host.calls();
    assert!(
        calls.contains(&"create_object"),
        "effect 名を取る作成 API を呼んでいません: {calls:?}"
    );
    assert!(
        !calls.contains(&"create_object_from_alias")
            && !calls.contains(&"create_object_from_media_file"),
        "既存 2 種の経路が呼ばれています: {calls:?}"
    );
    assert!(
        harness.host.mutated(),
        "作成が変更 API の発行として記録されていません"
    );
}

#[test]
fn the_existing_sources_keep_their_own_creation_api() {
    for (source, expected) in [
        (
            ObjectSource::ObjectAlias {
                alias: "[obj]".to_string(),
            },
            "create_object_from_alias",
        ),
        (
            ObjectSource::MediaFile {
                path: r"C:\media\clip.mp4".to_string(),
            },
            "create_object_from_media_file",
        ),
    ] {
        let harness = Harness::new();
        harness.host.clear_calls();
        harness
            .edit
            .create_object(&CreateObjectParams {
                source,
                placement: Placement {
                    scene_id: SCENE_ID,
                    layer: 1,
                    frame: 600,
                },
                expected_project_epoch: harness.epoch(),
            })
            .expect("作成に失敗しました");

        let calls = harness.host.calls();
        assert!(calls.contains(&expected), "{expected} を呼んでいません");
        assert!(
            !calls.contains(&"create_object"),
            "{expected} の経路が effect 名の作成 API へ流れています"
        );
    }
}

#[test]
fn an_effect_source_does_not_go_through_the_media_path_check() {
    // 作成元がパスを運ばない以上、パスの規則は掛からない。掛けると、パスとしては
    // 不正な文字列を名前に持つ effect が作成元にできなくなる。
    let harness = Harness::with(|host| {
        host.catalog.push(FakeCatalogEntry {
            name: r"..\図形:1".to_string(),
            effect_type: EffectType::Filter,
            flags: EffectFlags::from_raw(1),
            items: Vec::new(),
            facets: HashMap::new(),
        });
    });
    harness.host.clear_calls();
    harness
        .edit
        .create_object(&create_from_effect(&harness, r"..\図形:1", 1, 600))
        .expect("パスとして不正な effect 名が拒否されました");

    let calls = harness.host.calls();
    assert!(
        !calls.contains(&"is_support_media_file"),
        "メディア対応の確認が effect 名に掛かっています: {calls:?}"
    );
}

#[test]
fn an_unregistered_effect_source_is_rejected_without_entering_the_section() {
    let harness = Harness::new();
    let error = harness
        .edit
        .create_object(&create_from_effect(
            &harness,
            "存在しないエフェクト",
            1,
            600,
        ))
        .expect_err("未登録の effect 名から作成できました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("effect_not_registered"));
    assert_eq!(harness.host.enter_calls(), 0);
    harness.assert_untouched();
}

#[test]
fn an_effect_the_host_refuses_to_create_from_is_reported_apart_from_an_unregistered_one() {
    // 「登録されていない」と「登録されているが元にできない」は別の事実である。
    // 畳むと、要求元は名前の誤りと対応の欠如を区別できない。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::RejectObjectCreation)));
    let refused = harness
        .edit
        .create_object(&create_from_effect(&harness, "ぼかし", 1, 600))
        .expect_err("拒否された作成が成功として返りました");

    assert_eq!(refused.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(refused.details()["reason"], json!("effect_not_creatable"));

    let harness = Harness::new();
    let unregistered = harness
        .edit
        .create_object(&create_from_effect(
            &harness,
            "存在しないエフェクト",
            1,
            600,
        ))
        .expect_err("未登録の effect 名から作成できました");

    assert_ne!(
        refused.details()["reason"],
        unregistered.details()["reason"],
        "2 つの失敗が同じ名前で返っています"
    );
}

#[test]
fn an_occupied_creation_target_is_rejected_for_an_effect_source() {
    let harness = Harness::new();
    let error = harness
        .edit
        .create_object(&create_from_effect(&harness, "ぼかし", 1, 150))
        .expect_err("既存の対象へ重ねて作成できました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["reason"], json!("destination_occupied"));
    harness.assert_untouched();
}

#[test]
fn a_locked_layer_rejects_creating_from_an_effect_name() {
    let harness = Harness::new();
    let error = harness
        .edit
        .create_object(&create_from_effect(&harness, "ぼかし", 2, 600))
        .expect_err("ロックされたレイヤーへ作成できました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["reason"], json!("layer_locked"));
    harness.assert_untouched();
}

#[test]
fn every_effect_type_in_the_catalog_reaches_the_creation_api() {
    // どの effect が作成の元になれるかは SDK が述べていない。種別で絞ると、
    // 実際に作れる effect を呼ぶ前に拒むことになる。カタログに在る名前は
    // 種別を問わず SDK へ届くことを固定する。
    // カタログの種別構成そのものを表として固定する。構成が痩せると、絞り込みが
    // 入っても素通りする検査になる。
    let types: Vec<EffectType> = crate::edit::fake::fake_catalog()
        .into_iter()
        .map(|effect| effect.effect_type)
        .collect();
    assert_eq!(
        types,
        vec![
            EffectType::Filter,
            EffectType::Input,
            EffectType::Filter,
            EffectType::Output,
        ],
        "カタログの種別構成が変わると絞り込みの有無を判別できません"
    );

    for effect in crate::edit::fake::fake_catalog() {
        let harness = Harness::new();
        harness.host.clear_calls();
        harness
            .edit
            .create_object(&create_from_effect(&harness, &effect.name, 1, 600))
            .unwrap_or_else(|error| {
                panic!(
                    "{} ({:?}) の作成が拒否されました: {error}",
                    effect.name, effect.effect_type
                )
            });

        assert!(
            harness.host.calls().contains(&"create_object"),
            "{} ({:?}) が SDK へ届いていません",
            effect.name,
            effect.effect_type
        );
    }
}

#[test]
fn an_unregistered_effect_name_is_rejected_without_entering_the_section() {
    let harness = Harness::new();
    let error = harness
        .edit
        .add_effect(&AddEffectParams {
            object: harness.selector(1, 100),
            effect_name: "存在しないエフェクト".to_string(),
        })
        .expect_err("未登録の effect が付与されました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("effect_not_registered"));
    assert_eq!(harness.host.enter_calls(), 0);
}

// ------------------------------------------ 登録済みエイリアス名からの作成

/// 一覧の除外と作成の拒否が同じ fixture を見ることを保つための一時ディレクトリ。
///
/// fixture を 2 つに割ると、一覧と作成が別の対象について語ることになる。
fn alias_fixture() -> (TempDir, Vec<String>) {
    let dir = TempDir::new();
    let names = write_fixture(&dir);
    (dir, names)
}

/// 与えたディレクトリを解決済みのデータディレクトリとして持つ一式を組む。
fn alias_harness(dir: &TempDir) -> Harness {
    let harness = Harness::new();
    harness
        .host
        .set_alias_data_directory(Some(dir.path().to_path_buf()));
    harness
}

/// 一覧が返す名前を、生産経路と同じ関数から得る。
fn listed_alias_names(dir: &TempDir) -> Vec<String> {
    crate::alias::list_object_aliases(
        dir.path(),
        None,
        &default_page_window(),
        0,
        &crate::alias::DiskAliasFiles,
    )
    .items
    .into_iter()
    .map(|item| item.name)
    .collect()
}

/// 登録済みエイリアス名を作成元とする要求を組み立てる。
fn create_from_alias_name(
    harness: &Harness,
    name: &str,
    layer: u32,
    frame: u32,
) -> CreateObjectParams {
    CreateObjectParams {
        source: ObjectSource::AliasName {
            name: name.to_string(),
        },
        placement: Placement {
            scene_id: SCENE_ID,
            layer,
            frame,
        },
        expected_project_epoch: harness.epoch(),
    }
}

#[test]
fn every_alias_name_in_the_items_reaches_the_creation_api() {
    // 一覧に載る名前は必ず作成できる。載る/載らないの一致だけを見ても、作成が
    // 実際に通ることは分からない。SDK へ届いた回数で数える。
    let (dir, _) = alias_fixture();
    let listed = listed_alias_names(&dir);
    assert!(listed.len() > 1, "fixture が痩せています: {listed:?}");

    for name in &listed {
        let harness = alias_harness(&dir);
        harness.host.clear_calls();
        harness
            .edit
            .create_object(&create_from_alias_name(&harness, name, 1, 600))
            .unwrap_or_else(|error| panic!("{name} から作成できませんでした: {error}"));

        let calls = harness.host.calls();
        assert!(
            calls.contains(&"create_object_from_alias"),
            "{name} が生テキストの作成 API へ届いていません: {calls:?}"
        );
    }
}

#[test]
fn every_alias_name_missing_from_the_items_is_refused_with_the_documented_failure() {
    // 一覧から落ちた名前は、作成でも同じ条件によって落ちる。表は失敗の一覧
    // そのものであり、載らなかった名前が表に無ければテストが落ちる。
    let (dir, fixture) = alias_fixture();
    let listed: std::collections::BTreeSet<String> = listed_alias_names(&dir).into_iter().collect();
    let expected = [
        (
            "不正な.名前",
            ErrorCode::InvalidArgument,
            Some("forbidden_character"),
        ),
        ("巨大", ErrorCode::InvalidArgument, Some("too_long")),
        (
            "BOM付き",
            ErrorCode::InvalidArgument,
            Some("alias_not_parsable"),
        ),
        (
            "非UTF8",
            ErrorCode::InvalidArgument,
            Some("alias_not_parsable"),
        ),
        (
            "効果なし",
            ErrorCode::InvalidArgument,
            Some("alias_without_effect"),
        ),
    ];

    let mut refused = 0;
    for name in &fixture {
        if listed.contains(name) {
            continue;
        }
        let (_, code, reason) = expected
            .iter()
            .find(|(candidate, _, _)| candidate == name)
            .unwrap_or_else(|| panic!("{name} の失敗が表にありません"));
        let harness = alias_harness(&dir);
        let Err(error) = harness
            .edit
            .create_object(&create_from_alias_name(&harness, name, 1, 600))
        else {
            panic!("{name} から作成できてしまいました");
        };

        assert_eq!(error.error_code(), *code, "{name}");
        assert_eq!(
            error.details().get("reason").and_then(|v| v.as_str()),
            *reason,
            "{name}"
        );
        assert_eq!(harness.host.enter_calls(), 0, "{name} が区間へ入りました");
        harness.assert_untouched();
        refused += 1;
    }
    assert_eq!(refused, expected.len(), "落ちた名前の数が表と違います");
}

#[test]
fn an_alias_name_with_no_file_is_reported_as_not_found() {
    // 不在は名前を持たない。コードそのものが失敗を述べており、添えても要求元の
    // 分岐は増えない。
    let (dir, _) = alias_fixture();
    let harness = alias_harness(&dir);
    let error = harness
        .edit
        .create_object(&create_from_alias_name(&harness, "存在しない", 1, 600))
        .expect_err("存在しない名前から作成できました");

    assert_eq!(error.error_code(), ErrorCode::NotFound);
    assert!(error.details().get("reason").is_none());
    assert_eq!(harness.host.enter_calls(), 0);
    harness.assert_untouched();
}

#[test]
fn an_unresolvable_data_directory_is_told_apart_from_a_bad_name() {
    // 正しい名前で解決できなければ、要求そのものは正しく、この AviUtl2 では
    // 機能が使えないことを述べている。invalid_argument にすると、要求元は
    // 正しい名前を直そうとする。
    let harness = Harness::new();
    let error = harness
        .edit
        .create_object(&create_from_alias_name(&harness, "正常", 1, 600))
        .expect_err("データディレクトリ無しで作成できました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(
        error.details()["reason"],
        json!("alias_directory_unavailable")
    );
    assert_eq!(harness.host.enter_calls(), 0);
    harness.assert_untouched();

    // 名前の規則はディレクトリを要さずに決まる。解決できない環境で誤った名前を
    // 送ると、返るのは名前の側である。順序が逆だと、直せる誤りが「この AviUtl2
    // では使えない」として返り、要求元は名前を直す手掛かりを失う。
    for (name, reason) in [
        (r"..\エイリアス", "forbidden_character"),
        ("", "empty"),
        ("エイリアス\0", "contains_nul"),
    ] {
        let harness = Harness::new();
        let Err(error) = harness
            .edit
            .create_object(&create_from_alias_name(&harness, name, 1, 600))
        else {
            panic!("{name:?} から作成できてしまいました");
        };

        assert_eq!(error.error_code(), ErrorCode::InvalidArgument, "{name:?}");
        assert_eq!(error.details()["reason"], json!(reason), "{name:?}");
        harness.assert_untouched();
    }
}

#[test]
fn an_alias_name_is_diagnosed_before_the_preconditions_are_checked() {
    // 検査が区間の外にある帰結として、alias 側の失敗が前提条件より先に返る。
    // 期限切れの epoch・ロックされたレイヤー・塞がった宛先のいずれと組み合わせ
    // ても同じである。復旧の手が違う——再送では直らない誤りを、再送の前に伝える。
    let (dir, _) = alias_fixture();
    for (label, layer, frame) in [
        ("空きのある宛先", 1, 600),
        ("ロックされたレイヤー", 2, 600),
        ("塞がった宛先", 1, 150),
    ] {
        let harness = alias_harness(&dir);
        let mut params = create_from_alias_name(&harness, "存在しない", layer, frame);
        params.expected_project_epoch = "別のプロジェクト".to_string();
        let Err(error) = harness.edit.create_object(&params) else {
            panic!("{label} で作成できてしまいました");
        };

        assert_eq!(error.error_code(), ErrorCode::NotFound, "{label}");
        assert_eq!(harness.host.enter_calls(), 0, "{label}");
        harness.assert_untouched();
    }

    // 受け入れ規則を通る名前なら、前提条件の失敗がそのまま返る。alias 側が
    // 常に勝つ実装でも上の 3 件は通ってしまう。
    let harness = alias_harness(&dir);
    let mut params = create_from_alias_name(&harness, "正常", 1, 600);
    params.expected_project_epoch = "別のプロジェクト".to_string();
    let error = harness
        .edit
        .create_object(&params)
        .expect_err("別プロジェクトの前提が受理されました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));
    harness.assert_untouched();
}

#[test]
fn an_unsupported_media_file_is_diagnosed_after_the_preconditions() {
    // メディアの対応確認は SDK の区間内 API を要するため、区間の内側にある。
    // 軸は「区間の外で答えが出るか」の 1 つであり、種別ごとに順序を決めては
    // いない。前提条件が先に返ることが、その軸のもう一方の側である。
    let harness = Harness::new();
    let error = harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::MediaFile {
                path: r"C:\media\clip.xyz".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 600,
            },
            expected_project_epoch: "別のプロジェクト".to_string(),
        })
        .expect_err("別プロジェクトの前提が受理されました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));
    harness.assert_untouched();
}

#[test]
fn an_alias_name_creates_the_same_object_as_its_raw_text() {
    // 区間へ持ち込むのは読み取った生バイト列だけである。名前で作ったものと
    // 生テキストで作ったものが違えば、途中で中身を組み立て直している。
    let (dir, _) = alias_fixture();
    let by_name = alias_harness(&dir);
    let named = by_name
        .edit
        .create_object(&create_from_alias_name(&by_name, "正常", 1, 600))
        .expect("名前から作成できませんでした");

    let raw = create_from_raw_alias(crate::alias::tests::SINGLE);
    assert_eq!(created_identity(&named), created_identity(&raw));
    assert_eq!(named.created.len(), 1);
}

/// 作成された対象の同一性を、epoch を除いて取り出す。
///
/// epoch は一式ごとに新しく作られるため突き合わせられない。同一性を決めるのは
/// fingerprint であり、その材料は SDK へ渡ったバイト列である。
fn created_identity(outcome: &EditOutcome) -> Vec<(usize, usize, usize, Fingerprint)> {
    outcome
        .created
        .iter()
        .map(|object| {
            (
                object.layer,
                object.frame_start,
                object.frame_end,
                object.selector.fingerprint.clone(),
            )
        })
        .collect()
}

/// 生テキストを作成元とする要求を、既定の配置で実行する。
fn create_from_raw_alias(alias: &str) -> EditOutcome {
    let harness = Harness::new();
    harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: alias.to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 600,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect("生テキストから作成できませんでした")
}

#[test]
fn a_creation_by_name_hands_the_sdk_the_bytes_on_disk_and_not_a_re_encoding() {
    // パースは検証にのみ使い、書き戻さない。書き戻すと改行・空行・重複キーが
    // 保存されず、同じ対象の fingerprint がパーサの版で揺れる。
    //
    // 往復がバイト列を保存する入力で確かめても、書き戻す実装との差が出ない。
    // 損失を伴う入力を選び、差が出ることを先に確かめてから突き合わせる。
    let (dir, _) = alias_fixture();
    let on_disk = dir.alias_text("改行LF");
    let rewritten = on_disk
        .parse::<aviutl2::alias::Table>()
        .expect("往復の材料がパースできません")
        .to_string();
    assert_ne!(
        rewritten, on_disk,
        "往復が保存される入力では、書き戻す実装と区別できません"
    );

    let by_name = alias_harness(&dir);
    let named = by_name
        .edit
        .create_object(&create_from_alias_name(&by_name, "改行LF", 1, 600))
        .expect("名前から作成できませんでした");

    assert_eq!(
        created_identity(&named),
        created_identity(&create_from_raw_alias(&on_disk)),
        "SDK へ渡ったのがディスク上のバイト列ではありません"
    );
    // 書き戻したバイト列とは別物になる。同じであれば、この検査は差を
    // 捕まえられていない。
    assert_ne!(
        created_identity(&named),
        created_identity(&create_from_raw_alias(&rewritten)),
        "書き戻した文字列と区別が付いていません"
    );
}

#[test]
fn the_response_of_a_creation_by_name_carries_neither_the_alias_text_nor_a_path() {
    let (dir, _) = alias_fixture();
    let harness = alias_harness(&dir);
    let outcome = harness
        .edit
        .create_object(&create_from_alias_name(&harness, "正常", 1, 600))
        .expect("名前から作成できませんでした");

    let document = serde_json::to_string(&outcome).expect("応答の直列化");
    for forbidden in ["こんにちは", "frame=0,80", "effect.name", "Alias"] {
        assert!(
            !document.contains(forbidden),
            "{forbidden} が応答に含まれます: {document}"
        );
    }
    assert!(
        !document.contains(&dir.path().display().to_string()),
        "データディレクトリの絶対パスが応答に含まれます: {document}"
    );
}

#[test]
fn the_raw_alias_source_does_not_require_the_structure_the_admission_rule_requires() {
    // 生テキストの経路には構造の条件を掛けない。effect を 1 つも持たない
    // エイリアスは、名前で指定すれば拒否されるが生テキストでは通る。掛けると
    // 既存の受理範囲を狭め、一覧と作成の一致に寄与しないまま互換を壊す。
    let alias = "[Object]\r\nX=0.0\r\n";
    let harness = Harness::new();
    harness.host.clear_calls();
    harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: alias.to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 600,
            },
            expected_project_epoch: harness.epoch(),
        })
        .unwrap_or_else(|error| panic!("{alias:?} が拒否されました: {error}"));

    assert!(
        harness.host.calls().contains(&"create_object_from_alias"),
        "{alias:?} が SDK へ届いていません"
    );
}

#[test]
fn a_raw_alias_that_is_not_a_table_is_refused_under_the_same_name_as_a_named_one() {
    // 表として読めなければ移動行を 1 行も見られない。検証を掛けられない入力を
    // 黙って通すと、塞いだはずの口がその形の入力に対してだけ開いたままになる。
    let harness = Harness::new();
    harness.host.clear_calls();
    let error = harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "\u{feff}[Object]\r\n".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 600,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect_err("受理されました");

    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    assert_eq!(
        error.details()["reason"],
        json!(crate::alias::REASON_ALIAS_NOT_PARSABLE)
    );
    assert!(
        !harness.host.calls().contains(&"create_object_from_alias"),
        "拒否した要求が SDK へ届いています"
    );
}

/// 生テキストを作成元とする要求を組み立てる。
fn create_from_raw_alias_params(harness: &Harness, alias: &str) -> CreateObjectParams {
    CreateObjectParams {
        source: ObjectSource::ObjectAlias {
            alias: alias.to_string(),
        },
        placement: Placement {
            scene_id: SCENE_ID,
            layer: 1,
            frame: 600,
        },
        expected_project_epoch: harness.epoch(),
    }
}

/// 評価の死んだ移動行を 1 行だけ持つ生テキスト。
const ALIAS_WITH_A_DEAD_MOVEMENT: &str = "[Object]\r\nframe=0,80\r\n[Object.0]\r\neffect.name=標準描画\r\nX=-600.00,600.00,直線移動,8\r\n";

#[test]
fn a_raw_alias_whose_movement_row_cannot_be_written_is_refused_before_the_edit_section() {
    // ホストは不正な移動行を失敗として返さず、その行ごと捨てる。区間へ入る前に
    // 落ちるため、オブジェクトは 1 つも作られない。
    let harness = Harness::new();
    harness.host.clear_calls();
    let before = harness.object_count();
    let error = harness
        .edit
        .create_object(&create_from_raw_alias_params(
            &harness,
            ALIAS_WITH_A_DEAD_MOVEMENT,
        ))
        .expect_err("評価の死んだ移動行から作成できました");

    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    let details = error.details();
    assert_eq!(details["reason"], json!("track_flags_not_representable"));
    // どの節のどの項目かが分からなければ、要求元は直す行を選べない。
    assert_eq!(details["heading"], json!("Object.0"));
    assert_eq!(details["item"], json!("X"));
    assert_eq!(harness.object_count(), before);
    assert!(harness.host.enter_calls() == 0, "編集区間へ入りました");
    harness.assert_untouched();
}

#[test]
fn the_rejection_of_a_raw_alias_carries_neither_its_text_nor_the_value_of_the_row() {
    let harness = Harness::new();
    let error = harness
        .edit
        .create_object(&create_from_raw_alias_params(
            &harness,
            ALIAS_WITH_A_DEAD_MOVEMENT,
        ))
        .expect_err("評価の死んだ移動行から作成できました");

    let document = format!("{} {}", error, error.details());
    for forbidden in [
        "-600.00",
        "直線移動",
        "frame=0,80",
        "effect.name",
        "[Object]",
    ] {
        assert!(
            !document.contains(forbidden),
            "{forbidden} が応答に含まれます: {document}"
        );
    }
}

#[test]
fn a_creation_by_name_is_not_held_to_the_movement_rows_the_raw_text_is() {
    // 一覧は移動行を見ていない。作成にだけ条件を足せば「一覧に出た名前は必ず
    // 作成できる」が崩れ、一覧に載る名前が作れなくなる。
    let (dir, _) = alias_fixture();
    dir.write_alias("死んだ移動", ALIAS_WITH_A_DEAD_MOVEMENT.as_bytes());
    let harness = alias_harness(&dir);
    assert!(
        listed_alias_names(&dir).contains(&"死んだ移動".to_string()),
        "fixture が一覧に載っていません"
    );

    harness
        .edit
        .create_object(&create_from_alias_name(&harness, "死んだ移動", 1, 600))
        .expect("一覧に載る名前から作成できませんでした");
    // 生テキストとして同じバイト列を渡す経路は拒否する。
    let harness = Harness::new();
    harness
        .edit
        .create_object(&create_from_raw_alias_params(
            &harness,
            ALIAS_WITH_A_DEAD_MOVEMENT,
        ))
        .expect_err("生テキストの経路が拒否しませんでした");
}

// -------------------------------------------------------------- 作成の応答

#[test]
fn creation_reports_the_placement_the_host_chose() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "[obj]".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 600,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect("作成に失敗しました");

    let created = outcome.object.expect("作成された対象");
    assert_eq!(
        created.frame_start,
        600 + CREATE_FRAME_SHIFT,
        "要求位置をそのまま応答へ載せています"
    );
    assert_eq!(outcome.created.len(), 1);
    assert_eq!(outcome.created[0], created);
}

#[test]
fn creation_reports_every_object_the_alias_produced() {
    // 複数オブジェクトを含む alias は各オブジェクトが自分のレイヤーを持てる。
    // 配置先だけを走査していると、別のレイヤーへ作られた分は応答に現れず、
    // 要求元は自分が作ったものを移動も削除もできない。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::CreatePair)));
    harness.host.clear_calls();
    let outcome = harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "[obj][obj]".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 0,
                frame: 600,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect("作成に失敗しました");

    assert_eq!(
        outcome.created.len(),
        2,
        "2 件目以降が要求元から到達不能になります"
    );
    assert_eq!(outcome.object.as_ref(), outcome.created.first());
    let layers: Vec<usize> = outcome.created.iter().map(|item| item.layer).collect();
    assert_eq!(layers, vec![0, 1], "別レイヤーへ作られた分が漏れています");

    // 返った selector で 2 件目を個別に削除できる。
    harness
        .edit
        .delete_object(&DeleteObjectParams {
            selector: outcome.created[1].selector.clone(),
        })
        .expect("2 件目を個別に削除できません");
}

/// フェイクが保持する、オブジェクトが存在する最大レイヤー番号。
///
/// レイヤーの本数ではない。作成で伸び、削除で縮む。
fn occupied_layer_max(harness: &Harness) -> usize {
    harness
        .host
        .scene()
        .layers
        .iter()
        .rposition(|layer| !layer.objects.is_empty())
        .unwrap_or(0)
}

#[test]
fn creation_scans_every_layer_before_and_after() {
    // 走査はシーン全体に及ぶ。オブジェクトが存在する最大レイヤーまでを、
    // 作成の前後で 1 度ずつ見る。
    let harness = Harness::new();
    let occupied = occupied_layer_max(&harness);
    harness.host.clear_calls();
    harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "[obj]".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 600,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect("作成に失敗しました");

    let calls = harness.host.calls();
    let scans = calls
        .iter()
        .filter(|call| **call == "object_placements")
        .count();
    // 配置先が既に埋まっているレイヤーであれば、作成で最大レイヤーは伸びない。
    assert_eq!(
        scans,
        (occupied + 1) * 2,
        "シーン全体の走査が作成の前後で 1 度ずつ行われていません"
    );
    assert_eq!(
        calls.iter().filter(|call| **call == LAYER_MAX).count(),
        2,
        "走査範囲を作成の前後で決め直していません"
    );
}

#[test]
fn creation_reaches_a_layer_beyond_the_range_the_request_implied() {
    // 要求から決まる範囲は「オブジェクトが存在する最大レイヤーと配置先の
    // 大きい方」までである。alias がその先のレイヤーへ展開すると、作成後に
    // 最大レイヤーを読み直さない限り 2 件目が応答に現れず、要求元は自分が
    // 作ったものへ到達できない。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::CreatePair)));
    let destination = occupied_layer_max(&harness) + 1;
    let outcome = harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "[obj][obj]".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: destination as u32,
                frame: 600,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect("作成に失敗しました");

    let layers: Vec<usize> = outcome.created.iter().map(|item| item.layer).collect();
    assert_eq!(
        layers,
        vec![destination, destination + 1],
        "要求から決まる走査範囲の外へ作られた分が漏れています"
    );

    // 返った selector で範囲外の 1 件を個別に削除できる。
    harness
        .edit
        .delete_object(&DeleteObjectParams {
            selector: outcome.created[1].selector.clone(),
        })
        .expect("走査範囲の外へ作られた対象を削除できません");
}

#[test]
fn creation_from_a_media_file_takes_the_same_difference() {
    // 経路によって差分の範囲を変えると、SDK が複数のオブジェクトを作る場合に
    // 片方だけが取りこぼす。同じ危険には同じ対処を当てる。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::CreatePair)));
    let outcome = harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::MediaFile {
                path: r"C:\media\clip.mp4".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 600,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect("作成に失敗しました");

    let layers: Vec<usize> = outcome.created.iter().map(|item| item.layer).collect();
    assert_eq!(layers, vec![1, 2], "別レイヤーへ作られた分が漏れています");
}

#[test]
fn creation_from_an_effect_name_takes_the_same_difference() {
    // 経路によって差分の範囲を変えると、SDK が複数のオブジェクトを作る場合に
    // 片方だけが取りこぼす。同じ危険には同じ対処を当てる。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::CreatePair)));
    let outcome = harness
        .edit
        .create_object(&create_from_effect(&harness, "ぼかし", 0, 600))
        .expect("作成に失敗しました");

    let layers: Vec<usize> = outcome.created.iter().map(|item| item.layer).collect();
    assert_eq!(layers, vec![0, 1], "別レイヤーへ作られた分が漏れています");
    assert_eq!(outcome.object.as_ref(), outcome.created.first());

    // 返った selector で 2 件目を個別に削除できる。
    harness
        .edit
        .delete_object(&DeleteObjectParams {
            selector: outcome.created[1].selector.clone(),
        })
        .expect("2 件目を個別に削除できません");
}

#[test]
fn a_creation_that_produces_nothing_reports_the_mutation() {
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::CreateNothing)));
    let error = harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "[obj]".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 600,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect_err("位置を特定できないのに成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(error.details()["current_project_revision"], json!(1));
    assert_eq!(error.details()["retry_requires"], json!("refetch"));
}

// ------------------------------------------------------------------ read-back

#[test]
fn a_silently_ignored_enable_change_is_not_reported_as_success() {
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreEffectState)));
    let error = harness
        .edit
        .set_effect_enabled(&SetEffectEnabledParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            enabled: false,
        })
        .expect_err("無言で無視された変更が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("effect_state_immutable"));
    assert_eq!(error.details()["mutation_issued"], json!(true));
}

/// effect のロックが変わると、読み取りの値も対象の同一性も動くことを確かめる。
///
/// ロックは effect の fingerprint の材料であり、alias へも書き出されるため
/// オブジェクトの fingerprint まで追随する。追随しなければ、要求元はロックの
/// 前後を見分けられない selector を握り続ける。
#[test]
fn locking_an_effect_changes_the_object_fingerprint() {
    let harness = Harness::new();
    let before = harness.selector(1, 100);
    let before_effect = harness
        .read
        .get_object(&before)
        .expect("ロック前の詳細を取得できません")
        .effects
        .remove(1);
    assert!(!before_effect.locked);

    harness.host.scene.lock().unwrap().layers[1].objects[0].effects[1].locked = true;

    let after = harness.selector(1, 100);
    assert_ne!(
        before.fingerprint, after.fingerprint,
        "effect のロックを変えてもオブジェクトの fingerprint が変わりません"
    );

    // 読み直した selector はそのまま次の要求へ渡せる。ロック前の selector は
    // もう一致しない。
    let after_effect = harness
        .read
        .get_object(&after)
        .expect("読み直した selector で引けません")
        .effects
        .remove(1);
    assert!(
        after_effect.locked,
        "読み取りが effect のロックを返していません"
    );
    assert_ne!(
        before_effect.selector.fingerprint, after_effect.selector.fingerprint,
        "effect のロックを変えても effect の fingerprint が変わりません"
    );
    assert_eq!(
        harness.read.get_object(&before).unwrap_err().error_code(),
        ErrorCode::PreconditionFailed
    );
}

/// 変更を受け付けない状態変更が、SDK を呼ぶ前に弾かれることを確かめる。
///
/// 表で駆動する。無言で拒否される軸が増えたら行を足すだけで、同じ主張
/// （SDK を呼ばない・revision を進めない・成功として返さない）がそのまま掛かる。
#[test]
fn changes_the_host_never_applies_are_refused_before_the_sdk_is_called() {
    /// 変更を受け付けない対象と、それへ要求する状態変更。
    struct Immutable {
        /// 何を確かめているか。
        label: &'static str,
        /// 差し替える effect 名。
        effect_name: &'static str,
        /// 差し替える effect 列の位置。
        position: usize,
        /// 要求する有効・無効。
        enabled: bool,
    }

    let scenarios = [
        // 出力項目の有効・無効。
        Immutable {
            label: "出力項目の enabled",
            effect_name: "標準描画",
            position: 0,
            enabled: false,
        },
    ];

    for Immutable {
        label,
        effect_name,
        position,
        enabled,
    } in scenarios
    {
        let name = effect_name.to_string();
        let harness = Harness::with(move |host| {
            let mut scene = host.scene.lock().unwrap();
            scene.layers[1].objects[0].effects[position].name = name;
            drop(scene);
        });
        let selector = harness.effect_selector(1, 100, effect_name, 0);
        let Err(error) = harness
            .edit
            .set_effect_enabled(&SetEffectEnabledParams { selector, enabled })
        else {
            panic!("{label} が変更できました");
        };

        assert_eq!(
            error.error_code(),
            ErrorCode::UnsupportedOperation,
            "{label}"
        );
        assert_eq!(
            error.details()["reason"],
            json!("effect_state_immutable"),
            "{label}"
        );
        assert_eq!(
            harness.host.enter_calls(),
            0,
            "{label} で編集区間へ入りました"
        );
        harness.assert_untouched();
        assert!(
            !harness.project.modified(),
            "{label} で未保存の変更が記録されました"
        );
    }
}

#[test]
fn a_silently_ignored_rename_is_not_reported_as_success() {
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreObjectName)));
    let error = harness
        .edit
        .set_object_name(&SetObjectNameParams {
            selector: harness.selector(1, 100),
            name: Some("新しい名前".to_string()),
        })
        .expect_err("無言で無視された改名が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("change_not_applied"));
}

#[test]
fn a_silently_ignored_deletion_is_not_reported_as_success() {
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreDelete)));
    let error = harness
        .edit
        .delete_object(&DeleteObjectParams {
            selector: harness.selector(1, 100),
        })
        .expect_err("残っている対象が削除済みとして返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("change_not_applied"));
}

#[test]
fn deletion_confirms_that_the_target_is_gone() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .delete_object(&DeleteObjectParams {
            selector: harness.selector(1, 100),
        })
        .expect("削除に失敗しました");

    assert!(outcome.object.is_none());
    assert!(outcome.effect.is_none());
    // 削除の確認は同一区間内の読み直しで行う。
    let calls = harness.host.calls();
    let deleted = calls.iter().position(|call| *call == "delete_object");
    let confirmed = calls.iter().rposition(|call| *call == "object_identity");
    assert!(
        deleted < confirmed,
        "削除後の読み直しが行われていません: {calls:?}"
    );
}

/// 配下 effect を要しない operation が effect を読まないことを確かめる。
///
/// オブジェクトの同一性は alias だけで決まる。読めば、応答に現れない値の
/// 読み取り失敗が対象の解決と反映確認を巻き込む。
#[test]
fn edits_that_do_not_need_effects_never_read_them() {
    let harness = Harness::new();
    let selector = harness.selector(1, 100);
    // 要求の組み立てに使った読み取りは対象外にする。
    harness.host.clear_calls();

    harness
        .edit
        .set_object_name(&SetObjectNameParams {
            selector,
            name: Some("新しい名前".to_string()),
        })
        .expect("改名に失敗しました");

    assert!(
        !harness.host.calls().contains(&EFFECT_LIST),
        "effect を要しない operation が effect を読みました: {:?}",
        harness.host.calls()
    );
}

/// effect を指定する operation は effect を読むことを確かめる。
///
/// 列全体の位置と総数を材料にするため、対象の effect だけを読むことはできない。
#[test]
fn edits_that_target_an_effect_read_them() {
    let harness = Harness::new();
    let selector = harness.effect_selector(1, 100, "ぼかし", 0);
    harness.host.clear_calls();

    harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector,
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 30 },
        })
        .expect("設定項目の変更に失敗しました");

    assert!(
        harness.host.calls().contains(&EFFECT_LIST),
        "effect を読まずに effect を書き換えました: {:?}",
        harness.host.calls()
    );
}

#[test]
fn an_item_value_the_host_clamps_is_reported_as_a_failure() {
    // クライアントは要求した値を得ていない。成功として返すと、逸脱に気付く
    // 手段が要求元にも利用者にも無い。**読み直した実値を添えることで、要求元は
    // 要求した値がホストの手でどうなったかを知る**——切り詰めであれば、その値が
    // 値域の境界そのものである。
    let harness = Harness::new();
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            item: "範囲".to_string(),
            value: ItemValue::Integer {
                value: MAX_ITEM_VALUE + 150,
            },
        })
        .expect_err("切り詰められた値が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("item_value_not_applied"));
    assert_eq!(
        error.details()["observed_value"],
        json!(MAX_ITEM_VALUE.to_string())
    );
    // 巻き戻したため、読み直した値は応答が返る時点の現在値ではない。
    assert_eq!(error.details()["restored"], json!(true));
}

#[test]
fn an_item_value_within_the_host_limits_is_reported_as_read_back() {
    // 応答が返すのはホストが保持している値である。要求値をそのまま返すと、
    // 照合を通った後でも応答が実態を表さなくなる。**標本は要求値と実値が
    // 異なるものでなければならない。** 同じ値だと、要求を反響させるだけの実装
    // でも通る。
    let requested = ItemValue::Color {
        value: "FFAA00".to_string(),
    };
    let stored = ItemValue::Color {
        value: "ffaa00".to_string(),
    };
    assert_ne!(
        requested, stored,
        "標本の要求値と実値が同じで、反響しているだけの実装と区別できません"
    );

    let harness = harness_with_choice_effect();
    let outcome = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 300, SHAPE, 0),
            item: "色".to_string(),
            value: requested,
        })
        .expect("ホストが受理する値が失敗として扱われました");

    assert_eq!(changed_item(&outcome, "色"), stored);
}

#[test]
fn an_unknown_item_name_is_not_found() {
    let harness = Harness::new();
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            item: "存在しない項目".to_string(),
            value: ItemValue::Integer { value: 1 },
        })
        .expect_err("存在しない設定項目へ書き込めました");

    assert_eq!(error.error_code(), ErrorCode::NotFound);
    assert_eq!(error.details()["item"], json!("存在しない項目"));
    assert_eq!(
        harness
            .host
            .calls()
            .iter()
            .filter(|call| **call == ITEM_VALUE)
            .count(),
        1,
        "項目の存在を確かめる読み取りが行われていません"
    );
    harness.assert_untouched();
}

/// 設定項目の列挙に現れない項目名を持つフェイクを組む。
///
/// 列挙は effect カタログが公開する一覧から作られる。カタログに無い項目を
/// オブジェクト側だけに持たせると、列挙には現れないが名前で値を読める状態を
/// 再現できる。
fn harness_with_unlisted_item() -> Harness {
    Harness::with(|host| {
        let mut scene = host.scene.lock().unwrap();
        scene.layers[1].objects[0].effects[1]
            .items
            .push(EffectItem {
                name: "未知種別の項目".to_string(),
                item_type: EffectItemType::Unknown(99),
                value: ItemValue::Unknown {
                    raw: "future=1".to_string(),
                },
                track: None,
            });
        drop(scene);
    })
}

#[test]
fn an_item_missing_from_the_listing_but_readable_is_not_writable() {
    // 列挙は未知種別の項目を落とす。落ちた項目への書き込みを「項目が見つから
    // ない」として返すと、要求元は存在しない問題を指す失敗を受け取る。
    let harness = harness_with_unlisted_item();
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            item: "未知種別の項目".to_string(),
            value: ItemValue::Integer { value: 1 },
        })
        .expect_err("未知種別の項目へ書き込めました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("item_type_not_writable"));
    harness.assert_untouched();
}

/// 記録された呼び出しのうち、最初に変更 API が現れた位置。
fn first_mutation(calls: &[&'static str]) -> Option<usize> {
    calls.iter().position(|call| MUTATIONS.contains(call))
}

/// 記録された呼び出しのうち、指定した呼び出しが現れた回数。
fn count(calls: &[&'static str], call: &str) -> usize {
    calls.iter().filter(|recorded| **recorded == call).count()
}

#[test]
fn a_successful_write_reads_the_value_back_exactly_once() {
    // ホストは書き込みの成否を返さない。成功経路でも読み直さなければ、要求した
    // 値が入ったことを誰も確かめていない。**費用は 1 回に留める。**
    let harness = harness_with_unlisted_item();
    let selector = harness.effect_selector(1, 100, "ぼかし", 0);
    harness.host.clear_calls();

    harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector,
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 30 },
        })
        .expect("設定項目の変更に失敗しました");

    let calls = harness.host.calls();
    let first = first_mutation(&calls).expect("変更 API が呼ばれていません");
    assert_eq!(
        count(&calls[first..], ITEM_VALUE),
        1,
        "照合の読み直しは 1 回だけです: {calls:?}"
    );
}

#[test]
fn a_verified_write_reads_the_value_once_before_the_write() {
    // **書き込みの前に読むのは巻き戻しの材料である。** 照合が落ちたときに
    // 書き戻す生文字列は、発行してしまえば失われる。したがって組み合わせに
    // よらず 1 回読む——移動の事前確認が要らない要求（移動を書く要求、移動を
    // 持ち得ない種別）でも読む。移動の有無の判定は同じ文字列を使うため、
    // 読み取りが 2 回になることはない。
    let cases: [(&str, ItemValue, usize); 3] = [
        (
            STATIC_ITEM,
            ItemValue::Number {
                value: FiniteF64::try_new(30.0).expect("有限値"),
            },
            1,
        ),
        (STATIC_ITEM, movement(&[0.0, 50.0, 100.0], "直線移動"), 1),
        (
            // 移動を持ち得ない種別の代表。選択肢の effect が持つ。
            "メモ",
            ItemValue::Text {
                value: "覚書".to_string(),
            },
            1,
        ),
    ];
    for (item, value, expected) in cases {
        let text_item = item == "メモ";
        let harness = match text_item {
            true => harness_with_choice_effect(),
            false => harness_with_track_effect(),
        };
        let selector = match text_item {
            true => harness.effect_selector(1, 300, SHAPE, 0),
            false => harness.effect_selector(1, 100, COORDINATE, 0),
        };
        harness.host.clear_calls();
        harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector,
                item: item.to_string(),
                value: value.clone(),
            })
            .unwrap_or_else(|error| panic!("{item} の書き込みに失敗しました: {error}"));

        let calls = harness.host.calls();
        let first = first_mutation(&calls).expect("変更 API が呼ばれていません");
        assert_eq!(
            count(&calls[..first], ITEM_VALUE),
            expected,
            "{item} へ {} を書く前の読み取り回数が想定と異なります: {calls:?}",
            value.kind()
        );
    }
}

/// 選択肢から選ぶ設定項目を持つ effect を足したフェイクを組む。
///
/// カタログと対象オブジェクトの双方へ同じ effect を足す。種別はカタログの
/// 一覧から引かれるため、両方を揃えないと本番と同じ経路を通らない。
fn harness_with_choice_effect() -> Harness {
    Harness::with(|host| {
        host.catalog.push(shape_catalog_entry());
        host.scene.get_mut().unwrap().layers[1].objects[1]
            .effects
            .push(shape(0));
    })
}

/// 選択肢から選ぶ設定項目の名前。種別はそれぞれ別である。
const CHOICE_ITEMS: [&str; 3] = ["図形の種類", "マスクの種類", "形状"];

/// 選択肢を持つ項目への書き込み要求を組み立てる。
fn set_choice_item(harness: &Harness, item: &str, value: &str) -> SetObjectItemParams {
    SetObjectItemParams {
        selector: harness.effect_selector(1, 300, SHAPE, 0),
        item: item.to_string(),
        value: ItemValue::Choice {
            value: value.to_string(),
        },
    }
}

/// 選択肢を持つ項目のうち 1 つへの書き込み要求を組み立てる。
fn set_choice(harness: &Harness, value: &str) -> SetObjectItemParams {
    set_choice_item(harness, CHOICE_ITEMS[0], value)
}

#[test]
fn the_choice_items_of_the_fake_have_distinct_item_types() {
    // 名前が 3 つあっても種別が 1 つなら、種別ごとに経路が分かれても選択肢の
    // 試験群は気付けない。
    let items = shape(0).items;
    let types: Vec<EffectItemType> = CHOICE_ITEMS
        .iter()
        .map(|name| {
            items
                .iter()
                .find(|item| item.name == *name)
                .unwrap_or_else(|| panic!("設定項目 {name} がありません"))
                .item_type
                .clone()
        })
        .collect();
    assert_eq!(
        types,
        vec![
            EffectItemType::Select,
            EffectItemType::Mask,
            EffectItemType::Figure,
        ]
    );
}

/// 応答が返した effect から、指定した設定項目の値を取り出す。
fn changed_item(outcome: &EditOutcome, item: &str) -> ItemValue {
    outcome
        .effect
        .as_ref()
        .expect("変更後の effect")
        .items
        .iter()
        .find(|entry| entry.name == item)
        .unwrap_or_else(|| panic!("設定項目 {item} がありません"))
        .value
        .clone()
}

/// 区間 1 個分の移動を持つ値。
fn movement(values: &[f64], mode: &str) -> ItemValue {
    ItemValue::Track(aviutl2_mcp_core::TrackValue {
        values: values
            .iter()
            .map(|value| FiniteF64::try_new(*value).expect("有限値"))
            .collect(),
        mode: Some(mode.to_string()),
        params: Vec::new(),
        accelerate: false,
        decelerate: false,
        twopoint: false,
        reserved_flags: 0,
    })
}

/// 移動を持つ項目と持たない項目を備えたフェイクを組む。
///
/// カタログと対象オブジェクトの双方へ同じ effect を足す。種別はカタログの
/// 一覧から引かれるため、両方を揃えないと本番と同じ経路を通らない。
///
/// **中間点の数が違う 2 つの対象へ足す。** レイヤー 1 フレーム 100 の対象は
/// 中間点を 1 つ持ち、区間 2 個に対して値は 3 個である。フレーム 300 の対象は
/// 中間点を持たず、区間 1 個に対して値は 2 個である。1 つしか置かないと、
/// 「値の個数は区間数 + 1」の規則が片側の数でしか固定されない。
fn harness_with_track_effect() -> Harness {
    Harness::with(|host| {
        host.catalog.push(coordinate_catalog_entry());
        let layer = &mut host.scene.get_mut().unwrap().layers[1];
        layer.objects[0]
            .effects
            .push(coordinate(0, &[0.0, 50.0, 100.0]));
        layer.objects[1].effects.push(coordinate(0, &[0.0, 100.0]));
    })
}

/// 中間点を持たない対象のトラックバーへ値を書き込む要求を組み立てる。
fn set_track_item_without_midpoints(
    harness: &Harness,
    item: &str,
    value: ItemValue,
) -> SetObjectItemParams {
    SetObjectItemParams {
        selector: harness.effect_selector(1, 300, COORDINATE, 0),
        item: item.to_string(),
        value,
    }
}

/// トラックバーへ値を書き込む要求を組み立てる。
fn set_track_item(harness: &Harness, item: &str, value: ItemValue) -> SetObjectItemParams {
    SetObjectItemParams {
        selector: harness.effect_selector(1, 100, COORDINATE, 0),
        item: item.to_string(),
        value,
    }
}

/// 移動を持つ項目へ書き込む要求を組み立てる。
fn set_movement(harness: &Harness, value: ItemValue) -> SetObjectItemParams {
    set_track_item(harness, MOVING_ITEM, value)
}

#[test]
fn a_movement_the_host_knows_is_written_and_read_back() {
    // 一覧にある移動方法は書ける。ホストが桁を整えて返しても照合は通る。
    let harness = harness_with_track_effect();
    let outcome = harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            movement(&[0.0, 50.0, 100.0], "曲線移動"),
        ))
        .expect("一覧にある移動方法が拒否されました");

    assert_eq!(
        changed_item(&outcome, MOVING_ITEM),
        movement(&[0.0, 50.0, 100.0], "曲線移動")
    );
    assert!(harness.host.fatal_movement_writes().is_empty());
}

#[test]
fn a_scalar_that_would_erase_a_movement_is_refused() {
    // ホストは移動を持つ項目へ数値を書くと、移動も加速も中間点無視も捨てて
    // 成功を返す。生の文字列を渡しても同じであり、止められる場所はここしかない。
    let harness = harness_with_track_effect();
    let error = harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            ItemValue::Number {
                value: FiniteF64::try_new(0.0).expect("有限値"),
            },
        ))
        .expect_err("移動を消す書き込みが成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("track_movement_present"));
    // 対象がいま持つ移動を載せる。要求元はこれを読んで書き戻すか消すかを決める。
    assert_eq!(
        error.details()["current_value"],
        json!("0.00,50.00,100.00,直線移動,0|")
    );
    // 書き込みは発行していない。発行してしまえば移動は復元できない。
    harness.assert_untouched();
}

#[test]
fn a_movement_can_be_added_to_an_item_that_has_none() {
    // **アニメーションを作る経路がこれである。** 静的なトラックバーへ移動を
    // 書くと新しく移動が付く。現在値で拒むと、移動は既に移動を持つ項目にしか
    // 書けなくなり、alias から作り直すほかなくなる。
    let harness = harness_with_track_effect();
    let requested = movement(&[0.0, 50.0, 100.0], "直線移動");
    let outcome = harness
        .edit
        .set_object_item(&set_track_item(&harness, STATIC_ITEM, requested.clone()))
        .expect("移動を持たない項目へ移動を書けませんでした");

    assert_eq!(changed_item(&outcome, STATIC_ITEM), requested);

    // 読み直しても移動が付いている。応答だけが移動を名乗る状態と区別する。
    let detail = harness
        .read
        .get_object(&harness.selector(1, 100))
        .expect("対象の詳細");
    let item = detail
        .effects
        .iter()
        .find(|effect| effect.name == COORDINATE)
        .expect("effect がありません")
        .items
        .iter()
        .find(|item| item.name == STATIC_ITEM)
        .expect("設定項目がありません")
        .clone();
    assert_eq!(item.value, requested);
    assert_eq!(
        item.track.expect("移動情報").mode,
        "直線移動",
        "移動情報が付いていません"
    );
}

#[test]
fn a_movement_can_be_added_to_an_object_without_midpoints() {
    // 中間点を持たない対象の静的なトラックバーへ 2 値の移動を書くと成功する
    // （実測）。区間は 1 個であり、値は 2 個でなければならない。
    let harness = harness_with_track_effect();
    let requested = movement(&[0.0, 100.0], "直線移動");
    let outcome = harness
        .edit
        .set_object_item(&set_track_item_without_midpoints(
            &harness,
            STATIC_ITEM,
            requested.clone(),
        ))
        .expect("中間点を持たない対象へ移動を書けませんでした");
    assert_eq!(changed_item(&outcome, STATIC_ITEM), requested);

    // 区間 1 個に対して 3 値は多い。個数の規則は区間の数で決まり、両端で効く。
    let harness = harness_with_track_effect();
    let error = harness
        .edit
        .set_object_item(&set_track_item_without_midpoints(
            &harness,
            STATIC_ITEM,
            movement(&[0.0, 50.0, 100.0], "直線移動"),
        ))
        .expect_err("区間の数と合わない値が受理されました");
    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    assert_eq!(error.details()["reason"], json!("track_value_count"));

    // 同じ 2 値を、中間点を 1 つ持つ対象へ書くと個数が足りない。
    let harness = harness_with_track_effect();
    let error = harness
        .edit
        .set_object_item(&set_track_item(&harness, STATIC_ITEM, requested))
        .expect_err("区間の数と合わない値が受理されました");
    assert_eq!(error.details()["reason"], json!("track_value_count"));
}

#[test]
fn a_movement_written_to_an_item_that_cannot_hold_one_is_refused() {
    // ホストは移動を持ち得ない種別へ多値の文字列を渡しても先頭の値だけを使う。
    // 拒むのは種別と値の形の照合であり、移動の有無を見る判定ではない。
    let harness = harness_with_choice_effect();
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 300, SHAPE, 0),
            item: "メモ".to_string(),
            value: movement(&[0.0, 100.0], "直線移動"),
        })
        .expect_err("移動を持ち得ない種別への移動が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    harness.assert_untouched();
}

#[test]
fn a_movement_is_removed_by_writing_a_value_without_a_mode() {
    // 移動を消す手段はこれだけである。**表せるのだから、数値の書き込みが黙って
    // 消すことを成功として返す理由が無い。**
    let harness = harness_with_track_effect();
    let outcome = harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            ItemValue::Track(aviutl2_mcp_core::TrackValue {
                values: vec![FiniteF64::try_new(50.0).expect("有限値")],
                mode: None,
                params: Vec::new(),
                accelerate: false,
                decelerate: false,
                twopoint: false,
                reserved_flags: 0,
            }),
        ))
        .expect("移動を消す書き込みが拒否されました");

    assert_eq!(
        changed_item(&outcome, MOVING_ITEM),
        ItemValue::Number {
            value: FiniteF64::try_new(50.0).expect("有限値"),
        }
    );
    // 消した後は移動を持たない項目になる。数値で書き換えられる。
    harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            ItemValue::Number {
                value: FiniteF64::try_new(10.0).expect("有限値"),
            },
        ))
        .expect("移動を消した後の数値の書き込みが拒否されました");
}

#[test]
fn a_write_stops_when_the_current_value_cannot_be_read() {
    // 現在値を読めなければ移動の有無が分からない。読めないまま書き込むと、
    // 判定を迂回して移動が消え得る。**読めないことは、通してよい理由にならない。**
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::ItemValueUnreadable)));
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 40 },
        })
        .expect_err("現在値を読めないまま書き込みました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    harness.assert_untouched();
}

#[test]
fn a_movement_check_without_its_material_is_refused() {
    // 材料を読む条件と、移動の判定が要る条件は別の述語である。今日は前者が
    // 後者を含むが、片方だけを変えれば包含は破れる。そのとき「読んでいないから
    // 判定しない」と倒すと、移動を黙って消す書き込みが素通りする。**到達不能で
    // あることを根拠に分岐を消さず、到達したら落ちる形にする。**
    let items = vec![AvailableEffectItem {
        name: MOVING_ITEM.to_string(),
        item_type: EffectItemType::Number,
    }];
    let scalar = ItemValue::Number {
        value: FiniteF64::try_new(0.0).expect("有限値"),
    };
    let error = ensure_movement_write_with_origin(&items, MOVING_ITEM, &scalar, None)
        .expect_err("材料が無いまま移動の判定が素通りしました");
    assert_eq!(error.details()["reason"], json!("inverse_unavailable"));

    // 判定が要らない組み合わせは、材料が無くても通る。判定の対象そのものが無い。
    ensure_movement_write_with_origin(
        &items,
        MOVING_ITEM,
        &movement(&[0.0, 100.0], "直線移動"),
        None,
    )
    .expect("移動を書く要求まで拒否されました");

    // 材料があれば普段どおり判定する。拒否の向きは現在値が決める。
    ensure_movement_write_with_origin(
        &items,
        MOVING_ITEM,
        &scalar,
        Some("0.00,100.00,直線移動,0|"),
    )
    .expect_err("移動を消す書き込みが通りました");
    ensure_movement_write_with_origin(&items, MOVING_ITEM, &scalar, Some("50.00"))
        .expect("移動を持たない項目への数値が拒否されました");
}

/// 編集手順が実際に返した「移動が消える」失敗を集める。
///
/// 名前を生む経路が製品に在ることの裏付けとして用いる。一覧から値を組み立てる
/// のでは、返す呼び出しが 1 つも無くても検査が通ってしまう。
pub(crate) fn produced_movement_loss_failures() -> Vec<EditError> {
    let harness = harness_with_track_effect();
    vec![
        harness
            .edit
            .set_object_item(&set_movement(
                &harness,
                ItemValue::Number {
                    value: FiniteF64::try_new(0.0).expect("有限値"),
                },
            ))
            .expect_err("移動を消す書き込みが成功として返りました"),
    ]
}

#[test]
fn the_movement_loss_has_a_request_that_produces_it() {
    for failure in produced_movement_loss_failures() {
        assert_eq!(
            failure.details()["reason"],
            json!("track_movement_present"),
            "別の失敗が返りました"
        );
    }
}

#[test]
fn a_movement_read_from_the_object_can_be_written_straight_back() {
    // 読み取りが返した移動をそのまま書き戻せる。ホストが桁を整えても往復は
    // 成立し、対象の同一性も動かない。
    let harness = harness_with_track_effect();
    let selector = harness.selector(1, 100);
    let detail = harness.read.get_object(&selector).expect("対象の詳細");
    let effect = detail
        .effects
        .iter()
        .find(|effect| effect.name == COORDINATE)
        .expect("effect がありません")
        .clone();
    let value = effect
        .items
        .iter()
        .find(|item| item.name == MOVING_ITEM)
        .expect("設定項目がありません")
        .value
        .clone();
    assert!(
        matches!(value, ItemValue::Track(_)),
        "移動が移動として読めません: {value:?}"
    );

    let outcome = harness
        .edit
        .set_object_item(&set_movement(&harness, value.clone()))
        .expect("読み取った移動を書き戻せませんでした");

    assert_eq!(changed_item(&outcome, MOVING_ITEM), value);
    // 書き戻しても対象は変わっていない。fingerprint は設定値まで含めて算出
    // されるため、値が動けばここが食い違う。
    assert_eq!(outcome.object.expect("対象の概要").selector, selector);
    assert_eq!(outcome.effect.expect("effect"), effect);
}

#[test]
fn a_movement_with_an_unknown_mode_never_reaches_the_host() {
    // 一覧に無い移動方法を書くと実機はプロセスごと落ちる。**止められるのは
    // 書き込みの手前だけである。** 記録が空でなければ、検証を通り抜けた入力が
    // ホストへ届いている。panic は編集の入口が捕捉して失敗の応答へ畳むため、
    // 応答の形だけを見ても届いたことは分からない。
    let harness = harness_with_track_effect();
    let error = harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            movement(&[0.0, 50.0, 100.0], "存在しない移動"),
        ))
        .expect_err("存在しない移動方法が受理されました");

    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    assert_eq!(error.details()["reason"], json!("track_mode_unknown"));
    assert_eq!(
        harness.host.fatal_movement_writes(),
        Vec::<String>::new(),
        "落ちる移動方法がホストへ届きました"
    );
}

#[test]
fn a_rejected_movement_name_never_comes_back_in_the_failure() {
    // known_movements が運ぶのはホストの一覧であり、拒否された要求の名前では
    // ない。一覧に無いからこそ拒否されているのだから、要求の名前が紛れ込めば
    // それ自体が矛盾になる。
    //
    // **要求を通して組み立てた失敗で確かめる。** 手で組み立てた失敗では、
    // 要求の名前が応答へ入り込む経路そのものを通らない。
    let harness = harness_with_track_effect();
    let requested = "存在しない移動";
    let error = harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            movement(&[0.0, 50.0, 100.0], requested),
        ))
        .expect_err("存在しない移動方法が受理されました");

    let details = error.details();
    assert!(
        !details.to_string().contains(requested),
        "拒否された要求の名前が応答に現れました: {details}"
    );
    // 一覧そのものは運ぶ。運ばなければ要求元は選び直す材料を持たない。
    assert_eq!(
        details["known_movements"]
            .as_array()
            .expect("配列です")
            .len(),
        TRACK_MODES.len()
    );
}

#[test]
fn what_the_list_calls_unwritable_is_what_a_raw_alias_refuses() {
    // **移動を書く経路は 2 本あり、どちらも同じ 1 つの表を読む。** 片方にだけ
    // 条件を足せば、生テキストで作れるオブジェクトと設定項目として書ける値が
    // 食い違う。一覧を渡し損ねた実装も、可否を落とした一覧を渡す実装も、
    // ここで落ちる。
    let movements = vec![
        Movement {
            name: "直線移動".to_string(),
            writable: true,
        },
        Movement {
            name: "移動無し".to_string(),
            writable: false,
        },
    ];
    for movement_entry in &movements {
        let harness = Harness::new();
        harness.host.set_movements(movements.clone());
        let alias = format!(
            "[Object]\r\nframe=0,80\r\n[Object.0]\r\neffect.name=標準描画\r\nX=0.00,100.00,{},0\r\n",
            movement_entry.name
        );
        let result = harness
            .edit
            .create_object(&create_from_raw_alias_params(&harness, &alias));
        if movement_entry.writable {
            result.unwrap_or_else(|error| {
                panic!("{} が拒否されました: {error}", movement_entry.name)
            });
        } else {
            let error = result.expect_err("書けない移動方法が受理されました");
            assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
            assert_eq!(
                error.details()["reason"],
                json!("track_mode_not_writable"),
                "{} の拒否の理由",
                movement_entry.name
            );
            // 名前を選び直す手は通らない。一覧に無い名前とは別の失敗である。
            assert_ne!(error.details()["reason"], json!("track_mode_unknown"));
            harness.assert_untouched();
        }
    }
}

#[test]
fn what_the_list_calls_unwritable_is_what_set_object_item_refuses() {
    // **一覧と拒否が同じ表を読む。** 一覧が返した 1 件ずつについて、書けないと
    // 名乗ったものは書き込みが拒み、書けると名乗ったものは名前を理由に拒まれ
    // ない。名前を書き並べた検査は、一覧が変わったときにこの規律を守らない。
    let harness = harness_with_track_effect();
    let movements = vec![
        Movement {
            name: "直線移動".to_string(),
            writable: true,
        },
        Movement {
            name: "移動無し".to_string(),
            writable: false,
        },
    ];
    harness.host.set_movements(movements.clone());

    for movement_entry in &movements {
        let result = harness.edit.set_object_item(&set_movement(
            &harness,
            movement(&[0.0, 50.0, 100.0], &movement_entry.name),
        ));
        if movement_entry.writable {
            result.unwrap_or_else(|error| {
                panic!("{} が拒否されました: {error}", movement_entry.name)
            });
        } else {
            let error = result.expect_err("書けない移動方法が受理されました");
            assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
            assert_eq!(
                error.details()["reason"],
                json!("track_mode_not_writable"),
                "{} の拒否の理由",
                movement_entry.name
            );
            // 名前を選び直す手は通らない。一覧に無い名前とは別の失敗である。
            assert_ne!(error.details()["reason"], json!("track_mode_unknown"));
        }
    }

    // 拒否は書き込みを発行する手前で起きる。記録が空でなければ、検証を通り
    // 抜けた入力がホストへ届いている。
    assert_eq!(
        harness.host.fatal_movement_writes(),
        Vec::<String>::new(),
        "書けない移動方法がホストへ届きました"
    );
}

#[test]
fn a_movement_that_reaches_the_host_with_an_unknown_mode_is_recorded() {
    // **記録に入る経路があることを確かめる。** 空であることしか見ない検査は、
    // 記録そのものが壊れていても緑のまま通り、検証を外した変更を捕まえられない。
    // ホストが本当に知っている名前と、検証へ渡す一覧は別の出所を持つ。食い違わ
    // せれば、検証を通り抜けた書き込みがホストへ届く。
    let harness = harness_with_track_effect();
    let unknown = "存在しない移動";
    assert!(
        !TRACK_MODES.contains(&unknown),
        "ホストが知っている名前を未知の名前として使っています"
    );
    harness.host.set_movements(vec![Movement {
        name: unknown.to_string(),
        writable: true,
    }]);

    let error = with_silent_panic_hook(|| {
        harness
            .edit
            .set_object_item(&set_movement(
                &harness,
                movement(&[0.0, 50.0, 100.0], unknown),
            ))
            .expect_err("実機ならプロセスが落ちる書き込みが成功として返りました")
    });

    // 実機は落ちる。フェイクの panic は編集の入口が捕捉するため、応答からは
    // 内部の失敗としか見えない。
    assert_eq!(error.error_code(), ErrorCode::InternalError);
    assert_eq!(
        harness.host.fatal_movement_writes(),
        vec![unknown.to_string()],
        "落ちる移動方法がホストへ届いたのに記録されていません"
    );
}

#[test]
fn a_movement_whose_value_count_does_not_match_the_sections_is_refused() {
    // ホストは個数の不一致を拒否せず、余った値を評価せずに保存する。要求した
    // 区間の値が入らないことに気付く手段が要求元に無い。
    // 標本の対象は中間点を 1 つ持つ。区間 2 個に対して値は 3 個である。
    let harness = harness_with_track_effect();
    harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            movement(&[0.0, 50.0, 100.0], "直線移動"),
        ))
        .expect("区間の数と合う値が拒否されました");

    let error = harness
        .edit
        .set_object_item(&set_movement(&harness, movement(&[0.0, 100.0], "直線移動")))
        .expect_err("区間の数と合わない値が受理されました");

    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    assert_eq!(error.details()["reason"], json!("track_value_count"));
    assert!(harness.host.fatal_movement_writes().is_empty());
}

#[test]
fn no_movement_can_be_written_when_the_list_is_unavailable() {
    // 一覧を引けない環境では移動を 1 つも書けない。検証できないまま通すと、
    // その場でホストのプロセスが落ちる。
    let harness = harness_with_track_effect();
    harness.host.set_movements(Vec::new());
    let error = harness
        .edit
        .set_object_item(&set_movement(
            &harness,
            movement(&[0.0, 50.0, 100.0], "直線移動"),
        ))
        .expect_err("一覧が空でも移動が受理されました");

    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    assert_eq!(error.details()["reason"], json!("track_mode_unknown"));
    assert!(harness.host.fatal_movement_writes().is_empty());

    // 一覧を要さない書き込みは影響を受けない。
    harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 30 },
        })
        .expect("移動を含まない書き込みまで拒否されました");
}

#[test]
fn a_choice_value_the_host_ignores_is_reported_as_a_failure() {
    // SDK は選択肢を列挙する手段を持たず、選択肢に無い値を渡しても失敗を返さず
    // に無視する。読み直して照合しなければ、当て推量が外れたことを成功として
    // 報告してしまう。
    let rejected = "存在しない形";
    assert!(
        !CHOICE_VALUES.contains(&rejected),
        "ホストが受け付ける値を無効な値として使っています"
    );

    for item in CHOICE_ITEMS {
        let harness = harness_with_choice_effect();
        let error = harness
            .edit
            .set_object_item(&set_choice_item(&harness, item, rejected))
            .expect_err("選択肢に無い値が成功として返りました");

        assert_eq!(
            error.error_code(),
            ErrorCode::UnsupportedOperation,
            "{item}"
        );
        assert_eq!(
            error.details()["reason"],
            json!("item_value_not_applied"),
            "{item}"
        );
        // 書き込んだ直後に読み直した値が載る。この階級では変更前の値そのもの
        // であり、ホストは何も倒していない。
        assert_eq!(
            error.details()["observed_value"],
            json!(CHOICE_VALUES[0]),
            "{item}"
        );
        assert!(!error.retryable(), "{item} は読み直しても有効になりません");
    }
}

/// 実機でホストが値を書き換えた 3 件と、桁の丸め。
///
/// 要求する値・読み直される実値の組で持つ。いずれも「書けたのに要求した値が
/// 入っていない」状態であり、種別が違っても同じ失敗として返る。
fn rewritten_item_cases() -> Vec<(&'static str, ItemValue, &'static str)> {
    vec![
        // 書式の合わない色は、変更前の値ではなく白へ落ちる。
        (
            "色",
            ItemValue::Color {
                value: "#ff0000".to_string(),
            },
            DEFAULT_COLOR,
        ),
        // 未登録のフォント名は黙殺され、変更前の値が残る。
        (
            "フォント",
            ItemValue::Font {
                name: "NoSuchFont12345".to_string(),
            },
            DEFAULT_FONT,
        ),
        // 値域を外れた数値は切り詰められる。
        (
            "サイズ",
            ItemValue::Number {
                value: FiniteF64::try_new((MAX_ITEM_VALUE + 400) as f64).expect("有限値"),
            },
            "100.00",
        ),
        // 桁の多い小数は項目の桁へ丸められる。切り詰めと区別する材料がこちら側に
        // 無く、どちらも要求した値を得ていない。
        (
            "サイズ",
            ItemValue::Number {
                value: FiniteF64::try_new(1.2345).expect("有限値"),
            },
            "1.23",
        ),
    ]
}

#[test]
fn a_value_the_host_rewrites_is_reported_as_a_failure() {
    // ホストは書き込みの成否を返さない。読み直して照合しなければ、値が書き
    // 換えられたことを利用者もクライアントも知る手段が無い。
    for (item, requested, current) in rewritten_item_cases() {
        let harness = harness_with_choice_effect();
        let error = harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector: harness.effect_selector(1, 300, SHAPE, 0),
                item: item.to_string(),
                value: requested,
            })
            .expect_err("ホストが書き換えた値が成功として返りました");

        assert_eq!(
            error.error_code(),
            ErrorCode::UnsupportedOperation,
            "{item}"
        );
        assert_eq!(
            error.details()["reason"],
            json!("item_value_not_applied"),
            "{item}"
        );
        assert_eq!(error.details()["observed_value"], json!(current), "{item}");
    }
}

#[test]
fn the_failure_carries_the_host_value_and_not_the_requested_one() {
    // 応答へ反響させてよいのはホストの現在の状態だけである。要求された値は
    // 要求元の内容であり、載せない。
    let harness = harness_with_choice_effect();
    let requested = "NoSuchFont12345";
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 300, SHAPE, 0),
            item: "フォント".to_string(),
            value: ItemValue::Font {
                name: requested.to_string(),
            },
        })
        .expect_err("未登録のフォント名が成功として返りました");

    let details = error.details().to_string();
    assert!(
        details.contains(DEFAULT_FONT),
        "読み直した実値が載っていません: {details}"
    );
    assert!(
        !details.contains(requested),
        "要求された値が反響しています: {details}"
    );
}

/// 対象がいま持っている設定値を読み取り経路から得る。
///
/// 応答が返す値ではなく、プロジェクトの状態そのものを見る。応答だけを見ると、
/// 巻き戻したと名乗るだけで戻していない実装を区別できない。
fn stored_shape_item(harness: &Harness, item: &str) -> ItemValue {
    let selector = harness.selector(1, 300);
    harness
        .read
        .get_object(&selector)
        .expect("対象の詳細")
        .effects
        .into_iter()
        .find(|effect| effect.name == SHAPE)
        .expect("effect がありません")
        .items
        .into_iter()
        .find(|entry| entry.name == item)
        .unwrap_or_else(|| panic!("設定項目 {item} がありません"))
        .value
}

/// 書き込み検証が落ちる要求と、対象を戻す書き込みが要るか。
///
/// **2 つの階級を並べる。** 拒否ではホストが値を動かさず、戻すものが無い。
/// 倒しでは値が動くため書き戻す。要求元へ返す失敗はどちらも同じであり、違うのは
/// 我々が発行する書き込みの数だけである。
fn failed_verification_cases() -> Vec<(&'static str, ItemValue, bool)> {
    vec![
        // 拒否。選択肢に無い値は黙殺され、値も fingerprint も動かない。
        (
            "図形の種類",
            ItemValue::Choice {
                value: "存在しない形".to_string(),
            },
            false,
        ),
        // 拒否。書式の合わない色は既定値の白へ落ちるが、変更前の値が既に白で
        // ある。**ホストが書き込んだかどうかではなく、値が動いたかで分ける。**
        (
            "色",
            ItemValue::Color {
                value: "#ff0000".to_string(),
            },
            false,
        ),
        // 拒否。未登録のフォント名は黙殺される。
        (
            "フォント",
            ItemValue::Font {
                name: "NoSuchFont12345".to_string(),
            },
            false,
        ),
        // 倒し。値域を外れた数値は境界へ切り詰められ、変更前の値が失われる。
        (
            "サイズ",
            ItemValue::Number {
                value: FiniteF64::try_new((MAX_ITEM_VALUE + 400) as f64).expect("有限値"),
            },
            true,
        ),
        // 倒し。桁の多い小数は項目の桁へ丸められる。
        (
            "サイズ",
            ItemValue::Number {
                value: FiniteF64::try_new(1.2345).expect("有限値"),
            },
            true,
        ),
    ]
}

#[test]
fn the_failed_verification_cases_cover_both_classes() {
    // 片方の階級しか無い一覧では、「常に戻す」実装も「一度も戻さない」実装も
    // 検査を通り抜ける。**費用の検査は 2 つの階級の差でしか成立しない。**
    let classes: Vec<bool> = failed_verification_cases()
        .into_iter()
        .map(|(_, _, restoring)| restoring)
        .collect();
    assert!(classes.contains(&true), "倒しの階級の標本がありません");
    assert!(classes.contains(&false), "拒否の階級の標本がありません");

    // ホストが値を書き換える標本は、階級を割り当てないまま取り残さない。
    for (item, requested, _) in rewritten_item_cases() {
        assert!(
            failed_verification_cases()
                .iter()
                .any(|(name, value, _)| *name == item && *value == requested),
            "{item} へ {} を書く標本が階級を持ちません",
            requested.kind()
        );
    }
}

#[test]
fn a_failed_verification_leaves_the_item_at_its_value_before_the_write() {
    // **失敗が状態を残さない。** ホストが値を倒した場合も、要求元から見た対象は
    // 書き込みの前と同じ値を持つ。
    for (item, requested, _) in failed_verification_cases() {
        let harness = harness_with_choice_effect();
        let before = stored_shape_item(&harness, item);
        let error = harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector: harness.effect_selector(1, 300, SHAPE, 0),
                item: item.to_string(),
                value: requested,
            })
            .err()
            .unwrap_or_else(|| panic!("{item} の書き込み検証が落ちませんでした"));

        assert_eq!(
            error.details()["reason"],
            json!("item_value_not_applied"),
            "{item}"
        );
        assert_eq!(
            stored_shape_item(&harness, item),
            before,
            "{item} が書き込み前の値へ戻っていません"
        );
    }
}

#[test]
fn a_value_the_host_refuses_costs_no_restoring_write() {
    // **費用の検査である。** 値が正しいことだけを見ると、戻すものが無い階級で
    // 無駄な書き込みを発行しても通る。発行の回数そのものを数える。
    for (item, requested, restoring) in failed_verification_cases() {
        let harness = harness_with_choice_effect();
        let selector = harness.effect_selector(1, 300, SHAPE, 0);
        let origin = raw_item_value(&stored_shape_item(&harness, item));
        harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector,
                item: item.to_string(),
                value: requested,
            })
            .err()
            .unwrap_or_else(|| panic!("{item} の書き込み検証が落ちませんでした"));

        let writes = harness.host.item_value_arguments();
        assert_eq!(
            writes.len(),
            1 + usize::from(restoring),
            "{item} へ発行した書き込みの回数が想定と違います: {writes:?}"
        );
        if restoring {
            // 書き戻すのはホストが直前に返した生文字列そのものである。読み取り
            // 経路が解釈した値を組み立て直していれば、ここで食い違う。
            assert_eq!(writes[1], origin, "{item} の巻き戻しが別の値を書きました");
        }
    }
}

#[test]
fn a_selector_survives_a_failed_verification() {
    // **A2 の本体である。** 戻せば fingerprint も戻る——内容ハッシュであり、
    // 同じ内容へ戻せば同じ値が返る。要求元は失敗の後も同じ selector で続けられ、
    // 復旧に get_object を要さない。
    //
    // 倒しの階級で見る。拒否の階級は巻き戻しが無くても selector が生き残るため、
    // 復元したことを確かめられない。
    let harness = harness_with_choice_effect();
    let selector = harness.effect_selector(1, 300, SHAPE, 0);
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: selector.clone(),
            item: "サイズ".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new((MAX_ITEM_VALUE + 400) as f64).expect("有限値"),
            },
        })
        .expect_err("切り詰められた値が成功として返りました");
    assert_eq!(error.details()["reason"], json!("item_value_not_applied"));

    // 失敗の前に得たオブジェクトの selector で読み直せる。
    harness
        .read
        .get_object(&selector.object)
        .expect("失敗の後にオブジェクトの selector が死にました");
    // 同じ effect の selector で次の書き込みも通る。effect の fingerprint も
    // 戻っている。
    harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector,
            item: "サイズ".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new(2.0).expect("有限値"),
            },
        })
        .expect("失敗の後の書き込みが古い selector で拒否されました");
}

#[test]
fn a_restore_that_does_not_take_effect_names_the_state_as_unknown() {
    // 巻き戻しの書き込みも失敗し得る。**「書き込み API が真を返した」を成功と
    // 読まない**——読み直して元の文字列と一致することだけが根拠である。
    let harness = harness_with_choice_effect();
    let selector = harness.effect_selector(1, 300, SHAPE, 0);
    let before = stored_shape_item(&harness, "サイズ");
    harness
        .host
        .arm(|knobs| knobs.fault = Some(Fault::IgnoreItemRestore));

    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector,
            item: "サイズ".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new((MAX_ITEM_VALUE + 400) as f64).expect("有限値"),
            },
        })
        .expect_err("切り詰められた値が成功として返りました");

    assert_eq!(error.details()["reason"], json!("item_value_not_applied"));
    assert_eq!(error.details()["consistency_unknown"], json!(true));
    // 巻き戻しは発行したが効かなかった。効かなかったことは対象の値に現れる。
    assert_eq!(harness.host.item_value_arguments().len(), 2);
    assert_ne!(
        stored_shape_item(&harness, "サイズ"),
        before,
        "戻せていないのに戻ったと名乗っています"
    );
}

#[test]
fn a_read_back_that_fails_restores_and_names_the_state_as_unknown() {
    // 書き込んだ後の読み直しそのものが落ちると、適用されたかを確かめられない。
    // **材料は手元にあるため戻しに行く。** 戻せたことも確かめられないため、
    // 「戻せた」とは名乗らない。**確かめずに戻せたと名乗る形が Phase 4.5 の
    // 出発点である。**
    let harness = harness_with_choice_effect();
    let selector = harness.effect_selector(1, 300, SHAPE, 0);
    harness
        .host
        .arm(|knobs| knobs.fault = Some(Fault::ItemValueUnreadableAfterMutation));

    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector,
            item: "サイズ".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new(2.0).expect("有限値"),
            },
        })
        .expect_err("読み直せないまま成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    let details = error.details();
    assert_eq!(details["sdk_operation"], json!("get_effect_item_value"));
    assert_eq!(details["mutation_issued"], json!(true));
    // 巻き戻しを試みている。書き込みは前向きと戻しの 2 回発行された。
    assert_eq!(
        harness.host.item_value_arguments().len(),
        2,
        "巻き戻しを試みていません"
    );
    assert_eq!(details["restored"], json!(false));
    assert_eq!(details["consistency_unknown"], json!(true));
}

#[test]
fn the_observed_value_and_the_current_value_are_not_interchanged() {
    // **2 つのキーは別の時点を指す。** `observed_value` は書き込んだ直後に
    // 読み直した値であり、応答が返る時点の現在値ではない——巻き戻しが済んで
    // いる。`current_value` は書き込みを発行する前に落ちた失敗が運ぶ値であり、
    // 文字どおり現在値である。取り違えれば、要求元は戻したはずの状態を自分で
    // 再現する要求を組み立てる。
    let harness = harness_with_choice_effect();
    let not_applied = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 300, SHAPE, 0),
            item: "サイズ".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new((MAX_ITEM_VALUE + 400) as f64).expect("有限値"),
            },
        })
        .expect_err("切り詰められた値が成功として返りました");
    let details = not_applied.details();
    assert_eq!(details["reason"], json!("item_value_not_applied"));
    assert_eq!(
        details["observed_value"],
        json!(format!("{MAX_ITEM_VALUE}.00"))
    );
    assert!(
        details.get("current_value").is_none(),
        "巻き戻した後の値を現在値として名乗っています: {details}"
    );

    let track = harness_with_track_effect();
    let would_be_lost = track
        .edit
        .set_object_item(&set_movement(
            &track,
            ItemValue::Number {
                value: FiniteF64::try_new(0.0).expect("有限値"),
            },
        ))
        .expect_err("移動を消す書き込みが成功として返りました");
    let details = would_be_lost.details();
    assert_eq!(details["reason"], json!("track_movement_present"));
    assert!(details["current_value"].is_string());
    assert!(
        details.get("observed_value").is_none(),
        "書き込みを発行していない失敗が読み直した値を名乗っています: {details}"
    );
}

#[test]
fn the_restore_outcome_is_named_for_both_classes() {
    // **拒否の階級でも `restored` は真である。** 戻す書き込みが要らなかった
    // だけで、対象は書き込み前の値を持つ。要求元が取る行動は倒しの階級と
    // 変わらない。
    for (item, requested, _) in failed_verification_cases() {
        let harness = harness_with_choice_effect();
        let error = harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector: harness.effect_selector(1, 300, SHAPE, 0),
                item: item.to_string(),
                value: requested,
            })
            .err()
            .unwrap_or_else(|| panic!("{item} の書き込み検証が落ちませんでした"));

        let details = error.details();
        assert_eq!(details["restored"], json!(true), "{item}");
        assert!(
            details.get("consistency_unknown").is_none(),
            "{item} は戻せているのに中途半端な状態を名乗りました: {details}"
        );
    }

    // 戻せなかったときだけ偽になり、`consistency_unknown` が対で立つ。
    let harness = harness_with_choice_effect();
    let selector = harness.effect_selector(1, 300, SHAPE, 0);
    harness
        .host
        .arm(|knobs| knobs.fault = Some(Fault::IgnoreItemRestore));
    let details = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector,
            item: "サイズ".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new((MAX_ITEM_VALUE + 400) as f64).expect("有限値"),
            },
        })
        .expect_err("切り詰められた値が成功として返りました")
        .details();
    assert_eq!(details["restored"], json!(false));
    assert_eq!(details["consistency_unknown"], json!(true));
}

#[test]
fn a_restored_write_advances_the_revision_at_most_once() {
    // 巻き戻しは同じ許可で発行する。許可は最初の発行で確定した revision を
    // 保つため、書き込みが 2 回でも revision は 1 つしか進まない。
    let harness = harness_with_choice_effect();
    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 300, SHAPE, 0),
            item: "サイズ".to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new((MAX_ITEM_VALUE + 400) as f64).expect("有限値"),
            },
        })
        .expect_err("切り詰められた値が成功として返りました");

    // 巻き戻しが実際に発行されていなければ、revision が 1 に留まることは何も
    // 言っていない。
    assert_eq!(
        harness.host.item_value_arguments().len(),
        2,
        "巻き戻しの書き込みが発行されていません"
    );
    assert_eq!(harness.project.revision(), 1, "revision が 2 つ進みました");
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(error.details()["current_project_revision"], json!(1));
}

/// 候補の表だけが主張する値。**ホストが受け付ける値とは重ならない。**
const HINTED_VALUES: [&str; 2] = ["表だけにある形", "表だけにあるもう 1 つの形"];

/// 候補の表を持たせた [`shape_catalog_entry`]。
///
/// 表はカタログの側に持つ。読み取り経路が候補を引く先と同じ場所であり、
/// 書き込みの経路がそこを見るようになれば、この表を変えた結果が成否に現れる。
fn shape_catalog_entry_with_choices(values: &[&str]) -> FakeCatalogEntry {
    shape_catalog_entry_with_facets(ItemFacets {
        choices: Some(ItemChoices {
            values: values.iter().map(|value| (*value).to_string()).collect(),
            source: TableSource::Sidecar,
        }),
        range: None,
    })
}

/// 面の組を全項目へ持たせた [`shape_catalog_entry`]。
fn shape_catalog_entry_with_facets(facets: ItemFacets) -> FakeCatalogEntry {
    let facets = shape_catalog_entry()
        .items
        .into_iter()
        .map(|item| (item.name, facets.clone()))
        .collect();
    FakeCatalogEntry {
        facets,
        ..shape_catalog_entry()
    }
}

/// 候補の表を差し替えたうえで、選択肢の項目へ 1 件書き込む。
///
/// 表の中身を変えても結果が変わらないことを比べられるよう、成否と失敗の種別を
/// 1 つの値へ畳んで返す。
fn write_choice_with_table(
    table: Option<&[&str]>,
    item: &str,
    value: &str,
) -> Result<String, String> {
    let harness = Harness::with(|host| {
        host.catalog.push(match table {
            Some(values) => shape_catalog_entry_with_choices(values),
            None => shape_catalog_entry(),
        });
        host.scene.get_mut().unwrap().layers[1].objects[1]
            .effects
            .push(shape(0));
    });
    harness
        .edit
        .set_object_item(&set_choice_item(&harness, item, value))
        .map(|outcome| raw_item_value(&changed_item(&outcome, item)))
        .map_err(|error| format!("{:?} {}", error.error_code(), error.details()["reason"]))
}

#[test]
fn the_choices_table_never_decides_whether_a_write_goes_through() {
    // **候補はヒントであってゲートではない。** 表に無い値でも書き込みは通し、
    // 表に在る値が必ず通るとも約束しない。可否を決めるのはホストであり、表が
    // 実態から外れたときに事前検証を掛けていれば、正しい値が通らなくなる。
    //
    // 移動方法の一覧とは性質が違う。あちらは一覧に無い名前を書くとホストの
    // プロセスが落ちるため通す選択肢が無いが、候補を外した書き込みは最悪でも
    // ホストが値を無視するだけである。
    //
    // **覆う範囲は [`the_range_table_never_decides_whether_a_write_goes_through`]
    // と同じである。** 表はフェイクのカタログ側にあり、捕まえられるのは
    // [`crate::read::host::ReadHost::effect_facets`] を経由するゲートだけである。
    for value in HINTED_VALUES {
        assert!(
            !CHOICE_VALUES.contains(&value),
            "表だけの値としてホストが受け付ける値を使っています"
        );
    }

    for item in CHOICE_ITEMS {
        for value in [CHOICE_VALUES[1], HINTED_VALUES[0]] {
            // 表を持たない環境での結果を基準に取る。
            let baseline = write_choice_with_table(None, item, value);
            for table in [
                // 表が別の値だけを主張する。
                &HINTED_VALUES[..],
                // 表が 1 件も候補を持たない。
                &[][..],
                // 表が書こうとしている値を含む。
                &[value][..],
            ] {
                assert_eq!(
                    write_choice_with_table(Some(table), item, value),
                    baseline,
                    "{item} へ {value} を書く成否が表の中身で変わりました"
                );
            }
        }
    }

    // 基準そのものはホストの受け付ける値で決まっている。両方が同じ結果になる
    // 表では、表が効いていないことを確かめられない。
    assert!(write_choice_with_table(None, CHOICE_ITEMS[0], CHOICE_VALUES[1]).is_ok());
    assert!(write_choice_with_table(None, CHOICE_ITEMS[0], HINTED_VALUES[0]).is_err());
}

/// 数値の項目。ホストは [`MIN_ITEM_VALUE`]〜[`MAX_ITEM_VALUE`] へ倒す。
const NUMBER_ITEM: &str = "サイズ";

/// 値域の表を差し替えたうえで、数値の項目へ 1 件書き込む。
///
/// 表の中身を変えても結果が変わらないことを比べられるよう、成否と失敗の種別を
/// 1 つの値へ畳んで返す。
fn write_number_with_range(range: Option<(f64, f64)>, value: f64) -> Result<String, String> {
    let harness = Harness::with(|host| {
        host.catalog.push(match range {
            Some((min, max)) => shape_catalog_entry_with_facets(ItemFacets {
                choices: None,
                range: Some(ItemRange {
                    min: FiniteF64::try_new(min),
                    max: FiniteF64::try_new(max),
                    decimals: Some(0),
                    source: TableSource::Sidecar,
                }),
            }),
            None => shape_catalog_entry(),
        });
        host.scene.get_mut().unwrap().layers[1].objects[1]
            .effects
            .push(shape(0));
    });
    harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 300, SHAPE, 0),
            item: NUMBER_ITEM.to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new(value).expect("有限値"),
            },
        })
        .map(|outcome| raw_item_value(&changed_item(&outcome, NUMBER_ITEM)))
        .map_err(|error| format!("{:?} {}", error.error_code(), error.details()["reason"]))
}

#[test]
fn the_range_table_never_decides_whether_a_write_goes_through() {
    // **値域もヒントであってゲートではない。** 表の値域を外れる値でも書き込みは
    // 通し、表の値域に収まる値が必ず通るとも約束しない。可否を決めるのはホスト
    // であり、書き込みの経路は書いた値を読み直して照合する。
    //
    // **値域は候補より外れやすい。** 候補の陳腐化は足りなくなるだけだが、値域の
    // 陳腐化は狭くなる——版が上がって上限が広がったとき、事前検証を掛けて
    // いれば通るはずの値をこちら側が拒む。
    //
    // # この検査が覆う範囲
    //
    // 表はフェイクのカタログ側にあり、読み取り経路が面を引く先と同じ場所で
    // ある。**捕まえられるのは
    // [`crate::read::host::ReadHost::effect_facets`] を経由して面を読むゲート
    // だけである。** [`crate::item_facets::table`] を直に読むゲートはここを
    // 素通りする——あちらは実行ファイルへ埋め込んだ基底とデータディレクトリの
    // サイドカーだけを見ており、フェイクのカタログを見ないためである。
    //
    // **その隙間を塞ぐ手が現状は無い。** 表は要求ごとに解決するものではなく
    // 起動から 1 度きりであり、差し替える口が製品側に無い。検査のために口を
    // 開ければ、塞ごうとしている性質そのものを検査のために曲げることになる。
    let inside = (MAX_ITEM_VALUE / 2) as f64;
    let outside = (MAX_ITEM_VALUE + 400) as f64;

    for value in [inside, outside] {
        // 表を持たない環境での結果を基準に取る。
        let baseline = write_number_with_range(None, value);
        for range in [
            // 表が書こうとしている値より狭い範囲を主張する。
            (0.0, 1.0),
            // 表がホストより広い範囲を主張する。**版が上がった後の状態である。**
            (0.0, f64::from(u16::MAX)),
            // 表が書こうとしている値を含む。
            (value - 1.0, value + 1.0),
        ] {
            assert_eq!(
                write_number_with_range(Some(range), value),
                baseline,
                "{value} を書く成否が表の値域で変わりました"
            );
        }
    }

    // 基準そのものはホストの倒しで決まっている。両方が同じ結果になる値では、
    // 表が効いていないことを確かめられない。
    assert!(write_number_with_range(None, inside).is_ok());
    assert!(write_number_with_range(None, outside).is_err());
}

#[test]
fn a_choice_value_the_host_accepts_succeeds() {
    for item in CHOICE_ITEMS {
        let harness = harness_with_choice_effect();
        harness.host.clear_calls();

        let outcome = harness
            .edit
            .set_object_item(&set_choice_item(&harness, item, CHOICE_VALUES[1]))
            .unwrap_or_else(|error| panic!("{item} で選択肢に在る値が拒否されました: {error}"));

        assert_eq!(
            changed_item(&outcome, item),
            ItemValue::Choice {
                value: CHOICE_VALUES[1].to_string(),
            },
            "{item}"
        );
        let calls = harness.host.calls();
        let first = first_mutation(&calls).expect("変更 API が呼ばれていません");
        assert_eq!(
            count(&calls[first..], ITEM_VALUE),
            1,
            "{item} の照合の読み直しは 1 回だけです: {calls:?}"
        );
    }
}

#[test]
fn a_choice_value_read_from_the_object_can_be_written_straight_back() {
    // 読み取り経路が返した値を組み替えずに書き戻せることを、往復の形で固定
    // する。読み取り口はフェイクが保持する値をそのまま返すため、種別から値へ
    // の写像そのものはここを通らない。写像との突き合わせは写像を直接呼ぶ側が
    // 持ち、ここが確かめるのは書き込み側が同じ値を受理することである。
    for item in CHOICE_ITEMS {
        let harness = harness_with_choice_effect();
        let selector = harness.selector(1, 300);
        let detail = harness
            .read
            .get_object(&selector)
            .expect("対象の詳細を取得できませんでした");
        let value = detail
            .effects
            .iter()
            .find(|effect| effect.name == SHAPE)
            .expect("effect がありません")
            .items
            .iter()
            .find(|entry| entry.name == item)
            .unwrap_or_else(|| panic!("設定項目 {item} がありません"))
            .value
            .clone();
        assert!(
            matches!(value, ItemValue::Choice { .. }),
            "{item} が選択肢として読めません: {value:?}"
        );

        let outcome = harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector: harness.effect_selector(1, 300, SHAPE, 0),
                item: item.to_string(),
                value: value.clone(),
            })
            .unwrap_or_else(|error| panic!("{item} の書き戻しが失敗しました: {error}"));

        assert_eq!(changed_item(&outcome, item), value, "{item}");
    }
}

#[test]
fn a_value_the_host_writes_back_in_another_notation_is_not_a_mismatch() {
    // ホストは受理した値の表記も整える——色は小文字へ、実数は項目の桁へ揃える。
    // テキストは書き込み経路が符号化した表記のまま返る。**比較を種別ごとに
    // 定めたのは、これらを失敗と誤診断しないためである。** 一致の判定をバイト
    // 比較へ倒すと、ここが偽陽性の一覧になる。
    let cases = [
        (
            "色",
            ItemValue::Color {
                value: "FFAA00".to_string(),
            },
            ItemValue::Color {
                value: "ffaa00".to_string(),
            },
        ),
        (
            "サイズ",
            ItemValue::Number {
                value: FiniteF64::try_new(MAX_ITEM_VALUE as f64).expect("有限値"),
            },
            ItemValue::Number {
                value: FiniteF64::try_new(MAX_ITEM_VALUE as f64).expect("有限値"),
            },
        ),
        (
            "メモ",
            ItemValue::Text {
                value: "上\r\n下".to_string(),
            },
            ItemValue::Text {
                value: "上\n下".to_string(),
            },
        ),
    ];
    for (item, requested, stored) in cases {
        let harness = harness_with_choice_effect();
        let outcome = harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector: harness.effect_selector(1, 300, SHAPE, 0),
                item: item.to_string(),
                value: requested.clone(),
            })
            .unwrap_or_else(|error| panic!("{item} の書き込みが失敗として扱われました: {error}"));

        assert_eq!(changed_item(&outcome, item), stored, "{item}");

        // **標本が食い違いを含むことを確かめる。** 要求とホストの間に何の違いも
        // 無い標本は、バイト比較のままの実装でも通ってしまい、種別ごとの比較を
        // 定めた意味を試せていない。違いの現れ方は種別で分かれる——色とテキスト
        // は値の表現が、実数は表記だけが違う。
        let written = harness.host.item_value_arguments();
        assert_eq!(written.len(), 1, "{item} の書き込みが 1 回ではありません");
        assert!(
            requested != stored || raw_item_value(&stored) != written[0],
            "{item} の標本は要求とホストの間に違いが無く、比較の違いを試せていません"
        );
    }
}

#[test]
fn a_value_the_host_rewrote_is_told_apart_from_a_change_that_did_not_apply() {
    // 値域も選択肢も列挙できない以上、当て推量が外れることは常態である。
    // ヘッダーが変更を拒む旨を記していない setter の不一致と畳むと、要求元は
    // 「異常」と「よくある入力誤り」を区別できない。前者は報告する対象であり、
    // 後者は読み直した実値を見て送り直す対象である。
    let choice = harness_with_choice_effect();
    let rejected = choice
        .edit
        .set_object_item(&set_choice(&choice, "存在しない形"))
        .expect_err("選択肢に無い値が成功として返りました");

    let ignored =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreObjectName)));
    let not_applied = ignored
        .edit
        .set_object_name(&SetObjectNameParams {
            selector: ignored.selector(1, 100),
            name: Some("新しい名前".to_string()),
        })
        .expect_err("無視された改名が成功として返りました");

    assert_eq!(rejected.error_code(), not_applied.error_code());
    assert_eq!(
        rejected.details()["reason"],
        json!("item_value_not_applied")
    );
    assert_eq!(not_applied.details()["reason"], json!("change_not_applied"));
    assert_ne!(
        rejected.details()["reason"],
        not_applied.details()["reason"],
        "2 つの失敗が同じ名前を名乗っています"
    );
}

#[test]
fn every_kind_of_rewrite_shares_one_reason() {
    // 「ホストが拒んだ」と「ホストが別の値へ倒した」を区別する材料がこちら側に
    // 無い。どちらも読み直しが要求と違うとしか観測できないため、種別ごとに名前を
    // 割らない。
    let mut reasons: Vec<String> = Vec::new();
    for (item, requested, _) in rewritten_item_cases() {
        let harness = harness_with_choice_effect();
        let error = harness
            .edit
            .set_object_item(&SetObjectItemParams {
                selector: harness.effect_selector(1, 300, SHAPE, 0),
                item: item.to_string(),
                value: requested,
            })
            .expect_err("ホストが書き換えた値が成功として返りました");
        reasons.push(error.details()["reason"].to_string());
    }
    let choice = harness_with_choice_effect();
    reasons.push(
        choice
            .edit
            .set_object_item(&set_choice(&choice, "存在しない形"))
            .expect_err("選択肢に無い値が成功として返りました")
            .details()["reason"]
            .to_string(),
    );
    assert!(!reasons.is_empty());
    assert!(
        reasons.iter().all(|reason| *reason == reasons[0]),
        "書き換えの種類ごとに名前が分かれています: {reasons:?}"
    );
}

/// 編集手順が実際に返した「読み直しが要求と違う」失敗を集める。
///
/// 名前を生む経路が製品に在ることの裏付けとして用いる。一覧から値を組み立てる
/// のでは、返す呼び出しが 1 つも無くても検査が通ってしまう。
pub(crate) fn produced_item_value_mismatch_failures() -> Vec<EditError> {
    rewritten_item_cases()
        .into_iter()
        .map(|(item, requested, _)| {
            let harness = harness_with_choice_effect();
            harness
                .edit
                .set_object_item(&SetObjectItemParams {
                    selector: harness.effect_selector(1, 300, SHAPE, 0),
                    item: item.to_string(),
                    value: requested,
                })
                .expect_err("ホストが書き換えた値が成功として返りました")
        })
        .collect()
}

#[test]
fn the_item_value_mismatch_has_a_request_that_produces_it() {
    for failure in produced_item_value_mismatch_failures() {
        assert_eq!(
            failure.details()["reason"],
            json!("item_value_not_applied"),
            "別の失敗が返りました"
        );
    }
}

#[test]
fn an_added_effect_is_located_by_the_difference_in_the_name_list() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .add_effect(&AddEffectParams {
            object: harness.selector(1, 100),
            effect_name: "ぼかし".to_string(),
        })
        .expect("effect の付与に失敗しました");

    let effect = outcome.effect.expect("付与された effect");
    assert_eq!(effect.name, "ぼかし");
    // 既に同名が 1 つあるため、同名内の順序は 1 になる。
    assert_eq!(effect.index, 1);
    assert_eq!(effect.selector.effect_index, 1);
}

#[test]
fn an_added_effect_reports_where_it_landed_in_the_column() {
    // 既定の対象は `動画ファイル` と `ぼかし` を持つ。末尾へ `ぼかし` が入ると
    // 同名内の順序は 1、列の位置は 2 になり、2 つの数が食い違う。
    let harness = Harness::new();
    let outcome = harness
        .edit
        .add_effect(&AddEffectParams {
            object: harness.selector(1, 100),
            effect_name: "ぼかし".to_string(),
        })
        .expect("effect の付与に失敗しました");

    let effect = outcome.effect.expect("付与された effect");
    let scene = harness.host.scene();
    let effects = &scene.layers[1].objects[0].effects;
    assert_eq!(effect.position, effects.len() - 1);
    assert_eq!(effects[effect.position].name, effect.name);
    assert_eq!(effects[effect.position].index, effect.index);
    assert_ne!(effect.position, effect.index);
}

#[test]
fn every_effect_changing_response_reports_the_column_position() {
    // 既定の対象は `動画ファイル` と `ぼかし` を持つ。先頭の `ぼかし` は同名内で
    // 0 番目・列では 1 番目であり、2 つの数が食い違う。位置は対象を解決した時点の
    // 列から求め、変更後に読み直した列へ当てる。どちらの operation も列の構成を
    // 変えないため、2 つの列で同じ effect を指す。
    let harness = Harness::new();
    let outcomes = [
        (
            "set_object_item",
            harness
                .edit
                .set_object_item(&SetObjectItemParams {
                    selector: harness.effect_selector(1, 100, "ぼかし", 0),
                    item: "範囲".to_string(),
                    value: ItemValue::Integer { value: 30 },
                })
                .expect("set_object_item"),
        ),
        (
            "set_effect_enabled",
            harness
                .edit
                .set_effect_enabled(&SetEffectEnabledParams {
                    selector: harness.effect_selector(1, 100, "ぼかし", 0),
                    enabled: false,
                })
                .expect("set_effect_enabled"),
        ),
    ];
    for (tool, outcome) in outcomes {
        let effect = outcome.effect.expect("変更後の effect");
        assert_eq!(effect.index, 0, "{tool}");
        assert_eq!(effect.position, 1, "{tool}");
        let scene = harness.host.scene();
        let effects = &scene.layers[1].objects[0].effects;
        assert_eq!(effects[effect.position].name, effect.name, "{tool}");
        assert_eq!(effects[effect.position].index, effect.index, "{tool}");
    }
}

#[test]
fn moving_reports_the_placement_the_host_chose() {
    // ホストが宛先を調整しても移動そのものは成功している。要求値との一致を
    // 求めると、成功した移動が対象の不在として返る。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::AdjustMoveDestination)));
    let params = move_params(&harness);
    let outcome = harness
        .edit
        .move_object(&params)
        .expect("宛先を調整されただけで移動が失敗しました");

    let moved = outcome.object.expect("移動後の対象");
    assert_eq!(
        moved.frame_start,
        500 + MOVE_FRAME_SHIFT,
        "要求した宛先をそのまま応答へ載せています"
    );
    // 応答が返した selector はそのまま次の要求へ渡せる。
    harness
        .read
        .get_object(&moved.selector)
        .expect("応答が返した selector で引けません");
}

#[test]
fn moving_fails_when_the_new_placement_cannot_be_read() {
    // read-back が無くなるわけではない。位置を読めなければ応答を組み立てられず、
    // 変更を発行した後の失敗として返す。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::PositionUnreadable)));
    let params = move_params(&harness);
    let error = harness
        .edit
        .move_object(&params)
        .expect_err("位置を読めないのに成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(
        error.details()["sdk_operation"],
        json!("get_object_layer_frame")
    );
    assert_eq!(error.details()["mutation_issued"], json!(true));
}

#[test]
fn a_read_back_failure_after_a_mutation_keeps_the_revision_and_reports_it() {
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::ReadBack)));
    let params = move_params(&harness);
    let error = harness
        .edit
        .move_object(&params)
        .expect_err("読み直しの失敗が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(error.details()["current_project_revision"], json!(1));
    assert_eq!(
        harness.project.revision(),
        1,
        "変更が入ったのに revision が戻されました"
    );
    assert!(
        harness.project.modified(),
        "変更が入ったのに未保存の変更なしと報告されます"
    );
}

// ------------------------------------------------------------- revision の更新

#[test]
fn issuing_a_mutation_advances_the_revision_once() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_effect_enabled(&SetEffectEnabledParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            enabled: false,
        })
        .expect("状態の変更に失敗しました");

    assert_eq!(harness.project.revision(), 1);
    assert_eq!(outcome.project_revision, 1);
}

#[test]
fn a_failure_before_any_mutation_leaves_the_revision_alone() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.fingerprint = tamper(&params.selector.fingerprint);
    let _ = harness.edit.move_object(&params);

    assert_eq!(harness.project.revision(), 0);
    assert!(!harness.project.modified());
}

#[test]
fn changing_the_selection_does_not_advance_the_revision() {
    let harness = Harness::new();
    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition { layer: 1, frame: 5 }),
            selected_range: None,
            focus: None,
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect("選択状態の変更に失敗しました");

    assert_eq!(harness.project.revision(), 0);
    assert_eq!(state.project_revision, 0);
    assert!(
        !harness.project.modified(),
        "内容を変えない操作が未保存の変更として記録されました"
    );
}

#[test]
fn changing_the_selection_ignores_an_advanced_revision_but_not_a_stale_epoch() {
    let harness = Harness::new();
    let epoch = harness.epoch();
    harness.project.on_object_updated();

    harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition { layer: 1, frame: 5 }),
            selected_range: None,
            focus: None,
            display: None,
            expected_project_epoch: epoch,
        })
        .expect("revision が進んだだけで選択状態の変更が拒否されました");

    let error = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition { layer: 1, frame: 5 }),
            selected_range: None,
            focus: None,
            display: None,
            expected_project_epoch: "別のプロジェクト".to_string(),
        })
        .expect_err("別プロジェクトの前提が受理されました");
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));
}

// ------------------------------------------------------------------ 選択状態

#[test]
fn the_selection_is_applied_in_a_fixed_order() {
    let harness = Harness::new();
    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition {
                layer: 1,
                frame: 5_000,
            }),
            selected_range: Some(RangeChange::Set { start: 10, end: 20 }),
            focus: Some(FocusChange::Set {
                object: harness.selector(1, 100),
            }),
            display: Some(DisplayStart {
                layer: 1,
                frame: 60,
            }),
            expected_project_epoch: harness.epoch(),
        })
        .expect("選択状態の変更に失敗しました");

    let calls: Vec<_> = harness
        .host
        .calls()
        .into_iter()
        .filter(|call| MUTATIONS.contains(call))
        .collect();
    assert_eq!(
        calls,
        vec![
            "set_cursor_layer_frame",
            "set_select_range",
            "set_display_layer_frame",
            "set_focus_object"
        ]
    );
    assert_eq!(
        state.applied,
        vec![
            SelectionField::Cursor,
            SelectionField::SelectedRange,
            SelectionField::Display,
            SelectionField::Focus
        ]
    );
    // ホストが範囲外の値をクランプしても失敗にしない。応答は実際の値を返す。
    assert_eq!(state.cursor.frame, MAX_FRAME);
    assert!(state.cursor.layer <= MAX_LAYER);
    assert_eq!(
        state.focus.expect("フォーカス対象").frame_start,
        100,
        "フォーカスの観測値が返っていません"
    );
}

#[test]
fn a_display_start_can_be_the_only_requested_change() {
    let harness = Harness::new();
    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: None,
            display: Some(DisplayStart {
                layer: 2,
                frame: 30,
            }),
            expected_project_epoch: harness.epoch(),
        })
        .expect("表示開始位置だけの要求が拒否されました");

    assert_eq!(state.applied, vec![SelectionField::Display]);
    assert!(state.not_applied.is_empty());
    assert_eq!(state.display.frame_start, 30);
    assert_eq!(state.display.layer_start, 2);
    assert_eq!(harness.host.scene().display.frame_start, 30);
}

#[test]
fn a_request_without_a_display_start_does_not_touch_the_display() {
    let harness = Harness::new();
    harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition { layer: 1, frame: 5 }),
            selected_range: Some(RangeChange::Clear {}),
            focus: Some(FocusChange::Clear {}),
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect("選択状態の変更に失敗しました");

    let calls = harness
        .host
        .calls()
        .into_iter()
        .filter(|call| *call == "set_display_layer_frame")
        .count();
    assert_eq!(calls, 0, "省略した軸に対して SDK が呼ばれました");
}

#[test]
fn a_clamped_display_start_is_reported_as_not_applied() {
    // ホストは設定できる範囲へ調整する。要求どおりの位置に無い以上、反映された
    // とは言えない。
    let harness = Harness::new();
    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: None,
            display: Some(DisplayStart {
                layer: 0,
                frame: 5_000,
            }),
            expected_project_epoch: harness.epoch(),
        })
        .expect("クランプが失敗として返りました");

    assert!(state.applied.is_empty());
    assert_eq!(state.not_applied, vec![SelectionField::Display]);
    assert_eq!(state.display.frame_start, MAX_FRAME);
}

#[test]
fn the_display_span_does_not_decide_whether_the_start_was_applied() {
    // 表示フレーム数・表示レイヤー数は厳密な値ではない。これらを判定に使うと、
    // 開始位置が要求どおりでも適用できなかったと報告することになる。
    let harness = Harness::new();
    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: None,
            display: Some(DisplayStart {
                layer: 1,
                frame: 60,
            }),
            expected_project_epoch: harness.epoch(),
        })
        .expect("表示開始位置の変更に失敗しました");

    assert_ne!(state.display.frame_num, state.display.frame_start);
    assert_ne!(state.display.layer_num, state.display.layer_start);
    assert_eq!(state.applied, vec![SelectionField::Display]);
    assert!(state.not_applied.is_empty());
}

/// 表示開始位置を含む要求を組み立てる。
fn set_display(
    harness: &Harness,
    layer: u32,
    frame: u32,
    focus: Option<FocusChange>,
) -> SetSelectionParams {
    SetSelectionParams {
        expected_scene_id: SCENE_ID,
        cursor: None,
        selected_range: None,
        focus,
        display: Some(DisplayStart { layer, frame }),
        expected_project_epoch: harness.epoch(),
    }
}

#[test]
fn the_display_start_decides_its_own_membership_by_the_observed_position() {
    // 表示開始位置は「呼び出しが通ったか」ではなく「要求どおりの位置に入ったか」
    // で振り分ける。3 通りを 1 つの表として並べる。
    let harness = Harness::new();
    let focused = harness.selector(1, 100);

    // 範囲を超えた要求はクランプされ、適用できなかった側へ入る。
    let clamped = harness
        .edit
        .set_selection(&set_display(&harness, 30, 3_000, None))
        .expect("クランプが失敗として返りました");
    assert!(clamped.applied.is_empty());
    assert_eq!(clamped.not_applied, vec![SelectionField::Display]);
    assert_ne!(clamped.display.frame_start, 3_000);
    assert_ne!(clamped.display.layer_start, 30);

    // 範囲内の要求はそのまま入る。
    let exact = harness
        .edit
        .set_selection(&set_display(&harness, 0, 0, None))
        .expect("範囲内の表示開始位置が拒否されました");
    assert_eq!(exact.applied, vec![SelectionField::Display]);
    assert!(exact.not_applied.is_empty());
    assert_eq!(exact.display.frame_start, 0);
    assert_eq!(exact.display.layer_start, 0);

    // フォーカスを同時に指定しても、表示開始位置は要求どおりに残る。
    let with_focus = harness
        .edit
        .set_selection(&set_display(
            &harness,
            0,
            5,
            Some(FocusChange::Set { object: focused }),
        ))
        .expect("フォーカスを伴う要求が拒否されました");
    assert_eq!(
        with_focus.applied,
        vec![SelectionField::Display, SelectionField::Focus]
    );
    assert!(with_focus.not_applied.is_empty());
    assert_eq!(with_focus.display.frame_start, 5);
    assert_eq!(with_focus.display.layer_start, 0);
}

#[test]
fn a_clamped_cursor_stays_applied_while_a_clamped_display_start_does_not() {
    // 非対称は軸の性質の違いから来る。カーソルは反映値そのものが応答に載るため
    // 丸められたかを要求元が読める。表示範囲は開始位置以外が概数であり、載せた
    // 値から要求との一致を判定できないため、こちらだけを plugin が振り分ける。
    let harness = Harness::new();
    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition {
                layer: 30,
                frame: 3_000,
            }),
            selected_range: None,
            focus: None,
            display: Some(DisplayStart {
                layer: 30,
                frame: 3_000,
            }),
            expected_project_epoch: harness.epoch(),
        })
        .expect("クランプが失敗として返りました");

    assert_eq!(state.applied, vec![SelectionField::Cursor]);
    assert_eq!(state.not_applied, vec![SelectionField::Display]);
    assert_ne!(state.cursor.frame, 3_000);
    assert_ne!(state.display.frame_start, 3_000);
}

#[test]
fn a_focus_target_is_resolved_before_it_is_set() {
    let harness = Harness::new();
    let mut selector = harness.selector(1, 100);
    selector.fingerprint = tamper(&selector.fingerprint);

    let error = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: Some(FocusChange::Set { object: selector }),
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect_err("照合を経ずにフォーカスが設定されました");

    assert_eq!(error.details()["mismatch"], json!("fingerprint"));
    assert!(!harness.host.mutated());
    assert!(
        harness.host.scene().focus.is_none(),
        "解決できない対象の指定で選択が解除されました"
    );
}

#[test]
fn a_scene_guard_protects_the_selection() {
    let harness = Harness::new();
    let error = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID + 7,
            cursor: Some(CursorPosition { layer: 1, frame: 5 }),
            selected_range: None,
            focus: None,
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect_err("別シーンの前提が受理されました");

    assert_eq!(error.details()["mismatch"], json!("scene_id"));
    assert!(!harness.host.mutated());
}

// ------------------------------------------------------------------ 連続編集

#[test]
fn the_returned_selector_supports_the_next_edit_without_a_reread() {
    let harness = Harness::new();
    let first = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 30 },
        })
        .expect("1 回目の編集に失敗しました");

    let effect = first.effect.expect("変更後の effect");
    harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: effect.selector,
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 40 },
        })
        .expect("応答が返したセレクターで続けて編集できませんでした");
}

#[test]
fn the_previous_selector_is_rejected_on_the_second_edit() {
    let harness = Harness::new();
    let selector = harness.effect_selector(1, 100, "ぼかし", 0);

    harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: selector.clone(),
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 30 },
        })
        .expect("1 回目の編集に失敗しました");

    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector,
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 40 },
        })
        .expect_err("古いセレクターでの再送が受理されました");
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
}

// ------------------------------------------------- operation ごとの応答の形

#[test]
fn each_operation_fills_the_outcome_it_is_defined_to_fill() {
    // operation ごとの `object` / `effect` / `created` の設定内容を固定する。
    // この対応は core に存在しないため、ここでしか守られない。
    let harness = Harness::new();
    let outcome = harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "[obj]".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 600,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect("create_object");
    assert!(outcome.object.is_some());
    assert!(outcome.effect.is_none());
    assert_eq!(outcome.created.len(), 1);

    let harness = Harness::new();
    let outcome = harness
        .edit
        .move_object(&move_params(&harness))
        .expect("move_object");
    assert!(outcome.object.is_some());
    assert!(outcome.effect.is_none());
    assert!(outcome.created.is_empty());

    let harness = Harness::new();
    let outcome = harness
        .edit
        .delete_object(&DeleteObjectParams {
            selector: harness.selector(1, 100),
        })
        .expect("delete_object");
    assert!(outcome.object.is_none());
    assert!(outcome.effect.is_none());
    assert!(outcome.created.is_empty());

    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_object_name(&SetObjectNameParams {
            selector: harness.selector(1, 100),
            name: Some("名前".to_string()),
        })
        .expect("set_object_name");
    assert!(outcome.object.is_some());
    assert!(outcome.effect.is_none());
    assert!(outcome.created.is_empty());

    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 30 },
        })
        .expect("set_object_item");
    assert!(outcome.object.is_some());
    assert!(outcome.effect.is_some());
    assert!(outcome.created.is_empty());

    let harness = Harness::new();
    let outcome = harness
        .edit
        .add_effect(&AddEffectParams {
            object: harness.selector(1, 100),
            effect_name: "ぼかし".to_string(),
        })
        .expect("add_effect");
    assert!(outcome.object.is_some());
    assert!(outcome.effect.is_some());
    assert!(outcome.created.is_empty());

    let harness = Harness::new();
    let outcome = harness
        .edit
        .delete_effect(&DeleteEffectParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
        })
        .expect("delete_effect");
    assert!(outcome.object.is_some());
    assert!(
        outcome.effect.is_none(),
        "削除した effect を応答へ載せています"
    );
    assert!(outcome.created.is_empty());

    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_effect_enabled(&SetEffectEnabledParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            enabled: false,
        })
        .expect("set_effect_enabled");
    assert!(outcome.object.is_some());
    assert!(outcome.effect.is_some());
    assert!(outcome.created.is_empty());
}

// -------------------------------------------------------------- panic の境界

/// クロージャの内側の panic が、クロージャから漏れずに失敗へ変わることを確かめる。
///
/// 漏れた巻き戻しは実機では C の関数ポインタ境界でプロセスごと abort させる。
/// 応答のコードだけを見ると、クロージャの外側で捕捉しても同じ結果になるため、
/// **漏れなかったこと**まで確かめないと捕捉の位置を固定できない。
#[test]
fn a_panic_inside_the_closure_never_escapes_it() {
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.panic_at = Some(PanicPoint::InClosure)));
    let params = move_params(&harness);
    let error = with_silent_panic_hook(|| {
        harness
            .edit
            .move_object(&params)
            .expect_err("panic が伝播しました")
    });

    assert_eq!(error.error_code(), ErrorCode::InternalError);
    assert!(
        !harness.host.calls().contains(&CLOSURE_ESCAPED),
        "巻き戻しがクロージャの外へ漏れました。実機ではホストが落ちます"
    );
}

#[test]
fn a_panic_while_entering_the_section_is_caught() {
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.panic_at = Some(PanicPoint::EnterSection)));
    let params = move_params(&harness);
    let error = with_silent_panic_hook(|| {
        harness
            .edit
            .move_object(&params)
            .expect_err("panic が伝播しました")
    });

    assert_eq!(error.error_code(), ErrorCode::InternalError);
}

#[test]
fn a_panic_while_probing_readiness_is_caught() {
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.panic_at = Some(PanicPoint::IsReady)));
    let params = move_params(&harness);
    let error = with_silent_panic_hook(|| {
        harness
            .edit
            .move_object(&params)
            .expect_err("panic が伝播しました")
    });

    assert_eq!(error.error_code(), ErrorCode::InternalError);
}

// -------------------------------------------------------------- ロックの順序

#[test]
fn no_plugin_lock_is_held_while_the_sdk_runs() {
    let harness = Harness::with(|host| {
        host.arm(|knobs| knobs.probe_lock_in_section = true);
    });
    let params = move_params(&harness);
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let result = harness.edit.move_object(&params);
        let _ = tx.send(result.is_ok());
    });

    let finished = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("編集がプロジェクト境界のロックを保持したまま SDK を呼びました");
    assert!(finished);
}

// ------------------------------------------------------------------ エラー分類

#[test]
fn no_edit_failure_is_ever_reported_as_cancelled() {
    // 到達し得る失敗を一通り作り、取り消しとして返らないことを固定する。
    let scenarios: Vec<Box<dyn Fn() -> ErrorCode>> = vec![
        Box::new(|| {
            let harness = Harness::with(|host| host.arm(|knobs| knobs.ready = false));
            let params = move_params(&harness);
            harness.edit.move_object(&params).unwrap_err().error_code()
        }),
        Box::new(|| {
            let harness = Harness::with(|host| host.arm(|knobs| knobs.state = EditState::Preview));
            let params = move_params(&harness);
            harness.edit.move_object(&params).unwrap_err().error_code()
        }),
        Box::new(|| {
            let harness =
                Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::Section)));
            let params = move_params(&harness);
            harness.edit.move_object(&params).unwrap_err().error_code()
        }),
        Box::new(|| {
            let harness =
                Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::Mutation)));
            let params = move_params(&harness);
            harness.edit.move_object(&params).unwrap_err().error_code()
        }),
        Box::new(|| {
            let harness = Harness::new();
            let mut params = move_params(&harness);
            params.selector.fingerprint = tamper(&params.selector.fingerprint);
            harness.edit.move_object(&params).unwrap_err().error_code()
        }),
    ];
    for scenario in scenarios {
        assert_ne!(scenario(), ErrorCode::Cancelled);
    }
}

#[test]
fn a_failing_mutation_is_reported_as_issued() {
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::Mutation)));
    let params = move_params(&harness);
    let error = harness
        .edit
        .move_object(&params)
        .expect_err("変更 API の失敗");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(error.details()["sdk_operation"], json!("move_object"));
    assert_eq!(error.details()["mutation_issued"], json!(true));
}

#[test]
fn responses_and_failures_never_carry_a_handle_or_a_pointer() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .move_object(&move_params(&harness))
        .expect("移動に失敗しました");
    let text = serde_json::to_string(&outcome).expect("応答の直列化");
    assert!(!text.contains("0x"), "{text}");
    assert!(!text.to_lowercase().contains("handle"), "{text}");
}

/// 参照のみを取り込む使い方をしていることを、型として固定する。
///
/// レイヤーとオブジェクトの定義はフェイク側にしかない。ここで参照しておくと、
/// 定義を消したときにテストが落ちる。
#[test]
fn the_fake_scene_exposes_layers_and_objects() {
    let harness = Harness::new();
    let scene = harness.host.scene();
    let layer: &FakeLayer = &scene.layers[1];
    let object: &FakeObject = &layer.objects[0];
    assert_eq!(object.placement.frame_start, 100);
    assert!(!layer.locked);
}

#[test]
fn an_added_effect_is_located_even_when_the_host_does_not_append_it() {
    // 付与位置が末尾だと決めつけると、先頭へ挿入するホストで別の effect を
    // 指す selector を返してしまう。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::PrependEffect)));
    let outcome = harness
        .edit
        .add_effect(&AddEffectParams {
            object: harness.selector(1, 100),
            effect_name: "ぼかし".to_string(),
        })
        .expect("effect の付与に失敗しました");

    let effect = outcome.effect.expect("付与された effect");
    assert_eq!(effect.name, "ぼかし");
    // 先頭へ挿入されたため、同名内の順序は 0 になり既存の方が 1 へ繰り上がる。
    assert_eq!(effect.index, 0);
    // 列の位置も先頭である。末尾を決め打つと、ここで別の要素を指す。
    assert_eq!(effect.position, 0);
    let scene = harness.host.scene();
    let effects = &scene.layers[1].objects[0].effects;
    assert_eq!(effects[0].name, "ぼかし");
    assert_eq!(effects[0].index, 0);
}

#[test]
fn an_ambiguous_effect_difference_is_reported_instead_of_being_guessed() {
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::AddTwoEffects)));
    let error = harness
        .edit
        .add_effect(&AddEffectParams {
            object: harness.selector(1, 100),
            effect_name: "ぼかし".to_string(),
        })
        .expect_err("位置を特定できないのに selector が返りました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(error.details()["sdk_operation"], json!("create_effect"));
    assert_eq!(error.details()["mutation_issued"], json!(true));
}

#[test]
fn the_added_position_comes_from_the_difference_in_the_name_list() {
    let names = |list: &[&str]| -> Vec<String> { list.iter().map(|s| s.to_string()).collect() };

    // 末尾・中間・先頭のいずれへ挿入されても位置が求まる。
    assert_eq!(
        added_effect_position(&names(&["a", "b"]), &names(&["a", "b", "c"])),
        Some(2)
    );
    assert_eq!(
        added_effect_position(&names(&["a", "b"]), &names(&["a", "c", "b"])),
        Some(1)
    );
    assert_eq!(
        added_effect_position(&names(&["a", "b"]), &names(&["c", "a", "b"])),
        Some(0)
    );
    // 同名が並んでいても件数が 1 つ増えていれば位置が定まる。
    assert_eq!(
        added_effect_position(&names(&["a", "a"]), &names(&["a", "a", "a"])),
        Some(2)
    );

    // 増減が 1 件でない、あるいは並びが入れ替わった場合は位置を名乗らない。
    assert_eq!(added_effect_position(&names(&["a"]), &names(&["a"])), None);
    assert_eq!(
        added_effect_position(&names(&["a"]), &names(&["a", "b", "c"])),
        None
    );
    assert_eq!(
        added_effect_position(&names(&["a", "b"]), &names(&["b", "a", "c"])),
        None
    );
}

#[test]
fn every_nested_selector_is_checked_including_the_ones_inside_other_inputs() {
    // 判定は要求が含む全てのセレクターへ及ぶ。ネストしたセレクターだけが照合を
    // 免れると、そこから別プロジェクトの対象へ適用され得る。
    let harness = Harness::new();
    let mut item = SetObjectItemParams {
        selector: harness.effect_selector(1, 100, "ぼかし", 0),
        item: "範囲".to_string(),
        value: ItemValue::Integer { value: 10 },
    };
    item.selector.object.project_epoch = "別のプロジェクト".to_string();
    let error = harness
        .edit
        .set_object_item(&item)
        .expect_err("effect セレクターの内側の epoch 不一致が受理されました");
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));

    let harness = Harness::new();
    let mut object = harness.selector(1, 100);
    object.project_epoch = "別のプロジェクト".to_string();
    let error = harness
        .edit
        .add_effect(&AddEffectParams {
            object,
            effect_name: "ぼかし".to_string(),
        })
        .expect_err("付与先の epoch 不一致が受理されました");
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));

    let harness = Harness::new();
    let mut focus = harness.selector(1, 100);
    focus.project_epoch = "別のプロジェクト".to_string();
    let error = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: Some(FocusChange::Set { object: focus }),
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect_err("フォーカス対象の epoch 不一致が受理されました");
    // 選択状態の変更だけが epoch を 2 か所から受け取るため、出所を名乗る。
    assert_eq!(error.details()["mismatch"], json!("focus_project_epoch"));
    assert!(!harness.host.mutated());
}

#[test]
fn the_selection_change_names_which_epoch_did_not_match() {
    // 前提と focus の双方から epoch を受け取るのは選択状態の変更だけである。
    // どちらで落ちたかを伝えなければ、要求元は直す先を選べない。
    let harness = Harness::new();
    let error = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: Some(FocusChange::Set {
                object: harness.selector(1, 100),
            }),
            display: None,
            expected_project_epoch: "別のプロジェクト".to_string(),
        })
        .expect_err("別プロジェクトの前提が受理されました");
    assert_eq!(error.details()["mismatch"], json!("expected_project_epoch"));

    let harness = Harness::new();
    let mut focus = harness.selector(1, 100);
    focus.project_epoch = "別のプロジェクト".to_string();
    let error = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: Some(FocusChange::Set { object: focus }),
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect_err("別プロジェクトのフォーカス対象が受理されました");
    assert_eq!(error.details()["mismatch"], json!("focus_project_epoch"));

    // focus を省略した要求は epoch を 1 か所からしか受け取らない。出所を名乗る
    // 理由が無い。
    let harness = Harness::new();
    let error = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition { layer: 1, frame: 5 }),
            selected_range: None,
            focus: None,
            display: None,
            expected_project_epoch: "別のプロジェクト".to_string(),
        })
        .expect_err("別プロジェクトの前提が受理されました");
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));
}

#[test]
fn the_other_operations_do_not_tell_the_epoch_sources_apart() {
    // epoch を 1 か所からしか受け取らない要求で出所を名乗ると、要求元は
    // 1 つしか送っていない値に対して 2 つの分岐を持つことになる。
    for (name, run) in content_edits() {
        let harness = Harness::new();
        let mut target = harness.selector(1, 100);
        target.project_epoch = "別のプロジェクト".to_string();

        let Err(error) = run(&harness, target) else {
            panic!("{name} が別プロジェクトの対象を受理しました");
        };
        assert_eq!(
            error.details()["mismatch"],
            json!("project_epoch"),
            "{name} が epoch の出所を名乗りました"
        );
    }
}

// -------------------------------------------- SDK へ届かなかった変更の扱い

#[test]
fn a_failure_that_never_reached_the_sdk_is_not_recorded_as_a_mutation() {
    // 対象の存在確認は呼び出しの入口で行われ、そこで落ちた要求は SDK を
    // 呼ばずに戻る。プロジェクトは一切変わっていないため、変更を発行したと
    // 記録すると「何も変わっていないのに未保存の変更あり」が残り、要求元にも
    // 無意味な読み直しを強いる。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::TargetGone)));
    let params = move_params(&harness);
    let error = harness
        .edit
        .move_object(&params)
        .expect_err("届かなかった変更が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::NotFound);
    assert_eq!(error.details()["reason"], json!("target_missing"));
    assert!(
        error.details().get("mutation_issued").is_none(),
        "届いていない変更が発行として報告されました"
    );
    assert!(
        error.details().get("sdk_operation").is_none(),
        "呼ばれていない SDK 関数が名指しされました"
    );
    assert_eq!(
        harness.project.revision(),
        0,
        "何も変わっていないのに revision が進みました"
    );
    assert!(
        !harness.project.modified(),
        "何も変わっていないのに未保存の変更として記録されました"
    );
}

#[test]
fn a_failure_that_reached_the_sdk_is_still_recorded_as_a_mutation() {
    // 届いた呼び出しは、適用されたかどうかを戻り値から判断できない。
    // 判断できない場合は変更が入った側へ倒す。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::Mutation)));
    let params = move_params(&harness);
    let error = harness
        .edit
        .move_object(&params)
        .expect_err("変更 API の失敗");

    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(harness.project.revision(), 1);
}

// ------------------------------------------------------ 選択状態の部分適用

#[test]
fn a_partially_applied_selection_reports_both_lists() {
    // フォーカスだけが失敗する状況を作る。
    let harness = Harness::new();
    let focus = harness.selector(1, 100);
    harness
        .host
        .arm(|knobs| knobs.fault = Some(Fault::FocusGone));

    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: Some(FocusChange::Set { object: focus }),
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect("適用できた項目を伝える手段が失われました");

    assert!(state.applied.is_empty());
    assert_eq!(state.not_applied, vec![SelectionField::Focus]);
}

#[test]
fn the_same_selection_failure_does_not_change_success_by_what_else_was_requested() {
    // 同じ失敗が、同時に何を要求したかで成功にも失敗にも分かれてはならない。
    // 要求元から予測できなくなる。
    let harness = Harness::new();
    let focus = harness.selector(1, 100);
    harness
        .host
        .arm(|knobs| knobs.fault = Some(Fault::FocusGone));

    let alone = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: None,
            selected_range: None,
            focus: Some(FocusChange::Set {
                object: focus.clone(),
            }),
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect("フォーカスだけの要求");

    let harness = Harness::new();
    let focus = harness.selector(1, 100);
    harness
        .host
        .arm(|knobs| knobs.fault = Some(Fault::FocusGone));
    let combined = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition { layer: 0, frame: 1 }),
            selected_range: None,
            focus: Some(FocusChange::Set { object: focus }),
            display: None,
            expected_project_epoch: harness.epoch(),
        })
        .expect("カーソルを併せた要求");

    assert_eq!(alone.not_applied, vec![SelectionField::Focus]);
    assert_eq!(combined.not_applied, vec![SelectionField::Focus]);
}

#[test]
fn every_requested_selection_field_appears_in_exactly_one_list() {
    let harness = Harness::new();
    let state = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition { layer: 1, frame: 5 }),
            selected_range: Some(RangeChange::Clear {}),
            focus: Some(FocusChange::Clear {}),
            display: Some(DisplayStart {
                layer: 1,
                frame: 60,
            }),
            expected_project_epoch: harness.epoch(),
        })
        .expect("選択状態の変更");

    assert_eq!(
        state.applied,
        vec![
            SelectionField::Cursor,
            SelectionField::SelectedRange,
            SelectionField::Display,
            SelectionField::Focus
        ]
    );
    assert!(state.not_applied.is_empty());
    for field in &state.applied {
        assert!(
            !state.not_applied.contains(field),
            "{field:?} が両方に現れました"
        );
    }
}

#[test]
fn a_panic_after_a_mutation_still_reports_that_the_change_may_be_in() {
    // panic の捕捉は発行の記録を持つ許可ごと巻き戻す。変更が入った可能性を
    // 応答へ載せないと、revision は進んでいるのに要求元は「変更は入っていない
    // 恒久失敗」と読む。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.panic_at = Some(PanicPoint::AfterMutation)));
    let params = move_params(&harness);
    let error = with_silent_panic_hook(|| {
        harness
            .edit
            .move_object(&params)
            .expect_err("panic が伝播しました")
    });

    assert_eq!(error.error_code(), ErrorCode::InternalError);
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(error.details()["current_project_revision"], json!(1));
    assert_eq!(error.details()["retry_requires"], json!("refetch"));
    assert!(
        !harness.host.calls().contains(&CLOSURE_ESCAPED),
        "巻き戻しがクロージャの外へ漏れました"
    );
}

#[test]
fn a_panic_before_any_mutation_is_not_reported_as_a_possible_change() {
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.panic_at = Some(PanicPoint::InClosure)));
    let params = move_params(&harness);
    let error = with_silent_panic_hook(|| {
        harness
            .edit
            .move_object(&params)
            .expect_err("panic が伝播しました")
    });

    assert!(
        error.details().get("mutation_issued").is_none(),
        "何も変更していないのに変更が入った可能性として報告されました"
    );
}

// -------------------------------------------------- レイヤーの状態の変更

/// 何も変えないレイヤーの状態変更要求を組み立てる。
fn layer_state_params(harness: &Harness, layer: u32) -> SetLayerStateParams {
    SetLayerStateParams {
        expected_scene_id: SCENE_ID,
        layer,
        name: None,
        enabled: None,
        locked: None,
        expected_project_epoch: harness.epoch(),
    }
}

#[test]
fn the_three_layer_axes_can_be_set_alone_or_together() {
    // 軸ごとに、要求した軸だけが変わり、他の軸は元のままであること。
    let cases: [(&str, SetLayerStateParams, Option<&str>, bool, bool); 4] = {
        let harness = Harness::new();
        [
            (
                "name",
                SetLayerStateParams {
                    name: Some(LayerNameChange::Set {
                        name: "背景".to_string(),
                    }),
                    ..layer_state_params(&harness, 0)
                },
                Some("背景"),
                true,
                false,
            ),
            (
                "enabled",
                SetLayerStateParams {
                    enabled: Some(false),
                    ..layer_state_params(&harness, 0)
                },
                None,
                false,
                false,
            ),
            (
                "locked",
                SetLayerStateParams {
                    locked: Some(true),
                    ..layer_state_params(&harness, 0)
                },
                None,
                true,
                true,
            ),
            (
                "全て",
                SetLayerStateParams {
                    name: Some(LayerNameChange::Set {
                        name: "背景".to_string(),
                    }),
                    enabled: Some(false),
                    locked: Some(true),
                    ..layer_state_params(&harness, 0)
                },
                Some("背景"),
                false,
                true,
            ),
        ]
    };

    for (label, params, name, enabled, locked) in cases {
        let harness = Harness::new();
        let params = SetLayerStateParams {
            expected_project_epoch: harness.epoch(),
            ..params
        };
        let outcome = harness
            .edit
            .set_layer_state(&params)
            .unwrap_or_else(|error| panic!("{label} の変更が失敗しました: {error}"));

        assert_eq!(outcome.layer.index, 0, "{label}");
        assert_eq!(outcome.layer.name.as_deref(), name, "{label}");
        assert_eq!(outcome.layer.enabled, enabled, "{label}");
        assert_eq!(outcome.layer.locked, locked, "{label}");
        // 応答は読み取りの DTO をそのまま返すため、件数も載る。
        assert_eq!(outcome.layer.object_count, 1, "{label}");
        assert_eq!(outcome.project_epoch, harness.epoch(), "{label}");
        assert_eq!(outcome.project_revision, 1, "{label}");
        assert_eq!(harness.project.revision(), 1, "{label}");
        assert!(harness.project.modified(), "{label}");
    }
}

#[test]
fn resetting_the_layer_name_hands_the_sdk_no_name() {
    let harness = Harness::new();
    harness
        .edit
        .set_layer_state(&SetLayerStateParams {
            name: Some(LayerNameChange::Set {
                name: "背景".to_string(),
            }),
            ..layer_state_params(&harness, 0)
        })
        .expect("名前を設定できません");
    assert_eq!(harness.host.scene().layers[0].name.as_deref(), Some("背景"));

    let outcome = harness
        .edit
        .set_layer_state(&SetLayerStateParams {
            name: Some(LayerNameChange::Reset {}),
            ..layer_state_params(&harness, 0)
        })
        .expect("標準名へ戻せません");

    assert_eq!(outcome.layer.name, None, "標準名へ戻っていません");
    assert_eq!(harness.host.scene().layers[0].name, None);
    // 標準名へ戻す指定は、空の名前ではなく「名前を渡さない」ことで表す。
    assert_eq!(
        harness.host.layer_name_arguments(),
        vec![Some("背景".to_string()), None],
        "標準名へ戻す指定が空の名前として渡りました"
    );
}

/// レイヤーの状態のうち、要求できる軸。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayerAxis {
    Name,
    Enabled,
    Locked,
}

impl LayerAxis {
    /// 全軸。
    ///
    /// 要素数と内容は `layer_axes_are_exhaustive` が固定する。
    const ALL: [LayerAxis; 3] = [LayerAxis::Name, LayerAxis::Enabled, LayerAxis::Locked];

    /// 記録に残す軸の名前。
    fn label(self) -> &'static str {
        match self {
            LayerAxis::Name => "name",
            LayerAxis::Enabled => "enabled",
            LayerAxis::Locked => "locked",
        }
    }

    /// この軸だけを、現在と異なる値へ変える要求を組み立てる。
    ///
    /// **網羅 match で書く。** 軸を足すとここがコンパイルエラーになるため、
    /// read-back の確認から漏れることがない。要求値は必ず現在値と異なる——
    /// 同じ値を要求すると、照合が働かなくても一致してしまう。
    fn request(self, params: SetLayerStateParams) -> SetLayerStateParams {
        match self {
            LayerAxis::Name => SetLayerStateParams {
                name: Some(LayerNameChange::Set {
                    name: "背景".to_string(),
                }),
                ..params
            },
            LayerAxis::Enabled => SetLayerStateParams {
                enabled: Some(false),
                ..params
            },
            LayerAxis::Locked => SetLayerStateParams {
                locked: Some(true),
                ..params
            },
        }
    }
}

#[test]
fn layer_axes_are_exhaustive() {
    // 網羅 match は軸の追加を止めるが、`ALL` は手書きである。両方を突き合わせる。
    fn assert_listed(axis: LayerAxis) {
        match axis {
            LayerAxis::Name | LayerAxis::Enabled | LayerAxis::Locked => {}
        }
        assert!(
            LayerAxis::ALL.contains(&axis),
            "{} が LayerAxis::ALL に含まれていません",
            axis.label()
        );
    }

    assert_listed(LayerAxis::Name);
    assert_listed(LayerAxis::Enabled);
    assert_listed(LayerAxis::Locked);
    assert_eq!(LayerAxis::ALL.len(), 3);
}

#[test]
fn a_layer_state_that_did_not_take_effect_is_not_reported_as_a_success() {
    // 3 つの setter は戻り値を持たない。無言で無視されたことは読み直しでしか
    // 分からず、read-back が唯一の防波堤である。**軸ごとに確かめる。** 1 つの
    // 軸で通しても、他の軸の照合が抜けていれば、その軸の無言の拒否は成功と
    // して返る。
    for axis in LayerAxis::ALL {
        let name = axis.label();
        let harness =
            Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreLayerState)));
        let params = axis.request(layer_state_params(&harness, 0));
        let error = harness
            .edit
            .set_layer_state(&params)
            .err()
            .unwrap_or_else(|| panic!("{name} の反映されていない変更が成功として返りました"));

        assert_eq!(
            error.error_code(),
            ErrorCode::UnsupportedOperation,
            "{name}"
        );
        assert_eq!(
            error.details()["reason"],
            json!("change_not_applied"),
            "{name}"
        );
        // SDK へは届いている。届いた以上は変更が入った側へ倒す。
        assert_eq!(error.details()["mutation_issued"], json!(true), "{name}");
    }
}

#[test]
fn every_layer_axis_is_applied_when_the_host_accepts_it() {
    // 上の確認の対になる。要求が通る状態で失敗するなら、read-back の照合が
    // 厳しすぎることになる。
    for axis in LayerAxis::ALL {
        let name = axis.label();
        let harness = Harness::new();
        let params = axis.request(layer_state_params(&harness, 0));
        harness
            .edit
            .set_layer_state(&params)
            .unwrap_or_else(|error| panic!("{name} の変更が拒否されました: {error}"));
    }
}

#[test]
fn the_layer_state_read_back_takes_the_three_attributes_at_once() {
    let harness = Harness::new();
    let params = SetLayerStateParams {
        name: Some(LayerNameChange::Set {
            name: "背景".to_string(),
        }),
        enabled: Some(false),
        locked: Some(true),
        ..layer_state_params(&harness, 0)
    };
    harness.host.clear_calls();
    harness
        .edit
        .set_layer_state(&params)
        .expect("変更が失敗しました");

    let calls = harness.host.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| **call == LAYER_ATTRIBUTES)
            .count(),
        1,
        "読み直しが属性ごとに分かれています: {calls:?}"
    );
}

#[test]
fn changing_the_layer_state_is_not_stopped_by_the_layer_lock() {
    // ロックを外すこの operation にロックのガードを掛けると、ロックされた
    // レイヤーの行き止まりが解けなくなる。
    let harness = Harness::new();
    assert!(
        harness.host.scene().layers[2].locked,
        "レイヤー 2 がロックされていません"
    );
    harness.host.clear_calls();

    // ロックは 3 軸のいずれも止めない。ロックを外す軸だけを確かめると、名前や
    // 表示にだけガードが掛かった実装が素通りする。
    harness
        .edit
        .set_layer_state(&SetLayerStateParams {
            name: Some(LayerNameChange::Set {
                name: "背景".to_string(),
            }),
            ..layer_state_params(&harness, 2)
        })
        .expect("ロックされたレイヤーの名前を変えられません");
    harness
        .edit
        .set_layer_state(&SetLayerStateParams {
            enabled: Some(false),
            ..layer_state_params(&harness, 2)
        })
        .expect("ロックされたレイヤーの表示を変えられません");
    let outcome = harness
        .edit
        .set_layer_state(&SetLayerStateParams {
            locked: Some(false),
            ..layer_state_params(&harness, 2)
        })
        .expect("ロックされたレイヤーのロックを外せません");

    assert!(!outcome.layer.locked, "ロックが外れていません");
    assert!(!harness.host.scene().layers[2].locked);
    assert!(
        !harness.host.calls().contains(&LAYER_LOCK),
        "ロックの確認を行いました: {:?}",
        harness.host.calls()
    );
}

#[test]
fn changing_the_layer_state_advances_the_revision_once_for_all_three_axes() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_layer_state(&SetLayerStateParams {
            name: Some(LayerNameChange::Set {
                name: "背景".to_string(),
            }),
            enabled: Some(false),
            locked: Some(true),
            ..layer_state_params(&harness, 0)
        })
        .expect("変更が失敗しました");

    assert_eq!(outcome.project_revision, 1);
    assert_eq!(
        harness.project.revision(),
        1,
        "軸ごとに revision が進みました"
    );
}

// ------------------------------------- 内容を変える operation の共通の約束

/// 内容を変える operation を 1 つ、指す対象を差し替えて実行する。
///
/// 第 2 引数は「この要求はどのプロジェクトのどのシーンの、どの対象を指すか」の
/// 表明である。対象を持つ operation はこれをそのまま selector として渡し、対象
/// を持たない作成とレイヤーの状態変更は epoch を前提へ、シーンを guard へ、
/// レイヤー番号を対象へ写す。どれも同じ表明であり、食い違わせたときに拒否される
/// ことを同じ形で確かめられる。
///
/// 返すのは応答が載せる revision だけである。応答の型は operation ごとに
/// 異なるが、この表が確かめるのはどれにも共通する 1 つの値である。
type ContentEdit = fn(&Harness, ObjectSelector) -> Result<u64, EditError>;

/// operation を 1 つ実行する手続きを引く。
///
/// **網羅 match で書く。** operation を足すとここがコンパイルエラーになるため、
/// revision の加算・ロックの拒否・境界の照合を確かめる一連のテストから漏れる
/// ことがない。手書きの一覧にしておくと、足し忘れても全て緑のまま通ってしまう。
///
/// 選択状態の変更だけは内容を変えないため `None` を返す。含めるかどうかで
/// revision の扱いが変わるので、その区別もこの 1 か所に置く。
fn content_edit(operation: EditOperation) -> Option<ContentEdit> {
    Some(match operation {
        EditOperation::CreateObject => |harness: &Harness, target: ObjectSelector| {
            let ObjectSelector {
                project_epoch,
                scene_id,
                ..
            } = target;
            harness
                .edit
                .create_object(&CreateObjectParams {
                    source: ObjectSource::ObjectAlias {
                        alias: "[obj]".to_string(),
                    },
                    placement: Placement {
                        scene_id,
                        layer: 1,
                        frame: 600,
                    },
                    expected_project_epoch: project_epoch,
                })
                .map(revision_of)
        },
        EditOperation::MoveObject => |harness: &Harness, target| {
            harness
                .edit
                .move_object(&MoveObjectParams {
                    selector: target,
                    destination: Destination {
                        layer: 1,
                        frame: 500,
                    },
                })
                .map(revision_of)
        },
        EditOperation::DeleteObject => |harness: &Harness, target| {
            harness
                .edit
                .delete_object(&DeleteObjectParams { selector: target })
                .map(revision_of)
        },
        EditOperation::SetObjectName => |harness: &Harness, target| {
            harness
                .edit
                .set_object_name(&SetObjectNameParams {
                    selector: target,
                    name: Some("名前".to_string()),
                })
                .map(revision_of)
        },
        EditOperation::SetObjectItem => |harness: &Harness, target| {
            harness
                .edit
                .set_object_item(&SetObjectItemParams {
                    selector: harness.effect_selector_of(target, "ぼかし", 0),
                    item: "範囲".to_string(),
                    value: ItemValue::Integer { value: 30 },
                })
                .map(revision_of)
        },
        EditOperation::AddEffect => |harness: &Harness, target| {
            harness
                .edit
                .add_effect(&AddEffectParams {
                    object: target,
                    effect_name: "ぼかし".to_string(),
                })
                .map(revision_of)
        },
        EditOperation::DeleteEffect => |harness: &Harness, target| {
            harness
                .edit
                .delete_effect(&DeleteEffectParams {
                    selector: harness.effect_selector_of(target, "ぼかし", 0),
                })
                .map(revision_of)
        },
        EditOperation::SetEffectEnabled => |harness: &Harness, target| {
            harness
                .edit
                .set_effect_enabled(&SetEffectEnabledParams {
                    selector: harness.effect_selector_of(target, "ぼかし", 0),
                    enabled: false,
                })
                .map(revision_of)
        },
        EditOperation::SetLayerState => |harness: &Harness, target: ObjectSelector| {
            let ObjectSelector {
                project_epoch,
                scene_id,
                layer,
                ..
            } = target;
            harness
                .edit
                .set_layer_state(&SetLayerStateParams {
                    expected_scene_id: scene_id,
                    layer: layer as u32,
                    name: Some(LayerNameChange::Set {
                        name: "レイヤー".to_string(),
                    }),
                    enabled: None,
                    locked: None,
                    expected_project_epoch: project_epoch,
                })
                .map(|outcome| outcome.project_revision)
        },
        EditOperation::SetGridBpm => |harness: &Harness, target: ObjectSelector| {
            let ObjectSelector {
                project_epoch,
                scene_id,
                ..
            } = target;
            harness
                .edit
                .set_grid_bpm(&SetGridBpmParams {
                    expected_scene_id: scene_id,
                    entries: vec![grid_bpm(140.0, 3, 0.0, 0.0)],
                    expected_project_epoch: project_epoch,
                })
                .map(|outcome| outcome.project_revision)
        },
        EditOperation::CreateObjectSection => |harness: &Harness, target| {
            harness
                .edit
                .create_object_section(&CreateObjectSectionParams {
                    selector: target,
                    frame: 120,
                })
                .map(|outcome| outcome.project_revision)
        },
        EditOperation::DeleteObjectSection => |harness: &Harness, target| {
            harness
                .edit
                .delete_object_section(&DeleteObjectSectionParams {
                    selector: target,
                    section: 1,
                })
                .map(|outcome| outcome.project_revision)
        },
        EditOperation::MoveObjectSection => |harness: &Harness, target| {
            harness
                .edit
                .move_object_section(&MoveObjectSectionParams {
                    selector: target,
                    section: 1,
                    frame: 160,
                })
                .map(|outcome| outcome.project_revision)
        },
        EditOperation::SetSceneSettings => |harness: &Harness, target: ObjectSelector| {
            let ObjectSelector {
                project_epoch,
                scene_id,
                ..
            } = target;
            harness
                .edit
                .set_scene_settings(&SetSceneSettingsParams {
                    expected_scene_id: scene_id,
                    name: Some("本編".to_string()),
                    size: Some(SceneSize {
                        width: 1280,
                        height: 720,
                    }),
                    sample_rate: Some(44_100),
                    expected_project_epoch: project_epoch,
                })
                .map(|outcome| outcome.project_revision)
        },
        // 選択状態はプロジェクトの内容ではない。revision を進めない。
        EditOperation::SetSelection => return None,
        // 一括適用に対応する編集口のメソッドはまだ無い。実装を足すときに
        // ここへ手続きを書き、下の表から自動的に検査される。
        EditOperation::ApplyBatch => return None,
    })
}

/// 応答が載せる revision を取り出す。
fn revision_of(outcome: EditOutcome) -> u64 {
    outcome.project_revision
}

/// 内容を変える operation を全て、名前つきで列挙する。
fn content_edits() -> Vec<(&'static str, ContentEdit)> {
    EditOperation::ALL
        .into_iter()
        .filter_map(|operation| content_edit(operation).map(|run| (operation.as_str(), run)))
        .collect()
}

#[test]
fn the_content_edit_table_leaves_out_only_the_declared_operations() {
    // 網羅 match は operation の追加を止めるが、既存の枝を除外へ書き換えても
    // 止まらない。表から外れているものを併せて固定することで、追加も除外も
    // 見逃さない。
    let excluded: Vec<&str> = EditOperation::ALL
        .into_iter()
        .filter(|operation| content_edit(*operation).is_none())
        .map(EditOperation::as_str)
        .collect();

    assert_eq!(
        excluded,
        vec![
            EditOperation::SetSelection.as_str(),
            EditOperation::ApplyBatch.as_str(),
        ]
    );
}

#[test]
fn every_content_edit_accepts_a_revision_that_advanced() {
    // revision はプロジェクト全体で 1 つのカウンタであり、どの対象を編集しても
    // UI 上の操作でも進む。読み取りから編集までの間に進んだだけで拒否すると、
    // 人が編集しているプロジェクトでは収束しない。対象が変化していないことは
    // fingerprint が、プロジェクトが同じであることは epoch が保証する。
    for (name, run) in content_edits() {
        let harness = Harness::new();
        let target = harness.selector(1, 100);
        // 対象は変えずに revision だけを進める。fingerprint は一致したままである。
        harness.project.on_object_updated();

        run(&harness, target).unwrap_or_else(|error| {
            panic!("{name} が revision の進みを理由に拒否しました: {error}")
        });
        assert!(
            harness.host.mutated(),
            "{name} が変更 API を呼びませんでした"
        );
    }
}

#[test]
fn every_content_edit_advances_the_revision_once() {
    for (name, run) in content_edits() {
        let harness = Harness::new();
        let target = harness.selector(1, 100);
        let revision = run(&harness, target).unwrap_or_else(|error| {
            panic!("{name} が失敗しました: {error}");
        });

        assert_eq!(
            harness.project.revision(),
            1,
            "{name} が revision を進めていません"
        );
        assert_eq!(
            revision, 1,
            "{name} の応答が加算後の revision を返していません"
        );
        assert!(
            harness.project.modified(),
            "{name} が未保存の変更を記録していません"
        );
    }
}

/// ロックされたレイヤー上の対象に対する operation の可否。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockedLayer {
    /// `precondition_failed` / `layer_locked` で拒否される。
    Refused,
    /// 成功する。
    Allowed,
}

/// ロックされたレイヤーに対する operation の可否を述べる。
///
/// **網羅 match で書く。** operation を足すとここがコンパイルエラーになり、
/// 可否を書くまで進めない。手書きの一覧にすると、足し忘れた operation が
/// 検査を素通りする。
///
/// 拒否するのは、レイヤーのロックが UI で止めるもの——オブジェクトの削除と
/// 時間軸上の移動と、中間点の追加・移動・削除——に限る。設定値の変更も effect の
/// 増減も UI の設定パネルから行えるため、MCP からだけ拒む理由が無い。選択状態の
/// 変更は対象を書き換えないため表に載らない。実行できない operation も同じく
/// 載らない。
fn locked_layer(operation: EditOperation) -> Option<LockedLayer> {
    Some(match operation {
        EditOperation::CreateObject
        | EditOperation::MoveObject
        | EditOperation::DeleteObject
        | EditOperation::CreateObjectSection
        | EditOperation::DeleteObjectSection
        | EditOperation::MoveObjectSection => LockedLayer::Refused,
        EditOperation::SetObjectName
        | EditOperation::SetObjectItem
        | EditOperation::AddEffect
        | EditOperation::DeleteEffect
        | EditOperation::SetEffectEnabled
        // ロックを外す手段そのものをロックで止めると、行き止まりが解けなくなる。
        | EditOperation::SetLayerState
        // BPM グリッドとシーン設定はシーンに属し、どのレイヤーの対象にも触れない。
        | EditOperation::SetGridBpm
        | EditOperation::SetSceneSettings => LockedLayer::Allowed,
        EditOperation::SetSelection => return None,
        // 一括適用に対応する編集口のメソッドはまだ無い。実装を足すときに
        // 可否をここへ書く。
        EditOperation::ApplyBatch => return None,
    })
}

#[test]
fn the_layer_lock_stops_exactly_the_declared_operations() {
    for operation in EditOperation::ALL {
        let (Some(run), Some(expected)) = (content_edit(operation), locked_layer(operation)) else {
            continue;
        };
        let name = operation.as_str();
        let harness = Harness::new();
        let target = harness.selector(1, 100);
        // 対象・移動先・作成先を含むレイヤーをロックする。
        harness.host.lock_layer(1, true);

        let result = run(&harness, target);
        match expected {
            LockedLayer::Refused => {
                let Err(error) = result else {
                    panic!("{name} がロックされたレイヤーを書き換えました");
                };
                assert_eq!(
                    error.error_code(),
                    ErrorCode::PreconditionFailed,
                    "{name} がロックを前提条件として扱いません"
                );
                assert_eq!(error.details()["reason"], json!("layer_locked"), "{name}");
                assert!(!harness.host.mutated(), "{name} が変更 API を呼びました");
                assert_eq!(harness.project.revision(), 0, "{name}");
            }
            LockedLayer::Allowed => {
                result
                    .unwrap_or_else(|error| panic!("{name} がロックを理由に拒否しました: {error}"));
                assert!(harness.host.mutated(), "{name} が変更 API を呼びません");
            }
        }
    }
}

#[test]
fn the_layer_lock_table_leaves_out_only_the_declared_operations() {
    // 網羅 match は operation の追加を止めるが、既存の枝を除外へ書き換えても
    // 止まらない。表に載らないものを併せて固定する。
    let excluded: Vec<&str> = EditOperation::ALL
        .into_iter()
        .filter(|operation| locked_layer(*operation).is_none())
        .map(EditOperation::as_str)
        .collect();

    assert_eq!(
        excluded,
        vec![
            EditOperation::SetSelection.as_str(),
            EditOperation::ApplyBatch.as_str(),
        ]
    );
}

#[test]
fn moving_checks_the_lock_of_both_the_source_and_the_destination() {
    // 移動元だけがロックされている場合。
    let harness = Harness::new();
    let error = harness
        .edit
        .move_object(&MoveObjectParams {
            selector: harness.selector(2, 0),
            destination: Destination {
                layer: 1,
                frame: 500,
            },
        })
        .expect_err("ロックされたレイヤーから移動できました");
    assert_eq!(error.details()["reason"], json!("layer_locked"));
    assert_eq!(error.details()["layer"], json!(2));
    harness.assert_untouched();

    // 移動先だけがロックされている場合。
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.destination.layer = 2;
    params.destination.frame = 500;
    let error = harness
        .edit
        .move_object(&params)
        .expect_err("ロックされたレイヤーへ移動できました");
    assert_eq!(error.details()["reason"], json!("layer_locked"));
    assert_eq!(error.details()["layer"], json!(2));
    harness.assert_untouched();
}

#[test]
fn the_lock_check_reads_only_the_lock_state() {
    // ここで使うのは 1 ビットである。名前と表示まで読むと、応答に現れない値の
    // 読み取り失敗が移動と削除の可否を左右する。
    let harness = Harness::new();
    let params = move_params(&harness);
    harness.host.clear_calls();
    harness
        .edit
        .move_object(&params)
        .expect("移動に失敗しました");

    let calls = harness.host.calls();
    assert!(
        !calls.contains(&LAYER_ATTRIBUTES),
        "ロックの確認がレイヤー属性をまとめて読みました: {calls:?}"
    );
}

#[test]
fn moving_within_one_layer_reads_the_lock_state_once() {
    // 移動元と移動先が同じレイヤーになる移動で 2 回読む理由が無い。
    let harness = Harness::new();
    let params = move_params(&harness);
    harness.host.clear_calls();
    harness
        .edit
        .move_object(&params)
        .expect("移動に失敗しました");
    assert_eq!(
        harness
            .host
            .calls()
            .iter()
            .filter(|call| **call == LAYER_LOCK)
            .count(),
        1,
        "同一レイヤー内の移動でロック状態を 2 回読みました"
    );

    // レイヤーを跨ぐ移動では移動元と移動先の双方を読む。
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.destination.layer = 0;
    params.destination.frame = 500;
    harness.host.clear_calls();
    harness
        .edit
        .move_object(&params)
        .expect("移動に失敗しました");
    assert_eq!(
        harness
            .host
            .calls()
            .iter()
            .filter(|call| **call == LAYER_LOCK)
            .count(),
        2,
        "レイヤーを跨ぐ移動で片方のロックしか確かめていません"
    );
}

#[test]
fn every_content_edit_refuses_a_target_from_another_project() {
    // fingerprint の材料に project_epoch は含まれない。同じプロジェクトを複製
    // した 2 つのインスタンスでは fingerprint も revision も一致し得るため、
    // 対象が名乗る epoch の照合だけが、別インスタンス向けに作った要求を止める。
    for (name, run) in content_edits() {
        let harness = Harness::new();
        let mut target = harness.selector(1, 100);
        target.project_epoch = "別のプロジェクト".to_string();

        let Err(error) = run(&harness, target) else {
            panic!("{name} が別プロジェクトの対象を受理しました");
        };
        assert_eq!(
            error.error_code(),
            ErrorCode::PreconditionFailed,
            "{name} が epoch の食い違いを前提条件として扱いません"
        );
        assert_eq!(
            error.details()["mismatch"],
            json!("project_epoch"),
            "{name}"
        );
        harness.assert_untouched();
    }
}

#[test]
fn every_content_edit_refuses_a_target_from_another_scene() {
    // シーン切替のイベントは非同期であり、配送前の窓では revision が一致した
    // まま現在シーンだけが変わる。対象が名乗るシーンとの照合が抜けると、別の
    // シーンの同じ位置へ適用される。
    for (name, run) in content_edits() {
        let harness = Harness::new();
        let mut target = harness.selector(1, 100);
        target.scene_id = SCENE_ID + 7;

        let Err(error) = run(&harness, target) else {
            panic!("{name} が別シーンの対象を受理しました");
        };
        assert_eq!(
            error.error_code(),
            ErrorCode::PreconditionFailed,
            "{name} がシーンの食い違いを前提条件として扱いません"
        );
        assert_eq!(error.details()["mismatch"], json!("scene_id"), "{name}");
        harness.assert_untouched();
    }
}

#[test]
fn creation_checks_the_scene_guard_of_its_placement() {
    // 作成は対象を指すセレクターを持たないため、配置先の guard だけが別シーンへの
    // 適用を防ぐ。シーン切替のイベントは非同期であり、配送前の窓では revision が
    // 一致したまま別シーンになり得る。
    let harness = Harness::new();
    let error = harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "[obj]".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID + 7,
                layer: 1,
                frame: 600,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect_err("別シーン向けの作成が受理されました");

    assert_eq!(error.details()["mismatch"], json!("scene_id"));
    assert_eq!(error.details()["expected_scene_id"], json!(SCENE_ID + 7));
    harness.assert_untouched();
}

#[test]
fn the_response_revision_comes_from_the_increment_not_from_a_reread() {
    // ホストが plugin 発の編集にも対象更新を配送する環境では、加算のあとに
    // 読み直すと別の値を読む。応答が返す revision が非決定になり、要求元の
    // 次の編集が確率的に前提条件で落ちる。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.bump_after_mutation = 3));
    let params = move_params(&harness);
    let outcome = harness
        .edit
        .move_object(&params)
        .expect("移動に失敗しました");

    assert_eq!(
        outcome.project_revision, 1,
        "応答が加算時点の値ではなく読み直した値を返しています"
    );
    assert_eq!(
        harness.project.revision(),
        4,
        "読み直せば別の値になる状況が作れていません"
    );
}

#[test]
fn disabling_an_input_item_is_reported_with_the_reread_state() {
    // 入力項目は有効・無効を変更できる。応答が返す effect は読み直した値であり、
    // 要求値をそのまま echo したものではない。
    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_effect_enabled(&SetEffectEnabledParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            enabled: false,
        })
        .expect("入力項目の無効化が拒否されました");

    assert!(!outcome.effect.expect("変更後の effect").enabled);
}

/// 中間点を 3 つ持つ対象を用意した一式を組む。
///
/// 区間は 4 つになる。区間番号 `i` と `sections[i]` の対応が 1 つずれていれば、
/// 中間点が 1 つしか無い状態では区別できないため、番号を跨いで確かめられる
/// 数の中間点を置く。
fn harness_with_sections() -> Harness {
    let harness = Harness::new();
    harness.host.set_section_points(1, 100, vec![120, 150, 180]);
    harness
}

/// 応答の区間を `(start, end)` の列として取り出す。
fn section_pairs(outcome: &ObjectSectionsOutcome) -> Vec<(usize, usize)> {
    outcome
        .sections
        .iter()
        .map(|section| (section.start, section.end))
        .collect()
}

#[test]
fn the_section_index_addresses_the_same_element_of_the_sections_list() {
    // 区間番号 i は sections[i] を指す。i 番目の中間点を sections[i-1] へ写す
    // 実装は、区間 2 の削除で 120 ではなく 150 を消す。
    let harness = harness_with_sections();
    let outcome = harness
        .edit
        .delete_object_section(&DeleteObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 2,
        })
        .expect("区間 2 の削除が拒否されました");

    // 消えたのは sections[2].start = 150 であり、sections[1].start = 120 ではない。
    assert_eq!(harness.host.section_points(1, 100), vec![120, 180]);
    assert_eq!(
        section_pairs(&outcome),
        vec![(100, 119), (120, 179), (180, 200)]
    );
}

#[test]
fn moving_a_section_moves_the_boundary_that_starts_it() {
    // 区間 1 の開始位置は 1 番目の中間点 120 である。1 つずれた実装は 150 を
    // 動かし、応答の sections[1].start が要求したフレームにならない。
    let harness = harness_with_sections();
    let outcome = harness
        .edit
        .move_object_section(&MoveObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 1,
            frame: 110,
        })
        .expect("区間 1 の移動が拒否されました");

    assert_eq!(harness.host.section_points(1, 100), vec![110, 150, 180]);
    assert_eq!(outcome.sections[1].start, 110);
    assert_eq!(
        section_pairs(&outcome),
        vec![(100, 109), (110, 149), (150, 179), (180, 200)]
    );
}

/// フォーカスの区間番号が、対象の詳細が返す区間の列の添字であることを確かめる。
///
/// 2 つの tool にまたがる契約であり、片方の応答だけを見ても崩れに気付けない。
/// 同じ状態に対して両方を呼び、番号が列の範囲に収まること、指した要素が中間点で
/// 始まること、中間点を動かせば両者が揃って追随することを見る。
#[test]
fn the_focused_section_number_indexes_the_sections_of_the_focused_object() {
    let harness = harness_with_sections();
    harness.host.focus_object(Some((1, 100)), Some(2));

    let focused_section = |harness: &Harness| {
        let snapshot = harness
            .read
            .get_selection(SCENE_ID, &default_page_request())
            .expect("選択を取得できます")
            .expect("ページ要求が拒否されました");
        let focus = snapshot.focus.expect("フォーカス対象がありません");
        let section = snapshot.focus_section.expect("区間番号がありません");
        let detail = harness
            .read
            .get_object(&focus.selector)
            .expect("フォーカス対象の詳細を引けません");
        assert!(
            section < detail.sections.len(),
            "区間番号 {section} が区間の列 {:?} の外を指しています",
            detail.sections
        );
        (section, detail.sections[section].start)
    };

    // 区間 2 の開始位置は 2 番目の中間点である。
    assert_eq!(focused_section(&harness), (2, 150));

    harness
        .edit
        .move_object_section(&MoveObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 2,
            frame: 160,
        })
        .expect("区間 2 の移動が拒否されました");

    // 番号は変わらず、指す先だけが動く。
    assert_eq!(focused_section(&harness), (2, 160));
}

#[test]
fn creating_a_section_puts_the_frame_at_the_start_of_a_section() {
    let harness = harness_with_sections();
    let outcome = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 160,
        })
        .expect("中間点の追加が拒否されました");

    assert!(
        outcome.sections.iter().any(|section| section.start == 160),
        "追加したフレームが区間の開始フレームとして現れていません: {:?}",
        outcome.sections
    );
    assert_eq!(
        section_pairs(&outcome),
        vec![(100, 119), (120, 149), (150, 159), (160, 179), (180, 200)]
    );
}

#[test]
fn the_section_response_carries_the_state_after_the_change() {
    // 応答の sections は read-back そのものである。変更前の複製を返す実装では
    // 件数が増えない。
    let harness = harness_with_sections();
    let before = harness
        .read
        .get_object(&harness.selector(1, 100))
        .expect("対象の詳細を取得できませんでした")
        .sections
        .len();
    let outcome = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 160,
        })
        .expect("中間点の追加が拒否されました");

    assert_eq!(outcome.sections.len(), before + 1);
}

#[test]
fn the_section_response_carries_the_selector_after_the_change() {
    // 応答の selector と fingerprint は変更後に読み直した値である。要求で
    // 受け取った selector をそのまま返す実装では、対象の現在の姿が分からない。
    let harness = harness_with_sections();
    let selector = harness.selector(1, 100);
    let outcome = harness
        .edit
        .delete_object_section(&DeleteObjectSectionParams {
            selector: selector.clone(),
            section: 1,
        })
        .expect("中間点の削除が拒否されました");

    assert_eq!(outcome.object.selector.layer, selector.layer);
    assert_eq!(outcome.object.selector.frame, selector.frame);
    assert_eq!(outcome.object.selector.project_epoch, harness.epoch());
    // 読み直した対象をそのまま次の編集へ渡せる。
    harness
        .edit
        .delete_object_section(&DeleteObjectSectionParams {
            selector: outcome.object.selector.clone(),
            section: 1,
        })
        .expect("応答が返した selector で続けて編集できませんでした");
}

#[test]
fn the_section_response_carries_no_alias() {
    // 応答が返すのは概要であり詳細ではない。
    let harness = harness_with_sections();
    let outcome = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 160,
        })
        .expect("中間点の追加が拒否されました");
    let value = serde_json::to_value(&outcome).expect("応答は直列化できる");
    assert!(
        !value.to_string().contains("alias"),
        "応答に alias が現れています: {value}"
    );
}

/// 理由を実際に起こす要求を、事前確認へ通した結果として並べる。
///
/// [`SectionPreconditionReason`] に対する網羅 `match` であり `_` を使わない。
/// **理由を足すとここが落ち、それを起こす要求を書くまでコンパイルできない。**
/// 理由を数え上げるのは [`SectionPreconditionReason::ALL`] の役目であり、
/// 事前確認が実際にその理由で落とすことの証明は要求の側が持つ。
fn section_precondition_case(
    harness: &Harness,
    reason: &SectionPreconditionReason,
) -> Vec<EditError> {
    let selector = || harness.selector(1, 100);
    match reason {
        SectionPreconditionReason::FrameOutsideObject => vec![
            harness
                .edit
                .create_object_section(&CreateObjectSectionParams {
                    selector: selector(),
                    frame: 400,
                })
                .expect_err("オブジェクトの範囲外への追加が受理されました"),
        ],
        SectionPreconditionReason::SectionBoundaryExists => vec![
            harness
                .edit
                .create_object_section(&CreateObjectSectionParams {
                    selector: selector(),
                    frame: 150,
                })
                .expect_err("既にある境界への追加が受理されました"),
        ],
        // 区間数との比較は削除と移動の双方に掛かる。移動だけが素通りすると、
        // 番号が範囲外の要求が事前確認を抜けて SDK へ届く。
        SectionPreconditionReason::SectionIndexOutOfRange => vec![
            harness
                .edit
                .delete_object_section(&DeleteObjectSectionParams {
                    selector: selector(),
                    section: 4,
                })
                .expect_err("区間数以上の番号での削除が受理されました"),
            harness
                .edit
                .move_object_section(&MoveObjectSectionParams {
                    selector: selector(),
                    section: 4,
                    frame: 190,
                })
                .expect_err("区間数以上の番号での移動が受理されました"),
        ],
        SectionPreconditionReason::SectionMoveCrossesBoundary => vec![
            harness
                .edit
                .move_object_section(&MoveObjectSectionParams {
                    selector: selector(),
                    section: 1,
                    frame: 150,
                })
                .expect_err("後ろの中間点を越える移動が受理されました"),
            // 下限は 1 つ前の区間の開始フレーム「以下」を拒否する。等号を含め
            // ないと、中間点をひとつ前の境界そのものへ重ねられる。
            harness
                .edit
                .move_object_section(&MoveObjectSectionParams {
                    selector: selector(),
                    section: 1,
                    frame: 100,
                })
                .expect_err("ひとつ前の区間の開始フレームへの移動が受理されました"),
        ],
    }
}

/// 事前確認が実際に返した失敗を集める。
///
/// 起こす要求を持たない理由と、別の理由を名乗った失敗をその場で落とす。
fn section_precondition_failures(harness: &Harness) -> Vec<EditError> {
    let mut produced = Vec::new();
    for reason in SectionPreconditionReason::ALL {
        let failures = section_precondition_case(harness, reason);
        assert!(
            !failures.is_empty(),
            "{} を起こす要求がありません",
            reason.as_str()
        );
        for failure in &failures {
            assert_eq!(
                failure.details()["reason"],
                json!(reason.as_str()),
                "{} を起こすはずの要求が別の失敗を返しました",
                reason.as_str()
            );
        }
        produced.extend(failures);
    }
    produced
}

/// 中間点を 3 つ持つ一式を組み、事前確認が実際に返した失敗を集める。
pub(crate) fn produced_section_precondition_failures() -> Vec<EditError> {
    section_precondition_failures(&harness_with_sections())
}

#[test]
fn every_section_precondition_names_its_own_reason() {
    for error in produced_section_precondition_failures() {
        let reason = error.details()["reason"].clone();
        assert_eq!(
            error.error_code(),
            ErrorCode::PreconditionFailed,
            "{reason} が前提条件の不整合になっていません"
        );
        assert_eq!(error.details()["retry_requires"], json!("refetch"));
    }
}

#[test]
fn the_section_precondition_cases_cover_every_reason() {
    // 事前確認が名乗り得る 4 種を、どれか 1 つでも欠けたら落ちる形で固定する。
    let covered: std::collections::BTreeSet<String> = produced_section_precondition_failures()
        .iter()
        .map(|error| error.details()["reason"].to_string())
        .collect();
    let expected: std::collections::BTreeSet<String> = SectionPreconditionReason::ALL
        .iter()
        .map(|reason| json!(reason.as_str()).to_string())
        .collect();
    assert_eq!(covered, expected);
}

#[test]
fn a_failed_section_precondition_leaves_the_project_untouched() {
    let harness = harness_with_sections();
    let failures = section_precondition_failures(&harness);
    assert!(!failures.is_empty());
    harness.assert_untouched();
}

#[test]
fn creating_at_the_end_frame_of_the_object_is_accepted() {
    // 受け付ける範囲は閉区間である。終了フレームちょうどを外すと、最後の
    // 1 フレームだけ中間点を置けない穴ができる。
    let harness = harness_with_sections();
    let outcome = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 200,
        })
        .expect("終了フレームへの追加が拒否されました");
    assert_eq!(outcome.sections.last().expect("区間がある").start, 200);

    let error = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 201,
        })
        .expect_err("終了フレームより後への追加が受理されました");
    assert_eq!(error.details()["reason"], json!("frame_outside_object"));
}

#[test]
fn creating_at_the_start_frame_of_the_object_reports_an_existing_boundary() {
    // 開始フレームは範囲の内側であり、範囲外ではない。既に区間の開始位置で
    // あることが理由であり、要求元が直すべき点が違う。
    let harness = harness_with_sections();
    let error = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 100,
        })
        .expect_err("開始フレームへの追加が受理されました");
    assert_eq!(error.details()["reason"], json!("section_boundary_exists"));

    let error = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 99,
        })
        .expect_err("開始フレームより前への追加が受理されました");
    assert_eq!(error.details()["reason"], json!("frame_outside_object"));
}

#[test]
fn a_section_that_cannot_be_reread_is_reported_as_a_change_that_went_through() {
    // 変更は発行済みである。読み直せなかったことを「適用されなかった」として
    // 返すと、要求元は入った変更を無かったものとして次の要求を組み立てる。
    let harness = harness_with_sections();
    let selector = harness.selector(1, 100);
    harness.host.arm(|knobs| {
        knobs.fault = Some(Fault::SectionsUnreadable);
    });

    let error = harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector,
            frame: 160,
        })
        .expect_err("読み直せないのに成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(error.details()["current_project_revision"], json!(1));
    assert_eq!(error.details()["retry_requires"], json!("refetch"));
    // 事前確認は通っている。変更そのものはホストへ届いた。
    assert!(harness.host.mutated());
    assert_eq!(
        harness.host.section_points(1, 100),
        vec![120, 150, 160, 180]
    );
    assert_eq!(harness.project.revision(), 1);
}

#[test]
fn a_move_that_stops_short_of_the_neighbours_is_accepted() {
    // 事前確認が広すぎないことを確かめる。隣の中間点の直前・直後は通る。
    let harness = harness_with_sections();
    let outcome = harness
        .edit
        .move_object_section(&MoveObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 2,
            frame: 179,
        })
        .expect("隣の中間点を越えない移動が拒否されました");
    assert_eq!(outcome.sections[2].start, 179);
}

#[test]
fn a_move_to_the_end_of_the_object_is_accepted() {
    // 最後の区間の移動先はオブジェクトの終了フレームまで許す。
    let harness = harness_with_sections();
    let outcome = harness
        .edit
        .move_object_section(&MoveObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 3,
            frame: 200,
        })
        .expect("終了フレームへの移動が拒否されました");
    assert_eq!(outcome.sections[3].start, 200);

    let error = harness
        .edit
        .move_object_section(&MoveObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 3,
            frame: 201,
        })
        .expect_err("終了フレームより後への移動が受理されました");
    assert_eq!(
        error.details()["reason"],
        json!("section_move_crosses_boundary")
    );
}

#[test]
fn a_rejected_section_change_that_passed_the_precheck_names_the_sdk_function() {
    // 事前確認を通ったのに false が返る経路。要求元に直せることが無いため、
    // 要求の誤りではなく SDK の失敗として返す。
    let harness = harness_with_sections();
    let selector = harness.selector(1, 100);
    harness.host.arm(|knobs| {
        knobs.fault = Some(Fault::RejectSectionChange);
    });

    for (operation, error) in [
        (
            "create_object_section",
            harness
                .edit
                .create_object_section(&CreateObjectSectionParams {
                    selector: selector.clone(),
                    frame: 160,
                })
                .expect_err("拒否された追加が成功として返りました"),
        ),
        (
            "delete_object_section",
            harness
                .edit
                .delete_object_section(&DeleteObjectSectionParams {
                    selector: selector.clone(),
                    section: 1,
                })
                .expect_err("拒否された削除が成功として返りました"),
        ),
        (
            "move_object_section",
            harness
                .edit
                .move_object_section(&MoveObjectSectionParams {
                    selector: selector.clone(),
                    section: 1,
                    frame: 110,
                })
                .expect_err("拒否された移動が成功として返りました"),
        ),
    ] {
        assert_eq!(error.error_code(), ErrorCode::SdkError, "{operation}");
        assert_eq!(
            error.details()["reason"],
            json!("section_change_rejected"),
            "{operation}"
        );
        assert_eq!(
            error.details()["sdk_operation"],
            json!(operation),
            "{operation}"
        );
    }
}

#[test]
fn the_precheck_reads_the_sections_inside_the_edit_section() {
    // 事前確認は区間の内側で読み直した実態に対して行う。区間の外の複製で
    // 判定する実装では、この記録が変更の前に現れない。
    let harness = harness_with_sections();
    let selector = harness.selector(1, 100);
    harness.host.clear_calls();
    harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector,
            frame: 160,
        })
        .expect("中間点の追加が拒否されました");

    let calls = harness.host.calls();
    let mutation = calls
        .iter()
        .position(|call| *call == "create_object_section")
        .expect("変更 API が呼ばれていません");
    let first_read = calls
        .iter()
        .position(|call| *call == SECTION_RANGES)
        .expect("区間を読み直していません");
    assert!(
        first_read < mutation,
        "事前確認の読み直しが変更より後です: {calls:?}"
    );
    // 読み直しは事前確認と read-back の 2 回だけである。
    assert_eq!(
        calls.iter().filter(|call| **call == SECTION_RANGES).count(),
        2,
        "{calls:?}"
    );
}

/// 中間点を変える SDK の関数名。
///
/// フェイクが記録する名前であり、変更が発行されたかを名前で数えられる。成否だけを
/// 見ると、判定が変更の後に置かれた実装でも通ってしまう。
const SECTION_MUTATIONS: [&str; 3] = [
    "create_object_section",
    "delete_object_section",
    "move_object_section",
];

/// ロックされたレイヤーの対象に対して 3 operation を 1 度ずつ実行する。
fn locked_layer_section_changes(harness: &Harness) -> Vec<(&'static str, EditError)> {
    let selector = || harness.selector(1, 100);
    vec![
        (
            "create_object_section",
            harness
                .edit
                .create_object_section(&CreateObjectSectionParams {
                    selector: selector(),
                    frame: 160,
                })
                .expect_err("ロックされたレイヤーへ中間点を追加できました"),
        ),
        (
            "delete_object_section",
            harness
                .edit
                .delete_object_section(&DeleteObjectSectionParams {
                    selector: selector(),
                    section: 1,
                })
                .expect_err("ロックされたレイヤーの中間点を削除できました"),
        ),
        (
            "move_object_section",
            harness
                .edit
                .move_object_section(&MoveObjectSectionParams {
                    selector: selector(),
                    section: 1,
                    frame: 110,
                })
                .expect_err("ロックされたレイヤーの中間点を移動できました"),
        ),
    ]
}

#[test]
fn every_section_change_is_refused_on_a_locked_layer() {
    let harness = harness_with_sections();
    harness.host.lock_layer(1, true);

    for (operation, error) in locked_layer_section_changes(&harness) {
        assert_eq!(
            error.error_code(),
            ErrorCode::PreconditionFailed,
            "{operation}"
        );
        assert_eq!(
            error.details()["reason"],
            json!("layer_locked"),
            "{operation}"
        );
        assert_eq!(error.details()["layer"], json!(1), "{operation}");
    }

    // 数えるのは変更 API だけである。対象の解決とロック状態の読み取りは判定に
    // 要るため、読み取りが起きないことは求めない。
    let calls = harness.host.calls();
    for mutation in SECTION_MUTATIONS {
        assert!(
            !calls.contains(&mutation),
            "{mutation} が呼ばれました: {calls:?}"
        );
    }
    harness.assert_untouched();
}

#[test]
fn a_locked_layer_is_reported_before_the_section_precheck() {
    // 事前確認にも掛かる要求を送る。ロックの判定が事前確認より後にある実装は、
    // 要求元が直しても解けない理由を名乗り、要求元は往復を繰り返す。
    let harness = harness_with_sections();
    harness.host.lock_layer(1, true);
    let selector = || harness.selector(1, 100);

    let failures = [
        (
            "範囲外のフレームへの追加",
            harness
                .edit
                .create_object_section(&CreateObjectSectionParams {
                    selector: selector(),
                    frame: 400,
                })
                .expect_err("範囲外への追加が受理されました"),
        ),
        (
            "区間数以上の番号での削除",
            harness
                .edit
                .delete_object_section(&DeleteObjectSectionParams {
                    selector: selector(),
                    section: 4,
                })
                .expect_err("区間数以上の番号での削除が受理されました"),
        ),
        (
            "区間数以上の番号での移動",
            harness
                .edit
                .move_object_section(&MoveObjectSectionParams {
                    selector: selector(),
                    section: 4,
                    frame: 190,
                })
                .expect_err("区間数以上の番号での移動が受理されました"),
        ),
    ];

    for (label, error) in failures {
        assert_eq!(error.details()["reason"], json!("layer_locked"), "{label}");
    }
}

#[test]
fn every_section_change_passes_on_an_unlocked_layer() {
    // ガードが広すぎないこと。ロックしていないレイヤーでは 3 つとも通る。
    let harness = harness_with_sections();
    harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector: harness.selector(1, 100),
            frame: 160,
        })
        .expect("中間点の追加が拒否されました");
    harness
        .edit
        .move_object_section(&MoveObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 1,
            frame: 110,
        })
        .expect("中間点の移動が拒否されました");
    harness
        .edit
        .delete_object_section(&DeleteObjectSectionParams {
            selector: harness.selector(1, 100),
            section: 1,
        })
        .expect("中間点の削除が拒否されました");
}

#[test]
fn section_changes_do_not_read_the_effect_list() {
    // 応答は effect を含まない。読めば、無関係な読み取り失敗が反映済みの変更を
    // 失敗として報告させる。
    let harness = harness_with_sections();
    let selector = harness.selector(1, 100);
    harness.host.clear_calls();
    harness
        .edit
        .create_object_section(&CreateObjectSectionParams {
            selector,
            frame: 160,
        })
        .expect("中間点の追加が拒否されました");

    assert!(
        !harness.host.calls().contains(&EFFECT_LIST),
        "配下 effect を読んでいます: {:?}",
        harness.host.calls()
    );
}

#[test]
fn the_fake_names_the_call_that_could_not_produce_a_value() {
    // `sdk_operation` は失敗の出所を伝える値である。種別が違えば呼ばれる関数も
    // 違うのだから、名乗る関数も違う。フェイクが片方の名前で固定していると、
    // 出所の取り違えに気付ける経路がどこにも無くなる。
    use crate::read::host::{ReadHost, SceneValueReader};

    let harness = Harness::new();
    let object = harness.summary(1, 100);
    let host = FakeReadHost(harness.host.clone());

    let named = |missing_as_check: bool| {
        host.enter_read_section(move |scene: &dyn SceneValueReader| {
            let error = if missing_as_check {
                scene
                    .effect_check_values(object.layer, object.frame_start, 0, &["無い項目"], &[0])
                    .expect_err("存在しない項目で値が返りました")
            } else {
                scene
                    .effect_track_values(object.layer, object.frame_start, 0, &["無い項目"], &[0.0])
                    .expect_err("存在しない項目で値が返りました")
            };
            error.details()["sdk_operation"].clone()
        })
        .expect("参照区間へ入れます")
    };

    assert_eq!(named(false), json!("get_effect_track_value"));
    assert_eq!(named(true), json!("get_effect_check_value"));
}

/// 選択の取得がハンドルを参照区間の外へ出さないことを確かめる。
///
/// 選択はハンドルを 2 段で受け取る唯一の読み取りである。3 件を選択したフェイクで、
/// 応答が位置と同一性の材料だけで組み立てられることと、対象を指す内部の値が
/// 現れないことを見る。
#[test]
fn the_selection_of_three_objects_carries_no_handle() {
    let harness = Harness::new();
    // ホストが返す順序は規定されていない。昇順とは逆に並べて渡す。ホストが既に
    // 昇順で返していれば、並べ替えを外した実装でも同じ結果になる。
    let armed = [(1, 300), (1, 100), (0, 0)];
    let mut ascending = armed;
    ascending.sort();
    assert_ne!(armed, ascending, "フェイクが昇順で返しています");
    harness.host.select_objects(&armed);
    harness.host.focus_object(Some((1, 100)), Some(1));

    let snapshot = harness
        .read
        .get_selection(SCENE_ID, &default_page_request())
        .expect("選択を取得できます")
        .expect("ページ要求が拒否されました");

    // 列挙が返す概要とそのまま一致する。fingerprint まで同じであるため、
    // 要求元は返ってきた対象をそのまま編集へ渡せる。
    assert_eq!(
        snapshot.selected,
        vec![
            harness.summary(0, 0),
            harness.summary(1, 100),
            harness.summary(1, 300),
        ]
    );
    assert_eq!(snapshot.focus, Some(harness.summary(1, 100)));
    assert_eq!(snapshot.focus_section, Some(1));

    let payload = serde_json::to_string(&snapshot).expect("直列化できます");
    let lowered = payload.to_lowercase();
    for forbidden in ["handle", "pointer", "0x", "alias"] {
        assert!(
            !lowered.contains(forbidden),
            "{forbidden} が応答に現れました: {payload}"
        );
    }
}

/// BPM グリッドの置き換え要求を組み立てる。
fn set_grid_bpm(harness: &Harness, entries: Vec<GridBpm>) -> SetGridBpmParams {
    SetGridBpmParams {
        expected_scene_id: SCENE_ID,
        entries,
        expected_project_epoch: harness.epoch(),
    }
}

#[test]
fn replacing_the_grid_bpm_returns_the_list_read_back() {
    let harness = Harness::new();
    let entries = vec![grid_bpm(140.0, 3, 0.0, 0.25), grid_bpm(90.0, 4, 12.5, 0.0)];
    let outcome = harness
        .edit
        .set_grid_bpm(&set_grid_bpm(&harness, entries.clone()))
        .expect("BPM グリッドの置き換えに失敗しました");

    assert_eq!(outcome.entries, entries);
    assert_eq!(outcome.project_epoch, harness.epoch());
    assert_eq!(outcome.project_revision, 1);
    assert!(harness.project.modified());
}

#[test]
fn an_empty_grid_bpm_list_clears_the_grid() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_grid_bpm(&set_grid_bpm(&harness, Vec::new()))
        .expect("0 件の一覧が拒否されました");
    assert!(outcome.entries.is_empty());
}

#[test]
fn a_descending_grid_bpm_list_is_accepted_by_the_edit_path() {
    // 並べ替えはホストの仕事である。編集口が順序を要求すると、要求元は
    // read-back の順序と要求の順序の食い違いを説明できなくなる。
    let harness = Harness::new();
    let entries = vec![
        grid_bpm(120.0, 4, 30.0, 0.0),
        grid_bpm(120.0, 4, 20.0, 0.0),
        grid_bpm(120.0, 4, 10.0, 0.0),
    ];
    let outcome = harness
        .edit
        .set_grid_bpm(&set_grid_bpm(&harness, entries.clone()))
        .expect("降順の一覧が拒否されました");
    assert_eq!(outcome.entries, entries);
}

#[test]
fn a_grid_bpm_list_at_the_limit_reaches_the_host() {
    let harness = Harness::new();
    let entries = (0..MAX_GRID_BPM_ENTRIES)
        .map(|index| grid_bpm(120.0, 4, index as f64, 0.0))
        .collect::<Vec<_>>();
    let outcome = harness
        .edit
        .set_grid_bpm(&set_grid_bpm(&harness, entries))
        .expect("上限ちょうどの一覧が拒否されました");
    assert_eq!(outcome.entries.len(), MAX_GRID_BPM_ENTRIES);
}

#[test]
fn a_silently_ignored_grid_bpm_replacement_is_not_reported_as_success() {
    // 置き換えの API は戻り値を持たない。件数の照合だけが「送ったのに入って
    // いない」を捕まえる。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreGridBpm)));
    let error = harness
        .edit
        .set_grid_bpm(&set_grid_bpm(
            &harness,
            vec![grid_bpm(140.0, 3, 0.0, 0.0), grid_bpm(90.0, 4, 12.5, 0.0)],
        ))
        .expect_err("無視された置き換えが成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("change_not_applied"));
}

#[test]
fn a_host_that_rewrites_the_grid_bpm_values_is_not_a_failure() {
    // ホストは単精度で受け取り、並べ替えもする。値を照合する実装に戻すと、
    // 正常な正規化を失敗として報告するようになる。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::RewriteGridBpmValues)));
    let entries = vec![grid_bpm(140.0, 3, 0.0, 0.25), grid_bpm(90.0, 4, 12.5, 0.0)];
    let outcome = harness
        .edit
        .set_grid_bpm(&set_grid_bpm(&harness, entries.clone()))
        .expect("値の違いが失敗として返りました");

    assert_eq!(outcome.entries.len(), entries.len());
    assert_ne!(outcome.entries, entries, "フェイクが値を変えていません");
}

#[test]
fn the_grid_bpm_response_carries_no_handle_or_alias() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_grid_bpm(&set_grid_bpm(&harness, vec![grid_bpm(140.0, 3, 0.0, 0.25)]))
        .expect("BPM グリッドの置き換えに失敗しました");
    let value = serde_json::to_string(&outcome).expect("直列化できる");
    for secret in ["alias", "handle", "[1:100]"] {
        assert!(!value.contains(secret), "{secret} が応答に現れました");
    }
}

/// シーン設定の変更要求を組み立てる。
///
/// 3 つの軸はいずれも省略した状態から始める。軸ごとの検査は必要なものだけを
/// 立てて行う——全省略は要求の検証が弾くため、ここへは届かない。
fn set_scene_settings(harness: &Harness) -> SetSceneSettingsParams {
    SetSceneSettingsParams {
        expected_scene_id: SCENE_ID,
        name: None,
        size: None,
        sample_rate: None,
        expected_project_epoch: harness.epoch(),
    }
}

/// シーン設定の変更要求を 1 つ組み立てる手続き。
type SceneRequest = fn(&Harness) -> SetSceneSettingsParams;

/// シーン設定の 3 つの setter が呼ばれた回数を、名前・解像度・サンプリング
/// レートの順で数える。
///
/// **成否ではなく回数を数える。** 成否だけを見ると、名前が反映されなかった
/// ときに残る 2 つを発行してしまう実装でも通ってしまう。
fn scene_setter_calls(harness: &Harness) -> [usize; 3] {
    let calls = harness.host.calls();
    ["set_scene_name", "set_scene_size", "set_scene_sample_rate"]
        .map(|setter| calls.iter().filter(|call| **call == setter).count())
}

#[test]
fn changing_every_scene_axis_reports_a_change_that_cannot_be_undone() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_scene_settings(&SetSceneSettingsParams {
            name: Some("本編".to_string()),
            size: Some(SceneSize {
                width: 1280,
                height: 720,
            }),
            sample_rate: Some(44_100),
            ..set_scene_settings(&harness)
        })
        .expect("シーン設定の変更に失敗しました");

    assert_eq!(outcome.scene.id, SCENE_ID);
    assert_eq!(outcome.scene.name.as_deref(), Some("本編"));
    assert_eq!(outcome.scene.width, 1280);
    assert_eq!(outcome.scene.height, 720);
    assert_eq!(outcome.scene.sample_rate, 44_100);
    // 解像度とサンプリングレートは区間を抜けてから観測する。
    assert!(outcome.observed_after_edit);
    // AviUtl2 の取り消し操作ではシーン設定は元へ戻らない。
    assert!(outcome.non_undoable);
    assert_eq!(outcome.project_epoch, harness.epoch());
    assert_eq!(outcome.project_revision, 1);
    assert_eq!(harness.project.revision(), 1);
    assert!(harness.project.modified());
    assert_eq!(scene_setter_calls(&harness), [1, 1, 1]);
}

#[test]
fn each_scene_axis_can_be_set_on_its_own() {
    // 軸ごとに、その軸の setter だけが呼ばれる。まとめて発行する実装では、
    // 要求していない軸が現在値で上書きされる。
    let cases: [(&str, SceneRequest, [usize; 3]); 3] = [
        (
            "name",
            |harness| SetSceneSettingsParams {
                name: Some("本編".to_string()),
                ..set_scene_settings(harness)
            },
            [1, 0, 0],
        ),
        (
            "size",
            |harness| SetSceneSettingsParams {
                size: Some(SceneSize {
                    width: 1280,
                    height: 720,
                }),
                ..set_scene_settings(harness)
            },
            [0, 1, 0],
        ),
        (
            "sample_rate",
            |harness| SetSceneSettingsParams {
                sample_rate: Some(44_100),
                ..set_scene_settings(harness)
            },
            [0, 0, 1],
        ),
    ];

    for (label, build, expected) in cases {
        let harness = Harness::new();
        let outcome = harness
            .edit
            .set_scene_settings(&build(&harness))
            .unwrap_or_else(|error| panic!("{label} だけの変更が失敗しました: {error}"));

        assert_eq!(scene_setter_calls(&harness), expected, "{label}");
        assert_eq!(outcome.project_revision, 1, "{label}");
    }
}

#[test]
fn a_scene_name_that_did_not_take_effect_stops_before_the_other_axes() {
    // 名前の照合は区間の内側で完結する。反映されていなければ、取り消せない
    // 変更を 1 つも増やさずに戻る。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreSceneName)));
    let error = harness
        .edit
        .set_scene_settings(&SetSceneSettingsParams {
            name: Some("本編".to_string()),
            size: Some(SceneSize {
                width: 1280,
                height: 720,
            }),
            sample_rate: Some(44_100),
            ..set_scene_settings(&harness)
        })
        .expect_err("反映されていない名前が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("change_not_applied"));
    // 名前の setter は SDK へ届いている。届いた以上は変更が入った側へ倒す。
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(
        scene_setter_calls(&harness),
        [1, 0, 0],
        "名前が反映されないまま残りの軸を発行しました"
    );
    // シーンは 3 軸とも元のままである。
    let scene = harness.host.scene();
    assert_eq!(scene.name, SCENE_NAME);
    assert_eq!(scene.width, 1920);
    assert_eq!(scene.height, 1080);
    assert_eq!(scene.sample_rate, 48_000);
}

#[test]
fn a_host_that_adjusts_the_scene_settings_is_not_a_failure() {
    // 反映値は区間を抜けてから観測する。ホストが調整し得るうえ、観測までの間に
    // UI 操作も入り得る。差異を失敗にすると、成功した変更が失敗として返る。
    //
    // **解像度とサンプリングレートの両方で確かめる。** 片方だけを見ると、もう
    // 一方が要求値をそのまま返していても通ってしまう。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::ClampSceneSettings)));
    let requested = SceneSize {
        width: 3840,
        height: 2160,
    };
    let requested_sample_rate = 192_000;
    let outcome = harness
        .edit
        .set_scene_settings(&SetSceneSettingsParams {
            size: Some(requested),
            sample_rate: Some(requested_sample_rate),
            ..set_scene_settings(&harness)
        })
        .expect("要求値との差異が失敗として返りました");

    // 応答が載せるのは観測値である。
    assert_eq!(outcome.scene.width, MAX_SCENE_WIDTH);
    assert_eq!(outcome.scene.height, MAX_SCENE_HEIGHT);
    assert_eq!(outcome.scene.sample_rate, MAX_SCENE_SAMPLE_RATE);
    assert_ne!(
        outcome.scene.width, requested.width,
        "フェイクが解像度を調整していません"
    );
    assert_ne!(
        outcome.scene.height, requested.height,
        "フェイクが解像度を調整していません"
    );
    assert_ne!(
        outcome.scene.sample_rate, requested_sample_rate,
        "フェイクがサンプリングレートを調整していません"
    );
    assert!(outcome.observed_after_edit);
    assert_eq!(outcome.project_revision, 1);
}

#[test]
fn a_scene_renamed_after_the_section_is_reported_as_observed() {
    // 名前の照合は区間の内側で通る。そのうえで、区間を抜けてから観測するまでの
    // 間に UI が名前を付け直す状況を作る。差異は失敗ではなく、応答が載せるのは
    // 観測した名前である——要求値をそのまま返す実装ではここが食い違う。
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::RenameSceneAfterSection)));
    let requested = "本編";
    let outcome = harness
        .edit
        .set_scene_settings(&SetSceneSettingsParams {
            name: Some(requested.to_string()),
            ..set_scene_settings(&harness)
        })
        .expect("観測との差異が失敗として返りました");

    assert_eq!(outcome.scene.name.as_deref(), Some(RENAMED_SCENE_NAME));
    assert_ne!(
        outcome.scene.name.as_deref(),
        Some(requested),
        "応答が要求値をそのまま返しました"
    );
    assert!(outcome.observed_after_edit);
    assert_eq!(outcome.project_revision, 1);
    // 観測は区間を抜けた後にある。区間の内側で応答を組み立てていれば、記録の
    // 最後は setter になる。
    assert_eq!(
        harness.host.calls().last(),
        Some(&OBSERVED_SCENE),
        "シーンの観測が区間を抜けた後に行われていません: {:?}",
        harness.host.calls()
    );
}

#[test]
fn a_mismatched_scene_precondition_never_reaches_the_scene_setters() {
    let cases: [(&str, SceneRequest, &str); 2] = [
        (
            "expected_project_epoch",
            |harness| SetSceneSettingsParams {
                expected_project_epoch: "00000000-0000-4000-8000-000000000000".to_string(),
                name: Some("本編".to_string()),
                ..set_scene_settings(harness)
            },
            "project_epoch",
        ),
        (
            "expected_scene_id",
            |harness| SetSceneSettingsParams {
                expected_scene_id: SCENE_ID + 1,
                name: Some("本編".to_string()),
                ..set_scene_settings(harness)
            },
            "scene_id",
        ),
    ];

    for (label, build, mismatch) in cases {
        let harness = Harness::new();
        let error = harness
            .edit
            .set_scene_settings(&build(&harness))
            .err()
            .unwrap_or_else(|| panic!("{label} の不一致が受理されました"));

        assert_eq!(error.error_code(), ErrorCode::PreconditionFailed, "{label}");
        assert_eq!(error.details()["mismatch"], json!(mismatch), "{label}");
        // 取り消せない変更であるため、前提が崩れていれば 1 つも発行しない。
        assert_eq!(scene_setter_calls(&harness), [0, 0, 0], "{label}");
        harness.assert_untouched();
    }
}

#[test]
fn the_scene_settings_response_carries_no_alias_path_or_item_value() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_scene_settings(&SetSceneSettingsParams {
            name: Some("本編".to_string()),
            ..set_scene_settings(&harness)
        })
        .expect("シーン設定の変更に失敗しました");
    let value = serde_json::to_string(&outcome).expect("直列化できる");
    for secret in ["alias", "handle", "[1:100]", "0x", "C:\\"] {
        assert!(!value.contains(secret), "{secret} が応答に現れました");
    }
}

/// 理由を実際に起こす要求を、編集手順へ通した結果として並べる。
///
/// [`UnsupportedReason`] に対する網羅 `match` であり `_` を使わない。**理由を
/// 足すとここが落ち、それを起こす要求を書くまでコンパイルできない。** 理由を
/// 数え上げるのは [`UnsupportedReason::ALL`] の役目であり、編集手順が実際に
/// その理由で落とすことの証明は要求の側が持つ。
fn unsupported_target_case(reason: &UnsupportedReason) -> Vec<EditError> {
    match reason {
        UnsupportedReason::EffectNotRegistered => {
            let harness = Harness::new();
            vec![
                harness
                    .edit
                    .create_object(&create_from_effect(
                        &harness,
                        "存在しないエフェクト",
                        1,
                        600,
                    ))
                    .expect_err("未登録の effect 名から作成できました"),
            ]
        }
        UnsupportedReason::EffectNotCreatable => {
            let harness = Harness::with(|host| {
                host.arm(|knobs| knobs.fault = Some(Fault::RejectObjectCreation))
            });
            vec![
                harness
                    .edit
                    .create_object(&create_from_effect(&harness, "ぼかし", 1, 600))
                    .expect_err("拒否された作成が成功として返りました"),
            ]
        }
        UnsupportedReason::EffectStateImmutable => {
            let harness = Harness::with(|host| {
                host.arm(|knobs| knobs.fault = Some(Fault::IgnoreEffectState))
            });
            vec![
                harness
                    .edit
                    .set_effect_enabled(&SetEffectEnabledParams {
                        selector: harness.effect_selector(1, 100, "ぼかし", 0),
                        enabled: false,
                    })
                    .expect_err("無言で無視された変更が成功として返りました"),
            ]
        }
        UnsupportedReason::MediaNotSupported => {
            let harness = Harness::new();
            vec![
                harness
                    .edit
                    .create_object(&CreateObjectParams {
                        source: ObjectSource::MediaFile {
                            path: r"C:\media\clip.xyz".to_string(),
                        },
                        placement: Placement {
                            scene_id: SCENE_ID,
                            layer: 1,
                            frame: 600,
                        },
                        expected_project_epoch: harness.epoch(),
                    })
                    .expect_err("対応しないメディアから作成できました"),
            ]
        }
        UnsupportedReason::ItemTypeNotWritable => {
            let harness = harness_with_unlisted_item();
            vec![
                harness
                    .edit
                    .set_object_item(&SetObjectItemParams {
                        selector: harness.effect_selector(1, 100, "ぼかし", 0),
                        item: "未知種別の項目".to_string(),
                        value: ItemValue::Integer { value: 1 },
                    })
                    .expect_err("未知種別の項目へ書き込めました"),
            ]
        }
        UnsupportedReason::ChangeNotApplied => {
            let harness =
                Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreObjectName)));
            vec![
                harness
                    .edit
                    .set_object_name(&SetObjectNameParams {
                        selector: harness.selector(1, 100),
                        name: Some("新しい名前".to_string()),
                    })
                    .expect_err("無言で無視された改名が成功として返りました"),
            ]
        }
        UnsupportedReason::InverseUnavailable => {
            // 逆操作を組み立てられない sub-operation は、一括適用の事前解決相で
            // 落ちる。単独の operation にはこの相が無い。
            let harness = Harness::with(|host| {
                host.arm(|knobs| knobs.fault = Some(Fault::ItemValueUnreadable))
            });
            let params = ApplyBatchParams {
                operations: vec![
                    BatchOperation::MoveObject {
                        selector: harness.selector(0, 0),
                        destination: Destination {
                            layer: 1,
                            frame: 500,
                        },
                    },
                    BatchOperation::SetObjectItem {
                        selector: harness.effect_selector(1, 100, "ぼかし", 0),
                        item: "範囲".to_string(),
                        value: ItemValue::Integer { value: 40 },
                    },
                ],
            };
            vec![
                harness
                    .edit
                    .apply_batch(&params)
                    .expect_err("逆操作を組み立てられない要求が受理されました"),
            ]
        }
    }
}

/// 編集手順が実際に返した「対象が要求を受け付けない」失敗を集める。
///
/// 起こす要求を持たない理由と、別の理由を名乗った失敗をその場で落とす。
pub(crate) fn unsupported_target_failures() -> Vec<EditError> {
    let mut produced = Vec::new();
    for reason in UnsupportedReason::ALL {
        let failures = unsupported_target_case(reason);
        assert!(
            !failures.is_empty(),
            "{} を起こす要求がありません",
            reason.as_str()
        );
        for failure in &failures {
            // 発行後の失敗は覆いに包まれて返る。名乗る名前は覆いを通しても
            // 変わらないため、突き合わせは応答へ載る名前で行う。
            assert_eq!(
                failure.details()["reason"],
                json!(reason.as_str()),
                "{} を起こすはずの要求が別の失敗を返しました",
                reason.as_str()
            );
        }
        produced.extend(failures);
    }
    produced
}

#[test]
fn every_unsupported_reason_has_a_request_that_produces_it() {
    // 要求を書かないまま理由を足すと、応答に現れない名前が一覧へ残る。
    unsupported_target_failures();
}

/// 一括適用の統合テスト。
mod apply_batch;
