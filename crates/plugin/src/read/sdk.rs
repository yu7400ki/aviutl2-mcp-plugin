//! SDK の読み取り API を [`ReadHost`] へ写す実装。
//!
//! 参照区間の内側でだけ opaque handle を扱い、区間を抜ける前に所有型へ写す。
//! ハンドルは戻り値・ログ・エラーのいずれにも現れない。
//!
//! SDK に触れる部分と、そこから得た値を組み替える部分を分けている。後者は
//! 自由関数として切り出してあり、SDK 無しで検証できる。

use crate::EDIT_HANDLE;
use crate::read::error::ReadError;
use crate::read::host::{
    EditState, HostEditInfo, HostEffect, HostLayer, HostObject, HostObjectDetail,
    HostObjectPlacement, ReadHost, SceneReader,
};
use aviutl2::generic::{EditSectionError, EffectHandle, ObjectHandle, ReadSection};
use aviutl2_mcp_core::{
    AvailableEffect, AvailableEffectItem, EffectFlags, EffectItem, EffectItemType, EffectType,
    FiniteF64, ItemValue, SectionRange,
};
use std::collections::HashMap;
use std::ops::Range;

/// SDK 呼び出しの失敗を、失敗した関数名つきの型付きエラーにする。
fn sdk(operation: &'static str) -> ReadError {
    ReadError::Sdk { operation }
}

