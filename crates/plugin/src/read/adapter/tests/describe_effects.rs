//! effect の中身の説明の統合テスト。

use super::*;

#[test]
fn describe_effects_returns_every_requested_effect_in_the_requested_order() {
    // 名前の似た effect の使い分けは設定項目の顔ぶれで解ける。並べて引ける
    // ことと、要求の順が保たれることがその前提である。
    let adapter = adapter();
    let result = adapter
        .describe_effects(&describe_params(&["グロー", "ぼかし"]))
        .unwrap();

    let names: Vec<&str> = result
        .effects
        .iter()
        .map(|effect| effect.name.as_str())
        .collect();
    assert_eq!(names, vec!["グロー", "ぼかし"]);
    assert!(result.not_found.is_empty());

    // 項目の名前と種別はホストの列挙から来る。件数も並びもそのままである。
    // フェイクの番号は名前の昇順とも降順とも一致しないため、見栄えのために
    // 名前で並べ替えた実装はここに現れる。
    let glow: Vec<(&str, &EffectItemType)> = result.effects[0]
        .items
        .iter()
        .map(|item| (item.name.as_str(), &item.item_type))
        .collect();
    assert_eq!(
        glow,
        vec![
            ("グローの項目1", &EffectItemType::Integer),
            ("グローの項目0", &EffectItemType::Check),
            ("グローの項目3", &EffectItemType::Integer),
            ("グローの項目2", &EffectItemType::Check),
        ],
        "項目の一覧が別の effect のものになっているか、並びが崩れています"
    );
    assert_eq!(result.effects[1].items.len(), 1);
}

#[test]
fn describe_effects_asks_the_host_for_the_catalog_once() {
    // 登録の有無はカタログで決める。判定を effect ごとのループへ入れると、
    // 全件の列挙が要求した名前の数だけ走る。
    let adapter = adapter();
    adapter
        .describe_effects(&describe_params(&["グロー", "ぼかし", "存在しない効果"]))
        .unwrap();

    assert_eq!(
        adapter
            .host
            .calls()
            .iter()
            .filter(|call| **call == "effect_catalog")
            .count(),
        1,
        "カタログを要求ごとに 1 度だけ引いていません"
    );
}

#[test]
fn describe_effects_tells_a_described_effect_apart_from_an_undescribed_one() {
    // 説明を持つ effect と持たない effect は同じ応答に混ざる。持たない側を
    // 推測で埋めない。
    let adapter = adapter_with(|_| FakeHost {
        help: vec![(
            "グロー".to_string(),
            effect_help(
                Some("光を拡散させます"),
                // 列挙の先頭に来る項目。番号は並びのままではない。
                &[("グローの項目1", "拡散の量を指定します")],
            ),
        )],
        ..FakeHost::new()
    });
    let result = adapter
        .describe_effects(&describe_params(&["グロー", "ぼかし"]))
        .unwrap();

    assert_eq!(
        result.effects[0].description.as_deref(),
        Some("光を拡散させます")
    );
    assert_eq!(
        result.effects[0].items[0].description.as_deref(),
        Some("拡散の量を指定します")
    );
    // 説明の無い項目は同じ effect の中でも null になる。
    assert_eq!(result.effects[0].items[1].description, None);

    assert_eq!(result.effects[1].name, "ぼかし");
    assert_eq!(result.effects[1].description, None);
    assert_eq!(result.effects[1].items[0].description, None);
}

#[test]
fn describe_effects_never_reports_the_effect_description_as_an_item_description() {
    // 効果の説明が項目の説明として出れば、要求元は誤った文言を確信を持って
    // 使う。供給源はどちらも同じ節に並ぶため、区別を実際に確かめる。
    let adapter = adapter_with(|_| FakeHost {
        help: vec![(
            "ぼかし".to_string(),
            effect_help(
                Some("効果そのものの説明です"),
                &[("ぼかしの項目0", "項目の説明です")],
            ),
        )],
        ..FakeHost::new()
    });
    let result = adapter
        .describe_effects(&describe_params(&["ぼかし"]))
        .unwrap();

    let effect = &result.effects[0];
    assert_eq!(
        effect.description.as_deref(),
        Some("効果そのものの説明です")
    );
    assert_eq!(
        effect.items[0].description.as_deref(),
        Some("項目の説明です")
    );
    assert!(
        !effect
            .items
            .iter()
            .any(|item| item.description == effect.description),
        "項目の説明として効果の説明が出ています"
    );
}

