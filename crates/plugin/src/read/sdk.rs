//! SDK の読み取り API を [`ReadHost`] へ写す実装。
//!
//! 参照区間の内側でだけ opaque handle を扱い、区間を抜ける前に所有型へ写す。
//! ハンドルは戻り値・ログ・エラーのいずれにも現れない。

use crate::EDIT_HANDLE;
use crate::read::error::ReadError;
use crate::read::host::{
    EditState, HostEditInfo, HostEffect, HostLayer, HostObject, HostObjectDetail, ReadHost,
    SceneReader,
};
use aviutl2::generic::{EffectHandle, ObjectHandle, ReadSection};
use aviutl2_mcp_core::{
    AvailableEffect, AvailableEffectItem, EffectFlags, EffectItem, EffectItemType, EffectType,
    FiniteF64, ItemValue, SectionRange,
};
use std::collections::HashMap;

/// SDK 呼び出しの失敗を、失敗した関数名つきの型付きエラーにする。
fn sdk(operation: &'static str) -> ReadError {
    ReadError::Sdk { operation }
}

/// グローバルな編集ハンドルを介して SDK を呼ぶホスト。
pub struct SdkReadHost;

impl ReadHost for SdkReadHost {
    fn is_ready(&self) -> bool {
        // 未初期化でも偽を返す。参照解決を伴わないため呼び出しても落ちない。
        EDIT_HANDLE.is_ready()
    }

    fn edit_state(&self) -> Result<EditState, ReadError> {
        match EDIT_HANDLE.get_edit_state() {
            Ok(aviutl2::generic::EditState::Edit) => Ok(EditState::Edit),
            Ok(aviutl2::generic::EditState::Preview) => Ok(EditState::Preview),
            Ok(aviutl2::generic::EditState::Save) => Ok(EditState::Save),
            Err(_) => Err(sdk("get_edit_state")),
        }
    }

    fn edit_info(&self) -> Result<HostEditInfo, ReadError> {
        // 参照区間の外で取得する。区間の内側では公開されていない。
        let info = EDIT_HANDLE.get_edit_info();
        let size = |value: usize| u32::try_from(value).map_err(|_| sdk("get_edit_info"));
        Ok(HostEditInfo {
            scene_id: info.scene_id,
            width: size(info.width)?,
            height: size(info.height)?,
            // 有理数へ畳まれた後の分子・分母であり、ホストが保持する生の
            // rate/scale は約分によって失われている。
            fps_rate: *info.fps.numer(),
            fps_scale: *info.fps.denom(),
            sample_rate: size(info.sample_rate)?,
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
        })
    }

    fn effect_catalog(&self) -> Result<Vec<AvailableEffect>, ReadError> {
        let effects = EDIT_HANDLE.get_effects();
        let mut catalog = Vec::with_capacity(effects.len());
        for effect in effects {
            let items = EDIT_HANDLE
                .get_effect_items(&effect.name)
                .map_err(|_| sdk("enum_effect_item"))?;
            catalog.push(AvailableEffect {
                name: effect.name,
                effect_type: EffectType::from_raw(i32::from(effect.effect_type)),
                // 既知ビットのみを組み直した値であり、未知ビットは保持できない。
                flags: EffectFlags::from_raw(effect.flag.to_bits() as u32),
                items: items
                    .into_iter()
                    .map(|item| AvailableEffectItem {
                        name: item.name,
                        item_type: EffectItemType::from_raw(i32::from(item.item_type)),
                    })
                    .collect(),
            });
        }
        Ok(catalog)
    }

    fn enter_read_section<T, F>(&self, f: F) -> Result<T, ReadError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn SceneReader) -> T + Send,
    {
        // クロージャを保持する領域は呼び出しごとに解放されないため、
        // 捕らえるのは `f` だけに留める。
        EDIT_HANDLE
            .call_read_section(move |section| f(&SdkSceneReader { section }))
            .map_err(|_| sdk("call_read_section"))
    }
}

/// 参照区間の内側で SDK を呼ぶ読み取り口。
struct SdkSceneReader<'a> {
    section: &'a ReadSection,
}

