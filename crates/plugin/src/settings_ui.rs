//! 設定画面。
//!
//! ホストの設定メニューから開く modal ダイアログである。**開く手続きと値の
//! 詰め替えを分けてある**——後者は [`form`] にあり、HWND を持たずに確かめられる。
//!
//! # 触れるのはモジュールの `static` だけである
//!
//! 設定メニューのコールバックは `extern "C" fn` でありキャプチャを持てない。
//! 触れるのは [`crate::settings`] が持つモジュールの `static`（設定ファイルの
//! 場所の解決結果と現在の snapshot）だけであり、**plugin の singleton にも編集
//! ハンドルにも触れない。** どのスレッドから呼ばれても成り立つ形である。
//!
//! ラッパーの設定メニュー用のマクロを使わないのも同じ理由による。マクロが生成
//! するブリッジは plugin の singleton のロックを保持したままハンドラを実行する
//! が、**ハンドラは利用者が閉じるまで戻らない。** その間の終了手順が singleton
//! へ到達できなくなる。
//!
//! # 塞がるのはホストの UI だけである
//!
//! ダイアログを開いている間も要求処理は接続受理スレッドで走る。塞がるのは
//! AviUtl2 の UI であり、MCP は止まらない。

pub mod form;

use crate::settings;
use aviutl2_mcp_core::tool::ToolFamily;
use form::{BehaviorGroup, SettingsForm};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use win32_ui::layout::{Dimension, FlexLayout, JustifyContent, SizeValue, Tabs, labeled};
use win32_ui::widget::{Button, Label};
use win32_ui::{Dialog, DialogHandle, MessageBox};
use windows::Win32::Foundation::HWND;

/// ホストの設定メニューに出す名前。
pub const MENU_NAME: &str = "AviUtl2 MCP の設定";

/// ダイアログとメッセージボックスの見出し。
const DIALOG_TITLE: &str = "AviUtl2 MCP の設定";

/// tool の一覧を並べる列数。
const TOOL_COLUMNS: usize = 3;

/// tool 1 つ分の幅（論理 px）。最も長い tool 名が収まる幅を採る。
const TOOL_COLUMN_WIDTH: f32 = 230.0;

/// 「動作」ページの入力欄 1 つ分の幅（論理 px）。
const FIELD_WIDTH: f32 = 220.0;

/// 「動作」ページの 1 行に並べる入力欄の数。
const FIELDS_PER_ROW: usize = 2;

/// ボタンの幅（論理 px）。
const BUTTON_WIDTH: f32 = 90.0;

const PAGE_PADDING: f32 = 10.0;
const GROUP_GAP: f32 = 10.0;
const ITEM_GAP: f32 = 6.0;

/// 設定メニューのコールバック。
///
/// `extern "C" fn` の境界を越える unwind は、`panic = "unwind"` であっても abort
/// へ変換される。**したがって捕捉はこの関数の内側でなければならない。**
///
/// 捕捉を二重にしてあるのは、内側で記録に失敗しても境界を越えさせないため
/// である。ウィンドウプロシージャの内側にも捕捉層があるが、そちらとは独立に
/// 要る——コールバックの入口はプロシージャの外にある。
pub extern "C" fn config_menu_callback(
    parent: aviutl2::sys::plugin2::HWND,
    _instance: aviutl2::sys::plugin2::HINSTANCE,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let opened = catch_unwind(AssertUnwindSafe(|| open(HWND(parent))));
        if opened.is_err() {
            tracing::error!("設定画面の処理で panic を捕捉しました");
        }
    }));
}

/// 設定画面を開き、閉じるまで戻らない。
fn open(parent: HWND) {
    // 画面が映すのは**いま**ファイルにある内容である。別のプロセスが書いた
    // 変更もここで取り込む。
    settings::refresh();
    let settings = settings::current();
    let form = Rc::new(SettingsForm::new(&settings));

    let dialog = Dialog::new(DIALOG_TITLE);
    let handle = dialog.handle();
    let root = FlexLayout::column()
        .with_gap(GROUP_GAP)
        .with_padding(PAGE_PADDING)
        .with_layout(
            Tabs::new()
                .with_page("公開する tool", tool_page(&form))
                .with_page("動作", behavior_page(&form))
                .with_selected(0),
        )
        .with_layout(button_row(&form, &handle));

    if let Err(e) = dialog.with_layout(root).open(parent) {
        tracing::warn!("設定画面を開けませんでした: {e}");
        MessageBox::error(
            Some(parent),
            &format!("設定画面を開けませんでした。\n{e}"),
            DIALOG_TITLE,
        );
    }
}

/// 「公開する tool」ページ。
///
/// 族ごとに見出しを付け、その中を多列に並べる。**説明文は載せない**——tool の
/// 説明の出所は MCP server の tool 定義 1 つであり、写すと 2 か所で管理すること
/// になる。
fn tool_page(form: &SettingsForm) -> FlexLayout {
    let mut page = FlexLayout::column()
        .with_gap(GROUP_GAP)
        .with_padding(PAGE_PADDING);
    for family in ToolFamily::ALL {
        let toggles: Vec<_> = form.tools_in(family).collect();
        let mut rows = FlexLayout::column().with_gap(ITEM_GAP);
        for chunk in toggles.chunks(TOOL_COLUMNS) {
            let mut row = FlexLayout::row().with_gap(ITEM_GAP);
            for toggle in chunk {
                row = row.with_widget(
                    toggle
                        .control()
                        .with_width(SizeValue::Points(TOOL_COLUMN_WIDTH)),
                );
            }
            rows = rows.with_layout(row);
        }
        page = page
            .with_widget(Label::new(family_label(family)))
            .with_layout(rows);
    }
    page
}

