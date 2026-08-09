//! 同梱する skill の本文を検査する。
//!
//! **ファイルが在ることを見るのでは足りない。** 空の `SKILL.md` も、tool 説明を
//! 写しただけの `SKILL.md` も、存在の検査は通る。ここで見るのは中身である。
//!
//! 検査は 2 か所に分かれる。**それぞれの入力が在る場所に置いてある。**
//! 層 1 から落とした句と、層 1 から持ち越した検査は、その表を持つ server crate
//! の側で本文と突き合わせる。ここが見るのは、表を持たずに掛けられる性質——
//! ツリーの形、導線、見出しの立て方、未実測の項目、候補の写し、根拠に挙げた値、
//! 失敗の名指し——である。

use crate::agent_plugin::skills::SKILL_FILES;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// skill ツリーの根。
///
/// **本文を読む検査はここを見ない。** 見るのは埋め込みの一覧
/// （[`SKILL_FILES`]）であり、配られるのはそちらだからである。ディスクを要する
/// のはツリーの形の検査だけで、そちらは埋め込みが漏らしたファイルも見える形で
/// なければ意味を成さない。
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
    let name = format!("{SKILL_NAME}/SKILL.md");
    skill_files()
        .into_iter()
        .find_map(|(path, body)| (path == name).then_some(body))
        .unwrap_or_else(|| panic!("{name} が埋め込まれていません"))
}

/// 配られる skill ツリーを、根からの相対パスと本文の対で返す。
///
/// **ディスクではなく埋め込みを見る。** 利用者へ届くのは埋め込みのほうであり、
/// data ディレクトリに在って埋め込まれていないファイルは、どれだけ良く書けて
/// いても読まれない。
fn skill_files() -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = SKILL_FILES
        .iter()
        .map(|(path, body)| ((*path).to_string(), (*body).to_string()))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "skill ツリーに markdown がありません");
    files
}

/// ディレクトリを辿って**全ての**ファイルを集める。
///
/// **拡張子で絞らない。** 絞れば、埋め込みの一覧に無いファイルが「対象外」と
/// して見逃され、配布から漏れたことに誰も気付かない。
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{} を辿れません: {e}", dir.display()))
    {
        let path = entry.expect("ディレクトリの要素を読めません").path();
        if path.is_dir() {
            collect_files(root, &path, out);
            continue;
        }
        out.push(
            path.strip_prefix(root)
                .expect("根の外のファイルを拾いました")
                .to_string_lossy()
                .replace('\\', "/"),
        );
    }
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
/// している。
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

/// 実測していない事柄を指す語。
///
/// **skill は実測記録の要約であって、推測の置き場ではない。** 座標は画面に向かう
/// 3 軸を持つが、測ったのは画面に平行な 2 軸と、その面の中での回転だけである。
/// 残りは断定も推測も本文に置かない。
///
/// **語は連結せずに単独で挙げる。** `回転の向き` のような連結語だけを置くと、
/// `X軸回転は正で手前へ倒れる` のような断定が素通りする。
const UNMEASURED_TERMS: &[&str] = &["X軸回転", "Y軸回転", "奥行き"];

/// 実測していないことを名乗る句。
const UNMEASURED_DISCLAIMER: &str = "実測していない";

/// 実測を述べる行を見分ける語。
const MEASUREMENT_MARK: &str = "実測";

/// alias の書式を定めている参照文書。
const ALIAS_REFERENCE: &str = "references/object-alias.md";

/// 移動のフラグに書いてよい値の上限。
///
/// [`ALIAS_REFERENCE`] 自身が「フラグに書いてよいのは 0〜7」と定めている。
const MAX_TRACK_FLAGS: u64 = 7;

/// 上限の外の値を挙げてよい行——禁止そのものを述べる行——を見分ける語。
const FORBIDDEN_BIT: &str = "bit3";

/// 待つべき時間を運ぶ `details` のキー。
const RETRY_AFTER_KEY: &str = "retry_after_ms";

/// 待ち時間を運ぶ失敗の扱いに現れるべき名前。
///
/// **[`RETRY_AFTER_KEY`] を運ぶ失敗は 1 種類ではない。** `host_busy` を名指し
/// しなければ、本文の一般則——失敗したら同じ要求をそのまま送り直さない——が
/// そこへ当たり、正しい要求を作り直すことになる。そして待っても解けない側が
/// 在るため、**その先で名乗る `instance_stale` まで含めて初めて扱いが述べ
/// られる。**
const WAITING_FAILURE_NAMES: &[&str] = &["host_busy", "instance_stale"];