impl SceneReader for SdkSceneReader<'_> {
    fn scene_name(&self) -> Option<String> {
        // シーン名は編集情報に含まれず、取得できないこともある。
        self.section.get_scene_name().ok()
    }

    fn grid_bpm(&self) -> Result<Vec<FiniteF64>, ReadError> {
        let list = self
            .section
            .get_grid_bpm_list()
            .map_err(|_| sdk("get_grid_bpm_list"))?;
        Ok(list
            .into_iter()
            .filter_map(|bpm| FiniteF64::try_new(f64::from(bpm.tempo)))
            .collect())
    }

    fn layer(&self, layer: usize) -> Result<HostLayer, ReadError> {
        Ok(HostLayer {
            name: self
                .section
                .get_layer_name(layer)
                .map_err(|_| sdk("get_layer_name"))?,
            enabled: self
                .section
                .get_layer_enable(layer)
                .map_err(|_| sdk("get_layer_enable"))?,
            locked: self
                .section
                .get_layer_lock(layer)
                .map_err(|_| sdk("get_layer_lock"))?,
        })
    }

    fn objects_in_layer(&self, layer: usize) -> Result<Vec<HostObject>, ReadError> {
        // ラッパーのイテレータは失敗と終端を区別せず打ち切るため、自前で走査して
        // 途中の失敗を取りこぼさないようにする。
        let mut objects = Vec::new();
        let mut next_frame = 0usize;
        loop {
            let Some(handle) = self
                .section
                .find_object_after(layer, next_frame)
                .map_err(|_| sdk("find_object"))?
            else {
                return Ok(objects);
            };
            let object = self.object_at(handle)?;
            let advanced = object.frame_end.saturating_add(1);
            if !objects.is_empty() && advanced <= next_frame {
                // 探索位置が前進しない場合は同じ対象を返し続けるため打ち切る。
                return Ok(objects);
            }
            next_frame = advanced;
            objects.push(object);
        }
    }

    fn object_detail(
        &self,
        layer: usize,
        frame_start: usize,
    ) -> Result<HostObjectDetail, ReadError> {
        let handle = self
            .section
            .find_object_after(layer, frame_start)
            .map_err(|_| sdk("find_object"))?
            .ok_or(ReadError::ObjectNotFound)?;
        let object = self.object_at(handle)?;
        // 「指定フレーム以降」の探索であるため、開始フレームの一致を確かめる。
        if object.frame_start != frame_start {
            return Err(ReadError::ObjectNotFound);
        }

        let sections = self
            .section
            .get_object_section_ranges(handle)
            .map_err(|_| sdk("get_object_section_frame"))?
            .into_iter()
            .map(|range| SectionRange {
                start: range.start,
                end: range.end.saturating_sub(1),
            })
            .collect();

        Ok(HostObjectDetail {
            object,
            sections,
            effects: self.effects_of(handle)?,
        })
    }
}

