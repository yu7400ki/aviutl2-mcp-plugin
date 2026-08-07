//! 同梱する skill の本文を検査する。
//!
//! **ファイルが在ることを見るのでは足りない。** 空の `SKILL.md` も、tool 説明を
//! 写しただけの `SKILL.md` も、存在の検査は通る。ここで見るのは中身である。
//!
//! 検査は 2 か所に分かれる。**それぞれの入力が在る場所に置いてある。**
//! 層 1 から落とした句と、層 1 から持ち越した検査は、その表を持つ server crate
//! の側で本文と突き合わせる。ここが見るのは、表を持たずに掛けられる性質——
//! ツリーの形、節の一覧、名指ししてよい識別子、未実測の項目、候補の写し——で
//! ある。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// skill ツリーの根。
fn skills_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join("skills")
}

/// 同梱する唯一の skill の名前。
///
/// **1 つに保つ**——発火の条件が同じ（AviUtl2 を編集する）である以上、割っても
/// 両方が同時に読まれるだけで `description` の書き分けに意味が無い。
const SKILL_NAME: &str = "aviutl2-editing";

/// `SKILL.md` の本文。
fn skill_body() -> String {
    let path = skills_dir().join(SKILL_NAME).join("SKILL.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} を読めません: {e}", path.display()))
}

/// skill ツリーに含まれる全ての markdown を、根からの相対パスと本文の対で返す。
fn skill_files() -> Vec<(String, String)> {
    let mut files = Vec::new();
    collect_markdown(&skills_dir(), &skills_dir(), &mut files);
    files.sort();
    assert!(!files.is_empty(), "skill ツリーに markdown がありません");
    files
}

/// ディレクトリを辿って markdown を集める。
fn collect_markdown(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{} を辿れません: {e}", dir.display()))
    {
        let path = entry.expect("ディレクトリの要素を読めません").path();
        if path.is_dir() {
            collect_markdown(root, &path, out);
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("根の外のファイルを拾いました")
            .to_string_lossy()
            .replace('\\', "/");
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} を読めません: {e}", path.display()));
        out.push((relative, body));
    }
}

/// `##` の見出しを順に返す。
fn sections(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| line.strip_prefix("## "))
        .map(|title| title.trim().to_string())
        .collect()
}

/// 段階を問わない全ての見出しを返す。
fn headings(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let title = trimmed.trim_start_matches('#');
            (trimmed.starts_with('#') && title.starts_with(' ')).then(|| title.trim().to_string())
        })
        .collect()
}

/// 本文に現れる snake_case の識別子を集める。
///
/// **tool 名だけを数えない。** tool 名の一覧はこの crate から見えず、写したものを
/// 検査の側に置けば正本が 2 つになる。代わりに「下線を含む小文字の語」という
/// 形で拾う——tool 名も `details` のキーも入力 schema のフィールド名も同じ形を
/// しており、**skill が新しく何かを名指しし始めた瞬間に集合が広がる。**
fn identifiers(body: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut token = String::new();
    for ch in body.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            token.push(ch);
            continue;
        }
        if token.contains('_') && token.starts_with(|c: char| c.is_ascii_lowercase()) {
            found.insert(token.clone());
        }
        token.clear();
    }
    found
}

/// `SKILL.md` が持ってよい節と、その節が受け持つ役割。
///
/// **写しを持たないことを機械的に見る形がこれである。** 個々の tool を並べて
/// 動作を説明する節を足せば、この一覧に無い見出しとして落ちる。
const ALLOWED_SECTIONS: &[(&str, Purpose)] = &[
    ("まず対象を決める", Purpose::CrossToolConvention),
    ("番号は 0 始まりである", Purpose::CrossToolConvention),
    ("selector と世代", Purpose::CrossToolConvention),
    ("取り消しの単位", Purpose::CrossToolConvention),
    ("失敗したときにすること", Purpose::CrossToolConvention),
    ("使える効果と書ける値を引く", Purpose::ValueLookupRoute),
    ("参照文書", Purpose::ReferenceIndex),
];

