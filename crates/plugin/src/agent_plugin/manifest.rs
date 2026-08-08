//! 6 つの manifest と、生成物であることを述べる `README.md` の本文。
//!
//! **1 つの源から 6 つを作る。** `marketplace.json` / `plugin.json` / `mcp.json`
//! がそれぞれ 2 方言ぶん在り、名前・説明・版・所有者が一致していなければ
//! ならない。手で保てば必ずずれる。
//!
//! **二重化は減らせない。** Claude Code は agent-plugins.org に追従しておらず、
//! `plugin.json` も `mcp.json` も読まずに `.claude-plugin\` 以下の自分の形だけを
//! 見る。spec が用意する `extensions` は、それを読む client にしか効かない。
//! 2 方言は歩み寄らない別々の形であり、両方を満たすには両方を書くしかない。
//! **したがって二重化を無くすのではなく、生成に閉じ込める。**
//!
//! # spec が縛るもの
//!
//! `command` は**単一の実行可能トークン**であり、bare な実行体名か `./` で
//! 始まる plugin 相対パスのいずれかに限られる。**placeholder 展開も効かない。**
//! `${PLUGIN_ROOT}` が展開されるのは `args`・`env` の値・`cwd` だけであり、
//! 実行体そのものの位置を渡す口はそこには無い。**したがって exe は plugin root
//! の中に要る**（[`super::SERVER_COPY`]）。
//!
//! `env` に `PLUGIN_ROOT` / `PLUGIN_DATA` を置いてはならない。ここが書き出す
//! mcp 設定は `env` を 1 つも持たない。

use serde_json::{Value, json};

/// marketplace の名前。
pub const MARKETPLACE_NAME: &str = "aviutl2-mcp";

/// marketplace の説明。
pub const MARKETPLACE_DESCRIPTION: &str =
    "AviUtl ExEdit2 の編集を AI エージェントから扱うためのプラグイン";

/// plugin の名前。
///
/// spec は 1〜64 文字の小英数・ハイフン・ピリオドで、`--` と `..` を含まず
/// 英数で始まり英数で終わることを求める。
pub const PLUGIN_NAME: &str = "aviutl2";

/// plugin の説明。
pub const PLUGIN_DESCRIPTION: &str = "AviUtl ExEdit2 の編集内容を読み取り、変更する MCP server";

/// 所有者の名前。
pub const OWNER_NAME: &str = "yu7400ki";

/// リポジトリ。
pub const REPOSITORY: &str = "https://github.com/yu7400ki/aviutl2-mcp-plugin";

/// marketplace の分類。**client の慣習であり spec の対象外である。**
pub const CATEGORY: &str = "Media";

/// 検索語。
pub const KEYWORDS: &[&str] = &["aviutl2", "video-editing", "windows"];

/// 宣言する Agent Plugins の spec 版。
///
/// **`plugin.json` と mcp 設定は同じ版を宣言する。** 双方の `$schema` を
/// この 1 つから組み立てる。
pub const SPEC_VERSION: &str = "1.0.0";

/// 版・ライセンスの出所は `Cargo.toml` の `workspace.package` である。
///
/// **説明と名前はここに無い。** `CARGO_PKG_DESCRIPTION` が述べるのは crate の
/// 説明（`.aux2` そのもの）であり、manifest が要るのは marketplace と plugin で
/// 別々の 2 つの説明である。1 つの欄から 2 つは引けない。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// ライセンス。
pub const LICENSE: &str = env!("CARGO_PKG_LICENSE");

/// spec の schema の URL を組み立てる。
fn schema_url(name: &str) -> String {
    format!("https://agent-plugins.org/schemas/{SPEC_VERSION}/{name}.schema.json")
}

/// JSON を 1 ファイル分の本文にする。
fn document(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("Value からの直列化は失敗しない");
    text.push('\n');
    text
}

/// 2 方言に共通する plugin.json の本体。
fn plugin_body() -> serde_json::Map<String, Value> {
    json!({
        "name": PLUGIN_NAME,
        "version": VERSION,
        "description": PLUGIN_DESCRIPTION,
        "author": { "name": OWNER_NAME },
        "repository": REPOSITORY,
        "license": LICENSE,
        "keywords": KEYWORDS,
    })
    .as_object()
    .expect("object を組み立てている")
    .clone()
}