/// 「動作」ページ。
///
/// 群ごとに見出しを付け、入力欄を [`FIELDS_PER_ROW`] 個ずつ折り返して並べる。
fn behavior_page(form: &SettingsForm) -> FlexLayout {
    let mut page = FlexLayout::column()
        .with_gap(GROUP_GAP)
        .with_padding(PAGE_PADDING);
    for group in BehaviorGroup::ALL {
        let mut fields: Vec<FlexLayout> = Vec::new();
        if group == BehaviorGroup::Log {
            let choice = form.log_level();
            fields.push(field(labeled(choice.label(), choice.control())));
        }
        for input in form.numbers_in(group) {
            fields.push(field(labeled(&input.label(), input.control())));
        }
        page = page
            .with_widget(Label::new(group.label()))
            .with_layout(wrap(fields, FIELDS_PER_ROW));
    }
    page
}

/// レイアウトを `per_row` 個ずつ行へ折り返し、縦に積む。
fn wrap(items: Vec<FlexLayout>, per_row: usize) -> FlexLayout {
    let new_row = || FlexLayout::row().with_gap(ITEM_GAP);
    let mut rows = FlexLayout::column().with_gap(ITEM_GAP);
    let mut row = new_row();
    let mut filled = 0;
    for item in items {
        row = row.with_layout(item);
        filled += 1;
        if filled == per_row {
            rows = rows.with_layout(std::mem::replace(&mut row, new_row()));
            filled = 0;
        }
    }
    if filled > 0 {
        rows = rows.with_layout(row);
    }
    rows
}

/// OK とキャンセル。
///
/// **タブの外に置く。** どのページを見ていても同じ位置にある。
fn button_row(form: &Rc<SettingsForm>, handle: &DialogHandle) -> FlexLayout {
    let accept = {
        let form = Rc::clone(form);
        let handle = handle.clone();
        Button::primary("OK")
            .with_width(SizeValue::Points(BUTTON_WIDTH))
            .on_click(move || on_accept(&form, &handle))
    };
    let cancel = {
        let handle = handle.clone();
        Button::secondary("キャンセル")
            .with_width(SizeValue::Points(BUTTON_WIDTH))
            .on_click(move || handle.cancel())
    };
    FlexLayout::row()
        .with_gap(ITEM_GAP)
        .with_justify_content(JustifyContent::End)
        .with_widget(accept)
        .with_widget(cancel)
}

/// OK を押したときの処理。
///
/// **全項目を読んで検証し、範囲外なら閉じない。** 保存に失敗した場合も閉じない
/// ——黙って閉じると、利用者は変更が失われたことを知らないまま去る。
///
/// ウィンドウプロシージャの内側にも捕捉層があるが、ここでも捕捉する。**層が
/// 1 つだと、その実装が古い版に差し替わったときに穴が開く。**
fn on_accept(form: &SettingsForm, handle: &DialogHandle) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let parent = handle.hwnd();
        let change = match form.collect() {
            Ok(change) => change,
            Err(messages) => {
                MessageBox::error(
                    parent,
                    &format!("入力できない値があります。\n\n{}", messages.join("\n")),
                    DIALOG_TITLE,
                );
                return;
            }
        };
        if !change.is_empty()
            && let Err(e) = settings::save(&change)
        {
            tracing::error!("設定を保存できませんでした: {e:#}");
            MessageBox::error(
                parent,
                &format!("設定を保存できませんでした。\n{e:#}"),
                DIALOG_TITLE,
            );
            return;
        }
        handle.accept();
    }));
}

/// 族の見出し。
fn family_label(family: ToolFamily) -> &'static str {
    match family {
        ToolFamily::Read => "読み取り",
        ToolFamily::Edit => "編集",
        ToolFamily::Render => "描画",
    }
}

/// 入力欄 1 つ分の幅を与える。
fn field(layout: FlexLayout) -> FlexLayout {
    layout.with_width(Dimension::length(FIELD_WIDTH))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::settings::Settings;

    /// 族の見出しが全 variant に付いていること。
    #[test]
    fn every_family_has_a_heading() {
        for family in ToolFamily::ALL {
            assert!(!family_label(family).is_empty(), "{family:?}");
        }
    }

    /// 群の見出しが全 variant に付いていること。
    #[test]
    fn every_behavior_group_has_a_heading() {
        for group in BehaviorGroup::ALL {
            assert!(!group.label().is_empty(), "{group:?}");
        }
    }

    /// 「動作」ページの入力欄が、群の見出しごとに 1 つ以上あること。
    ///
    /// **見出しだけの群を作らない。** 画面の高さは最も高いページに揃うため、
    /// 中身の無い見出しはそのまま余白になる。
    #[test]
    fn every_behavior_group_carries_at_least_one_field() {
        let form = SettingsForm::new(&Settings::default());

        for group in BehaviorGroup::ALL {
            let count = form.numbers_in(group).count() + usize::from(group == BehaviorGroup::Log);
            assert!(count > 0, "{group:?} に入力欄がありません");
        }
    }
}