/// 参照文書が持ってよい節。
///
/// **`SKILL.md` だけを固定しても足りない。** 節を丸ごと足す経路は
/// `references/` の側にも同じだけ開いており、そちらは量が増えるぶん
/// 気付かれにくい。ファイルごとに一覧を持つ。
const ALLOWED_REFERENCE_SECTIONS: &[(&str, &[&str])] = &[
    (
        "references/layers.md",
        &[
            "番号が大きいレイヤーが手前に描かれる",
            "この規則は応答に現れない",
            "組むときの向き",
        ],
    ),
    (
        "references/object-alias.md",
        &[
            "なぜ使うか",
            "構造",
            "frame 行が区間を決める",
            "トラックバーの値と移動",
            "黙って捨てられる書き方がある",
            "トラックバーグループ",
            "未確認",
        ],
    ),
    (
        "references/workflows.md",
        &[
            "見る → 組み立てる → 作る → 描いて確かめる",
            "空のプロジェクトから始めるとき",
            "失敗を踏んだとき",
        ],
    ),
];

/// 節が受け持つ役割。**`SKILL.md` が持ってよいのはこの 3 つだけである。**
///
/// 一覧へ節を足すとき、3 つのどれに当たるかを言えないなら、それは
/// `SKILL.md` に属さない節である。
#[derive(Debug, Clone, Copy)]
enum Purpose {
    /// 複数の tool にまたがる規約。
    CrossToolConvention,
    /// `references/` への導線と、どんなときにどれを開くか。
    ReferenceIndex,
    /// 候補を引く経路。**候補の値そのものではない。**
    ValueLookupRoute,
}

/// skill が名指ししてよい識別子。
///
/// **一覧は許可であって説明ではない。** ここに在るのは、複数の tool にまたがる
/// 規約を述べるために名前を出さざるを得ないものだけである。tool を 1 個ずつ
/// 解説し始めれば、この一覧に無い名前が本文へ現れる。
const IDENTIFIERS_THE_SKILL_MAY_NAME: &[&str] = &[
    // 対象のインスタンスを決める経路。
    "instance_id",
    "list_instances",
    // プロジェクト境界の照合材料と、それを運ばない要求。
    "project_epoch",
    "expected_project_epoch",
    "project_revision",
    // 前提の epoch を要求する（selector を持たない）tool。
    "create_object",
    "set_layer_state",
    "set_selection",
    "set_grid_bpm",
    "set_scene_settings",
    // 取り消し単位を作るか確かめていない tool。
    "create_object_section",
    "delete_object_section",
    "move_object_section",
    // 取り消し単位をまとめる経路。
    "apply_batch",
    // 失敗を読むときのコードとキー。
    "precondition_failed",
    "current_object",
    "failed_object",
    "mutation_issued",
    "change_applied",
    // 何が在るかと、書ける値を引く経路。
    "list_available_effects",
    "describe_effects",
    "get_object",
    "list_fonts",
    // オブジェクトを 1 呼び出しで組み立てる入力。
    "object_alias",
];

/// 実測していない事柄を指す語。
///
/// **skill は実測記録の要約であって、推測の置き場ではない。** 座標系・単位は
/// 実機で確かめていないため、断定も推測も本文に置かない。
///
/// **語は連結せずに単独で挙げる。** `拡大率の基準` のような連結語だけを置くと、
/// `拡大率は 100 が原寸である` のような断定が素通りする。
const UNMEASURED_TERMS: &[&str] = &["座標系", "原点", "回転の向き", "拡大率", "等倍", "画面中央"];

/// 実測していないことを名乗る句。
const UNMEASURED_DISCLAIMER: &str = "実測していない";

