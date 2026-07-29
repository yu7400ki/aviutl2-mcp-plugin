//! SDK の編集 API を [`EditHost`] へ写す実装。
//!
//! 編集区間の内側でだけ opaque handle を扱い、区間を抜ける前に所有型へ写す。
//! ハンドルは戻り値・ログ・エラーのいずれにも現れない。ハンドル型には
//! スレッド間で送れる旨の実装が付いており、型検査では区間の外への持ち出しを
//! 止められないため、境界の内側で閉じるのは本モジュールの責務である。
//!
//! ハンドルの `Debug` は生ポインタを出力するため、書式化に用いない。

use crate::EDIT_HANDLE;
use crate::edit::error::EditError;
use crate::edit::host::{EditHost, EffectSlot, HostSelection, ObjectSlot, SceneEditor};
use crate::edit::precondition::MutationTicket;
use crate::edit::resolve::{ResolvedEffect, ResolvedObject};
use crate::read::ReadError;
use crate::read::host::{EditState, HostEditInfo, HostObject, ReadHost, SceneReader};
use crate::read::sdk::{SdkReadHost, SdkSceneReader, non_negative};
use aviutl2::generic::{
    EditSection, EffectHandle, MediaFileSupportMode, ObjectHandle, ReadSection,
};
use aviutl2_mcp_core::{AvailableEffect, AvailableEffectItem, Cursor, EffectItemType, FrameRange};
use std::cell::RefCell;

/// SDK 呼び出しの失敗を、失敗した関数名つきの型付きエラーにする。
fn sdk(operation: &'static str) -> EditError {
    EditError::Sdk { operation }
}

/// メディア対応の確認方法。
///
/// 拡張子だけの判定では、対応しないファイルの作成が SDK 側の失敗になる。作成は
/// 取り消し単位を伴うため、呼ぶ前に確定させる方が失敗の理由を正しく伝えられる。
const MEDIA_SUPPORT_MODE: MediaFileSupportMode = MediaFileSupportMode::Strict;

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
        })
    }

    fn enter_edit_section<T, F>(&self, f: F) -> Result<T, EditError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn SceneEditor) -> T + Send,
    {
        // クロージャを保持する領域は呼び出しごとに解放されないため、
        // 捕らえるのは `f` だけに留める。
        EDIT_HANDLE
            .call_edit_section(move |section| {
                let editor = SdkSceneEditor::new(section);
                f(&editor)
            })
            .map_err(|_| sdk("call_edit_section"))
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
        let detail =
            self.object_detail(non_negative(position.layer), non_negative(position.start))?;
        Ok(Some(detail.object))
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
    fn new(section: &'a EditSection) -> Self {
        let read_section: &ReadSection = section;
        Self {
            section,
            reader: SdkSceneReader {
                section: read_section,
            },
            info: entry_edit_info(section),
            objects: RefCell::new(Vec::new()),
            effects: RefCell::new(Vec::new()),
        }
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
fn entry_edit_info(section: &EditSection) -> HostEditInfo {
    let info = &section.info;
    let size = |value: usize| u32::try_from(value).unwrap_or(0);
    HostEditInfo {
        scene_id: info.scene_id,
        width: size(info.width),
        height: size(info.height),
        fps_rate: *info.fps.numer(),
        fps_scale: *info.fps.denom(),
        sample_rate: size(info.sample_rate),
        cursor_frame: info.frame,
        cursor_layer: info.layer,
        frame_max: info.frame_max,
        layer_max: info.layer_max,
        display_frame_start: info.display_frame_start,
        display_layer_start: info.display_layer_start,
        display_frame_num: info.display_frame_num,
        display_layer_num: info.display_layer_num,
        select_range_start: info.select_range_start,
        select_range_end: info.select_range_end,
    }
}

impl SceneEditor for SdkSceneEditor<'_> {
    fn reader(&self) -> &dyn SceneReader {
        &self.reader
    }

    fn entry_edit_info(&self) -> &HostEditInfo {
        &self.info
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
            .map_err(|_| sdk("create_object_from_alias"))
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
            .map_err(|_| sdk("create_object_from_media_file"))
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
            .map_err(|_| sdk("move_object"))
    }

    fn delete_object(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
    ) -> Result<(), EditError> {
        self.section
            .delete_object(self.object(object.slot())?)
            .map_err(|_| sdk("delete_object"))
    }

    fn set_object_name(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        name: Option<&str>,
    ) -> Result<(), EditError> {
        self.section
            .set_object_name(self.object(object.slot())?, name)
            .map_err(|_| sdk("set_object_name"))
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
            .map_err(|_| sdk("create_effect"))
    }

    fn delete_effect(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        effect: &ResolvedEffect<'_>,
    ) -> Result<(), EditError> {
        self.section
            .delete_effect(self.object(object.slot())?, self.effect(effect.slot())?)
            .map_err(|_| sdk("delete_effect"))
    }

    fn set_effect_enabled(
        &self,
        _ticket: MutationTicket<'_>,
        effect: &ResolvedEffect<'_>,
        enabled: bool,
    ) -> Result<(), EditError> {
        self.section
            .set_effect_enable(self.effect(effect.slot())?, enabled)
            .map_err(|_| sdk("set_effect_enable"))
    }

    fn set_effect_locked(
        &self,
        _ticket: MutationTicket<'_>,
        effect: &ResolvedEffect<'_>,
        locked: bool,
    ) -> Result<(), EditError> {
        self.section
            .set_effect_lock(self.effect(effect.slot())?, locked)
            .map_err(|_| sdk("set_effect_lock"))
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
            .map_err(|_| sdk("set_effect_item_value"))
    }

    fn set_cursor(
        &self,
        _ticket: MutationTicket<'_>,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.section
            .set_cursor_layer_frame(layer, frame)
            .map_err(|_| sdk("set_cursor_layer_frame"))
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
        .map_err(|_| sdk("set_select_range"))
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
            .map_err(|_| sdk("set_focus_object"))
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
}