impl SdkSceneReader<'_> {
    /// ハンドルが指すオブジェクトを所有型へ写す。
    fn object_at(&self, handle: ObjectHandle) -> Result<HostObject, ReadError> {
        let position = self
            .section
            .get_object_layer_frame(handle)
            .map_err(|_| sdk("get_object_layer_frame"))?;
        // 文字列は次の呼び出しまでしか有効でないため、1 つずつ所有型へ写す。
        let name = self
            .section
            .get_object_name(handle)
            .map_err(|_| sdk("get_object_name"))?;
        let alias = self
            .section
            .get_object_alias(handle)
            .map_err(|_| sdk("get_object_alias"))?;
        Ok(HostObject {
            layer: position.layer,
            frame_start: position.start,
            frame_end: position.end,
            name,
            alias,
        })
    }

    /// オブジェクトに付与された effect を所有型へ写す。
    ///
    /// effect の列挙は 0 件でも失敗を返し、付与が無い状態と区別できない。
    /// 失敗を読み取り全体の失敗にすると effect の無いオブジェクトを一切
    /// 読めなくなるため、空の列として扱う。
    fn effects_of(&self, object: ObjectHandle) -> Result<Vec<HostEffect>, ReadError> {
        let Ok(handles) = self.section.get_effects(object) else {
            tracing::debug!("effect の列挙結果が空か取得に失敗しました");
            return Ok(Vec::new());
        };

        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut effects = Vec::with_capacity(handles.len());
        for handle in handles {
            let name = self
                .section
                .get_effect_name(handle)
                .map_err(|_| sdk("get_effect_name"))?;
            let index = seen.entry(name.clone()).or_insert(0);
            let effect_index = *index;
            *index += 1;

            effects.push(HostEffect {
                enabled: self
                    .section
                    .get_effect_enable(handle)
                    .map_err(|_| sdk("get_effect_enable"))?,
                locked: self
                    .section
                    .get_effect_lock(handle)
                    .map_err(|_| sdk("get_effect_lock"))?,
                items: self.effect_items(handle, &name)?,
                name,
                index: effect_index,
            });
        }
        Ok(effects)
    }

    /// effect の設定項目と現在値を所有型へ写す。
    ///
    /// 設定項目の定義は編集ハンドルの列挙から、値は参照区間から得る。
    fn effect_items(
        &self,
        effect: EffectHandle,
        effect_name: &str,
    ) -> Result<Vec<EffectItem>, ReadError> {
        let definitions = EDIT_HANDLE
            .get_effect_items(effect_name)
            .map_err(|_| sdk("enum_effect_item"))?;

        let mut items = Vec::with_capacity(definitions.len());
        for definition in definitions {
            let item_type = EffectItemType::from_raw(i32::from(definition.item_type));
            let raw = self
                .section
                .get_effect_item_value(effect, &definition.name)
                .map_err(|_| sdk("get_effect_item_value"))?;
            // トラックバー以外の項目では失敗するため、移動情報が無いものとして扱う。
            let track = self
                .section
                .get_effect_track_info(effect, &definition.name)
                .ok()
                .flatten()
                .map(track_info);
            items.push(EffectItem {
                value: item_value(&item_type, raw),
                name: definition.name,
                item_type,
                track,
            });
        }
        Ok(items)
    }
}

/// トラックバーの移動情報を所有型へ写す。
fn track_info(track: aviutl2::generic::TrackInfo) -> aviutl2_mcp_core::TrackInfo {
    aviutl2_mcp_core::TrackInfo {
        mode: track.mode,
        params: track
            .params
            .into_iter()
            .filter_map(FiniteF64::try_new)
            .collect(),
        accelerate: track.accelerate,
        decelerate: track.decelerate,
        twopoint: track.twopoint,
        timecontrol: track.timecontrol,
        group_num: track.group_num,
        group_index: track.group_index,
        group_name: track.group_name,
    }
}

/// 設定項目の種別に応じて生文字列を値へ写す。
///
/// 対応する表現が無い種別と、種別どおりに解釈できない値は生文字列のまま返す。
fn item_value(item_type: &EffectItemType, raw: String) -> ItemValue {
    match item_type {
        EffectItemType::Integer => match raw.trim().parse::<i64>() {
            Ok(value) => ItemValue::Integer { value },
            Err(_) => ItemValue::Unknown { raw },
        },
        EffectItemType::Number => match raw.trim().parse::<f64>().ok().and_then(FiniteF64::try_new)
        {
            Some(value) => ItemValue::Number { value },
            None => ItemValue::Unknown { raw },
        },
        EffectItemType::Check => match parse_check(&raw) {
            Some(value) => ItemValue::Bool { value },
            None => ItemValue::Unknown { raw },
        },
        EffectItemType::Color => ItemValue::Color { value: raw },
        EffectItemType::Select | EffectItemType::Combo => ItemValue::Choice {
            value: raw,
            index: None,
        },
        EffectItemType::File => ItemValue::File { path: raw },
        EffectItemType::Folder => ItemValue::Folder { path: raw },
        EffectItemType::Font => ItemValue::Font { name: raw },
        EffectItemType::Text | EffectItemType::String => ItemValue::Text { value: raw },
        EffectItemType::Scene
        | EffectItemType::Range
        | EffectItemType::Mask
        | EffectItemType::Figure
        | EffectItemType::Data
        | EffectItemType::Unknown(_) => ItemValue::Unknown { raw },
    }
}

