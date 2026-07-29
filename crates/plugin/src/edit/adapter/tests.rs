//! 編集手順の統合テスト。
//!
//! フェイクは [`EditHost`] / [`SceneEditor`] の位置に差し込むため、検証の対象は
//! adapter の本番実装そのものになる。フェイクは呼び出しを順序ごと記録するので、
//! 順序自体を検証できる。

use super::*;
use crate::edit::fake::{
    CLOSURE_ESCAPED, CREATE_FRAME_SHIFT, FakeEditHost, FakeLayer, FakeObject, FakeReadHost, Fault,
    Knobs, MAX_FRAME, MAX_ITEM_VALUE, MAX_LAYER, MUTATIONS, PanicPoint, SCENE_ID,
};
use crate::read::{HostReadAdapter, ReadAdapter};
use crate::test_support::with_silent_panic_hook;
use aviutl2_mcp_core::{
    CursorPosition, Destination, EffectSelector, ErrorCode, Expected, Fingerprint, ItemValue,
    ObjectSelector, PageRequest, Placement,
};
use serde_json::json;
use std::sync::mpsc::channel;
use std::time::Duration;

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

    /// 現在のプロジェクト世代を前提として組み立てる。
    fn expected(&self) -> Expected {
        Expected {
            project_epoch: self.project.epoch(),
            project_revision: self.project.revision(),
        }
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
        expected: harness.expected(),
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
fn the_expected_epoch_is_checked_first() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    // 前提・セレクター・シーン・fingerprint の全てを壊しても、最初の段で落ちる。
    params.expected.project_epoch = "別のプロジェクト".to_string();
    params.selector.project_epoch = "さらに別のプロジェクト".to_string();
    params.selector.scene_id = 9;
    params.selector.fingerprint = tamper(&params.selector.fingerprint);

    let error = harness.edit.move_object(&params).expect_err("epoch 不一致");
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));
    harness.assert_untouched();
}

#[test]
fn the_selector_epoch_is_checked_before_the_revision() {
    let harness = Harness::new();
    harness.project.on_object_updated();
    let mut params = move_params(&harness);
    params.expected.project_revision = 0;
    params.selector.project_epoch = "別のプロジェクト".to_string();

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("セレクターの epoch 不一致");
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));
    assert!(!harness.host.mutated());
}

#[test]
fn a_revision_mismatch_is_rejected_even_when_the_fingerprint_matches() {
    let harness = Harness::new();
    let params = move_params(&harness);
    // 対象は変えずに revision だけを進める。fingerprint は一致したままである。
    harness.project.on_object_updated();

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("revision 不一致が受理されました");
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["mismatch"], json!("project_revision"));
    assert_eq!(error.details()["current_project_revision"], json!(1));
    assert!(!harness.host.mutated());
}

#[test]
fn a_scene_guard_mismatch_is_checked_before_the_algorithm() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.scene_id = 9;
    params.selector.fingerprint_algorithm =
        aviutl2_mcp_core::FingerprintAlgorithm::Unknown("sha256-future-v9".to_string());

    let error = harness.edit.move_object(&params).expect_err("シーン不一致");
    assert_eq!(error.details()["mismatch"], json!("scene_id"));
    assert_eq!(error.details()["expected_scene_id"], json!(9));
    harness.assert_untouched();
}

