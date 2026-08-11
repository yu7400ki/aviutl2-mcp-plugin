//! 編集手順の統合テスト。
//!
//! フェイクは [`EditHost`] / [`SceneEditor`] の位置に差し込むため、検証の対象は
//! adapter の本番実装そのものになる。フェイクは呼び出しを順序ごと記録するので、
//! 順序自体を検証できる。

use super::*;
use crate::alias::tests::{TempDir, write_fixture};
use crate::edit::error::{EffectPreconditionReason, SectionPreconditionReason};
use crate::edit::fake::{
    CHOICE_VALUES, CLOSURE_ESCAPED, COORDINATE, CREATE_FRAME_SHIFT, DEFAULT_COLOR, DEFAULT_FONT,
    EFFECT_LIST, FakeCatalogEntry, FakeEditHost, FakeLayer, FakeObject, FakeReadHost, Fault,
    GROUP_CONTROL, ITEM_VALUE, Knobs, LAYER_ATTRIBUTES, LAYER_LOCK, LAYER_MAX, LAYER_RANGE_ITEM,
    MAX_FRAME, MAX_ITEM_VALUE, MAX_LAYER, MAX_SCENE_HEIGHT, MAX_SCENE_SAMPLE_RATE, MAX_SCENE_WIDTH,
    MOVE_FRAME_SHIFT, MOVING_ITEM, MUTATIONS, OBSERVED_SCENE, PanicPoint, READ_SECTION,
    RENAMED_SCENE_NAME, SCENE_ID, SCENE_ITEM, SCENE_NAME, SECTION_RANGES, SHAPE, STATIC_ITEM,
    TRACK_DEFAULT_PARAM, TRACK_MODES, TRACK_TIME_CONTROL_MODE, blur, coordinate,
    coordinate_catalog_entry, group_control, group_control_catalog_entry, raw_item_value, shape,
    shape_catalog_entry,
};
use crate::read::{HostReadAdapter, ReadAdapter};
use crate::test_support::{default_page_request, default_page_window, with_silent_panic_hook};
use aviutl2_mcp_core::{
    ApplyBatchParams, AvailableEffectItem, BatchOperation, CreateObjectSectionParams,
    CursorPosition, DeleteObjectSectionParams, Destination, DisplayStart, EditOperation,
    EffectFlags, EffectItem, EffectItemType, EffectSelector, EffectType, ErrorCode, Fingerprint,
    FiniteF64, GridBpm, ItemChoices, ItemFacets, ItemRange, ItemValue, LayerNameChange,
    MAX_GRID_BPM_ENTRIES, MoveEffectParams, MoveObjectSectionParams, Movement,
    ObjectSectionsOutcome, ObjectSelector, Placement, RangeChange, SceneSize, SelectionField,
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
        // 落ちたのは編集である。読み取りとして名乗ると、要求元は読み取りだけを
        // 試して通ると読む。
        assert_eq!(
            error.to_string(),
            format!("{state}のため編集できません"),
            "編集の拒否が編集として名乗っていません"
        );
        let details = error.details();
        assert_eq!(details["edit_state"], json!(state.as_str()));
        assert_eq!(details["retry_after_ms"], json!(2_000));
        assert_eq!(details["retry_requires"], json!("resend"));
        assert_eq!(harness.host.enter_calls(), 0, "{state} で区間へ入りました");
        harness.assert_untouched();
    }
}