/// チェックボックスの値を解釈する。
fn parse_check(raw: &str) -> Option<bool> {
    match raw.trim() {
        "0" | "false" => Some(false),
        "1" | "true" => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_value_follows_item_type() {
        assert_eq!(
            item_value(&EffectItemType::Integer, "42".to_string()),
            ItemValue::Integer { value: 42 }
        );
        assert_eq!(
            item_value(&EffectItemType::Number, "1.5".to_string()),
            ItemValue::Number {
                value: FiniteF64::try_new(1.5).unwrap()
            }
        );
        assert_eq!(
            item_value(&EffectItemType::Check, "1".to_string()),
            ItemValue::Bool { value: true }
        );
        assert_eq!(
            item_value(&EffectItemType::File, r"C:\movie.mp4".to_string()),
            ItemValue::File {
                path: r"C:\movie.mp4".to_string()
            }
        );
        assert_eq!(
            item_value(&EffectItemType::Select, "通常".to_string()),
            ItemValue::Choice {
                value: "通常".to_string(),
                index: None
            }
        );
        assert_eq!(
            item_value(&EffectItemType::String, "字幕".to_string()),
            ItemValue::Text {
                value: "字幕".to_string()
            }
        );
    }

    #[test]
    fn unparsable_values_keep_raw_text() {
        for item_type in [
            EffectItemType::Integer,
            EffectItemType::Number,
            EffectItemType::Check,
        ] {
            assert_eq!(
                item_value(&item_type, "?".to_string()),
                ItemValue::Unknown {
                    raw: "?".to_string()
                },
                "{item_type} が生文字列を保持しません"
            );
        }
    }

    #[test]
    fn unsupported_item_types_keep_raw_text() {
        for item_type in [
            EffectItemType::Scene,
            EffectItemType::Range,
            EffectItemType::Mask,
            EffectItemType::Figure,
            EffectItemType::Data,
            EffectItemType::Unknown(99),
        ] {
            assert_eq!(
                item_value(&item_type, "raw".to_string()),
                ItemValue::Unknown {
                    raw: "raw".to_string()
                },
                "{item_type} が生文字列を保持しません"
            );
        }
    }

    #[test]
    fn non_finite_numbers_are_not_accepted() {
        for raw in ["NaN", "inf", "-inf"] {
            assert_eq!(
                item_value(&EffectItemType::Number, raw.to_string()),
                ItemValue::Unknown {
                    raw: raw.to_string()
                }
            );
        }
    }

    #[test]
    fn check_accepts_only_known_forms() {
        assert_eq!(parse_check("0"), Some(false));
        assert_eq!(parse_check("1"), Some(true));
        assert_eq!(parse_check("true"), Some(true));
        assert_eq!(parse_check("2"), None);
        assert_eq!(parse_check(""), None);
    }

    #[test]
    fn effect_type_values_match_the_sdk_order() {
        // ラッパーの種別を i32 経由で写すため、値の対応が崩れると落ちる。
        for (wrapper, expected) in [
            (aviutl2::generic::EffectType::Filter, EffectType::Filter),
            (aviutl2::generic::EffectType::Input, EffectType::Input),
            (
                aviutl2::generic::EffectType::SceneChange,
                EffectType::Transition,
            ),
            (aviutl2::generic::EffectType::Control, EffectType::Control),
            (aviutl2::generic::EffectType::Output, EffectType::Output),
        ] {
            assert_eq!(EffectType::from_raw(i32::from(wrapper)), expected);
        }
    }

    #[test]
    fn effect_item_type_values_match_the_sdk_order() {
        for (wrapper, expected) in [
            (
                aviutl2::generic::EffectItemType::Integer,
                EffectItemType::Integer,
            ),
            (
                aviutl2::generic::EffectItemType::Select,
                EffectItemType::Select,
            ),
            (
                aviutl2::generic::EffectItemType::Folder,
                EffectItemType::Folder,
            ),
        ] {
            assert_eq!(EffectItemType::from_raw(i32::from(wrapper)), expected);
        }
    }

    #[test]
    fn sdk_errors_report_the_failing_function() {
        let error = sdk("find_object");
        assert_eq!(error.details()["sdk_operation"], "find_object");
    }

    #[test]
    fn uninitialized_edit_handle_is_not_ready() {
        // テストでは編集ハンドルが初期化されないため、準備前として扱われる。
        // 参照解決を伴う経路へ入らないことを、ここで確かめる。
        assert!(!SdkReadHost.is_ready());
    }
}
