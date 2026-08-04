//! SDK の編集 API を [`EditHost`] へ写す実装。
//!
//! 編集区間の内側でだけ opaque handle を扱い、区間を抜ける前に所有型へ写す。
//! ハンドルは戻り値・ログ・エラーのいずれにも現れない。ハンドル型には
//! スレッド間で送れる旨の実装が付いており、型検査では区間の外への持ち出しを
//! 止められないため、境界の内側で閉じるのは本モジュールの責務である。
//!
//! ハンドルの `Debug` は生ポインタを出力するため、書式化に用いない。

use crate::EDIT_HANDLE;
use crate::edit::error::{EditError, NotIssuedReason};
use crate::edit::host::{
    EditHost, EffectSlot, HostSelection, ObjectPosition, ObjectSlot, SceneEditor,
};
use crate::edit::precondition::MutationTicket;
use crate::edit::resolve::{ResolvedEffect, ResolvedObject};
use crate::read::ReadError;
use crate::read::host::{EditState, HostEditInfo, HostObject, ReadHost, SceneReader};
use crate::read::sdk::{
    SdkReadHost, SdkSceneReader, host_edit_info, non_negative, to_inclusive_sections,
};
use aviutl2::generic::{
    EditSection, EditSectionError, EffectHandle, MediaFileSupportMode, ObjectHandle, ReadSection,
};
use aviutl2_mcp_core::{
    AvailableEffect, AvailableEffectItem, Cursor, DisplayRange, EffectItemType, FrameRange,
    SectionRange,
};
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// SDK 呼び出しの失敗を、失敗した関数名つきの型付きエラーにする。
fn sdk(operation: &'static str) -> EditError {
    EditError::Sdk { operation }
}

/// 変更 API の失敗を、SDK へ届いたかどうかで分けて写す。
///
/// ラッパーは対象の存在確認・整数変換・NUL 検査を呼び出しの入口で行い、これらに
/// 引っ掛かった要求は SDK を呼ばずに専用の理由で戻る。SDK が実際に失敗を返した
/// 場合と区別できるため、区別したまま伝える。届いていない失敗を SDK の失敗と
/// して扱うと、プロジェクトが一切変わっていないのに変更を発行したことになる。
/// 網羅 match で書く。`_` で受けると、上流が失敗の種類を足したり割り直したり
/// したときに黙って SDK の失敗として扱われ、届いていない要求が発行として
/// 記録されるところまで戻ってしまう。
fn mutation_failure(operation: &'static str, error: &EditSectionError) -> EditError {
    let not_issued = |reason| EditError::NotIssued { reason };
    match error {
        // 対象の存在確認は呼び出しの先頭にあり、SDK へは届かない。
        EditSectionError::ObjectDoesNotExist | EditSectionError::EffectDoesNotExist => {
            not_issued(NotIssuedReason::TargetMissing)
        }
        // 引数を SDK の型へ写す変換は関数ポインタを呼ぶ前に評価される。
        EditSectionError::ValueOutOfRange(_)
        | EditSectionError::InputCstrContainsNull(_)
        | EditSectionError::InputCwstrContainsNull(_) => {
            not_issued(NotIssuedReason::ArgumentNotRepresentable)
        }
        // SDK が実際に失敗を返した、あるいは返した値を解釈できなかった。
        EditSectionError::ApiCallFailed
        | EditSectionError::NonUtf8Data(_)
        | EditSectionError::ParseFailed(_) => sdk(operation),
    }
}

/// 中間点の変更の失敗を、SDK が拒んだ場合とそれ以外に分けて写す。
///
/// 中間点の 3 つは戻り値が `bool` であり、ラッパーは `false` を
/// [`EditSectionError::ApiCallFailed`] として返す。事前確認を通ったうえでの
/// `false` は要求元に直せる誤りではないため、他の SDK 失敗と混ぜず専用の名前で
/// 返す。届かなかった失敗の分類は [`mutation_failure`] のものをそのまま使う。
fn section_mutation_failure(operation: &'static str, error: &EditSectionError) -> EditError {
    match mutation_failure(operation, error) {
        EditError::Sdk { operation } => EditError::SectionChangeRejected { operation },
        other => other,
    }
}

