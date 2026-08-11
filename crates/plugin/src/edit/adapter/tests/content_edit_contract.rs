//! 内容を変える operation 全体に共通する契約の統合テスト。

use super::*;

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
        EditOperation::MoveEffect => |harness: &Harness, target| {
            harness
                .edit
                .move_effect(&MoveEffectParams {
                    selector: harness.effect_selector_of(target, "ぼかし", 0),
                    position: 0,
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
        | EditOperation::MoveEffect
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
fn the_effect_precondition_asks_for_a_refetch() {
    for error in produced_effect_precondition_failures() {
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

#[test]
fn every_unsupported_reason_has_a_request_that_produces_it() {
    // 要求を書かないまま理由を足すと、応答に現れない名前が一覧へ残る。
    unsupported_target_failures();
}
