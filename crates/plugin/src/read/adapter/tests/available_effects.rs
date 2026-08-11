//! 利用可能な effect の一覧の統合テスト。

use super::*;

#[test]
fn list_available_effects_returns_catalog() {
    let adapter = adapter();
    let result = adapter.list_available_effects_page(None).unwrap();
    assert_eq!(result.items.len(), fake_catalog().len());
    assert_eq!(result.page.total_count as usize, fake_catalog().len());
}

#[test]
fn list_available_effects_filters_by_type() {
    let adapter = adapter();
    let result = adapter
        .list_available_effects_page(Some(&EffectType::Input))
        .unwrap();
    let names: Vec<&str> = result.items.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["動画ファイル", "画像ファイル"]);

    let none = adapter
        .list_available_effects_page(Some(&EffectType::Transition))
        .unwrap();
    assert!(none.items.is_empty());
}

#[test]
fn list_available_effects_leaves_the_description_null_without_a_source() {
    // 説明の供給源はホストが同梱するファイルだけである。それを読めない環境
    // では説明が出ないが、一覧そのものは働き続ける。
    let adapter = adapter();
    let result = adapter.list_available_effects_page(None).unwrap();

    assert_eq!(result.items.len(), fake_catalog().len());
    for effect in &result.items {
        assert!(
            effect.description.is_none(),
            "{} に説明が付きました",
            effect.name
        );
    }
    assert_eq!(result.items[0].name, "ぼかし");
    assert_eq!(result.items[0].item_count, 1);
    assert_eq!(result.items[0].effect_type, EffectType::Filter);
}

#[test]
fn list_available_effects_carries_the_description_of_each_effect() {
    // 説明を持つ effect にはその説明が、持たない effect には null が載る。
    // 説明は全文で運ぶ——2 行目に発見の鍵がある説明が実在する。
    let glow = "光を拡散させます\n発光量を指定します";
    let adapter = adapter_with(|_| FakeHost {
        help: vec![
            (
                "グロー".to_string(),
                effect_help(Some(glow), &[("グローの項目0", "拡散の量です")]),
            ),
            (
                "標準描画".to_string(),
                effect_help(Some("描画のしかたを決めます"), &[]),
            ),
        ],
        ..FakeHost::new()
    });
    let result = adapter.list_available_effects_page(None).unwrap();

    let described: Vec<(&str, Option<&str>)> = result
        .items
        .iter()
        .map(|effect| (effect.name.as_str(), effect.description.as_deref()))
        .collect();
    assert_eq!(
        described,
        vec![
            ("ぼかし", None),
            ("動画ファイル", None),
            ("グロー", Some(glow)),
            ("画像ファイル", None),
            ("標準描画", Some("描画のしかたを決めます")),
        ],
        "説明が別の effect へ付いているか、落ちています"
    );
}

#[test]
fn list_available_effects_asks_for_the_description_only_for_the_requested_page() {
    // 説明の取得も窓の分だけである。供給源の引き直しを応答へ載せない
    // effect まで広げない。
    let adapter = adapter();
    let page = page_request(3, 1, None).window();
    adapter.list_available_effects(None, &page).unwrap();

    let asked = adapter
        .host
        .calls()
        .iter()
        .filter(|call| **call == "effect_help")
        .count();
    assert_eq!(asked, 1, "窓の外の effect について説明を引いています");
}

#[test]
fn list_available_effects_reports_the_item_count_of_each_effect() {
    let adapter = adapter();
    let result = adapter.list_available_effects_page(None).unwrap();
    let counts: Vec<usize> = result
        .items
        .iter()
        .map(|effect| effect.item_count)
        .collect();
    assert_eq!(
        counts,
        fake_catalog()
            .iter()
            .map(|entry| entry.items.len())
            .collect::<Vec<usize>>()
    );
}

#[test]
fn list_available_effects_counts_items_only_for_the_requested_page() {
    // 項目の列挙は effect ごとの呼び出しである。全件について呼ぶと、費用が
    // 要求ページではなく登録数で決まる。
    let adapter = adapter();
    let page = page_request(1, 2, None).window();
    let result = adapter.list_available_effects(None, &page).unwrap();

    let names: Vec<String> = result.items.iter().map(|e| e.name.clone()).collect();
    assert_eq!(
        names,
        vec!["動画ファイル".to_string(), "グロー".to_string()]
    );
    assert_eq!(
        adapter.host.item_count_queries(),
        names,
        "窓の外の effect について設定項目を数えています"
    );
    assert!(
        adapter.host.item_count_queries().len() < fake_catalog().len(),
        "カタログの全件について設定項目を数えています"
    );
}

#[test]
fn list_available_effects_counts_items_only_for_the_filtered_page() {
    // 絞り込みは窓より先に効く。絞る前の並びで窓を切ると、応答へ載らない
    // effect の項目を数えることになる。
    let adapter = adapter();
    let page = page_request(0, 1, None).window();
    let result = adapter
        .list_available_effects(Some(&EffectType::Filter), &page)
        .unwrap();

    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].name, "ぼかし");
    assert_eq!(result.page.total_count, 2, "絞り込み後の総件数を返します");
    assert_eq!(
        adapter.host.item_count_queries(),
        vec!["ぼかし".to_string()],
        "窓の外の effect について設定項目を数えています"
    );
}
