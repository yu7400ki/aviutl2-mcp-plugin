//! 対象作成の統合テスト。

use super::*;

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

/// テキスト種別の設定項目を持つ効果をカタログへ載せる形。
fn text_catalog_entry() -> FakeCatalogEntry {
    FakeCatalogEntry {
        name: "テキスト".to_string(),
        effect_type: EffectType::Filter,
        flags: EffectFlags::from_raw(1),
        items: vec![AvailableEffectItem {
            name: "テキスト".to_string(),
            item_type: EffectItemType::Text,
        }],
        facets: HashMap::new(),
    }
}

/// パス種別の設定項目を持つ効果をカタログへ載せる形。
fn image_file_catalog_entry() -> FakeCatalogEntry {
    FakeCatalogEntry {
        name: "画像ファイル".to_string(),
        effect_type: EffectType::Input,
        flags: EffectFlags::from_raw(1),
        items: vec![AvailableEffectItem {
            name: "ファイル".to_string(),
            item_type: EffectItemType::File,
        }],
        facets: HashMap::new(),
    }
}

/// テキスト種別とパス種別の双方を公開する効果を載せた一式を組む。
///
/// 既定のカタログはどちらの種別も持たない。**同じ綴りが種別で分かれること
/// は、双方を載せて初めて 1 つの一式の上で見える。**
fn alias_row_harness() -> Harness {
    Harness::with(|host| {
        host.catalog.push(text_catalog_entry());
        host.catalog.push(image_file_catalog_entry());
    })
}

/// テキスト種別の行が、エスケープを組まない `\` を綴る生テキスト。
const ALIAS_WITH_A_LOOSE_BACKSLASH: &str =
    "[Object]\r\nframe=0,80\r\n[Object.0]\r\neffect.name=テキスト\r\nテキスト=C:\\temp\\note\r\n";

/// パス種別の行が、生の `\` を綴る生テキスト。
///
/// **ホストが書き出すのはこの形である。** パス種別の値は `\` を 1 つも解かない。
const ALIAS_WITH_A_PATH_ROW: &str = "[Object]\r\nframe=0,80\r\n[Object.0]\r\neffect.name=画像ファイル\r\nファイル=C:\\temp\\note.png\r\n";

#[test]
fn a_raw_alias_whose_text_row_spells_a_backslash_loosely_is_refused_before_the_edit_section() {
    // テキスト種別の値では `\` がエスケープを組む。組まない綴りはホストの解釈と
    // 食い違う値になるため、区間へ入る前に落とす。
    let harness = alias_row_harness();
    harness.host.clear_calls();
    let before = harness.object_count();
    let error = harness
        .edit
        .create_object(&create_from_raw_alias_params(
            &harness,
            ALIAS_WITH_A_LOOSE_BACKSLASH,
        ))
        .expect_err("緩い綴りのテキスト行から作成できました");

    assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    let details = error.details();
    assert_eq!(details["reason"], json!("unescaped_backslash"));
    // どの節のどの項目かが分からなければ、要求元は直す行を選べない。
    assert_eq!(details["heading"], json!("Object.0"));
    assert_eq!(details["item"], json!("テキスト"));
    assert_eq!(harness.object_count(), before);
    assert!(harness.host.enter_calls() == 0, "編集区間へ入りました");
    harness.assert_untouched();
    // 落ちた行の値は運ばない。
    let document = format!("{} {}", error, error.details());
    assert!(!document.contains("C:"), "値が応答に含まれます: {document}");
}

#[test]
fn a_raw_alias_whose_path_row_carries_a_raw_backslash_reaches_the_sdk() {
    // **綴りだけを材料にした一律の規則はここで落ちる。** パス種別の値は解かれず、
    // 書いた綴りがそのまま保存される。書き換えようもない——`\` を 2 つにすれば
    // `\` が 2 つ並んだパスになる。
    let harness = alias_row_harness();
    harness.host.clear_calls();
    harness
        .edit
        .create_object(&create_from_raw_alias_params(
            &harness,
            ALIAS_WITH_A_PATH_ROW,
        ))
        .expect("パス種別の行を持つエイリアスが拒否されました");

    assert!(
        harness.host.calls().contains(&"create_object_from_alias"),
        "エイリアスが SDK へ届いていません"
    );
}