#[test]
fn the_skill_tree_holds_only_directories_that_have_a_skill_md() {
    // plugin root 直下の `skills/` は、`SKILL.md` を持つ各ディレクトリが 1 skill
    // であると spec が定めている。ファイルを直接置いても、`SKILL.md` の無い
    // ディレクトリを置いても、client からは何も見えない。
    let root = skills_dir();
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&root).expect("skills ディレクトリを辿れません") {
        let path = entry.expect("skills の要素を読めません").path();
        assert!(
            path.is_dir(),
            "skills の直下にディレクトリ以外があります: {}",
            path.display()
        );
        assert!(
            path.join("SKILL.md").is_file(),
            "{} が SKILL.md を持っていません",
            path.display()
        );
        names.push(path.file_name().unwrap().to_string_lossy().into_owned());
    }
    assert_eq!(
        names,
        vec![SKILL_NAME.to_string()],
        "skill は 1 つに保つ。発火の条件が同じである以上、割っても両方が同時に読まれる"
    );
}

#[test]
fn the_skill_body_declares_up_front_that_it_carries_no_copy() {
    // 宣言は最初の節より前に置く。要約する読み手は末尾を落とす。
    let body = skill_body();
    let preamble = body
        .split("\n## ")
        .next()
        .expect("本文が空です")
        .to_string();
    for phrase in [
        "個々の tool が何をするかを述べない",
        "tool 定義と入力 schema が正本",
        "正本が 2 つになり",
    ] {
        assert!(
            preamble.contains(phrase),
            "写しを置かない宣言が冒頭にありません: {phrase}"
        );
    }
}

#[test]
fn the_skill_has_only_the_sections_it_is_allowed_to_have() {
    // C-T2。**節の一覧を検査の側に置く。** 本文の側だけを見ても、増えた節が
    // 写しなのか新しい規約なのかは機械では分からない。
    //
    // **限界を承知で使う形である。** ここが緑であることは、不変条件 1 を
    // 満たしたことを意味しない——既にある節の中へ、識別子を 1 つも使わずに
    // tool の動作を説明する段落を書けば素通りする。C-T2 が「機械的に見るのは
    // 難しい」と述べているのはこの穴のことであり、塞ぐ手は無い。
    // **節を足す経路だけを塞いでいる。**
    let found = sections(&skill_body());
    let allowed: Vec<String> = ALLOWED_SECTIONS
        .iter()
        .map(|(title, _)| (*title).to_string())
        .collect();
    assert_eq!(
        found, allowed,
        "SKILL.md の節が、持ってよい一覧と一致しません"
    );

    for (path, titles) in ALLOWED_REFERENCE_SECTIONS {
        let body = skill_files()
            .into_iter()
            .find_map(|(name, body)| (name == format!("{SKILL_NAME}/{path}")).then_some(body))
            .unwrap_or_else(|| panic!("{path} がありません"));
        let expected: Vec<String> = titles.iter().map(|title| (*title).to_string()).collect();
        assert_eq!(
            sections(&body),
            expected,
            "{path} の節が、持ってよい一覧と一致しません"
        );
    }
    // 一覧を持たない参照文書があると、そのファイルだけ節を足し放題になる。
    for (name, _) in skill_files() {
        let Some(path) = name.strip_prefix(&format!("{SKILL_NAME}/")) else {
            continue;
        };
        assert!(
            path == "SKILL.md"
                || ALLOWED_REFERENCE_SECTIONS
                    .iter()
                    .any(|(known, _)| *known == path),
            "{path} の節の一覧が検査側にありません"
        );
    }
}

#[test]
fn no_heading_is_the_name_of_something_the_tools_define() {
    // tool や設定キーを見出しにすると、その下は必ずそれ 1 個の解説になる。
    for (name, body) in skill_files() {
        for heading in headings(&body) {
            assert!(
                identifiers(&heading).is_empty(),
                "{name} の見出しが個別の名前を掲げています: {heading}"
            );
        }
    }
}

