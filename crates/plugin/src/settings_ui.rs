//! 設定画面。
//!
//! ホストの設定メニューから開く modal ダイアログである。**開く手続きと値の
//! 詰め替えを分けてある**——後者は [`form`] にあり、HWND を持たずに確かめられる。
//!
//! # 触れるのはモジュールの `static` だけである
//!
//! 設定メニューのコールバックは `extern "C" fn` でありキャプチャを持てない。
//! 触れるのは次の 2 つの `static` だけであり、**plugin の singleton にも編集
//! ハンドルにも触れない。** どのスレッドから呼ばれても成り立つ形である。
//!
//! | 触れるもの | 経路 |
//! |---|---|
//! | [`crate::settings`] の読み書き口（設定ファイルの場所と現在の snapshot） | `refresh` / `current` / `save` |
//! | 稼働中の記録の水準を差し替える口 | `refresh` / `save` が設定の変化を記録の層へ届ける |
//!
//! **いずれもロックを保持したままダイアログへ入らない。** 保持するのは呼び出し
//! 1 つ分の区間だけである。
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
use aviutl2_mcp_core::settings::SettingsChange;
use aviutl2_mcp_core::tool::ToolFamily;
use form::{BehaviorGroup, SettingsForm};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use win32_ui::layout::{Dimension, FlexLayout, JustifyContent, SizeValue, Tabs, labeled};
use win32_ui::widget::{Button, Label};
use win32_ui::{Dialog, DialogHandle, MessageBox};
use windows::Win32::Foundation::HWND;

/// ホストの設定メニューに出す名前。
pub const MENU_NAME: &str = "AviUtl2 MCP";

/// agent plugin の生成を扱うページの見出し。
///
/// **生成物の `README.md` がこの名前で画面への道順を示す。** 写しにすると
/// 見出しを変えたときに片方だけが古くなり、読み手が存在しないページを探す。
pub const AGENT_PLUGIN_PAGE: &str = "エージェントプラグイン";

/// ダイアログとメッセージボックスの見出し。
///
/// **メニューの名前より 1 語長い。** 窓は親の見出しを伴わずに開くため、
/// 名前だけでは何を映しているのかが分からない。
const DIALOG_TITLE: &str = "AviUtl2 MCP の設定";

/// tool の一覧を並べる列数。
const TOOL_COLUMNS: usize = 3;

/// tool 1 つ分の幅（論理 px）。最も長い tool 名が収まる幅を採る。
const TOOL_COLUMN_WIDTH: f32 = 230.0;

/// 「動作」ページの入力欄 1 つ分の幅（論理 px）。
const FIELD_WIDTH: f32 = 220.0;

/// 「動作」ページの 1 行に並べる入力欄の数。
const FIELDS_PER_ROW: usize = 2;

/// 「エージェントプラグイン」ページの切り替え 1 つ分の幅（論理 px）。
const AGENT_PLUGIN_TOGGLE_WIDTH: f32 = 320.0;

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
                .with_page(AGENT_PLUGIN_PAGE, agent_plugin_page(&form))
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

/// 「エージェントプラグイン」ページ。
///
/// **生成先を 1 行で示す。** 利用者は client へ marketplace を登録するときに
/// その場所を要する。示さなければ、この画面と client 側の作業が繋がらない。
fn agent_plugin_page(form: &SettingsForm) -> FlexLayout {
    let mut toggles = FlexLayout::column().with_gap(ITEM_GAP);
    for input in form.agent_plugin() {
        toggles = toggles.with_widget(
            input
                .control()
                .with_width(SizeValue::Points(AGENT_PLUGIN_TOGGLE_WIDTH)),
        );
    }
    FlexLayout::column()
        .with_gap(GROUP_GAP)
        .with_padding(PAGE_PADDING)
        .with_widget(Label::new("生成するもの"))
        .with_layout(toggles)
        .with_widget(Label::new("生成先"))
        .with_layout(
            FlexLayout::column()
                .with_gap(ITEM_GAP)
                .with_widget(Label::new(&destination_line())),
        )
}