/// `.claude-plugin\marketplace.json`。
///
/// **spec の対象外である。** 「インストール元・レジストリ・マーケットプレイス」は
/// client 所有の振る舞いだと spec が明記しており、この形に保証は無い。変わり得る
/// 前提で扱い、変わったら生成側の 1 か所を直す。
pub fn claude_marketplace() -> String {
    document(&json!({
        "name": MARKETPLACE_NAME,
        "description": MARKETPLACE_DESCRIPTION,
        "owner": { "name": OWNER_NAME },
        "plugins": [
            {
                "name": PLUGIN_NAME,
                "description": PLUGIN_DESCRIPTION,
                "source": format!("./{}", super::PLUGIN_ROOT),
                "category": CATEGORY,
            }
        ],
    }))
}

/// `.agents\plugins\marketplace.json`。
///
/// Claude Code 方言との違いは `source` が object であることと `policy` を持つ
/// ことだけである。
pub fn spec_marketplace() -> String {
    document(&json!({
        "name": MARKETPLACE_NAME,
        "description": MARKETPLACE_DESCRIPTION,
        "owner": { "name": OWNER_NAME },
        "plugins": [
            {
                "name": PLUGIN_NAME,
                "description": PLUGIN_DESCRIPTION,
                "source": { "source": "local", "path": format!("./{}", super::PLUGIN_ROOT) },
                "policy": { "installation": "AVAILABLE", "authentication": "ON_USE" },
                "category": CATEGORY,
            }
        ],
    }))
}

/// `plugins\aviutl2\.claude-plugin\plugin.json`。
///
/// **`$schema` を持たない。** spec 版だけが持つ。
pub fn claude_plugin() -> String {
    document(&Value::Object(plugin_body()))
}

/// `plugins\aviutl2\plugin.json`。
pub fn spec_plugin() -> String {
    let mut body = plugin_body();
    body.insert("$schema".to_string(), json!(schema_url("plugin")));
    document(&Value::Object(body))
}

/// `plugins\aviutl2\.mcp.json`。
///
/// **`${CLAUDE_PLUGIN_ROOT}` を使う。** spec の `command` は placeholder 展開を
/// 受けないが、Claude Code 方言はその制約に従わない別の慣習である。
pub fn claude_mcp() -> String {
    document(&json!({
        "mcpServers": {
            PLUGIN_NAME: {
                "command": format!("${{CLAUDE_PLUGIN_ROOT}}/bin/{}", super::SERVER_EXECUTABLE),
            }
        }
    }))
}

/// `plugins\aviutl2\mcp.json`。
///
/// **`command` は `./` で始まる plugin 相対パスである。** 絶対パスを書けば
/// client 側で静かに落ちる。`cwd` を省くため、client は plugin root を作業
/// ディレクトリにする。
pub fn spec_mcp() -> String {
    document(&json!({
        "$schema": schema_url("mcp"),
        "mcpServers": {
            PLUGIN_NAME: {
                "type": "stdio",
                "command": format!("./bin/{}", super::SERVER_EXECUTABLE),
            }
        }
    }))
}

