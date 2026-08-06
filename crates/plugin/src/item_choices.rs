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
//! 基底はバイナリへ取り込む。走査を持たず、書き手は我々である。
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
//! 2. サイドカーを、**直下を先に、サブディレクトリを後に**重ねる。どちらの中でも
//!    名前の大小を無視した昇順で重ねる。
//! 3. 置換は (効果, 項目) の粒度で行う。**加算しない**——加算では基底の誤りを
//!    利用者が消せず、結果も予測しにくい。
//! 4. 同じ (効果, 項目) を複数のファイルが主張したら、後に来たものが勝つ。
//!    決定的であり、ログへ 1 行残す。
//!
//! **深いほうを後に重ねるのは、より個別の主張だからである。** サブディレクトリの
//! サイドカーはフォルダごと配布されたプラグインに属し、そのプラグインの項目に
//! ついて述べている。直下のサイドカーはどの配布物にも属さない。
//!
//! **名前の大小で優劣を決めない。** Windows のファイルシステムは大小を区別せず、
//! `AAA` と `aaa` は同じ名前として扱われる。バイト順で並べると、同じ名前の
//! つもりで置いたファイルが綴りの大小だけで前後し、勝ち負けが反転する。
//!
//! # 厳しさを分ける
//!
//! **基底とサイドカーで受け入れ方が違う。** 分けるのは、書き手が誰かと、壊れが
//! いつ分かるかが違うためである。
//!
//! 基底は我々が生成してバイナリへ焼き込む。形を外したことは我々の誤りであり、
//! 検査とビルドの時点で分かるべきである。そこで**未知のトップレベルフィールドも
//! `effects` の欠落も拒む**——素通しにすると、綴りを 1 文字外しただけで表が
//! 丸ごと空になり、候補が全件 `null` へ戻ったことに誰も気付けない。
//!
//! サイドカーは第三者が書き、壊れが分かるのは利用者の環境で読んだ瞬間である。
//! 我々には直す手が無く、1 件の誤りで他のファイルまで落とす理由も無い。そこで
//! **未知のフィールドは無視する**——[`aviutl2_mcp_core::ItemValue`] やセレクター
//! が受け取る側で既に取っている方針と揃える。
//!
//! # 壊れていても失敗させない
//!
//! 書き手が第三者である以上、中身の品質を我々が保証することはできない。
//!
//! - 開けない: そのファイルを丸ごと無視し、ログへ 1 行残す
//! - 上限を超える: 同じく丸ごと無視する。**開けないこととは別の文言で残す**
//! - JSON として解釈できない: 同じく丸ごと無視し、別の文言で残す
//! - 未知のフィールド: そのフィールドだけを無視する
//! - この環境に存在しない effect の項目: 黙って無視する。利用者が複数の環境で
//!   同じファイルを使うことがある
//! - 同じ (効果, 項目) の重複: 上記の後勝ちで解決する
//!
//! いずれも tool の失敗にしない。**この扱いは基底には要らない**——埋め込みは
//! 走査を持たず、壊れ方も上の 3 つとは別の意味を持つ（[`builtin_table`]）。
//!
//! # 合計の費用に上限を置く
//!
//! 走査するのは N ファイルであり、1 件あたりの上限だけでは合計が決まらない。
//! [`MAX_SIDECAR_FILES`] と [`MAX_SIDECAR_BYTES`] の 2 つで頭打ちにする——読む
//! バイト数の合計は両者の積を超えず、表の大きさは読んだバイト数を超えない。
//! **上限は要求元が動かせない。** この経路の費用は要求の内容で決まらないため、
//! 予算では守れない（[`crate::movement`] と [`crate::effect_help`] が 1 ファイル
//! について同じ扱いをしている）。
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
//! - `effects` は効果名から設定項目の表を引く。**基底では省略できない。**
//! - 設定項目の表は、項目名から候補の配列を引く。候補は文字列であり、書いた順
//!   がそのまま応答へ出る。
//! - 基底はこれ以外のキーを持てない。サイドカーでは無視される。読む人の居ない
//!   注記も、算出に全 effect × 全項目の列挙を要する陳腐化の印も持たない。

use crate::alias::{data_directory, read_bounded};
use aviutl2_mcp_core::{ChoicesSource, ItemChoices};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 埋め込む基底の表。
const BUILTIN_TABLE: &str = include_str!("../data/effect_item_choices.json");

/// サイドカーを探すディレクトリの名前。
const PLUGIN_DIRECTORY: &str = "Plugin";