#[test]
fn the_skill_names_nothing_outside_the_conventions_it_carries() {
    // C-T2 の本体。**名指しの集合が広がることが、写しが混ざった徴候である。**
    let allowed: BTreeSet<&str> = IDENTIFIERS_THE_SKILL_MAY_NAME.iter().copied().collect();
    let mut used: BTreeSet<String> = BTreeSet::new();
    for (name, body) in skill_files() {
        for identifier in identifiers(&body) {
            assert!(
                allowed.contains(identifier.as_str()),
                "{name} が規約の外の名前を挙げています: {identifier}"
            );
            used.insert(identifier);
        }
    }
    // 使われなくなった許可を残すと、一覧が「昔そう書いてあった」の記録へ変わる。
    for identifier in &allowed {
        assert!(
            used.contains(*identifier),
            "誰も名指ししていない許可が残っています: {identifier}"
        );
    }
}

/// `SKILL.md` が述べるべき、複数 tool にまたがる規約 1 件。
struct Convention {
    /// 何についての規約か。
    topic: &'static str,
    /// 本文に在るべき句。**全て在ること**を求める。
    phrases: &'static [&'static str],
}

/// `SKILL.md` が持つべき規約と経路。
///
/// **層 1 から落とした句の行き先は server crate の表が持つ。** ここに在るのは、
/// 落とした句には含まれないが skill が述べると決まっているもの——対象の決め方、
/// 失敗の受け止め方、何が在るかを引く経路——を含めた、話題の側からの網羅で
/// ある。
const CONVENTIONS_THE_SKILL_MUST_STATE: &[Convention] = &[
    Convention {
        topic: "instance_id の取り方",
        phrases: &["instance_id", "list_instances が返す"],
    },
    Convention {
        topic: "frame / layer が 0 始まりで UI の表示と 1 ずれること",
        phrases: &["1 始まりで表示する", "0 始まりの番号"],
    },
    Convention {
        topic: "selector を組み立てず往復させること",
        phrases: &["selector は自分で組み立てない"],
    },
    Convention {
        topic: "selector を持たない tool での expected_project_epoch",
        phrases: &["expected_project_epoch を要求し", "省略できない"],
    },
    Convention {
        topic: "要求が project_revision を運ばないこと",
        phrases: &["project_revision を運ばない"],
    },
    Convention {
        topic: "1 呼び出しが 1 つの取り消し単位であることと apply_batch を選ぶ基準",
        phrases: &["1 つの取り消し単位になる", "apply_batch を選ぶ"],
    },
    Convention {
        topic: "失敗したらリトライではなく読み直すこと",
        phrases: &["同じ要求をそのまま送り直さない", "組み立て直す"],
    },
    // **候補を引く経路は、設定項目の値だけではない。** 実測で失われたのは
    // 「当てられなかった」ではなく「在ることを知らなかった」であり、効果
    // そのものを引く経路が無ければ、知らないものは思い付けないままになる。
    Convention {
        topic: "どんな効果が在るかを引く経路",
        phrases: &[
            "list_available_effects が返す",
            "1 つも候補に上がらなかった",
        ],
    },
];

#[test]
fn the_skill_body_states_every_cross_tool_convention() {
    let body = skill_body();
    for convention in CONVENTIONS_THE_SKILL_MUST_STATE {
        for phrase in convention.phrases {
            assert!(
                body.contains(phrase),
                "{} を本文が述べていません: {phrase}",
                convention.topic
            );
        }
    }
}

