//! 有効・無効を切り替えられるチェックボックス。
//!
//! **同意の内訳を無効表示にする口が、素のチェックボックスに無い。** 他の入力
//! （数値・コンボボックス・テキスト）は生成時に無効状態を適用できるが、
//! チェックボックスだけは `EnableWindow` を掛ける契機を外から作れない。
//! ここが包むのはその 1 点であり、値の保持も通知の配送も内側へそのまま委ねる。
//!
//! **値は保つ。** 無効にしても選択状態は変わらず、有効へ戻せば元の値がそこに
//! ある——同意を戻したときに前の選択が残ることを、画面の側でも成り立たせる。

use std::cell::Cell;
use std::rc::Rc;
use win32_ui::Result;
use win32_ui::layout::SizeValue;
use win32_ui::widget::{CheckBox, CreateCtx, MeasureCtx, Widget};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;

/// 有効・無効を切り替えられるチェックボックス。
///
/// クローンは同じ状態を指す。画面へ置いた後も、控えたクローンから
/// [`Toggle::set_enabled`] を呼べる。
#[derive(Clone)]
pub struct Toggle(Rc<Inner>);

struct Inner {
    checkbox: CheckBox,
    enabled: Cell<bool>,
}

impl Toggle {
    /// 見出し付きの切り替えを作る。
    pub fn new(label: &str) -> Self {
        Self(Rc::new(Inner {
            checkbox: CheckBox::new(label),
            enabled: Cell::new(true),
        }))
    }

    /// 初期の選択状態。
    pub fn checked(self, checked: bool) -> Self {
        self.set_checked(checked);
        self
    }

    /// 初期の有効・無効。
    pub fn enabled(self, enabled: bool) -> Self {
        self.set_enabled(enabled);
        self
    }

    /// 幅を与える。
    pub fn with_width(self, width: SizeValue) -> Self {
        let _ = self.0.checkbox.clone().with_width(width);
        self
    }

    /// 選択状態の変化時のハンドラを足す。
    pub fn on_change<F>(self, handler: F) -> Self
    where
        F: FnMut(bool) + 'static,
    {
        let _ = self.0.checkbox.clone().on_change(handler);
        self
    }

    /// 現在の選択状態。
    pub fn is_checked(&self) -> bool {
        self.0.checkbox.is_checked()
    }

    /// 選択状態を差し替える。**有効・無効には触れない。**
    pub fn set_checked(&self, checked: bool) {
        self.0.checkbox.set_checked(checked);
    }

    /// 有効・無効を切り替える。**選択状態には触れない。**
    pub fn set_enabled(&self, enabled: bool) {
        self.0.enabled.set(enabled);
        self.apply_enabled();
    }

    /// 有効かどうか。
    pub fn is_enabled(&self) -> bool {
        self.0.enabled.get()
    }

    /// 保持している有効・無効をウィンドウへ反映する。
    ///
    /// ウィンドウがまだ無ければ何もしない。生成の直後に呼ぶため、開いた時点の
    /// 状態も反映される。
    fn apply_enabled(&self) {
        let Some(hwnd) = self.0.checkbox.hwnd() else {
            return;
        };
        // SAFETY: `hwnd` は内側のチェックボックスが所有する生存中のウィンドウ。
        unsafe {
            let _ = EnableWindow(hwnd, self.0.enabled.get());
        }
    }
}

impl Widget for Toggle {
    fn build_node(&self, tree: &mut taffy::TaffyTree, ctx: &MeasureCtx) -> Result<taffy::NodeId> {
        self.0.checkbox.build_node(tree, ctx)
    }

    fn create(&self, ctx: &mut CreateCtx, offset: (f32, f32)) -> Result<Vec<i32>> {
        let ids = self.0.checkbox.create(ctx, offset)?;
        self.apply_enabled();
        Ok(ids)
    }

    fn on_command(&self, code: u16) {
        self.0.checkbox.on_command(code);
    }

    fn on_notify(&self, code: u32) {
        self.0.checkbox.on_notify(code);
    }

    fn cache_state(&self) {
        self.0.checkbox.cache_state();
    }

    fn hwnd(&self) -> Option<HWND> {
        self.0.checkbox.hwnd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabling_keeps_the_value() {
        let toggle = Toggle::new("生成する").checked(true);
        assert!(toggle.is_enabled());

        toggle.set_enabled(false);
        assert!(!toggle.is_enabled());
        assert!(toggle.is_checked(), "無効にしたら値が失われました");

        toggle.set_enabled(true);
        assert!(toggle.is_checked());
    }

    #[test]
    fn a_clone_shares_the_state() {
        // 画面へ置いた後も、控えたクローンから切り替えられなければならない。
        let toggle = Toggle::new("生成する");
        let placed = toggle.clone();
        toggle.set_enabled(false);
        assert!(!placed.is_enabled());
    }
}
