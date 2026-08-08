//! skill を同梱した agent plugin の生成。
//!
//! `%LOCALAPPDATA%\AviUtl2Mcp` の下へ marketplace・plugin manifest・mcp 設定・
//! skill ツリーを書き出し、server の実行体を plugin root へ複製する。
//!
//! # なぜ plugin が書くのか
//!
//! 書ける主体は 2 つしか無い。server は**鶏と卵になる**——server を起動するのは
//! クライアントであり、起動するには marketplace が既に在る必要がある。3 つ目の
//! 実行体（インストーラ）を配る案は、署名・実行の入口・利用者の手順のいずれをも
//! 増やす割に、書くのは数 KB の JSON と markdown である。**加えて、
//! `%LOCALAPPDATA%\AviUtl2Mcp` のルートを保護 DACL 付きで作るのは plugin であり、
//! 保護を知らないインストーラが先にルートを作れば instance の登録ごと止まる。**
//!
//! plugin は必要な条件を既に全て満たしている——保護ディレクトリを作る口、
//! バイト列を持ち運ぶ手段（`include_str!`）、壊れかけの中間状態を残さない
//! 書き込み、起動ごとに走る契機、利用者の同意を預かる設定画面。**新しい機構を
//! 1 つも要さない。**
//!
//! # 同意が要る
//!
//! **既定では 1 バイトも書かない**（[`aviutl2_mcp_core::settings::AgentPluginSettings`]）。
//! 生成物が AviUtl2 の外側——別のアプリが読む設定——に効くためである。
//! **撤回できない opt-in は opt-in ではない**ため、切り替えを倒したぶんは消す。
//!
//! # 分け方
//!
//! [`plan`] は設定を受け取ってファイル名と内容の対を返すだけの純粋な関数で
//! ある。`%LOCALAPPDATA%` へ書かずに内容を検査できる。書き出しと削除は
//! [`install`] にあり、そちらが触れるのは [`plan`] が返した列挙だけである。

pub mod manifest;
pub mod skills;

mod install;

pub use install::sync;
#[cfg(test)]
pub(crate) use install::test_hook;

use aviutl2_mcp_core::settings::AgentPluginSettings;
use std::collections::BTreeSet;

/// plugin root の、基底からの相対パス。
pub const PLUGIN_ROOT: &str = "plugins/aviutl2";

/// 複製元にも複製先にも使う server の実行体の名前。
///
/// `aviutl2.toml` は plugin と server を同じ `Plugin\` へ置く。**複製元は
/// 自 DLL のディレクトリの中にある。**
pub const SERVER_EXECUTABLE: &str = "aviutl2-mcp-server.exe";

/// Claude Code 方言の marketplace。
const CLAUDE_MARKETPLACE: &str = ".claude-plugin/marketplace.json";
/// Claude Code 方言の plugin manifest。
const CLAUDE_PLUGIN: &str = "plugins/aviutl2/.claude-plugin/plugin.json";
/// Claude Code 方言の mcp 設定。
const CLAUDE_MCP: &str = "plugins/aviutl2/.mcp.json";
/// agent-plugins.org 方言の marketplace。
const SPEC_MARKETPLACE: &str = ".agents/plugins/marketplace.json";
/// agent-plugins.org 方言の plugin manifest。
const SPEC_PLUGIN: &str = "plugins/aviutl2/plugin.json";
/// agent-plugins.org 方言の mcp 設定。
const SPEC_MCP: &str = "plugins/aviutl2/mcp.json";
/// server の実行体の複製先。
const SERVER_COPY: &str = "plugins/aviutl2/bin/aviutl2-mcp-server.exe";
/// 生成物であることを述べる文書。
const README: &str = "README.md";
/// skill ツリーの根。**plugin root 直下に固定であると spec が定めている。**
const SKILLS_ROOT: &str = "plugins/aviutl2/skills";

/// 生成するファイル 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFile {
    /// 基底からの相対パス。区切りは `/`。
    pub path: String,
    /// 本文。
    pub contents: String,
}

/// ある設定の下で何を書き、何を消すか。
///
/// **走査を持たない。** 消す対象は生成器が生成したパスの列挙だけであり、
/// ディレクトリを辿って消す経路も、ルートを再帰削除する経路も無い。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationPlan {
    files: Vec<GeneratedFile>,
    executable: Option<String>,
}

impl GenerationPlan {
    /// 内容ごと書き出すファイル。
    pub fn files(&self) -> &[GeneratedFile] {
        &self.files
    }

