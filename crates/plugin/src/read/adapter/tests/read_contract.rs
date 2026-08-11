//! 全 read operation に共通する契約の統合テスト。

use super::*;

#[test]
fn each_operation_enters_the_read_section_at_most_once() {
    fn entries(adapter: &HostReadAdapter<FakeHost>) -> usize {
        adapter
            .host
            .calls()
            .iter()
            .filter(|call| **call == "enter_read_section")
            .count()
    }

    let edit_info = adapter();
    edit_info.get_edit_info().unwrap();
    assert_eq!(entries(&edit_info), 1, "get_edit_info");

    let current_scene = adapter();
    current_scene.get_current_scene().unwrap();
    assert_eq!(entries(&current_scene), 1, "get_current_scene");

    let layers = adapter();
    layers.list_layers(0).unwrap();
    assert_eq!(entries(&layers), 1, "list_layers");

    let objects = adapter();
    objects.list_objects_page(0, None).unwrap();
    assert_eq!(entries(&objects), 1, "list_objects");

    let object = adapter();
    let selector = sample_selector(&object);
    object.get_object(&selector).unwrap();
    assert_eq!(entries(&object), 1, "get_object");

    // effect のカタログは編集ハンドルから直接得られ、参照区間を必要としない。
    let effects = adapter();
    effects.list_available_effects_page(None).unwrap();
    assert_eq!(entries(&effects), 0, "list_available_effects");

    // 設定項目の列挙も説明の取得も編集ハンドルの外側で完結する。
    let described = adapter();
    described
        .describe_effects(&describe_params(&["グロー", "ぼかし"]))
        .unwrap();
    assert_eq!(entries(&described), 0, "describe_effects");

    // フォントとモジュールの列挙も編集ハンドルの機能である。
    let fonts = adapter();
    fonts.list_fonts().unwrap();
    assert_eq!(entries(&fonts), 0, "list_fonts");

    let modules = adapter();
    modules.list_modules(None).unwrap();
    assert_eq!(entries(&modules), 0, "list_modules");

    // パレットは色の取得に区間が要る。名前の列挙も同じ区間の内側で行うため、
    // 入る回数は 1 度で足りる。
    let palettes = adapter();
    palettes.list_palettes_page().unwrap();
    assert_eq!(entries(&palettes), 1, "list_palettes");
}

#[test]
fn read_results_do_not_expose_handles() {
    let adapter = adapter();
    let selector = sample_selector(&adapter);
    let mut documents = vec![
        serde_json::to_string(&adapter.get_edit_info().unwrap()).unwrap(),
        serde_json::to_string(&adapter.get_object(&selector).unwrap()).unwrap(),
        serde_json::to_string(&adapter.list_objects_page(0, None).unwrap().items).unwrap(),
        serde_json::to_string(&adapter.list_layers(0).unwrap().items).unwrap(),
    ];
    documents.push(serde_json::to_string(&adapter.get_current_scene().unwrap().0).unwrap());
    documents.push(serde_json::to_string(&adapter.list_fonts().unwrap().items).unwrap());
    documents.push(serde_json::to_string(&adapter.list_palettes_page().unwrap()).unwrap());
    documents.push(serde_json::to_string(&adapter.list_modules(None).unwrap().items).unwrap());
    // 選択の取得はハンドルを 2 段で受け取る唯一の読み取りである。3 件の
    // 選択とフォーカスを持つホストで確かめる。
    let selection = adapter_with(|_| selecting_host());
    documents.push(serde_json::to_string(&selection.get_selection_page(0).unwrap()).unwrap());

    for document in documents {
        let lowered = document.to_lowercase();
        for forbidden in ["handle", "pointer", "0x"] {
            assert!(
                !lowered.contains(forbidden),
                "{forbidden} が応答に含まれます: {document}"
            );
        }
    }
}