/// 補間後の値が得られなかった失敗にする。
///
/// 値を取る 2 つの呼び出しは「対象が無い」も「呼び出しが失敗した」も同じ
/// 失敗として返す。呼び出す前に対象の存在と種別とフレームの範囲を確かめて
/// あるため、ここへ来た失敗は値そのものが得られなかったことを指す。
fn unavailable(operation: &'static str) -> ReadError {
    ReadError::TrackValueUnavailable { operation }
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
        //
        // この呼び出しはフレームレートを有理数へ畳む際、分母が 0 だと panic
        // する。呼び出し側が捕捉層で包むことを前提にしている。
        host_edit_info(&EDIT_HANDLE.get_edit_info())
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
///
/// 編集区間からも用いる。SDK の編集区間は参照区間へ Deref するため、読み取りは
/// 同じ実装で行える。読み取りを別に書くと、読み取りが返す fingerprint と編集が
/// 照合する fingerprint が別の材料から算出され得る。
pub(crate) struct SdkSceneReader<'a> {
    pub(crate) section: &'a ReadSection,
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
        finite_values(
            list.into_iter().map(|bpm| f64::from(bpm.tempo)),
            "get_grid_bpm_list",
        )
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
            locked: self.layer_locked(layer)?,
        })
    }

    fn layer_locked(&self, layer: usize) -> Result<bool, ReadError> {
        self.section
            .get_layer_lock(layer)
            .map_err(|_| sdk("get_layer_lock"))
    }

    fn object_count(&self, layer: usize) -> Result<usize, ReadError> {
        // 件数しか要らないため、名前と alias は読まない。参照ロックを保持した
        // まま不要な文字列を写すのを避ける。
        let positions = scan_layer(|frame| {
            let Some(handle) = self.find_object_from(layer, frame)? else {
                return Ok(None);
            };
            let position = self
                .section
                .get_object_layer_frame(handle)
                .map_err(|_| sdk("get_object_layer_frame"))?;
            Ok(Some((non_negative(position.end), ())))
        })?;
        Ok(positions.len())
    }

    fn object_placements(&self, layer: usize) -> Result<Vec<HostObjectPlacement>, ReadError> {
        scan_layer(|frame| {
            let Some(handle) = self.find_object_from(layer, frame)? else {
                return Ok(None);
            };
            let placement = self.placement_at(handle)?;
            Ok(Some((placement.frame_end, placement)))
        })
    }

    fn object_identity(&self, layer: usize, frame_start: usize) -> Result<HostObject, ReadError> {
        let handle = self.locate_object(layer, frame_start)?;
        ensure_start_frame(self.object_at(handle)?, frame_start)
    }

    fn object_detail(
        &self,
        layer: usize,
        frame_start: usize,
    ) -> Result<HostObjectDetail, ReadError> {
        let handle = self.locate_object(layer, frame_start)?;
        let object = ensure_start_frame(self.object_at(handle)?, frame_start)?;
        let effects = self.effects_of(handle)?;

        let sections = to_inclusive_sections(
            self.section
                .get_object_section_ranges(handle)
                .map_err(|_| sdk("get_object_section_frame"))?,
        );

        Ok(HostObjectDetail {
            object,
            effects,
            sections,
        })
    }

    fn effect_track_values(
        &self,
        layer: usize,
        frame_start: usize,
        effect_position: usize,
        item_names: &[&str],
        frames: &[f64],
    ) -> Result<Vec<Vec<FiniteF64>>, ReadError> {
        // ハンドルは参照区間の内側で有効であり続ける。項目ごとに引き直さない。
        let effect = self.locate_effect(layer, frame_start, effect_position)?;
        let mut items = Vec::with_capacity(item_names.len());
        for item_name in item_names {
            let mut values = Vec::with_capacity(frames.len());
            for frame in frames {
                // 小数部はフレーム間の位置を指す。丸めずにそのまま渡す。
                values.push(
                    self.section
                        .get_effect_track_value(effect, item_name, *frame)
                        .map_err(|_| unavailable("get_effect_track_value"))?,
                );
            }
            items.push(finite_values(values, "get_effect_track_value")?);
        }
        Ok(items)
    }

    fn effect_check_values(
        &self,
        layer: usize,
        frame_start: usize,
        effect_position: usize,
        item_names: &[&str],
        frames: &[usize],
    ) -> Result<Vec<Vec<bool>>, ReadError> {
        let effect = self.locate_effect(layer, frame_start, effect_position)?;
        let mut items = Vec::with_capacity(item_names.len());
        for item_name in item_names {
            let mut values = Vec::with_capacity(frames.len());
            for frame in frames {
                values.push(
                    self.section
                        .get_effect_check_value(effect, item_name, *frame)
                        .map_err(|_| unavailable("get_effect_check_value"))?,
                );
            }
            items.push(values);
        }
        Ok(items)
    }

    fn track_group_item_names(
        &self,
        layer: usize,
        frame_start: usize,
        effect_name: &str,
        effect_index: usize,
        group_name: &str,
    ) -> Result<Vec<String>, ReadError> {
        let object = self.locate_object(layer, frame_start)?;
        // 0 件は「指定グループが無い」であって失敗ではない。ラッパーは件数の
        // 問い合わせと名前の取得の 2 段を内側で済ませ、0 件を空の列として返す。
        self.section
            .get_object_track_group_names(object, effect_name, effect_index, group_name)
            .map_err(|_| sdk("get_object_track_group_names"))
    }
}