/// ツリーの直下に置く `README.md`。
///
/// **JSON にはコメントを書けない。** 生成物であることと、手で編集しても次回の
/// 起動で戻ることを述べる場所はここしか無い。
pub fn readme() -> String {
    format!(
        r#"# AviUtl2 MCP の agent plugin

このディレクトリの次のものは **AviUtl2 MCP プラグインが生成している**。

- `.claude-plugin/`
- `.agents/`
- `plugins/`
- この `README.md`

**手で編集しても、次に AviUtl2 を起動した時点で書き戻される。** 内容の源は
プラグインのバイナリの中にしかなく、ここに置かれたファイルはその写しである。
`plugins/{plugin}/bin/{exe}` も同じで、プラグイン本体の隣にある実行体と
起動ごとに照合され、違えば複製し直される。

生成をやめるには、AviUtl2 の「設定」→「{menu}」→「{page}」で
「{consent}」を外す。**外せば上の 4 つは消える。**

生成の対象ではないもの（プラグインが別の目的で使っており、設定を外しても
消えない）:

- `settings.json` — AviUtl2 MCP の共有設定
- `instances/` — 起動中のインスタンスの一覧
- `artifacts/` `render/` — 一時的な成果物

version {version}
"#,
        plugin = PLUGIN_NAME,
        exe = super::SERVER_EXECUTABLE,
        menu = crate::settings_ui::MENU_NAME,
        page = crate::settings_ui::AGENT_PLUGIN_PAGE,
        consent = crate::settings_ui::form::AgentPluginToggle::Generate.label(),
        version = VERSION,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成した本文を JSON として読み直す。
    fn parse(text: &str) -> Value {
        assert!(text.ends_with('\n'), "本文が改行で終わっていません");
        serde_json::from_str(text).expect("生成した本文を JSON として読めません")
    }

    /// 6 つの manifest を (呼び名, 本文) で並べる。
    fn all() -> Vec<(&'static str, Value)> {
        vec![
            (
                ".claude-plugin/marketplace.json",
                parse(&claude_marketplace()),
            ),
            (
                ".agents/plugins/marketplace.json",
                parse(&spec_marketplace()),
            ),
            (
                "plugins/aviutl2/.claude-plugin/plugin.json",
                parse(&claude_plugin()),
            ),
            ("plugins/aviutl2/plugin.json", parse(&spec_plugin())),
            ("plugins/aviutl2/.mcp.json", parse(&claude_mcp())),
            ("plugins/aviutl2/mcp.json", parse(&spec_mcp())),
        ]
    }

    /// manifest が述べる plugin の名前・版・説明を、書いてあるものだけ集める。
    fn stated(value: &Value) -> Vec<(&'static str, String)> {
        let mut found = Vec::new();
        let mut state = |entry: &Value| {
            for key in ["name", "version", "description"] {
                if let Some(text) = entry.get(key).and_then(Value::as_str) {
                    let key: &'static str = match key {
                        "name" => "name",
                        "version" => "version",
                        _ => "description",
                    };
                    found.push((key, text.to_string()));
                }
            }
        };
        // marketplace は自分の名前と説明も持つ。plugin について述べているのは
        // `plugins` の要素のほうである。
        match value.get("plugins").and_then(Value::as_array) {
            Some(plugins) => {
                assert_eq!(
                    plugins.len(),
                    1,
                    "marketplace が並べる plugin が 1 つではありません"
                );
                state(&plugins[0]);
            }
            None if value.get("mcpServers").is_some() => {}
            None => state(value),
        }
        found
    }

    #[test]
    fn every_manifest_states_the_same_name_version_and_description() {
        // 6 ファイルは名前・説明・版が一致していなければならない。**手で保てば
        // 必ずずれる。** ここで見るのは、生成が 1 つの源から作れていることである。
        let mut seen: std::collections::BTreeMap<&str, String> = std::collections::BTreeMap::new();
        let mut carriers = 0usize;
        for (name, value) in all() {
            let stated = stated(&value);
            if stated.is_empty() {
                continue;
            }
            carriers += 1;
            for (key, text) in stated {
                match seen.get(key) {
                    Some(known) => assert_eq!(known, &text, "{name} の {key} が食い違います"),
                    None => {
                        seen.insert(key, text);
                    }
                }
            }
        }
        assert_eq!(carriers, 4, "名前を述べる manifest の数が変わりました");
        assert_eq!(seen.get("name").map(String::as_str), Some(PLUGIN_NAME));
        assert_eq!(seen.get("version").map(String::as_str), Some(VERSION));
        assert_eq!(
            seen.get("description").map(String::as_str),
            Some(PLUGIN_DESCRIPTION)
        );
    }

    #[test]
    fn both_marketplaces_state_the_same_name_and_description() {
        for (name, value) in [
            (".claude-plugin", parse(&claude_marketplace())),
            (".agents", parse(&spec_marketplace())),
        ] {
            assert_eq!(value["name"], json!(MARKETPLACE_NAME), "{name}");
            assert_eq!(
                value["description"],
                json!(MARKETPLACE_DESCRIPTION),
                "{name}"
            );
            assert_eq!(value["owner"]["name"], json!(OWNER_NAME), "{name}");
        }
    }

    #[test]
    fn the_spec_manifests_declare_the_same_schema_version() {
        // spec は「`plugin.json` と mcp 設定は同じ spec 版を宣言する」と定めて
        // いる。**版が割れると、片方だけが別の規約で読まれる。**
        let version_of = |value: &Value| {
            let schema = value["$schema"]
                .as_str()
                .expect("spec 版の manifest が $schema を持っていません")
                .to_string();
            schema
                .split('/')
                .rev()
                .nth(1)
                .expect("schema の URL に版がありません")
                .to_string()
        };
        let plugin = version_of(&parse(&spec_plugin()));
        let mcp = version_of(&parse(&spec_mcp()));
        assert_eq!(plugin, mcp, "2 つの $schema の版が食い違います");
        assert_eq!(plugin, SPEC_VERSION);
    }

    #[test]
    fn the_claude_plugin_manifest_carries_no_schema() {
        // `$schema` は spec 版だけが持つ。Claude Code はこれを読まない。
        assert!(parse(&claude_plugin()).get("$schema").is_none());
        assert!(parse(&claude_mcp()).get("$schema").is_none());
    }

    #[test]
    fn the_plugin_name_matches_what_the_spec_allows() {
        // 1〜64 文字の小英数・ハイフン・ピリオド。`--` と `..` を含まず、
        // 英数で始まり英数で終わる。
        let name = PLUGIN_NAME;
        assert!((1..=64).contains(&name.len()), "長さが範囲外です: {name}");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.'),
            "使えない文字があります: {name}"
        );
        assert!(!name.contains("--") && !name.contains(".."), "{name}");
        for edge in [name.chars().next().unwrap(), name.chars().last().unwrap()] {
            assert!(
                edge.is_ascii_alphanumeric(),
                "両端が英数ではありません: {name}"
            );
        }
        // 生成側が実際にこの名前を書いていること。
        assert_eq!(parse(&spec_plugin())["name"], json!(name));
    }

    #[test]
    fn the_spec_command_is_a_plugin_relative_path() {
        // spec の `command` は bare な実行体名か `./` で始まる plugin 相対パスの
        // いずれかであり、**placeholder 展開も効かない。** 絶対パスを書けば
        // client 側で静かに落ちる。
        let command = parse(&spec_mcp())["mcpServers"][PLUGIN_NAME]["command"]
            .as_str()
            .expect("command がありません")
            .to_string();
        assert!(
            command.starts_with("./"),
            "plugin 相対パスではありません: {command}"
        );
        assert!(!command.contains(':'), "絶対パスを書いています: {command}");
        assert!(
            !command.contains('$'),
            "placeholder を書いています: {command}"
        );
        assert!(
            command.ends_with(super::super::SERVER_EXECUTABLE),
            "{command}"
        );
    }

    #[test]
    fn no_mcp_configuration_sets_the_reserved_environment_names() {
        // spec は `env` に `PLUGIN_ROOT` / `PLUGIN_DATA` を置くことを禁じている。
        for (name, value) in [
            ("claude", parse(&claude_mcp())),
            ("spec", parse(&spec_mcp())),
        ] {
            let server = &value["mcpServers"][PLUGIN_NAME];
            let env = server.get("env");
            for reserved in ["PLUGIN_ROOT", "PLUGIN_DATA"] {
                assert!(
                    env.and_then(|env| env.get(reserved)).is_none(),
                    "{name} の env が {reserved} を持っています"
                );
            }
        }
    }

    #[test]
    fn the_readme_says_the_tree_is_generated_and_reverts() {
        let text = readme();
        for phrase in ["生成している", "書き戻される", "外せば"] {
            assert!(text.contains(phrase), "README が {phrase} を述べていません");
        }
        // 生成の対象ではないものを名指しする。**同じディレクトリに同居して
        // いる以上、区別が読めなければ README は不安を増やすだけである。**
        for kept in ["settings.json", "instances/", "artifacts/"] {
            assert!(text.contains(kept), "README が {kept} に触れていません");
        }
    }

    /// README が示す道順が、画面に実在する見出しであること。
    ///
    /// **README は「設定 → メニュー → ページ → 切り替え」を辿らせる。** 4 つの
    /// うち 1 つでも写しにすると、見出しを変えたときに片方だけが古くなり、読み手
    /// は存在しないものを探す。**画面の側の定数から引いていることを固定する。**
    #[test]
    fn the_readme_names_the_headings_the_screen_actually_has() {
        let text = readme();
        for heading in [
            crate::settings_ui::MENU_NAME,
            crate::settings_ui::AGENT_PLUGIN_PAGE,
            crate::settings_ui::form::AgentPluginToggle::Generate.label(),
        ] {
            assert!(
                text.contains(heading),
                "README が画面の見出し「{heading}」を示していません"
            );
        }
    }

    /// 切り替えの見出しが設計の語彙を持ち出さないこと。
    ///
    /// **「方言」は生成する側の事情である。** 2 つの形が歩み寄らないことを
    /// 利用者は知らなくてよく、選んでいるのはどの相手に向けて置くかである。
    /// 相手の名前は綴りのまま出す。
    #[test]
    fn the_toggles_name_the_client_and_not_the_shape_of_its_files() {
        use crate::settings_ui::form::AgentPluginToggle;
        for toggle in AgentPluginToggle::ALL {
            let label = toggle.label();
            assert!(
                !label.contains("方言"),
                "「{label}」が設計の語彙を利用者へ出しています"
            );
        }
        assert!(
            AgentPluginToggle::Claude.label().contains("Claude Code"),
            "Claude Code を名指ししていません"
        );
        assert!(
            AgentPluginToggle::AgentPlugins
                .label()
                .contains("Agent Plugins"),
            "Agent Plugins を綴りのまま名指ししていません"
        );
    }
}
