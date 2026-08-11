//! ホストが受け付ける移動方法と、その名前で書けるかどうか。
//!
//! 名前は AviUtl2 のデータディレクトリ直下の `aviutl2.ini` が
//! `[Movement.<名前>]` の節として並べている。SDK には一覧を引く手段が無く、
//! ファイルだけが供給源である。
//!
//! **一覧は書き込みのゲートである。** 一覧に無い名前をトラックバーへ書くと、
//! ホストが投げた C++ の例外が `extern "C"` の境界を越えて入り、巻き戻せずに
//! プロセスごと落ちる。設定項目の選択肢と違い、「一覧はヒントであってゲートでは
//! ない」を適用できない。
//!
//! **一覧に載るのに書けない名前がある。** 登録されていて名前としては正しく、
//! 書き込みも受理されるが、読み直しがその移動を失う。移動を消すには移動方法を
//! 指定しない指定（`mode` が `null`）を使う。
//!
//! # 一覧と拒否は同じ表を読む
//!
//! 名前と可否は 1 つの値（[`Movement`]）として組み立て、要求元へ並べる一覧と
//! 書き込みの検証（[`aviutl2_mcp_core::validate_track_value`]）が同じ値を見る。
//! **こうしなければ「一覧に載る ⇒ 書ける」が 1 件ずつについて成り立たない**
//! ——一覧を出す側と拒む側が別々に可否を決めれば、片方にだけ条件を足せてしまい、
//! 要求元は使えない名前を選ぶか、使える名前を拒まれる。
//!
//! # 書けない名前の表
//!
//! 可否は [`BUILTIN_FACETS`] が持つ。**名前をコードへ埋め込まない**——移動方法の
//! 集合は環境ごとに違い、この plugin が知らない名前に同じ性質のものが無いとは
//! 言えない。表に無い名前は書ける側になる。基底に載るのは実測できた分だけで
//! ある。
//!
//! 解決はデータディレクトリと同じく plugin の生存期間中に 1 度だけ行う
//! （[`crate::alias::data_directory`]）。ファイルは編集中に書き換わり得るが、
//! 移動方法の集合は AviUtl2 の版で決まるため、1 度確定させて使う。
//!
//! # 表の形
//!
//! **可否を起こす生成器が書き出すのもこの形である。**
//!
//! ```json
//! {
//!   "movements": {
//!     "移動無し": { "writable": false },
//!     "直線移動": { "writable": true }
//!   }
//! }
//! ```
//!
//! - `movements` は移動方法の名前から可否を引く。**省略できない。**
//! - `writable` は真偽値であり、省略できない。「測っていない」を表す形は無い
//!   ——測れていない名前は表に載せない。

use crate::alias::{data_directory, read_bounded};
use crate::item_facets::parse_object;
use aviutl2_mcp_core::Movement;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

/// 設定ファイルの名前。
const SETTINGS_FILE: &str = "aviutl2.ini";

/// 移動方法 1 つの節の見出しが始まる並び。
const SECTION_PREFIX: &str = "[Movement.";

/// 埋め込む可否の基底。
const BUILTIN_FACETS: &str = include_str!("../data/movement_facets.json");

/// 設定ファイルとして読み込む最大バイト数。
///
/// この経路の費用は要求の内容で決まらないため、予算では守れない。上限は
/// 要求元が動かせない。
pub const MAX_SETTINGS_INI_BYTES: u64 = 4 * 1024 * 1024;

/// 移動方法 1 件について表が述べたこと。
///
/// **未知のフィールドを拒む。** 書き手は我々であり、綴りを外したのなら生成器の
/// 誤りである。素通しにすると、可否が全件「書ける」へ戻ったことに誰も気付け
/// ない。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MovementFacet {
    writable: bool,
}

/// 可否の表 1 つの外形。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FacetsDocument {
    movements: HashMap<String, MovementFacet>,
}

/// 解決した移動方法。
static MOVEMENTS: OnceLock<Vec<Movement>> = OnceLock::new();