impl SdkSceneReader<'_> {
    /// 指定フレーム以降で最初に見つかる対象のハンドル。
    fn find_object_from(
        &self,
        layer: usize,
        frame: usize,
    ) -> Result<Option<ObjectHandle>, ReadError> {
        self.section
            .find_object_after(layer, frame)
            .map_err(|_| sdk("find_object"))
    }

    /// 開始フレームで対象を引く。対象が無ければ不在として返す。
    fn locate_object(&self, layer: usize, frame: usize) -> Result<ObjectHandle, ReadError> {
        self.find_object_from(layer, frame)?
            .ok_or(ReadError::ObjectNotFound {
                detected_by: "find_object",
            })
    }

    /// effect 列の位置で effect のハンドルを引く。
    ///
    /// ハンドルを取る口を使うため、effect 名へ同名内の順序を表すサフィックスを
    /// 組み立てずに済む。
    fn locate_effect(
        &self,
        layer: usize,
        frame_start: usize,
        position: usize,
    ) -> Result<EffectHandle, ReadError> {
        let object = self.locate_object(layer, frame_start)?;
        let handles = self
            .section
            .get_effects(object)
            .map_err(|_| sdk("get_effect_list"))?;
        handles.get(position).copied().ok_or(sdk("get_effect_list"))
    }

    /// ハンドルが指すオブジェクトの位置と名前を所有型へ写す。
    fn placement_at(&self, handle: ObjectHandle) -> Result<HostObjectPlacement, ReadError> {
        let position = self
            .section
            .get_object_layer_frame(handle)
            .map_err(|_| sdk("get_object_layer_frame"))?;
        // 文字列は次の呼び出しまでしか有効でないため、1 つずつ所有型へ写す。
        let name = self
            .section
            .get_object_name(handle)
            .map_err(|_| sdk("get_object_name"))?;
        Ok(HostObjectPlacement {
            layer: non_negative(position.layer),
            frame_start: non_negative(position.start),
            frame_end: non_negative(position.end),
            name,
        })
    }

    /// ハンドルが指すオブジェクトを、同一性の材料まで含めて所有型へ写す。
    ///
    /// alias を読むのはこの 1 か所だけであり、fingerprint の材料は必ずここで
    /// 揃う。alias は配下 effect の設定値を含むため、effect を読む必要はない。
    fn object_at(&self, handle: ObjectHandle) -> Result<HostObject, ReadError> {
        let placement = self.placement_at(handle)?;
        let alias = self
            .section
            .get_object_alias(handle)
            .map_err(|_| sdk("get_object_alias"))?;
        Ok(HostObject { placement, alias })
    }

    /// オブジェクトに付与された effect を所有型へ写す。
    ///
    /// 一覧の取得は effect が 0 件でも失敗を返すため、失敗した場合は
    /// [`classify_effect_list`] で 0 件と失敗を切り分ける。
    fn effects_of(&self, object: ObjectHandle) -> Result<Vec<HostEffect>, ReadError> {
        let handles = match self.section.get_effects(object) {
            Ok(handles) => handles,
            Err(error) => {
                // 先頭 effect の取得はオブジェクトが存在しないと判断できたときだけ
                // 行う。一覧の失敗が既にオブジェクトの不在を示していれば、同じ
                // 理由で失敗するだけの呼び出しになる。
                let decision = classify_effect_list(section_failure(&error), || {
                    self.section
                        .get_first_effect(object)
                        .err()
                        .map(|error| section_failure(&error))
                });
                return match decision {
                    EffectListDecision::Empty => Ok(Vec::new()),
                    EffectListDecision::ObjectNotFound { detected_by } => {
                        Err(ReadError::ObjectNotFound { detected_by })
                    }
                    EffectListDecision::ListFailed => {
                        tracing::warn!("effect の一覧を取得できませんでした");
                        Err(sdk("get_effect_list"))
                    }
                };
            }
        };

        let mut names = Vec::with_capacity(handles.len());
        for handle in &handles {
            names.push(
                self.section
                    .get_effect_name(*handle)
                    .map_err(|_| sdk("get_effect_name"))?,
            );
        }
        let indices = assign_effect_indices(&names);

        let mut effects = Vec::with_capacity(handles.len());
        for ((handle, name), index) in handles.into_iter().zip(names).zip(indices) {
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
                index,
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
                .map(track_info)
                .transpose()?;
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

/// 参照区間の呼び出しが失敗した理由のうち、判別に用いる区別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionFailure {
    /// 対象のオブジェクトが存在しない。
    ObjectMissing,
    /// 呼び出しそのものが失敗した。
    CallFailed,
}

/// effect 一覧の取得に失敗した対象をどう扱うか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectListDecision {
    /// effect が付いていない。
    Empty,
    /// オブジェクトが存在しない。
    ObjectNotFound {
        /// 不在を検出した SDK 関数の名前。
        detected_by: &'static str,
    },
    /// 一覧の取得だけが失敗した。
    ListFailed,
}

