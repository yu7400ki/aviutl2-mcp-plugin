//! 公開する tool の集合と、その変化の観測。
//!
//! # 判定の材料は 1 つの snapshot だけである
//!
//! `tools/list` の filtering も call-time の受付判定も [`ToolVisibility`] を
//! 通る。[`ToolVisibility`] は設定 snapshot 1 つから作られ、snapshot は 1 回の
//! `Arc` の差し替えで反映される。**半分だけ適用された状態を観測する経路が無い。**
//!
//! # 常時有効な tool の floor
//!
//! [`ALWAYS_ENABLED_TOOL`] は公開しない指定に含まれていても必ず公開する。判定の
//! 最終段（[`ToolVisibility::allows`]）で適用するため、どの経路から問い合わせても
//! 同じ結果になる。

use crate::settings::SettingsSource;
use aviutl2_mcp_core::settings::Settings;
use aviutl2_mcp_core::tool::ALWAYS_ENABLED_TOOL;
use std::collections::BTreeSet;
use std::sync::Arc;

/// 現在どの tool を公開するかの判定。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolVisibility {
    disabled: BTreeSet<String>,
}

impl ToolVisibility {
    /// 設定 snapshot から作る。
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            disabled: settings.disabled_tools().clone(),
        }
    }

    /// 全 tool を公開する判定。
    ///
    /// 共有設定を持たない構築口が使う。設定が無いことを無効化の表明とは
    /// 見なさない。
    pub fn all_enabled() -> Self {
        Self::default()
    }

    /// tool を公開するか。
    ///
    /// **[`ALWAYS_ENABLED_TOOL`] の floor をここで適用する。** 公開しない指定に
    /// 含まれていても常に真を返す。
    pub fn allows(&self, name: &str) -> bool {
        name == ALWAYS_ENABLED_TOOL || !self.disabled.contains(name)
    }

    /// 与えた catalog のうち公開する名前を並べる。
    pub fn visible<'a, I>(&self, catalog: I) -> BTreeSet<String>
    where
        I: IntoIterator<Item = &'a str>,
    {
        catalog
            .into_iter()
            .filter(|name| self.allows(name))
            .map(str::to_string)
            .collect()
    }
}

/// 公開する tool の集合の変化を追う。
///
/// 設定が差し替わっても、**公開する集合が変わらなければ何も起きない。** ログ
/// レベルや期限だけを変えたときに要求元へ `tools/list` の取り直しを求めない
/// ためである。
///
/// 観測を取りこぼしても正しさは失われない。`tools/list` と call-time の受付
/// 判定はいずれも要求の時点で現在の snapshot を読むため、**通知は最適化で
/// あって正しさの担保ではない。**
#[derive(Debug)]
pub struct ToolListWatch {
    source: Arc<SettingsSource>,
    catalog: Vec<String>,
    applied: u64,
    visible: BTreeSet<String>,
}

impl ToolListWatch {
    /// 現在の集合を基準として観測を始める。
    ///
    /// `catalog` は server が登録している tool 名の全体である。
    pub fn new(source: Arc<SettingsSource>, catalog: Vec<String>) -> Self {
        let (applied, visible) = Self::observe(&source, &catalog);
        Self {
            source,
            catalog,
            applied,
            visible,
        }
    }

    /// 公開する集合が前回の観測から変わっていれば真を返す。
    ///
    /// 設定が差し替わっていなければ集合を計算し直さない。
    pub fn changed(&mut self) -> bool {
        if self.source.applied() == self.applied {
            return false;
        }
        let (applied, visible) = Self::observe(&self.source, &self.catalog);
        self.applied = applied;
        if visible == self.visible {
            return false;
        }
        self.visible = visible;
        true
    }

    /// 現在公開している tool 名。
    pub fn visible(&self) -> &BTreeSet<String> {
        &self.visible
    }

    /// 差し替えの回数と、そのとき以降の snapshot から求めた集合を取る。
    ///
    /// **回数を先に読む。** 後に読むと、読み終えてから snapshot を取るまでの間の
    /// 差し替えを「観測済み」として記録してしまい、その変化に気付けなくなる。
    /// 先に読めば記録する回数は snapshot と同じか古いだけであり、次の観測で
    /// 必ず拾い直す。
    fn observe(source: &SettingsSource, catalog: &[String]) -> (u64, BTreeSet<String>) {
        let applied = source.applied();
        let settings = source.settings();
        let visible =
            ToolVisibility::from_settings(&settings).visible(catalog.iter().map(String::as_str));
        (applied, visible)
    }
}

#[cfg(test)]
mod tests;
