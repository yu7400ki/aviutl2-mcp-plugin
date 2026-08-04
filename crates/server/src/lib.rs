//! AviUtl2 MCP Plugin 用 stdio MCP server のライブラリ。
//!
//! stdout は MCP プロトコル専用とし、ログは stderr へ出力する。
//! discovery と読み取り operation を MCP の read tool / resource として提供する。

pub mod api;
pub mod artifact;
pub mod discovery;
pub mod identity;
pub mod mcp;
pub mod pipe_client;
pub mod redact;
pub mod settings;
pub mod win_io;

/// 既定のログレベル。
///
/// operation・correlation_id・所要時間・結果コードの記録は運用上の要求であり、
/// `RUST_LOG` を設定しない利用者でも失われないよう `info` を既定とする。
/// `EnvFilter` の既定は `error` であるため、明示的に上書きする。
///
/// 共有設定の `log_level` の既定と同じ値であり、設定を持たない呼び出し口が
/// 使う。
const DEFAULT_LOG_FILTER: &str = aviutl2_mcp_core::settings::DEFAULT_LOG_LEVEL;

/// 要求・応答の本文をそのまま記録する外部 crate に課すレベル上限。
///
/// 本文には `instance_id` のように対象を一意に特定できる値が原文で現れる。
/// 自クレートは [`redact`] を通した匿名化表現だけを記録するが、対象を指定しない
/// `RUST_LOG=debug` はその外側の crate まで一括で引き上げるため、匿名化が
/// 迂回される。ログは不具合の報告に添えて持ち出されるので、匿名化を自クレートの
/// 責務として保てるよう、本文を出す crate には `RUST_LOG` の内容によらず上限を課す。
///
/// 上限は環境変数で解除できない。解除手段を用意すると、不具合の報告を求める
/// 場面でそれを有効にするよう案内され、塞いだ経路がそのまま開き直されてしまう。
const EXTERNAL_LOG_CEILINGS: &[&str] = &["rmcp=warn"];

/// ログを stderr へ構造化出力するよう初期化する。
///
/// レベルは `RUST_LOG` 環境変数、引数の `log_level`、[`DEFAULT_LOG_FILTER`] の
/// 順に採る。**環境変数を先に見るのは、設定ファイルごと読めない状況を診断する
/// 経路を残すためである。** `LOG_FORMAT=json` で JSON 出力を選ぶ。いずれの
/// 場合も [`EXTERNAL_LOG_CEILINGS`] は適用される。
///
/// **レベルはプロセスの寿命の間ずっと固定である。** subscriber は一度しか
/// 立てられず、設定を変えたときに効くのは次回の起動からになる。
pub fn init_logging(log_level: &str) {
    let format = std::env::var("LOG_FORMAT").unwrap_or_default();
    let json_mode = format.eq_ignore_ascii_case("json");

    let builder = tracing_subscriber::fmt()
        .with_env_filter(env_filter(log_level))
        .with_writer(std::io::stderr)
        .with_ansi(false);

    if json_mode {
        builder.json().init();
    } else {
        builder.init();
    }
}

/// `RUST_LOG` を読み、未設定・不正なら与えられたレベルへ落とす。
///
/// 読み取った内容に [`EXTERNAL_LOG_CEILINGS`] を重ねる。同じ対象への指定は
/// 後から足したものが優先されるため、`RUST_LOG` が何を指定していても上限は残る。
fn env_filter(log_level: &str) -> tracing_subscriber::EnvFilter {
    let mut filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::try_new(log_level)
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_LOG_FILTER))
    });
    for ceiling in EXTERNAL_LOG_CEILINGS {
        filter = filter.add_directive(parse_ceiling(ceiling));
    }
    filter
}

/// レベル上限の指定を directive へ解釈する。
///
/// 対象は本モジュールの定数だけであり、解釈できない書式は
/// [`external_log_ceilings_are_valid_directives`] が起動前に弾く。
fn parse_ceiling(ceiling: &str) -> tracing_subscriber::filter::Directive {
    ceiling
        .parse()
        .unwrap_or_else(|e| panic!("レベル上限 {ceiling} を解釈できません: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_log_ceilings_are_valid_directives() {
        for ceiling in EXTERNAL_LOG_CEILINGS {
            parse_ceiling(ceiling);
        }
    }

    #[test]
    fn external_log_ceilings_survive_a_verbose_rust_log() {
        // `RUST_LOG=debug` のように対象を指定しない設定でも上限は残る。
        let mut filter = tracing_subscriber::EnvFilter::new("debug");
        for ceiling in EXTERNAL_LOG_CEILINGS {
            filter = filter.add_directive(parse_ceiling(ceiling));
        }

        let rendered = filter.to_string();
        for ceiling in EXTERNAL_LOG_CEILINGS {
            assert!(
                rendered.contains(ceiling),
                "上限 {ceiling} が失われています: {rendered}"
            );
        }
    }
}
