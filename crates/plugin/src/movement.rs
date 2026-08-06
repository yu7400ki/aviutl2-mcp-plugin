//! ホストが受け付ける移動方法の名前。
//!
//! AviUtl2 のデータディレクトリ直下の `aviutl2.ini` が `[Movement.<名前>]` の
//! 節として並べている。SDK には一覧を引く手段が無く、ファイルだけが供給源で
//! ある。
//!
//! **一覧は書き込みのゲートである。** 一覧に無い名前をトラックバーへ書くと、
//! ホストが投げた C++ の例外が `extern "C"` の境界を越えて入り、巻き戻せずに
//! プロセスごと落ちる。設定項目の選択肢と違い、「一覧はヒントであってゲートでは
//! ない」を適用できない。
//!
//! 解決はデータディレクトリと同じく plugin の生存期間中に 1 度だけ行う
//! （[`crate::alias::data_directory`]）。ファイルは編集中に書き換わり得るが、
//! 移動方法の集合は AviUtl2 の版で決まるため、1 度確定させて使う。

use crate::alias::{data_directory, read_bounded};
use std::path::Path;
use std::sync::OnceLock;

/// 設定ファイルの名前。
const SETTINGS_FILE: &str = "aviutl2.ini";

/// 移動方法 1 つの節の見出しが始まる並び。
const SECTION_PREFIX: &str = "[Movement.";

/// 設定ファイルとして読み込む最大バイト数。
///
/// この経路の費用は要求の内容で決まらないため、予算では守れない。上限は
/// 要求元が動かせない。
pub const MAX_SETTINGS_INI_BYTES: u64 = 4 * 1024 * 1024;

/// 解決した移動方法の名前。
static MOVEMENTS: OnceLock<Vec<String>> = OnceLock::new();

/// ホストが受け付ける移動方法の名前を返す。
///
/// **読めなければ空の一覧を返す。** データディレクトリを解決できない、設定
/// ファイルが無い、大きすぎる、UTF-8 として読めない、`[Movement.*]` の節が
/// 無い——いずれも空になる。空の一覧では移動を持つトラックバーの値が 1 つも
/// 書けず、要求は `track_mode_unknown` で拒否される。
///
/// **推測で通す選択肢は無い。** 名前を検証せずに書くと、その場でホストの
/// プロセスが落ちる。書けないことは、落とすことより軽い。
///
/// 静的な値（`mode` が `null` の移動）と、移動を含まない数値の書き込みは
/// 一覧を要さないため、読めなくても従来どおり書ける。
pub fn movements() -> &'static [String] {
    MOVEMENTS.get_or_init(|| {
        let names = data_directory().map(read_movements).unwrap_or_default();
        if names.is_empty() {
            tracing::info!("移動方法の一覧を読めませんでした。トラックバーの移動は書き込めません");
        } else {
            tracing::info!("移動方法の一覧: {} 件", names.len());
        }
        names
    })
}

/// 設定ファイルから移動方法の名前を読む。失敗はすべて空の一覧に畳む。
fn read_movements(data_dir: &Path) -> Vec<String> {
    read_movement_names(&data_dir.join(SETTINGS_FILE)).unwrap_or_default()
}

/// 設定ファイル 1 つを読んで `[Movement.*]` の節の名前を集める。
fn read_movement_names(path: &Path) -> Option<Vec<String>> {
    let bytes = read_bounded(path, MAX_SETTINGS_INI_BYTES).ok()??;
    let names = movement_names(&String::from_utf8(bytes).ok()?);
    (!names.is_empty()).then_some(names)
}

/// 節の見出しから移動方法の名前を出現順に集める。
///
/// **表として解釈せず、見出しの行だけを読む。** エイリアスの UI 状態ファイルは
/// 節の中の `label=` を引くため表が要るが、ここで要るのは名前だけである。表を
/// 通すと、キーを 1 つも持たない節が落ちる——落ちた移動方法はそのまま
/// 「書けない移動方法」になる。見出しを直に読めばその取りこぼしが無い。
///
/// 見出しは 1 行に収まっているものだけを読む。行を跨ぐ書き方は AviUtl2 が
/// 保存する形ではなく、現れた場合は読み落として「書けない」側へ倒れる。
fn movement_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some(name) = line
            .trim()
            .strip_prefix(SECTION_PREFIX)
            .and_then(|rest| rest.strip_suffix(']'))
        else {
            continue;
        };
        if name.is_empty() || names.iter().any(|known| known == name) {
            continue;
        }
        names.push(name.to_string());
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 実機の `aviutl2.ini` が並べる形。値の側は移動方法ごとに異なる。
    const SETTINGS: &str = "[Window]\r\nmain=0,0,1280,720\r\n\
[Movement.回転]\r\nparam=0\r\n\
[Movement.直線移動]\r\n\
[Movement.直線移動(時間制御)]\r\n\
[Movement.移動無し]\r\n\
[Movement.再生範囲]\r\n";

    fn write_settings(dir: &Path, text: &str) {
        fs::write(dir.join(SETTINGS_FILE), text.as_bytes()).expect("書ける");
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aviutl2-mcp-movement-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("作れる");
        dir
    }

    #[test]
    fn the_names_come_from_the_movement_sections() {
        let dir = temp_dir("sections");
        write_settings(&dir, SETTINGS);
        assert_eq!(
            read_movements(&dir),
            vec![
                "回転".to_string(),
                "直線移動".to_string(),
                "直線移動(時間制御)".to_string(),
                "移動無し".to_string(),
                "再生範囲".to_string(),
            ]
        );
    }

    #[test]
    fn a_section_without_keys_still_counts() {
        // 表として解釈するとキーを持たない節が落ちる。落ちた移動方法は
        // そのまま書けない移動方法になる。
        assert_eq!(
            movement_names("[Movement.移動無し]\r\n[Movement.回転]\r\nparam=0\r\n"),
            vec!["移動無し".to_string(), "回転".to_string()]
        );
    }

    #[test]
    fn lines_that_are_not_movement_headers_are_ignored() {
        assert_eq!(
            movement_names("[Window]\r\n[Movement]\r\nname=[Movement.罠]\r\n[Movement.本物]\r\n"),
            vec!["本物".to_string()]
        );
    }

    #[test]
    fn an_unreadable_file_yields_no_names() {
        // 読めない形をすべて空へ畳む。空の一覧では移動が 1 つも書けない。
        let dir = temp_dir("unreadable");
        // ファイルが無い。
        assert_eq!(read_movements(&dir), Vec::<String>::new());
        // 節が無い。
        write_settings(&dir, "[Window]\r\nmain=0\r\n");
        assert_eq!(read_movements(&dir), Vec::<String>::new());
        // UTF-8 として読めない。
        fs::write(dir.join(SETTINGS_FILE), [0xff, 0xfe, 0x00]).expect("書ける");
        assert_eq!(read_movements(&dir), Vec::<String>::new());
    }

    #[test]
    fn a_file_over_the_limit_is_not_read() {
        let dir = temp_dir("oversized");
        let mut text = SETTINGS.to_string();
        while (text.len() as u64) <= MAX_SETTINGS_INI_BYTES {
            text.push_str("[Movement.埋め草]\r\n");
        }
        write_settings(&dir, &text);
        assert_eq!(read_movements(&dir), Vec::<String>::new());
    }
}
