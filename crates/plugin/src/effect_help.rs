//! ホストが同梱する効果の説明。
//!
//! AviUtl2 の実行ファイルと同じディレクトリの `Default.aul2` が
//! `[Tips.<効果名>]` の節として並べている。SDK には説明を引く手段が無く、
//! このファイルだけが供給源である。
//!
//! **説明を我々が書き足すことはしない。** ここが返すのはホストが同梱した文言
//! そのものだけであり、供給源に無い効果の説明は `None` になる。推測で書いた
//! 説明は他人のソフトウェアについての検証できない主張であり、微妙に外した
//! ものを受け取った側は確信を持って誤用する。説明が無ければ慎重に扱われる。
//!
//! **説明は切り詰めない。** 発見の鍵が 2 行目以降に置かれている説明が実在する
//! ため、先頭行だけに切ると、説明を載せることで埋めようとしている欠落がその
//! まま残る。説明を持つのは供給元が挙げた効果に限られ、全文を載せても一覧の
//! 大きさは効果の登録数では決まらない。
//!
//! **ファイルの位置は SDK の契約の外にある。** 設定ハンドルが公開するのは
//! データディレクトリだけで、同梱ファイルはそこには無い。読み取り専用の参照に
//! 限ったうえで、ホストの実行ファイルの位置を Win32 から直接求める。解決にも
//! 読み取りにも失敗し得るが、失敗しても説明が出ないだけである——効果の選択肢
//! も設定項目の一覧も別の経路で得るため、一覧の中核はここに依存しない。
//! 失敗は応答へ出さず、ログへ 1 行残す。
//!
//! 読み込みは plugin の生存期間中に 1 度だけ行う（[`crate::alias::data_directory`]
//! や [`crate::movement::movements`] と同じ扱い）。ファイルはホストが同梱する
//! ものであり、内容は AviUtl2 の版で決まる。版を入れ替えればホストごと起動し
//! 直されて plugin も読み込み直されるため、要求のたびに開いても得るものが無い。

use crate::alias::read_bounded;
use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;

/// 説明を収めたファイルの名前。
const HELP_FILE: &str = "Default.aul2";

/// 効果 1 つの節の見出しが始まる並び。
const SECTION_PREFIX: &str = "[Tips.";

/// 効果そのものの説明を持つキーの名前。
///
/// `.` を含むが節の入れ子ではなく 1 つの値キーである。
const EFFECT_DESCRIPTION_KEY: &str = "effect.name";

/// 説明ファイルとして読み込む最大バイト数。
///
/// この経路の費用は要求の内容で決まらないため、予算では守れない。上限は
/// 要求元が動かせない。
pub const MAX_HELP_BYTES: u64 = 4 * 1024 * 1024;

/// 実行ファイルのパスを受け取る最初の長さ。
const INITIAL_PATH_LEN: usize = 260;

/// 実行ファイルのパスを受け取る領域の上限。
///
/// Windows のパスの上限を収める長さであり、これでも収まらない応答は解決の
/// 失敗として扱う。
const MAX_PATH_LEN: usize = 32 * 1024;

/// 解決した効果の説明。
static DESCRIPTIONS: OnceLock<HashMap<String, String>> = OnceLock::new();

/// 効果名に対応する説明を返す。供給源に無ければ `None`。
///
/// 初回の要求で 1 度だけ読み込む。説明が得られないことは plugin が起動できない
/// 理由ではないため、初期化時には読まない。
pub fn description_of(effect_name: &str) -> Option<&'static str> {
    descriptions().get(effect_name).map(String::as_str)
}

/// 効果名から説明を引く表を返す。読めなければ空の表になる。
fn descriptions() -> &'static HashMap<String, String> {
    DESCRIPTIONS.get_or_init(|| {
        let table = host_directory().map(read_descriptions).unwrap_or_default();
        if table.is_empty() {
            tracing::info!("効果の説明を読めませんでした。説明は返しません");
        } else {
            tracing::info!("効果の説明: {} 件", table.len());
        }
        table
    })
}