    /// server の実行体を複製する先。生成しないなら `None`。
    pub fn executable(&self) -> Option<&str> {
        self.executable.as_deref()
    }

    /// 1 バイトも書かないか。
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.executable.is_none()
    }

    /// 書き出すパスの全体。
    pub fn written_paths(&self) -> BTreeSet<String> {
        self.files
            .iter()
            .map(|file| file.path.clone())
            .chain(self.executable.clone())
            .collect()
    }

    /// 消すパスの全体。
    ///
    /// **全ての切り替えを立てたときに書くものから、いま書くものを引いた差で
    /// ある。** 走査で集めないため、生成器が作らないファイルは決して消えない。
    pub fn removed_paths(&self) -> BTreeSet<String> {
        let written = self.written_paths();
        every_generated_path()
            .into_iter()
            .filter(|path| !written.contains(path))
            .collect()
    }

    /// 空なら消してよいディレクトリ。**深い順に並ぶ。**
    ///
    /// 書き出すパスの祖先は含まない。書いた直後に消しに行っても失敗するだけで
    /// あり、無変更のディレクトリの更新時刻を動かす理由が無い。
    pub fn pruned_directories(&self) -> Vec<String> {
        let kept: BTreeSet<String> = self
            .written_paths()
            .iter()
            .flat_map(|path| ancestors(path))
            .collect();
        let mut directories: Vec<String> = every_generated_path()
            .iter()
            .flat_map(|path| ancestors(path))
            .filter(|directory| !kept.contains(directory))
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect();
        // 深い順に並べる。親を先に消そうとしても中身が残っていて消えない。
        directories.sort_by(|a, b| depth(b).cmp(&depth(a)).then_with(|| a.cmp(b)));
        directories
    }
}

/// パスの祖先ディレクトリを、基底の直下まで集める。
fn ancestors(path: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = path;
    while let Some((parent, _)) = rest.rsplit_once('/') {
        found.push(parent.to_string());
        rest = parent;
    }
    found
}

/// 基底から数えた深さ。
fn depth(path: &str) -> usize {
    path.matches('/').count()
}