#[test]
fn describe_effects_names_what_it_could_not_find_instead_of_dropping_it() {
    // 落ちた名前に気付けなければ、要求元は「その effect には設定項目が無い」
    // と誤読する。見つかった分は返しつつ、見つからなかった名前を明示する。
    let adapter = adapter();
    let result = adapter
        .describe_effects(&describe_params(&["ぐろー", "ぼかし", "存在しない効果"]))
        .unwrap();

    let names: Vec<&str> = result
        .effects
        .iter()
        .map(|effect| effect.name.as_str())
        .collect();
    assert_eq!(names, vec!["ぼかし"]);
    assert_eq!(
        result.not_found,
        vec!["ぐろー".to_string(), "存在しない効果".to_string()],
        "見つからなかった名前が要求の順で並んでいません"
    );
    // 登録の無い名前について設定項目を列挙しない。列挙の失敗は「登録が無い」
    // と区別できず、要求全体を落としてしまう。
    assert_eq!(
        adapter
            .host
            .calls()
            .iter()
            .filter(|call| **call == "effect_items")
            .count(),
        1
    );
}

#[test]
fn describe_effects_keeps_every_line_of_the_help_text() {
    // 発見の鍵が 2 行目にある説明が実在する。効果の説明も項目の説明も
    // 先頭行だけに切らない。
    let adapter = adapter_with(|_| FakeHost {
        help: vec![(
            "ぼかし".to_string(),
            effect_help(
                Some("単色の図形を作成します\nsvgファイルから読み込むことも出来ます"),
                &[(
                    "ぼかしの項目0",
                    "図形の種類を選択します\nボタンクリックでsvgファイルを選択出来ます",
                )],
            ),
        )],
        ..FakeHost::new()
    });
    let result = adapter
        .describe_effects(&describe_params(&["ぼかし"]))
        .unwrap();

    let effect = &result.effects[0];
    let description = effect.description.as_deref().expect("効果の説明がある");
    assert_eq!(description.lines().count(), 2);
    assert!(description.lines().nth(1).unwrap().contains("svg"));

    let item = effect.items[0]
        .description
        .as_deref()
        .expect("項目の説明がある");
    assert_eq!(item.lines().count(), 2);
    assert!(item.lines().nth(1).unwrap().contains("svg"));
}

#[test]
fn describe_effects_carries_the_choices_of_each_item_with_their_source() {
    // 候補を知らないことが損失の原因である。読み取り経路へ出さなければ、
    // 要求元は候補の存在そのものに気付けない。
    let adapter = adapter_with(|_| FakeHost {
        facets: vec![(
            "グロー".to_string(),
            effect_facets(&[
                (
                    "グローの項目1",
                    choices_facet(&["通常", "加算"], TableSource::BuiltinTable),
                ),
                (
                    "グローの項目0",
                    choices_facet(&["円", "四角形"], TableSource::Sidecar),
                ),
            ]),
        )],
        ..FakeHost::new()
    });
    let result = adapter
        .describe_effects(&describe_params(&["グロー", "ぼかし"]))
        .unwrap();

    let described: Vec<(&str, Option<&ItemChoices>)> = result.effects[0]
        .items
        .iter()
        .map(|item| (item.name.as_str(), item.choices.as_ref()))
        .collect();
    assert_eq!(
        described,
        vec![
            (
                "グローの項目1",
                Some(&ItemChoices {
                    values: vec!["通常".to_string(), "加算".to_string()],
                    source: TableSource::BuiltinTable,
                })
            ),
            (
                "グローの項目0",
                Some(&ItemChoices {
                    values: vec!["円".to_string(), "四角形".to_string()],
                    source: TableSource::Sidecar,
                })
            ),
            // 表に無い項目は null である。空の配列で代えると、「候補が
            // 1 つも無い項目」と「表に載っていない項目」の区別が付かない。
            ("グローの項目3", None),
            ("グローの項目2", None),
        ],
        "候補が別の項目へ付いているか、由来が入れ替わっています"
    );
    // 表を持たない effect の項目も null になる。
    assert!(
        result.effects[1]
            .items
            .iter()
            .all(|item| item.choices.is_none()),
        "表に無い effect へ候補が付きました"
    );
}