/// ホストの実行ファイルが置かれたディレクトリを返す。解決できなければ `None`。
fn host_directory() -> Option<PathBuf> {
    module_file_name()
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// 現在のプロセスの実行ファイルのパスを返す。
///
/// 受け取る領域は足りるまで広げる。**取得は書き込んだ長さしか返さない。**
/// 領域と同じ長さが返った場合は切り詰められた可能性があり、その値をパスとして
/// 扱うと別の場所を指す。
fn module_file_name() -> Option<PathBuf> {
    let mut buffer = vec![0u16; INITIAL_PATH_LEN];
    loop {
        // SAFETY: 第 1 引数の `None` は現在のプロセスの実行ファイルを指す。
        // `buffer` は呼び出し中を通じて生存する書き込み可能な領域であり、
        // 長さは呼び出し先へスライスとして渡る。
        let written = unsafe { GetModuleFileNameW(None, &mut buffer) } as usize;
        if written == 0 {
            return None;
        }
        if written < buffer.len() {
            return Some(PathBuf::from(OsString::from_wide(&buffer[..written])));
        }
        if buffer.len() >= MAX_PATH_LEN {
            return None;
        }
        buffer.resize((buffer.len() * 2).min(MAX_PATH_LEN), 0);
    }
}

/// ディレクトリ直下の説明ファイルを読む。失敗はすべて空の表に畳む。
fn read_descriptions(dir: PathBuf) -> HashMap<String, String> {
    read_help_file(&dir.join(HELP_FILE)).unwrap_or_default()
}

/// 説明ファイル 1 つを読んで表を組み立てる。
fn read_help_file(path: &Path) -> Option<HashMap<String, String>> {
    let bytes = read_bounded(path, MAX_HELP_BYTES).ok()??;
    let table = parse_descriptions(&String::from_utf8(bytes).ok()?);
    (!table.is_empty()).then_some(table)
}

/// 節の見出しと `キー=値` の行から、効果の説明を集める。
///
/// 見る節は `[Tips.<効果名>]` だけである。他の見出しが現れたらそれ以降の行は
/// どの効果にも属さない——節を跨いで値を拾うと、効果と無関係な文言が説明に
/// 化ける。
///
/// 節の中で見るキーは 1 つだけである。他のキーは設定項目の説明だが、一覧は
/// 項目を返さない。項目の一覧は実行時に得られるため、ファイルの記述と食い違う
/// ことがない。
///
/// 同じ効果名の節が 2 度現れた場合は先に現れたものを採る。
///
/// 見出しが無い行・`=` を持たない行・空のファイルはいずれも黙って読み飛ばす。
/// 説明はあれば使うものであり、書式の乱れを失敗として表に出す先が無い。
fn parse_descriptions(text: &str) -> HashMap<String, String> {
    let mut descriptions = HashMap::new();
    let mut section: Option<&str> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line
                .strip_prefix(SECTION_PREFIX)
                .and_then(|rest| rest.strip_suffix(']'))
                .filter(|name| !name.is_empty());
            continue;
        }
        let Some(name) = section else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key != EFFECT_DESCRIPTION_KEY {
            continue;
        }
        descriptions
            .entry(name.to_string())
            .or_insert_with(|| unescape_newlines(value));
    }
    descriptions
}