#[test]
fn a_creation_by_name_is_not_held_to_the_text_rows_the_raw_text_is() {
    // 一覧は本文の行を 1 つも見ていない。作成にだけ条件を足せば「一覧に出た
    // 名前は必ず作成できる」が崩れる。
    let (dir, _) = alias_fixture();
    dir.write_alias("緩い綴り", ALIAS_WITH_A_LOOSE_BACKSLASH.as_bytes());
    let harness = alias_row_harness();
    harness
        .host
        .set_alias_data_directory(Some(dir.path().to_path_buf()));
    assert!(
        listed_alias_names(&dir).contains(&"緩い綴り".to_string()),
        "fixture が一覧に載っていません"
    );

    harness
        .edit
        .create_object(&create_from_alias_name(&harness, "緩い綴り", 1, 600))
        .expect("一覧に載る名前から作成できませんでした");
    // 生テキストとして同じバイト列を渡す経路は拒否する。
    let harness = alias_row_harness();
    harness
        .edit
        .create_object(&create_from_raw_alias_params(
            &harness,
            ALIAS_WITH_A_LOOSE_BACKSLASH,
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

// ------------------------------------------------ 配置が調整されたことの申告

/// 配置先からの相対位置を並べた、複数オブジェクト形式のエイリアスを綴る。
///
/// 節は渡された順に番号を持つ。フェイクは作成のたびに開始フレームを
/// [`CREATE_FRAME_SHIFT`] だけ後ろへずらすため、その分を織り込んだ相対フレーム
/// だけが実際に生まれる配置と一致する。
fn alias_with_relative_placements(nodes: [(usize, usize); 2]) -> String {
    let mut alias = String::new();
    for (index, (layer, frame)) in nodes.into_iter().enumerate() {
        alias.push_str(&format!(
            "[{index}]\r\nlayer={layer}\r\nframe={frame},{}\r\n[{index}.0]\r\neffect.name=図形\r\n",
            frame + 59,
        ));
    }
    alias
}

/// 生テキストのエイリアスを、2 件が生まれる一式へ配置先 (0, 600) で渡す。
fn create_pair_from_alias(alias: &str) -> Result<EditOutcome, EditError> {
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::CreatePair)));
    harness.edit.create_object(&CreateObjectParams {
        source: ObjectSource::ObjectAlias {
            alias: alias.to_string(),
        },
        placement: Placement {
            scene_id: SCENE_ID,
            layer: 0,
            frame: 600,
        },
        expected_project_epoch: harness.epoch(),
    })
}

/// 応答が並べる、実際に生まれた配置。
fn created_positions(outcome: &EditOutcome) -> Vec<(usize, usize)> {
    outcome
        .created
        .iter()
        .map(|item| (item.layer, item.frame_start))
        .collect()
}

#[test]
fn an_alias_that_landed_where_it_asked_does_not_claim_an_adjustment() {
    // 相対値へ配置先を加えたものが要求した配置である。実際の配置がそれと 1 件も
    // 違わなければ、要求元が全件を見る理由は無い。
    let outcome = create_pair_from_alias(&alias_with_relative_placements([
        (0, CREATE_FRAME_SHIFT),
        (1, CREATE_FRAME_SHIFT + 60),
    ]))
    .expect("作成に失敗しました");

    assert_eq!(
        created_positions(&outcome),
        vec![
            (0, 600 + CREATE_FRAME_SHIFT),
            (1, 600 + CREATE_FRAME_SHIFT + 60)
        ]
    );
    assert_eq!(outcome.placement_adjusted, Some(false));
}

#[test]
fn the_comparison_does_not_depend_on_the_order_the_alias_numbers_its_objects() {
    // 要求した配置は節の番号順に並び、実際の配置はレイヤーの昇順に並ぶ。順序を
    // 持つ突き合わせにすると、番号を降順に綴ったエイリアスは要求どおりに生まれ
    // ても真になる。
    let outcome = create_pair_from_alias(&alias_with_relative_placements([
        (1, CREATE_FRAME_SHIFT + 60),
        (0, CREATE_FRAME_SHIFT),
    ]))
    .expect("作成に失敗しました");

    assert_eq!(
        created_positions(&outcome),
        vec![
            (0, 600 + CREATE_FRAME_SHIFT),
            (1, 600 + CREATE_FRAME_SHIFT + 60)
        ]
    );
    assert_eq!(outcome.placement_adjusted, Some(false));
}