/// メディア対応の確認方法。
///
/// 拡張子だけを見る。厳密な確認は実際にファイルを開いて読めるかを調べるため、
/// 編集区間の内側では使えない。区間はホストのメインスレッド上で走り、割り込む
/// 手段が無いので、ネットワーク越しのパスや巨大なファイルを渡されると解析が
/// 終わるまで操作が止まる。
///
/// 拡張子は通るが実際には読めないファイルは、作成そのものが失敗して SDK の
/// 失敗として返る。作成の失敗は理由を伝えないため、そこから対応していない
/// ファイルだと名乗ることはしない。
const MEDIA_SUPPORT_MODE: MediaFileSupportMode = MediaFileSupportMode::ExtensionOnly;

/// グローバルな編集ハンドルを介して SDK を呼ぶホスト。
pub struct SdkEditHost;

impl EditHost for SdkEditHost {
    fn is_ready(&self) -> bool {
        // 未初期化でも偽を返す。参照解決を伴わないため呼び出しても落ちない。
        EDIT_HANDLE.is_ready()
    }

    fn edit_state(&self) -> Result<EditState, EditError> {
        Ok(SdkReadHost.edit_state()?)
    }

    fn effect_catalog(&self) -> Result<Vec<AvailableEffect>, EditError> {
        Ok(SdkReadHost.effect_catalog()?)
    }

    fn observed_selection(&self) -> Result<HostSelection, EditError> {
        let info = SdkReadHost.edit_info()?;
        // フォーカス対象は参照区間の内側でしか読めない。編集区間を抜けた後で
        // なければ反映されないため、ここで改めて区間へ入る。
        let focus = EDIT_HANDLE
            .call_read_section(|section| {
                let reader = SdkSceneReader { section };
                reader.focused_object()
            })
            .map_err(|_| sdk("call_read_section"))??;
        Ok(HostSelection {
            scene_id: info.scene_id,
            cursor: Cursor {
                frame: info.cursor_frame,
                layer: info.cursor_layer,
            },
            selected_range: match (info.select_range_start, info.select_range_end) {
                (Some(start), Some(end)) => Some(FrameRange { start, end }),
                _ => None,
            },
            focus,
            display: DisplayRange {
                frame_start: info.display_frame_start,
                layer_start: info.display_layer_start,
                frame_num: info.display_frame_num,
                layer_num: info.display_layer_num,
            },
        })
    }

    fn enter_edit_section<T, F>(&self, f: F) -> Result<T, EditError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn SceneEditor) -> T + Send,
    {
        // 編集区間のコールバックは C の関数ポインタから呼ばれる。ここから
        // 巻き戻しが漏れるとホストのプロセスごと落ちるため、区間へ渡すものは
        // 全体を捕捉層で包む。呼び出し側のクロージャだけを包むと、区間の入口で
        // 行う編集情報の複製が保護から外れる。
        //
        // クロージャを保持する領域は呼び出しごとに解放されないため、捕らえるのは
        // `f` だけに留める。捕らえた巻き戻しの内容もここで捨て、区間の外へ
        // 持ち出さない。
        let outcome = EDIT_HANDLE
            .call_edit_section(move |section| {
                catch_unwind(AssertUnwindSafe(|| {
                    SdkSceneEditor::new(section).map(|editor| f(&editor))
                }))
                .map_err(|_| ())
            })
            .map_err(|_| sdk("call_edit_section"))?;
        outcome.map_err(|()| EditError::Panicked)?
    }
}

