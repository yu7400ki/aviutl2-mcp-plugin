//! 設定項目が取り得る値の候補。
//!
//! **SDK からは取れない。** 設定項目の列挙がコールバックへ渡すのは名前と種別
//! だけであり、選択肢を返す関数がヘッダーに存在しない。組み込み effect の候補を
//! 並べたファイルもディスク上に無い。供給源は、この plugin へ埋め込んだ基底の
//! 表と、走査で見つけたサイドカーの 2 つだけである。
//!
//! **候補はヒントであってゲートではない。** ここに無い値でも書き込みは通し、
//! ここに在る値が必ず通るとも約束しない。可否の判定はホストへの書き戻しと
//! その読み直しに委ねる。版ずれ・プラグインの追加・未知の effect で表が実態から
//! 外れたとき、事前検証を掛けていれば「正しい値なのに通らない」へ退化する。
//! 候補を知らずに総当たりになる状態より悪い。**移動方法の一覧
//! （[`crate::movement::movements`]）とは性質が違う**——あちらは一覧に無い名前を
//! 書くとホストのプロセスが落ちるため通す選択肢が無いが、候補は外しても最悪で
//! ホストが値を無視するだけである。
//!
//! # 埋め込みの基底
//!
//! 基底はバイナリへ取り込む。走査もパース失敗の経路も持たない——構文の
//! 正しさはこのモジュールの検査が押さえる。
//!
//! # サイドカー
//!
//! データディレクトリ直下の `Plugin` と、そのサブディレクトリ 1 段から
//! [`SIDECAR_SUFFIX`] で終わるファイルを集める。
//!
//! **書き手はプラグインの提供者である。** 自分のプラグインの候補を配布物へ
//! 同梱できるようにするのが目的であり、利用者が手で書く前提は置かない。
//!
//! **専用のサブディレクトリを掘らない。** 作者が配布物を展開するだけで候補も
//! 入る形にするためである。プラグイン本体と `.ini` や `.conf` が同じ
//! ディレクトリへ同居する慣習は既に確立している。
//!
//! **サブディレクトリは 1 段だけ辿る。** プラグインがフォルダごと配布される形が
//! 実在する一方、2 段以上を要する形は無い。
//!
//! **ファイル名の規則は第三者に対する契約である。** 公開した後で変えると、既に
//! 配布されたサイドカーが黙って読まれなくなる。読み手が居ない欄を消すのとは
//! 違い、規則の変更は相手のファイルを無効にする。
//!
//! **書き手は特定できない。** 作者が配布物へ同梱したのか、利用者が自分で置いた
//! のかを区別する手段が我々には無い。[`ChoicesSource::Sidecar`] が述べるのは
//! 「走査で見つけた」ことだけである。
//!
//! # 重ね方
//!
//! 1. 埋め込みの基底を先に置く。パスを持たないため走査順の影響を受けない。
//! 2. サイドカーをパスの昇順で重ねる。
//! 3. 置換は (効果, 項目) の粒度で行う。**加算しない**——加算では基底の誤りを
//!    利用者が消せず、結果も予測しにくい。
//! 4. 同じ (効果, 項目) を複数のファイルが主張したら、昇順で後に来たものが
//!    勝つ。決定的であり、ログへ 1 行残す。
//!
//! # 壊れていても失敗させない
//!
//! 書き手が第三者である以上、中身の品質を我々が保証することはできない。
//!
//! - パースできない: そのファイルを丸ごと無視し、ログへ 1 行残す
//! - 未知のフィールド: そのフィールドだけを無視する
//! - この環境に存在しない effect の項目: 黙って無視する。利用者が複数の環境で
//!   同じファイルを使うことがある
//! - 同じ (効果, 項目) の重複: 上記の後勝ちで解決する
//!
//! いずれも tool の失敗にしない。**この扱いは基底には要らない**——埋め込みは
//! 走査もパース失敗の経路も持たない。
//!
//! # 表の形
//!
//! 基底もサイドカーも同じ形である。**候補を起こす生成器が書き出すのもこの形で
//! ある。**
//!
//! ```json
//! {
//!   "effects": {
//!     "テキスト": {
//!       "文字揃え": ["左寄せ[上]", "中央揃え[中]"]
//!     }
//!   }
//! }
//! ```
//!
//! - `effects` は効果名から設定項目の表を引く。
//! - 設定項目の表は、項目名から候補の配列を引く。候補は文字列であり、書いた順
//!   がそのまま応答へ出る。
//! - これ以外のキーは無視する。読む人の居ない注記も、算出に全 effect × 全項目の
//!   列挙を要する陳腐化の印も持たない。