/// 生成先を 1 行で示す。
///
/// 解決できない場合も置き場所の**書き方**は示す。空欄にすると、利用者は
/// どこを探せばよいか分からないまま画面を閉じる。
fn destination_line() -> String {
    match crate::registry::discovery_root() {
        Ok(root) => root.display().to_string(),
        Err(_) => r"%LOCALAPPDATA%\AviUtl2Mcp".to_string(),
    }
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
fn on_accept(form: &SettingsForm, handle: &DialogHandle) {
    guarded_accept(form, handle, saving_and_syncing(settings::save));
}

/// 保存に続けて生成物へ反映する口を組み立てる。
///
/// **適用した時点で反映する。** 有効にした直後に何も現れないのは、利用者から
/// 見て失敗と区別が付かない。起動時（`register`）も同じ
/// [`crate::agent_plugin::sync`] を呼び、そちらは差分の是正であって別の判断を
/// 持たない。
///
/// **保存が失敗したら反映しない。** 書けなかった設定に合わせて生成物を動かせば、
/// 画面が示す状態とディスクの状態が食い違う。
///
/// これは設定画面の規律を破らない——生成に要るのは設定と自 DLL のパスだけで
/// あり、plugin の singleton にも編集ハンドルにも触れない。
///
/// 保存の口を引数に取るのは、合成そのものを画面なしで確かめられるようにする
/// ためである。
fn saving_and_syncing<S>(save: S) -> impl FnOnce(&SettingsChange) -> anyhow::Result<()>
where
    S: FnOnce(&SettingsChange) -> anyhow::Result<()>,
{
    move |change| {
        save(change)?;
        crate::agent_plugin::sync();
        Ok(())
    }
}

/// 保存の口を差し替えられる形の OK。
///
/// **捕捉と記録はここ 1 か所に置く。** ウィンドウプロシージャの内側にも捕捉層が
/// あるが、ここでも捕捉する——層が 1 つだと、その実装が古い版に差し替わった
/// ときに穴が開く。**捕捉したら必ず記録する。** 内側で捕まえる以上、外側の層は
/// 数を進めず、繰り返しに対する復旧も発動しない。**記録が無ければ、OK が
/// 無反応なダイアログが痕跡を残さずに残る。**
///
/// 捕捉を二重にしてあるのは、記録そのものが panic しても境界を越えさせない
/// ためである。
fn guarded_accept<S>(form: &SettingsForm, handle: &DialogHandle, save: S)
where
    S: FnOnce(&SettingsChange) -> anyhow::Result<()>,
{
    let _ = catch_unwind(AssertUnwindSafe(move || {
        match catch_unwind(AssertUnwindSafe(move || accept_outcome(form, save))) {
            Ok(Ok(())) => handle.accept(),
            Ok(Err(message)) => MessageBox::error(handle.hwnd(), &message, DIALOG_TITLE),
            Err(_) => tracing::error!("設定の確定で panic を捕捉しました"),
        }
    }));
}

/// OK の判定。
///
/// **全項目を読んで検証し、範囲外なら画面を閉じない。** 保存に失敗した場合も
/// 閉じない——黙って閉じると、利用者は変更が失われたことを知らないまま去る。
/// 開いたままであれば、もう一度 OK を押すだけで済む。
///
/// 戻り値の `Err` は利用者へ示す文言である。**閉じてよいかだけを返し、画面の
/// 操作を行わない**ため、ウィンドウを作らずに判定を確かめられる。
fn accept_outcome<S>(form: &SettingsForm, save: S) -> Result<(), String>
where
    S: FnOnce(&SettingsChange) -> anyhow::Result<()>,
{
    let change = form
        .collect()
        .map_err(|messages| format!("入力できない値があります。\n\n{}", messages.join("\n")))?;
    // 変更が無ければ書かない。書けば更新時刻が動き、他のプロセスに読み直しを
    // 強いるだけである。
    if change.is_empty() {
        return Ok(());
    }
    save(&change).map_err(|e| {
        tracing::error!("設定を保存できませんでした: {e:#}");
        format!("設定を保存できませんでした。\n{e:#}")
    })
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

    /// 「エージェントプラグイン」ページの群が中身を持つこと。
    ///
    /// **見出しだけの群を作らない。** 画面の高さは最も高いページに揃うため、
    /// 中身の無い見出しはそのまま余白になる。
    #[test]
    fn the_agent_plugin_page_carries_both_of_its_groups() {
        let form = SettingsForm::new(&Settings::default());

        assert_eq!(
            form.agent_plugin().len(),
            form::AgentPluginToggle::ALL.len(),
            "切り替えを載せ忘れています"
        );
        assert!(!destination_line().is_empty(), "生成先の行が空です");
    }

    /// 生成先が 1 行で示されること。
    ///
    /// 利用者は client へ marketplace を登録するときにこの場所を要する。
    #[test]
    fn the_agent_plugin_page_names_where_the_tree_is_generated() {
        let line = destination_line();

        assert_eq!(line.lines().count(), 1, "生成先が 1 行に収まっていません");
        assert!(
            line.contains("AviUtl2Mcp"),
            "生成先が示されていません: {line}"
        );
    }

    /// 保存の呼び出しを記録する差し替え口。
    #[derive(Default)]
    struct RecordingSave {
        saved: std::cell::RefCell<Vec<SettingsChange>>,
    }

    impl RecordingSave {
        fn save(&self) -> impl FnOnce(&SettingsChange) -> anyhow::Result<()> {
            move |change| {
                self.saved.borrow_mut().push(change.clone());
                Ok(())
            }
        }

        fn count(&self) -> usize {
            self.saved.borrow().len()
        }
    }

    /// 同意そのものの切り替え。
    fn consent(form: &SettingsForm) -> &form::AgentPluginInput {
        form.agent_plugin()
            .iter()
            .find(|input| input.toggle().is_consent())
            .expect("同意の切り替えが画面にありません")
    }

    /// 保存した後に生成物へ反映すること。
    ///
    /// **有効にした直後に何も現れないのは、利用者から見て失敗と区別が付かない。**
    /// 起動時と設定画面が同じ関数を通ることを、画面の側から固定する。
    #[test]
    fn accepting_reflects_the_saved_settings_on_the_generated_tree() {
        let _hook = crate::agent_plugin::test_hook::install();
        let form = SettingsForm::new(&Settings::default());
        consent(&form).control().set_checked(true);
        let saver = RecordingSave::default();

        assert_eq!(
            accept_outcome(&form, saving_and_syncing(saver.save())),
            Ok(())
        );

        assert_eq!(saver.count(), 1);
        assert_eq!(
            crate::agent_plugin::test_hook::calls(),
            1,
            "保存しただけで生成物へ反映していません"
        );
    }

    /// 保存に失敗したら反映しないこと。
    ///
    /// 書けなかった設定に合わせて生成物を動かせば、画面が示す状態とディスクの
    /// 状態が食い違う。
    #[test]
    fn a_failed_save_reflects_nothing() {
        let _hook = crate::agent_plugin::test_hook::install();
        let form = SettingsForm::new(&Settings::default());
        consent(&form).control().set_checked(true);

        let saving = saving_and_syncing(|_: &SettingsChange| Err(anyhow::anyhow!("書けません")));
        assert!(accept_outcome(&form, saving).is_err());

        assert_eq!(
            crate::agent_plugin::test_hook::calls(),
            0,
            "保存に失敗したのに生成物へ反映しました"
        );
    }

    /// 何も変えていなければ保存しないこと。
    ///
    /// 書けば更新時刻が動き、他のプロセスに読み直しを強いるだけである。
    #[test]
    fn an_untouched_form_is_accepted_without_saving() {
        let form = SettingsForm::new(&Settings::default());
        let saver = RecordingSave::default();

        assert_eq!(accept_outcome(&form, saver.save()), Ok(()));
        assert_eq!(saver.count(), 0, "変更が無いのに保存しました");
    }

    /// 変更があれば、その変更点だけを保存すること。
    #[test]
    fn a_touched_form_saves_only_the_change() {
        let form = SettingsForm::new(&Settings::default());
        form.numbers_in(BehaviorGroup::Timing)
            .next()
            .unwrap()
            .control()
            .set_value(150);
        let saver = RecordingSave::default();

        assert_eq!(accept_outcome(&form, saver.save()), Ok(()));

        let saved = saver.saved.borrow();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].budget_scale_percent, Some(150));
        assert!(saved[0].tools.is_empty());
    }

    /// 検証を通らない入力は保存へ進まず、閉じてよいとも言わないこと。
    #[test]
    fn invalid_input_neither_saves_nor_accepts() {
        let form = SettingsForm::new(&Settings::default());
        form.numbers_in(BehaviorGroup::Timing)
            .next()
            .unwrap()
            .control()
            .set_text("いち");
        let saver = RecordingSave::default();

        let message = accept_outcome(&form, saver.save()).unwrap_err();

        assert!(message.contains("入力できない値があります"), "{message}");
        assert_eq!(saver.count(), 0, "検証を通らない入力を保存しました");
    }

    /// 保存に失敗したら閉じてよいと言わず、理由を伝えること。
    #[test]
    fn a_failed_save_is_reported_and_does_not_accept() {
        let form = SettingsForm::new(&Settings::default());
        form.numbers_in(BehaviorGroup::Timing)
            .next()
            .unwrap()
            .control()
            .set_value(150);

        let message = accept_outcome(&form, |_| {
            Err(anyhow::anyhow!(
                "設定の名前付き mutex を獲得できませんでした"
            ))
        })
        .unwrap_err();

        assert!(message.contains("設定を保存できませんでした"), "{message}");
        assert!(message.contains("mutex"), "理由が伝わりません: {message}");
    }

    /// 確定の途中で panic しても、境界を越えず、記録が残ること。
    ///
    /// **内側で捕まえる以上、ウィンドウプロシージャの捕捉層は数を進めない。**
    /// 繰り返しに対する復旧も発動しないため、記録が無ければ「OK が無反応な
    /// ダイアログ」が痕跡を残さずに残る。
    #[test]
    fn a_panic_while_confirming_is_caught_and_logged() {
        let form = SettingsForm::new(&Settings::default());
        form.numbers_in(BehaviorGroup::Timing)
            .next()
            .unwrap()
            .control()
            .set_value(150);
        let dialog = Dialog::new(DIALOG_TITLE);
        let handle = dialog.handle();

        let logs = crate::test_support::with_silent_panic_hook(|| {
            crate::test_support::capture_logs(|| {
                guarded_accept(&form, &handle, |_| panic!("保存の途中で panic させます"));
            })
        });

        assert!(
            logs.contains("ERROR") && logs.contains("panic を捕捉しました"),
            "捕捉した panic が記録されていません: {logs}"
        );
    }
}