/// 失敗した理由を判別に用いる区別へ写す。
fn section_failure(error: &EditSectionError) -> SectionFailure {
    match error {
        EditSectionError::ObjectDoesNotExist => SectionFailure::ObjectMissing,
        _ => SectionFailure::CallFailed,
    }
}

/// effect 一覧の取得失敗を、0 件・オブジェクト不在・列挙の失敗へ切り分ける。
///
/// 一覧の取得は effect が 0 件でも失敗を返すため、失敗そのものからは 0 件かを
/// 判断できない。`probe` は先頭 effect を引く試行の失敗理由（成功なら `None`）を
/// 返し、これを独立した信号として用いる。
///
/// オブジェクトが存在しない場合は effect の有無を判定できない。空の列を返すと
/// 「effect が付いていない」という誤った主張になるため、失敗として返す。この
/// 区別は一覧の失敗理由そのものに現れるため、先頭 effect を引くまでもなく決まる。
///
/// 先頭が引けるなら effect は存在し、一覧の取得だけが失敗している。先頭も
/// 引けず、かつオブジェクトは存在するなら、付与された effect が無い。
fn classify_effect_list(
    list: SectionFailure,
    probe: impl FnOnce() -> Option<SectionFailure>,
) -> EffectListDecision {
    match list {
        SectionFailure::ObjectMissing => EffectListDecision::ObjectNotFound {
            detected_by: "get_effect_list",
        },
        SectionFailure::CallFailed => match probe() {
            None => EffectListDecision::ListFailed,
            Some(SectionFailure::ObjectMissing) => EffectListDecision::ObjectNotFound {
                detected_by: "get_first_effect",
            },
            Some(SectionFailure::CallFailed) => EffectListDecision::Empty,
        },
    }
}

/// レイヤーを開始フレームの昇順に走査する。
///
/// `next` は「指定フレーム以降で最初に見つかる対象」の終了フレームと値を返し、
/// 対象が無ければ `None` を返す。次の探索は終了フレームの次から始める。
///
/// 探索位置が前進しない場合は同じ対象を返し続けるため、失敗として返す。途中
/// までの一覧を全件として返すと、列挙の母集合から対象が欠けたまま正当な
/// スナップショットとして扱われてしまう。
fn scan_layer<T>(
    mut next: impl FnMut(usize) -> Result<Option<(usize, T)>, ReadError>,
) -> Result<Vec<T>, ReadError> {
    let mut items = Vec::new();
    let mut next_frame = 0usize;
    loop {
        let Some((frame_end, item)) = next(next_frame)? else {
            return Ok(items);
        };
        let advanced = frame_end.saturating_add(1);
        if advanced <= next_frame {
            return Err(sdk("find_object"));
        }
        next_frame = advanced;
        items.push(item);
    }
}

/// 開始フレームが要求と完全に一致することを確かめる。
///
/// 探索は「指定フレーム以降」であり、途中フレームを指定すると後続の対象が
/// 返る。開始フレームで一致する対象だけを受け入れる。
fn ensure_start_frame(object: HostObject, frame_start: usize) -> Result<HostObject, ReadError> {
    if object.placement.frame_start == frame_start {
        Ok(object)
    } else {
        Err(ReadError::ObjectNotFound {
            detected_by: "find_object",
        })
    }
}

/// 終端を含まない区間を、終端を含む区間へ変換する。
pub(crate) fn to_inclusive_sections(ranges: Vec<Range<usize>>) -> Vec<SectionRange> {
    ranges
        .into_iter()
        .map(|range| SectionRange {
            start: range.start,
            end: range.end.saturating_sub(1),
        })
        .collect()
}

/// 同名 effect へ出現順の 0 始まりインデックスを割り当てる。
fn assign_effect_indices(names: &[String]) -> Vec<usize> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    names
        .iter()
        .map(|name| {
            let next = seen.entry(name.as_str()).or_insert(0);
            let index = *next;
            *next += 1;
            index
        })
        .collect()
}

