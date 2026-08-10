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
pub mod settings;
#[cfg(test)]
mod test_support;
pub mod win_io;

use aviutl2_mcp_core::settings::Settings;
use std::sync::OnceLock;
use tracing::warn;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::reload;
use tracing_subscriber::util::SubscriberInitExt;

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
/// 自クレートは [`aviutl2_mcp_core::redact`] を通した匿名化表現だけを記録する
/// が、対象を指定しない `RUST_LOG=debug` はその外側の crate まで一括で引き
/// 上げるため、匿名化が迂回される。ログは不具合の報告に添えて持ち出されるので、
/// 匿名化を自クレートの責務として保てるよう、本文を出す crate には `RUST_LOG`
/// の内容によらず上限を課す。
///
/// 上限は環境変数で解除できない。解除手段を用意すると、不具合の報告を求める
/// 場面でそれを有効にするよう案内され、塞いだ経路がそのまま開き直されてしまう。
const EXTERNAL_LOG_CEILINGS: &[&str] = &["rmcp=warn"];

/// 稼働中の subscriber のレベルを差し替える口。
///
/// subscriber はプロセスに 1 つしか無いため、口も 1 つで足りる。
/// [`init_logging`] を呼んでいない場合（試験など）は空のままであり、
/// [`apply_log_level`] は何もしない。
static LOG_RELOAD: OnceLock<Box<dyn Fn(EnvFilter) + Send + Sync>> = OnceLock::new();

/// ログを stderr へ構造化出力するよう初期化する。
///
/// レベルは `RUST_LOG` 環境変数、設定の `log_level`、[`DEFAULT_LOG_FILTER`] の
/// 順に採る。**環境変数を先に見るのは、設定ファイルごと読めない状況を診断する
/// 経路を残すためである。** いずれの場合も [`EXTERNAL_LOG_CEILINGS`] は
/// 適用される。
///
/// **レベルは稼働中に差し替えられる。** filter を `reload::Layer` の下に置き、
/// 設定が変わったら [`apply_log_level`] が差し替える。挟まるのは読み取りロック
/// 1 回であり、**他の 8 項目と同じく「保存すれば効く」形になる。**
pub fn init_logging(settings: &Settings) {
    let (filter, rejected) = build_filter(settings.effective_log_level());
    let (layer, handle) = reload::Layer::new(filter);
    let _ = LOG_RELOAD.set(Box::new(move |filter| {
        if let Err(e) = handle.reload(filter) {
            warn!(error = %e, "ログレベルを差し替えられませんでした");
        }
    }));

    let registry = tracing_subscriber::registry().with(layer);
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(false);
    registry.with(fmt_layer).init();

    report_rejected_log_level(rejected);
}

/// 設定のログレベルを稼働中の subscriber へ反映する。
///
/// [`init_logging`] を呼んでいない場合は何もしない。
pub fn apply_log_level(settings: &Settings) {
    let Some(reload) = LOG_RELOAD.get() else {
        return;
    };
    let (filter, rejected) = build_filter(settings.effective_log_level());
    report_rejected_log_level(rejected);
    reload(filter);
}

/// 解釈できなかったログレベルの指定を記録する。
fn report_rejected_log_level(rejected: Option<String>) {
    if let Some(rejected) = rejected {
        warn!("ログレベル {rejected} を解釈できないため {DEFAULT_LOG_FILTER} を用います");
    }
}

/// `RUST_LOG` を読み、未設定・不正なら与えられたレベルへ落とす。
///
/// 戻り値の 2 つ目は、**解釈できなかったために既定へ戻した指定**である。
/// `EnvFilter::new` は解釈に失敗しても値を返す lossy な口であり、そのまま使うと
/// 記録が `error` 以下へ落ちたことを誰も知らせない。
///
/// 読み取った内容に [`EXTERNAL_LOG_CEILINGS`] を重ねる。同じ対象への指定は
/// 後から足したものが優先されるため、`RUST_LOG` が何を指定していても上限は残る。
fn build_filter(log_level: &str) -> (EnvFilter, Option<String>) {
    let (mut filter, rejected) = match EnvFilter::try_from_default_env() {
        Ok(filter) => (filter, None),
        Err(_) => match EnvFilter::try_new(log_level) {
            Ok(filter) => (filter, None),
            Err(_) => (
                EnvFilter::new(DEFAULT_LOG_FILTER),
                Some(log_level.to_string()),
            ),
        },
    };
    for ceiling in EXTERNAL_LOG_CEILINGS {
        filter = filter.add_directive(parse_ceiling(ceiling));
    }
    (filter, rejected)
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