use crate::alias::{data_directory, read_bounded};
use aviutl2_mcp_core::{ChoicesSource, ItemChoices};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 埋め込む基底の表。
const BUILTIN_TABLE: &str = include_str!("../data/effect_item_choices.json");

/// サイドカーを探すディレクトリの名前。
const PLUGIN_DIRECTORY: &str = "Plugin";

/// サイドカーのファイル名の末尾。
///
/// **第三者に対する契約である。** 変えれば既存のサイドカーが読まれなくなる。
pub const SIDECAR_SUFFIX: &str = ".aviutl2-mcp.json";

/// サイドカー 1 件として読み込む最大バイト数。
///
/// この経路の費用は要求の内容で決まらないため、予算では守れない。上限は
/// 要求元が動かせない。
pub const MAX_SIDECAR_BYTES: u64 = 4 * 1024 * 1024;

/// 表 1 つの外形。基底もサイドカーも同じ形である。
///
/// **未知のフィールドは無視する。** 拒まないのは、我々が知らない欄を持つ
/// ファイルでも、知っている欄の分は使えるからである。
#[derive(Debug, Default, Deserialize)]
struct ChoicesDocument {
    /// 効果名から、設定項目名と候補の対応を引く。
    #[serde(default)]
    effects: HashMap<String, HashMap<String, Vec<String>>>,
}

/// 表を 1 つ重ねた結果。
#[derive(Debug, Default, PartialEq, Eq)]
struct OverlayReport {
    /// 表へ入れた (効果, 項目) の数。
    applied: usize,
    /// 既にあった候補を置き換えた (効果, 項目) の並び。
    replaced: Vec<(String, String)>,
}

/// 効果名と設定項目名から候補を引く表。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChoicesTable {
    effects: HashMap<String, HashMap<String, ItemChoices>>,
}

impl ChoicesTable {
    /// 効果 1 件分の候補を、設定項目名から引ける形で返す。
    ///
    /// 表に無い効果は `None` である。この環境に存在しない効果が表に在ることも、
    /// その逆もある。
    pub fn effect(&self, effect_name: &str) -> Option<&HashMap<String, ItemChoices>> {
        self.effects.get(effect_name)
    }

    /// 効果と設定項目を指して 1 件を引く。表に無ければ `None`。
    pub fn get(&self, effect_name: &str, item_name: &str) -> Option<&ItemChoices> {
        self.effects.get(effect_name)?.get(item_name)
    }

    /// 表が持つ (効果, 項目) の数。
    pub fn entry_count(&self) -> usize {
        self.effects.values().map(HashMap::len).sum()
    }

    /// 表を 1 つ重ねる。
    ///
    /// **置換は (効果, 項目) の粒度である。** 効果ごと差し替えれば、1 項目を
    /// 直したいだけのサイドカーが同じ効果の他の項目を消す。候補の配列そのものは
    /// 丸ごと入れ替える——既存の候補へ継ぎ足す形にすると、基底の誤りを消す手が
    /// 無くなる。
    fn overlay(&mut self, document: ChoicesDocument, source: ChoicesSource) -> OverlayReport {
        let mut report = OverlayReport::default();
        for (effect_name, items) in document.effects {
            let entry = self.effects.entry(effect_name.clone()).or_default();
            for (item_name, values) in items {
                let choices = ItemChoices { values, source };
                if entry.insert(item_name.clone(), choices).is_some() {
                    report.replaced.push((effect_name.clone(), item_name));
                }
                report.applied += 1;
            }
        }
        report
    }
}

/// 解決した表。
static TABLE: OnceLock<ChoicesTable> = OnceLock::new();

/// 効果名と設定項目名から候補を引く表を返す。
///
/// 読み込みは初回の要求で 1 度だけ行う。候補が得られないことは plugin が
/// 起動できない理由ではないため、初期化時には読まない。
pub fn table() -> &'static ChoicesTable {
    TABLE.get_or_init(|| load_table(data_directory()))
}