/// SDK の編集情報を所有型へ写す。
///
/// 読み取りと編集区間の入口はどちらもこの 1 か所を通る。同じ規約を 2 通りに
/// 実装すると、同じホストの同じ値を層ごとに別の値として読むことになる。
///
/// 負値を `as usize` で畳んだ巨大値は 0 へ丸め、u32 へ写せない大きさは
/// [`ReadError::EditInfoOutOfRange`] として扱う。畳まれた値をそのまま返すと、
/// 位置や範囲として使ったときに実在しない座標を指す。
pub(crate) fn host_edit_info(info: &aviutl2::generic::EditInfo) -> Result<HostEditInfo, ReadError> {
    Ok(HostEditInfo {
        scene_id: info.scene_id,
        width: edit_info_size(info.width)?,
        height: edit_info_size(info.height)?,
        // 有理数へ畳まれた後の分子・分母であり、ホストが保持する生の
        // rate/scale は約分によって失われている。分母は有理数を構築できた
        // 時点で 0 にならない。
        fps_rate: *info.fps.numer(),
        fps_scale: *info.fps.denom(),
        sample_rate: edit_info_size(info.sample_rate)?,
        cursor_frame: non_negative(info.frame),
        cursor_layer: non_negative(info.layer),
        frame_max: non_negative(info.frame_max),
        layer_max: non_negative(info.layer_max),
        display_frame_start: non_negative(info.display_frame_start),
        display_layer_start: non_negative(info.display_layer_start),
        display_frame_num: non_negative(info.display_frame_num),
        display_layer_num: non_negative(info.display_layer_num),
        select_range_start: info.select_range_start.map(non_negative),
        select_range_end: info.select_range_end.map(non_negative),
    })
}

/// 編集情報が持つ大きさを、受け渡せる幅へ写す。
///
/// 写せないことは呼び出しの失敗ではない。取得そのものは成功しており、
/// 返ってきた値が範囲外だったのだから、両者は別の失敗として名乗る。
fn edit_info_size(value: usize) -> Result<u32, ReadError> {
    u32::try_from(value).map_err(|_| ReadError::EditInfoOutOfRange)
}

/// ラッパーが負値を `as usize` で畳んだ値を 0 へ丸める。
///
/// フレーム番号・レイヤー番号は元が `i32` であり、正当な値が `i32::MAX` を
/// 超えることはない。超えている場合は負値が巨大値へ化けたものとして扱う。
pub(crate) fn non_negative(value: usize) -> usize {
    if value > i32::MAX as usize { 0 } else { value }
}

/// 浮動小数点の並びを、全要素が有限であることを確かめて写す。
///
/// 非有限値を落として並びを短くすると、要素数まで変わってしまう。要素数は
/// 移動方法のパラメータ数として fingerprint の入力に含まれ、grid の並びとしても
/// 応答に現れる。欠けた並びを完全な並びとして返せば、読み取った対象とは別の
/// ものを正当な値として扱うことになるため、失敗として返す。
fn finite_values(
    values: impl IntoIterator<Item = f64>,
    operation: &'static str,
) -> Result<Vec<FiniteF64>, ReadError> {
    values
        .into_iter()
        .map(|value| {
            FiniteF64::try_new(value).ok_or_else(|| {
                tracing::warn!("非有限の数値を受け取りました");
                sdk(operation)
            })
        })
        .collect()
}

