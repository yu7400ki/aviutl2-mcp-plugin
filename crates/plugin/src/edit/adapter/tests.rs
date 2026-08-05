//! 編集手順の統合テスト。
//!
//! フェイクは [`EditHost`] / [`SceneEditor`] の位置に差し込むため、検証の対象は
//! adapter の本番実装そのものになる。フェイクは呼び出しを順序ごと記録するので、
//! 順序自体を検証できる。

use super::*;
use crate::edit::fake::{
    CLOSURE_ESCAPED, CREATE_FRAME_SHIFT, EFFECT_LIST, FakeEditHost, FakeLayer, FakeObject,
    FakeReadHost, Fault, ITEM_VALUE, Knobs, LAYER_ATTRIBUTES, LAYER_LOCK, LAYER_MAX, MAX_FRAME,
    MAX_ITEM_VALUE, MAX_LAYER, MOVE_FRAME_SHIFT, MUTATIONS, PanicPoint, READ_SECTION, SCENE_ID,
    SECTION_RANGES,
};
use crate::read::{HostReadAdapter, ReadAdapter};
use crate::test_support::with_silent_panic_hook;
use aviutl2_mcp_core::{
    AvailableEffect, CreateObjectSectionParams, CursorPosition, DeleteObjectSectionParams,
    Destination, EditOperation, EffectFlags, EffectItem, EffectItemType, EffectSelector,
    EffectType, ErrorCode, Fingerprint, FiniteF64, GridBpm, ItemValue, LayerNameChange,
    MAX_GRID_BPM_ENTRIES, MoveObjectSectionParams, ObjectSectionsOutcome, ObjectSelector,
    PageRequest, Placement,
};
use serde_json::json;
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
                    .list_objects(SCENE_ID, None, &PageRequest::default())
            })
            .expect("列挙に失敗しました")
            .expect("ページ要求が拒否されました");
        page.items
            .into_iter()
            .find(|item| item.layer == layer && item.frame_start == frame)
            .unwrap_or_else(|| panic!("レイヤー {layer} フレーム {frame} の対象がありません"))
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
        host.catalog.push(AvailableEffect {
            name: r"..\図形:1".to_string(),
            effect_type: EffectType::Filter,
            flags: EffectFlags::from_raw(1),
            items: Vec::new(),
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
        before_effect.fingerprint, after_effect.fingerprint,
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
fn a_normalized_item_value_is_reported_instead_of_being_treated_as_a_failure() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            item: "範囲".to_string(),
            value: ItemValue::Integer {
                value: MAX_ITEM_VALUE + 150,
            },
        })
        .expect("正規化された値が失敗として扱われました");

    let effect = outcome.effect.expect("変更後の effect");
    let item = effect
        .items
        .iter()
        .find(|item| item.name == "範囲")
        .expect("設定項目");
    assert_eq!(
        item.value,
        ItemValue::Integer {
            value: MAX_ITEM_VALUE
        },
        "応答が書いた値をそのまま返しています"
    );
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