#[test]
fn every_reference_is_reachable_from_the_body_and_every_link_resolves() {
    // 導線の無い参照文書は、置いていないのと同じである。逆に、切れた導線は
    // 「読めるはずのものが読めない」であり、無いことより悪い。
    let body = skill_body();
    let root = skills_dir().join(SKILL_NAME);
    let mut referenced = 0usize;
    for (name, _) in skill_files() {
        let Some(relative) = name.strip_prefix(&format!("{SKILL_NAME}/")) else {
            panic!("skill の外に markdown があります: {name}");
        };
        if relative == "SKILL.md" {
            continue;
        }
        assert!(
            body.contains(relative),
            "{relative} への導線が SKILL.md にありません"
        );
        referenced += 1;
    }
    assert!(referenced > 0, "参照文書が 1 つもありません");

    for (name, content) in skill_files() {
        for target in markdown_link_targets(&content) {
            let base = if name.contains('/') {
                root.join(
                    name.rsplit_once('/')
                        .map(|(dir, _)| dir)
                        .unwrap_or_default()
                        .trim_start_matches(SKILL_NAME)
                        .trim_start_matches('/'),
                )
            } else {
                root.clone()
            };
            let resolved = base.join(target.trim_start_matches("./"));
            assert!(
                resolved.is_file(),
                "{name} の導線が解決しません: {target}（{}）",
                resolved.display()
            );
        }
    }
}

/// markdown の inline link の宛先のうち、ツリー内の markdown を指すものを返す。
fn markdown_link_targets(body: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find("](") {
        let after = &rest[open + 2..];
        let Some(close) = after.find(')') else {
            break;
        };
        let target = &after[..close];
        if target.ends_with(".md") {
            targets.push(target.to_string());
        }
        rest = &after[close..];
    }
    targets
}

#[test]
fn the_skill_writes_nothing_about_what_was_never_measured() {
    // 座標系・単位は実機で確かめていない。断定すれば嘘になり、推測を「未確認」の
    // 札を付けて置けば skill が推測の置き場になる。**触れてよいのは、書いて
    // いないと名乗る 1 行だけである。**
    let mut disclaimers = 0usize;
    for (name, body) in skill_files() {
        for line in body.lines() {
            let Some(term) = UNMEASURED_TERMS.iter().find(|term| line.contains(**term)) else {
                continue;
            };
            assert!(
                line.contains(UNMEASURED_DISCLAIMER),
                "{name} が実測していない事柄を述べています（{term}）: {line}"
            );
            disclaimers += 1;
        }
    }
    assert!(
        disclaimers <= 1,
        "実測していない事柄に触れる行が {disclaimers} 行あります"
    );
}

#[test]
fn the_skill_copies_no_choice_from_the_builtin_table() {
    // 候補の正本は読み取り経路が返す表である。手書きの markdown へ写せば
    // 正本が 2 つになり、**陳腐化が「足りなくなる」から「間違う」へ落ちる。**
    //
    // **判定は (効果, 項目) ごとに 3 語であり、その下は通る。** 1 つの項目から
    // 2 語だけ挙げる書き方も、効果をまたいで 1 語ずつ並べる書き方も素通り
    // する。閾値を下げれば、候補と無関係に同じ語を使っただけで落ちるように
    // なる——`通常` や `回転` は候補の値であると同時にただの日本語である。
    // **緑であることは、写しが 1 語も無いことを意味しない。**
    let table: serde_json::Value =
        serde_json::from_str(include_str!("../data/effect_item_facets.json"))
            .expect("基底の表を解釈できません");
    let effects = table["effects"]
        .as_object()
        .expect("基底の表に effects がありません");
    let files = skill_files();

    let mut groups = 0usize;
    for (effect, items) in effects {
        for (item, facets) in items.as_object().expect("項目の集合がありません") {
            let Some(choices) = facets.get("choices").and_then(|value| value.as_array()) else {
                continue;
            };
            groups += 1;
            for (name, body) in &files {
                let copied: Vec<&str> = choices
                    .iter()
                    .filter_map(|choice| choice.as_str())
                    .filter(|choice| body.contains(*choice))
                    .collect();
                // 1 語 2 語の一致は、候補と無関係に同じ語を使っただけであり得る。
                // **3 語並べば、それは列挙である。**
                assert!(
                    copied.len() < 3,
                    "{name} が {effect} / {item} の候補を写しています: {copied:?}"
                );
            }
        }
    }
    assert!(groups > 0, "基底の表に候補が 1 つもありません");
}