/// 行に現れるインラインコードのうち、移動行として読めるものからフラグを返す。
///
/// 移動行は `<値>,…,<移動モード名>,<フラグ>[|<パラメータ>…]` であり、フラグは
/// 最後のコンマの後ろに在る。**整数として読めない末尾は移動行ではない**——
/// フラグを欠いた行も、コンマを含むただのテキストも、ここで落ちる。
fn track_flags(line: &str) -> Vec<u64> {
    line.split('`')
        .skip(1)
        .step_by(2)
        .filter_map(|span| {
            let (_, tail) = span.rsplit_once(',')?;
            tail.split('|').next().unwrap_or(tail).parse::<u64>().ok()
        })
        .collect()
}

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
fn the_embedded_tree_matches_the_files_on_disk() {
    // **`include_str!` はリテラルしか受け付けない。** 一覧は手書きであり、
    // data ディレクトリへ足したファイルは黙って配布から漏れる。ここが
    // その経路を塞ぐ——漏れは「参照文書が開けない」として利用者側に出る。
    //
    // **拡張子で絞らずに突き合わせる。** 絞れば markdown 以外だけが素通りし、
    // 塞いだつもりの穴が種類ごとに残る。`include_str!` はテキストしか受け
    // ないため、二値のファイルを skill へ置くならまず埋め込みの形を決める
    // ことになる——ここが落ちることでそれが分かる。
    let mut on_disk = Vec::new();
    collect_files(&skills_dir(), &skills_dir(), &mut on_disk);
    on_disk.sort();
    let embedded: Vec<String> = skill_files().into_iter().map(|(path, _)| path).collect();
    assert_eq!(embedded, on_disk, "埋め込みの一覧とツリーが食い違います");

    for (path, body) in skill_files() {
        let on_disk = std::fs::read_to_string(skills_dir().join(&path))
            .unwrap_or_else(|e| panic!("{path} を読めません: {e}"));
        assert_eq!(body, on_disk, "{path} の埋め込みが古くなっています");
    }
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
    // 画面の外へ向かう軸は実機で確かめていない。断定すれば嘘になり、推測を
    // 「未確認」の札を付けて置けば skill が推測の置き場になる。**触れてよいのは、
    // 書いていないと名乗る 1 行だけである。**
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

#[test]
fn the_alias_reference_grounds_its_flag_bits_in_a_value_it_allows() {
    // **文書が自らの禁止に触れる例を根拠にしない。** 0〜7 の外を挙げれば、
    // 写した読み手はその文書が禁じた状態を作る。禁止そのものを述べる行だけが
    // 外の値を挙げてよい——そこでは値が根拠ではなく対象である。
    let name = format!("{SKILL_NAME}/{ALIAS_REFERENCE}");
    let body = skill_files()
        .into_iter()
        .find_map(|(path, body)| (path == name).then_some(body))
        .unwrap_or_else(|| panic!("{name} が埋め込まれていません"));

    let mut grounds = 0usize;
    for line in body.lines() {
        if !line.contains(MEASUREMENT_MARK) || line.contains(FORBIDDEN_BIT) {
            continue;
        }
        for flags in track_flags(line) {
            assert!(
                flags <= MAX_TRACK_FLAGS,
                "{name} が 0〜{MAX_TRACK_FLAGS} の外のフラグを根拠にしています（{flags}）: {line}"
            );
            grounds += 1;
        }
    }
    assert!(grounds > 0, "{name} にフラグの根拠を述べる行がありません");
}

#[test]
fn the_skill_names_every_failure_that_carries_a_wait() {
    // **待ち時間を運ぶ失敗を 1 つしか名乗らないと、残りへ一般則が当たる。**
    // 一般則は「同じ要求をそのまま送り直さない」であり、正しい要求を作り直す
    // という最も無駄な一手を選ばせる。名指しは同じ節で行う——別の節に置けば、
    // 失敗を読んでいる読み手のところに届かない。
    let body = skill_body();
    let section = body
        .split("\n## ")
        .find(|section| section.contains(RETRY_AFTER_KEY))
        .unwrap_or_else(|| panic!("{RETRY_AFTER_KEY} を述べる節がありません"));
    for failure in WAITING_FAILURE_NAMES {
        assert!(
            section.contains(failure),
            "待ち時間を運ぶ失敗の扱いに {failure} が現れません"
        );
    }
}
