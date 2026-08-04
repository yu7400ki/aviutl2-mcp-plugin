//! MCP tool の名前。
//!
//! 一覧を書き写さず、operation の一覧から導く。
//!
//! ```text
//! tool 名 = operation 名   （operation を持つもの）
//!         + list_instances （対応する operation を持たない 1 件）
//! ```
//!
//! 導出にすることで、operation を足したときに tool 名の一覧へ足し忘れる経路が
//! 構造的に無くなる。[`KnownOperation`] の網羅性は
//! [`crate::operation`] のテストが縛っている。写像は恒等であり、規則を
//! 書き間違える余地が無い。
//!
//! # 名前だけを持つ
//!
//! 表示名も説明も持たない。tool の説明は MCP server の tool 定義が唯一の出所で
//! あり、ここへ写すと同じ文章を 2 か所で管理することになる。

use crate::operation::{EditOperation, KnownOperation, ReadOperation, RenderOperation};

/// 無効化できない tool。
///
/// discovery と設定導線の入口であり、公開しなければ他の tool が必須で要求する
/// `instance_id` を得る手段が無くなる。読み手はこの名前が公開しない指定に
/// 含まれていても無視する。
pub const ALWAYS_ENABLED_TOOL: &str = "list_instances";

/// tool が属する族。
///
/// 切替の対象になる tool は、対応する operation の族でそのまま分かれる。設定
/// 画面はこの族で見出しを付けて並べる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFamily {
    /// 読み取り。
    Read,
    /// 編集。
    Edit,
    /// 描画。
    Render,
}

impl ToolFamily {
    /// 全 variant。
    ///
    /// 要素数と内容は `tool_family_all_is_exhaustive` テストで固定する。
    pub const ALL: [ToolFamily; 3] = [ToolFamily::Read, ToolFamily::Edit, ToolFamily::Render];

    /// operation が属する族。
    ///
    /// **`_` を使わない網羅 `match` である。** [`KnownOperation`] へ族を足すと
    /// 腕が足りずコンパイルが落ちるため、族を決めないまま新しい tool を公開する
    /// 状態にはならない。
    pub const fn of(operation: KnownOperation) -> Self {
        match operation {
            KnownOperation::Read(_) => ToolFamily::Read,
            KnownOperation::Edit(_) => ToolFamily::Edit,
            KnownOperation::Render(_) => ToolFamily::Render,
        }
    }

    /// 族に属する operation を並べる。
    pub fn operations(self) -> Vec<KnownOperation> {
        match self {
            ToolFamily::Read => ReadOperation::ALL
                .into_iter()
                .map(KnownOperation::Read)
                .collect(),
            ToolFamily::Edit => EditOperation::ALL
                .into_iter()
                .map(KnownOperation::Edit)
                .collect(),
            ToolFamily::Render => RenderOperation::ALL
                .into_iter()
                .map(KnownOperation::Render)
                .collect(),
        }
    }

    /// 族に属する tool 名を並べる。
    pub fn tool_names(self) -> impl Iterator<Item = String> {
        self.operations()
            .into_iter()
            .map(|operation| operation.as_str().to_string())
    }
}

/// 個別に切り替えられる tool の名前を族の順に並べる。
///
/// [`ALWAYS_ENABLED_TOOL`] は含まない。設定画面が切替の候補として並べるのは
/// この一覧である。
pub fn togglable_tool_names() -> impl Iterator<Item = String> {
    ToolFamily::ALL.into_iter().flat_map(ToolFamily::tool_names)
}

