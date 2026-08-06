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
//!
//! **表のパーサは流用しない。** `aviutl2::alias::Table` は、節の見出しにも
//! `キー=値` にも当たらない行を不正な行として扱い、パース全体を失敗させる。
//! このファイルの先頭行は `;` で始まるコメントであり、1 行目で表が得られない。
//! 加えて節名を `.` で割って入れ子にするため、`[Tips.<効果名>]` は `Tips` の
//! 下の階層になり、`.` を含む効果名はさらに割れる。ここで要るのは節の見出しと
//! キー 1 つだけであり、行を直に読めば取りこぼしが無い（[`crate::movement`] が
//! 同じ理由で見出しを直に読んでいる）。

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
///
/// **説明が 0 件になる理由は 3 つあり、いずれもログでは別の行になる。** 実行
/// ファイルの位置を解決できない・そこにファイルが無いか読めない・読めたが効果の
/// 節が 1 つも無い、の 3 つは対処が違う。同じ文言に畳むと、どれが起きたのかを
/// ログから切り分けられない。
fn descriptions() -> &'static HashMap<String, String> {
    DESCRIPTIONS.get_or_init(|| {
        let Some(dir) = host_directory() else {
            tracing::info!(
                "ホストの実行ファイルの位置を解決できませんでした。効果の説明は返しません"
            );
            return HashMap::new();
        };
        let path = dir.join(HELP_FILE);
        let Some(table) = read_descriptions(&dir) else {
            tracing::info!(
                "{} を読めませんでした。効果の説明は返しません",
                path.display()
            );
            return HashMap::new();
        };
        if table.is_empty() {
            tracing::info!(
                "{} に効果の節がありませんでした。効果の説明は返しません",
                path.display()
            );
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

/// ディレクトリ直下の説明ファイルを読んで表を組み立てる。
///
/// **ファイルを読めなかったことと、読めたが効果の節が無かったことを区別する。**
/// 前者は `None`、後者は空の表である。どちらも説明は出ないが、原因が違う。
/// 無い・大きすぎる・UTF-8 として読めないはいずれも `None` になる。
fn read_descriptions(dir: &Path) -> Option<HashMap<String, String>> {
    let bytes = read_bounded(&dir.join(HELP_FILE), MAX_HELP_BYTES).ok()??;
    Some(parse_descriptions(&String::from_utf8(bytes).ok()?))
}

/// 節の見出しと `キー=値` の行から、効果の説明を集める。
///
/// 見る節は `[Tips.<効果名>]` だけである。他の見出しが現れたらそれ以降の行は
/// どの効果にも属さない——節を跨いで値を拾うと、効果と無関係な文言が説明に
/// 化ける。
///
/// 節の中で見るキーは [`EFFECT_DESCRIPTION_KEY`] だけである。**節の中の並びに
/// 頼らない。** 他のキーは設定項目の説明であり、効果の説明として返せば、
/// エラーの出ないまま誤った文言が確信を持って使われる。項目の説明を運ばないのは
/// 一覧が項目を返さないためであり、項目の一覧は実行時に得られるので、ファイルの
/// 記述と食い違うことがない。
///
/// `;` で始まるコメント行は、`=` を含む形で現れてもキーが一致しないため採られ
/// ない。節の外のコメント行は、そもそもどの効果にも属さない。
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
/// **`\\n`（`\` 2 つ + `n`）は改行にならない。** 先頭の 2 文字が「その他の
/// 並び」として素通しされ、続く `n` はただの文字として出るため、`\\n` は
/// `\\n` のまま残る。設定値の codec なら `\\` を `\` へ解いて `\n` になる。
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
    use std::collections::BTreeSet;
    use std::fs;

    /// 供給元が並べる形。
    ///
    /// **キーの並びに頼れない形にしてある。** `[Tips.画像合成(オブジェクト)]` は
    /// 項目のキーが `effect.name` より前に並び、節の中にコメント行を挟む。
    /// キー名を見ずに先頭の `キー=値` を採る実装では別の文言が採られる。
    ///
    /// **`[Language]` も `effect.name` を持つ。** 節の見出しの判定を緩めると
    /// 表示言語の対応表が効果として現れる。
    ///
    /// 効果名には括弧を含むものがある。節の見出しの切り出しはこれを壊さない。
    const HELP: &str = ";AviUtl2 Default\r\n\
[Language]\r\n\
図形=Figure\r\n\
effect.name=表示言語の対応表であって効果の説明ではありません\r\n\
[Tips.図形]\r\n\
effect.name=単色の図形を作成します\\nsvgファイルから読み込むことも出来ます\r\n\
図形の種類=図形の種類を選択します\\nボタンクリックでsvgファイルを選択出来ます\r\n\
ライン幅=図形を描画するラインの幅を指定します\r\n\
[Tips.画像合成(オブジェクト)]\r\n\
;合成の設定\r\n\
合成モード=合成のしかたを選択します\r\n\
X=合成する位置を指定します\r\n\
effect.name=別のオブジェクトを合成します\r\n\
[Tips.テキスト]\r\n\
effect.name=文字を表示します\r\n";

    /// テスト用の作業ディレクトリ。抜けるときに消す。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "aviutl2-mcp-effect-help-{name}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&dir).expect("作れる");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write_help(&self, text: &str) {
            self.write_help_bytes(text.as_bytes());
        }

        fn write_help_bytes(&self, bytes: &[u8]) {
            fs::write(self.0.join(HELP_FILE), bytes).expect("書ける");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// 供給元の形を読んだ表。
    fn help_table() -> HashMap<String, String> {
        let dir = TempDir::new("help");
        dir.write_help(HELP);
        read_descriptions(dir.path()).expect("読める")
    }

    #[test]
    fn the_description_comes_from_the_effect_key_of_the_tips_section() {
        let table = help_table();

        assert_eq!(
            table.get("図形").map(String::as_str),
            Some("単色の図形を作成します\nsvgファイルから読み込むことも出来ます"),
            "効果の説明が全文で返りません"
        );
        // 項目のキーが先に並ぶ節でも、採るのは効果のキーである。並び順で決めると
        // 設定項目の説明が効果の説明として出る。
        assert_eq!(
            table.get("画像合成(オブジェクト)").map(String::as_str),
            Some("別のオブジェクトを合成します"),
            "節の中の並びで説明を選んでいます"
        );
        assert_eq!(
            table.get("テキスト").map(String::as_str),
            Some("文字を表示します")
        );
    }

    #[test]
    fn item_keys_and_other_sections_are_not_effect_descriptions() {
        // 節の中の他のキーは設定項目の説明であり、効果の説明ではない。
        // `[Language]` は表示言語の対応表であって効果の節ではなく、`effect.name`
        // を持っていても効果として現れてはならない。
        let table = help_table();
        let names: BTreeSet<&str> = table.keys().map(String::as_str).collect();

        assert_eq!(
            names,
            BTreeSet::from(["図形", "画像合成(オブジェクト)", "テキスト"]),
            "効果以外の節や項目のキーが混じっています"
        );
    }

    #[test]
    fn a_comment_line_inside_a_section_is_skipped() {
        // 節の中のコメント行は説明でも項目でもない。`=` を含む形で現れても、
        // キーが一致しないため説明には採られない。
        let table = parse_descriptions(
            "[Tips.図形]\r\n;effect.name=コメント\r\n;単なる注記\r\neffect.name=説明\r\n",
        );

        assert_eq!(table.get("図形").map(String::as_str), Some("説明"));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn the_second_line_of_a_description_is_kept() {
        // 発見の鍵が 2 行目に置かれている説明が実在する。先頭行だけに切ると、
        // 説明を載せる意味そのものが失われる。
        let description = help_table().remove("図形").expect("説明がある");

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
            let dir = TempDir::new(name);
            dir.write_help(text);
            assert!(
                read_descriptions(dir.path()).expect("読める").is_empty(),
                "{name} が説明を返しました"
            );
        }
    }

    #[test]
    fn a_readable_file_without_effect_sections_is_told_apart_from_an_unreadable_one() {
        // どちらも説明は 0 件だが原因が違う。畳むと、ログから切り分けられない。
        let readable = TempDir::new("no-tips");
        readable.write_help(";コメントだけ\r\n[Language]\r\n図形=Figure\r\n");
        assert_eq!(
            read_descriptions(readable.path()),
            Some(HashMap::new()),
            "読めたファイルが読めなかったことになっています"
        );

        let missing = TempDir::new("missing");
        assert_eq!(read_descriptions(missing.path()), None);
    }

    #[test]
    fn a_file_over_the_limit_is_not_read() {
        let dir = TempDir::new("oversized");
        let head = "[Tips.図形]\r\neffect.name=説明\r\n";
        // 上限をちょうど 1 バイト超える。境界そのものを跨がせる。
        let mut text = String::from(head);
        text.push_str(&";".repeat(MAX_HELP_BYTES as usize + 1 - head.len()));
        assert_eq!(text.len() as u64, MAX_HELP_BYTES + 1);
        dir.write_help(&text);

        assert_eq!(read_descriptions(dir.path()), None);
    }

    #[test]
    fn a_non_utf8_file_yields_no_description() {
        let dir = TempDir::new("non-utf8");
        dir.write_help_bytes(b"[Tips.\xff\xfe]\r\neffect.name=\xff\r\n");

        assert_eq!(read_descriptions(dir.path()), None);
    }

    #[test]
    fn only_the_newline_escape_is_unwrapped() {
        assert_eq!(unescape_newlines(r"1 行目\n2 行目"), "1 行目\n2 行目");
        // 改行以外の並びは `\` ごと残る。設定値の codec と違い `\\` を解かない。
        assert_eq!(unescape_newlines(r"C:\\temp\path"), r"C:\\temp\path");
        // 最も紛らわしい並び。`\\` を解かないため改行にはならない。
        assert_eq!(unescape_newlines(r"1 行目\\n2 行目"), r"1 行目\\n2 行目");
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
