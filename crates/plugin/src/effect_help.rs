//! ホストが同梱する効果と設定項目の説明。
//!
//! AviUtl2 の実行ファイルと同じディレクトリの `Default.aul2` が
//! `[Tips.<効果名>]` の節として並べている。SDK には説明を引く手段が無く、
//! このファイルだけが供給源である。
//!
//! 節の中では `effect.name` が効果そのものの説明、それ以外のキーが設定項目の
//! 説明である。**両者を取り違えない。** 項目の説明が効果の説明として出れば、
//! 受け取った側はエラーの出ないまま誤った文言を確信を持って使う。
//!
//! **説明を我々が書き足すことはしない。** ここが返すのはホストが同梱した文言
//! そのものだけであり、供給源に無い説明は `None` になる。推測で書いた説明は
//! 他人のソフトウェアについての検証できない主張であり、微妙に外したものを
//! 受け取った側は確信を持って誤用する。説明が無ければ慎重に扱われる。
//!
//! **説明は切り詰めない。** 発見の鍵が 2 行目以降に置かれている説明が実在する
//! ため、先頭行だけに切ると、説明を載せることで埋めようとしている欠落がその
//! まま残る。説明を持つのは供給元が挙げた効果に限られ、全文を載せても応答の
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
//! その中の `キー=値` だけであり、行を直に読めば取りこぼしが無い
//! （[`crate::movement`] が同じ理由で見出しを直に読んでいる）。

use crate::alias::read_bounded;
use crate::read::host::HostEffectHelp;
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

/// コメント行が始まる文字。
const COMMENT_PREFIX: char = ';';

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

/// 解決した効果ごとの説明。
static HELP: OnceLock<HashMap<String, HostEffectHelp>> = OnceLock::new();

/// 効果名に対応する説明を返す。供給源に節が無ければ `None`。
///
/// 初回の要求で 1 度だけ読み込む。説明が得られないことは plugin が起動できない
/// 理由ではないため、初期化時には読まない。
pub fn help_of(effect_name: &str) -> Option<&'static HostEffectHelp> {
    help_table().get(effect_name)
}