#[test]
fn an_unknown_fingerprint_algorithm_is_checked_before_the_resolution() {
    let harness = Harness::new();
    let mut params = move_params(&harness);
    params.selector.fingerprint_algorithm =
        aviutl2_mcp_core::FingerprintAlgorithm::Unknown("sha256-future-v9".to_string());
    // 解決できない座標を併せて指定しても、方式の段で落ちる。
    params.selector.frame = 9_999;

    let error = harness.edit.move_object(&params).expect_err("方式不一致");
    assert_eq!(error.details()["mismatch"], json!("fingerprint_algorithm"));
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
        expected: harness.expected(),
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

#[test]
fn the_boundary_is_revalidated_just_before_the_mutation() {
    // 対象の解決と fingerprint の再計算の間に revision が進む状況を作る。
    let harness = Harness::with(|host| host.arm(|knobs| knobs.bump_on_detail = 1));
    let params = move_params(&harness);

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("解決中の変化が見過ごされました");
    assert_eq!(error.details()["mismatch"], json!("project_revision"));
    assert!(
        !harness.host.mutated(),
        "再検証を通らずに変更 API が呼ばれました"
    );
    assert!(
        error.details().get("mutation_issued").is_none(),
        "何も変更していないのに変更発行として報告されました"
    );
}

#[test]
fn a_project_boundary_change_during_the_resolution_stops_the_mutation() {
    let harness = Harness::with(|host| host.arm(|knobs| knobs.renew_on_detail = true));
    let params = move_params(&harness);

    let error = harness
        .edit
        .move_object(&params)
        .expect_err("プロジェクトが入れ替わったのに変更されました");
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
            expected: harness.expected(),
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
            expected: harness.expected(),
        })
        .expect_err("オブジェクトの fingerprint 改竄が通りました");
    assert_eq!(error.details()["mismatch"], json!("fingerprint"));

    let mut effect_tampered = selector;
    effect_tampered.fingerprint = tamper(&effect_tampered.fingerprint);
    let error = harness
        .edit
        .delete_effect(&DeleteEffectParams {
            selector: effect_tampered,
            expected: harness.expected(),
        })
        .expect_err("effect の fingerprint 改竄が通りました");
    assert_eq!(error.details()["mismatch"], json!("fingerprint"));
    harness.assert_untouched();
}

#[test]
fn a_missing_effect_is_not_found() {
    let harness = Harness::new();
    let mut selector = harness.effect_selector(1, 100, "ぼかし", 0);
    selector.effect_index = 5;

    let error = harness
        .edit
        .delete_effect(&DeleteEffectParams {
            selector,
            expected: harness.expected(),
        })
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
            expected: harness.expected(),
        })
        .expect_err("ロックされたレイヤーの対象が削除されました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    assert_eq!(error.details()["reason"], json!("layer_locked"));
    assert_eq!(error.details()["layer"], json!(2));
    assert_eq!(error.details()["retry_requires"], json!("refetch"));
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
            expected: harness.expected(),
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
            expected: harness.expected(),
        })
        .expect_err("対応しないメディアから作成できました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("media_not_supported"));
    harness.assert_untouched();
}

#[test]
fn an_unregistered_effect_name_is_rejected_without_entering_the_section() {
    let harness = Harness::new();
    let error = harness
        .edit
        .add_effect(&AddEffectParams {
            object: harness.selector(1, 100),
            effect_name: "存在しないエフェクト".to_string(),
            expected: harness.expected(),
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
            expected: harness.expected(),
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
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::CreatePair)));
    let outcome = harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "[obj][obj]".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 600,
            },
            expected: harness.expected(),
        })
        .expect("作成に失敗しました");

    assert_eq!(
        outcome.created.len(),
        2,
        "2 件目以降が要求元から到達不能になります"
    );
    assert_eq!(outcome.object.as_ref(), outcome.created.first());
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
            expected: harness.expected(),
        })
        .expect_err("位置を特定できないのに成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::SdkError);
    assert_eq!(error.details()["mutation_issued"], json!(true));
    assert_eq!(error.details()["current_project_revision"], json!(1));
    assert_eq!(error.details()["retry_requires"], json!("refetch"));
}

// ------------------------------------------------------------------ read-back

#[test]
fn a_silently_ignored_effect_state_change_is_not_reported_as_success() {
    let harness =
        Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::IgnoreEffectState)));
    let error = harness
        .edit
        .set_effect_state(&SetEffectStateParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            enabled: Some(false),
            locked: None,
            expected: harness.expected(),
        })
        .expect_err("無言で無視された変更が成功として返りました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("effect_state_immutable"));
    assert_eq!(error.details()["mutation_issued"], json!(true));
}