/// 埋め込みの基底へサイドカーを重ねた表を組み立てる。
///
/// `data_dir` は AviUtl2 のデータディレクトリである。解決できない環境では基底
/// だけの表になる。
///
/// 件数はログへ残す。基底とサイドカーのどちらが何件を持ち込んだのかが分から
/// なければ、応答に現れた候補の出所を後から辿れない。
pub fn load_table(data_dir: Option<&Path>) -> ChoicesTable {
    let mut table = builtin_table();
    let builtin_entries = table.entry_count();

    let mut files = 0usize;
    let mut applied = 0usize;
    let mut replaced = 0usize;
    for path in sidecar_paths(data_dir) {
        let Some(document) = read_document(&path) else {
            tracing::info!(
                "{} を選択肢の表として解釈できませんでした。このファイルは使いません",
                path.display()
            );
            continue;
        };
        files += 1;
        let report = table.overlay(document, ChoicesSource::Sidecar);
        applied += report.applied;
        replaced += report.replaced.len();
        for (effect_name, item_name) in report.replaced {
            tracing::info!(
                "{} が {effect_name} の {item_name} の候補を置き換えました",
                path.display()
            );
        }
    }

    tracing::info!(
        "選択肢の候補: 基底 {builtin_entries} 件、サイドカー {files} ファイルの {applied} 件、うち置き換え {replaced} 件"
    );
    table
}

/// 埋め込んだ基底を解釈する。
///
/// **失敗しない。** 取り込むのはビルド時に確定した文字列であり、JSON として
/// 解釈できることは [`tests::the_builtin_table_is_valid_json`] が押さえている。
fn builtin_table() -> ChoicesTable {
    let document: ChoicesDocument =
        serde_json::from_str(BUILTIN_TABLE).expect("埋め込んだ基底の表を解釈できません");
    let mut table = ChoicesTable::default();
    table.overlay(document, ChoicesSource::BuiltinTable);
    table
}

/// サイドカーのパスをパスの昇順で集める。
///
/// 走査するのは `Plugin` 直下と、そのサブディレクトリ 1 段だけである。並びは
/// 列挙の順に頼らない——列挙の順はファイルシステムが決めるものであり、後勝ちの
/// 結果が環境によって変わってしまう。
fn sidecar_paths(data_dir: Option<&Path>) -> Vec<PathBuf> {
    let Some(plugin_dir) = data_dir.and_then(plugin_directory) else {
        return Vec::new();
    };
    let mut paths = sidecars_in(&plugin_dir);
    for directory in subdirectories(&plugin_dir) {
        paths.extend(sidecars_in(&directory));
    }
    paths.sort();
    paths
}

/// データディレクトリ配下のプラグインディレクトリを正規化して得る。
///
/// **デバイス名の置換が起きない形（`\\?\` 前置）へ正規化する。** 正規化は
/// ディレクトリ 1 つにつき 1 度だけ行い、サブディレクトリのパスは列挙が返す
/// エントリから組み立てるため、この 1 度で全件に効く。
///
/// 存在しなければ `None` を返す。不在は「ディレクトリが違う」ことも「プラグインを
/// 1 つも入れていない」ことも意味し、我々には区別できない。
fn plugin_directory(data_dir: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(data_dir.join(PLUGIN_DIRECTORY)).ok()
}

/// ディレクトリ直下のサイドカーを集める。
///
/// ディレクトリは除く。判定は列挙が返した属性で決まり、ファイルを開かない。
/// 名前の大小は問わない——配布物のファイル名の大小まで書き手へ課さない。
fn sidecars_in(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| !kind.is_dir()).unwrap_or(true))
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.to_ascii_lowercase().ends_with(SIDECAR_SUFFIX))
        })
        .map(|entry| entry.path())
        .collect()
}

/// ディレクトリ直下のサブディレクトリを集める。
fn subdirectories(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .map(|entry| entry.path())
        .collect()
}