#[test]
fn a_section_failure_is_reclassified_as_an_edit_that_was_blocked() {
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
    // 落ちたのは編集である。受付判定を通った後に再生や出力が始まった競合でも、
    // 名乗る文言は受付判定で拒んだ場合と同じでなければならない。
    assert_eq!(error.to_string(), "ファイル出力中のため編集できません");
    assert_eq!(error.details()["edit_state"], json!("save"));
    assert_eq!(error.details()["retry_requires"], json!("resend"));
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

/// 読み取り経路が参照区間へ入った回数。
fn read_sections(harness: &Harness) -> usize {
    harness
        .host
        .calls()
        .iter()
        .filter(|call| **call == READ_SECTION)
        .count()
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

// ------------------------------------------------------------------ read-back

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

/// 記録された呼び出しのうち、最初に変更 API が現れた位置。
fn first_mutation(calls: &[&'static str]) -> Option<usize> {
    calls.iter().position(|call| MUTATIONS.contains(call))
}

/// 記録された呼び出しのうち、指定した呼び出しが現れた回数。
fn count(calls: &[&'static str], call: &str) -> usize {
    calls.iter().filter(|recorded| **recorded == call).count()
}

/// パラメータを持たない移動の値。
fn movement(values: &[f64], mode: &str) -> ItemValue {
    movement_with_params(values, mode, &[])
}

/// パラメータを添えた移動の値。
fn movement_with_params(values: &[f64], mode: &str, params: &[f64]) -> ItemValue {
    let finite = |value: &f64| FiniteF64::try_new(*value).expect("有限値");
    ItemValue::Track(aviutl2_mcp_core::TrackValue {
        values: values.iter().map(finite).collect(),
        mode: Some(mode.to_string()),
        params: params.iter().map(finite).collect(),
        accelerate: false,
        decelerate: false,
        twopoint: false,
        reserved_flags: 0,
    })
}

/// 3 区間分の値と、[`TRACK_DEFAULT_PARAM`] の移動方法へ個数の合わないパラメータ。
fn movement_with_mismatched_params() -> ItemValue {
    movement_with_params(&[0.0, 50.0, 100.0], TRACK_DEFAULT_PARAM.0, &[30.0, 40.0])
}

/// [`movement_with_mismatched_params`] をホストが差し替えた後の生文字列。
fn replaced_movement_raw() -> String {
    format!(
        "0.00,50.00,100.00,{},0|{:.2}",
        TRACK_DEFAULT_PARAM.0, TRACK_DEFAULT_PARAM.1
    )
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
fn every_unsupported_reason_has_a_request_that_produces_it() {
    // 要求を書かないまま理由を足すと、応答に現れない名前が一覧へ残る。
    unsupported_target_failures();
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

    let harness = Harness::new();
    let outcome = harness
        .edit
        .move_effect(&MoveEffectParams {
            selector: harness.effect_selector(1, 100, "ぼかし", 0),
            position: 0,
        })
        .expect("move_effect");
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

/// 理由を実際に起こす要求を、編集手順へ通した結果として並べる。
///
/// [`EffectPreconditionReason`] に対する網羅 `match` であり `_` を使わない。
/// 理由を足すとここが落ち、それを起こす要求を書くまでコンパイルできない。
fn effect_precondition_case(
    harness: &Harness,
    reason: &EffectPreconditionReason,
) -> Vec<EditError> {
    match reason {
        EffectPreconditionReason::PositionOutOfRange => {
            // 既定の対象は effect を 2 つ持つ。列の長さちょうどと、それより
            // 先の位置の双方を落とす。
            [2, 7]
                .into_iter()
                .map(|position| {
                    harness
                        .edit
                        .move_effect(&MoveEffectParams {
                            selector: harness.effect_selector(1, 100, "ぼかし", 0),
                            position,
                        })
                        .expect_err("列の長さ以上の移動先が受理されました")
                })
                .collect()
        }
    }
}

/// 編集手順が実際に返した effect の前提条件の失敗を集める。
pub(crate) fn produced_effect_precondition_failures() -> Vec<EditError> {
    let harness = Harness::new();
    let mut produced = Vec::new();
    for reason in EffectPreconditionReason::ALL {
        let failures = effect_precondition_case(&harness, reason);
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
    // 事前確認は変更を 1 つも発行しない。
    harness.assert_untouched();
    produced
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
        UnsupportedReason::EffectNotMovable => {
            // 名乗るのは発行の前だけである。カタログの種別を読んで判定して
            // おり、名前が主張する内容を確かめている。
            let refused_by_type = Harness::new();
            vec![
                refused_by_type
                    .edit
                    .move_effect(&MoveEffectParams {
                        selector: refused_by_type.effect_selector(1, 100, "動画ファイル", 0),
                        position: 1,
                    })
                    .expect_err("フィルタでない effect を動かせました"),
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

/// effect 付与の統合テスト。
mod add_effect;
/// 一括適用の統合テスト。
mod apply_batch;
/// 内容を変える operation 全体に共通する契約の統合テスト。
mod content_edit_contract;
/// 対象作成の統合テスト。
mod create_object;
/// 対象削除の統合テスト。
mod delete_object;
/// effect の順序移動の統合テスト。
mod move_effect;
/// 対象移動の統合テスト。
mod move_object;
/// 区間の作成・削除・移動の統合テスト。
mod object_section;
/// 対象を解決する際の検証順序の統合テスト。
mod resolution_order;
/// effect 有効・無効変更の統合テスト。
mod set_effect_enabled;
/// BPM グリッド置き換えの統合テスト。
mod set_grid_bpm;
/// レイヤー状態変更の統合テスト。
mod set_layer_state;
/// 設定項目の値変更の統合テスト。
mod set_object_item;
/// 移動(トラックバー)を持つ設定項目の統合テスト。
mod set_object_item_movement;
/// 対象名変更の統合テスト。
mod set_object_name;
/// シーン設定変更の統合テスト。
mod set_scene_settings;
/// 選択状態変更の統合テスト。
mod set_selection;