#[test]
fn an_output_effect_is_rejected_before_the_section_when_the_type_is_known() {
    let harness = Harness::with(|host| {
        let mut scene = host.scene.lock().unwrap();
        scene.layers[1].objects[0].effects[0].name = "標準描画".to_string();
        drop(scene);
    });
    let error = harness
        .edit
        .set_effect_state(&SetEffectStateParams {
            selector: harness.effect_selector(1, 100, "標準描画", 0),
            enabled: Some(false),
            locked: None,
            expected: harness.expected(),
        })
        .expect_err("出力項目の有効・無効が変更できました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(harness.host.enter_calls(), 0);
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
            expected: harness.expected(),
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
            expected: harness.expected(),
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
            expected: harness.expected(),
        })
        .expect("削除に失敗しました");

    assert!(outcome.object.is_none());
    assert!(outcome.effect.is_none());
    // 削除の確認は同一区間内の読み直しで行う。
    let calls = harness.host.calls();
    let deleted = calls.iter().position(|call| *call == "delete_object");
    let confirmed = calls.iter().rposition(|call| *call == "object_detail");
    assert!(
        deleted < confirmed,
        "削除後の読み直しが行われていません: {calls:?}"
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
            expected: harness.expected(),
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
            expected: harness.expected(),
        })
        .expect_err("存在しない設定項目へ書き込めました");

    assert_eq!(error.error_code(), ErrorCode::NotFound);
    assert_eq!(error.details()["item"], json!("存在しない項目"));
    harness.assert_untouched();
}

#[test]
fn an_added_effect_is_located_by_the_difference_in_the_name_list() {
    let harness = Harness::new();
    let outcome = harness
        .edit
        .add_effect(&AddEffectParams {
            object: harness.selector(1, 100),
            effect_name: "ぼかし".to_string(),
            expected: harness.expected(),
        })
        .expect("effect の付与に失敗しました");

    let effect = outcome.effect.expect("付与された effect");
    assert_eq!(effect.name, "ぼかし");
    // 既に同名が 1 つあるため、同名内の順序は 1 になる。
    assert_eq!(effect.index, 1);
    assert_eq!(effect.selector.effect_index, 1);
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
        .set_effect_state(&SetEffectStateParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            // 2 つの変更 API を発行しても加算は 1 度きりである。
            enabled: Some(false),
            locked: Some(true),
            expected: harness.expected(),
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
            expected: harness.expected(),
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
fn changing_the_selection_ignores_a_stale_revision_but_not_a_stale_epoch() {
    let harness = Harness::new();
    let expected = harness.expected();
    harness.project.on_object_updated();

    harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition { layer: 1, frame: 5 }),
            selected_range: None,
            focus: None,
            expected: expected.clone(),
        })
        .expect("revision の照合で選択状態の変更が拒否されました");

    let error = harness
        .edit
        .set_selection(&SetSelectionParams {
            expected_scene_id: SCENE_ID,
            cursor: Some(CursorPosition { layer: 1, frame: 5 }),
            selected_range: None,
            focus: None,
            expected: Expected {
                project_epoch: "別のプロジェクト".to_string(),
                ..expected
            },
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
            expected: harness.expected(),
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
            "set_focus_object"
        ]
    );
    assert_eq!(
        state.applied,
        vec![
            SelectionField::Cursor,
            SelectionField::SelectedRange,
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
            expected: harness.expected(),
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
            expected: harness.expected(),
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
            expected: harness.expected(),
        })
        .expect("1 回目の編集に失敗しました");

    let effect = first.effect.expect("変更後の effect");
    harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: effect.selector,
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 40 },
            expected: Expected {
                project_epoch: first.project_epoch,
                project_revision: first.project_revision,
            },
        })
        .expect("応答が返したセレクターで続けて編集できませんでした");
}