/// 値の中の `\n`（`\` と `n` の 2 文字）を実際の改行へ戻す。
///
/// 実改行で返す。エスケープ表記のまま渡すと、受け取った側は説明の中の `\n` を
/// 文字どおりの 2 文字として読む。
///
/// **`\` の他の並びは `\` ごとそのまま残す。** この書式が包むと分かっているのは
/// 改行だけであり、他の並びの解き方は定まっていない。
///
/// **設定値の codec は用いない。** [`aviutl2_mcp_core::decode_host_text`] は
/// 書き込みと対にして往復させる設定値の書式であり、`\\` を `\` へ解く規則を
/// 含む。ここには書き込みが無く往復もしないため、その規則を持ち込むと説明文の
/// `\` が根拠なく落ちる。
fn unescape_newlines(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            decoded.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => decoded.push('\n'),
            Some(other) => {
                decoded.push('\\');
                decoded.push(other);
            }
            None => decoded.push('\\'),
        }
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 供給元が並べる形。先頭はコメント行で、効果の節以外の節も混じる。
    const HELP: &str = ";AviUtl2 Default\r\n\
[Language]\r\n\
図形=Figure\r\n\
[Tips.図形]\r\n\
effect.name=単色の図形を作成します\\nsvgファイルから読み込むことも出来ます\r\n\
図形の種類=図形の種類を選択します\\nボタンクリックでsvgファイルを選択出来ます\r\n\
ライン幅=図形を描画するラインの幅を指定します\r\n\
[Tips.テキスト]\r\n\
effect.name=文字を表示します\r\n";

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aviutl2-mcp-effect-help-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("作れる");
        dir
    }

    fn write_help(dir: &Path, text: &str) {
        fs::write(dir.join(HELP_FILE), text.as_bytes()).expect("書ける");
    }

    #[test]
    fn the_description_comes_from_the_effect_key_of_the_tips_section() {
        let dir = temp_dir("sections");
        write_help(&dir, HELP);
        let table = read_descriptions(dir);

        assert_eq!(
            table.get("図形").map(String::as_str),
            Some("単色の図形を作成します\nsvgファイルから読み込むことも出来ます"),
            "効果の説明が全文で返りません"
        );
        assert_eq!(
            table.get("テキスト").map(String::as_str),
            Some("文字を表示します")
        );
    }

    #[test]
    fn item_keys_and_other_sections_are_not_effect_descriptions() {
        // 節の中の他のキーは設定項目の説明であり、効果の説明ではない。
        // `[Language]` は表示言語の対応表であって効果の節ではない。
        let dir = temp_dir("other-keys");
        write_help(&dir, HELP);
        let table = read_descriptions(dir);

        assert_eq!(table.len(), 2, "効果以外の節や項目のキーが混じっています");
        assert!(!table.contains_key("図形の種類"));
    }

    #[test]
    fn the_second_line_of_a_description_is_kept() {
        // 発見の鍵が 2 行目に置かれている説明が実在する。先頭行だけに切ると、
        // 説明を載せる意味そのものが失われる。
        let dir = temp_dir("second-line");
        write_help(&dir, HELP);
        let description = read_descriptions(dir).remove("図形").expect("説明がある");

        assert_eq!(description.lines().count(), 2);
        assert!(description.lines().nth(1).unwrap().contains("svg"));
    }

    #[test]
    fn a_broken_file_yields_no_description_instead_of_failing() {
        for (name, text) in [
            ("empty", ""),
            ("no-section", "effect.name=見出しの無い説明\r\n"),
            ("no-equals", "[Tips.図形]\r\neffect.name\r\n図形の種類\r\n"),
            ("unclosed-section", "[Tips.図形\r\neffect.name=説明\r\n"),
            ("empty-section-name", "[Tips.]\r\neffect.name=説明\r\n"),
            (
                "value-before-section",
                "effect.name=前\r\n[Language]\r\neffect.name=別の節\r\n",
            ),
        ] {
            let dir = temp_dir(name);
            write_help(&dir, text);
            assert!(
                read_descriptions(dir).is_empty(),
                "{name} が説明を返しました"
            );
        }
    }

    #[test]
    fn a_missing_file_yields_no_description() {
        let dir = temp_dir("missing");
        assert!(read_descriptions(dir).is_empty());
    }

    #[test]
    fn a_file_over_the_limit_is_not_read() {
        let dir = temp_dir("oversized");
        let mut text = String::from("[Tips.図形]\r\neffect.name=説明\r\n");
        text.push_str(&";x".repeat(MAX_HELP_BYTES as usize));
        write_help(&dir, &text);
        assert!(read_descriptions(dir).is_empty());
    }

    #[test]
    fn a_non_utf8_file_yields_no_description() {
        let dir = temp_dir("non-utf8");
        fs::write(
            dir.join(HELP_FILE),
            b"[Tips.\xff\xfe]\r\neffect.name=\xff\r\n",
        )
        .expect("書ける");
        assert!(read_descriptions(dir).is_empty());
    }

    #[test]
    fn only_the_newline_escape_is_unwrapped() {
        assert_eq!(unescape_newlines(r"1 行目\n2 行目"), "1 行目\n2 行目");
        // 改行以外の並びは `\` ごと残る。設定値の codec と違い `\\` を解かない。
        assert_eq!(unescape_newlines(r"C:\\temp\path"), r"C:\\temp\path");
        assert_eq!(unescape_newlines(r"末尾の\"), r"末尾の\");
    }

    #[test]
    fn the_first_section_wins_when_an_effect_appears_twice() {
        let table = parse_descriptions(
            "[Tips.図形]\r\neffect.name=先\r\n[Tips.図形]\r\neffect.name=後\r\n",
        );
        assert_eq!(table.get("図形").map(String::as_str), Some("先"));
    }

    #[test]
    fn the_host_directory_is_the_directory_of_the_running_executable() {
        // 解決そのものはこの環境でも成立する。ここに説明ファイルが無いことが、
        // 説明の得られない環境の再現になっている。
        let dir = host_directory().expect("実行ファイルの位置を解決できません");
        assert!(
            dir.is_dir(),
            "{} がディレクトリではありません",
            dir.display()
        );
        assert_eq!(
            module_file_name().as_deref().and_then(Path::parent),
            Some(dir.as_path())
        );
    }
}