/// ホストが受け付ける移動方法を返す。
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
///
/// **一覧を組み立てるのは [`resolve`] だけである。** ここは解決した結果を
/// 覚えるだけであり、名前を絞る手も並べ替える手も持たない。
pub fn movements() -> &'static [Movement] {
    MOVEMENTS.get_or_init(|| {
        resolve(
            data_directory().map(read_movements).unwrap_or_default(),
            &builtin_facets(),
        )
    })
}

/// 埋め込んだ基底を解釈する。
///
/// **解釈できなければ panic する。** ホストと利用者の環境にある `aviutl2.ini`
/// とは扱いを変えている。あちらは無いことも壊れていることも正常な状態のひとつ
/// であり、我々に直す手が無い。こちらは我々が生成してビルドへ焼き込んだもので
/// あり、壊れていれば我々の誤りである。空へ畳むと、可否が全件「書ける」へ戻った
/// 状態が「表を持たない環境」と見分けられないまま出荷される。
///
/// この経路は本番では起こらない。構文はこのモジュールの検査が押さえており、
/// 外れていればビルドの前に落ちる。
fn builtin_facets() -> HashMap<String, MovementFacet> {
    parse_object::<FacetsDocument>(BUILTIN_FACETS.as_bytes())
        .expect("埋め込んだ可否の表を解釈できません")
        .movements
}