#[test]
fn the_previous_selector_is_rejected_on_the_second_edit() {
    let harness = Harness::new();
    let selector = harness.effect_selector(1, 100, "ぼかし", 0);
    let expected = harness.expected();

    harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector: selector.clone(),
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 30 },
            expected: expected.clone(),
        })
        .expect("1 回目の編集に失敗しました");

    let error = harness
        .edit
        .set_object_item(&SetObjectItemParams {
            selector,
            item: "範囲".to_string(),
            value: ItemValue::Integer { value: 40 },
            expected,
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
            expected: harness.expected(),
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
            expected: harness.expected(),
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
            expected: harness.expected(),
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
            expected: harness.expected(),
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
            expected: harness.expected(),
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
            expected: harness.expected(),
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
        .set_effect_state(&SetEffectStateParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            enabled: Some(false),
            locked: None,
            expected: harness.expected(),
        })
        .expect("set_effect_state");
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
            expected: harness.expected(),
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
            expected: harness.expected(),
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
        expected: harness.expected(),
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
            expected: harness.expected(),
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
            expected: harness.expected(),
        })
        .expect_err("フォーカス対象の epoch 不一致が受理されました");
    assert_eq!(error.details()["mismatch"], json!("project_epoch"));
    assert!(!harness.host.mutated());
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
            expected: harness.expected(),
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
            expected: harness.expected(),
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
            expected: harness.expected(),
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
            expected: harness.expected(),
        })
        .expect("選択状態の変更");

    assert_eq!(
        state.applied,
        vec![
            SelectionField::Cursor,
            SelectionField::SelectedRange,
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

// ------------------------------------- 内容を変える operation の共通の約束

/// 内容を変える operation を 1 つ実行する。
type ContentEdit = fn(&Harness, Expected) -> Result<EditOutcome, EditError>;

/// 内容を変える 8 つの operation を 1 つずつ実行する。
///
/// 選択状態の変更だけは内容を変えないため含めない。含めるかどうかで
/// revision の扱いが変わるので、一覧はここへ 1 つだけ置く。
fn content_edits() -> Vec<(&'static str, ContentEdit)> {
    vec![
        ("create_object", |harness, expected| {
            harness.edit.create_object(&CreateObjectParams {
                source: ObjectSource::ObjectAlias {
                    alias: "[obj]".to_string(),
                },
                placement: Placement {
                    scene_id: SCENE_ID,
                    layer: 1,
                    frame: 600,
                },
                expected,
            })
        }),
        ("move_object", |harness, expected| {
            harness.edit.move_object(&MoveObjectParams {
                selector: harness.selector(1, 100),
                destination: Destination {
                    layer: 1,
                    frame: 500,
                },
                expected,
            })
        }),
        ("delete_object", |harness, expected| {
            harness.edit.delete_object(&DeleteObjectParams {
                selector: harness.selector(1, 100),
                expected,
            })
        }),
        ("set_object_name", |harness, expected| {
            harness.edit.set_object_name(&SetObjectNameParams {
                selector: harness.selector(1, 100),
                name: Some("名前".to_string()),
                expected,
            })
        }),
        ("set_object_item", |harness, expected| {
            harness.edit.set_object_item(&SetObjectItemParams {
                selector: harness.effect_selector(1, 100, "ぼかし", 0),
                item: "範囲".to_string(),
                value: ItemValue::Integer { value: 30 },
                expected,
            })
        }),
        ("add_effect", |harness, expected| {
            harness.edit.add_effect(&AddEffectParams {
                object: harness.selector(1, 100),
                effect_name: "ぼかし".to_string(),
                expected,
            })
        }),
        ("delete_effect", |harness, expected| {
            harness.edit.delete_effect(&DeleteEffectParams {
                selector: harness.effect_selector(1, 100, "ぼかし", 0),
                expected,
            })
        }),
        ("set_effect_state", |harness, expected| {
            harness.edit.set_effect_state(&SetEffectStateParams {
                selector: harness.effect_selector(1, 100, "ぼかし", 0),
                enabled: Some(false),
                locked: None,
                expected,
            })
        }),
    ]
}

#[test]
fn every_content_edit_checks_the_revision() {
    // 内容を変える operation から revision の照合が外れると、同じ前提での
    // 再送が通り、削除に対して残る唯一のガードが失われる。
    for (name, run) in content_edits() {
        let harness = Harness::new();
        let stale = harness.expected();
        // 対象は変えずに revision だけを進める。fingerprint は一致したままである。
        harness.project.on_object_updated();

        let Err(error) = run(&harness, stale) else {
            panic!("{name} が古い revision の前提を受理しました");
        };
        assert_eq!(
            error.error_code(),
            ErrorCode::PreconditionFailed,
            "{name} の revision 不一致が前提条件の不整合になりません"
        );
        assert_eq!(
            error.details()["mismatch"],
            json!("project_revision"),
            "{name}"
        );
        assert!(
            !harness.host.mutated(),
            "{name} が判定を通らずに変更 API を呼びました"
        );
    }
}

#[test]
fn every_content_edit_advances_the_revision_once() {
    for (name, run) in content_edits() {
        let harness = Harness::new();
        let expected = harness.expected();
        let outcome = run(&harness, expected).unwrap_or_else(|error| {
            panic!("{name} が失敗しました: {error}");
        });

        assert_eq!(
            harness.project.revision(),
            1,
            "{name} が revision を進めていません"
        );
        assert_eq!(
            outcome.project_revision, 1,
            "{name} の応答が加算後の revision を返していません"
        );
        assert!(
            harness.project.modified(),
            "{name} が未保存の変更を記録していません"
        );
    }
}

#[test]
fn every_content_edit_refuses_a_locked_layer() {
    // ロックの確認が一部の経路にしか無いと、残りの経路から無言で書き換えられる。
    // 利用者が明示的にロックした対象への書き換えは削除と同格の破壊である。
    for (name, run) in content_edits() {
        let harness = Harness::new();
        let expected = harness.expected();
        // 対象と作成先を含むレイヤーをロックする。
        harness.host.lock_layer(1, true);

        let Err(error) = run(&harness, expected) else {
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
            expected: harness.expected(),
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
fn an_audio_only_effect_refuses_a_lock_change_before_the_section() {
    // 音声だけを扱う effect はロックを変更できない。SDK を呼んでしまえば、
    // 届いた以上は変更が入った側へ倒すほかなく、何も変わっていないのに
    // revision が進む。呼ぶ前に分かる対象は呼ばずに弾く。
    let harness = Harness::with(|host| {
        let mut scene = host.scene.lock().unwrap();
        scene.layers[1].objects[0].effects[1].name = "音声フェード".to_string();
        drop(scene);
    });
    let error = harness
        .edit
        .set_effect_state(&SetEffectStateParams {
            selector: harness.effect_selector(1, 100, "音声フェード", 0),
            enabled: None,
            locked: Some(true),
            expected: harness.expected(),
        })
        .expect_err("音声 effect のロックが変更できました");

    assert_eq!(error.error_code(), ErrorCode::UnsupportedOperation);
    assert_eq!(error.details()["reason"], json!("effect_state_immutable"));
    assert_eq!(harness.host.enter_calls(), 0);
    harness.assert_untouched();
}

#[test]
fn an_effect_that_handles_video_as_well_is_not_refused_by_the_flags_alone() {
    // フラグは画像と音声が同時に立ち得る。音声のフラグだけを見て弾くと、
    // 変更できる対象まで拒否する。
    let harness = Harness::new();
    harness
        .edit
        .set_effect_state(&SetEffectStateParams {
            selector: harness.effect_selector(1, 100, "動画ファイル", 0),
            enabled: None,
            locked: Some(true),
            expected: harness.expected(),
        })
        .expect("画像も扱う effect のロック変更が拒否されました");
}
