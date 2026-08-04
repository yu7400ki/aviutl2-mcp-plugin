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
use tokio::sync::watch;

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

/// 公開する tool の集合が変わるのを待ち受ける。
///
/// **供給元が押し出す。** 設定が差し替わったときにだけ起床するため、何も
/// 変わっていない間に問い合わせる経路が無い。
///
/// 設定が差し替わっても、**公開する集合が変わらなければ起床したことを外へ
/// 伝えない。** ログレベルや期限だけを変えたときに要求元へ `tools/list` の
/// 取り直しを求めないためである。
///
/// 起床を取りこぼしても正しさは失われない。`tools/list` と call-time の受付
/// 判定はいずれも要求の時点で現在の snapshot を読むため、**通知は最適化で
/// あって正しさの担保ではない。**
#[derive(Debug)]
pub struct ToolListWatch {
    settings: watch::Receiver<Arc<Settings>>,
    catalog: Vec<String>,
    visible: BTreeSet<String>,
}

impl ToolListWatch {
    /// 現在の集合を基準として待ち受けを始める。
    ///
    /// `catalog` は server が登録している tool 名の全体である。
    ///
    /// **供給元への参照を持たない。** 購読だけを持つため、供給元が失われれば
    /// [`ToolListWatch::changed`] が終わりを返す。待ち受ける側が供給元を
    /// 生かし続けることはない。
    pub fn new(source: &SettingsSource, catalog: Vec<String>) -> Self {
        let settings = source.subscribe();
        let visible = ToolVisibility::from_settings(&settings.borrow())
            .visible(catalog.iter().map(String::as_str));
        Self {
            settings,
            catalog,
            visible,
        }
    }

    /// 公開する集合が変わるまで待つ。
    ///
    /// 変わったら真を返す。**供給元が失われたら偽を返して終わる**——待ち受けを
    /// 畳む契機はこれだけである。
    ///
    /// 起床しても集合が同じなら、外へ伝えずに待ち直す。
    pub async fn changed(&mut self) -> bool {
        while self.settings.changed().await.is_ok() {
            let visible = {
                let settings = self.settings.borrow_and_update();
                ToolVisibility::from_settings(&settings)
                    .visible(self.catalog.iter().map(String::as_str))
            };
            if visible == self.visible {
                continue;
            }
            self.visible = visible;
            return true;
        }
        false
    }

    /// 現在公開している tool 名。
    pub fn visible(&self) -> &BTreeSet<String> {
        &self.visible
    }
}

#[cfg(test)]
mod tests;