#[test]
fn describe_effects_carries_the_range_of_each_item_with_its_source() {
    // 値域を知らないことは、値域外の指定と値域内の取り違えの両方を招く。
    // 後者は書き戻し照合も通るため、書く前に読めることだけが手段になる。
    let adapter = adapter_with(|_| FakeHost {
        facets: vec![(
            "グロー".to_string(),
            effect_facets(&[
                (
                    "グローの項目1",
                    range_facet(Some(1.0), Some(4000.0), Some(0), TableSource::BuiltinTable),
                ),
                // **測れた側だけを載せる。** 3 つの値は個別に欠ける。
                (
                    "グローの項目0",
                    range_facet(None, Some(100.0), None, TableSource::Sidecar),
                ),
            ]),
        )],
        ..FakeHost::new()
    });
    let result = adapter
        .describe_effects(&describe_params(&["グロー", "ぼかし"]))
        .unwrap();

    let described: Vec<(&str, Option<ItemRange>)> = result.effects[0]
        .items
        .iter()
        .map(|item| (item.name.as_str(), item.range))
        .collect();
    assert_eq!(
        described,
        vec![
            (
                "グローの項目1",
                Some(ItemRange {
                    min: FiniteF64::try_new(1.0),
                    max: FiniteF64::try_new(4000.0),
                    decimals: Some(0),
                    source: TableSource::BuiltinTable,
                })
            ),
            (
                "グローの項目0",
                Some(ItemRange {
                    min: None,
                    max: FiniteF64::try_new(100.0),
                    decimals: None,
                    source: TableSource::Sidecar,
                })
            ),
            // 表に無い項目は null である。**値域そのものが null であることと、
            // 上限だけが null であることは別の事実である。**
            ("グローの項目3", None),
            ("グローの項目2", None),
        ],
        "値域が別の項目へ付いているか、測れていない側が埋められています"
    );
    assert!(
        result.effects[1]
            .items
            .iter()
            .all(|item| item.range.is_none()),
        "表に無い effect へ値域が付きました"
    );
}

#[test]
fn describe_effects_does_not_filter_the_facets_by_item_type() {
    // **種別で絞らない。** 表に載っていれば text の項目にも候補と値域が付く。
    // 絞ると、表が書いた記述を我々の判断で黙って落とすことになり、落とした
    // 側に「面が無い」と「面を出さないことにした」の区別は届かない。
    //
    // **数値や選択肢の項目で確かめても意味が無い**——種別で絞る実装でも
    // 通ってしまう。値域を持ちそうにない種別で見る。
    let adapter = adapter_with(|_| FakeHost {
        catalog: vec![FakeCatalogEntry {
            summary: HostEffectSummary {
                name: "字幕".to_string(),
                effect_type: EffectType::Filter,
                flags: EffectFlags::from_raw(1),
            },
            items: vec![AvailableEffectItem {
                name: "本文".to_string(),
                item_type: EffectItemType::Text,
            }],
        }],
        facets: vec![(
            "字幕".to_string(),
            effect_facets(&[(
                "本文",
                ItemFacets {
                    choices: Some(ItemChoices {
                        values: vec!["既定".to_string()],
                        source: TableSource::Sidecar,
                    }),
                    range: Some(ItemRange {
                        min: None,
                        max: FiniteF64::try_new(1024.0),
                        decimals: None,
                        source: TableSource::Sidecar,
                    }),
                },
            )]),
        )],
        ..FakeHost::new()
    });
    let result = adapter
        .describe_effects(&describe_params(&["字幕"]))
        .unwrap();

    let item = &result.effects[0].items[0];
    assert_eq!(item.item_type, EffectItemType::Text);
    assert!(item.choices.is_some(), "text の項目から候補が落ちました");
    assert_eq!(
        item.range.and_then(|range| range.max),
        FiniteF64::try_new(1024.0),
        "text の項目から値域が落ちました"
    );
}

#[test]
fn describe_effects_ignores_a_table_entry_for_an_item_that_does_not_exist_here() {
    // 利用者は複数の環境で同じサイドカーを使う。この環境のホストが列挙
    // しない項目は、表に在っても応答には現れない。落とさずに失敗へ倒すと、
    // 環境をまたぐ配布物が tool を壊す。
    let adapter = adapter_with(|_| FakeHost {
        facets: vec![(
            "ぼかし".to_string(),
            effect_facets(&[
                (
                    "ぼかしの項目0",
                    choices_facet(&["通常"], TableSource::Sidecar),
                ),
                (
                    "この環境に無い項目",
                    choices_facet(&["値"], TableSource::Sidecar),
                ),
            ]),
        )],
        ..FakeHost::new()
    });
    let result = adapter
        .describe_effects(&describe_params(&["ぼかし"]))
        .unwrap();

    let names: Vec<&str> = result.effects[0]
        .items
        .iter()
        .map(|item| item.name.as_str())
        .collect();
    assert_eq!(names, vec!["ぼかしの項目0"]);
    assert!(result.effects[0].items[0].choices.is_some());
}