#[test]
fn an_alias_with_a_single_object_out_of_place_claims_the_adjustment_without_naming_it() {
    // 1 件だけがずれた結果と、全件がずれた結果は同じ真を返す。どの 1 件かは
    // 名乗らない——実際の配置は created の各要素が既に持っている。
    let one_moved = create_pair_from_alias(&alias_with_relative_placements([
        (0, CREATE_FRAME_SHIFT),
        (1, CREATE_FRAME_SHIFT + 61),
    ]))
    .expect("作成に失敗しました");
    assert_eq!(
        created_positions(&one_moved),
        vec![
            (0, 600 + CREATE_FRAME_SHIFT),
            (1, 600 + CREATE_FRAME_SHIFT + 60)
        ],
        "先頭は要求どおりの位置に生まれています"
    );
    assert_eq!(one_moved.placement_adjusted, Some(true));

    let all_moved = create_pair_from_alias(&alias_with_relative_placements([(0, 0), (1, 60)]))
        .expect("作成に失敗しました");
    assert_eq!(
        all_moved.placement_adjusted, one_moved.placement_adjusted,
        "ずれた件数が応答から読めています"
    );
}

#[test]
fn an_alias_that_produced_a_different_number_of_objects_claims_the_adjustment() {
    // 件数が違えば、位置を突き合わせるまでもなく要求どおりではない。
    let outcome =
        create_pair_from_alias("[Object]\r\nframe=0\r\n[Object.0]\r\neffect.name=図形\r\n")
            .expect("作成に失敗しました");

    assert_eq!(outcome.created.len(), 2);
    assert_eq!(outcome.placement_adjusted, Some(true));
}

#[test]
fn a_media_file_and_an_effect_name_are_compared_against_the_placement_itself() {
    // 相対位置を持たない作成元では、要求した配置は配置先そのものである。
    // フェイクは開始フレームを後ろへずらすため、どちらも調整として現れる。
    let harness = Harness::new();
    let from_media = harness
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
    assert_eq!(
        created_positions(&from_media),
        vec![(1, 600 + CREATE_FRAME_SHIFT)]
    );
    assert_eq!(from_media.placement_adjusted, Some(true));

    let harness = Harness::new();
    let from_effect = harness
        .edit
        .create_object(&create_from_effect(&harness, "ぼかし", 1, 600))
        .expect("作成に失敗しました");
    assert_eq!(
        created_positions(&from_effect),
        vec![(1, 600 + CREATE_FRAME_SHIFT)]
    );
    assert_eq!(from_effect.placement_adjusted, Some(true));
}

#[test]
fn a_length_the_host_chose_is_not_counted_as_an_adjustment() {
    // 比べるのは配置だけである。長さを含めると、長さを要求できない作成元では
    // 常に真になり、作成元によって名乗る条件が変わる。
    let harness = Harness::new();
    let outcome = harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: format!(
                    "[Object]\r\nframe={CREATE_FRAME_SHIFT},{}\r\n[Object.0]\r\neffect.name=図形\r\n",
                    CREATE_FRAME_SHIFT + 999
                ),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 600,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect("作成に失敗しました");

    let created = &outcome.created[0];
    assert_eq!(
        (created.layer, created.frame_start),
        (1, 600 + CREATE_FRAME_SHIFT)
    );
    assert_ne!(
        created.frame_end,
        600 + CREATE_FRAME_SHIFT + 999,
        "要求した長さがそのまま入っており、長さの調整を作れていません"
    );
    assert_eq!(outcome.placement_adjusted, Some(false));
}

