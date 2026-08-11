//! エイリアス・フォント・モジュール・パレットの一覧の統合テスト。

use super::*;

#[test]
fn list_object_aliases_reports_an_unavailable_directory_instead_of_panicking() {
    // 設定ハンドルが初期化されていない環境では、データディレクトリの解決が
    // panic で打ち切られる。捕捉層まで上げると「想定外の内部失敗」になり、
    // 要求元は他の tool も動かないものと読む。
    let adapter = adapter();
    let error = adapter
        .list_object_aliases(None, &default_page_window())
        .unwrap_err();

    assert!(matches!(error, ReadError::AliasDirectoryUnavailable));
    assert_eq!(
        error.error_code(),
        aviutl2_mcp_core::ErrorCode::UnsupportedOperation
    );
    // SDK を 1 度も呼ばない。準備状態の問い合わせも参照区間も通らない。
    assert!(adapter.host.calls().is_empty(), "SDK を呼びました");
}

#[test]
fn list_fonts_returns_every_registered_name() {
    let adapter = adapter();
    let snapshot = adapter.list_fonts().unwrap();
    assert_eq!(snapshot.items, fake_fonts());
}

#[test]
fn list_modules_filters_by_type() {
    let adapter = adapter();
    let all = adapter.list_modules(None).unwrap();
    assert_eq!(all.items.len(), fake_modules().len());

    let inputs = adapter
        .list_modules(Some(&ModuleType::PluginInput))
        .unwrap();
    assert_eq!(inputs.items.len(), 1);
    assert_eq!(inputs.items[0].name, "入力プラグイン");

    let none = adapter
        .list_modules(Some(&ModuleType::ScriptCamera))
        .unwrap();
    assert!(none.items.is_empty());
}

#[test]
fn list_palettes_returns_the_fixed_number_of_colors() {
    let adapter = adapter();
    let result = adapter.list_palettes_page().unwrap();
    assert_eq!(result.items.len(), fake_palette_names().len());
    for palette in &result.items {
        assert_eq!(
            palette.colors.len(),
            PALETTE_COLOR_COUNT,
            "{} の色数",
            palette.name
        );
        assert_eq!(palette.colors.len(), 64);
    }
}

#[test]
fn list_palettes_reads_the_colors_of_the_page_only() {
    // 色は 1 件あたり 64 個ある。応答へ載せない分まで読むと、参照区間の
    // 保持時間が要求ページではなく登録数で決まってしまう。
    let adapter = adapter();
    let result = adapter
        .list_palettes(&page_request(1, 2, None).window())
        .unwrap();

    let read_colors = adapter
        .host
        .calls()
        .iter()
        .filter(|call| **call == "palette_colors")
        .count();
    assert_eq!(result.items.len(), 2);
    assert_eq!(read_colors, 2, "窓の外まで色を読んでいます");
    assert!(
        fake_palette_names().len() > 2,
        "全件と窓が同じ件数では読み過ぎを検出できません"
    );
}

#[test]
fn list_palettes_drops_only_the_palette_whose_colors_are_missing() {
    // 列挙が返した名前で情報が取れないのは異常だが、その 1 件のために一覧
    // 全体を落とさない。落としたことは総件数に現れる。
    let missing = "暖色".to_string();
    let adapter = adapter_with(|_| {
        let mut host = FakeHost::new();
        host.palettes_without_colors = vec![missing.clone()];
        host
    });
    let result = adapter.list_palettes_page().unwrap();

    let names: Vec<&str> = result
        .items
        .iter()
        .map(|palette| palette.name.as_str())
        .collect();
    assert!(!names.contains(&missing.as_str()), "{names:?}");
    assert_eq!(names.len(), fake_palette_names().len() - 1);
    assert_eq!(
        result.page.total_count as usize,
        fake_palette_names().len() - 1,
        "落とした件数が総件数に反映されていません"
    );
    assert_eq!(result.page.count as usize, names.len());
}

#[test]
fn list_palettes_returns_a_null_current_name_when_the_host_does_not_name_one() {
    // 現在のパレット名は付随情報である。取れないことで一覧を落とさない。
    let adapter = adapter_with(|_| {
        let mut host = FakeHost::new();
        host.current_palette = None;
        host
    });
    let result = adapter.list_palettes_page().unwrap();

    assert_eq!(result.current, None);
    assert_eq!(result.items.len(), fake_palette_names().len());
}

#[test]
fn list_palettes_names_the_colors_of_the_palette_it_reports() {
    // 名前と色の組を取り違える実装は、色が名前の関数であることで現れる。
    let adapter = adapter();
    let result = adapter.list_palettes_page().unwrap();
    for palette in &result.items {
        assert_eq!(
            palette.colors,
            fake_palette_colors(&palette.name),
            "{} の色が別のパレットのものです",
            palette.name
        );
    }
}
