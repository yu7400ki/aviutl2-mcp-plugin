//! 同梱する skill のツリー。
//!
//! **バイナリへ取り込む。** 走査を持たないのは、配布物が
//! `Plugin\aviutl2-mcp-plugin.aux2` と `Plugin\aviutl2-mcp-server.exe` の 2 つで
//! あり、markdown を配る 3 つ目が無いためである。プラグインを導入すること自体が
//! skill の入手になる（[`crate::item_facets`] の基底の表と同じ形）。
//!
//! 副次的に、skill・manifest・server exe・plugin が単一の版数で動く。skill だけを
//! 直しても plugin の version が上がる。

/// skill ツリーの全ファイル。
///
/// 対は「`skills/` の根からの相対パス」と「本文」である。区切りは `/` で書く
/// ——生成側が組み立てるのはツリー内の位置であり、`Path` の join がそこを
/// 埋める。
///
/// **一覧は手書きである。** `include_str!` はリテラルしか受け付けず、走査で
/// 集める経路はバイナリの中に無い。したがって data ディレクトリへ足したファイルが
/// 黙って配布から漏れ得る。**それを塞ぐのは
/// `the_embedded_tree_matches_the_files_on_disk` であり、この一覧の外に置いてある。**
/// 突き合わせは拡張子を問わない——`include_str!` が受けないファイルを skill へ
/// 置いたときも、素通りではなく検査の失敗として出る。
pub const SKILL_FILES: &[(&str, &str)] = &[
    (
        "aviutl2-editing/SKILL.md",
        include_str!("../../data/skills/aviutl2-editing/SKILL.md"),
    ),
    (
        "aviutl2-editing/references/layers.md",
        include_str!("../../data/skills/aviutl2-editing/references/layers.md"),
    ),
    (
        "aviutl2-editing/references/object-alias.md",
        include_str!("../../data/skills/aviutl2-editing/references/object-alias.md"),
    ),
    (
        "aviutl2-editing/references/workflows.md",
        include_str!("../../data/skills/aviutl2-editing/references/workflows.md"),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_skill_file_lives_under_a_directory_that_has_a_skill_md() {
        // plugin root 直下の `skills/` は、`SKILL.md` を持つ各ディレクトリが
        // 1 skill であると spec が定めている。埋め込みの側でも同じ形を保つ。
        let skills: Vec<&str> = SKILL_FILES
            .iter()
            .filter_map(|(path, _)| path.strip_suffix("/SKILL.md"))
            .collect();
        assert!(!skills.is_empty(), "SKILL.md が 1 つもありません");
        for (path, body) in SKILL_FILES {
            assert!(
                skills
                    .iter()
                    .any(|skill| path.starts_with(&format!("{skill}/"))),
                "{path} が SKILL.md を持つディレクトリの下にありません"
            );
            assert!(!body.trim().is_empty(), "{path} が空です");
        }
    }
}