#[test]
fn a_registered_alias_is_compared_against_the_relative_placements_of_its_own_body() {
    // 名前で指定されたエイリアスも、相対位置は本文から引く。配置先だけを要求と
    // すれば、2 件を生む本文との件数が食い違い、常に真になる。
    let dir = TempDir::new();
    dir.write_alias(
        "相対",
        alias_with_relative_placements([(0, CREATE_FRAME_SHIFT), (1, CREATE_FRAME_SHIFT + 60)])
            .as_bytes(),
    );
    let harness = Harness::with(|host| host.arm(|knobs| knobs.fault = Some(Fault::CreatePair)));
    harness
        .host
        .set_alias_data_directory(Some(dir.path().to_path_buf()));

    let outcome = harness
        .edit
        .create_object(&create_from_alias_name(&harness, "相対", 0, 600))
        .expect("作成に失敗しました");

    assert_eq!(
        created_positions(&outcome),
        vec![
            (0, 600 + CREATE_FRAME_SHIFT),
            (1, 600 + CREATE_FRAME_SHIFT + 60)
        ]
    );
    assert_eq!(outcome.placement_adjusted, Some(false));
}

#[test]
fn the_occupancy_check_looks_at_the_first_object_rather_than_at_the_placement() {
    // レイヤー 1 はフレーム 0 が空いており、100-200 が埋まっている。相対 frame を
    // 持つエイリアスの先頭はその埋まった区間へ来る。要求元は名乗られた位置を見て
    // 次の宛先を選ぶため、空いている配置先を返せば同じ失敗を繰り返す経路になる。
    let harness = Harness::new();
    let error = harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "[Object]\r\nframe=150\r\n[Object.0]\r\neffect.name=図形\r\n".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 0,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect_err("埋まった位置へ来るエイリアスが受理されました");

    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    let details = error.details();
    assert_eq!(details["reason"], json!("destination_occupied"));
    assert_eq!(details["layer"], json!(1));
    assert_eq!(
        details["frame"],
        json!(150),
        "空いている配置先を、衝突する位置として名乗っています"
    );
    assert_eq!(
        details["occupied_by"],
        json!({ "frame_start": 100, "frame_end": 200 })
    );
    harness.assert_untouched();

    // 同じ配置先へ、相対 frame を持たないエイリアスは置ける。落ちたのは配置先で
    // はなく先頭オブジェクトの位置である。
    let harness = Harness::new();
    harness
        .edit
        .create_object(&CreateObjectParams {
            source: ObjectSource::ObjectAlias {
                alias: "[Object]\r\nframe=0\r\n[Object.0]\r\neffect.name=図形\r\n".to_string(),
            },
            placement: Placement {
                scene_id: SCENE_ID,
                layer: 1,
                frame: 0,
            },
            expected_project_epoch: harness.epoch(),
        })
        .expect("空いている配置先への作成が拒まれました");
}

/// 同じ 2 つの相対位置を、番号の順だけ入れ替えたエイリアスへ渡す。
///
/// 配置先はレイヤー 1 のフレーム 0 である。相対フレーム 150 の側は 100-200 の
/// 対象と重なり、相対フレーム 0 の側は空いている。
fn create_from_swapped_pair(occupied_first: bool) -> Result<EditOutcome, EditError> {
    let nodes = match occupied_first {
        true => [(0, 150), (0, 0)],
        false => [(0, 0), (0, 150)],
    };
    let harness = Harness::new();
    harness.edit.create_object(&CreateObjectParams {
        source: ObjectSource::ObjectAlias {
            alias: alias_with_relative_placements(nodes),
        },
        placement: Placement {
            scene_id: SCENE_ID,
            layer: 1,
            frame: 0,
        },
        expected_project_epoch: harness.epoch(),
    })
}

#[test]
fn the_occupancy_check_looks_at_the_first_object_rather_than_at_a_later_one() {
    // 確かめるのは 1 点だけであり、それは番号が先頭の節である。2 つの要求は同じ
    // 2 つの位置を持ち、番号の順だけが違う。
    let error =
        create_from_swapped_pair(true).expect_err("先頭が埋まったエイリアスが受理されました");
    assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
    let details = error.details();
    assert_eq!(details["reason"], json!("destination_occupied"));
    assert_eq!(details["layer"], json!(1));
    assert_eq!(details["frame"], json!(150));

    // 先頭以外が埋まっていても拒まない。ずれるかどうかはホストが決めることで
    // あり、その規則を我々は持っていない。要求は成功し、応答が調整を名乗る。
    let outcome = create_from_swapped_pair(false).expect("先頭以外の重なりで作成が拒まれました");
    assert_eq!(outcome.placement_adjusted, Some(true));
}