/// サイドカーのファイル名の末尾。
///
/// **第三者に対する契約である。** 変えれば既存のサイドカーが読まれなくなる。
pub(crate) const SIDECAR_SUFFIX: &str = ".aviutl2-mcp.json";

/// サイドカー 1 件として読み込む最大バイト数。
pub(crate) const MAX_SIDECAR_BYTES: u64 = 1024 * 1024;

/// 読み込むサイドカーの最大件数。
///
/// 重ねる順の先頭から数える。超えた分は読まない——落ちるのは後に来るファイル、
/// つまり優劣で勝つ側だが、どちらを落としても失われるものはある。**決定的で
/// あることのほうが大事である。**
pub(crate) const MAX_SIDECAR_FILES: usize = 64;

/// 表の中身。効果名から設定項目名と候補の対応を引く。
type ChoicesEntries = HashMap<String, HashMap<String, Vec<String>>>;

/// 埋め込む基底 1 つの外形。
///
/// **未知のトップレベルフィールドも `effects` の欠落も拒む。** 書き手は我々で
/// あり、形を外したのなら生成器の誤りである。素通しにすると、キーを 1 文字
/// 綴り違えただけで表が丸ごと空になり、候補が全件 `null` へ戻る。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuiltinDocument {
    effects: ChoicesEntries,
}

/// サイドカー 1 つの外形。
///
/// **未知のフィールドは無視し、`effects` の欠落も受け入れる。** 書き手は第三者
/// であり、我々が知らない欄を持つファイルでも知っている欄の分は使える。
#[derive(Debug, Default, Deserialize)]
struct SidecarDocument {
    #[serde(default)]
    effects: ChoicesEntries,
}

/// サイドカーを使えなかった理由。
///
/// **1 つの文言へ畳まない。** 上限を超えたファイルと綴りを外したファイルは
/// 書き手が次に取る行動が違う（[`crate::effect_help`] が同じ理由で失敗の種別を
/// 分けている）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidecarRejection {
    /// 開けない。
    Unreadable,
    /// [`MAX_SIDECAR_BYTES`] を超えている。
    TooLarge,
    /// JSON として解釈できない。
    NotParsable,
}

impl fmt::Display for SidecarRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SidecarRejection::Unreadable => f.write_str("開けませんでした"),
            SidecarRejection::TooLarge => {
                write!(f, "上限 {MAX_SIDECAR_BYTES} バイトを超えています")
            }
            SidecarRejection::NotParsable => f.write_str("JSON として解釈できませんでした"),
        }
    }
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
pub(crate) struct ChoicesTable {
    effects: HashMap<String, HashMap<String, ItemChoices>>,
}

impl ChoicesTable {
    /// 効果 1 件分の候補を、設定項目名から引ける形で返す。
    ///
    /// 表に無い効果は `None` である。この環境に存在しない効果が表に在ることも、
    /// その逆もある。
    pub(crate) fn effect(&self, effect_name: &str) -> Option<&HashMap<String, ItemChoices>> {
        self.effects.get(effect_name)
    }

    /// 表が持つ (効果, 項目) の数。
    pub(crate) fn entry_count(&self) -> usize {
        self.effects.values().map(HashMap::len).sum()
    }