/// 読んだ名前の並びへ可否を添えて一覧を組み立てる。
///
/// **名前を落とさず、並びも変えない。** 書けない名前を外すと、それは「一覧に
/// 無い名前」として拒否され、実在する移動方法を無いと告げることになる。
///
/// **表に無い名前は書ける側になる。** 表に載るのは実測できた分だけであり、
/// 環境ごとに追加された移動方法はそこに現れない。
fn resolve(names: Vec<String>, facets: &HashMap<String, MovementFacet>) -> Vec<Movement> {
    let movements: Vec<Movement> = names
        .into_iter()
        .map(|name| Movement {
            writable: facets.get(&name).is_none_or(|facet| facet.writable),
            name,
        })
        .collect();
    if movements.is_empty() {
        tracing::info!("移動方法の一覧を読めませんでした。トラックバーの移動は書き込めません");
    } else {
        tracing::info!(
            "移動方法の一覧: {} 件、うち書けないもの {} 件",
            movements.len(),
            movements
                .iter()
                .filter(|movement| !movement.writable)
                .count()
        );
    }
    movements
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
    use aviutl2_mcp_core::{FiniteF64, TrackValue, TrackWriteTarget, validate_track_value};
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

    /// 可否の表を JSON から組み立てる。
    fn facets(text: &str) -> HashMap<String, MovementFacet> {
        parse_object::<FacetsDocument>(text.as_bytes())
            .expect("解釈できる")
            .movements
    }

    /// 名前の並びを作る。
    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn the_builtin_facets_are_valid_json() {
        // `include_str!` は文字列を取り込むだけで、JSON として解釈できることを
        // 保証しない。解釈できなければ移動方法の一覧そのものが引けない。
        assert!(
            parse_object::<FacetsDocument>(BUILTIN_FACETS.as_bytes()).is_some(),
            "基底を JSON として解釈できません"
        );
    }

    #[test]
    fn the_builtin_facets_still_name_a_movement_that_cannot_be_written() {
        // 基底は実測の記録である。空へ戻っても、書けない名前だけが消えても構文
        // としては通り、可否は全件「書ける」へ静かに倒れる。個別の名前を挙げる
        // 形では押さえられない——集合は環境ごとに違い、基底に載るのは実測できた
        // 分だけである。
        assert!(
            builtin_facets().values().any(|facet| !facet.writable),
            "埋め込んだ基底が、書けない移動方法を 1 つも名乗っていません"
        );
    }

    #[test]
    fn the_builtin_facets_refuse_a_shape_they_did_not_intend() {
        // 書き手は我々である。綴りを外した表を素通しにすると、可否が全件
        // 「書ける」へ戻ったことに誰も気付けない。
        for source in [
            // トップレベルの綴り違い。
            r#"{"movement":{"移動無し":{"writable":false}}}"#,
            // 知らない欄が増えた。
            r#"{"movements":{},"notice":"読む人は居ません"}"#,
            // 葉の綴り違い。**素通しにすると 1 件ずつ静かに書ける側へ倒れる。**
            r#"{"movements":{"移動無し":{"writeable":false}}}"#,
            // 可否そのものが無い。測れていない名前は表に載せない。
            r#"{"movements":{"移動無し":{}}}"#,
            // 可否に「測れていない」を表す形は無い。
            r#"{"movements":{"移動無し":{"writable":null}}}"#,
            r#"{"movements":null}"#,
            // 表ではない。
            "[]",
            r#"[{"movements":{}}]"#,
        ] {
            assert!(
                parse_object::<FacetsDocument>(source.as_bytes()).is_none(),
                "基底が {source} を受け入れました"
            );
        }
    }

    #[test]
    fn a_name_the_facets_do_not_mention_is_writable() {
        // 表に載るのは実測できた分だけである。環境ごとに追加された移動方法は
        // そこに現れず、書ける側で返る。
        let facets = facets(r#"{"movements":{"移動無し":{"writable":false}}}"#);
        assert_eq!(
            resolve(names(&["直線移動", "移動無し", "提供者の移動"]), &facets),
            vec![
                Movement {
                    name: "直線移動".to_string(),
                    writable: true,
                },
                Movement {
                    name: "移動無し".to_string(),
                    writable: false,
                },
                Movement {
                    name: "提供者の移動".to_string(),
                    writable: true,
                },
            ]
        );
    }

    #[test]
    fn the_resolved_list_carries_every_name_that_was_read_in_the_order_it_was_read() {
        // **一覧は名前を落とさない。** 書けない名前を外すと、それは「一覧に無い
        // 名前」として拒否され、実在する移動方法を無いと告げることになる。
        //
        // 入力の並びと出力の名前の並びを突き合わせる。落ちた 1 件を個別に名指し
        // する形では、標本に無い名前が落ちても通ってしまう。
        let facets =
            facets(r#"{"movements":{"移動無し":{"writable":false},"回転":{"writable":false}}}"#);
        let read = names(&["回転", "直線移動", "移動無し", "提供者の移動"]);
        let resolved = resolve(read.clone(), &facets);
        assert_eq!(
            resolved
                .iter()
                .map(|movement| movement.name.clone())
                .collect::<Vec<String>>(),
            read
        );
    }

    #[test]
    fn what_the_list_calls_unwritable_is_what_the_write_refuses() {
        // **一覧と拒否が同じ表を読む。** 一覧が返した 1 件ずつについて、書けない
        // と名乗ったものは検証が拒み、書けると名乗ったものは名前を理由に拒まれ
        // ない。名前を書き並べた検査は、表が変わったときにこの規律を守らない。
        let facets = facets(
            r#"{"movements":{"移動無し":{"writable":false},"書けない移動":{"writable":false}}}"#,
        );
        let movements = resolve(
            names(&["直線移動", "移動無し", "曲線移動", "書けない移動"]),
            &facets,
        );
        let target = TrackWriteTarget {
            section_count: 1,
            movements: &movements,
        };
        for movement in &movements {
            let value = TrackValue {
                values: vec![
                    FiniteF64::try_new(0.0).expect("有限値"),
                    FiniteF64::try_new(100.0).expect("有限値"),
                ],
                mode: Some(movement.name.clone()),
                params: Vec::new(),
                accelerate: false,
                decelerate: false,
                twopoint: false,
                reserved_flags: 0,
                expression: None,
            };
            let reason = validate_track_value(&value, target)
                .err()
                .and_then(|error| error.reason());
            if movement.writable {
                assert!(
                    !matches!(
                        reason,
                        Some("track_mode_unknown" | "track_mode_not_writable")
                    ),
                    "{} が名前を理由に拒まれました: {reason:?}",
                    movement.name
                );
            } else {
                assert_eq!(
                    reason,
                    Some("track_mode_not_writable"),
                    "{} が書けないと名乗ったのに拒まれません",
                    movement.name
                );
            }
        }
    }
}