/// サイドカーを 1 件読んで解釈する。
///
/// 読めない・上限を超える・JSON として解釈できないのはいずれも `None` になる。
/// 呼び出し側はその 1 件を落とし、他のファイルの候補には触れない。
fn read_document(path: &Path) -> Option<ChoicesDocument> {
    let bytes = read_bounded(path, MAX_SIDECAR_BYTES).ok()??;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;

    /// テスト用のデータディレクトリ。抜けるときに消す。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir()
                .join(format!("aviutl2-mcp-item-choices-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(dir.join(PLUGIN_DIRECTORY)).expect("作れる");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        /// `Plugin` 直下へサイドカーを置く。
        fn write_sidecar(&self, name: &str, contents: &str) {
            fs::write(self.0.join(PLUGIN_DIRECTORY).join(name), contents).expect("書ける");
        }

        /// `Plugin` 配下の相対パスへファイルを置く。親ディレクトリは作る。
        fn write_nested(&self, relative: &str, contents: &str) {
            let path = self.0.join(PLUGIN_DIRECTORY).join(relative);
            fs::create_dir_all(path.parent().expect("親がある")).expect("作れる");
            fs::write(path, contents).expect("書ける");
        }

        fn load(&self) -> ChoicesTable {
            load_table(Some(self.path()))
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// 表 1 つを (効果, 項目, 候補) の並びから組み立てる。
    fn document(entries: &[(&str, &str, &[&str])]) -> String {
        let mut effects = serde_json::Map::new();
        for (effect, item, values) in entries {
            let items = effects
                .entry((*effect).to_string())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            items
                .as_object_mut()
                .expect("object として入れた")
                .insert((*item).to_string(), serde_json::json!(values));
        }
        serde_json::json!({ "effects": effects }).to_string()
    }

    /// 候補の値だけを取り出す。
    fn values_of(table: &ChoicesTable, effect: &str, item: &str) -> Option<Vec<String>> {
        Some(table.get(effect, item)?.values.clone())
    }

    /// 表が持つ (効果, 項目) の集合。
    fn entries(table: &ChoicesTable) -> BTreeSet<(String, String)> {
        table
            .effects
            .iter()
            .flat_map(|(effect, items)| {
                items.keys().map(move |item| (effect.clone(), item.clone()))
            })
            .collect()
    }

    #[test]
    fn the_builtin_table_is_valid_json() {
        // `include_str!` は文字列を取り込むだけで、JSON として解釈できることを
        // 保証しない。基底が解釈できなければ候補は 1 件も出ない。
        let document: ChoicesDocument =
            serde_json::from_str(BUILTIN_TABLE).expect("基底を JSON として解釈できません");
        let mut table = ChoicesTable::default();
        table.overlay(document, ChoicesSource::BuiltinTable);
        assert_eq!(table.entry_count(), builtin_table().entry_count());
    }

    #[test]
    fn the_builtin_table_is_in_place_before_any_sidecar() {
        // 基底はパスを持たないため走査順の影響を受けない。サイドカーが 1 件も
        // 無い環境でも、基底の候補はそのまま引ける。
        let dir = TempDir::new();
        let table = dir.load();
        assert_eq!(table.entry_count(), builtin_table().entry_count());
    }

    #[test]
    fn a_sidecar_replaces_the_values_instead_of_adding_to_them() {
        // 加算では基底の誤りを利用者が消せず、結果も予測しにくい。
        let mut table = ChoicesTable::default();
        table.overlay(
            serde_json::from_str(&document(&[(
                "テキスト",
                "文字揃え",
                &["左寄せ[上]", "中央揃え[中]"],
            )]))
            .unwrap(),
            ChoicesSource::BuiltinTable,
        );
        let report = table.overlay(
            serde_json::from_str(&document(&[("テキスト", "文字揃え", &["右寄せ[下]"])])).unwrap(),
            ChoicesSource::Sidecar,
        );

        assert_eq!(
            values_of(&table, "テキスト", "文字揃え"),
            Some(vec!["右寄せ[下]".to_string()])
        );
        assert_eq!(
            report.replaced,
            vec![("テキスト".to_string(), "文字揃え".to_string())]
        );
    }

    #[test]
    fn a_sidecar_leaves_the_other_items_of_the_same_effect_alone() {
        // 置換の粒度は (効果, 項目) である。効果ごと差し替えると、1 項目を直す
        // だけのサイドカーが同じ効果の他の項目を消す。
        let dir = TempDir::new();
        dir.write_sidecar(
            &format!("基底役{SIDECAR_SUFFIX}"),
            &document(&[
                ("テキスト", "文字揃え", &["左寄せ[上]"][..]),
                ("テキスト", "文字装飾", &["標準文字", "影付き文字"][..]),
            ]),
        );
        dir.write_sidecar(
            &format!("直す役{SIDECAR_SUFFIX}"),
            &document(&[("テキスト", "文字揃え", &["中央揃え[中]"])]),
        );

        let table = dir.load();
        assert_eq!(
            values_of(&table, "テキスト", "文字揃え"),
            Some(vec!["中央揃え[中]".to_string()])
        );
        assert_eq!(
            values_of(&table, "テキスト", "文字装飾"),
            Some(vec!["標準文字".to_string(), "影付き文字".to_string()])
        );
    }

    #[test]
    fn the_last_sidecar_in_path_order_wins() {
        // 並びは列挙の順ではなくパスの昇順で決まる。ファイルシステムの返す順に
        // 頼ると、同じ組み合わせが環境によって別の結果になる。
        let dir = TempDir::new();
        for name in ["3", "1", "2"] {
            dir.write_sidecar(
                &format!("{name}{SIDECAR_SUFFIX}"),
                &document(&[("テキスト", "文字揃え", &[name])]),
            );
        }

        let table = dir.load();
        assert_eq!(
            values_of(&table, "テキスト", "文字揃え"),
            Some(vec!["3".to_string()])
        );

        // 置いた順を変えても結果は変わらない。
        let reversed = TempDir::new();
        for name in ["2", "1", "3"] {
            reversed.write_sidecar(
                &format!("{name}{SIDECAR_SUFFIX}"),
                &document(&[("テキスト", "文字揃え", &[name])]),
            );
        }
        assert_eq!(
            values_of(&reversed.load(), "テキスト", "文字揃え"),
            Some(vec!["3".to_string()])
        );
    }

    #[test]
    fn the_scan_reaches_one_level_of_subdirectories_and_no_further() {
        // フォルダごと配布されるプラグインが実在するため 1 段は辿る。2 段以上を
        // 要する形は無く、辿れば走査の費用だけが増える。
        let dir = TempDir::new();
        dir.write_nested(
            &format!("直下{SIDECAR_SUFFIX}"),
            &document(&[("直下", "項目", &["値"])]),
        );
        dir.write_nested(
            &format!("一段目/中{SIDECAR_SUFFIX}"),
            &document(&[("一段目", "項目", &["値"])]),
        );
        dir.write_nested(
            &format!("一段目/二段目/奥{SIDECAR_SUFFIX}"),
            &document(&[("二段目", "項目", &["値"])]),
        );

        let table = dir.load();
        assert!(table.get("直下", "項目").is_some());
        assert!(table.get("一段目", "項目").is_some());
        assert_eq!(
            table.get("二段目", "項目"),
            None,
            "2 段目まで走査しています"
        );
    }

    #[test]
    fn the_source_says_where_the_values_came_from() {
        // 由来は取り込み方そのもので決まる。ファイル名からは見分けられない。
        let mut table = ChoicesTable::default();
        table.overlay(
            serde_json::from_str(&document(&[("テキスト", "文字揃え", &["左寄せ[上]"])])).unwrap(),
            ChoicesSource::BuiltinTable,
        );
        assert_eq!(
            table.get("テキスト", "文字揃え").map(|c| c.source),
            Some(ChoicesSource::BuiltinTable)
        );

        let dir = TempDir::new();
        dir.write_sidecar(
            &format!("提供者{SIDECAR_SUFFIX}"),
            &document(&[("テキスト", "文字揃え", &["中央揃え[中]"])]),
        );
        assert_eq!(
            dir.load()
                .get("テキスト", "文字揃え")
                .map(|choices| choices.source),
            Some(ChoicesSource::Sidecar)
        );
    }

    #[test]
    fn a_broken_sidecar_only_costs_its_own_entries() {
        // 書き手が第三者である以上、品質は保証できない。壊れた 1 件のために
        // 他のファイルの候補まで落とさない。
        let dir = TempDir::new();
        dir.write_sidecar(&format!("壊れている{SIDECAR_SUFFIX}"), "{\"effects\":");
        dir.write_sidecar(&format!("空{SIDECAR_SUFFIX}"), "");
        dir.write_sidecar(&format!("配列{SIDECAR_SUFFIX}"), "[]");
        dir.write_sidecar(
            &format!("型が違う{SIDECAR_SUFFIX}"),
            "{\"effects\":{\"テキスト\":{\"文字揃え\":1}}}",
        );
        dir.write_sidecar(
            &format!("正しい{SIDECAR_SUFFIX}"),
            &document(&[("テキスト", "文字揃え", &["中央揃え[中]"])]),
        );

        let table = dir.load();
        assert_eq!(
            values_of(&table, "テキスト", "文字揃え"),
            Some(vec!["中央揃え[中]".to_string()])
        );
    }

    #[test]
    fn an_unknown_field_costs_only_itself() {
        // 未知のフィールドだけを無視する。ファイルごと落とすと、我々が知らない
        // 欄を 1 つ持つだけで候補が全部消える。
        let dir = TempDir::new();
        dir.write_sidecar(
            &format!("未知の欄{SIDECAR_SUFFIX}"),
            "{\"notice\":\"読む人は居ません\",\"version\":3,\"effects\":{\"テキスト\":{\"文字揃え\":[\"中央揃え[中]\"]}}}",
        );

        assert_eq!(
            values_of(&dir.load(), "テキスト", "文字揃え"),
            Some(vec!["中央揃え[中]".to_string()])
        );
    }

    #[test]
    fn an_effect_that_does_not_exist_here_is_kept_without_complaint() {
        // 利用者は複数の環境で同じファイルを使う。この環境に無い effect の項目を
        // 失敗として扱えば、環境をまたぐ配布物が使えなくなる。読み取り経路は
        // ホストの列挙に現れた項目だけを引くため、余った項目は応答に出ない。
        let dir = TempDir::new();
        dir.write_sidecar(
            &format!("別環境{SIDECAR_SUFFIX}"),
            &document(&[("この環境に無い効果", "項目", &["値"])]),
        );

        let table = dir.load();
        assert!(table.get("この環境に無い効果", "項目").is_some());
        assert_eq!(table.effect("入っていない効果"), None);
    }

    #[test]
    fn only_the_files_that_carry_the_suffix_are_read() {
        // 規則に合わない名前は読まない。プラグイン本体や設定ファイルが同居する
        // ディレクトリを走査するため、名前だけが我々のファイルの目印である。
        let dir = TempDir::new();
        dir.write_sidecar("設定.json", &document(&[("読まない", "項目", &["値"])]));
        dir.write_sidecar("plugin.ini", &document(&[("読まない", "項目", &["値"])]));
        dir.write_sidecar(
            &format!("大文字{}", SIDECAR_SUFFIX.to_uppercase()),
            &document(&[("読む", "項目", &["値"])]),
        );
        // ディレクトリは名前が合っていても開かない。
        fs::create_dir_all(
            dir.path()
                .join(PLUGIN_DIRECTORY)
                .join(format!("紛らわしい{SIDECAR_SUFFIX}")),
        )
        .expect("作れる");

        let table = dir.load();
        let mut expected = entries(&builtin_table());
        expected.insert(("読む".to_string(), "項目".to_string()));
        assert_eq!(entries(&table), expected);
    }

    #[test]
    fn a_file_over_the_limit_is_not_read() {
        let dir = TempDir::new();
        let head = document(&[("テキスト", "文字揃え", &["中央揃え[中]"])]);
        let mut text = head.clone();
        // 上限をちょうど 1 バイト超える。JSON としては壊れるが、上限の判定は
        // 中身を読む前に効く。
        text.push_str(&" ".repeat(MAX_SIDECAR_BYTES as usize + 1 - head.len()));
        assert_eq!(text.len() as u64, MAX_SIDECAR_BYTES + 1);
        dir.write_sidecar(&format!("巨大{SIDECAR_SUFFIX}"), &text);

        assert_eq!(dir.load(), builtin_table());
    }

    #[test]
    fn a_missing_plugin_directory_leaves_the_builtin_table_alone() {
        let dir = TempDir::new();
        fs::remove_dir_all(dir.path().join(PLUGIN_DIRECTORY)).expect("消せる");
        assert_eq!(dir.load(), builtin_table());
        assert_eq!(load_table(None), builtin_table());
    }
}