impl SdkSceneReader<'_> {
    /// フォーカスされているオブジェクトを所有型へ写す。
    fn focused_object(&self) -> Result<Option<HostObject>, EditError> {
        let Some(handle) = self
            .section
            .get_focused_object()
            .map_err(|_| sdk("get_focus_object"))?
        else {
            return Ok(None);
        };
        let position = self
            .section
            .get_object_layer_frame(handle)
            .map_err(|_| sdk("get_object_layer_frame"))?;
        // 応答へ載せるのは概要だけであり、配下 effect は読まない。
        let object =
            self.object_identity(non_negative(position.layer), non_negative(position.start))?;
        Ok(Some(object))
    }
}

/// 編集区間の内側で SDK を呼ぶ編集口。
///
/// 解決したハンドルは内部の表へ積み、外へは添字だけを渡す。表は区間の生存期間で
/// 破棄されるため、ハンドルが区間を越えることがない。
struct SdkSceneEditor<'a> {
    section: &'a EditSection,
    reader: SdkSceneReader<'a>,
    info: HostEditInfo,
    objects: RefCell<Vec<ObjectHandle>>,
    effects: RefCell<Vec<EffectHandle>>,
}

impl<'a> SdkSceneEditor<'a> {
    /// 編集区間から編集口を作る。
    fn new(section: &'a EditSection) -> Result<Self, EditError> {
        let read_section: &ReadSection = section;
        Ok(Self {
            section,
            reader: SdkSceneReader {
                section: read_section,
            },
            info: entry_edit_info(section)?,
            objects: RefCell::new(Vec::new()),
            effects: RefCell::new(Vec::new()),
        })
    }

    /// 添字からオブジェクトのハンドルを引く。
    fn object(&self, slot: ObjectSlot) -> Result<ObjectHandle, EditError> {
        self.objects
            .borrow()
            .get(slot.0)
            .copied()
            .ok_or(sdk("get_object_layer_frame"))
    }

    /// 添字から effect のハンドルを引く。
    fn effect(&self, slot: EffectSlot) -> Result<EffectHandle, EditError> {
        self.effects
            .borrow()
            .get(slot.0)
            .copied()
            .ok_or(sdk("get_effect_list"))
    }
}

/// 区間へ入った時点の編集情報を所有型へ写す。
///
/// 区間内で変更を適用した後は古くなるが、シーンの guard は入口の値と照合する
/// ため、この複製で足りる。
///
/// 写し方は読み取り経路と共有する。同じホストの同じ値を層ごとに別の規約で
/// 写すと、シーン以外のフィールドを使い始めた時点で読み取りと編集が別の値を
/// 見ることになる。
fn entry_edit_info(section: &EditSection) -> Result<HostEditInfo, EditError> {
    Ok(host_edit_info(&section.info)?)
}