/// 切替の対象になり得る MCP tool 名の全体。
///
/// 切替の対象にならない [`ALWAYS_ENABLED_TOOL`] も含む。`tools/list` に載り得る
/// 名前はこれで尽きており、MCP server が登録した tool 名の集合と一致する。
pub fn all_tool_names() -> impl Iterator<Item = String> {
    std::iter::once(ALWAYS_ENABLED_TOOL.to_string()).chain(togglable_tool_names())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn tool_names_equal_the_operation_names() {
        for family in ToolFamily::ALL {
            let names: Vec<String> = family.tool_names().collect();
            let operations = family.operations();
            assert_eq!(names.len(), operations.len());
            for (name, operation) in names.iter().zip(operations) {
                assert_eq!(
                    name,
                    operation.as_str(),
                    "{operation:?} の tool 名が operation 名と一致していません"
                );
            }
        }
    }

    #[test]
    fn all_tool_names_contains_the_always_enabled_tool() {
        let names: BTreeSet<String> = all_tool_names().collect();
        assert!(
            names.contains(ALWAYS_ENABLED_TOOL),
            "{ALWAYS_ENABLED_TOOL} が一覧に含まれていません"
        );
    }

    #[test]
    fn the_always_enabled_tool_is_not_offered_for_toggling() {
        let togglable: BTreeSet<String> = togglable_tool_names().collect();
        assert!(!togglable.contains(ALWAYS_ENABLED_TOOL));
        assert_eq!(togglable.len() + 1, all_tool_names().count());
    }

    #[test]
    fn all_tool_names_has_no_duplicates() {
        let names: Vec<String> = all_tool_names().collect();
        let unique: BTreeSet<&String> = names.iter().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "tool 名が重複しています: {names:?}"
        );
    }

    /// [`ToolFamily::ALL`] が全 variant を含むことを固定する。
    ///
    /// 仕組みは `crate::operation` の同名のテスト群と同じである。
    #[test]
    fn tool_family_all_is_exhaustive() {
        fn assert_listed(family: ToolFamily) {
            match family {
                ToolFamily::Read | ToolFamily::Edit | ToolFamily::Render => {}
            }
            assert!(
                ToolFamily::ALL.contains(&family),
                "{family:?} が ToolFamily::ALL に含まれていません"
            );
        }

        assert_listed(ToolFamily::Read);
        assert_listed(ToolFamily::Edit);
        assert_listed(ToolFamily::Render);
        assert_eq!(ToolFamily::ALL.len(), 3);
    }

    #[test]
    fn each_family_lists_exactly_the_operations_that_belong_to_it() {
        for family in ToolFamily::ALL {
            for operation in family.operations() {
                assert_eq!(
                    ToolFamily::of(operation),
                    family,
                    "{operation:?} が {family:?} に並んでいますが族が一致しません"
                );
            }
        }
        assert_eq!(
            ToolFamily::Read.operations().len(),
            ReadOperation::ALL.len()
        );
        assert_eq!(
            ToolFamily::Edit.operations().len(),
            EditOperation::ALL.len()
        );
        assert_eq!(
            ToolFamily::Render.operations().len(),
            RenderOperation::ALL.len()
        );
    }

    #[test]
    fn the_families_cover_every_known_operation() {
        let listed: BTreeSet<&'static str> = ToolFamily::ALL
            .into_iter()
            .flat_map(ToolFamily::operations)
            .map(KnownOperation::as_str)
            .collect();
        let known: BTreeSet<&'static str> = ReadOperation::ALL
            .into_iter()
            .map(ReadOperation::as_str)
            .chain(EditOperation::ALL.into_iter().map(EditOperation::as_str))
            .chain(
                RenderOperation::ALL
                    .into_iter()
                    .map(RenderOperation::as_str),
            )
            .collect();
        assert_eq!(listed, known);
    }

    /// tool 名の集合に、接頭辞を持つ名前が 1 つも含まれないこと。
    ///
    /// 導出の結果だけを見る。写像が恒等であることは
    /// `tool_names_equal_the_operation_names` が別に固定しているため、
    /// この結果は導出を経由するすべての呼び出し側に自動で及ぶ。
    #[test]
    fn no_tool_name_carries_the_old_prefix() {
        for name in all_tool_names() {
            assert!(
                !name.starts_with("aviutl2_"),
                "{name} が古い接頭辞を持っています"
            );
        }
    }
}