#[test]
fn a_successful_item_write_never_probes_the_value() {
    // 追加の読み取りは失敗経路でだけ行う。成功する要求の費用は変わらない。
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

    assert!(
        !harness.host.calls().contains(&ITEM_VALUE),
        "成功経路で項目の値を読み直しました: {:?}",
        harness.host.calls()
    );
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
    // 反映値は編集と原子的に観測されたものではない。
    assert!(state.observed_after_edit);
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
    harness.host.clear_calls();
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
/// 時間軸上の移動——に限る。設定値の変更も effect の増減も UI の設定パネルから
/// 行えるため、MCP からだけ拒む理由が無い。選択状態の変更は対象を書き換えない
/// ため表に載らない。実行できない operation も同じく載らない。
fn locked_layer(operation: EditOperation) -> Option<LockedLayer> {
    Some(match operation {
        EditOperation::CreateObject | EditOperation::MoveObject | EditOperation::DeleteObject => {
            LockedLayer::Refused
        }
        EditOperation::SetObjectName
        | EditOperation::SetObjectItem
        | EditOperation::AddEffect
        | EditOperation::DeleteEffect
        | EditOperation::SetEffectEnabled
        // ロックを外す手段そのものをロックで止めると、行き止まりが解けなくなる。
        | EditOperation::SetLayerState
        // 中間点はオブジェクトの位置も長さも変えない。動くのはオブジェクトの
        // 内側の分割位置だけであり、ロックが止める 2 つのいずれでもない。
        | EditOperation::CreateObjectSection
        | EditOperation::DeleteObjectSection
        | EditOperation::MoveObjectSection
        // BPM グリッドはシーンに属し、どのレイヤーの対象にも触れない。
        | EditOperation::SetGridBpm => LockedLayer::Allowed,
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
    assert_eq!(
        outcome.object.fingerprint,
        outcome.object.selector.fingerprint
    );
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

/// 事前確認で落ちる要求と、そこで名乗るべき理由。
fn section_precondition_failures(harness: &Harness) -> Vec<(&'static str, EditError)> {
    let selector = || harness.selector(1, 100);
    vec![
        (
            "frame_outside_object",
            harness
                .edit
                .create_object_section(&CreateObjectSectionParams {
                    selector: selector(),
                    frame: 400,
                })
                .expect_err("オブジェクトの範囲外への追加が受理されました"),
        ),
        (
            "section_boundary_exists",
            harness
                .edit
                .create_object_section(&CreateObjectSectionParams {
                    selector: selector(),
                    frame: 150,
                })
                .expect_err("既にある境界への追加が受理されました"),
        ),
        (
            "section_index_out_of_range",
            harness
                .edit
                .delete_object_section(&DeleteObjectSectionParams {
                    selector: selector(),
                    section: 4,
                })
                .expect_err("区間数以上の番号での削除が受理されました"),
        ),
        // 区間数との比較は削除と移動の双方に掛かる。移動だけが素通りすると、
        // 番号が範囲外の要求が事前確認を抜けて SDK へ届く。
        (
            "section_index_out_of_range",
            harness
                .edit
                .move_object_section(&MoveObjectSectionParams {
                    selector: selector(),
                    section: 4,
                    frame: 190,
                })
                .expect_err("区間数以上の番号での移動が受理されました"),
        ),
        (
            "section_move_crosses_boundary",
            harness
                .edit
                .move_object_section(&MoveObjectSectionParams {
                    selector: selector(),
                    section: 1,
                    frame: 150,
                })
                .expect_err("後ろの中間点を越える移動が受理されました"),
        ),
        // 下限は 1 つ前の区間の開始フレーム「以下」を拒否する。等号を含めないと、
        // 中間点をひとつ前の境界そのものへ重ねられる。
        (
            "section_move_crosses_boundary",
            harness
                .edit
                .move_object_section(&MoveObjectSectionParams {
                    selector: selector(),
                    section: 1,
                    frame: 100,
                })
                .expect_err("ひとつ前の区間の開始フレームへの移動が受理されました"),
        ),
    ]
}

#[test]
fn every_section_precondition_names_its_own_reason() {
    for (reason, error) in section_precondition_failures(&harness_with_sections()) {
        assert_eq!(
            error.error_code(),
            ErrorCode::PreconditionFailed,
            "{reason} が前提条件の不整合になっていません"
        );
        assert_eq!(error.details()["reason"], json!(reason));
        assert_eq!(error.details()["retry_requires"], json!("refetch"));
    }
}

#[test]
fn the_section_precondition_cases_cover_every_reason() {
    // 事前確認が名乗り得る 4 種を、どれか 1 つでも欠けたら落ちる形で固定する。
    let harness = harness_with_sections();
    let covered: std::collections::BTreeSet<&str> = section_precondition_failures(&harness)
        .into_iter()
        .map(|(reason, _)| reason)
        .collect();
    let expected: std::collections::BTreeSet<&str> = SectionPreconditionReason::ALL
        .iter()
        .map(|reason| reason.as_str())
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
    use crate::read::host::{ReadHost, SceneReader};

    let harness = Harness::new();
    let object = harness.summary(1, 100);
    let host = FakeReadHost(harness.host.clone());

    let named = |missing_as_check: bool| {
        host.enter_read_section(move |scene: &dyn SceneReader| {
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
        .get_selection(SCENE_ID, &PageRequest::default())
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

/// 一括適用の統合テスト。
mod apply_batch;