impl SceneEditor for SdkSceneEditor<'_> {
    fn reader(&self) -> &dyn SceneReader {
        &self.reader
    }

    fn entry_edit_info(&self) -> &HostEditInfo {
        &self.info
    }

    fn occupied_layer_max(&self) -> Result<usize, EditError> {
        // 区間が持つ編集情報は入口の複製であり、区間内の作成を反映しない。
        // 編集ハンドル側の取得は参照ロックが同一スレッドで再入可能であり、
        // 区間の内側からも現在の値を読める。
        Ok(SdkReadHost.edit_info()?.layer_max)
    }

    fn bind_object(&self, layer: usize, frame_start: usize) -> Result<ObjectSlot, EditError> {
        let handle = self
            .reader
            .section
            .find_object_after(layer, frame_start)
            .map_err(|_| sdk("find_object"))?
            .ok_or(ReadError::ObjectNotFound {
                detected_by: "find_object",
            })?;
        // 探索は「指定フレーム以降」であるため、開始フレームの完全一致を
        // 確かめてからでないと後続の対象を掴む。
        let position = self
            .reader
            .section
            .get_object_layer_frame(handle)
            .map_err(|_| sdk("get_object_layer_frame"))?;
        if non_negative(position.start) != frame_start {
            return Err(ReadError::ObjectNotFound {
                detected_by: "find_object",
            }
            .into());
        }
        let mut objects = self.objects.borrow_mut();
        objects.push(handle);
        Ok(ObjectSlot(objects.len() - 1))
    }

    fn bind_effect(&self, object: ObjectSlot, position: usize) -> Result<EffectSlot, EditError> {
        let object = self.object(object)?;
        let handle = *self
            .reader
            .section
            .get_effects(object)
            .map_err(|_| sdk("get_effect_list"))?
            .get(position)
            .ok_or(sdk("get_effect_list"))?;
        let mut effects = self.effects.borrow_mut();
        effects.push(handle);
        Ok(EffectSlot(effects.len() - 1))
    }

    fn effect_items(
        &self,
        effect: &ResolvedEffect<'_>,
    ) -> Result<Vec<AvailableEffectItem>, EditError> {
        let definitions = EDIT_HANDLE
            .get_effect_items(&effect.info().name)
            .map_err(|_| sdk("enum_effect_item"))?;
        Ok(definitions
            .into_iter()
            .map(|item| AvailableEffectItem {
                name: item.name,
                item_type: EffectItemType::from_raw(i32::from(item.item_type)),
            })
            .collect())
    }

    fn effect_item_value(
        &self,
        effect: &ResolvedEffect<'_>,
        item: &str,
    ) -> Result<String, EditError> {
        self.reader
            .section
            .get_effect_item_value(self.effect(effect.slot())?, item)
            .map_err(|_| sdk("get_effect_item_value"))
    }

    fn supports_media_file(&self, path: &str) -> Result<bool, EditError> {
        self.reader
            .section
            .is_support_media_file(path, MEDIA_SUPPORT_MODE)
            .map_err(|_| sdk("is_support_media_file"))
    }

    fn create_object_from_alias(
        &self,
        _ticket: MutationTicket<'_>,
        alias: &str,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        // 長さの指定は受け付けない。0 を渡すとホストが長さと挿入位置を決める。
        self.section
            .create_object_from_alias(alias, layer, frame, 0)
            .map(|_| ())
            .map_err(|error| mutation_failure("create_object_from_alias", &error))
    }

    fn create_object_from_media_file(
        &self,
        _ticket: MutationTicket<'_>,
        path: &str,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.section
            .create_object_from_media_file(path, layer, frame, None)
            .map(|_| ())
            .map_err(|error| mutation_failure("create_object_from_media_file", &error))
    }

    fn object_position(&self, object: &ResolvedObject<'_>) -> Result<ObjectPosition, EditError> {
        let position = self
            .reader
            .section
            .get_object_layer_frame(self.object(object.slot())?)
            .map_err(|_| sdk("get_object_layer_frame"))?;
        Ok(ObjectPosition {
            layer: non_negative(position.layer),
            frame_start: non_negative(position.start),
        })
    }

    fn move_object(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.section
            .move_object(self.object(object.slot())?, layer, frame)
            .map_err(|error| mutation_failure("move_object", &error))
    }

    fn delete_object(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
    ) -> Result<(), EditError> {
        self.section
            .delete_object(self.object(object.slot())?)
            .map_err(|error| mutation_failure("delete_object", &error))
    }

    fn object_sections(&self, object: &ResolvedObject<'_>) -> Result<Vec<SectionRange>, EditError> {
        let ranges = self
            .reader
            .section
            .get_object_section_ranges(self.object(object.slot())?)
            .map_err(|_| sdk("get_object_section_frame"))?;
        Ok(to_inclusive_sections(ranges))
    }

    fn create_object_section(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        frame: usize,
    ) -> Result<(), EditError> {
        self.section
            .create_object_section(self.object(object.slot())?, frame)
            .map_err(|error| section_mutation_failure("create_object_section", &error))
    }

    fn delete_object_section(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        section: usize,
    ) -> Result<(), EditError> {
        self.section
            .delete_object_section(self.object(object.slot())?, section)
            .map_err(|error| section_mutation_failure("delete_object_section", &error))
    }

    fn move_object_section(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        section: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.section
            .move_object_section(self.object(object.slot())?, section, frame)
            .map_err(|error| section_mutation_failure("move_object_section", &error))
    }

    fn set_object_name(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        name: Option<&str>,
    ) -> Result<(), EditError> {
        self.section
            .set_object_name(self.object(object.slot())?, name)
            .map_err(|error| mutation_failure("set_object_name", &error))
    }

    fn create_effect(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        effect_name: &str,
    ) -> Result<(), EditError> {
        self.section
            .create_effect(self.object(object.slot())?, effect_name)
            .map(|_| ())
            .map_err(|error| mutation_failure("create_effect", &error))
    }

    fn delete_effect(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        effect: &ResolvedEffect<'_>,
    ) -> Result<(), EditError> {
        self.section
            .delete_effect(self.object(object.slot())?, self.effect(effect.slot())?)
            .map_err(|error| mutation_failure("delete_effect", &error))
    }

    fn set_effect_enabled(
        &self,
        _ticket: MutationTicket<'_>,
        effect: &ResolvedEffect<'_>,
        enabled: bool,
    ) -> Result<(), EditError> {
        self.section
            .set_effect_enable(self.effect(effect.slot())?, enabled)
            .map_err(|error| mutation_failure("set_effect_enable", &error))
    }

    fn set_effect_item(
        &self,
        _ticket: MutationTicket<'_>,
        effect: &ResolvedEffect<'_>,
        item: &str,
        value: &str,
    ) -> Result<(), EditError> {
        // ハンドル指定を用いる。名前と順序を文字列で結合する経路は、順序 0 の
        // 表記が解決するかを確定できない。
        self.section
            .set_effect_item_value(self.effect(effect.slot())?, item, value)
            .map_err(|error| mutation_failure("set_effect_item_value", &error))
    }

    fn set_layer_name(
        &self,
        _ticket: MutationTicket<'_>,
        layer: usize,
        name: Option<&str>,
    ) -> Result<(), EditError> {
        self.section
            .set_layer_name(layer, name)
            .map_err(|error| mutation_failure("set_layer_name", &error))
    }

    fn set_layer_enabled(
        &self,
        _ticket: MutationTicket<'_>,
        layer: usize,
        enabled: bool,
    ) -> Result<(), EditError> {
        self.section
            .set_layer_enable(layer, enabled)
            .map_err(|error| mutation_failure("set_layer_enable", &error))
    }

    fn set_layer_locked(
        &self,
        _ticket: MutationTicket<'_>,
        layer: usize,
        locked: bool,
    ) -> Result<(), EditError> {
        self.section
            .set_layer_lock(layer, locked)
            .map_err(|error| mutation_failure("set_layer_lock", &error))
    }

    fn set_cursor(
        &self,
        _ticket: MutationTicket<'_>,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.section
            .set_cursor_layer_frame(layer, frame)
            .map_err(|error| mutation_failure("set_cursor_layer_frame", &error))
    }

    fn set_display_start(
        &self,
        _ticket: MutationTicket<'_>,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.section
            .set_display_layer_frame(layer, frame)
            .map_err(|error| mutation_failure("set_display_layer_frame", &error))
    }

    fn set_select_range(
        &self,
        _ticket: MutationTicket<'_>,
        range: Option<FrameRange>,
    ) -> Result<(), EditError> {
        match range {
            Some(range) => self.section.set_select_range(range.start, range.end),
            None => self.section.clear_select_range(),
        }
        .map_err(|error| mutation_failure("set_select_range", &error))
    }

    fn set_focus_object(
        &self,
        _ticket: MutationTicket<'_>,
        object: Option<&ResolvedObject<'_>>,
    ) -> Result<(), EditError> {
        let handle = object
            .map(|object| self.object(object.slot()))
            .transpose()?;
        self.section
            .set_focus_object(handle)
            .map_err(|error| mutation_failure("set_focus_object", &error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninitialized_edit_handle_is_not_ready() {
        // テストでは編集ハンドルが初期化されないため、準備前として扱われる。
        // 編集区間へ入る経路は準備前の呼び出しで落ちるため、ここを通らない。
        assert!(!SdkEditHost.is_ready());
    }

    #[test]
    fn media_support_is_checked_without_opening_the_file() {
        // 厳密な確認は実際にファイルを開いて読めるかを調べる。編集区間は
        // ホストのメインスレッド上で走り割り込めないため、区間の内側で行うと
        // 解析が終わるまで操作が止まる。
        assert!(matches!(
            MEDIA_SUPPORT_MODE,
            MediaFileSupportMode::ExtensionOnly
        ));
    }

    #[test]
    fn a_failure_before_the_sdk_call_is_told_apart_from_an_sdk_failure() {
        // 届いていない失敗を SDK の失敗として扱うと、プロジェクトが一切
        // 変わっていないのに変更を発行したことになる。
        //
        // 対象の存在確認も、引数を SDK の型へ写す変換も、いずれも FFI の
        // 呼び出しより前にある。到達しないまま戻る理由をすべて挙げる。
        let out_of_range = u8::try_from(300u32).expect_err("範囲外の変換");
        let utf8_nul = std::ffi::CString::new("a\0b").expect_err("NUL を含む文字列");
        let utf16_nul = aviutl2::config::translate_strict("a\0b")
            .expect_err("NUL を含む文字列が UTF-16 へ写りました");
        let not_issued: Vec<(EditSectionError, NotIssuedReason)> = vec![
            (
                EditSectionError::ObjectDoesNotExist,
                NotIssuedReason::TargetMissing,
            ),
            (
                EditSectionError::EffectDoesNotExist,
                NotIssuedReason::TargetMissing,
            ),
            (
                EditSectionError::ValueOutOfRange(out_of_range),
                NotIssuedReason::ArgumentNotRepresentable,
            ),
            (
                EditSectionError::InputCstrContainsNull(utf8_nul),
                NotIssuedReason::ArgumentNotRepresentable,
            ),
            (
                EditSectionError::InputCwstrContainsNull(utf16_nul),
                NotIssuedReason::ArgumentNotRepresentable,
            ),
        ];
        for (error, expected) in not_issued {
            let mapped = mutation_failure("move_object", &error);
            assert!(
                matches!(mapped, EditError::NotIssued { reason } if reason == expected),
                "{error} が {} として扱われませんでした",
                expected.as_str()
            );
        }

        assert!(
            matches!(
                mutation_failure("move_object", &EditSectionError::ApiCallFailed),
                EditError::Sdk {
                    operation: "move_object"
                }
            ),
            "SDK の失敗が届かなかった扱いになりました"
        );
    }

    #[test]
    fn a_section_change_that_the_sdk_refused_is_told_apart_from_a_failure_before_the_call() {
        // 中間点の 3 つは `bool` を返し、ラッパーは `false` を ApiCallFailed と
        // して返す。事前確認を通ったうえでの `false` は要求元に直せる誤りでは
        // ないため、他の SDK 失敗と混ぜず専用の名前で返す。
        assert!(
            matches!(
                section_mutation_failure("create_object_section", &EditSectionError::ApiCallFailed),
                EditError::SectionChangeRejected {
                    operation: "create_object_section"
                }
            ),
            "false が拒否として扱われませんでした"
        );

        // 届いていない失敗の分類は変わらない。プロジェクトは一切変わって
        // いないため、拒否として扱うと発行したことになる。
        assert!(
            matches!(
                section_mutation_failure(
                    "delete_object_section",
                    &EditSectionError::ObjectDoesNotExist
                ),
                EditError::NotIssued {
                    reason: NotIssuedReason::TargetMissing
                }
            ),
            "届かなかった失敗が拒否として扱われました"
        );

        let out_of_range = u8::try_from(300u32).expect_err("範囲外の変換");
        assert!(
            matches!(
                section_mutation_failure(
                    "move_object_section",
                    &EditSectionError::ValueOutOfRange(out_of_range)
                ),
                EditError::NotIssued {
                    reason: NotIssuedReason::ArgumentNotRepresentable
                }
            ),
            "引数を写せない失敗が拒否として扱われました"
        );
    }
}