#[test]
fn describe_effects_asks_for_the_facets_once_per_effect() {
    // 面も効果ごとに 1 度で引く。項目ごとに引くと、同じ表を項目の数だけ
    // 引き直す。
    let adapter = adapter();
    adapter
        .describe_effects(&describe_params(&["グロー", "標準描画"]))
        .unwrap();

    let asked = adapter
        .host
        .calls()
        .iter()
        .filter(|call| **call == "effect_facets")
        .count();
    assert_eq!(asked, 2, "効果の数と面を引いた回数が食い違っています");
}

#[test]
fn describe_effects_asks_the_host_once_per_effect() {
    // 説明は効果ごとに 1 度で節全体を引く。項目ごとに引くと、同じ節を項目の
    // 数だけ引き直すうえ、効果の説明を指すキーを項目名として渡せてしまう。
    let adapter = adapter();
    adapter
        .describe_effects(&describe_params(&["グロー", "標準描画"]))
        .unwrap();

    let asked = adapter
        .host
        .calls()
        .iter()
        .filter(|call| **call == "effect_help")
        .count();
    assert_eq!(asked, 2, "効果の数と説明を引いた回数が食い違っています");
}

#[test]
fn describe_effects_carries_the_group_of_each_item_as_the_host_returned_it() {
    // 設定項目の一覧は平らな列で返る。どこが 1 つの組かを述べるのは
    // グループだけであり、位置も所属アイテム名もホストが返した値のまま運ぶ。
    //
    // 同じグループに属する 2 件へ別々の位置を持たせ、どちらの位置も設定項目の
    // 列挙順とは一致させない。位置を取り違えた実装も、ホストの戻り値ではなく
    // 列挙順から位置を作った実装も、結果に現れる。
    let adapter = adapter_with(|_| FakeHost {
        groups: vec![
            member_group("グロー", "グローの項目1", 1, &GLOW_AXES),
            member_group("グロー", "グローの項目0", 0, &GLOW_AXES),
        ],
        ..FakeHost::new()
    });
    let result = adapter
        .describe_effects(&describe_params(&["グロー"]))
        .unwrap();

    let items = &result.effects[0].items;
    assert_eq!(items[0].name, "グローの項目1");
    assert_eq!(items[0].group, Some(item_group(1, &GLOW_AXES)));
    assert_eq!(items[1].name, "グローの項目0");
    assert_eq!(items[1].group, Some(item_group(0, &GLOW_AXES)));
}

#[test]
fn describe_effects_reports_no_group_for_an_item_that_belongs_to_none() {
    // 属さないことは null で表す。空のグループを作ると、所属アイテムが
    // 1 件も無いグループに属していることになる。
    let adapter = adapter();
    let result = adapter
        .describe_effects(&describe_params(&["グロー", "ぼかし"]))
        .unwrap();

    assert!(
        result
            .effects
            .iter()
            .flat_map(|effect| &effect.items)
            .all(|item| item.group.is_none()),
        "属さない項目にグループが付きました"
    );
}

#[test]
fn describe_effects_fails_when_a_group_cannot_be_read() {
    // 引けなかったことを null にすると、属さないことと同じ形で届く。要求元は
    // 「この項目はグループに属さない」と読み、区別する手段を失う。
    let adapter = adapter_with(|_| FakeHost {
        groups: vec![(
            ("グロー".to_string(), "グローの項目3".to_string()),
            FakeItemGroup::Unavailable,
        )],
        ..FakeHost::new()
    });

    let error = adapter
        .describe_effects(&describe_params(&["グロー"]))
        .expect_err("引けなかった項目が失敗として返っていません");
    assert!(
        matches!(
            error,
            ReadError::Sdk {
                operation: "get_effect_item_group_names"
            }
        ),
        "{error:?}"
    );
}