/// トラックバーの移動情報を所有型へ写す。
fn track_info(
    track: aviutl2::generic::TrackInfo,
) -> Result<aviutl2_mcp_core::TrackInfo, ReadError> {
    Ok(aviutl2_mcp_core::TrackInfo {
        mode: track.mode,
        params: finite_values(track.params, "get_effect_track_info")?,
        accelerate: track.accelerate,
        decelerate: track.decelerate,
        twopoint: track.twopoint,
        timecontrol: track.timecontrol,
        group_num: track.group_num,
        group_index: track.group_index,
        group_name: track.group_name,
    })
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
        EffectItemType::Number => {
            match raw.trim().parse::<f64>().ok().and_then(FiniteF64::try_new) {
                Some(value) => ItemValue::Number { value },
                None => ItemValue::Unknown { raw },
            }
        }
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
    use aviutl2_mcp_core::ErrorCode;

    fn placement(frame_start: usize, frame_end: usize) -> HostObjectPlacement {
        HostObjectPlacement {
            layer: 1,
            frame_start,
            frame_end,
            name: None,
        }
    }

    fn object(frame_start: usize, frame_end: usize) -> HostObject {
        HostObject {
            placement: placement(frame_start, frame_end),
            alias: format!("[{frame_start}]"),
        }
    }

    /// 開始・終了フレームの組を並べたレイヤーに対する「指定フレーム以降」の探索。
    ///
    /// 指定フレームより後ろに終端がある最初の対象を返す。SDK の `find_object`
    /// と同じく、途中フレームを指定してもその対象が返る。
    fn layer_scan(
        placements: Vec<(usize, usize)>,
    ) -> impl FnMut(usize) -> Result<Option<(usize, HostObjectPlacement)>, ReadError> {
        move |frame| {
            Ok(placements
                .iter()
                .find(|(_, end)| *end >= frame)
                .map(|(start, end)| (*end, placement(*start, *end))))
        }
    }

    #[test]
    fn scan_layer_collects_objects_in_start_frame_order() {
        let objects = scan_layer(layer_scan(vec![(0, 99), (100, 200), (300, 400)])).unwrap();
        assert_eq!(
            objects
                .iter()
                .map(|object| object.frame_start)
                .collect::<Vec<_>>(),
            vec![0, 100, 300]
        );
    }

    #[test]
    fn scan_layer_returns_empty_for_empty_layer() {
        assert!(scan_layer(layer_scan(Vec::new())).unwrap().is_empty());
    }

    #[test]
    fn scan_layer_accepts_an_object_at_frame_zero() {
        // 0 フレームで始まり 0 フレームで終わる対象でも打ち切られない。
        let objects = scan_layer(layer_scan(vec![(0, 0), (1, 1)])).unwrap();
        assert_eq!(objects.len(), 2);
    }

    #[test]
    fn scan_layer_propagates_failures() {
        let error = scan_layer::<()>(|_| Err(sdk("get_object_layer_frame"))).unwrap_err();
        assert_eq!(error.details()["sdk_operation"], "get_object_layer_frame");
    }

    #[test]
    fn scan_layer_fails_when_the_search_does_not_advance() {
        // 同じ対象を返し続ける探索を、途中までの一覧の成功として返さない。
        let error = scan_layer(|_| Ok(Some((0usize, ())))).unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::SdkError);
        assert_eq!(error.details()["sdk_operation"], "find_object");
    }

    #[test]
    fn scan_layer_fails_when_the_search_moves_backwards() {
        let mut calls = 0;
        let error = scan_layer(|_| {
            calls += 1;
            Ok(Some((if calls == 1 { 100 } else { 50 }, ())))
        })
        .unwrap_err();
        assert_eq!(
            error.error_code(),
            ErrorCode::SdkError,
            "後退する探索が成功として返りました"
        );
    }

    #[test]
    fn missing_object_is_not_reported_as_zero_effects() {
        // 一覧の失敗がオブジェクトの不在を示すなら、effect の有無は判定できない。
        // 空の列を返すと「effect が付いていない」という誤った主張になる。
        let mut probed = false;
        let decision = classify_effect_list(SectionFailure::ObjectMissing, || {
            probed = true;
            Some(SectionFailure::CallFailed)
        });
        assert_eq!(
            decision,
            EffectListDecision::ObjectNotFound {
                detected_by: "get_effect_list"
            }
        );
        assert!(!probed, "不在が分かっているのに先頭 effect を引きました");
    }

    #[test]
    fn zero_effects_requires_the_object_to_exist() {
        // 一覧も先頭も引けず、どちらも呼び出しの失敗であれば 0 件として扱う。
        assert_eq!(
            classify_effect_list(SectionFailure::CallFailed, || Some(
                SectionFailure::CallFailed
            )),
            EffectListDecision::Empty
        );
        // 先頭を引く段でオブジェクトの不在が分かった場合は 0 件にしない。
        // 不在を検出したのは先頭 effect を引く呼び出しであり、一覧ではない。
        assert_eq!(
            classify_effect_list(SectionFailure::CallFailed, || Some(
                SectionFailure::ObjectMissing
            )),
            EffectListDecision::ObjectNotFound {
                detected_by: "get_first_effect"
            }
        );
    }

    #[test]
    fn list_failure_with_a_first_effect_is_a_failure() {
        assert_eq!(
            classify_effect_list(SectionFailure::CallFailed, || None),
            EffectListDecision::ListFailed
        );
    }

    #[test]
    fn only_missing_object_maps_to_object_missing() {
        assert_eq!(
            section_failure(&EditSectionError::ObjectDoesNotExist),
            SectionFailure::ObjectMissing
        );
        for error in [
            EditSectionError::ApiCallFailed,
            EditSectionError::EffectDoesNotExist,
        ] {
            assert_eq!(
                section_failure(&error),
                SectionFailure::CallFailed,
                "{error} が不在として扱われました"
            );
        }
    }

    #[test]
    fn ensure_start_frame_requires_exact_match() {
        assert_eq!(
            ensure_start_frame(object(100, 200), 100)
                .unwrap()
                .placement
                .frame_start,
            100
        );
        for frame in [99usize, 101, 150, 200] {
            assert!(
                matches!(
                    ensure_start_frame(object(100, 200), frame),
                    Err(ReadError::ObjectNotFound { .. })
                ),
                "フレーム {frame} が開始フレームとして受理されました"
            );
        }
    }

    #[test]
    fn sections_drop_the_exclusive_end() {
        assert_eq!(
            to_inclusive_sections(vec![120..180, 180..241]),
            vec![
                SectionRange {
                    start: 120,
                    end: 179
                },
                SectionRange {
                    start: 180,
                    end: 240
                },
            ]
        );
    }

    #[test]
    fn empty_section_does_not_underflow() {
        let empty: Range<usize> = 0..0;
        assert_eq!(
            to_inclusive_sections(vec![empty]),
            vec![SectionRange { start: 0, end: 0 }]
        );
        assert!(to_inclusive_sections(Vec::new()).is_empty());
    }

    #[test]
    fn effect_indices_count_per_name_in_order() {
        let names = ["ぼかし", "動画ファイル", "ぼかし", "ぼかし", "動画ファイル"]
            .map(str::to_string)
            .to_vec();
        assert_eq!(assign_effect_indices(&names), vec![0, 0, 1, 2, 1]);
    }

    #[test]
    fn effect_indices_are_empty_for_no_effects() {
        assert!(assign_effect_indices(&[]).is_empty());
    }

    #[test]
    fn negative_positions_are_folded_to_zero() {
        // ラッパーが `-1 as usize` で畳んだ値を正当な位置として扱わない。
        assert_eq!(non_negative(-1i32 as u32 as usize), 0);
        assert_eq!(non_negative(i32::MAX as usize + 1), 0);
        assert_eq!(non_negative(i32::MAX as usize), i32::MAX as usize);
        assert_eq!(non_negative(0), 0);
        assert_eq!(non_negative(1080), 1080);
    }

    /// ホストが渡す編集情報の生の姿。
    ///
    /// ラッパーは各値を `as usize` で畳んでから所有型へ写すため、負値は
    /// 巨大値として届く。生の値から組み立てなければ、その畳み込みを含む
    /// 写しの経路を通らない。
    fn raw_edit_info() -> aviutl2::sys::plugin2::EDIT_INFO {
        aviutl2::sys::plugin2::EDIT_INFO {
            width: 1920,
            height: 1080,
            rate: 30_000,
            scale: 1_001,
            sample_rate: 48_000,
            frame: 0,
            layer: 0,
            frame_max: 100,
            layer_max: 1,
            display_frame_start: 0,
            display_layer_start: 0,
            display_frame_num: 100,
            display_layer_num: 2,
            select_range_start: -1,
            select_range_end: -1,
            grid_bpm_tempo: 120.0,
            grid_bpm_beat: 4,
            grid_bpm_offset: 0.0,
            scene_id: 3,
        }
    }

    /// 生の編集情報を、実際の写しの経路へ通す。
    fn mapped(raw: &aviutl2::sys::plugin2::EDIT_INFO) -> Result<HostEditInfo, ReadError> {
        host_edit_info(&unsafe { aviutl2::generic::EditInfo::from_raw(raw) })
    }

    #[test]
    fn the_edit_info_mapping_reports_an_out_of_range_value() {
        // 読み取り経路の写しは 1 か所しかなく、レンダリング経路もここを通る。
        // 変換だけを見るテストは、写しがその変換を使っていることを見ていない。
        let info = mapped(&raw_edit_info()).expect("正常な編集情報です");
        assert_eq!(info.width, 1920);
        assert_eq!(info.height, 1080);

        // 負の解像度は巨大な usize として届く。位置や範囲として使えば
        // 実在しない座標を指すため、値の範囲外として返す。
        for raw in [
            aviutl2::sys::plugin2::EDIT_INFO {
                width: -1,
                ..raw_edit_info()
            },
            aviutl2::sys::plugin2::EDIT_INFO {
                height: -1,
                ..raw_edit_info()
            },
            aviutl2::sys::plugin2::EDIT_INFO {
                sample_rate: -1,
                ..raw_edit_info()
            },
        ] {
            let error = mapped(&raw).expect_err("範囲外として返ります");
            assert_eq!(error.error_code(), ErrorCode::SdkError);
            let details = error.details();
            assert_eq!(details["sdk_operation"], "get_edit_info");
            assert_eq!(details["reason"], "edit_info_out_of_range");
        }
    }

    #[test]
    fn an_unrepresentable_size_is_reported_as_an_out_of_range_value() {
        // 写せない大きさは呼び出しの失敗ではない。取得は成功しており、
        // 応答は同じ関数を名指ししたまま失敗の種別だけを分ける。
        assert_eq!(edit_info_size(1920).unwrap(), 1920);
        let error = edit_info_size(u32::MAX as usize + 1).unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::SdkError);
        let details = error.details();
        assert_eq!(details["sdk_operation"], "get_edit_info");
        assert_eq!(details["reason"], "edit_info_out_of_range");
    }

    #[test]
    fn finite_values_keeps_every_element() {
        let values = finite_values([120.0, -0.5, 0.0], "get_grid_bpm_list").unwrap();
        assert_eq!(
            values.iter().map(FiniteF64::get).collect::<Vec<_>>(),
            vec![120.0, -0.5, 0.0]
        );
        assert!(finite_values([], "get_grid_bpm_list").unwrap().is_empty());
    }

    #[test]
    fn non_finite_values_fail_instead_of_shortening_the_list() {
        // 落として並びを短くすると要素数が変わる。要素数は応答にも
        // fingerprint の入力にも現れるため、欠けた並びを完全な並びとして
        // 返さない。
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let error = finite_values([1.0, value, 2.0], "get_effect_track_info")
                .expect_err("非有限値が受理されました");
            assert_eq!(error.error_code(), ErrorCode::SdkError);
            assert_eq!(error.details()["sdk_operation"], "get_effect_track_info");
        }
    }

    #[test]
    fn track_info_fails_on_non_finite_params() {
        let track = aviutl2::generic::TrackInfo {
            mode: "直線移動".to_string(),
            params: vec![0.5, f64::NAN],
            accelerate: false,
            decelerate: false,
            twopoint: false,
            timecontrol: false,
            group_num: 1,
            group_index: 0,
            group_name: None,
        };
        let error = track_info(track).expect_err("非有限のパラメータが受理されました");
        assert_eq!(error.error_code(), ErrorCode::SdkError);
    }

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