/// 設定の下で生成するものを決める。
///
/// **有効な方言が 0 なら何も書かない。`skill` だけを立てても書かない。**
/// skill は plugin root の中にあって初めて発見されるため、manifest の無い
/// `skills/` を置いても、それを見つける client が居ない。
///
/// この規則の利点は、矛盾する状態が生まれないことである。「同意しているのに
/// 何も生成されない」は `generate` を倒したのと同じ結果になり、`generate` と
/// 内訳の間に整合性の検査が要らない。
pub fn plan(settings: &AgentPluginSettings) -> GenerationPlan {
    let claude = settings.generate && settings.claude;
    let spec = settings.generate && settings.agent_plugins;
    if !claude && !spec {
        return GenerationPlan {
            files: Vec::new(),
            executable: None,
        };
    }

    let mut files = Vec::new();
    if claude {
        files.push(file(CLAUDE_MARKETPLACE, manifest::claude_marketplace()));
        files.push(file(CLAUDE_PLUGIN, manifest::claude_plugin()));
        files.push(file(CLAUDE_MCP, manifest::claude_mcp()));
    }
    if spec {
        files.push(file(SPEC_MARKETPLACE, manifest::spec_marketplace()));
        files.push(file(SPEC_PLUGIN, manifest::spec_plugin()));
        files.push(file(SPEC_MCP, manifest::spec_mcp()));
    }
    files.push(file(README, manifest::readme()));
    if settings.skill {
        for (path, body) in skills::SKILL_FILES {
            files.push(file(&format!("{SKILLS_ROOT}/{path}"), (*body).to_string()));
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    GenerationPlan {
        files,
        executable: Some(SERVER_COPY.to_string()),
    }
}

fn file(path: &str, contents: String) -> GeneratedFile {
    GeneratedFile {
        path: path.to_string(),
        contents,
    }
}

/// 生成器が作り得るパスの全体。
///
/// **消す対象はここから引いた差だけである。** 走査で集めないことが、
/// `settings.json`・`instances`・`artifacts`・`render` に触れないことの根拠に
/// なっている。
fn every_generated_path() -> BTreeSet<String> {
    plan(&AgentPluginSettings {
        generate: true,
        claude: true,
        agent_plugins: true,
        skill: true,
    })
    .written_paths()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全ての切り替えを立てた設定。
    fn all_on() -> AgentPluginSettings {
        AgentPluginSettings {
            generate: true,
            claude: true,
            agent_plugins: true,
            skill: true,
        }
    }

    /// 16 通りを回すための、bit から設定への写し。
    fn toggles(bits: u8) -> AgentPluginSettings {
        AgentPluginSettings {
            generate: bits & 1 != 0,
            claude: bits & 2 != 0,
            agent_plugins: bits & 4 != 0,
            skill: bits & 8 != 0,
        }
    }

    /// §7.3 の表を検査の側へ写したもの。**生成側の実装を参照しない。**
    fn expected_paths(settings: &AgentPluginSettings) -> BTreeSet<String> {
        let claude = settings.generate && settings.claude;
        let spec = settings.generate && settings.agent_plugins;
        let mut expected: BTreeSet<String> = BTreeSet::new();
        if !claude && !spec {
            return expected;
        }
        if claude {
            expected.insert(".claude-plugin/marketplace.json".to_string());
            expected.insert("plugins/aviutl2/.claude-plugin/plugin.json".to_string());
            expected.insert("plugins/aviutl2/.mcp.json".to_string());
        }
        if spec {
            expected.insert(".agents/plugins/marketplace.json".to_string());
            expected.insert("plugins/aviutl2/plugin.json".to_string());
            expected.insert("plugins/aviutl2/mcp.json".to_string());
        }
        expected.insert("plugins/aviutl2/bin/aviutl2-mcp-server.exe".to_string());
        expected.insert("README.md".to_string());
        if settings.skill {
            for (path, _) in skills::SKILL_FILES {
                expected.insert(format!("plugins/aviutl2/skills/{path}"));
            }
        }
        expected
    }

    #[test]
    fn every_combination_of_the_four_toggles_writes_the_documented_paths() {
        for bits in 0..16u8 {
            let settings = toggles(bits);
            assert_eq!(
                plan(&settings).written_paths(),
                expected_paths(&settings),
                "切り替え {settings:?} で書くパスが表と一致しません"
            );
        }
    }

    #[test]
    fn no_enabled_dialect_writes_nothing_even_with_the_skill_on() {
        // skill は plugin root の中にあって初めて発見される。manifest の無い
        // `skills/` を置いても、それを見つける client が居ない。
        for settings in [
            AgentPluginSettings {
                generate: false,
                claude: true,
                agent_plugins: true,
                skill: true,
            },
            AgentPluginSettings {
                generate: true,
                claude: false,
                agent_plugins: false,
                skill: true,
            },
        ] {
            let plan = plan(&settings);
            assert!(plan.is_empty(), "{settings:?} で書き出しが生じました");
            assert!(plan.written_paths().is_empty(), "{settings:?}");
            assert_eq!(
                plan.removed_paths(),
                every_generated_path(),
                "{settings:?} で消し残しが出ます"
            );
        }
    }

    #[test]
    fn what_a_toggle_stops_writing_is_exactly_what_it_starts_removing() {
        // **残骸が出ないことと、余分を消さないことを同時に見る。** 片側だけの
        // 検査では、消し過ぎと消し足りないのどちらかが通ってしまう。
        for bits in 0..16u8 {
            for toggle in [1u8, 2, 4, 8] {
                let on = toggles(bits | toggle);
                let off = toggles(bits & !toggle);
                let written_on = plan(&on).written_paths();
                let written_off = plan(&off).written_paths();
                let removed_on = plan(&on).removed_paths();
                let removed_off = plan(&off).removed_paths();

                let stopped: BTreeSet<_> = written_on.difference(&written_off).cloned().collect();
                let started: BTreeSet<_> = removed_off.difference(&removed_on).cloned().collect();
                assert_eq!(
                    stopped, started,
                    "切り替え {toggle} を倒したときに消す集合が、立てていたときに書く集合と違います"
                );
            }
        }
    }

    #[test]
    fn writing_and_removing_never_overlap_and_together_cover_everything() {
        for bits in 0..16u8 {
            let plan = plan(&toggles(bits));
            let written = plan.written_paths();
            let removed = plan.removed_paths();
            assert!(
                written.intersection(&removed).next().is_none(),
                "書くものを同時に消そうとしています: {bits}"
            );
            let union: BTreeSet<_> = written.union(&removed).cloned().collect();
            assert_eq!(union, every_generated_path(), "{bits}");
        }
    }

    #[test]
    fn nothing_the_plugin_needs_is_ever_removed() {
        // 保護ルートには消してはならないものが同居している。**消す対象が
        // 生成した列挙に限られることの帰結を、名指しで固定する。**
        let forbidden = ["settings.json", "instances", "artifacts", "render"];
        for bits in 0..16u8 {
            let plan = plan(&toggles(bits));
            for path in plan
                .removed_paths()
                .iter()
                .chain(&plan.pruned_directories())
            {
                let head = path.split('/').next().unwrap_or(path);
                assert!(
                    !forbidden.contains(&head),
                    "生成器が {path} を消そうとしています"
                );
                assert!(!path.is_empty(), "基底そのものを消そうとしています");
                assert!(
                    !path.starts_with(".."),
                    "基底の外を消そうとしています: {path}"
                );
            }
        }
    }

    #[test]
    fn pruned_directories_go_deepest_first_and_spare_the_ones_still_in_use() {
        let without_skill = plan(&AgentPluginSettings {
            skill: false,
            ..all_on()
        });
        let pruned = without_skill.pruned_directories();
        assert!(
            pruned.contains(&"plugins/aviutl2/skills/aviutl2-editing/references".to_string()),
            "{pruned:?}"
        );
        assert!(
            !pruned.contains(&"plugins/aviutl2".to_string()),
            "まだ使っているディレクトリを消そうとしています: {pruned:?}"
        );
        let depths: Vec<usize> = pruned.iter().map(|path| depth(path)).collect();
        assert!(
            depths.windows(2).all(|pair| pair[0] >= pair[1]),
            "深い順に並んでいません: {pruned:?}"
        );

        // 全て倒せば、生成したディレクトリは 1 つ残らず候補になる。
        let empty = plan(&AgentPluginSettings::default());
        for directory in [
            ".claude-plugin",
            ".agents",
            ".agents/plugins",
            "plugins",
            "plugins/aviutl2",
            "plugins/aviutl2/bin",
            "plugins/aviutl2/skills",
        ] {
            assert!(
                empty.pruned_directories().contains(&directory.to_string()),
                "{directory} が消す候補にありません"
            );
        }
    }

    #[test]
    fn the_generated_paths_stay_under_the_plugin_root_they_declare() {
        assert_eq!(PLUGIN_ROOT, format!("plugins/{}", manifest::PLUGIN_NAME));
        for path in [
            CLAUDE_PLUGIN,
            CLAUDE_MCP,
            SPEC_PLUGIN,
            SPEC_MCP,
            SERVER_COPY,
        ] {
            assert!(
                path.starts_with(&format!("{PLUGIN_ROOT}/")),
                "{path} が plugin root の外にあります"
            );
        }
        assert_eq!(SKILLS_ROOT, format!("{PLUGIN_ROOT}/skills"));
        assert_eq!(
            SERVER_COPY,
            format!("{PLUGIN_ROOT}/bin/{SERVER_EXECUTABLE}")
        );
    }

    #[test]
    fn the_skill_tree_lands_directly_under_the_plugin_root() {
        // spec は `skills/` を plugin root 直下に固定し、`SKILL.md` を持つ直下の
        // 各ディレクトリを 1 skill と定めている。
        let plan = plan(&all_on());
        let skill_paths: Vec<&str> = plan
            .files()
            .iter()
            .filter_map(|file| file.path.strip_prefix(&format!("{SKILLS_ROOT}/")))
            .collect();
        assert!(!skill_paths.is_empty(), "skill が 1 つも載っていません");
        let mut roots: BTreeSet<&str> = BTreeSet::new();
        for path in &skill_paths {
            let (root, _) = path
                .split_once('/')
                .unwrap_or_else(|| panic!("skills の直下にファイルがあります: {path}"));
            roots.insert(root);
        }
        for root in &roots {
            assert!(
                skill_paths.contains(&format!("{root}/SKILL.md").as_str()),
                "{root} が SKILL.md を持っていません"
            );
        }
    }

    #[test]
    fn the_executable_is_copied_whenever_any_dialect_is_on() {
        for bits in 0..16u8 {
            let settings = toggles(bits);
            let plan = plan(&settings);
            let expected = settings.generate && (settings.claude || settings.agent_plugins);
            assert_eq!(
                plan.executable().is_some(),
                expected,
                "{settings:?} で実行体の複製の有無が表と違います"
            );
        }
    }
}