#[test]
fn describe_effects_carries_a_group_that_does_not_name_the_item_it_was_asked_about() {
    // 所属アイテム名の位置番目が問い合わせた項目名と一致することは
    // 確かめない。一致しないときに採れる手は落とすことしか無く、落とせば
    // 「グループに属さない」として届いて引けなかったことと同じ形になる。
    let names = ["別の項目A", "別の項目B", "別の項目C"];
    let adapter = adapter_with(|_| FakeHost {
        groups: vec![
            member_group("グロー", "グローの項目1", 2, &names),
            // 位置が所属アイテム名の外を指す。
            member_group("グロー", "グローの項目0", 5, &["グローの項目0"]),
        ],
        ..FakeHost::new()
    });
    let result = adapter
        .describe_effects(&describe_params(&["グロー"]))
        .unwrap();

    let items = &result.effects[0].items;
    assert_eq!(
        items[0].group,
        Some(item_group(2, &names)),
        "問い合わせた項目名を含まないグループが落とされました"
    );
    assert_eq!(
        items[1].group,
        Some(item_group(5, &["グローの項目0"])),
        "所属アイテム名の外を指す位置が落とされました"
    );
}

#[test]
fn describe_effects_mixes_grouped_and_ungrouped_items_in_one_effect() {
    // 1 つの effect の中で、別々のグループへ属する項目と属さない項目が
    // 混ざる。グループが隣の項目や別の effect へ漏れないことを列全体で見る。
    let adapter = adapter_with(|_| FakeHost {
        groups: mixed_groups(),
        ..FakeHost::new()
    });
    let result = adapter
        .describe_effects(&describe_params(&["グロー", "ぼかし"]))
        .unwrap();

    let described: Vec<(&str, Option<ItemGroup>)> = result.effects[0]
        .items
        .iter()
        .map(|item| (item.name.as_str(), item.group.clone()))
        .collect();
    assert_eq!(
        described,
        vec![
            ("グローの項目1", Some(item_group(1, &GLOW_AXES))),
            ("グローの項目0", Some(item_group(0, &GLOW_AXES))),
            ("グローの項目3", Some(item_group(0, &["グローの項目3"]))),
            ("グローの項目2", None),
        ],
        "グループが別の項目へ付いているか、属さない項目に現れています"
    );
    assert!(
        result.effects[1]
            .items
            .iter()
            .all(|item| item.group.is_none()),
        "別の effect の項目へグループが漏れました"
    );
}

#[test]
fn describe_effects_asks_for_the_group_once_per_item() {
    // グループは項目ごとに引く。引いたグループの所属アイテム名から他の項目の
    // グループを導くと、項目名が effect の中で一意であることを前提に置く。
    //
    // **導ける材料が実際に返る状況で数える。** 属する項目が 1 件も無ければ
    // 導きようが無く、導く実装でも回数が項目数と等しくなる。
    let adapter = adapter_with(|_| FakeHost {
        groups: mixed_groups(),
        ..FakeHost::new()
    });
    let result = adapter
        .describe_effects(&describe_params(&["グロー", "ぼかし"]))
        .unwrap();

    let items: usize = result.effects.iter().map(|effect| effect.items.len()).sum();
    let asked = adapter
        .host
        .calls()
        .iter()
        .filter(|call| **call == "effect_item_group")
        .count();
    assert_eq!(
        asked, items,
        "設定項目の数とグループを引いた回数が食い違っています"
    );
}

#[test]
fn the_fake_host_answers_about_a_group_in_three_ways() {
    // グループに属する・属さない・引けないの 3 通りを作り分けられる。
    // 引けないを作れなければ、失敗を「属さない」へ倒した実装を検査できない。
    let group = ItemGroup {
        index: 1,
        item_names: vec!["グローの項目1".to_string(), "グローの項目0".to_string()],
    };
    let host = FakeHost {
        groups: vec![
            (
                ("グロー".to_string(), "グローの項目0".to_string()),
                FakeItemGroup::Member(group.clone()),
            ),
            (
                ("グロー".to_string(), "グローの項目2".to_string()),
                FakeItemGroup::Unavailable,
            ),
        ],
        ..FakeHost::new()
    };

    assert_eq!(
        host.effect_item_group("グロー", "グローの項目0").unwrap(),
        Some(group)
    );
    assert_eq!(
        host.effect_item_group("グロー", "グローの項目3").unwrap(),
        None
    );
    assert!(matches!(
        host.effect_item_group("グロー", "グローの項目2"),
        Err(ReadError::Sdk {
            operation: "get_effect_item_group_names"
        })
    ));
}