    /// 表を 1 つ重ねる。
    ///
    /// **置換は (効果, 項目) の粒度である。** 効果ごと差し替えれば、1 項目を
    /// 直したいだけのサイドカーが同じ効果の他の項目を消す。候補の配列そのものは
    /// 丸ごと入れ替える——既存の候補へ継ぎ足す形にすると、基底の誤りを消す手が
    /// 無くなる。
    fn overlay(&mut self, entries: ChoicesEntries, source: ChoicesSource) -> OverlayReport {
        let mut report = OverlayReport::default();
        for (effect_name, items) in entries {
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
pub(crate) fn table() -> &'static ChoicesTable {
    TABLE.get_or_init(|| load_table(data_directory()))
}

/// 埋め込みの基底へサイドカーを重ねた表を組み立てる。
///
/// `data_dir` は AviUtl2 のデータディレクトリである。解決できない環境では基底
/// だけの表になる。
///
/// 件数はログへ残す。基底とサイドカーのどちらが何件を持ち込んだのかが分から
/// なければ、応答に現れた候補の出所を後から辿れない。
pub(crate) fn load_table(data_dir: Option<&Path>) -> ChoicesTable {
    let mut table = builtin_table();
    let builtin_entries = table.entry_count();

    let mut paths = sidecar_paths(data_dir);
    if paths.len() > MAX_SIDECAR_FILES {
        tracing::info!(
            "サイドカーが {} 件あります。重ねる順の先頭 {MAX_SIDECAR_FILES} 件だけを読みます",
            paths.len()
        );
        paths.truncate(MAX_SIDECAR_FILES);
    }

    let mut files = 0usize;
    let mut applied = 0usize;
    let mut replaced = 0usize;
    for path in paths {
        let document = match read_document(&path) {
            Ok(document) => document,
            Err(rejection) => {
                tracing::info!(
                    "{} を選択肢の表として使いません: {rejection}",
                    path.display()
                );
                continue;
            }
        };
        files += 1;
        let report = table.overlay(document.effects, ChoicesSource::Sidecar);
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
/// **解釈できなければ panic する。** 同じ「供給源が読めない」でも、
/// [`crate::effect_help`] の `Default.aul2` や [`crate::movement`] の
/// `aviutl2.ini` とは扱いを変えている。あちらはホストと利用者の環境にある
/// ファイルであり、無いことも壊れていることも正常な状態のひとつで、我々に直す
/// 手が無い。こちらは我々が生成してビルドへ焼き込んだものであり、壊れていれば
/// 我々の誤りである。空へ畳むと、候補が全件 `null` になった状態が「候補を持た
/// ない環境」と見分けられないまま出荷される。
///
/// **プロセスは落ちない。** 読み取り経路の捕捉層がここを包んでおり、要求は
/// `internal` として返る。[`OnceLock`] は毒されないため要求のたびに再試行し、
/// 毎回同じ失敗になる。
///
/// この経路は本番では起こらない。構文と中身の不変条件はこのモジュールの検査が
/// 押さえており、外れていればビルドの前に落ちる。
fn builtin_table() -> ChoicesTable {
    let document: BuiltinDocument =
        parse_object(BUILTIN_TABLE.as_bytes()).expect("埋め込んだ基底の表を解釈できません");
    let mut table = ChoicesTable::default();
    table.overlay(document.effects, ChoicesSource::BuiltinTable);
    table
}

/// サイドカーのパスを重ねる順に集める。
///
/// 走査するのは `Plugin` 直下と、そのサブディレクトリ 1 段だけである。**直下を
/// 先に、サブディレクトリを後に並べる**——深いほうがより個別の主張であり、
/// 後に来たものが勝つ。並びは列挙の順に頼らない。列挙の順はファイルシステムが
/// 決めるものであり、後勝ちの結果が環境によって変わってしまう。
fn sidecar_paths(data_dir: Option<&Path>) -> Vec<PathBuf> {
    let Some(plugin_dir) = data_dir.and_then(plugin_directory) else {
        return Vec::new();
    };
    let mut paths = sorted_by_name(sidecars_in(&plugin_dir));
    for directory in sorted_by_name(subdirectories(&plugin_dir)) {
        paths.extend(sorted_by_name(sidecars_in(&directory)));
    }
    paths
}

/// 名前の大小を無視した昇順へ並べる。
///
/// **バイト順では並べない。** Windows のファイルシステムは大小を区別しないため、
/// `AAA` と `aaa` は同じ名前である。バイト順は大文字を小文字より前へ置き、
/// 同じ名前のつもりで置いたファイルが綴りの大小だけで前後する。
///
/// 大小を無視して同じになる名前は同じディレクトリに共存できないため、この鍵で
/// 並びは一意に決まる。
fn sorted_by_name(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort_by_key(|path| {
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
    });
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
///
/// **リンクは除かない** — 開けば実体へ届くものを、名前の段で落とさない。名前が
/// 規則に合うジャンクションはここを通り、開く段で落ちる（[`read_document`] が
/// 「開けない」として扱う）。
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
///
/// ここではリンクを除く。ディレクトリへのジャンクションは属性の判定を通らず、
/// 走査は 1 段を超えない。
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
/// 失敗の 3 種は畳まずに返す。呼び出し側はその 1 件を落とし、他のファイルの
/// 候補には触れない。
fn read_document(path: &Path) -> Result<SidecarDocument, SidecarRejection> {
    let bytes = match read_bounded(path, MAX_SIDECAR_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Err(SidecarRejection::TooLarge),
        Err(_) => return Err(SidecarRejection::Unreadable),
    };
    parse_object(&bytes).ok_or(SidecarRejection::NotParsable)
}

/// JSON 全体がオブジェクトであることを確かめてから型へ写す。
///
/// **serde の struct はシーケンス形の入力も受け付ける。** `[]` のような配列は
/// 表ではないが、そのまま渡すとフィールドの既定値だけを持つ表として通り、
/// 書き手には何も伝わらないまま候補が 0 件になる。
fn parse_object<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if !value.is_object() {
        return None;
    }
    serde_json::from_value(value).ok()
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

        /// `Plugin` 直下のパスを組み立てる。
        fn sidecar_path(&self, name: &str) -> PathBuf {
            self.0.join(PLUGIN_DIRECTORY).join(name)
        }

        /// `Plugin` 直下へサイドカーを置く。
        fn write_sidecar(&self, name: &str, contents: &str) {
            fs::write(self.sidecar_path(name), contents).expect("書ける");
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

    /// 表の中身を、サイドカーとして解釈して取り出す。
    fn parsed(text: &str) -> ChoicesEntries {
        serde_json::from_str::<SidecarDocument>(text)
            .expect("解釈できる")
            .effects
    }

    /// 効果と設定項目を指して 1 件を引く。
    fn choices_of<'a>(
        table: &'a ChoicesTable,
        effect: &str,
        item: &str,
    ) -> Option<&'a ItemChoices> {
        table.effect(effect)?.get(item)
    }

    /// 候補の値だけを取り出す。
    fn values_of(table: &ChoicesTable, effect: &str, item: &str) -> Option<Vec<String>> {
        Some(choices_of(table, effect, item)?.values.clone())
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
        assert!(
            parse_object::<BuiltinDocument>(BUILTIN_TABLE.as_bytes()).is_some(),
            "基底を JSON として解釈できません"
        );
    }

    #[test]
    fn the_builtin_table_carries_choices_that_are_worth_returning() {
        // **件数を数え合わせるだけの検査にしない。** 同じ文字列を同じ経路で
        // 2 度読んで比べても恒真であり、表が空になっても通ってしまう。ここで
        // 見るのは、表が育っても成り立つ性質だけである——値そのものを書けば
        // 生成のたびに壊れる。
        let table = builtin_table();
        assert!(
            table.entry_count() > 0,
            "基底が空です。綴りを外したキーは黙って全件を落とします"
        );

        for (effect_name, items) in &table.effects {
            assert!(
                !items.is_empty(),
                "{effect_name} が設定項目を 1 つも持ちません"
            );
            for (item_name, choices) in items {
                assert!(
                    !choices.values.is_empty(),
                    "{effect_name} の {item_name} の候補が空です"
                );
                let unique: BTreeSet<&String> = choices.values.iter().collect();
                assert_eq!(
                    unique.len(),
                    choices.values.len(),
                    "{effect_name} の {item_name} の候補が重複しています"
                );
                assert!(
                    choices.values.iter().all(|value| !value.is_empty()),
                    "{effect_name} の {item_name} が空の候補を持ちます"
                );
                assert_eq!(
                    choices.source,
                    ChoicesSource::BuiltinTable,
                    "{effect_name} の {item_name} の由来が基底になっていません"
                );
            }
        }
    }

    #[test]
    fn the_builtin_table_refuses_a_shape_it_did_not_intend() {
        // 書き手は我々である。トップレベルのキーを綴り違えた表を素通しにすると、
        // 候補が全件 null へ戻ったことに誰も気付けない。サイドカーは同じ入力を
        // 受け入れる——書き手が第三者であり、知らない欄を持つファイルでも
        // 知っている欄の分は使えるためである。
        for source in [
            // `effects` の綴り違い。
            r#"{"efects":{"テキスト":{"文字揃え":["中央揃え[中]"]}}}"#,
            // 知らない欄が増えた。
            r#"{"effects":{},"notice":"読む人は居ません"}"#,
            // `effects` そのものが無い。
            "{}",
        ] {
            assert!(
                parse_object::<BuiltinDocument>(source.as_bytes()).is_none(),
                "基底が {source} を受け入れました"
            );
            assert!(
                parse_object::<SidecarDocument>(source.as_bytes()).is_some(),
                "サイドカーが {source} を拒みました"
            );
        }

        // オブジェクト以外はどちらも表ではない。serde の struct はシーケンス形の
        // 入力も受け付けるため、明示的に退けなければ `[]` が空の表として通る。
        for source in ["[]", "3", "null", "\"表\""] {
            assert!(
                parse_object::<BuiltinDocument>(source.as_bytes()).is_none(),
                "基底が {source} を受け入れました"
            );
            assert!(
                parse_object::<SidecarDocument>(source.as_bytes()).is_none(),
                "サイドカーが {source} を受け入れました"
            );
        }
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
            parsed(&document(&[(
                "テキスト",
                "文字揃え",
                &["左寄せ[上]", "中央揃え[中]"],
            )])),
            ChoicesSource::BuiltinTable,
        );
        let report = table.overlay(
            parsed(&document(&[("テキスト", "文字揃え", &["右寄せ[下]"])])),
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
    fn the_last_sidecar_in_name_order_wins() {
        // 並びは列挙の順ではなく名前の昇順で決まる。ファイルシステムの返す順に
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
    fn the_order_of_two_sidecars_does_not_depend_on_the_case_of_their_names() {
        // Windows のファイルシステムは大小を区別しない。バイト順で並べると
        // 大文字が先に来て、`A` `b` `C` では `b` が最後になる。大小を無視した
        // 昇順なら最後は `C` である。
        let dir = TempDir::new();
        for name in ["A", "b", "C"] {
            dir.write_sidecar(
                &format!("{name}{SIDECAR_SUFFIX}"),
                &document(&[("テキスト", "文字揃え", &[name])]),
            );
        }

        assert_eq!(
            values_of(&dir.load(), "テキスト", "文字揃え"),
            Some(vec!["C".to_string()])
        );
    }

    #[test]
    fn a_sidecar_in_a_subdirectory_wins_over_one_directly_in_the_plugin_directory() {
        // 深いほうが後に来る。サブディレクトリのサイドカーはフォルダごと配布
        // されたプラグインに属し、そのプラグインの項目について述べている。
        //
        // **直下の名前の大小で反転しない。** バイト順で並べると `AAA` は
        // サブディレクトリの前に、`zzz` は後に来て、同じ名前のつもりで置いた
        // ファイルが綴りだけで勝ち負けを変える。
        for name in ["AAA", "aaa", "ZZZ", "zzz"] {
            let dir = TempDir::new();
            dir.write_sidecar(
                &format!("{name}{SIDECAR_SUFFIX}"),
                &document(&[("テキスト", "文字揃え", &["直下"])]),
            );
            dir.write_nested(
                &format!("mmm/mmm{SIDECAR_SUFFIX}"),
                &document(&[("テキスト", "文字揃え", &["サブ"])]),
            );

            assert_eq!(
                values_of(&dir.load(), "テキスト", "文字揃え"),
                Some(vec!["サブ".to_string()]),
                "直下の名前が {name} のときに優劣が変わりました"
            );
        }
    }

    #[test]
    fn the_last_subdirectory_in_name_order_wins() {
        // サブディレクトリ同士の優劣も名前で決まる。ディレクトリの列挙順に
        // 頼ると、同じ組み合わせが環境によって別の結果になる。
        let dir = TempDir::new();
        for name in ["B", "a", "C"] {
            dir.write_nested(
                &format!("{name}/表{SIDECAR_SUFFIX}"),
                &document(&[("テキスト", "文字揃え", &[name])]),
            );
        }

        assert_eq!(
            values_of(&dir.load(), "テキスト", "文字揃え"),
            Some(vec!["C".to_string()])
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
        assert!(choices_of(&table, "直下", "項目").is_some());
        assert!(choices_of(&table, "一段目", "項目").is_some());
        assert_eq!(
            choices_of(&table, "二段目", "項目"),
            None,
            "2 段目まで走査しています"
        );
    }

    #[test]
    fn the_source_says_where_the_values_came_from() {
        // 由来は取り込み方そのもので決まる。ファイル名からは見分けられない。
        let mut table = ChoicesTable::default();
        table.overlay(
            parsed(&document(&[("テキスト", "文字揃え", &["左寄せ[上]"])])),
            ChoicesSource::BuiltinTable,
        );
        assert_eq!(
            choices_of(&table, "テキスト", "文字揃え").map(|choices| choices.source),
            Some(ChoicesSource::BuiltinTable)
        );

        let dir = TempDir::new();
        dir.write_sidecar(
            &format!("提供者{SIDECAR_SUFFIX}"),
            &document(&[("テキスト", "文字揃え", &["中央揃え[中]"])]),
        );
        assert_eq!(
            choices_of(&dir.load(), "テキスト", "文字揃え").map(|choices| choices.source),
            Some(ChoicesSource::Sidecar)
        );
    }

    /// JSON として解釈できないサイドカーの壊れ方。
    ///
    /// **1 つの検査へ束ねない。** 束ねると、どれか 1 つだけ退行したときに、
    /// どの壊れ方が通ってしまったのかを名指しできない。
    const BROKEN_SIDECARS: [(&str, &str); 4] = [
        ("途中で切れている", "{\"effects\":"),
        ("空", ""),
        ("配列", "[]"),
        ("型が違う", "{\"effects\":{\"テキスト\":{\"文字揃え\":1}}}"),
    ];

    #[test]
    fn a_broken_sidecar_only_costs_its_own_entries() {
        // 書き手が第三者である以上、品質は保証できない。壊れた 1 件のために
        // 他のファイルの候補まで落とさない。壊れ方ごとに 1 件ずつ確かめる。
        for (name, contents) in BROKEN_SIDECARS {
            let dir = TempDir::new();
            dir.write_sidecar(&format!("{name}{SIDECAR_SUFFIX}"), contents);
            // 基底が主張していない効果を使う。基底に在る組を上書きすると、
            // 件数が増えないまま通ってしまう。
            dir.write_sidecar(
                &format!("正しい{SIDECAR_SUFFIX}"),
                &document(&[("提供者の効果", "文字揃え", &["中央揃え[中]"])]),
            );

            let table = dir.load();
            assert_eq!(
                values_of(&table, "提供者の効果", "文字揃え"),
                Some(vec!["中央揃え[中]".to_string()]),
                "{name} が他のファイルの候補まで落としました"
            );
            assert_eq!(
                table.entry_count(),
                builtin_table().entry_count() + 1,
                "{name} が候補を持ち込みました"
            );
        }
    }

    #[test]
    fn the_three_ways_of_failing_are_told_apart() {
        // 上限を超えたファイルと綴りを外したファイルでは、書き手が次に取る行動が
        // 違う。同じ文言に畳むと、どれが起きたのかをログから切り分けられない。
        let dir = TempDir::new();

        for (name, contents) in BROKEN_SIDECARS {
            let name = format!("{name}{SIDECAR_SUFFIX}");
            dir.write_sidecar(&name, contents);
            assert_eq!(
                read_document(&dir.sidecar_path(&name)).err(),
                Some(SidecarRejection::NotParsable),
                "{name}"
            );
        }

        let oversized = format!("巨大{SIDECAR_SUFFIX}");
        dir.write_sidecar(&oversized, &" ".repeat(MAX_SIDECAR_BYTES as usize + 1));
        assert_eq!(
            read_document(&dir.sidecar_path(&oversized)).err(),
            Some(SidecarRejection::TooLarge)
        );

        assert_eq!(
            read_document(&dir.sidecar_path(&format!("無い{SIDECAR_SUFFIX}"))).err(),
            Some(SidecarRejection::Unreadable)
        );

        // ログへ出るのは種別ごとに違う文言である。
        let texts: BTreeSet<String> = [
            SidecarRejection::Unreadable,
            SidecarRejection::TooLarge,
            SidecarRejection::NotParsable,
        ]
        .iter()
        .map(SidecarRejection::to_string)
        .collect();
        assert_eq!(texts.len(), 3, "失敗の文言が畳まれています: {texts:?}");
    }

    #[test]
    fn the_number_of_sidecars_that_are_read_is_capped() {
        // 走査するのは N ファイルであり、1 件あたりの上限だけでは合計が決まら
        // ない。読む件数は重ねる順の先頭から数え、超えた分は読まない。
        let dir = TempDir::new();
        for index in 0..MAX_SIDECAR_FILES + 2 {
            dir.write_sidecar(
                &format!("{index:03}{SIDECAR_SUFFIX}"),
                &document(&[(&format!("効果{index:03}"), "項目", &["値"])]),
            );
        }

        let table = dir.load();
        assert_eq!(
            table.entry_count(),
            builtin_table().entry_count() + MAX_SIDECAR_FILES
        );
        assert!(
            table
                .effect(&format!("効果{:03}", MAX_SIDECAR_FILES - 1))
                .is_some()
        );
        assert_eq!(table.effect(&format!("効果{MAX_SIDECAR_FILES:03}")), None);
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
        assert!(choices_of(&table, "この環境に無い効果", "項目").is_some());
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