/// 効果名から説明を引く表を返す。読めなければ空の表になる。
///
/// **説明が 0 件になる理由は 3 つあり、いずれもログでは別の行になる。** 実行
/// ファイルの位置を解決できない・そこにファイルが無いか読めない・読めたが効果の
/// 節が 1 つも無い、の 3 つは対処が違う。同じ文言に畳むと、どれが起きたのかを
/// ログから切り分けられない。
fn help_table() -> &'static HashMap<String, HostEffectHelp> {
    HELP.get_or_init(|| {
        let Some(dir) = host_directory() else {
            tracing::info!(
                "ホストの実行ファイルの位置を解決できませんでした。効果の説明は返しません"
            );
            return HashMap::new();
        };
        let path = dir.join(HELP_FILE);
        let Some(table) = read_help(&dir) else {
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
fn read_help(dir: &Path) -> Option<HashMap<String, HostEffectHelp>> {
    let bytes = read_bounded(&dir.join(HELP_FILE), MAX_HELP_BYTES).ok()??;
    Some(parse_help(&String::from_utf8(bytes).ok()?))
}

/// 節の見出しと `キー=値` の行から、効果ごとの説明を集める。
///
/// 見る節は `[Tips.<効果名>]` だけである。他の見出しが現れたらそれ以降の行は
/// どの効果にも属さない——節を跨いで値を拾うと、効果と無関係な文言が説明に
/// 化ける。
///
/// 節の中では [`EFFECT_DESCRIPTION_KEY`] だけが効果そのものの説明であり、他の
/// キーは設定項目の説明である。**節の中の並びに頼らない。** 先頭の `キー=値` を
/// 効果の説明として採る実装では、項目の説明が効果の説明として出て、エラーの
/// 出ないまま誤った文言が確信を持って使われる。
///
/// 集めた項目の説明は項目名を確定させない。項目の一覧はホストの列挙から得る
/// ため、ここに無い項目も、ここに在って列挙に無い項目も在り得る。
///
/// `;` で始まる行はコメントとして読み飛ばす。**キー名で選り分けるだけでは
/// 足りない。** 項目の説明はキー名を問わずに採るため、`;キー=値` の形の
/// コメントがそのまま項目の説明として現れてしまう。
///
/// 同じ効果名の節が 2 度現れた場合は先に現れた節を丸ごと採る。節の中で同じ
/// キーが 2 度現れた場合も先に現れたものを採る。
///
/// 見出しが無い行・`=` を持たない行・空のファイルはいずれも黙って読み飛ばす。
/// 説明はあれば使うものであり、書式の乱れを失敗として表に出す先が無い。
fn parse_help(text: &str) -> HashMap<String, HostEffectHelp> {
    let mut table: HashMap<String, HostEffectHelp> = HashMap::new();
    let mut section: Option<(&str, HostEffectHelp)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            flush_section(&mut table, section.take());
            section = line
                .strip_prefix(SECTION_PREFIX)
                .and_then(|rest| rest.strip_suffix(']'))
                .filter(|name| !name.is_empty())
                .map(|name| (name, HostEffectHelp::default()));
            continue;
        }
        if line.starts_with(COMMENT_PREFIX) {
            continue;
        }
        let Some((_, help)) = section.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key == EFFECT_DESCRIPTION_KEY {
            help.description
                .get_or_insert_with(|| unescape_newlines(value));
        } else {
            help.items
                .entry(key.to_string())
                .or_insert_with(|| unescape_newlines(value));
        }
    }
    flush_section(&mut table, section);
    table
}

/// 読み終えた節を表へ移す。同じ効果名が既に在れば先の節を残す。
///
/// **説明を 1 つも持たない節は表へ入れない。** 見出しだけが在っても引ける文言が
/// 無く、記録すれば「説明を持つ効果の数」がログでも表の大きさでも狂う。
fn flush_section(
    table: &mut HashMap<String, HostEffectHelp>,
    section: Option<(&str, HostEffectHelp)>,
) {
    let Some((name, help)) = section else {
        return;
    };
    if help.description.is_none() && help.items.is_empty() {
        return;
    }
    table.entry(name.to_string()).or_insert(help);
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
    fn parsed_help() -> HashMap<String, HostEffectHelp> {
        let dir = TempDir::new("help");
        dir.write_help(HELP);
        read_help(dir.path()).expect("読める")
    }

    /// 効果の説明だけを取り出す。
    fn description_of(table: &HashMap<String, HostEffectHelp>, effect: &str) -> Option<String> {
        table.get(effect)?.description.clone()
    }

    /// 設定項目の説明だけを取り出す。
    fn item_description_of(
        table: &HashMap<String, HostEffectHelp>,
        effect: &str,
        item: &str,
    ) -> Option<String> {
        table.get(effect)?.items.get(item).cloned()
    }

    #[test]
    fn the_description_comes_from_the_effect_key_of_the_tips_section() {
        let table = parsed_help();

        assert_eq!(
            description_of(&table, "図形").as_deref(),
            Some("単色の図形を作成します\nsvgファイルから読み込むことも出来ます"),
            "効果の説明が全文で返りません"
        );
        // 項目のキーが先に並ぶ節でも、採るのは効果のキーである。並び順で決めると
        // 設定項目の説明が効果の説明として出る。
        assert_eq!(
            description_of(&table, "画像合成(オブジェクト)").as_deref(),
            Some("別のオブジェクトを合成します"),
            "節の中の並びで説明を選んでいます"
        );
        assert_eq!(
            description_of(&table, "テキスト").as_deref(),
            Some("文字を表示します")
        );
    }

    #[test]
    fn item_keys_and_other_sections_are_not_effect_descriptions() {
        // 節の中の他のキーは設定項目の説明であり、効果の説明ではない。
        // `[Language]` は表示言語の対応表であって効果の節ではなく、`effect.name`
        // を持っていても効果として現れてはならない。
        let table = parsed_help();
        let names: BTreeSet<&str> = table.keys().map(String::as_str).collect();

        assert_eq!(
            names,
            BTreeSet::from(["図形", "画像合成(オブジェクト)", "テキスト"]),
            "効果以外の節や項目のキーが混じっています"
        );
    }

    #[test]
    fn the_other_keys_of_a_section_are_kept_as_item_descriptions() {
        // 項目の説明は捨てない。名前の似た効果の使い分けは、散文ではなく設定
        // 項目の顔ぶれとその説明で解ける。
        let table = parsed_help();

        assert_eq!(
            item_description_of(&table, "図形", "ライン幅").as_deref(),
            Some("図形を描画するラインの幅を指定します")
        );
        // 項目のキーが `effect.name` より前に並ぶ節でも取りこぼさない。
        assert_eq!(
            item_description_of(&table, "画像合成(オブジェクト)", "合成モード").as_deref(),
            Some("合成のしかたを選択します")
        );
        assert_eq!(
            item_description_of(&table, "画像合成(オブジェクト)", "X").as_deref(),
            Some("合成する位置を指定します")
        );
    }

    #[test]
    fn the_effect_key_never_appears_as_an_item_description() {
        // `effect.name` は効果そのものの説明である。項目の表へ入れると、項目の
        // 説明として効果の説明が出る。
        let table = parsed_help();

        for effect in ["図形", "画像合成(オブジェクト)", "テキスト"] {
            let help = table.get(effect).expect("節がある");
            assert!(
                !help.items.contains_key(EFFECT_DESCRIPTION_KEY),
                "{effect} の項目に {EFFECT_DESCRIPTION_KEY} が混じっています"
            );
            let description = help.description.as_deref().expect("効果の説明がある");
            assert!(
                !help.items.values().any(|item| item == description),
                "{effect} の項目の説明が効果の説明と同じです"
            );
        }
        // 説明を 1 つだけ持つ節は、その 1 つを効果の説明として持つ。
        assert!(
            table.get("テキスト").expect("節がある").items.is_empty(),
            "項目の説明を持たない節が項目を名乗っています"
        );
    }

    #[test]
    fn a_comment_line_inside_a_section_is_skipped() {
        // 節の中のコメント行は説明でも項目でもない。項目の説明はキー名を問わずに
        // 採るため、読み飛ばさなければ `;キー=値` が項目の説明として現れる。
        let table = parse_help(
            "[Tips.図形]\r\n;effect.name=コメント\r\n;単なる注記\r\neffect.name=説明\r\n",
        );

        assert_eq!(description_of(&table, "図形").as_deref(), Some("説明"));
        assert!(table.get("図形").expect("節がある").items.is_empty());
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn the_second_line_of_a_description_is_kept() {
        // 発見の鍵が 2 行目に置かれている説明が実在する。先頭行だけに切ると、
        // 説明を載せる意味そのものが失われる。効果の説明も項目の説明も同じで
        // ある——`図形` が svg を読めることは、項目の説明の 2 行目にしか無い。
        let table = parsed_help();

        let description = description_of(&table, "図形").expect("効果の説明がある");
        assert_eq!(description.lines().count(), 2);
        assert!(description.lines().nth(1).unwrap().contains("svg"));

        let item = item_description_of(&table, "図形", "図形の種類").expect("項目の説明がある");
        assert_eq!(item.lines().count(), 2);
        assert!(item.lines().nth(1).unwrap().contains("svg"));
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
                read_help(dir.path()).expect("読める").is_empty(),
                "{name} が説明を返しました"
            );
        }
    }

    #[test]
    fn a_section_with_only_item_descriptions_is_still_recorded() {
        // 効果の説明を持たない節でも、項目の説明は引ける。効果の説明の有無で
        // 節ごと落とすと、項目の説明だけが書かれた効果が空として返る。
        let table = parse_help("[Tips.グロー]\r\n拡散=光の広がりを指定します\r\n");

        assert_eq!(description_of(&table, "グロー"), None);
        assert_eq!(
            item_description_of(&table, "グロー", "拡散").as_deref(),
            Some("光の広がりを指定します")
        );
    }

    #[test]
    fn a_readable_file_without_effect_sections_is_told_apart_from_an_unreadable_one() {
        // どちらも説明は 0 件だが原因が違う。畳むと、ログから切り分けられない。
        let readable = TempDir::new("no-tips");
        readable.write_help(";コメントだけ\r\n[Language]\r\n図形=Figure\r\n");
        assert_eq!(
            read_help(readable.path()),
            Some(HashMap::new()),
            "読めたファイルが読めなかったことになっています"
        );

        let missing = TempDir::new("missing");
        assert_eq!(read_help(missing.path()), None);
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

        assert_eq!(read_help(dir.path()), None);
    }

    #[test]
    fn a_non_utf8_file_yields_no_description() {
        let dir = TempDir::new("non-utf8");
        dir.write_help_bytes(b"[Tips.\xff\xfe]\r\neffect.name=\xff\r\n");

        assert_eq!(read_help(dir.path()), None);
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
        // 節は丸ごと先勝ちである。キーごとに先勝ちにすると、後の節の項目だけが
        // 先の節へ紛れ込む。
        let table = parse_help(
            "[Tips.図形]\r\neffect.name=先\r\n[Tips.図形]\r\neffect.name=後\r\nライン幅=後の項目\r\n",
        );
        assert_eq!(description_of(&table, "図形").as_deref(), Some("先"));
        assert_eq!(item_description_of(&table, "図形", "ライン幅"), None);
    }

    #[test]
    fn the_first_key_wins_when_it_appears_twice_in_one_section() {
        let table = parse_help(
            "[Tips.図形]\r\neffect.name=先\r\neffect.name=後\r\nライン幅=先\r\nライン幅=後\r\n",
        );
        assert_eq!(description_of(&table, "図形").as_deref(), Some("先"));
        assert_eq!(
            item_description_of(&table, "図形", "ライン幅").as_deref(),
            Some("先")
        );
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
