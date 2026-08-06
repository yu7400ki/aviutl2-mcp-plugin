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
    EditState, HostEditInfo, HostEffect, HostEffectChoices, HostEffectHelp, HostEffectSummary,
    HostLayer, HostObject, HostObjectDetail, HostObjectPlacement, ReadHost, SceneReader,
    SceneValueReader,
};
use aviutl2::generic::{EditSectionError, EffectHandle, ObjectHandle, ReadSection};
use aviutl2_mcp_core::{
    AvailableEffectItem, EffectFlags, EffectItem, EffectItemType, EffectType, FiniteF64, GridBpm,
    ItemValue, ModuleEntry, ModuleType, Rgba, SectionRange, decode_host_text, decode_track_value,
    parse_check_value,
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

    fn effect_catalog(&self) -> Result<Vec<HostEffectSummary>, ReadError> {
        Ok(EDIT_HANDLE
            .get_effects()
            .into_iter()
            .map(|effect| HostEffectSummary {
                name: effect.name,
                effect_type: EffectType::from_raw(i32::from(effect.effect_type)),
                // 既知ビットのみを組み直した値であり、未知ビットは保持できない。
                flags: EffectFlags::from_raw(effect.flag.to_bits() as u32),
            })
            .collect())
    }

    fn effect_item_count(&self, effect_name: &str) -> Result<usize, ReadError> {
        Ok(self.effect_items(effect_name)?.len())
    }

    fn effect_items(&self, effect_name: &str) -> Result<Vec<AvailableEffectItem>, ReadError> {
        // 件数だけを引く経路と同じ列挙を通る。別の呼び出しにすると、一覧が
        // 名乗った件数と中身の件数が食い違い得る。
        Ok(EDIT_HANDLE
            .get_effect_items(effect_name)
            .map_err(|_| sdk("enum_effect_item"))?
            .into_iter()
            .map(|item| AvailableEffectItem {
                name: item.name,
                item_type: EffectItemType::from_raw(i32::from(item.item_type)),
            })
            .collect())
    }

    fn effect_help(&self, effect_name: &str) -> HostEffectHelp {
        // 編集ハンドルを通らない。供給源はホストが同梱するファイルだけである。
        crate::effect_help::help_of(effect_name)
            .cloned()
            .unwrap_or_default()
    }

    fn effect_choices(&self, effect_name: &str) -> HostEffectChoices {
        // 編集ハンドルを通らない。供給源は埋め込んだ基底とサイドカーだけである。
        HostEffectChoices {
            items: crate::item_choices::table()
                .effect(effect_name)
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn font_names(&self) -> Result<Vec<String>, ReadError> {
        // 列挙は打ち切れない。コールバックは戻り値を持たず、途中で止める手段が
        // 無いため、1 ページを返す要求でも全件が返る。
        Ok(EDIT_HANDLE.get_font_names())
    }

    fn modules(&self) -> Result<Vec<ModuleEntry>, ReadError> {
        Ok(EDIT_HANDLE
            .get_modules()
            .into_iter()
            .map(module_entry)
            .collect())
    }

    fn enter_read_section<T, F>(&self, f: F) -> Result<T, ReadError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn SceneValueReader) -> T + Send,
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

    fn grid_bpm(&self) -> Result<Vec<GridBpm>, ReadError> {
        let list = self
            .section
            .get_grid_bpm_list()
            .map_err(|_| sdk("get_grid_bpm_list"))?;
        list.into_iter().map(grid_bpm).collect()
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
}

impl SceneValueReader for SdkSceneReader<'_> {
    fn palette_names(&self) -> Result<Vec<String>, ReadError> {
        // 列挙は編集ハンドルの機能だが、色と同じ区間の内側で呼ぶ。分けると、
        // 名前を集めてから色を読むまでの間にパレットが差し替わり得る。
        Ok(EDIT_HANDLE.get_palette_names())
    }

    fn current_palette_name(&self) -> Option<String> {
        // 一覧に対する付随情報であり、取得できないこともある。
        self.section.get_palette_name().ok()
    }

    fn palette_colors(&self, name: &str) -> Option<Vec<Rgba>> {
        self.section.get_palette_info(name).ok().map(palette_colors)
    }

    fn selected_placements(&self) -> Result<Vec<HostObjectPlacement>, ReadError> {
        // ハンドルは区間の内側で位置へ写し切る。戻り値へ持ち出さないため、
        // ここから先の経路にハンドルは現れない。
        let handles = self
            .section
            .get_selected_objects()
            .map_err(|_| sdk("get_selected_object"))?;
        handles
            .into_iter()
            .map(|handle| self.placement_at(handle))
            .collect()
    }

    fn focused_object(&self) -> Result<Option<HostObject>, ReadError> {
        let Some(handle) = self
            .section
            .get_focused_object()
            .map_err(|_| sdk("get_focus_object"))?
        else {
            return Ok(None);
        };
        // 応答へ載せるのは概要だけであり、配下 effect は読まない。
        Ok(Some(self.object_at(handle)?))
    }

    fn focus_section(&self) -> Result<Option<usize>, ReadError> {
        self.section
            .get_focus_object_section()
            .map_err(|_| sdk("get_focus_object_section"))
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
            // **値より先に読む。** 移動の有無は生文字列の読み方を決めるため、
            // 値の解釈はこれを持っていなければ行えない。
            //
            // トラックバー以外の項目では失敗するため、移動情報が無いものとして
            // 扱う。移動を持たないトラックバーは成功して `None` を返す。
            let track = self
                .section
                .get_effect_track_info(effect, &definition.name)
                .ok()
                .flatten()
                .map(track_info)
                .transpose()?;
            let raw = self
                .section
                .get_effect_item_value(effect, &definition.name)
                .map_err(|_| sdk("get_effect_item_value"))?;
            items.push(EffectItem {
                value: item_value(&item_type, raw, track.as_ref()),
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
        .map(|value| finite_value(value, operation))
        .collect()
}

/// 非有限の数値を失敗として拒む。
fn finite_value(value: f64, operation: &'static str) -> Result<FiniteF64, ReadError> {
    FiniteF64::try_new(value).ok_or_else(|| {
        tracing::warn!("非有限の数値を受け取りました");
        sdk(operation)
    })
}

/// BPM 情報を所有型へ写す。
///
/// 4 つのフィールドを全て運ぶ。浮動小数点の 3 つは非有限値を拒む——一部だけを
/// 検査すると、検査していないフィールドの非有限値が JSON で null へ落ちる。
fn grid_bpm(info: aviutl2::generic::BpmInfo) -> Result<GridBpm, ReadError> {
    const OPERATION: &str = "get_grid_bpm_list";
    Ok(GridBpm {
        tempo: finite_value(f64::from(info.tempo), OPERATION)?,
        beat: i64::from(info.beat),
        start: finite_value(info.start, OPERATION)?,
        offset: finite_value(f64::from(info.offset), OPERATION)?,
    })
}

/// パレット情報を所有型の色の列へ写す。
///
/// 件数は SDK が固定長で定めており、写した結果も常に
/// [`aviutl2_mcp_core::PALETTE_COLOR_COUNT`] 件になる。不透明度は常に 255 だが
/// 落とさない。
/// 落とすと、SDK が返す形と応答の形が別物になる。
fn palette_colors(info: aviutl2::generic::PaletteInfo) -> Vec<Rgba> {
    info.colors
        .into_iter()
        .map(|color| Rgba {
            r: color.r,
            g: color.g,
            b: color.b,
            a: color.a,
        })
        .collect()
}

/// モジュール情報を所有型へ写す。
///
/// 名前と説明文はラッパーが所有文字列として渡してくるため、ここでの複製は
/// 要らない。種別は raw 値を経由して写す——名前で対応付けると、既知の種別が
/// 増えたときに写し先を足し忘れても静かに通る。
fn module_entry(info: aviutl2::generic::ModuleInfo) -> ModuleEntry {
    ModuleEntry {
        module_type: ModuleType::from_raw(i32::from(info.module_type)),
        name: info.name,
        information: info.information,
    }
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
///
/// 選択肢から選ぶ種別は、選択された表示文字列をそのまま持つ。生文字列として
/// 返すと書き込みが受け付けない形になり、読み取った値をそのまま書き戻せなく
/// なる。
///
/// テキスト種別だけは、ホストが返すエスケープ表記を [`decode_host_text`] で
/// 解いてから載せる。書き込みが同じ表記へ符号化するため、解かなければ読みと
/// 書きが非対称になり、読み取った値を書き戻すたびに包みが育つ。ホストが
/// エスケープ表記で扱わない種別（パス・色・フォント名・選択肢）には掛けない。
/// **これらの値をホストが一切書き換えないという意味ではない**——色は受理した
/// 表記を小文字へ揃えて返す。掛けないのは、包みが最初から無いからである。
///
/// 移動を持つトラックバーの値は 1 つの数値にならない。ホストは区間ごとの値と
/// 移動方法を 1 本の文字列で返すため、数値として解釈できない値がここへ来る。
/// **数値として読める値の扱いは変わらない**——静的なトラックバーは
/// [`ItemValue::Number`] のままであり、移動を持つ項目だけが
/// [`ItemValue::Track`] になる（[`movement_or_unknown`]）。
fn item_value(
    item_type: &EffectItemType,
    raw: String,
    track: Option<&aviutl2_mcp_core::TrackInfo>,
) -> ItemValue {
    match item_type {
        EffectItemType::Integer => match raw.trim().parse::<i64>() {
            Ok(value) => ItemValue::Integer { value },
            Err(_) => movement_or_unknown(raw, track),
        },
        EffectItemType::Number => {
            match raw.trim().parse::<f64>().ok().and_then(FiniteF64::try_new) {
                Some(value) => ItemValue::Number { value },
                None => movement_or_unknown(raw, track),
            }
        }
        EffectItemType::Check => match parse_check_value(&raw) {
            Some(value) => ItemValue::Bool { value },
            None => ItemValue::Unknown { raw },
        },
        EffectItemType::Color => ItemValue::Color { value: raw },
        EffectItemType::Select
        | EffectItemType::Combo
        | EffectItemType::Mask
        | EffectItemType::Figure => ItemValue::Choice { value: raw },
        EffectItemType::File => ItemValue::File { path: raw },
        EffectItemType::Folder => ItemValue::Folder { path: raw },
        EffectItemType::Font => ItemValue::Font { name: raw },
        EffectItemType::Text | EffectItemType::String => ItemValue::Text {
            value: decode_host_text(&raw),
        },
        EffectItemType::Scene
        | EffectItemType::Range
        | EffectItemType::Data
        | EffectItemType::Unknown(_) => ItemValue::Unknown { raw },
    }
}

/// 数値として読めなかった値を、移動として復号する。
///
/// **移動を持つ項目だけが対象である。** ホストは移動方法の名前を持たない
/// トラックバーに対して移動情報を返さないため、`track` が `null` の値は本当に
/// 未知である。そこまで復号を広げると、区切りを含むだけの生値が移動として
/// 読まれ、書き戻したときにホストへ渡るのは我々が捏造した移動になる。
///
/// 復号できない文字列も未知のまま返す。読めなかったことは、読めたふりより
/// 安全である。
///
/// 復号できても移動方法の名前を持たない値は返さない。`track` が非 `null` で
/// あることと矛盾しており、どちらが実態かをこちら側では決められない。
fn movement_or_unknown(raw: String, track: Option<&aviutl2_mcp_core::TrackInfo>) -> ItemValue {
    match track
        .and_then(|_| decode_track_value(&raw))
        .filter(|decoded| decoded.mode.is_some())
    {
        Some(decoded) => ItemValue::Track(decoded),
        None => ItemValue::Unknown { raw },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::{
        AvailableEffectItem, ErrorCode, ItemWriteError, PALETTE_COLOR_COUNT, TrackValue,
        TrackWriteTarget, prepare_item_write,
    };

    #[test]
    fn the_effect_help_comes_from_the_host_help_file() {
        // 説明は編集ハンドルからは得られない。供給源はホストが同梱するファイル
        // だけであり、この境界はそれを引き写すだけである。
        for name in ["図形", "ぼかし", "存在しない効果"] {
            assert_eq!(
                SdkReadHost.effect_help(name),
                crate::effect_help::help_of(name)
                    .cloned()
                    .unwrap_or_default(),
                "{name} の説明が供給源と別のところから来ています"
            );
        }
        // 実行ファイルの隣に供給源が無い環境では説明が出ない。読み取り側の
        // フェイクが説明の無い環境を既定にしている根拠である。
        assert_eq!(SdkReadHost.effect_help("図形"), HostEffectHelp::default());
    }

    #[test]
    fn the_effect_choices_come_from_the_merged_table() {
        // 候補も編集ハンドルからは得られない。この境界は埋め込みの基底へ
        // サイドカーを重ねた表を引き写すだけである。
        for name in ["テキスト", "図形", "存在しない効果"] {
            assert_eq!(
                SdkReadHost.effect_choices(name).items,
                crate::item_choices::table()
                    .effect(name)
                    .cloned()
                    .unwrap_or_default(),
                "{name} の候補が表と別のところから来ています"
            );
        }
    }

    /// 移動を持たない項目の読み取り。
    fn read_value(item_type: &EffectItemType, raw: &str) -> ItemValue {
        item_value(item_type, raw.to_string(), None)
    }

    /// 移動を持つ項目の読み取り。
    ///
    /// ホストが返す移動情報の中身は値の解釈に使わない。使うのは「移動を持つ」
    /// という事実だけであり、区間ごとの値は生文字列の側にしかない。
    fn read_moving_value(item_type: &EffectItemType, raw: &str) -> ItemValue {
        let track = aviutl2_mcp_core::TrackInfo {
            mode: "直線移動".to_string(),
            params: Vec::new(),
            accelerate: false,
            decelerate: false,
            twopoint: false,
            timecontrol: false,
            group_num: 1,
            group_index: 0,
            group_name: None,
        };
        item_value(item_type, raw.to_string(), Some(&track))
    }

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

    /// ホストが渡す BPM 情報の生の姿。
    fn raw_bpm_info() -> aviutl2::generic::BpmInfo {
        aviutl2::generic::BpmInfo {
            tempo: 120.0,
            beat: 4,
            start: 1.5,
            offset: 0.25,
        }
    }

    #[test]
    fn a_bpm_entry_carries_all_four_fields() {
        // tempo だけを取ると、読み取った一覧をそのまま書き戻す経路で
        // beat / start / offset が失われる。
        let mapped = grid_bpm(raw_bpm_info()).expect("正常な BPM 情報です");
        assert_eq!(mapped.tempo.get(), 120.0);
        assert_eq!(mapped.beat, 4);
        assert_eq!(mapped.start.get(), 1.5);
        assert_eq!(mapped.offset.get(), 0.25);
    }

    #[test]
    fn a_non_finite_bpm_field_fails_on_every_float() {
        // 検査を 1 つのフィールドだけに掛けると、残りの非有限値が JSON で
        // null へ落ちる。3 つの浮動小数点すべてを見る。
        for (single, double) in [
            (f32::NAN, f64::NAN),
            (f32::INFINITY, f64::INFINITY),
            (f32::NEG_INFINITY, f64::NEG_INFINITY),
        ] {
            let broken = [
                (
                    "tempo",
                    aviutl2::generic::BpmInfo {
                        tempo: single,
                        ..raw_bpm_info()
                    },
                ),
                (
                    "start",
                    aviutl2::generic::BpmInfo {
                        start: double,
                        ..raw_bpm_info()
                    },
                ),
                (
                    "offset",
                    aviutl2::generic::BpmInfo {
                        offset: single,
                        ..raw_bpm_info()
                    },
                ),
            ];
            for (field, info) in broken {
                let error = grid_bpm(info).expect_err("非有限値が受理されました");
                assert_eq!(error.error_code(), ErrorCode::SdkError, "{field}");
                assert_eq!(
                    error.details()["sdk_operation"],
                    "get_grid_bpm_list",
                    "{field}"
                );
            }
        }
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
            read_value(&EffectItemType::Integer, "42"),
            ItemValue::Integer { value: 42 }
        );
        assert_eq!(
            read_value(&EffectItemType::Number, "1.5"),
            ItemValue::Number {
                value: FiniteF64::try_new(1.5).unwrap()
            }
        );
        assert_eq!(
            read_value(&EffectItemType::Check, "1"),
            ItemValue::Bool { value: true }
        );
        assert_eq!(
            read_value(&EffectItemType::File, r"C:\movie.mp4"),
            ItemValue::File {
                path: r"C:\movie.mp4".to_string()
            }
        );
        assert_eq!(
            read_value(&EffectItemType::Select, "通常"),
            ItemValue::Choice {
                value: "通常".to_string(),
            }
        );
        assert_eq!(
            read_value(&EffectItemType::String, "字幕"),
            ItemValue::Text {
                value: "字幕".to_string()
            }
        );
    }

    #[test]
    fn text_values_are_decoded_and_other_types_are_left_alone() {
        // テキスト種別だけがエスケープ表記を解く。ホストがエスケープ表記で
        // 扱わない種別へ掛けると、`\` を含む値が壊れる。
        for item_type in [EffectItemType::Text, EffectItemType::String] {
            assert_eq!(
                read_value(&item_type, r"C:\\temp\nの先"),
                ItemValue::Text {
                    value: "C:\\temp\nの先".to_string(),
                },
                "{item_type}"
            );
        }
        assert_eq!(
            read_value(&EffectItemType::File, r"C:\\temp"),
            ItemValue::File {
                path: r"C:\\temp".to_string(),
            }
        );
        assert_eq!(
            read_value(&EffectItemType::Select, r"\n"),
            ItemValue::Choice {
                value: r"\n".to_string(),
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
                read_value(&item_type, "?"),
                ItemValue::Unknown {
                    raw: "?".to_string()
                },
                "{item_type} が生文字列を保持しません"
            );
        }
    }

    #[test]
    fn a_moving_trackbar_reads_as_a_movement() {
        // ホストは区間ごとの値と移動方法を 1 本の文字列で返す。生値のまま返すと
        // 書き戻せる形にならず、往復の不変条件がこの 1 種類の値でだけ破れる。
        let mut expected = TrackValue {
            values: vec![
                FiniteF64::try_new(-600.0).expect("有限値"),
                FiniteF64::try_new(600.0).expect("有限値"),
            ],
            mode: Some("直線移動".to_string()),
            params: Vec::new(),
            accelerate: false,
            decelerate: false,
            twopoint: false,
        };
        assert_eq!(
            read_moving_value(&EffectItemType::Number, "-600.00,600.00,直線移動,0"),
            ItemValue::Track(expected.clone())
        );
        // フラグもパラメータも運ぶ。落とすと書き戻しで移動が変わる。
        expected.accelerate = true;
        expected.twopoint = true;
        expected.params = vec![FiniteF64::try_new(15.0).expect("有限値")];
        assert_eq!(
            read_moving_value(&EffectItemType::Number, "-600.00,600.00,直線移動,5|15.00"),
            ItemValue::Track(expected)
        );
    }

    #[test]
    fn a_static_trackbar_stays_a_number() {
        // 移動を持たない項目は区間の数に依らず 1 値である。全トラックバーを
        // 移動として返すと、移動を持たない大多数の項目まで応答が膨らむ。
        assert_eq!(
            read_value(&EffectItemType::Number, "0.00"),
            ItemValue::Number {
                value: FiniteF64::try_new(0.0).expect("有限値"),
            }
        );
        // 移動を持つ項目でも、数値として読める値は数値のままである。
        assert_eq!(
            read_moving_value(&EffectItemType::Number, "12.50"),
            ItemValue::Number {
                value: FiniteF64::try_new(12.5).expect("有限値"),
            }
        );
        assert_eq!(
            read_moving_value(&EffectItemType::Integer, "42"),
            ItemValue::Integer { value: 42 }
        );
    }

    #[test]
    fn a_value_without_a_movement_stays_unknown() {
        // 復号の対象は移動を持つ項目だけである。広げると、区切りを含むだけの
        // 生値が移動として読まれ、書き戻しでホストへ渡るのは捏造した移動になる。
        for raw in ["-600.00,600.00,直線移動,0", "0,1", "?"] {
            assert_eq!(
                read_value(&EffectItemType::Number, raw),
                ItemValue::Unknown {
                    raw: raw.to_string()
                },
                "{raw} が移動として読まれました"
            );
        }
        // 移動を持つ項目でも、復号できない値は未知のままである。
        for raw in ["-600.00,600.00,直線移動", "0,100,直線移動,x"] {
            assert_eq!(
                read_moving_value(&EffectItemType::Number, raw),
                ItemValue::Unknown {
                    raw: raw.to_string()
                },
                "{raw} が移動として読まれました"
            );
        }
        // 移動方法の名前を持たない値も返さない。移動情報が非 null であることと
        // 矛盾しており、どちらが実態かをこちら側では決められない。
        assert_eq!(
            read_moving_value(&EffectItemType::Integer, "0.50"),
            ItemValue::Unknown {
                raw: "0.50".to_string()
            }
        );
    }

    #[test]
    fn a_movement_that_was_read_can_be_written_straight_back() {
        // 読み取りが返した値をそのまま書き戻せる。ホストが桁を整えても、
        // 復号は同じ移動へ辿り着く。
        let value = read_moving_value(&EffectItemType::Number, "-600.00,600.00,直線移動,0");
        let items = vec![AvailableEffectItem {
            name: "X".to_string(),
            item_type: EffectItemType::Number,
        }];
        let movements = vec!["直線移動".to_string()];
        let write = prepare_item_write(
            &items,
            "X",
            &value,
            TrackWriteTarget {
                section_count: 1,
                movements: &movements,
            },
        )
        .expect("読み取った移動が書き戻せません");
        assert_eq!(write.value(), "-600,600,直線移動,0");
        assert_eq!(
            write.read_back_matches("-600.00,600.00,直線移動,0"),
            Some(true)
        );
    }

    #[test]
    fn unsupported_item_types_keep_raw_text() {
        for item_type in [
            EffectItemType::Scene,
            EffectItemType::Range,
            EffectItemType::Data,
            EffectItemType::Unknown(99),
        ] {
            assert_eq!(
                read_value(&item_type, "raw"),
                ItemValue::Unknown {
                    raw: "raw".to_string()
                },
                "{item_type} が生文字列を保持しません"
            );
        }
    }

    #[test]
    fn every_choice_type_reads_into_a_choice_value() {
        // 選択肢から選ぶ 4 種別は同じ形で返る。生文字列で返すと書き込みが
        // 受け付けず、読み取った値をそのまま書き戻せない。
        for item_type in [
            EffectItemType::Select,
            EffectItemType::Combo,
            EffectItemType::Mask,
            EffectItemType::Figure,
        ] {
            assert_eq!(
                read_value(&item_type, "四角形"),
                ItemValue::Choice {
                    value: "四角形".to_string(),
                },
                "{item_type} が選択肢として返りません"
            );
        }
    }

    /// 既知の種別と、未知を名乗る種別を 1 つ並べる。
    ///
    /// 読み取りの写像は未知の種別にも答えるため、検査の対象は既知の一覧より
    /// 1 件広い。
    fn all_item_types() -> Vec<EffectItemType> {
        EffectItemType::ALL
            .iter()
            .cloned()
            .chain([EffectItemType::Unknown(99)])
            .collect()
    }

    /// 種別ごとに、読み取りが返す値と、その値をそのまま書き戻した結果を並べる。
    ///
    /// 読み取りの写像・書き込みを公開する種別・種別が受け付ける値の形の 3 つが
    /// 揃って初めて、読み取った値がそのまま書き戻せる。どれか 1 つが欠けると
    /// この表と食い違う。
    ///
    /// **テキスト種別の生値にはエスケープ表記を含める。** 生値をそのまま値へ
    /// 載せる種別と同じ入力にすると、復号と符号化のどちらを外しても表が通り、
    /// 対称性の検査が働かなくなる。
    ///
    /// 表の生値はいずれも移動を持たない項目のものである。移動の往復は種別では
    /// なく移動の有無で分かれるため、種別を軸にしたこの表とは別に見る
    /// （[`a_movement_that_was_read_can_be_written_straight_back`]）。
    fn read_then_write_back() -> Vec<(
        EffectItemType,
        &'static str,
        ItemValue,
        Result<String, ItemWriteError>,
    )> {
        let unknown_value = Err(ItemWriteError::UnknownValue);
        vec![
            (
                EffectItemType::Integer,
                "42",
                ItemValue::Integer { value: 42 },
                Ok("42".to_string()),
            ),
            (
                EffectItemType::Number,
                "1.5",
                ItemValue::Number {
                    value: FiniteF64::try_new(1.5).unwrap(),
                },
                Ok("1.5".to_string()),
            ),
            (
                EffectItemType::Check,
                "1",
                ItemValue::Bool { value: true },
                Ok("1".to_string()),
            ),
            (
                EffectItemType::Text,
                r"字幕\n2 行目",
                ItemValue::Text {
                    value: "字幕\n2 行目".to_string(),
                },
                Ok(r"字幕\n2 行目".to_string()),
            ),
            (
                EffectItemType::String,
                r"C:\\temp",
                ItemValue::Text {
                    value: r"C:\temp".to_string(),
                },
                Ok(r"C:\\temp".to_string()),
            ),
            (
                EffectItemType::File,
                r"C:\movie.mp4",
                ItemValue::File {
                    path: r"C:\movie.mp4".to_string(),
                },
                Ok(r"C:\movie.mp4".to_string()),
            ),
            (
                EffectItemType::Color,
                "#ff8800",
                ItemValue::Color {
                    value: "#ff8800".to_string(),
                },
                Ok("#ff8800".to_string()),
            ),
            (
                EffectItemType::Select,
                "通常",
                ItemValue::Choice {
                    value: "通常".to_string(),
                },
                Ok("通常".to_string()),
            ),
            (
                EffectItemType::Scene,
                "0",
                ItemValue::Unknown {
                    raw: "0".to_string(),
                },
                unknown_value.clone(),
            ),
            (
                EffectItemType::Range,
                "0",
                ItemValue::Unknown {
                    raw: "0".to_string(),
                },
                unknown_value.clone(),
            ),
            (
                EffectItemType::Combo,
                "左寄せ[上]",
                ItemValue::Choice {
                    value: "左寄せ[上]".to_string(),
                },
                Ok("左寄せ[上]".to_string()),
            ),
            (
                EffectItemType::Mask,
                "四角形",
                ItemValue::Choice {
                    value: "四角形".to_string(),
                },
                Ok("四角形".to_string()),
            ),
            (
                EffectItemType::Font,
                "Meiryo",
                ItemValue::Font {
                    name: "Meiryo".to_string(),
                },
                Ok("Meiryo".to_string()),
            ),
            (
                EffectItemType::Figure,
                "星型",
                ItemValue::Choice {
                    value: "星型".to_string(),
                },
                Ok("星型".to_string()),
            ),
            (
                EffectItemType::Data,
                "opaque",
                ItemValue::Unknown {
                    raw: "opaque".to_string(),
                },
                unknown_value.clone(),
            ),
            (
                EffectItemType::Folder,
                r"C:\assets",
                ItemValue::Folder {
                    path: r"C:\assets".to_string(),
                },
                Ok(r"C:\assets".to_string()),
            ),
            (
                EffectItemType::Unknown(99),
                "opaque",
                ItemValue::Unknown {
                    raw: "opaque".to_string(),
                },
                unknown_value,
            ),
        ]
    }

    #[test]
    fn the_read_mapping_and_the_write_path_agree_for_every_item_type() {
        // 読み取りが返した値をそのまま書き戻す。表が種別を網羅しているため、
        // 読み取りの写像・公開する種別・受け付ける値の形のどれを片方だけ
        // 変えても落ちる。
        let table = read_then_write_back();
        assert_eq!(
            table
                .iter()
                .map(|(item_type, ..)| item_type.clone())
                .collect::<Vec<_>>(),
            all_item_types(),
            "表が種別を網羅していません"
        );
        for (item_type, raw, expected_read, expected_write) in table {
            let value = read_value(&item_type, raw);
            assert_eq!(value, expected_read, "{item_type} の読み取り");
            let items = vec![AvailableEffectItem {
                name: "項目".to_string(),
                item_type: item_type.clone(),
            }];
            assert_eq!(
                prepare_item_write(
                    &items,
                    "項目",
                    &value,
                    // 表の値はいずれも移動を含まない。対象の中身が参照されない
                    // ため、空の対象で足りる。
                    TrackWriteTarget {
                        section_count: 0,
                        movements: &[],
                    },
                )
                .map(|write| write.value().to_string()),
                expected_write,
                "{item_type} の書き戻し"
            );
        }
    }

    #[test]
    fn non_finite_numbers_are_not_accepted() {
        for raw in ["NaN", "inf", "-inf"] {
            assert_eq!(
                read_value(&EffectItemType::Number, raw),
                ItemValue::Unknown {
                    raw: raw.to_string()
                }
            );
        }
    }

    #[test]
    fn check_values_the_host_does_not_use_stay_raw() {
        // 解釈できない表記を真偽値へ丸めると、書き戻したときに別の値になる。
        for raw in ["2", "", "yes"] {
            assert_eq!(
                read_value(&EffectItemType::Check, raw),
                ItemValue::Unknown {
                    raw: raw.to_string()
                }
            );
        }
        assert_eq!(
            read_value(&EffectItemType::Check, "true"),
            ItemValue::Bool { value: true }
        );
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
    fn palette_colors_keeps_every_color_and_the_alpha() {
        // 写像はフェイクの外側にあり、読み取り口を差し替えた検査では 1 度も
        // 通らない。SDK の型から直接呼んで固定する。
        let colors = std::array::from_fn(|index| aviutl2::generic::PaletteColor {
            r: index as u8,
            g: 1,
            b: 2,
            a: 255,
        });
        let mapped = palette_colors(aviutl2::generic::PaletteInfo { colors });

        assert_eq!(mapped.len(), PALETTE_COLOR_COUNT);
        assert_eq!(mapped.len(), 64);
        for (index, color) in mapped.iter().enumerate() {
            assert_eq!(color.r, index as u8, "{index} 番目の色が並び替わっています");
            assert_eq!((color.g, color.b), (1, 2), "{index}");
            assert_eq!(color.a, 255, "{index} 番目の不透明度が落ちています");
        }
    }

    #[test]
    fn module_entries_map_every_known_type() {
        // 種別の写像は raw 値を経由する。既知の種別が増えたときに写し先を
        // 足し忘れると、ここが未知の種別として落とす。
        for (wrapper, expected) in [
            (
                aviutl2::generic::ModuleType::ScriptFilter,
                ModuleType::ScriptFilter,
            ),
            (
                aviutl2::generic::ModuleType::ScriptObject,
                ModuleType::ScriptObject,
            ),
            (
                aviutl2::generic::ModuleType::ScriptCamera,
                ModuleType::ScriptCamera,
            ),
            (
                aviutl2::generic::ModuleType::ScriptTrack,
                ModuleType::ScriptTrack,
            ),
            (
                aviutl2::generic::ModuleType::ScriptModule,
                ModuleType::ScriptModule,
            ),
            (
                aviutl2::generic::ModuleType::PluginInput,
                ModuleType::PluginInput,
            ),
            (
                aviutl2::generic::ModuleType::PluginOutput,
                ModuleType::PluginOutput,
            ),
            (
                aviutl2::generic::ModuleType::PluginFilter,
                ModuleType::PluginFilter,
            ),
            (
                aviutl2::generic::ModuleType::PluginGeneric,
                ModuleType::PluginGeneric,
            ),
        ] {
            let entry = module_entry(aviutl2::generic::ModuleInfo {
                module_type: wrapper,
                name: "名前".to_string(),
                information: "説明".to_string(),
            });
            assert_eq!(entry.module_type, expected, "{wrapper:?}");
            assert_eq!(entry.name, "名前");
            assert_eq!(entry.information, "説明");
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
