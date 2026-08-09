//! 読み取り operation の手順。
//!
//! SDK 呼び出しは [`ReadHost`] へ委ね、ここでは受付可否の判定・参照区間の
//! 使い方・応答へ載せる DTO の組み立てだけを行う。セレクターの解決は編集と
//! 共有する [`crate::read::resolve`] が持つ。SDK の型は現れない。

use crate::project::ProjectState;
use crate::read::error::ReadError;
use crate::read::host::{
    EditState, HostEditInfo, HostEffectSummary, HostObjectDetail, ReadHost, SceneValueReader,
};
use crate::read::resolve::{
    dropped_from_page, effect_fingerprint_inputs, effect_info_at, find_effect_position,
    object_summary, resolve_selected_detail, scene_info,
};
use crate::read::{Page, ProjectStatus, ReadAdapter, Snapshot};
use aviutl2_mcp_core::{
    AvailableEffect, Cursor, DescribeEffectsParams, DescribeEffectsResult, DisplayRange, EditInfo,
    EffectDescription, EffectInfo, EffectItem, EffectItemDescription, EffectItemValues,
    EffectSelector, EffectType, EvaluatedItem, EvaluatedItemKind, Extent,
    GetEffectItemValuesParams, LayerInfo, ListAvailableEffectsResult, ListObjectAliasesResult,
    ListPalettesResult, MAX_EVALUATED_ITEMS, ModuleEntry, ModuleType, ObjectDetail, ObjectFilter,
    ObjectSelector, ObjectSummary, PageWindow, PaletteEntry, SceneInfo, SelectionSnapshot,
    SnapshotRevisionMismatch, TrackGroup, ValidatedPageRequest, take_page, take_window,
};
use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

/// [`ReadHost`] の上に読み取り operation を実装した adapter。
pub struct HostReadAdapter<H> {
    host: H,
    project: Arc<ProjectState>,
}

impl<H> HostReadAdapter<H> {
    /// ホストとプロジェクト状態から adapter を作る。
    pub fn new(host: H, project: Arc<ProjectState>) -> Self {
        Self { host, project }
    }
}

impl<H: ReadHost> HostReadAdapter<H> {
    /// 読み取りを受け付けられる状態かを確かめる。
    ///
    /// 準備前の編集ハンドルは読み取り API の呼び出し自体が許されないため、
    /// ここを通らない限り [`ReadHost`] の他のメソッドを呼ばない。
    ///
    /// 準備状態の問い合わせも捕捉層で包む。[`ReadHost`] の呼び出しは実装ごとに
    /// panic し得るため、どのメソッドから入っても接続の境界まで巻き戻らない形を
    /// 保つ。
    fn ensure_readable(&self) -> Result<(), ReadError> {
        if !catch(|| self.host.is_ready())? {
            return Err(ReadError::NotReady);
        }
        match self.edit_state()? {
            EditState::Edit => Ok(()),
            state => Err(ReadError::EditBlocked { state }),
        }
    }

    /// 現在の編集状態を取得する。
    fn edit_state(&self) -> Result<EditState, ReadError> {
        guard(|| self.host.edit_state())
    }

    /// 参照区間の外で編集情報を取得する。
    ///
    /// この取得はフレームレートの分母が 0 のとき panic する。参照区間の外、
    /// つまり通常の Rust スレッドで起きるため、捕捉しなければ接続の境界まで
    /// 巻き戻り、応答を返さないまま切断してしまう。ここで型付きの失敗へ落とす。
    fn edit_info(&self) -> Result<HostEditInfo, ReadError> {
        guard(|| self.host.edit_info())
    }

    /// 登録済み effect の見出しを取得する。
    fn effect_catalog(&self) -> Result<Vec<HostEffectSummary>, ReadError> {
        guard(|| self.host.effect_catalog())
    }

    /// panic を捕捉した状態で参照区間へ入る。
    ///
    /// 参照区間のコールバックは C の関数ポインタから呼ばれるため、panic を
    /// 境界の外へ伝播させるとホストのプロセスごと落ちる。クロージャを捕捉層で
    /// 包んでからホストへ渡し、境界を越える巻き戻しを起こさない。
    ///
    /// 参照区間へ入る呼び出し自体も panic し得る。準備前の編集ハンドルは
    /// 呼び出しの入口で落ちるため、渡すクロージャだけを包んでも捕捉できない。
    /// 捕捉しなければ接続の境界まで巻き戻り、要求元は応答ではなく切断を
    /// 観測する。呼び出し全体を捕捉層で包む。
    ///
    /// クロージャを保持する領域は呼び出しごとに解放されないため、捕らえる値は
    /// 参照と数値だけに留め、所有値を移し込まない。
    fn read_section<T, F>(&self, f: F) -> Result<T, ReadError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn SceneValueReader) -> Result<T, ReadError> + Send,
    {
        let entered = catch(|| {
            self.host
                .enter_read_section(move |scene| guard(|| f(scene)))
        })?;
        match entered {
            Ok(result) => result,
            Err(error) => Err(self.classify_section_failure(error)),
        }
    }

    /// 参照区間へ入れなかった失敗を、現在の編集状態で分類し直す。
    ///
    /// 参照の確保は再生・出力中に失敗する。受付判定と参照の確保の間に再生や
    /// 出力が始まる競合がこの失敗の主因であり、戻り値だけでは他の失敗と
    /// 区別できない。編集状態を読み直して再生・出力中であれば、時間を置けば
    /// 解消する失敗として返す。読み直しにも失敗した場合は元の分類を保つ。
    fn classify_section_failure(&self, error: ReadError) -> ReadError {
        match self.edit_state() {
            Ok(EditState::Edit) | Err(_) => error,
            Ok(state) => ReadError::EditBlocked { state },
        }
    }
}

/// クロージャの panic を型付きの失敗へ変換し、戻り値はそのまま返す。
fn catch<T>(f: impl FnOnce() -> T) -> Result<T, ReadError> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|_| ReadError::Panicked)
}

/// 失敗を返し得るクロージャの panic を型付きの失敗へ変換する。
fn guard<T>(f: impl FnOnce() -> Result<T, ReadError>) -> Result<T, ReadError> {
    catch(f).and_then(|result| result)
}

impl<H: ReadHost> ReadAdapter for HostReadAdapter<H> {
    fn project_status(&self) -> ProjectStatus {
        // epoch・revision・modified はいずれもプロジェクト状態が保持しており、
        // SDK を呼ばずに読める。受付判定を通さないのはそのためである。
        ProjectStatus {
            epoch: self.project.epoch(),
            revision: self.project.revision(),
            modified: self.project.modified(),
        }
    }

    fn get_edit_info(&self) -> Result<EditInfo, ReadError> {
        self.ensure_readable()?;
        let info = self.edit_info()?;
        let epoch = self.project.epoch();
        let project = self.project.as_ref();

        let (revision, scene_name, grid_bpm) = self.read_section(move |scene| {
            let revision = project.revision();
            let scene_name = scene.scene_name();
            let grid_bpm = scene.grid_bpm()?;
            Ok((revision, scene_name, grid_bpm))
        })?;

        Ok(EditInfo {
            scene: scene_info(&info, scene_name),
            cursor: Cursor {
                frame: info.cursor_frame,
                layer: info.cursor_layer,
            },
            extent: Extent {
                frame_max: info.frame_max,
                layer_max: info.layer_max,
            },
            display: DisplayRange {
                frame_start: info.display_frame_start,
                layer_start: info.display_layer_start,
                frame_num: info.display_frame_num,
                layer_num: info.display_layer_num,
            },
            selected_range: info.selected_range,
            grid_bpm,
            project_epoch: epoch,
            project_revision: revision,
        })
    }

    fn get_current_scene(&self) -> Result<(SceneInfo, u64), ReadError> {
        self.ensure_readable()?;
        let info = self.edit_info()?;
        let project = self.project.as_ref();

        let (revision, scene_name) =
            self.read_section(move |scene| Ok((project.revision(), scene.scene_name())))?;

        Ok((scene_info(&info, scene_name), revision))
    }

    fn list_layers(&self, expected_scene_id: i32) -> Result<Snapshot<LayerInfo>, ReadError> {
        self.ensure_readable()?;
        let info = self.edit_info()?;
        ensure_scene(&info, expected_scene_id)?;
        let layer_max = info.layer_max;
        let project = self.project.as_ref();

        let (snapshot_revision, items) = self.read_section(move |scene| {
            let revision = project.revision();
            // 件数は編集情報由来であり事前に確保しない。ラッパーが負値を
            // 畳んだ値がそのまま容量として渡ると確保だけで落ちる。
            let mut items = Vec::new();
            for index in 0..=layer_max {
                let layer = scene.layer(index)?;
                let object_count = scene.object_count(index)?;
                items.push(LayerInfo {
                    index,
                    name: layer.name,
                    enabled: layer.enabled,
                    locked: layer.locked,
                    object_count,
                });
            }
            Ok((revision, items))
        })?;

        Ok(Snapshot {
            items,
            snapshot_revision,
        })
    }

    fn list_objects(
        &self,
        expected_scene_id: i32,
        filter: Option<&ObjectFilter>,
        page: &ValidatedPageRequest,
    ) -> Result<Result<Page<ObjectSummary>, SnapshotRevisionMismatch>, ReadError> {
        self.ensure_readable()?;
        let info = self.edit_info()?;
        ensure_scene(&info, expected_scene_id)?;
        let layers = layer_range(filter, info.layer_max);
        let scene_id = info.scene_id;
        let epoch = self.project.epoch();
        let epoch = epoch.as_str();
        let project = self.project.as_ref();
        let page = *page;

        self.read_section(move |scene| {
            let revision = project.revision();
            // 位置と名前だけを読んで全件を並べる。総件数と並び順はここで確定し、
            // ページのメタ情報もこの並びから組み立てる。
            let mut placements = Vec::new();
            for layer in layers {
                placements.extend(scene.object_placements(layer)?);
            }

            let (window, meta) = match take_page(&placements, &page, revision) {
                Ok(page) => page,
                Err(error) => return Ok(Err(error)),
            };

            // alias を読むのは窓に入った対象だけにする。応答へ載せない対象まで
            // 読むと、参照区間の保持時間が要求ページではなくプロジェクトの規模で
            // 決まってしまう。配下 effect は概要に現れないため読まない。
            let mut items = Vec::with_capacity(window.len());
            for placement in window {
                let object = scene
                    .object_identity(placement.layer, placement.frame_start)
                    .map_err(enumeration_failure)?;
                items.push(object_summary(epoch, scene_id, &object));
            }
            Ok(Ok(Page { items, meta }))
        })
    }

    fn get_object(&self, selector: &ObjectSelector) -> Result<ObjectDetail, ReadError> {
        self.ensure_readable()?;
        let info = self.edit_info()?;
        ensure_scene(&info, selector.scene_id)?;

        let epoch = self.project.epoch();
        if epoch != selector.project_epoch {
            return Err(ReadError::EpochMismatch);
        }

        let scene_id = info.scene_id;
        let epoch = epoch.as_str();
        let project = self.project.as_ref();

        self.read_section(move |scene| {
            let revision = project.revision();
            let (summary, detail) = resolve_selected_detail(scene, epoch, scene_id, selector)?;
            Ok(object_detail(summary, revision, detail))
        })
    }

    fn get_selection(
        &self,
        expected_scene_id: i32,
        page: &ValidatedPageRequest,
    ) -> Result<Result<SelectionSnapshot, SnapshotRevisionMismatch>, ReadError> {
        self.ensure_readable()?;
        let info = self.edit_info()?;
        ensure_scene(&info, expected_scene_id)?;
        let scene_id = info.scene_id;
        let epoch = self.project.epoch();
        let epoch = epoch.as_str();
        let project = self.project.as_ref();
        let page = *page;

        self.read_section(move |scene| {
            let revision = project.revision();

            // フォーカス対象と区間番号は同じ区間の内側で続けて読む。
            let focused = scene.focused_object()?;
            let section = scene.focus_section()?;
            // 区間番号はフォーカス対象の性質である。対象が無いのに番号だけが
            // 返る組は応答へ載せない。
            let focus_section = focused.as_ref().and(section);

            // 位置だけを読んで全件を並べる。ホストが返す順序は規定されて
            // いないため、ここで並べ替えて総件数と並び順を確定する。並びを
            // 揃えないと、ページ間で順序が変わって取りこぼしと重複が同時に
            // 起きる。オブジェクトの列挙と同じ並びにすることで、要求元は
            // 2 つの応答を突き合わせられる。
            let mut placements = scene.selected_placements()?;
            placements.sort_by_key(|placement| (placement.layer, placement.frame_start));

            let (window, meta) = match take_page(&placements, &page, revision) {
                Ok(page) => page,
                Err(error) => return Ok(Err(error)),
            };

            // alias を読むのは窓に入った対象だけにする。応答へ載せない対象まで
            // 読むと、参照区間の保持時間が要求ページではなく選択の規模で
            // 決まってしまう。
            let mut selected = Vec::with_capacity(window.len());
            for placement in window {
                let object = scene
                    .object_identity(placement.layer, placement.frame_start)
                    .map_err(enumeration_failure)?;
                selected.push(object_summary(epoch, scene_id, &object));
            }

            Ok(Ok(SelectionSnapshot {
                project_revision: revision,
                focus: focused
                    .as_ref()
                    .map(|object| object_summary(epoch, scene_id, object)),
                focus_section,
                selected,
                page: meta,
            }))
        })
    }

    fn list_available_effects(
        &self,
        effect_type: Option<&EffectType>,
        page: &PageWindow,
    ) -> Result<ListAvailableEffectsResult, ReadError> {
        self.ensure_readable()?;
        // カタログは参照区間を必要としないため、列挙の直前の revision を採る。
        let snapshot_revision = self.project.revision();

        // 見出しだけを先に集める。項目数は窓に入った分だけ読む。
        let mut summaries = self.effect_catalog()?;
        if let Some(effect_type) = effect_type {
            summaries.retain(|effect| effect.effect_type == *effect_type);
        }
        let (window, meta) = take_window(&summaries, page, snapshot_revision);

        let mut items = Vec::with_capacity(window.len());
        for effect in window {
            let item_count = guard(|| self.host.effect_item_count(&effect.name))?;
            items.push(AvailableEffect {
                // 説明はホストが同梱するものだけを運ぶ。無い effect は null に
                // なり、供給源を読めない環境では全件が null になる。
                description: catch(|| self.host.effect_help(&effect.name))?.description,
                name: effect.name,
                effect_type: effect.effect_type,
                flags: effect.flags,
                item_count,
            });
        }
        Ok(ListAvailableEffectsResult { items, page: meta })
    }

    fn describe_effects(
        &self,
        params: &DescribeEffectsParams,
    ) -> Result<DescribeEffectsResult, ReadError> {
        self.ensure_readable()?;

        // 登録の有無はカタログで決める。設定項目の列挙は「登録されていない」と
        // 「列挙に失敗した」を同じ失敗で返すため、それだけでは分けられない。
        let registered: HashSet<String> = self
            .effect_catalog()?
            .into_iter()
            .map(|effect| effect.name)
            .collect();

        let mut effects = Vec::new();
        let mut not_found = Vec::new();
        for name in &params.effect_names {
            if !registered.contains(name) {
                not_found.push(name.clone());
                continue;
            }
            let listed = guard(|| self.host.effect_items(name))?;
            let help = catch(|| self.host.effect_help(name))?;
            let facets = catch(|| self.host.effect_facets(name))?;
            let mut items = Vec::with_capacity(listed.len());
            for item in listed {
                // 説明も面も項目名で引く。ホストの列挙に無い項目は現れず、
                // 持たない項目は null になる。
                let item_facets = facets.items.get(&item.name);
                // グループは項目ごとに 1 度引く。所属アイテム名から他の項目の
                // グループを導くと、項目名が effect の中で一意であることを
                // 前提に置くことになる。
                let group = guard(|| self.host.effect_item_group(name, &item.name))?;
                items.push(EffectItemDescription {
                    description: help.items.get(&item.name).cloned(),
                    choices: item_facets.and_then(|facets| facets.choices.clone()),
                    range: item_facets.and_then(|facets| facets.range),
                    group,
                    name: item.name,
                    item_type: item.item_type,
                });
            }
            effects.push(EffectDescription {
                name: name.clone(),
                description: help.description,
                items,
            });
        }
        Ok(DescribeEffectsResult { effects, not_found })
    }

    fn list_fonts(&self) -> Result<Snapshot<String>, ReadError> {
        self.ensure_readable()?;
        // 参照区間を必要としないため、列挙の直前の revision を採る。
        let snapshot_revision = self.project.revision();
        let items = guard(|| self.host.font_names())?;
        Ok(Snapshot {
            items,
            snapshot_revision,
        })
    }

    fn list_palettes(&self, page: &PageWindow) -> Result<ListPalettesResult, ReadError> {
        self.ensure_readable()?;
        let project = self.project.as_ref();
        let page = *page;

        self.read_section(move |scene| {
            let revision = project.revision();

            // 名前だけを先に集める。色は窓に入った分だけ読む。
            let names = scene.palette_names()?;
            let (window, meta) = take_window(&names, &page, revision);

            let mut items = Vec::with_capacity(window.len());
            let mut dropped = 0usize;
            for name in window {
                match scene.palette_colors(&name) {
                    Some(colors) => items.push(PaletteEntry { name, colors }),
                    // 列挙が返した名前で色が取れないのは異常だが、その 1 件の
                    // ために一覧全体を落とさない。落とした件数は総件数へ反映する。
                    None => dropped += 1,
                }
            }

            // 現在のパレット名は付随情報である。取れなくても一覧は返す。
            let current = scene.current_palette_name();

            Ok(ListPalettesResult {
                current,
                page: dropped_from_page(meta, dropped, items.len()),
                items,
            })
        })
    }

    fn list_modules(
        &self,
        module_type: Option<&ModuleType>,
    ) -> Result<Snapshot<ModuleEntry>, ReadError> {
        self.ensure_readable()?;
        // 参照区間を必要としないため、列挙の直前の revision を採る。
        let snapshot_revision = self.project.revision();
        let mut items = guard(|| self.host.modules())?;
        if let Some(module_type) = module_type {
            items.retain(|module| module.module_type == *module_type);
        }
        Ok(Snapshot {
            items,
            snapshot_revision,
        })
    }

    fn list_object_aliases(
        &self,
        label: Option<&str>,
        page: &PageWindow,
    ) -> Result<ListObjectAliasesResult, ReadError> {
        // SDK を 1 度も呼ばない。受付判定も参照区間も通らず、読むのは
        // AviUtl2 のデータディレクトリ配下のファイルだけである。
        let Some(data_dir) = crate::alias::data_directory() else {
            return Err(ReadError::AliasDirectoryUnavailable);
        };
        // 列挙を始めた時点の revision を採る。一覧の内容はこの値に連動しない。
        let snapshot_revision = self.project.revision();
        Ok(crate::alias::list_object_aliases(
            data_dir,
            label,
            page,
            snapshot_revision,
            &crate::alias::DiskAliasFiles,
        ))
    }

    fn get_effect_item_values(
        &self,
        params: &GetEffectItemValuesParams,
    ) -> Result<EffectItemValues, ReadError> {
        self.ensure_readable()?;
        let info = self.edit_info()?;
        let selector = &params.effect;
        ensure_scene(&info, selector.object.scene_id)?;

        let epoch = self.project.epoch();
        if epoch != selector.object.project_epoch {
            return Err(ReadError::EpochMismatch);
        }

        let scene_id = info.scene_id;
        let epoch = epoch.as_str();
        let project = self.project.as_ref();
        // フレームは種別ごとに違う形で渡す。トラックバーは小数部がフレーム間の
        // 位置を指すためそのまま運び、チェックボックスは整数フレームしか取らない。
        let track_frames: Vec<f64> = params.frames.iter().map(|frame| frame.get()).collect();
        let check_frames: Vec<usize> = track_frames.iter().map(|frame| *frame as usize).collect();
        let requested = params.items.as_deref();

        self.read_section(move |scene| {
            let revision = project.revision();
            let (summary, detail) =
                resolve_selected_detail(scene, epoch, scene_id, &selector.object)?;
            let position = find_effect_position(
                &detail.effects,
                &selector.effect_name,
                selector.effect_index,
            )
            .ok_or(ReadError::EffectNotFound)?;
            let effect = effect_info_at(&summary.selector, &detail.effects, position).ok_or(
                ReadError::Sdk {
                    operation: "get_effect_list",
                },
            )?;
            if effect.selector.fingerprint != selector.fingerprint {
                return Err(ReadError::EffectFingerprintMismatch);
            }

            // 呼び出す前に区別できる失敗をここで出し切る。ラッパーは呼び出しの
            // 失敗と値が無いことを 1 つの失敗へ潰すため、通した後の失敗からは
            // 何が起きたのかを名乗れない。
            let targets = select_evaluated_items(&detail.effects[position].items, requested)?;
            ensure_frames_within(&summary, &track_frames)?;

            // 種別ごとにまとめて評価する。対象の解決が項目数に比例しない。
            let track_names = targets.names_of(EvaluatedItemKind::Track);
            let check_names = targets.names_of(EvaluatedItemKind::Check);
            let mut track_values = if track_names.is_empty() {
                Vec::new()
            } else {
                scene.effect_track_values(
                    summary.layer,
                    summary.frame_start,
                    position,
                    &track_names,
                    &track_frames,
                )?
            }
            .into_iter();
            let mut check_values = if check_names.is_empty() {
                Vec::new()
            } else {
                scene.effect_check_values(
                    summary.layer,
                    summary.frame_start,
                    position,
                    &check_names,
                    &check_frames,
                )?
            }
            .into_iter();

            let mut group_names: HashMap<String, Vec<String>> = HashMap::new();
            let mut items = Vec::with_capacity(targets.items.len());
            for (item, kind) in &targets.items {
                items.push(match kind {
                    EvaluatedItemKind::Track => EvaluatedItem::Track {
                        // 要求した項目の数だけ返ることは境界の義務である。
                        // 足りないなら値は得られていない。
                        values: track_values
                            .next()
                            .ok_or(unavailable("get_effect_track_value"))?,
                        group: track_group(scene, &summary, selector, item, &mut group_names)?,
                        name: item.name.clone(),
                    },
                    EvaluatedItemKind::Check => EvaluatedItem::Check {
                        values: check_values
                            .next()
                            .ok_or(unavailable("get_effect_check_value"))?,
                        name: item.name.clone(),
                    },
                });
            }

            Ok(EffectItemValues {
                project_revision: revision,
                frames: params.frames.clone(),
                items,
                truncated: targets.truncated,
            })
        })
    }
}

/// 事前確認を通した要求に対して値が得られなかった失敗にする。
fn unavailable(operation: &'static str) -> ReadError {
    ReadError::TrackValueUnavailable { operation }
}

/// 評価する設定項目の選び方の結果。
struct EvaluationTargets<'a> {
    /// 評価する項目と、その評価の種別。
    items: Vec<(&'a EffectItem, EvaluatedItemKind)>,
    /// 上限で打ち切ったか。
    truncated: bool,
}

impl EvaluationTargets<'_> {
    /// 指定した種別の項目名を、要求した順序のまま並べる。
    fn names_of(&self, kind: EvaluatedItemKind) -> Vec<&str> {
        self.items
            .iter()
            .filter(|(_, item_kind)| *item_kind == kind)
            .map(|(item, _)| item.name.as_str())
            .collect()
    }
}

/// 評価する設定項目を、要求と effect の項目一覧から決める。
///
/// 名指しされた項目は、存在しないことと種別が違うことを別の失敗として返す。
/// 前者は名前を直す要求であり、後者は別の項目を選ぶ要求であって、要求元が次に
/// 取る行動が違う。
///
/// 省略された場合は評価できる項目すべてを対象とするが、件数は上限で打ち切る。
/// 項目数が上限を超える effect はあり得るため、黙って落とさずに打ち切ったことを
/// 伝える。
fn select_evaluated_items<'a>(
    items: &'a [EffectItem],
    requested: Option<&[String]>,
) -> Result<EvaluationTargets<'a>, ReadError> {
    let Some(requested) = requested else {
        let mut all: Vec<(&EffectItem, EvaluatedItemKind)> = items
            .iter()
            .filter_map(|item| Some((item, item.item_type.evaluated_kind()?)))
            .collect();
        let truncated = all.len() > MAX_EVALUATED_ITEMS;
        all.truncate(MAX_EVALUATED_ITEMS);
        return Ok(EvaluationTargets {
            items: all,
            truncated,
        });
    };

    let mut selected = Vec::with_capacity(requested.len());
    for name in requested {
        let item = items
            .iter()
            .find(|item| item.name == *name)
            .ok_or(ReadError::ItemNotFound)?;
        let kind = item
            .item_type
            .evaluated_kind()
            .ok_or(ReadError::ItemNotEvaluatable)?;
        selected.push((item, kind));
    }
    Ok(EvaluationTargets {
        items: selected,
        truncated: false,
    })
}

/// 要求されたフレームが対象オブジェクトの範囲に収まることを確かめる。
///
/// フレームはシーンの絶対フレーム番号である。オブジェクトの外を指す要求には
/// 補間する対象そのものが無い。
fn ensure_frames_within(summary: &ObjectSummary, frames: &[f64]) -> Result<(), ReadError> {
    let start = summary.frame_start as f64;
    let end = summary.frame_end as f64;
    if frames.iter().all(|frame| *frame >= start && *frame <= end) {
        Ok(())
    } else {
        Err(ReadError::FrameOutOfRange)
    }
}

/// トラックバー項目が属するグループを組み立てる。
///
/// 所属アイテム名の取得はグループ名ごとに 1 度だけ行う。同じグループの項目を
/// まとめて評価する要求で、同じ一覧を項目の数だけ引き直さない。
///
/// グループのトラック数と所属アイテム名の件数は一致するとは限らない。一致を
/// 強制せず、両方をそのまま返して要求元に見せる。
fn track_group(
    scene: &dyn SceneValueReader,
    summary: &ObjectSummary,
    selector: &EffectSelector,
    item: &EffectItem,
    cache: &mut HashMap<String, Vec<String>>,
) -> Result<Option<TrackGroup>, ReadError> {
    let Some(track) = &item.track else {
        return Ok(None);
    };
    let Some(name) = &track.group_name else {
        return Ok(None);
    };
    let item_names = match cache.get(name) {
        Some(cached) => cached.clone(),
        None => {
            let fetched = scene.track_group_item_names(
                summary.layer,
                summary.frame_start,
                &selector.effect_name,
                selector.effect_index,
                name,
            )?;
            cache.insert(name.clone(), fetched.clone());
            fetched
        }
    };
    Ok(Some(TrackGroup {
        name: name.clone(),
        index: track.group_index,
        count: track.group_num,
        item_names,
    }))
}

/// 列挙の途中で対象を読めなくなった失敗を、列挙そのものの失敗として畳む。
///
/// 対象を 1 つも指定しない列挙で「対象が見つからない」を返しても、要求元は何が
/// 見つからなかったのかを特定できず、次の行動も決められない。列挙が全件を
/// 返せなかったという事実として伝える。
///
/// 不在は対象の探索でも effect 一覧の取得でも検出され得る。切り分けを誤った
/// 系統へ誘導しないよう、実際に検出した呼び出しをそのまま引き継ぐ。
fn enumeration_failure(error: ReadError) -> ReadError {
    match error {
        ReadError::ObjectNotFound { detected_by } => ReadError::Sdk {
            operation: detected_by,
        },
        other => other,
    }
}

/// 現在シーンが要求の前提と一致することを確かめる。
fn ensure_scene(info: &HostEditInfo, expected_scene_id: i32) -> Result<(), ReadError> {
    if info.scene_id == expected_scene_id {
        Ok(())
    } else {
        Err(ReadError::SceneMismatch {
            expected: expected_scene_id,
            current: info.scene_id,
        })
    }
}

/// 絞り込み条件を現在のレイヤー数で丸めた走査範囲。
///
/// 上限が現在のレイヤー数を超える指定は丸める。下限が現在のレイヤー数を超える
/// 場合は空の範囲になり、結果 0 件として扱う。下限が上限を上回る指定は要求の
/// 誤りとして呼び出し側が先に弾く。
fn layer_range(filter: Option<&ObjectFilter>, layer_max: usize) -> RangeInclusive<usize> {
    let min = filter.and_then(|filter| filter.layer_min).unwrap_or(0);
    let max = filter
        .and_then(|filter| filter.layer_max)
        .unwrap_or(layer_max)
        .min(layer_max);
    min..=max
}

/// オブジェクトの詳細を、算出済みの概要と組み合わせて組み立てる。
fn object_detail(summary: ObjectSummary, revision: u64, detail: HostObjectDetail) -> ObjectDetail {
    let effects = effect_fingerprint_inputs(&detail.effects)
        .map(|input| EffectInfo::new(summary.selector.clone(), input))
        .collect();
    ObjectDetail {
        alias: detail.object.alias,
        summary,
        sections: detail.sections,
        effects,
        project_revision: revision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read::host::{
        HostEffect, HostEffectFacets, HostEffectHelp, HostLayer, HostObject, HostObjectPlacement,
        SceneReader,
    };
    use crate::test_support::{
        alias_with_effects, default_page_request, default_page_window, page_request,
        with_silent_panic_hook,
    };
    use aviutl2_mcp_core::{
        AvailableEffectItem, EffectFlags, EffectItem, EffectItemType, ErrorCode, Fingerprint,
        FiniteF64, FrameRange, GridBpm, ItemChoices, ItemFacets, ItemGroup, ItemRange, ItemValue,
        PALETTE_COLOR_COUNT, Rgba, SectionRange, TableSource, TrackInfo, TrackValue,
    };
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// panic させる位置。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum PanicPoint {
        /// 参照区間の外。準備状態の問い合わせで落ちる。
        IsReady,
        /// 参照区間の外。編集情報の取得で落ちる。
        EditInfo,
        /// 参照区間へ入る呼び出しそのもの。クロージャは呼ばれない。
        EnterSection,
        /// 参照区間の内側。
        SceneName,
        /// 参照区間の内側。
        ObjectPlacements,
    }

    /// 配下 effect の一覧を引いたことを表す記録。
    const EFFECT_LIST: &str = "get_effect_list";

    /// 編集情報の取得が失敗するしかた。
    ///
    /// どちらも同じ SDK 関数から来る同じコードの失敗であり、フェイクの境界で
    /// 作り分けなければ区別が付いているかを確かめられない。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum EditInfoFailure {
        /// 呼び出しそのものが失敗した。
        CallFailed,
        /// 取得は成功したが、返ってきた値が受け渡せる範囲を超えている。
        OutOfRange,
    }

    /// テスト用のオブジェクト。
    ///
    /// 同一性の材料と配下 effect を別に保つ。読み取り経路がそれぞれを別の呼び
    /// 出しで返すため、フェイクも同じ形で保持する。
    #[derive(Debug, Clone)]
    struct FakeObject {
        identity: HostObject,
        effects: Vec<HostEffect>,
    }

    /// テスト用のレイヤー。
    #[derive(Debug, Clone)]
    struct FakeLayer {
        name: Option<String>,
        enabled: bool,
        locked: bool,
        objects: Vec<FakeObject>,
    }

    /// 設定項目 1 件についてホストがグループをどう答えるか。
    #[derive(Debug, Clone)]
    enum FakeItemGroup {
        /// グループに属する。
        Member(ItemGroup),
        /// 引けない。
        Unavailable,
    }

    /// SDK の代わりに定型データを返すホスト。
    ///
    /// 呼び出された経路を記録するため、受付前に SDK を呼ばないことを検証できる。
    /// 準備前の呼び出しは、実際の SDK と同じく panic で落とす。
    struct FakeHost {
        ready: bool,
        state: EditState,
        /// 2 回目以降の編集状態。参照区間の失敗後の読み直しに使う。
        later_state: Option<EditState>,
        edit_state_calls: AtomicUsize,
        info: HostEditInfo,
        /// 編集情報の取得を失敗させるしかた。
        edit_info_failure: Option<EditInfoFailure>,
        scene_name: Option<String>,
        grid_bpm: Vec<GridBpm>,
        layers: Vec<FakeLayer>,
        catalog: Vec<FakeCatalogEntry>,
        /// ホスト環境が持つ effect ごとの説明。
        ///
        /// **既定は空である。** 供給源を読めない環境がそのまま既定であり、説明が
        /// 得られることを前提にした経路を作らない。
        help: Vec<(String, HostEffectHelp)>,
        /// 表が持つ effect ごとの面。
        ///
        /// **既定は空である。** 中身を持たない基底だけの環境がそのまま既定で
        /// あり、面が得られることを前提にした経路を作らない。
        facets: Vec<(String, HostEffectFacets)>,
        /// 設定項目ごとのグループ。effect 名と設定項目名の組で引く。
        ///
        /// **一覧に無い項目はグループに属さない。** 属さないことと引けないことを
        /// 別々に作れるよう、引けない項目は [`FakeItemGroup::Unavailable`] で置く。
        groups: Vec<((String, String), FakeItemGroup)>,
        /// 設定項目の数を問い合わせた effect 名を、問い合わせた順に覚える。
        ///
        /// 窓の外の effect について問い合わせていないことを、件数でも名前でも
        /// 数えられる。
        item_count_queries: Mutex<Vec<String>>,
        panic_at: Option<PanicPoint>,
        /// 対象そのものの読み取りを失敗させる開始フレーム。
        ///
        /// 特定のオブジェクトだけが読めない状況を作り、他の対象の読み取りが
        /// 巻き込まれないことを確かめるために用いる。
        object_read_fails_at: Option<usize>,
        /// 配下 effect の読み取りだけを失敗させる対象の開始フレーム。
        ///
        /// 同一性の材料は読めるのに effect の一覧だけが取れない状況を作る。
        effects_fail_at: Option<usize>,
        /// 走査には現れるのに読み直せない対象の開始フレーム。
        ///
        /// 走査と読み直しの間に対象が消えた状況を作る。
        object_missing_at: Option<usize>,
        /// 参照区間の確保そのものを失敗させる。
        section_fails: bool,
        /// 参照区間へ入る直前に進めるプロジェクト revision の回数。
        bump_on_enter: u64,
        /// 参照区間へ入る直前にプロジェクト境界を更新するか。
        ///
        /// 境界は非再入の Mutex で守られている。読み取りが区間を跨いでそれを
        /// 保持していれば、この更新で待ち合わせが解けなくなる。
        renew_boundary_on_enter: bool,
        /// 値の取得が SDK 境界で受け取った引数の記録。
        ///
        /// 種別ごとにフレームの形が違うことを、境界で受け取った値そのもので
        /// 確かめられる。
        evaluations: Mutex<Vec<Evaluation>>,
        /// 値の取得を失敗させる設定項目名。
        ///
        /// 事前確認をすべて通ったのに値が返らない状況を作る。
        values_unavailable_for: Option<String>,
        /// トラックバーグループごとの所属アイテム名。
        ///
        /// 一覧に無いグループ名は 0 件で返る。「指定グループが無い」は失敗では
        /// ないというヘッダーの明記をそのまま写す。
        group_item_names: Vec<(String, Vec<String>)>,
        /// タイムライン上で選択されている対象を、ホストが返す順序で並べたもの。
        ///
        /// レイヤー番号と開始フレーム番号の組で指す。並び順は規定されて
        /// いないため、与えた順序をそのまま返す。
        selected: Vec<(usize, usize)>,
        /// 登録済みフォント名。
        fonts: Vec<String>,
        /// 登録済みモジュール。
        modules: Vec<ModuleEntry>,
        /// 登録済みパレット名を、ホストが返す順序で並べたもの。
        palettes: Vec<String>,
        /// 色を取得できないパレット名。
        ///
        /// 列挙が返した名前で情報が取れない状況を作る。
        palettes_without_colors: Vec<String>,
        /// 現在のパレット名。名乗らないホストは `None`。
        current_palette: Option<String>,
        /// オブジェクト設定ウィンドウで選択されている対象。
        focus: Option<(usize, usize)>,
        /// フォーカス対象の区間番号。
        ///
        /// [`Self::focus`] とは独立に持つ。対象が無いのに番号だけを返すホストを
        /// 作れる。
        focus_section: Option<usize>,
        project: Option<Arc<ProjectState>>,
        calls: Mutex<Vec<&'static str>>,
    }

    /// 値の取得が受け取った引数。
    ///
    /// 種別ごとに 1 度だけ呼ばれる。項目ごとに呼び直していれば要素数で分かる。
    #[derive(Debug, Clone, PartialEq)]
    enum Evaluation {
        /// トラックバー。小数部を保ったフレームを受け取る。
        Track {
            items: Vec<String>,
            frames: Vec<f64>,
        },
        /// チェックボックス。整数フレームを受け取る。
        Check {
            items: Vec<String>,
            frames: Vec<usize>,
        },
    }

    /// フレームから作るトラックバーの値。
    ///
    /// 値をフレームの単射な関数にしてある。並びが入れ替われば結果に現れる。
    fn track_value_at(frame: f64) -> f64 {
        frame * 10.0 + 1.0
    }

    /// フレームから作るチェックボックスの値。
    fn check_value_at(frame: usize) -> bool {
        frame.is_multiple_of(2)
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                ready: true,
                state: EditState::Edit,
                later_state: None,
                edit_state_calls: AtomicUsize::new(0),
                info: fake_edit_info(),
                edit_info_failure: None,
                scene_name: Some("Scene 1".to_string()),
                grid_bpm: vec![sample_grid_bpm()],
                layers: fake_layers(),
                catalog: fake_catalog(),
                help: Vec::new(),
                facets: Vec::new(),
                groups: Vec::new(),
                item_count_queries: Mutex::new(Vec::new()),
                panic_at: None,
                object_read_fails_at: None,
                effects_fail_at: None,
                object_missing_at: None,
                section_fails: false,
                bump_on_enter: 0,
                renew_boundary_on_enter: false,
                evaluations: Mutex::new(Vec::new()),
                values_unavailable_for: None,
                group_item_names: Vec::new(),
                selected: Vec::new(),
                fonts: fake_fonts(),
                modules: fake_modules(),
                palettes: fake_palette_names(),
                palettes_without_colors: Vec::new(),
                current_palette: Some("[標準.既定]".to_string()),
                focus: None,
                focus_section: None,
                project: None,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn evaluations(&self) -> Vec<Evaluation> {
            self.evaluations.lock().unwrap().clone()
        }

        /// 設定項目の数を問い合わせた effect 名を、問い合わせた順に返す。
        fn item_count_queries(&self) -> Vec<String> {
            self.item_count_queries.lock().unwrap().clone()
        }

        /// 準備前の呼び出しを、実際の SDK と同じ失敗モードで再現する。
        fn assert_ready(&self, api: &str) {
            assert!(self.ready, "準備前に {api} が呼ばれました");
        }

        fn record(&self, call: &'static str) {
            self.calls.lock().unwrap().push(call);
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl ReadHost for FakeHost {
        fn is_ready(&self) -> bool {
            assert_ne!(
                self.panic_at,
                Some(PanicPoint::IsReady),
                "準備状態の問い合わせで panic させます"
            );
            self.ready
        }

        fn edit_state(&self) -> Result<EditState, ReadError> {
            self.assert_ready("get_edit_state");
            self.record("edit_state");
            let calls = self.edit_state_calls.fetch_add(1, Ordering::Relaxed);
            Ok(if calls == 0 {
                self.state
            } else {
                self.later_state.unwrap_or(self.state)
            })
        }

        fn edit_info(&self) -> Result<HostEditInfo, ReadError> {
            self.assert_ready("get_edit_info");
            self.record("edit_info");
            assert_ne!(
                self.panic_at,
                Some(PanicPoint::EditInfo),
                "参照区間の外で panic させます"
            );
            match self.edit_info_failure {
                Some(EditInfoFailure::CallFailed) => Err(ReadError::Sdk {
                    operation: "get_edit_info",
                }),
                Some(EditInfoFailure::OutOfRange) => Err(ReadError::EditInfoOutOfRange),
                None => Ok(self.info.clone()),
            }
        }

        fn effect_catalog(&self) -> Result<Vec<HostEffectSummary>, ReadError> {
            self.assert_ready("get_effects");
            self.record("effect_catalog");
            Ok(self
                .catalog
                .iter()
                .map(|entry| entry.summary.clone())
                .collect())
        }

        fn effect_item_count(&self, effect_name: &str) -> Result<usize, ReadError> {
            self.assert_ready("enum_effect_item");
            self.record("effect_item_count");
            self.item_count_queries
                .lock()
                .unwrap()
                .push(effect_name.to_string());
            self.catalog
                .iter()
                .find(|entry| entry.summary.name == effect_name)
                .map(|entry| entry.items.len())
                .ok_or(ReadError::Sdk {
                    operation: "enum_effect_item",
                })
        }

        fn effect_items(&self, effect_name: &str) -> Result<Vec<AvailableEffectItem>, ReadError> {
            self.assert_ready("enum_effect_item");
            self.record("effect_items");
            self.catalog
                .iter()
                .find(|entry| entry.summary.name == effect_name)
                .map(|entry| entry.items.clone())
                .ok_or(ReadError::Sdk {
                    operation: "enum_effect_item",
                })
        }

        fn effect_item_group(
            &self,
            effect_name: &str,
            item_name: &str,
        ) -> Result<Option<ItemGroup>, ReadError> {
            self.assert_ready("get_effect_item_group_names");
            self.record("effect_item_group");
            match self
                .groups
                .iter()
                .find(|((effect, item), _)| effect == effect_name && item == item_name)
                .map(|(_, group)| group)
            {
                Some(FakeItemGroup::Member(group)) => Ok(Some(group.clone())),
                Some(FakeItemGroup::Unavailable) => Err(ReadError::Sdk {
                    operation: "get_effect_item_group_names",
                }),
                None => Ok(None),
            }
        }

        fn effect_help(&self, effect_name: &str) -> HostEffectHelp {
            self.assert_ready("effect_help");
            self.record("effect_help");
            self.help
                .iter()
                .find(|(name, _)| name == effect_name)
                .map(|(_, help)| help.clone())
                .unwrap_or_default()
        }

        fn effect_facets(&self, effect_name: &str) -> HostEffectFacets {
            self.assert_ready("effect_facets");
            self.record("effect_facets");
            self.facets
                .iter()
                .find(|(name, _)| name == effect_name)
                .map(|(_, facets)| facets.clone())
                .unwrap_or_default()
        }

        fn font_names(&self) -> Result<Vec<String>, ReadError> {
            self.assert_ready("enum_font_name");
            self.record("font_names");
            Ok(self.fonts.clone())
        }

        fn modules(&self) -> Result<Vec<ModuleEntry>, ReadError> {
            self.assert_ready("enum_module_info");
            self.record("modules");
            Ok(self.modules.clone())
        }

        fn enter_read_section<T, F>(&self, f: F) -> Result<T, ReadError>
        where
            T: Send + 'static,
            F: FnOnce(&dyn SceneValueReader) -> T + Send,
        {
            self.assert_ready("call_read_section");
            self.record("enter_read_section");
            // 実際の SDK は準備前の呼び出しをこの位置の assert で落とす。
            // クロージャは呼ばれないため、渡す側を包んでも捕捉できない。
            assert_ne!(
                self.panic_at,
                Some(PanicPoint::EnterSection),
                "参照区間へ入る呼び出しで panic させます"
            );
            if self.section_fails {
                return Err(ReadError::Sdk {
                    operation: "call_read_section",
                });
            }
            if let Some(project) = &self.project {
                for _ in 0..self.bump_on_enter {
                    project.on_object_updated();
                }
                if self.renew_boundary_on_enter {
                    project.on_project_load(Some(r"C:\projects\reopened.aup2"));
                }
            }
            let scene = FakeScene { host: self };
            Ok(f(&scene))
        }
    }

    struct FakeScene<'a> {
        host: &'a FakeHost,
    }

    impl SceneReader for FakeScene<'_> {
        fn scene_name(&self) -> Option<String> {
            assert_ne!(
                self.host.panic_at,
                Some(PanicPoint::SceneName),
                "参照区間の内側で panic させます"
            );
            self.host.scene_name.clone()
        }

        fn grid_bpm(&self) -> Result<Vec<GridBpm>, ReadError> {
            Ok(self.host.grid_bpm.clone())
        }

        fn layer(&self, layer: usize) -> Result<HostLayer, ReadError> {
            let fake = self.host.layers.get(layer).ok_or(ReadError::Sdk {
                operation: "get_layer_name",
            })?;
            Ok(HostLayer {
                name: fake.name.clone(),
                enabled: fake.enabled,
                locked: fake.locked,
            })
        }

        fn layer_locked(&self, layer: usize) -> Result<bool, ReadError> {
            Ok(self.layer(layer)?.locked)
        }

        fn object_count(&self, layer: usize) -> Result<usize, ReadError> {
            self.host.record("object_count");
            Ok(self
                .host
                .layers
                .get(layer)
                .map(|fake| fake.objects.len())
                .unwrap_or_default())
        }

        fn object_placements(&self, layer: usize) -> Result<Vec<HostObjectPlacement>, ReadError> {
            self.host.record("object_placements");
            assert_ne!(
                self.host.panic_at,
                Some(PanicPoint::ObjectPlacements),
                "参照区間の内側で panic させます"
            );
            Ok(self
                .host
                .layers
                .get(layer)
                .map(|fake| {
                    fake.objects
                        .iter()
                        .map(|object| object.identity.placement.clone())
                        .collect()
                })
                .unwrap_or_default())
        }

        fn object_identity(
            &self,
            layer: usize,
            frame_start: usize,
        ) -> Result<HostObject, ReadError> {
            self.host.record("object_identity");
            if self.host.object_missing_at == Some(frame_start) {
                // 対象の探索が不在を検出する。alias を読む前に分かる。
                return Err(ReadError::ObjectNotFound {
                    detected_by: "find_object",
                });
            }
            Ok(self.find(layer, frame_start)?.identity.clone())
        }

        fn object_detail(
            &self,
            layer: usize,
            frame_start: usize,
        ) -> Result<HostObjectDetail, ReadError> {
            self.host.record("object_detail");
            if self.host.object_missing_at == Some(frame_start) {
                // 実際の SDK では effect 一覧の取得も不在を検出する。検出元が
                // 対象の探索とは限らないことを、この経路で再現する。
                return Err(ReadError::ObjectNotFound {
                    detected_by: "get_effect_list",
                });
            }
            let object = self.find(layer, frame_start)?;
            // effect の一覧を引くのはこの経路だけである。記録しておくことで、
            // effect を読まないはずの経路が読んでいないことを確かめられる。
            self.host.record(EFFECT_LIST);
            if self.host.effects_fail_at == Some(frame_start) {
                return Err(ReadError::Sdk {
                    operation: "get_effect_list",
                });
            }
            Ok(HostObjectDetail {
                object: object.identity.clone(),
                effects: object.effects.clone(),
                sections: vec![SectionRange {
                    start: object.identity.placement.frame_start,
                    end: object.identity.placement.frame_end,
                }],
            })
        }
    }

    impl SceneValueReader for FakeScene<'_> {
        fn palette_names(&self) -> Result<Vec<String>, ReadError> {
            self.host.record("palette_names");
            Ok(self.host.palettes.clone())
        }

        fn current_palette_name(&self) -> Option<String> {
            self.host.record("current_palette_name");
            self.host.current_palette.clone()
        }

        fn palette_colors(&self, name: &str) -> Option<Vec<Rgba>> {
            self.host.record("palette_colors");
            if self.host.palettes_without_colors.iter().any(|n| n == name) {
                return None;
            }
            Some(fake_palette_colors(name))
        }

        fn selected_placements(&self) -> Result<Vec<HostObjectPlacement>, ReadError> {
            self.host.record("selected_placements");
            self.host
                .selected
                .iter()
                .map(|&(layer, frame_start)| {
                    Ok(self.find(layer, frame_start)?.identity.placement.clone())
                })
                .collect()
        }

        fn focused_object(&self) -> Result<Option<HostObject>, ReadError> {
            self.host.record("focused_object");
            match self.host.focus {
                Some((layer, frame_start)) => {
                    Ok(Some(self.find(layer, frame_start)?.identity.clone()))
                }
                None => Ok(None),
            }
        }

        fn focus_section(&self) -> Result<Option<usize>, ReadError> {
            self.host.record("focus_section");
            Ok(self.host.focus_section)
        }

        fn effect_track_values(
            &self,
            layer: usize,
            frame_start: usize,
            effect_position: usize,
            item_names: &[&str],
            frames: &[f64],
        ) -> Result<Vec<Vec<FiniteF64>>, ReadError> {
            self.host.record("effect_track_values");
            self.locate_effect(layer, frame_start, effect_position)?;
            self.host
                .evaluations
                .lock()
                .unwrap()
                .push(Evaluation::Track {
                    items: item_names.iter().map(|name| name.to_string()).collect(),
                    frames: frames.to_vec(),
                });
            item_names
                .iter()
                .map(|item_name| {
                    if self.host.values_unavailable_for.as_deref() == Some(item_name) {
                        return Err(ReadError::TrackValueUnavailable {
                            operation: "get_effect_track_value",
                        });
                    }
                    Ok(frames
                        .iter()
                        .map(|frame| FiniteF64::try_new(track_value_at(*frame)).expect("有限値"))
                        .collect())
                })
                .collect()
        }

        fn effect_check_values(
            &self,
            layer: usize,
            frame_start: usize,
            effect_position: usize,
            item_names: &[&str],
            frames: &[usize],
        ) -> Result<Vec<Vec<bool>>, ReadError> {
            self.host.record("effect_check_values");
            self.locate_effect(layer, frame_start, effect_position)?;
            self.host
                .evaluations
                .lock()
                .unwrap()
                .push(Evaluation::Check {
                    items: item_names.iter().map(|name| name.to_string()).collect(),
                    frames: frames.to_vec(),
                });
            item_names
                .iter()
                .map(|item_name| {
                    if self.host.values_unavailable_for.as_deref() == Some(item_name) {
                        return Err(ReadError::TrackValueUnavailable {
                            operation: "get_effect_check_value",
                        });
                    }
                    Ok(frames.iter().copied().map(check_value_at).collect())
                })
                .collect()
        }

        fn track_group_item_names(
            &self,
            layer: usize,
            frame_start: usize,
            effect_name: &str,
            effect_index: usize,
            group_name: &str,
        ) -> Result<Vec<String>, ReadError> {
            self.host.record("track_group_item_names");
            let object = self.find(layer, frame_start)?;
            if find_effect_position(&object.effects, effect_name, effect_index).is_none() {
                return Err(ReadError::Sdk {
                    operation: "get_object_track_group_names",
                });
            }
            Ok(self
                .host
                .group_item_names
                .iter()
                .find(|(name, _)| name == group_name)
                .map(|(_, names)| names.clone())
                .unwrap_or_default())
        }
    }

    impl FakeScene<'_> {
        /// 開始フレームで対象を引く。
        ///
        /// 対象そのものが読めない状況をここで再現する。同一性の材料を読む経路と
        /// 詳細を読む経路の双方が通る。
        fn find(&self, layer: usize, frame_start: usize) -> Result<&FakeObject, ReadError> {
            let object = self
                .host
                .layers
                .get(layer)
                .and_then(|fake| {
                    fake.objects
                        .iter()
                        .find(|object| object.identity.placement.frame_start == frame_start)
                })
                .ok_or(ReadError::ObjectNotFound {
                    detected_by: "find_object",
                })?;
            if self.host.object_read_fails_at == Some(frame_start) {
                return Err(ReadError::Sdk {
                    operation: "get_object_alias",
                });
            }
            Ok(object)
        }

        /// effect 列の位置で effect を引く。
        ///
        /// 実際の SDK はハンドルを列から引き当てる。位置が列の外なら値は取れない。
        fn locate_effect(
            &self,
            layer: usize,
            frame_start: usize,
            position: usize,
        ) -> Result<&HostEffect, ReadError> {
            self.find(layer, frame_start)?
                .effects
                .get(position)
                .ok_or(ReadError::Sdk {
                    operation: "get_effect_list",
                })
        }
    }

    /// 4 つのフィールドが揃った BPM 情報。
    fn sample_grid_bpm() -> GridBpm {
        GridBpm {
            tempo: FiniteF64::try_new(120.0).unwrap(),
            beat: 4,
            start: FiniteF64::try_new(1.5).unwrap(),
            offset: FiniteF64::try_new(0.25).unwrap(),
        }
    }

    fn fake_edit_info() -> HostEditInfo {
        HostEditInfo {
            scene_id: 0,
            width: 1920,
            height: 1080,
            fps_rate: 30000,
            fps_scale: 1001,
            sample_rate: 48000,
            cursor_frame: 12,
            cursor_layer: 1,
            frame_max: 3600,
            layer_max: 2,
            display_frame_start: 0,
            display_layer_start: 0,
            display_frame_num: 600,
            display_layer_num: 10,
            selected_range: Some(FrameRange { start: 10, end: 20 }),
        }
    }

    fn object(
        layer: usize,
        frame_start: usize,
        frame_end: usize,
        name: Option<&str>,
    ) -> FakeObject {
        FakeObject {
            identity: HostObject {
                placement: HostObjectPlacement {
                    layer,
                    frame_start,
                    frame_end,
                    name: name.map(str::to_string),
                },
                alias: format!("[{layer}:{frame_start}]"),
            },
            effects: Vec::new(),
        }
    }

    /// 配下 effect を持つオブジェクト。
    ///
    /// alias は配下 effect の設定値を含む。ホストが返す alias と同じ性質を
    /// 持たせなければ、effect を変えても対象の同一性が変わらないフェイクに
    /// なってしまう。
    fn object_with_effects(
        layer: usize,
        frame_start: usize,
        frame_end: usize,
        name: Option<&str>,
        effects: Vec<HostEffect>,
    ) -> FakeObject {
        let base = object(layer, frame_start, frame_end, name);
        FakeObject {
            identity: HostObject {
                alias: alias_with_effects(&base.identity.alias, &effects),
                ..base.identity
            },
            effects,
        }
    }

    fn fake_layers() -> Vec<FakeLayer> {
        vec![
            FakeLayer {
                name: Some("背景".to_string()),
                enabled: true,
                locked: false,
                objects: vec![object(0, 0, 99, None)],
            },
            FakeLayer {
                name: None,
                enabled: true,
                locked: true,
                objects: vec![
                    object_with_effects(1, 100, 200, Some("立ち絵"), fake_effects()),
                    object(1, 300, 400, Some("字幕")),
                ],
            },
            FakeLayer {
                name: Some("効果".to_string()),
                enabled: false,
                locked: false,
                objects: Vec::new(),
            },
        ]
    }

    /// ファイルの設定項目を 1 つ持つ effect。
    fn file_effect(name: &str, index: usize, path: &str) -> HostEffect {
        HostEffect {
            name: name.to_string(),
            index,
            enabled: true,
            locked: false,
            items: vec![EffectItem {
                name: "ファイル".to_string(),
                item_type: EffectItemType::File,
                value: ItemValue::File {
                    path: path.to_string(),
                },
                track: None,
            }],
        }
    }

    fn fake_effects() -> Vec<HostEffect> {
        vec![file_effect("動画ファイル", 0, r"C:\movie.mp4")]
    }

    /// テスト用のカタログ 1 件。
    ///
    /// 見出しと設定項目の数を別に保つ。読み取り経路がそれぞれを別の呼び出しで
    /// 返すため、フェイクも同じ形で保持する。
    #[derive(Debug, Clone)]
    struct FakeCatalogEntry {
        summary: HostEffectSummary,
        items: Vec<AvailableEffectItem>,
    }

    /// 件数から設定項目の定義を作る。
    ///
    /// 名前に effect 名を含めてあり、別の effect の項目を返した実装は結果に
    /// 現れる。
    ///
    /// **番号は隣り合う 2 つを入れ替えて振る。** 番号を並びのまま振ると、名前の
    /// 昇順と列挙順が一致し、名前で並べ替える実装が恒等になって検査を素通りする。
    /// 見栄えのために名前順へ整えるのは、列挙順を約束する応答にとって最も
    /// 起こりやすい取り違えである。入れ替えた並びは昇順とも降順とも一致しない。
    ///
    /// 種別は列挙の位置で決める。名前だけを並べ替えた実装では、名前と種別の
    /// 対応が崩れて結果に現れる。
    fn catalog_items(effect: &str, count: usize) -> Vec<AvailableEffectItem> {
        (0..count)
            .map(|index| AvailableEffectItem {
                name: format!("{effect}の項目{}", swapped_number(index, count)),
                item_type: if index.is_multiple_of(2) {
                    EffectItemType::Integer
                } else {
                    EffectItemType::Check
                },
            })
            .collect()
    }

    /// 隣り合う 2 つを入れ替えた番号を返す。
    ///
    /// 件数が奇数のときの末尾は相手が無いためそのまま残る。件数 1 では入れ替え
    /// が起きず、並びの検査もできない——並びを見る対象には 2 件以上を使う。
    fn swapped_number(index: usize, count: usize) -> usize {
        if index.is_multiple_of(2) {
            if index + 1 < count { index + 1 } else { index }
        } else {
            index - 1
        }
    }

    fn catalog_entry(
        name: &str,
        effect_type: EffectType,
        flags: u32,
        item_count: usize,
    ) -> FakeCatalogEntry {
        FakeCatalogEntry {
            summary: HostEffectSummary {
                name: name.to_string(),
                effect_type,
                flags: EffectFlags::from_raw(flags),
            },
            items: catalog_items(name, item_count),
        }
    }

    /// ホストが同梱する説明の 1 節を組み立てる。
    ///
    /// 効果の説明と項目の説明を別の引数で受ける。片方を他方へ渡す取り違えは、
    /// 組み立ての時点では起こり得ない形にしてある。
    fn effect_help(description: Option<&str>, items: &[(&str, &str)]) -> HostEffectHelp {
        HostEffectHelp {
            description: description.map(str::to_string),
            items: items
                .iter()
                .map(|(name, text)| (name.to_string(), text.to_string()))
                .collect(),
        }
    }

    /// effect 1 件分の面を組み立てる。
    fn effect_facets(items: &[(&str, ItemFacets)]) -> HostEffectFacets {
        HostEffectFacets {
            items: items
                .iter()
                .map(|(name, facets)| ((*name).to_string(), facets.clone()))
                .collect(),
        }
    }

    /// 候補だけを持つ面の組。
    ///
    /// 由来を項目ごとに指定する。同じ effect の中に基底の候補とサイドカーの
    /// 候補が混ざる形は、表を重ねれば普通に起こる。
    fn choices_facet(values: &[&str], source: TableSource) -> ItemFacets {
        ItemFacets {
            choices: Some(ItemChoices {
                values: values.iter().map(|value| (*value).to_string()).collect(),
                source,
            }),
            range: None,
        }
    }

    /// 値域だけを持つ面の組。
    fn range_facet(
        min: Option<f64>,
        max: Option<f64>,
        decimals: Option<u32>,
        source: TableSource,
    ) -> ItemFacets {
        ItemFacets {
            choices: None,
            range: Some(ItemRange {
                min: min.and_then(FiniteF64::try_new),
                max: max.and_then(FiniteF64::try_new),
                decimals,
                source,
            }),
        }
    }

    /// 位置と所属アイテム名からグループを組み立てる。
    fn item_group(index: usize, item_names: &[&str]) -> ItemGroup {
        ItemGroup {
            index,
            item_names: item_names.iter().map(|name| (*name).to_string()).collect(),
        }
    }

    /// 設定項目 1 件がグループに属するときのフェイクの答え。
    fn member_group(
        effect: &str,
        item: &str,
        index: usize,
        item_names: &[&str],
    ) -> ((String, String), FakeItemGroup) {
        (
            (effect.to_string(), item.to_string()),
            FakeItemGroup::Member(item_group(index, item_names)),
        )
    }

    /// グローの 2 件が成すグループの所属アイテム名。
    ///
    /// **設定項目の列挙順と並びを変えてある。** グループ内の位置を列挙順から
    /// 作った実装は、ホストが返した位置と食い違って結果に現れる。
    const GLOW_AXES: [&str; 2] = ["グローの項目0", "グローの項目1"];

    /// グローだけがグループを持ち、その中でも属さない項目が残る答え。
    ///
    /// 別々のグループを 2 つ置く。どちらの位置も列挙順とは一致しない。
    fn mixed_groups() -> Vec<((String, String), FakeItemGroup)> {
        vec![
            member_group("グロー", "グローの項目1", 1, &GLOW_AXES),
            member_group("グロー", "グローの項目0", 0, &GLOW_AXES),
            member_group("グロー", "グローの項目3", 0, &["グローの項目3"]),
        ]
    }

    /// フェイクの effect カタログ。
    ///
    /// 種別ごとに複数件を並べる。1 件ずつしか無いと、種別で絞ってから窓を切る
    /// 順序と、窓を切ってから絞る順序の区別が付かない。
    fn fake_catalog() -> Vec<FakeCatalogEntry> {
        vec![
            catalog_entry("ぼかし", EffectType::Filter, 1, 1),
            catalog_entry("動画ファイル", EffectType::Input, 3, 0),
            catalog_entry("グロー", EffectType::Filter, 1, 4),
            catalog_entry("画像ファイル", EffectType::Input, 1, 2),
            catalog_entry("標準描画", EffectType::Output, 1, 6),
        ]
    }

    fn fake_fonts() -> Vec<String> {
        vec![
            "MS UI Gothic".to_string(),
            "游ゴシック".to_string(),
            "Segoe UI".to_string(),
        ]
    }

    fn fake_modules() -> Vec<ModuleEntry> {
        vec![
            ModuleEntry {
                module_type: ModuleType::ScriptObject,
                name: "テキスト".to_string(),
                information: "標準搭載のオブジェクトスクリプト".to_string(),
            },
            ModuleEntry {
                module_type: ModuleType::PluginInput,
                name: "入力プラグイン".to_string(),
                information: "動画の読み込み".to_string(),
            },
            ModuleEntry {
                module_type: ModuleType::PluginOutput,
                name: "出力プラグイン".to_string(),
                information: "動画の書き出し".to_string(),
            },
        ]
    }

    fn fake_palette_names() -> Vec<String> {
        vec![
            "既定".to_string(),
            "暖色".to_string(),
            "寒色".to_string(),
            "単色".to_string(),
        ]
    }

    /// パレット名から色を作る。
    ///
    /// 名前ごとに違う色にしてあり、別のパレットの色を返した実装は結果に現れる。
    fn fake_palette_colors(name: &str) -> Vec<Rgba> {
        let seed = name.chars().count() as u8;
        (0..PALETTE_COLOR_COUNT)
            .map(|index| Rgba {
                r: seed,
                g: index as u8,
                b: 0,
                a: 255,
            })
            .collect()
    }

    impl<H: ReadHost> HostReadAdapter<H> {
        /// 既定のページ要求でオブジェクトを列挙する。
        ///
        /// フェイクの対象数は既定の 1 ページに収まる。
        fn list_objects_page(
            &self,
            expected_scene_id: i32,
            filter: Option<&ObjectFilter>,
        ) -> Result<Page<ObjectSummary>, ReadError> {
            self.list_objects(expected_scene_id, filter, &default_page_request())
                .map(|page| page.expect("既定のページ要求が拒否されました"))
        }

        /// 既定のページ要求で選択状態を取得する。
        fn get_selection_page(
            &self,
            expected_scene_id: i32,
        ) -> Result<SelectionSnapshot, ReadError> {
            self.get_selection(expected_scene_id, &default_page_request())
                .map(|snapshot| snapshot.expect("既定のページ要求が拒否されました"))
        }

        /// 既定のページ要求でパレットを列挙する。
        fn list_palettes_page(&self) -> Result<ListPalettesResult, ReadError> {
            self.list_palettes(&default_page_window())
        }

        /// 既定のページ要求で effect を列挙する。
        fn list_available_effects_page(
            &self,
            effect_type: Option<&EffectType>,
        ) -> Result<ListAvailableEffectsResult, ReadError> {
            self.list_available_effects(effect_type, &default_page_window())
        }
    }

    /// 検証を通した effect の中身の要求を組み立てる。
    fn describe_params(names: &[&str]) -> DescribeEffectsParams {
        let params = DescribeEffectsParams {
            effect_names: names.iter().map(|name| name.to_string()).collect(),
        };
        params.validate().expect("要求内容の検証を通る");
        params
    }

    /// adapter とプロジェクト状態を組み立てる。
    fn adapter_with(
        host: impl FnOnce(&Arc<ProjectState>) -> FakeHost,
    ) -> HostReadAdapter<FakeHost> {
        let project = Arc::new(ProjectState::new());
        let host = host(&project);
        HostReadAdapter::new(host, project)
    }

    fn adapter() -> HostReadAdapter<FakeHost> {
        adapter_with(|_| FakeHost::new())
    }

    /// 全 read operation を 1 度ずつ実行し、エラーコードを集める。
    fn error_codes_of_all_operations(adapter: &HostReadAdapter<FakeHost>) -> Vec<ErrorCode> {
        let selector = sample_selector(adapter);
        vec![
            adapter.get_edit_info().err().map(|e| e.error_code()),
            adapter.get_current_scene().err().map(|e| e.error_code()),
            adapter.list_layers(0).err().map(|e| e.error_code()),
            adapter
                .list_objects_page(0, None)
                .err()
                .map(|e| e.error_code()),
            adapter.get_object(&selector).err().map(|e| e.error_code()),
            adapter
                .list_available_effects_page(None)
                .err()
                .map(|e| e.error_code()),
            adapter
                .describe_effects(&describe_params(&["ぼかし"]))
                .err()
                .map(|e| e.error_code()),
            adapter
                .get_effect_item_values(&item_values_params(
                    sample_effect_selector(adapter),
                    &[100.0],
                    None,
                ))
                .err()
                .map(|e| e.error_code()),
            adapter.get_selection_page(0).err().map(|e| e.error_code()),
            adapter.list_fonts().err().map(|e| e.error_code()),
            adapter.list_palettes_page().err().map(|e| e.error_code()),
            adapter.list_modules(None).err().map(|e| e.error_code()),
        ]
        .into_iter()
        .map(|code| code.expect("成功してしまいました"))
        .collect()
    }

    /// レイヤー 1・フレーム 100 のオブジェクトを指すセレクター。
    ///
    /// 材料はホストが保持する値から採る。alias は配下 effect の設定値を含むため、
    /// 位置と名前だけを写した複製からは正しい値を組み立てられない。
    fn sample_selector(adapter: &HostReadAdapter<FakeHost>) -> ObjectSelector {
        let object = fake_layers()[1].objects[0].clone();
        object_summary(&adapter.project.epoch(), 0, &object.identity).selector
    }

    /// ホストが保持するオブジェクトの配下 effect を指すセレクター。
    ///
    /// fingerprint は effect 列の位置と総数まで含めて算出されるため、列そのもの
    /// から組み立てる。
    fn effect_selector_of(
        adapter: &HostReadAdapter<FakeHost>,
        effect_name: &str,
        effect_index: usize,
    ) -> EffectSelector {
        let object = adapter.host.layers[1].objects[0].clone();
        let summary = object_summary(&adapter.project.epoch(), 0, &object.identity);
        let position = find_effect_position(&object.effects, effect_name, effect_index)
            .expect("effect が見つかりません");
        effect_info_at(&summary.selector, &object.effects, position)
            .expect("effect の情報を組み立てられません")
            .selector
    }

    /// 既定のフェイクが持つ effect を指すセレクター。
    fn sample_effect_selector(adapter: &HostReadAdapter<FakeHost>) -> EffectSelector {
        effect_selector_of(adapter, "動画ファイル", 0)
    }

    /// 補間後の値の要求を組み立てる。
    fn item_values_params(
        effect: EffectSelector,
        frames: &[f64],
        items: Option<&[&str]>,
    ) -> GetEffectItemValuesParams {
        GetEffectItemValuesParams {
            effect,
            frames: frames
                .iter()
                .map(|frame| FiniteF64::try_new(*frame).expect("有限値"))
                .collect(),
            items: items.map(|names| names.iter().map(|name| name.to_string()).collect()),
        }
    }

    /// 移動を持つトラックバーの設定項目を作る。
    ///
    /// **移動情報と移動を含む値は対で現れる。** ホストは移動方法の名前を持たない
    /// トラックバーに対して移動情報を返さないため、片方だけを持つ項目は実機では
    /// 起こらない。値を数値のままにすると、実機には無い組み合わせの上で読み取りを
    /// 検査することになる。
    ///
    /// `group` はグループ名・グループ内の位置・グループのトラック数の組である。
    fn track_item(name: &str, group: Option<(&str, usize, usize)>) -> EffectItem {
        EffectItem {
            name: name.to_string(),
            item_type: EffectItemType::Number,
            value: ItemValue::Track(TrackValue {
                values: [0.0, 100.0]
                    .iter()
                    .map(|value| FiniteF64::try_new(*value).expect("有限値"))
                    .collect(),
                mode: Some(MOVEMENT_MODE.to_string()),
                params: Vec::new(),
                accelerate: false,
                decelerate: false,
                twopoint: false,
                reserved_flags: 0,
            }),
            track: Some(TrackInfo {
                mode: MOVEMENT_MODE.to_string(),
                params: Vec::new(),
                accelerate: false,
                decelerate: false,
                twopoint: false,
                timecontrol: false,
                group_num: group.map(|(_, _, count)| count).unwrap_or(1),
                group_index: group.map(|(_, index, _)| index).unwrap_or(0),
                group_name: group.map(|(name, _, _)| name.to_string()),
            }),
        }
    }

    /// 移動を持たないトラックバーの設定項目を作る。
    ///
    /// 値は区間の数に依らず 1 つであり、移動情報は返らない。**所属グループも
    /// 移動情報の中にしか無いため、移動を持たない項目からは読めない。**
    fn static_track_item(name: &str) -> EffectItem {
        EffectItem {
            name: name.to_string(),
            item_type: EffectItemType::Number,
            value: ItemValue::Number {
                value: FiniteF64::try_new(0.0).expect("有限値"),
            },
            track: None,
        }
    }

    /// フェイクの項目が名乗る移動方法。
    const MOVEMENT_MODE: &str = "直線移動";

    /// チェックボックスの設定項目を作る。
    fn check_item(name: &str) -> EffectItem {
        EffectItem {
            name: name.to_string(),
            item_type: EffectItemType::Check,
            value: ItemValue::Bool { value: false },
            track: None,
        }
    }

    /// 任意フレームでの値を持たない設定項目を作る。
    fn text_item(name: &str) -> EffectItem {
        EffectItem {
            name: name.to_string(),
            item_type: EffectItemType::Text,
            value: ItemValue::Text {
                value: "字幕".to_string(),
            },
            track: None,
        }
    }

    /// 評価できる項目と評価できない項目を混ぜた effect。
    ///
    /// X と Y は同じグループに属する移動を持つ項目、拡大率は移動を持たない項目で
    /// ある。**移動の有無は同じ種別の中で分かれる。** 片方だけを置くと、種別で
    /// 判定する実装と移動の有無で判定する実装を見分けられない。
    fn mixed_effect() -> HostEffect {
        HostEffect {
            name: "標準描画".to_string(),
            index: 0,
            enabled: true,
            locked: false,
            items: vec![
                track_item("X", Some((TRACK_GROUP, 0, 3))),
                track_item("Y", Some((TRACK_GROUP, 1, 3))),
                static_track_item("拡大率"),
                check_item("反転"),
                text_item("説明"),
            ],
        }
    }

    /// トラックバーグループの名前。
    const TRACK_GROUP: &str = "座標";

    /// 混ぜた effect を持つ対象と、そのグループの所属アイテム名を備えた adapter。
    ///
    /// グループのトラック数は 3 なのに所属アイテム名は 2 件である。両者が一致
    /// しないことをフェイクの既定に据え、一致を前提にした実装がここで落ちる。
    fn mixed_adapter() -> HostReadAdapter<FakeHost> {
        adapter_with(|_| FakeHost {
            group_item_names: vec![(
                TRACK_GROUP.to_string(),
                vec!["X".to_string(), "Y".to_string()],
            )],
            ..host_with_effects(vec![mixed_effect()])
        })
    }

    /// 評価した項目の名前を並べる。
    fn evaluated_names(values: &EffectItemValues) -> Vec<&str> {
        values
            .items
            .iter()
            .map(|item| match item {
                EvaluatedItem::Track { name, .. } | EvaluatedItem::Check { name, .. } => {
                    name.as_str()
                }
            })
            .collect()
    }

    /// トラックバー項目として評価された値を取り出す。
    fn track_values<'a>(values: &'a EffectItemValues, item_name: &str) -> &'a [FiniteF64] {
        values
            .items
            .iter()
            .find_map(|item| match item {
                EvaluatedItem::Track { name, values, .. } if name == item_name => Some(&values[..]),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{item_name} がトラックバーとして返っていません"))
    }

    /// トラックバー項目のグループを取り出す。
    fn group_of<'a>(values: &'a EffectItemValues, item_name: &str) -> Option<&'a TrackGroup> {
        values
            .items
            .iter()
            .find_map(|item| match item {
                EvaluatedItem::Track { name, group, .. } if name == item_name => Some(group),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{item_name} がトラックバーとして返っていません"))
            .as_ref()
    }

    #[test]
    fn evaluated_values_follow_the_requested_frames_in_order() {
        // 値は位置で対応付ける。並びが崩れると別のフレームの値を読むことになる。
        let adapter = mixed_adapter();
        let frames = [150.0, 120.5, 199.0];
        let values = adapter
            .get_effect_item_values(&item_values_params(
                effect_selector_of(&adapter, "標準描画", 0),
                &frames,
                Some(&["X"]),
            ))
            .expect("評価できます");

        assert_eq!(
            values.frames.iter().map(FiniteF64::get).collect::<Vec<_>>(),
            frames.to_vec(),
            "要求したフレームがそのまま返っていません"
        );
        assert_eq!(
            track_values(&values, "X")
                .iter()
                .map(FiniteF64::get)
                .collect::<Vec<_>>(),
            frames.map(track_value_at).to_vec(),
            "値の並びが要求したフレームの並びと違います"
        );
    }

    #[test]
    fn a_fractional_frame_keeps_its_fraction_for_a_track_and_loses_it_for_a_check() {
        // 小数部はフレーム間の位置を指す。トラックバーへ丸めて渡すと中間点の間を
        // 問えなくなる。チェックボックスは整数フレームしか取らない。
        let adapter = mixed_adapter();
        adapter
            .get_effect_item_values(&item_values_params(
                effect_selector_of(&adapter, "標準描画", 0),
                &[120.5, 130.75],
                Some(&["X", "反転"]),
            ))
            .expect("評価できます");

        assert_eq!(
            adapter.host.evaluations(),
            vec![
                Evaluation::Track {
                    items: vec!["X".to_string()],
                    frames: vec![120.5, 130.75],
                },
                Evaluation::Check {
                    items: vec!["反転".to_string()],
                    frames: vec![120, 130],
                },
            ]
        );
    }

    #[test]
    fn the_effect_is_resolved_once_per_kind_not_once_per_item() {
        // 参照区間の内側ではハンドルが有効であり続ける。項目ごとに effect を
        // 引き直すと、対象の解決が項目数に比例する。
        let adapter = mixed_adapter();
        adapter
            .get_effect_item_values(&item_values_params(
                effect_selector_of(&adapter, "標準描画", 0),
                &[120.0],
                Some(&["X", "Y", "拡大率", "反転"]),
            ))
            .expect("評価できます");

        let evaluations = adapter.host.evaluations();
        assert_eq!(evaluations.len(), 2, "{evaluations:?}");
        assert_eq!(
            evaluations[0],
            Evaluation::Track {
                items: vec!["X".to_string(), "Y".to_string(), "拡大率".to_string()],
                frames: vec![120.0],
            }
        );
        assert_eq!(
            evaluations[1],
            Evaluation::Check {
                items: vec!["反転".to_string()],
                frames: vec![120],
            }
        );
    }

    #[test]
    fn a_missing_effect_a_missing_item_a_wrong_kind_and_a_refused_value_are_four_answers() {
        // effect が無い・項目名が誤っている・種別が違う・値が返らないは、要求元が
        // 次に取る行動がそれぞれ違う。畳むと切り分けられない。
        let adapter = mixed_adapter();
        let selector = effect_selector_of(&adapter, "標準描画", 0);
        let missing_effect = adapter
            .get_effect_item_values(&item_values_params(
                EffectSelector {
                    effect_name: "存在しない効果".to_string(),
                    ..selector.clone()
                },
                &[120.0],
                Some(&["X"]),
            ))
            .expect_err("存在しない effect が受理されました");
        let missing_item = adapter
            .get_effect_item_values(&item_values_params(
                selector.clone(),
                &[120.0],
                Some(&["存在しない項目"]),
            ))
            .expect_err("存在しない項目名が受理されました");
        let wrong_kind = adapter
            .get_effect_item_values(&item_values_params(
                selector.clone(),
                &[120.0],
                Some(&["説明"]),
            ))
            .expect_err("評価できない種別が受理されました");

        let refusing = adapter_with(|_| FakeHost {
            values_unavailable_for: Some("X".to_string()),
            group_item_names: vec![(
                TRACK_GROUP.to_string(),
                vec!["X".to_string(), "Y".to_string()],
            )],
            ..host_with_effects(vec![mixed_effect()])
        });
        let refused = refusing
            .get_effect_item_values(&item_values_params(
                effect_selector_of(&refusing, "標準描画", 0),
                &[120.0],
                Some(&["X"]),
            ))
            .expect_err("値が返らないのに成功しました");

        let answers: Vec<(ErrorCode, serde_json::Value)> =
            [&missing_effect, &missing_item, &wrong_kind, &refused]
                .into_iter()
                .map(|error| (error.error_code(), error.details()["reason"].clone()))
                .collect();
        assert_eq!(
            answers,
            vec![
                (ErrorCode::NotFound, serde_json::json!("target_missing")),
                (ErrorCode::NotFound, serde_json::json!("item_not_found")),
                (
                    ErrorCode::UnsupportedOperation,
                    serde_json::json!("item_not_evaluatable")
                ),
                (
                    ErrorCode::SdkError,
                    serde_json::json!("track_value_unavailable")
                ),
            ]
        );
        let distinct: std::collections::BTreeSet<String> =
            answers.iter().map(|answer| format!("{answer:?}")).collect();
        assert_eq!(
            distinct.len(),
            answers.len(),
            "同じ応答になった失敗があります"
        );
    }

    #[test]
    fn omitting_the_item_names_selects_every_evaluatable_item() {
        // 評価できない種別は現れない。要求元が effect の項目名を知らなくても
        // 「評価できるものを全部」と言えるようにする。
        let adapter = mixed_adapter();
        let values = adapter
            .get_effect_item_values(&item_values_params(
                effect_selector_of(&adapter, "標準描画", 0),
                &[120.0],
                None,
            ))
            .expect("評価できます");

        assert_eq!(evaluated_names(&values), vec!["X", "Y", "拡大率", "反転"]);
        assert!(!values.truncated);
    }

    #[test]
    fn omitting_the_item_names_truncates_at_the_limit() {
        // 項目数が上限を超える effect はあり得る。黙って落とさず、打ち切った
        // ことを伝える。
        for (count, expected, truncated) in [
            (MAX_EVALUATED_ITEMS - 1, MAX_EVALUATED_ITEMS - 1, false),
            (MAX_EVALUATED_ITEMS, MAX_EVALUATED_ITEMS, false),
            (MAX_EVALUATED_ITEMS + 1, MAX_EVALUATED_ITEMS, true),
        ] {
            let effect = HostEffect {
                items: (0..count)
                    .map(|i| track_item(&format!("項目{i}"), None))
                    .collect(),
                ..mixed_effect()
            };
            let adapter = adapter_with(|_| host_with_effects(vec![effect]));
            let values = adapter
                .get_effect_item_values(&item_values_params(
                    effect_selector_of(&adapter, "標準描画", 0),
                    &[120.0],
                    None,
                ))
                .expect("評価できます");

            assert_eq!(values.items.len(), expected, "{count} 件の effect");
            assert_eq!(values.truncated, truncated, "{count} 件の effect");
        }
    }

    #[test]
    fn a_group_is_returned_with_both_counts_even_when_they_disagree() {
        // グループのトラック数と所属アイテム名の件数が同じであるとは定められて
        // いない。一致を強制せず、両方を返して要求元に見せる。
        let adapter = mixed_adapter();
        let values = adapter
            .get_effect_item_values(&item_values_params(
                effect_selector_of(&adapter, "標準描画", 0),
                &[120.0],
                Some(&["X", "Y", "拡大率"]),
            ))
            .expect("件数が食い違っても失敗しません");

        let group = group_of(&values, "X").expect("グループに属します");
        assert_eq!(group.name, TRACK_GROUP);
        assert_eq!(group.index, 0);
        assert_eq!(group.count, 3);
        assert_eq!(group.item_names, vec!["X".to_string(), "Y".to_string()]);
        assert_ne!(group.count, group.item_names.len());
        assert_eq!(group_of(&values, "Y").expect("グループに属します").index, 1);
        assert_eq!(
            group_of(&values, "拡大率"),
            None,
            "グループに属さない項目がグループを名乗りました"
        );
        assert_eq!(
            adapter
                .host
                .calls()
                .iter()
                .filter(|call| **call == "track_group_item_names")
                .count(),
            1,
            "同じグループの所属アイテム名を引き直しています"
        );
    }

    #[test]
    fn a_group_that_the_host_does_not_know_is_not_a_failure() {
        // 所属アイテム名が 0 件で返るのは「指定グループが無い」であって失敗
        // ではない。
        let adapter = adapter_with(|_| host_with_effects(vec![mixed_effect()]));
        let values = adapter
            .get_effect_item_values(&item_values_params(
                effect_selector_of(&adapter, "標準描画", 0),
                &[120.0],
                Some(&["X"]),
            ))
            .expect("0 件でも失敗しません");
        assert!(
            group_of(&values, "X")
                .expect("グループに属します")
                .item_names
                .is_empty()
        );
    }

    #[test]
    fn a_frame_outside_the_object_is_a_precondition_failure() {
        // フレームはシーンの絶対フレーム番号である。対象の外を指す要求には
        // 補間する対象そのものが無い。
        let adapter = mixed_adapter();
        for frame in [99.0, 200.5, 300.0] {
            let error = adapter
                .get_effect_item_values(&item_values_params(
                    effect_selector_of(&adapter, "標準描画", 0),
                    &[120.0, frame],
                    Some(&["X"]),
                ))
                .unwrap_err();
            assert_eq!(
                error.error_code(),
                ErrorCode::PreconditionFailed,
                "フレーム {frame}"
            );
            assert_eq!(error.details()["reason"], "frame_out_of_range");
        }
        assert!(
            adapter.host.evaluations().is_empty(),
            "範囲外のまま値を読みに行きました"
        );
        // 端は含む。オブジェクトが占めるフレームは開始から終了までである。
        for frame in [100.0, 200.0] {
            assert!(
                adapter
                    .get_effect_item_values(&item_values_params(
                        effect_selector_of(&adapter, "標準描画", 0),
                        &[frame],
                        Some(&["X"]),
                    ))
                    .is_ok(),
                "端のフレーム {frame} が拒否されました"
            );
        }
    }

    #[test]
    fn an_unknown_effect_and_a_stale_effect_fingerprint_are_told_apart() {
        let adapter = mixed_adapter();
        let selector = effect_selector_of(&adapter, "標準描画", 0);

        let unknown = adapter
            .get_effect_item_values(&item_values_params(
                EffectSelector {
                    effect_name: "存在しない効果".to_string(),
                    ..selector.clone()
                },
                &[120.0],
                Some(&["X"]),
            ))
            .expect_err("存在しない effect が受理されました");
        assert_eq!(unknown.error_code(), ErrorCode::NotFound);
        assert_eq!(unknown.details()["reason"], "target_missing");

        let stale = adapter
            .get_effect_item_values(&item_values_params(
                EffectSelector {
                    fingerprint: sample_selector(&adapter).fingerprint,
                    ..selector
                },
                &[120.0],
                Some(&["X"]),
            ))
            .expect_err("古い fingerprint が受理されました");
        assert_eq!(stale.error_code(), ErrorCode::PreconditionFailed);
    }

    #[test]
    fn the_response_carries_neither_a_handle_nor_an_alias() {
        // 値そのものは載せるが、対象を指す内部の値と alias は載せない。
        let adapter = mixed_adapter();
        let values = adapter
            .get_effect_item_values(&item_values_params(
                effect_selector_of(&adapter, "標準描画", 0),
                &[120.0],
                None,
            ))
            .expect("評価できます");
        let json = serde_json::to_string(&values).expect("直列化できます");
        for forbidden in ["alias", "handle", "selector", "0x"] {
            assert!(
                !json.contains(forbidden),
                "{forbidden} が現れました: {json}"
            );
        }
    }

    #[test]
    fn not_ready_rejects_every_operation_without_touching_sdk() {
        let adapter = adapter_with(|_| FakeHost {
            ready: false,
            ..FakeHost::new()
        });

        for code in error_codes_of_all_operations(&adapter) {
            assert_eq!(code, ErrorCode::HostBusy);
        }
        assert!(
            adapter.host.calls().is_empty(),
            "準備前に SDK を呼び出しました: {:?}",
            adapter.host.calls()
        );
    }

    #[test]
    fn not_ready_advises_retry() {
        let adapter = adapter_with(|_| FakeHost {
            ready: false,
            ..FakeHost::new()
        });
        let error = adapter.get_edit_info().unwrap_err();
        assert!(error.retryable());
        assert!(error.retry_after_ms().is_some());
    }

    #[test]
    fn preview_and_save_are_edit_blocked_without_entering_read_section() {
        for state in [EditState::Preview, EditState::Save] {
            let adapter = adapter_with(|_| FakeHost {
                state,
                ..FakeHost::new()
            });

            for code in error_codes_of_all_operations(&adapter) {
                assert_eq!(
                    code,
                    ErrorCode::EditBlocked,
                    "{state} で拒否されませんでした"
                );
            }
            assert!(
                !adapter.host.calls().contains(&"enter_read_section"),
                "{state} で参照区間へ入りました: {:?}",
                adapter.host.calls()
            );
            assert!(
                !adapter.host.calls().contains(&"edit_info"),
                "{state} で編集情報を取得しました"
            );
        }
    }

    #[test]
    fn edit_blocked_reports_current_state() {
        let adapter = adapter_with(|_| FakeHost {
            state: EditState::Save,
            ..FakeHost::new()
        });
        let error = adapter.get_edit_info().unwrap_err();
        assert_eq!(error.details()["edit_state"], "save");
        assert!(error.retryable());
    }

    #[test]
    fn a_failed_edit_info_call_is_told_apart_from_an_out_of_range_value() {
        // 読み取り経路にも同じ切り分けが要る。片方の経路だけを直すと、同じ
        // 壊れ方が呼び出し口によって別の応答になる。
        let call_failed = adapter_with(|_| FakeHost {
            edit_info_failure: Some(EditInfoFailure::CallFailed),
            ..FakeHost::new()
        });
        let call_error = call_failed.get_edit_info().unwrap_err();
        let call_details = call_error.details();
        assert_eq!(call_error.error_code(), ErrorCode::SdkError);
        assert_eq!(call_details["sdk_operation"], "get_edit_info");
        assert!(
            call_details.get("reason").is_none(),
            "呼び出しの失敗に名前が付きました: {call_details}"
        );

        let out_of_range = adapter_with(|_| FakeHost {
            edit_info_failure: Some(EditInfoFailure::OutOfRange),
            ..FakeHost::new()
        });
        let value_error = out_of_range.get_edit_info().unwrap_err();
        let value_details = value_error.details();
        assert_eq!(value_error.error_code(), call_error.error_code());
        assert_eq!(
            value_details["sdk_operation"],
            call_details["sdk_operation"]
        );
        assert_eq!(value_details["reason"], "edit_info_out_of_range");
    }

    #[test]
    fn guard_converts_panic_into_internal_error() {
        let error = with_silent_panic_hook(|| {
            guard::<()>(|| panic!("参照区間の内側で panic させます")).unwrap_err()
        });
        assert_eq!(error.error_code(), ErrorCode::InternalError);
    }

    #[test]
    fn guard_passes_through_success_and_failure() {
        assert_eq!(guard(|| Ok(7)).unwrap(), 7);
        let error = guard::<()>(|| {
            Err(ReadError::ObjectNotFound {
                detected_by: "find_object",
            })
        })
        .unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::NotFound);
    }

    #[test]
    fn panic_inside_read_section_becomes_internal_error() {
        let adapter = adapter_with(|_| FakeHost {
            panic_at: Some(PanicPoint::SceneName),
            ..FakeHost::new()
        });

        let error = with_silent_panic_hook(|| adapter.get_edit_info().unwrap_err());
        assert_eq!(error.error_code(), ErrorCode::InternalError);
        assert!(adapter.host.calls().contains(&"enter_read_section"));
    }

    #[test]
    fn panic_inside_object_lookup_becomes_internal_error() {
        let adapter = adapter_with(|_| FakeHost {
            panic_at: Some(PanicPoint::ObjectPlacements),
            ..FakeHost::new()
        });
        let selector = sample_selector(&adapter);

        let error = with_silent_panic_hook(|| adapter.get_object(&selector).unwrap_err());
        assert_eq!(error.error_code(), ErrorCode::InternalError);
    }

    #[test]
    fn panic_entering_the_read_section_becomes_internal_error() {
        // 参照区間へ入る呼び出しは、渡すクロージャを包んでも捕捉できない位置で
        // 落ち得る。捕捉しなければ接続の境界まで巻き戻り、要求元は応答ではなく
        // 切断を観測する。
        let adapter = adapter_with(|_| FakeHost {
            panic_at: Some(PanicPoint::EnterSection),
            ..FakeHost::new()
        });
        let selector = sample_selector(&adapter);

        with_silent_panic_hook(|| {
            for error in [
                adapter.get_edit_info().unwrap_err(),
                adapter.get_current_scene().unwrap_err(),
                adapter.list_layers(0).unwrap_err(),
                adapter.list_objects_page(0, None).unwrap_err(),
                adapter.get_object(&selector).unwrap_err(),
            ] {
                assert_eq!(error.error_code(), ErrorCode::InternalError);
            }
        });
    }

    #[test]
    fn catch_returns_the_value_without_flattening() {
        assert_eq!(catch(|| 7).unwrap(), 7);
        let error = with_silent_panic_hook(|| {
            catch::<()>(|| panic!("参照区間へ入る呼び出しで panic させます")).unwrap_err()
        });
        assert_eq!(error.error_code(), ErrorCode::InternalError);
    }

    #[test]
    fn panic_while_asking_readiness_becomes_internal_error() {
        // 受付判定の最初の一手も捕捉層の内側に置く。ここだけ素通しにすると、
        // 準備状態の問い合わせが落ちた場合に限って接続の境界まで巻き戻る。
        let adapter = adapter_with(|_| FakeHost {
            panic_at: Some(PanicPoint::IsReady),
            ..FakeHost::new()
        });

        with_silent_panic_hook(|| {
            for code in error_codes_of_all_operations(&adapter) {
                assert_eq!(code, ErrorCode::InternalError);
            }
        });
    }

    #[test]
    fn panic_outside_the_read_section_becomes_internal_error() {
        // 編集情報の取得はフレームレートの分母が 0 のとき panic する。参照区間の
        // 外で起きるため、捕捉しなければ接続の境界まで巻き戻り、応答を返さない
        // まま切断される。
        let adapter = adapter_with(|_| FakeHost {
            panic_at: Some(PanicPoint::EditInfo),
            ..FakeHost::new()
        });

        with_silent_panic_hook(|| {
            for error in [
                adapter.get_edit_info().unwrap_err(),
                adapter.get_current_scene().unwrap_err(),
                adapter.list_layers(0).unwrap_err(),
                adapter.list_objects_page(0, None).unwrap_err(),
            ] {
                assert_eq!(error.error_code(), ErrorCode::InternalError);
            }
        });
        assert!(
            !adapter.host.calls().contains(&"enter_read_section"),
            "編集情報を取得できないまま参照区間へ入りました"
        );
    }

    #[test]
    fn section_failure_during_playback_is_reported_as_edit_blocked() {
        // 受付判定と参照の確保の間に再生が始まると、参照の確保だけが失敗する。
        // 編集状態を読み直して、時間を置けば解消する失敗として返す。
        let adapter = adapter_with(|_| FakeHost {
            section_fails: true,
            later_state: Some(EditState::Preview),
            ..FakeHost::new()
        });

        let error = adapter.get_edit_info().unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::EditBlocked);
        assert_eq!(error.details()["edit_state"], "preview");
        assert!(error.retryable());
        assert!(error.retry_after_ms().is_some());
    }

    #[test]
    fn section_failure_while_editing_remains_sdk_error() {
        // 再生・出力に由来しない失敗は分類を変えない。
        let adapter = adapter_with(|_| FakeHost {
            section_fails: true,
            ..FakeHost::new()
        });

        let error = adapter.get_edit_info().unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::SdkError);
        assert_eq!(error.details()["sdk_operation"], "call_read_section");
    }

    #[test]
    fn errors_from_inside_the_section_are_not_reclassified() {
        // 参照区間へは入れており、内側の失敗は編集状態と無関係である。
        let adapter = adapter_with(|_| FakeHost {
            later_state: Some(EditState::Save),
            ..FakeHost::new()
        });
        let mut selector = sample_selector(&adapter);
        selector.frame = 1000;

        assert_eq!(
            adapter.get_object(&selector).unwrap_err().error_code(),
            ErrorCode::NotFound
        );
    }

    #[test]
    fn get_edit_info_maps_host_values() {
        let adapter = adapter();
        let info = adapter.get_edit_info().unwrap();

        assert_eq!(info.scene.id, 0);
        assert_eq!(info.scene.name.as_deref(), Some("Scene 1"));
        assert_eq!(info.scene.width, 1920);
        assert_eq!(info.scene.fps_rate, 30000);
        assert_eq!(info.scene.fps_scale, 1001);
        assert_eq!(info.scene.fps.map(|fps| fps.get()), Some(30000.0 / 1001.0));
        assert_eq!(
            info.cursor,
            Cursor {
                frame: 12,
                layer: 1
            }
        );
        assert_eq!(
            info.extent,
            Extent {
                frame_max: 3600,
                layer_max: 2
            }
        );
        assert_eq!(info.selected_range, Some(FrameRange { start: 10, end: 20 }));
        // 一覧は 4 つのフィールドを揃えて返る。tempo だけを運ぶと、読み取った
        // 一覧をそのまま書き戻す経路で残りの 3 つが失われる。
        assert_eq!(info.grid_bpm, vec![sample_grid_bpm()]);
        assert_eq!(info.project_epoch, adapter.project.epoch());
    }

    #[test]
    fn fps_is_absent_when_denominator_is_zero() {
        let adapter = adapter_with(|_| FakeHost {
            info: HostEditInfo {
                fps_scale: 0,
                ..fake_edit_info()
            },
            ..FakeHost::new()
        });
        let info = adapter.get_edit_info().unwrap();
        assert_eq!(info.scene.fps, None);
        assert_eq!(info.scene.fps_rate, 30000);
        assert_eq!(info.scene.fps_scale, 0);
    }

    #[test]
    fn unselected_range_is_absent() {
        let adapter = adapter_with(|_| FakeHost {
            info: HostEditInfo {
                selected_range: None,
                ..fake_edit_info()
            },
            ..FakeHost::new()
        });
        assert_eq!(adapter.get_edit_info().unwrap().selected_range, None);
    }

    #[test]
    fn get_current_scene_returns_scene_and_revision() {
        let adapter = adapter();
        adapter.project.on_object_updated();
        let (scene, revision) = adapter.get_current_scene().unwrap();
        assert_eq!(scene.id, 0);
        assert_eq!(revision, 1);
    }

    #[test]
    fn snapshot_revision_is_taken_inside_read_section() {
        // 参照区間へ入った時点の revision を採る。区間へ入る前の値を採っていると
        // ここで 0 が返り、テストが落ちる。
        let adapter = adapter_with(|project| FakeHost {
            bump_on_enter: 3,
            project: Some(Arc::clone(project)),
            ..FakeHost::new()
        });

        assert_eq!(adapter.list_layers(0).unwrap().snapshot_revision, 3);
    }

    #[test]
    fn list_layers_enumerates_up_to_layer_max() {
        let adapter = adapter();
        let snapshot = adapter.list_layers(0).unwrap();

        assert_eq!(snapshot.items.len(), 3);
        assert_eq!(snapshot.items[0].index, 0);
        assert_eq!(snapshot.items[0].name.as_deref(), Some("背景"));
        assert_eq!(snapshot.items[0].object_count, 1);
        assert_eq!(snapshot.items[1].name, None);
        assert!(snapshot.items[1].locked);
        assert_eq!(snapshot.items[1].object_count, 2);
        assert!(!snapshot.items[2].enabled);
        assert_eq!(snapshot.items[2].object_count, 0);
    }

    #[test]
    fn list_layers_counts_objects_without_reading_them() {
        // 件数のために名前や alias まで読むと、参照ロックを保持する時間が
        // オブジェクト数に比例して伸びる。
        let adapter = adapter();
        adapter.list_layers(0).unwrap();

        let calls = adapter.host.calls();
        assert!(calls.contains(&"object_count"), "{calls:?}");
        for forbidden in ["object_placements", "object_identity", "object_detail"] {
            assert!(
                !calls.contains(&forbidden),
                "件数のために {forbidden} を呼んでいます: {calls:?}"
            );
        }
    }

    #[test]
    fn scene_guard_rejects_other_scene() {
        let adapter = adapter();
        for error in [
            adapter.list_layers(7).unwrap_err(),
            adapter.list_objects_page(7, None).unwrap_err(),
            adapter.get_selection_page(7).unwrap_err(),
        ] {
            assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
            assert_eq!(error.details()["expected_scene_id"], 7);
            assert_eq!(error.details()["current_scene_id"], 0);
        }
    }

    #[test]
    fn list_objects_enumerates_every_layer_by_default() {
        let adapter = adapter();
        let snapshot = adapter.list_objects_page(0, None).unwrap();
        assert_eq!(snapshot.items.len(), 3);
        assert_eq!(snapshot.items[0].layer, 0);
        assert_eq!(snapshot.items[1].frame_start, 100);
        assert_eq!(snapshot.items[1].frame_end, 200);
        assert_eq!(snapshot.items[1].name.as_deref(), Some("立ち絵"));
    }

    #[test]
    fn list_objects_applies_layer_filter() {
        let adapter = adapter();
        let filter = ObjectFilter {
            layer_min: Some(1),
            layer_max: Some(1),
        };
        let snapshot = adapter.list_objects_page(0, Some(&filter)).unwrap();
        assert_eq!(snapshot.items.len(), 2);
        assert!(snapshot.items.iter().all(|item| item.layer == 1));
    }

    #[test]
    fn list_objects_clamps_filter_to_existing_layers() {
        let adapter = adapter();
        let filter = ObjectFilter {
            layer_min: None,
            layer_max: Some(999),
        };
        assert_eq!(
            adapter
                .list_objects_page(0, Some(&filter))
                .unwrap()
                .items
                .len(),
            3
        );
    }

    #[test]
    fn list_objects_treats_the_filter_as_already_validated() {
        // 絞り込み条件の妥当性は要求の復号と同じ場所で判定するため、逆転した
        // 範囲はここへ届かない。届いた場合も空の範囲として扱われるだけで、
        // 矛盾した指定がホストへ渡ることはない。
        let adapter = adapter();
        let filter = ObjectFilter {
            layer_min: Some(2),
            layer_max: Some(1),
        };
        let snapshot = adapter
            .list_objects_page(0, Some(&filter))
            .expect("検証は呼び出し側の責務であり、ここでは失敗させない");
        assert!(snapshot.items.is_empty());
    }

    #[test]
    fn list_objects_selector_can_be_resolved() {
        let adapter = adapter();
        let snapshot = adapter.list_objects_page(0, None).unwrap();
        for summary in snapshot.items {
            let detail = adapter.get_object(&summary.selector).unwrap();
            assert_eq!(
                detail.summary.selector.fingerprint,
                summary.selector.fingerprint
            );
        }
    }

    /// 参照区間の内側で指定の呼び出しを行った回数。
    fn calls_of(adapter: &HostReadAdapter<FakeHost>, call: &str) -> usize {
        adapter
            .host
            .calls()
            .iter()
            .filter(|recorded| **recorded == call)
            .count()
    }

    /// 同一性の材料を読んだ回数。
    fn identity_reads(adapter: &HostReadAdapter<FakeHost>) -> usize {
        calls_of(adapter, "object_identity")
    }

    /// 配下 effect を含む詳細を読んだ回数。
    fn detail_reads(adapter: &HostReadAdapter<FakeHost>) -> usize {
        calls_of(adapter, "object_detail")
    }

    /// ページ窓の外にある対象を読まないことを確かめる。
    #[test]
    fn list_objects_reads_details_only_within_the_page() {
        let adapter = adapter();
        let page = adapter
            .list_objects(0, None, &page_request(1, 1, None))
            .unwrap()
            .unwrap();

        // 総件数は列挙全体の件数であり、窓の件数ではない。並び順も変わらない。
        assert_eq!(page.meta.total_count, 3);
        assert_eq!(page.meta.offset, 1);
        assert!(page.meta.has_more);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].layer, 1);
        assert_eq!(page.items[0].frame_start, 100);

        assert_eq!(identity_reads(&adapter), 1, "窓の外の対象まで読んでいます");
    }

    /// 重い読み取りの回数が、プロジェクトの規模ではなく要求ページの件数で
    /// 決まることを確かめる。
    #[test]
    fn list_objects_bounds_detail_reads_by_the_page_size() {
        const TOTAL: usize = 200;
        const LIMIT: u32 = 5;

        let adapter = adapter_with(|_| FakeHost {
            layers: vec![FakeLayer {
                name: None,
                enabled: true,
                locked: false,
                objects: (0..TOTAL)
                    .map(|index| object(0, index * 10, index * 10 + 5, None))
                    .collect(),
            }],
            info: HostEditInfo {
                layer_max: 0,
                ..fake_edit_info()
            },
            ..FakeHost::new()
        });

        let page = adapter
            .list_objects(0, None, &page_request(0, LIMIT, None))
            .unwrap()
            .unwrap();

        assert_eq!(page.meta.total_count, TOTAL as u32);
        assert_eq!(page.items.len(), LIMIT as usize);
        assert_eq!(identity_reads(&adapter), LIMIT as usize);
    }

    /// 列挙が「対象が見つからない」を返さないことを確かめる。
    ///
    /// 対象を 1 つも指定しない列挙で不在を返しても、要求元は何が見つからな
    /// かったのかを特定できない。窓を確定してから対象が消えたのは列挙の失敗
    /// である。
    #[test]
    fn list_objects_does_not_report_not_found() {
        let adapter = adapter_with(|_| FakeHost {
            object_missing_at: Some(100),
            ..FakeHost::new()
        });

        let error = adapter.list_objects_page(0, None).unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::SdkError);
        // 畳んだ後も、実際に不在を検出した呼び出しを指す。
        assert_eq!(error.details()["sdk_operation"], "find_object");
    }

    /// 対象を指定する取得では、不在がそのまま不在として返ることを確かめる。
    ///
    /// 列挙側の畳み込みが、対象を指定する経路まで巻き込んではならない。
    #[test]
    fn get_object_reports_not_found_when_the_target_vanished() {
        let adapter = adapter_with(|_| FakeHost {
            object_missing_at: Some(100),
            ..FakeHost::new()
        });
        let selector = sample_selector(&adapter);

        assert_eq!(
            adapter.get_object(&selector).unwrap_err().error_code(),
            ErrorCode::NotFound
        );
    }

    /// スナップショット revision が一致しない要求で、重い読み取りへ進まない
    /// ことを確かめる。
    #[test]
    fn list_objects_rejects_a_stale_snapshot_revision_before_reading_details() {
        let adapter = adapter();
        let error = adapter
            .list_objects(0, None, &page_request(0, 50, Some(99)))
            .unwrap()
            .unwrap_err();

        assert_eq!(
            error,
            SnapshotRevisionMismatch {
                requested: 99,
                current: 0,
            }
        );
        assert_eq!(identity_reads(&adapter), 0);
    }

    #[test]
    fn get_object_returns_detail_for_matching_selector() {
        let adapter = adapter();
        let selector = sample_selector(&adapter);
        let detail = adapter.get_object(&selector).unwrap();

        assert_eq!(detail.summary.layer, 1);
        assert_eq!(detail.summary.frame_start, 100);
        // alias は配下 effect の設定値を含むため、位置だけの表記では終わらない。
        assert!(detail.alias.starts_with("[1:100]"), "{}", detail.alias);
        assert!(
            detail.alias.contains("動画ファイル"),
            "alias に配下 effect が現れません: {}",
            detail.alias
        );
        assert_eq!(detail.sections.len(), 1);
        assert_eq!(detail.effects.len(), 1);
        assert_eq!(detail.effects[0].name, "動画ファイル");
        assert_eq!(detail.effects[0].selector.object, detail.summary.selector);
    }

    #[test]
    fn get_object_gives_each_effect_its_position_in_the_column() {
        // 3 件のうち 2 件を同名にする。同名内の順序と列の位置が食い違う要素が
        // 無ければ、両者を取り違えた実装が通る。
        let adapter = adapter_with(|_| {
            host_with_effects(vec![
                file_effect("動画ファイル", 0, r"C:\movie.mp4"),
                file_effect("ぼかし", 0, r"C:\mask.png"),
                file_effect("ぼかし", 1, r"C:\mask2.png"),
            ])
        });
        let selector = listed_sample(&adapter).selector;
        let detail = adapter.get_object(&selector).expect("対象の詳細");

        let positions: Vec<usize> = detail
            .effects
            .iter()
            .map(|effect| effect.position)
            .collect();
        assert_eq!(positions, vec![0, 1, 2]);
        let indices: Vec<usize> = detail.effects.iter().map(|effect| effect.index).collect();
        assert_eq!(indices, vec![0, 0, 1]);
    }

    #[test]
    fn get_object_returns_a_movement_as_a_movement() {
        // 移動を持つ項目は区間ごとの値と移動方法を運び、移動を持たない項目は
        // 1 つの数値のままである。同じ種別の中で分かれるため、応答の形を
        // 種別だけで決めていれば片方が食い違う。
        let adapter = mixed_adapter();
        let selector = listed_sample(&adapter).selector;
        let detail = adapter.get_object(&selector).expect("対象の詳細");
        let items = &detail.effects[0].items;
        let value = |name: &str| {
            items
                .iter()
                .find(|item| item.name == name)
                .unwrap_or_else(|| panic!("設定項目 {name} がありません"))
                .value
                .clone()
        };

        let ItemValue::Track(track) = value("X") else {
            panic!("移動を持つ項目が移動として返りません: {:?}", value("X"));
        };
        assert_eq!(track.mode.as_deref(), Some(MOVEMENT_MODE));
        assert_eq!(track.values.len(), 2);
        assert!(
            matches!(value("拡大率"), ItemValue::Number { .. }),
            "移動を持たない項目まで移動として返りました: {:?}",
            value("拡大率")
        );
    }

    /// 候補の絞り込みが、候補以外の詳細を読まずに済むことを確かめる。
    #[test]
    fn get_object_reads_the_detail_of_the_candidate_only() {
        let adapter = adapter();
        let selector = sample_selector(&adapter);
        adapter.get_object(&selector).unwrap();

        assert_eq!(detail_reads(&adapter), 1, "候補以外の詳細まで読んでいます");
        assert_eq!(
            identity_reads(&adapter),
            0,
            "詳細と同一性の材料を二重に読んでいます"
        );
    }

    /// 同じレイヤーにある無関係な対象が読めなくても、対象の取得が成功することを
    /// 確かめる。
    ///
    /// 候補の絞り込みでレイヤー内の全対象の alias を読むと、無関係な対象の不調が
    /// 対象の取得を巻き込んで失敗させる。
    #[test]
    fn get_object_is_unaffected_by_a_failing_sibling() {
        // レイヤー 1 には開始フレーム 100 と 300 の対象がある。300 の読み取りだけを
        // 失敗させ、100 を取得する。
        let adapter = adapter_with(|_| FakeHost {
            object_read_fails_at: Some(300),
            ..FakeHost::new()
        });
        let selector = sample_selector(&adapter);

        let detail = adapter
            .get_object(&selector)
            .expect("同じレイヤーの別対象の失敗に巻き込まれました");
        assert_eq!(detail.summary.frame_start, 100);
    }

    #[test]
    fn get_object_matches_start_frame_exactly() {
        // 開始フレーム以降の探索を流用していると、範囲内のフレームでも
        // 同じオブジェクトが解決されてしまう。
        let adapter = adapter();
        let mut selector = sample_selector(&adapter);
        selector.frame = 150;

        let error = adapter.get_object(&selector).unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::NotFound);
    }

    #[test]
    fn get_object_reports_not_found_when_no_candidate() {
        let adapter = adapter();
        let mut selector = sample_selector(&adapter);
        selector.frame = 1000;
        assert_eq!(
            adapter.get_object(&selector).unwrap_err().error_code(),
            ErrorCode::NotFound
        );
    }

    /// 名前が変わった対象が、一致する対象なしではなく内容の食い違いとして
    /// 返ることを確かめる。
    ///
    /// 名前で候補を絞ると、この状況は候補 0 件になり「再試行しても解消しない」
    /// として返る。実際には読み直せば要求を作り直せる。
    #[test]
    fn get_object_reports_precondition_failed_after_the_target_is_renamed() {
        let project = Arc::new(ProjectState::new());
        let before = HostReadAdapter::new(FakeHost::new(), Arc::clone(&project));
        let selector = sample_selector(&before);

        let renamed = HostReadAdapter::new(
            FakeHost {
                layers: {
                    let mut layers = fake_layers();
                    layers[1].objects[0] =
                        object_with_effects(1, 100, 200, Some("改名後"), fake_effects());
                    layers
                },
                ..FakeHost::new()
            },
            Arc::clone(&project),
        );

        let error = renamed.get_object(&selector).unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
        assert!(
            matches!(error, ReadError::FingerprintMismatch { .. }),
            "{error} が内容の食い違いとして返っていません"
        );
    }

    /// 食い違いの応答が返したセレクターで読み直せることを確かめる。
    ///
    /// 現在の姿を返さなければ、要求元は列挙まで戻って対象を探し直すほかない。
    #[test]
    fn a_content_mismatch_returns_a_selector_that_resolves() {
        let project = Arc::new(ProjectState::new());
        let stale = HostReadAdapter::new(FakeHost::new(), Arc::clone(&project));
        let selector = sample_selector(&stale);

        let adapter = HostReadAdapter::new(
            FakeHost {
                layers: {
                    let mut layers = fake_layers();
                    layers[1].objects[0] =
                        object_with_effects(1, 100, 200, Some("改名後"), fake_effects());
                    layers
                },
                ..FakeHost::new()
            },
            Arc::clone(&project),
        );

        let ReadError::FingerprintMismatch { current_object } =
            adapter.get_object(&selector).unwrap_err()
        else {
            panic!("内容の食い違いとして返っていません");
        };
        assert_eq!(current_object.name.as_deref(), Some("改名後"));

        let detail = adapter
            .get_object(&current_object.selector)
            .expect("失敗が返したセレクターで読み直せません");
        assert_eq!(detail.summary, *current_object);
    }

    /// 名前を名乗らないセレクターでも対象が特定できることを確かめる。
    #[test]
    fn get_object_resolves_a_selector_without_a_name() {
        let adapter = adapter();
        let mut selector = sample_selector(&adapter);
        selector.name = None;

        let detail = adapter
            .get_object(&selector)
            .expect("名前を持たない指定が拒否されました");
        assert_eq!(detail.summary.frame_start, 100);
    }

    /// 名前だけが食い違うセレクターが、位置と内容で解決されることを確かめる。
    ///
    /// 名前は fingerprint の材料であり、対象が実際に改名されていれば
    /// fingerprint が捕まえる。セレクターの名前欄そのものは絞り込みに使わない。
    #[test]
    fn get_object_ignores_the_name_carried_by_the_selector() {
        let adapter = adapter();
        let mut selector = sample_selector(&adapter);
        selector.name = Some("別の名前".to_string());

        let detail = adapter
            .get_object(&selector)
            .expect("名前の食い違いで対象を見失いました");
        assert_eq!(detail.summary.name.as_deref(), Some("立ち絵"));
    }

    #[test]
    fn get_object_reports_ambiguous_selector_for_multiple_candidates() {
        let adapter = adapter_with(|_| {
            let mut layers = fake_layers();
            // 同じ開始フレームの候補を 2 件にする。
            layers[1].objects = vec![
                object(1, 100, 200, Some("立ち絵")),
                object(1, 100, 250, Some("立ち絵")),
            ];
            FakeHost {
                layers,
                ..FakeHost::new()
            }
        });
        let selector = sample_selector(&adapter);

        let error = adapter.get_object(&selector).unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::AmbiguousSelector);
        assert_eq!(error.details()["candidate_count"], 2);
    }

    #[test]
    fn get_object_reports_precondition_failed_for_fingerprint_mismatch() {
        let adapter = adapter_with(|_| {
            let mut layers = fake_layers();
            // 位置と名前は同じまま alias だけ変える。
            layers[1].objects[0].identity.alias = "[changed]".to_string();
            FakeHost {
                layers,
                ..FakeHost::new()
            }
        });
        let selector = sample_selector(&adapter);

        assert_eq!(
            adapter.get_object(&selector).unwrap_err().error_code(),
            ErrorCode::PreconditionFailed
        );
    }

    #[test]
    fn get_object_reports_precondition_failed_for_scene_mismatch() {
        let adapter = adapter();
        let mut selector = sample_selector(&adapter);
        selector.scene_id = 5;

        let error = adapter.get_object(&selector).unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
        assert_eq!(error.details()["expected_scene_id"], 5);
    }

    #[test]
    fn get_object_reports_precondition_failed_for_epoch_mismatch() {
        let adapter = adapter();
        let mut selector = sample_selector(&adapter);
        selector.project_epoch = "00000000-0000-0000-0000-000000000000".to_string();

        let error = adapter.get_object(&selector).unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
        assert!(
            !adapter.host.calls().contains(&"enter_read_section"),
            "epoch 不一致で参照区間へ入りました"
        );
    }

    #[test]
    fn a_selector_carrying_a_tampered_fingerprint_is_rejected() {
        // 要求は算出方式を運ばない。方式が変われば digest も変わるため、対象の
        // 同一性は fingerprint の照合だけで守られる。
        let adapter = adapter();
        let mut selector = sample_selector(&adapter);
        selector.fingerprint = format!("sha256:{}", "0".repeat(64))
            .parse()
            .expect("差し替えた fingerprint の書式");

        let error = adapter.get_object(&selector).unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
        assert!(
            matches!(error, ReadError::FingerprintMismatch { .. }),
            "{error} が fingerprint の食い違いとして返っていません"
        );
    }

    /// 参照区間へ入った回数。
    fn section_entries(adapter: &HostReadAdapter<FakeHost>) -> usize {
        adapter
            .host
            .calls()
            .iter()
            .filter(|call| **call == "enter_read_section")
            .count()
    }

    /// 対象が動いた後のセレクターが、一致する対象なしとして拒否されることを確かめる。
    #[test]
    fn get_object_reports_not_found_after_the_target_moved() {
        let adapter = adapter_with(|_| {
            // レイヤー 1 の対象が開始フレーム 100 から 105 へ動く。
            let mut layers = fake_layers();
            layers[1].objects[0] = object(1, 105, 205, Some("立ち絵"));
            FakeHost {
                layers,
                ..FakeHost::new()
            }
        });
        let selector = sample_selector(&adapter);

        assert_eq!(
            adapter.get_object(&selector).unwrap_err().error_code(),
            ErrorCode::NotFound
        );
    }

    /// 移動先へ別の対象が居座った場合に、fingerprint の照合で拒否されることを
    /// 確かめる。
    ///
    /// 位置だけで対象を決めていると、旧セレクターが別の対象へ解決されてしまう。
    #[test]
    fn get_object_reports_precondition_failed_when_another_object_took_the_place() {
        let adapter = adapter_with(|_| {
            let mut layers = fake_layers();
            // 元の対象は動き、空いた位置に同名で別内容の対象が入る。
            layers[1].objects[0] = object(1, 105, 205, Some("立ち絵"));
            let mut intruder = object(1, 100, 150, Some("立ち絵"));
            intruder.identity.alias = "[1:100]#2".to_string();
            layers[1].objects.push(intruder);
            FakeHost {
                layers,
                ..FakeHost::new()
            }
        });
        let selector = sample_selector(&adapter);

        assert_eq!(
            adapter.get_object(&selector).unwrap_err().error_code(),
            ErrorCode::PreconditionFailed
        );
    }

    /// 対象が動くと、読み取り口が返す fingerprint も変わることを確かめる。
    ///
    /// epoch を共有させるため、プロジェクト状態は 2 つの adapter で共用する。
    /// これにより差分は対象の位置だけになる。
    #[test]
    fn fingerprint_from_the_adapter_changes_when_the_target_moves() {
        let project = Arc::new(ProjectState::new());
        let before = HostReadAdapter::new(FakeHost::new(), Arc::clone(&project));
        let after = HostReadAdapter::new(
            FakeHost {
                layers: {
                    let mut layers = fake_layers();
                    layers[1].objects[0] = object(1, 105, 205, Some("立ち絵"));
                    layers
                },
                ..FakeHost::new()
            },
            Arc::clone(&project),
        );

        let fingerprint_of = |adapter: &HostReadAdapter<FakeHost>, frame_start: usize| {
            adapter
                .list_objects_page(0, None)
                .unwrap()
                .items
                .into_iter()
                .find(|item| item.layer == 1 && item.frame_start == frame_start)
                .unwrap_or_else(|| panic!("開始フレーム {frame_start} の対象がありません"))
                .selector
                .fingerprint
        };

        assert_ne!(
            fingerprint_of(&before, 100),
            fingerprint_of(&after, 105),
            "対象が動いても fingerprint が変わりません"
        );
    }

    /// プロジェクトを開き直すと、旧セレクターが拒否されることを確かめる。
    ///
    /// epoch の再発行はプロジェクト境界そのものであり、それ以前に得た
    /// セレクターは参照区間へ入る前に拒否される。
    #[test]
    fn get_object_is_rejected_after_the_project_is_reopened() {
        let adapter = adapter();
        let selector = sample_selector(&adapter);
        adapter
            .get_object(&selector)
            .expect("開き直す前のセレクターが解決できません");
        let entered = section_entries(&adapter);

        adapter
            .project
            .on_project_load(Some(r"C:\projects\reopened.aup2"));

        let error = adapter.get_object(&selector).unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
        assert_eq!(
            section_entries(&adapter),
            entered,
            "epoch を再発行した後のセレクターで参照区間へ入りました"
        );
    }

    /// 別スレッドで実行し、期限内に完了しなければ落とす。
    ///
    /// 待ち合わせが解けない場合をここで検出する。完了すればその時点で戻るため、
    /// 期限まで待つことはない。
    fn complete_within<T: Send + 'static>(
        timeout: Duration,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(f());
        });
        receiver
            .recv_timeout(timeout)
            .expect("読み取りが期限内に完了しませんでした")
    }

    /// 参照区間の内側からプロジェクト境界へ触れても読み取りが完了することを
    /// 確かめる。
    ///
    /// 境界は非再入の Mutex で守られている。読み取りが区間を跨いでそれを保持して
    /// いれば、同じスレッドからの更新で待ち合わせが解けなくなる。epoch を区間の
    /// 外で採ることが、この経路を成立させている。
    #[test]
    fn reading_completes_when_the_project_boundary_changes_inside_the_section() {
        let project = Arc::new(ProjectState::new());
        let epoch = project.epoch();
        let adapter = HostReadAdapter::new(
            FakeHost {
                renew_boundary_on_enter: true,
                project: Some(Arc::clone(&project)),
                ..FakeHost::new()
            },
            Arc::clone(&project),
        );

        let (grid_bpm, object_count) = complete_within(Duration::from_secs(10), move || {
            let info = adapter.get_edit_info().unwrap();
            let objects = adapter.list_objects_page(0, None).unwrap();
            (info.grid_bpm.len(), objects.items.len())
        });

        assert_eq!(grid_bpm, 1);
        assert_eq!(object_count, 3);
        assert_ne!(
            project.epoch(),
            epoch,
            "参照区間の内側で境界が更新されていません"
        );
    }

    #[test]
    fn list_available_effects_returns_catalog() {
        let adapter = adapter();
        let result = adapter.list_available_effects_page(None).unwrap();
        assert_eq!(result.items.len(), fake_catalog().len());
        assert_eq!(result.page.total_count as usize, fake_catalog().len());
    }

    #[test]
    fn list_available_effects_filters_by_type() {
        let adapter = adapter();
        let result = adapter
            .list_available_effects_page(Some(&EffectType::Input))
            .unwrap();
        let names: Vec<&str> = result.items.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["動画ファイル", "画像ファイル"]);

        let none = adapter
            .list_available_effects_page(Some(&EffectType::Transition))
            .unwrap();
        assert!(none.items.is_empty());
    }

    #[test]
    fn list_available_effects_leaves_the_description_null_without_a_source() {
        // 説明の供給源はホストが同梱するファイルだけである。それを読めない環境
        // では説明が出ないが、一覧そのものは働き続ける。
        let adapter = adapter();
        let result = adapter.list_available_effects_page(None).unwrap();

        assert_eq!(result.items.len(), fake_catalog().len());
        for effect in &result.items {
            assert!(
                effect.description.is_none(),
                "{} に説明が付きました",
                effect.name
            );
        }
        assert_eq!(result.items[0].name, "ぼかし");
        assert_eq!(result.items[0].item_count, 1);
        assert_eq!(result.items[0].effect_type, EffectType::Filter);
    }

    #[test]
    fn list_available_effects_carries_the_description_of_each_effect() {
        // 説明を持つ effect にはその説明が、持たない effect には null が載る。
        // 説明は全文で運ぶ——2 行目に発見の鍵がある説明が実在する。
        let glow = "光を拡散させます\n発光量を指定します";
        let adapter = adapter_with(|_| FakeHost {
            help: vec![
                (
                    "グロー".to_string(),
                    effect_help(Some(glow), &[("グローの項目0", "拡散の量です")]),
                ),
                (
                    "標準描画".to_string(),
                    effect_help(Some("描画のしかたを決めます"), &[]),
                ),
            ],
            ..FakeHost::new()
        });
        let result = adapter.list_available_effects_page(None).unwrap();

        let described: Vec<(&str, Option<&str>)> = result
            .items
            .iter()
            .map(|effect| (effect.name.as_str(), effect.description.as_deref()))
            .collect();
        assert_eq!(
            described,
            vec![
                ("ぼかし", None),
                ("動画ファイル", None),
                ("グロー", Some(glow)),
                ("画像ファイル", None),
                ("標準描画", Some("描画のしかたを決めます")),
            ],
            "説明が別の effect へ付いているか、落ちています"
        );
    }

    #[test]
    fn list_available_effects_asks_for_the_description_only_for_the_requested_page() {
        // 説明の取得も窓の分だけである。供給源の引き直しを応答へ載せない
        // effect まで広げない。
        let adapter = adapter();
        let page = page_request(3, 1, None).window();
        adapter.list_available_effects(None, &page).unwrap();

        let asked = adapter
            .host
            .calls()
            .iter()
            .filter(|call| **call == "effect_help")
            .count();
        assert_eq!(asked, 1, "窓の外の effect について説明を引いています");
    }

    #[test]
    fn list_available_effects_reports_the_item_count_of_each_effect() {
        let adapter = adapter();
        let result = adapter.list_available_effects_page(None).unwrap();
        let counts: Vec<usize> = result
            .items
            .iter()
            .map(|effect| effect.item_count)
            .collect();
        assert_eq!(
            counts,
            fake_catalog()
                .iter()
                .map(|entry| entry.items.len())
                .collect::<Vec<usize>>()
        );
    }

    #[test]
    fn list_available_effects_counts_items_only_for_the_requested_page() {
        // 項目の列挙は effect ごとの呼び出しである。全件について呼ぶと、費用が
        // 要求ページではなく登録数で決まる。
        let adapter = adapter();
        let page = page_request(1, 2, None).window();
        let result = adapter.list_available_effects(None, &page).unwrap();

        let names: Vec<String> = result.items.iter().map(|e| e.name.clone()).collect();
        assert_eq!(
            names,
            vec!["動画ファイル".to_string(), "グロー".to_string()]
        );
        assert_eq!(
            adapter.host.item_count_queries(),
            names,
            "窓の外の effect について設定項目を数えています"
        );
        assert!(
            adapter.host.item_count_queries().len() < fake_catalog().len(),
            "カタログの全件について設定項目を数えています"
        );
    }

    #[test]
    fn list_available_effects_counts_items_only_for_the_filtered_page() {
        // 絞り込みは窓より先に効く。絞る前の並びで窓を切ると、応答へ載らない
        // effect の項目を数えることになる。
        let adapter = adapter();
        let page = page_request(0, 1, None).window();
        let result = adapter
            .list_available_effects(Some(&EffectType::Filter), &page)
            .unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].name, "ぼかし");
        assert_eq!(result.page.total_count, 2, "絞り込み後の総件数を返します");
        assert_eq!(
            adapter.host.item_count_queries(),
            vec!["ぼかし".to_string()],
            "窓の外の effect について設定項目を数えています"
        );
    }

    #[test]
    fn describe_effects_returns_every_requested_effect_in_the_requested_order() {
        // 名前の似た effect の使い分けは設定項目の顔ぶれで解ける。並べて引ける
        // ことと、要求の順が保たれることがその前提である。
        let adapter = adapter();
        let result = adapter
            .describe_effects(&describe_params(&["グロー", "ぼかし"]))
            .unwrap();

        let names: Vec<&str> = result
            .effects
            .iter()
            .map(|effect| effect.name.as_str())
            .collect();
        assert_eq!(names, vec!["グロー", "ぼかし"]);
        assert!(result.not_found.is_empty());

        // 項目の名前と種別はホストの列挙から来る。件数も並びもそのままである。
        // フェイクの番号は名前の昇順とも降順とも一致しないため、見栄えのために
        // 名前で並べ替えた実装はここに現れる。
        let glow: Vec<(&str, &EffectItemType)> = result.effects[0]
            .items
            .iter()
            .map(|item| (item.name.as_str(), &item.item_type))
            .collect();
        assert_eq!(
            glow,
            vec![
                ("グローの項目1", &EffectItemType::Integer),
                ("グローの項目0", &EffectItemType::Check),
                ("グローの項目3", &EffectItemType::Integer),
                ("グローの項目2", &EffectItemType::Check),
            ],
            "項目の一覧が別の effect のものになっているか、並びが崩れています"
        );
        assert_eq!(result.effects[1].items.len(), 1);
    }

    #[test]
    fn describe_effects_asks_the_host_for_the_catalog_once() {
        // 登録の有無はカタログで決める。判定を effect ごとのループへ入れると、
        // 全件の列挙が要求した名前の数だけ走る。
        let adapter = adapter();
        adapter
            .describe_effects(&describe_params(&["グロー", "ぼかし", "存在しない効果"]))
            .unwrap();

        assert_eq!(
            adapter
                .host
                .calls()
                .iter()
                .filter(|call| **call == "effect_catalog")
                .count(),
            1,
            "カタログを要求ごとに 1 度だけ引いていません"
        );
    }

    #[test]
    fn describe_effects_tells_a_described_effect_apart_from_an_undescribed_one() {
        // 説明を持つ effect と持たない effect は同じ応答に混ざる。持たない側を
        // 推測で埋めない。
        let adapter = adapter_with(|_| FakeHost {
            help: vec![(
                "グロー".to_string(),
                effect_help(
                    Some("光を拡散させます"),
                    // 列挙の先頭に来る項目。番号は並びのままではない。
                    &[("グローの項目1", "拡散の量を指定します")],
                ),
            )],
            ..FakeHost::new()
        });
        let result = adapter
            .describe_effects(&describe_params(&["グロー", "ぼかし"]))
            .unwrap();

        assert_eq!(
            result.effects[0].description.as_deref(),
            Some("光を拡散させます")
        );
        assert_eq!(
            result.effects[0].items[0].description.as_deref(),
            Some("拡散の量を指定します")
        );
        // 説明の無い項目は同じ effect の中でも null になる。
        assert_eq!(result.effects[0].items[1].description, None);

        assert_eq!(result.effects[1].name, "ぼかし");
        assert_eq!(result.effects[1].description, None);
        assert_eq!(result.effects[1].items[0].description, None);
    }

    #[test]
    fn describe_effects_never_reports_the_effect_description_as_an_item_description() {
        // 効果の説明が項目の説明として出れば、要求元は誤った文言を確信を持って
        // 使う。供給源はどちらも同じ節に並ぶため、区別を実際に確かめる。
        let adapter = adapter_with(|_| FakeHost {
            help: vec![(
                "ぼかし".to_string(),
                effect_help(
                    Some("効果そのものの説明です"),
                    &[("ぼかしの項目0", "項目の説明です")],
                ),
            )],
            ..FakeHost::new()
        });
        let result = adapter
            .describe_effects(&describe_params(&["ぼかし"]))
            .unwrap();

        let effect = &result.effects[0];
        assert_eq!(
            effect.description.as_deref(),
            Some("効果そのものの説明です")
        );
        assert_eq!(
            effect.items[0].description.as_deref(),
            Some("項目の説明です")
        );
        assert!(
            !effect
                .items
                .iter()
                .any(|item| item.description == effect.description),
            "項目の説明として効果の説明が出ています"
        );
    }

    #[test]
    fn describe_effects_names_what_it_could_not_find_instead_of_dropping_it() {
        // 落ちた名前に気付けなければ、要求元は「その effect には設定項目が無い」
        // と誤読する。見つかった分は返しつつ、見つからなかった名前を明示する。
        let adapter = adapter();
        let result = adapter
            .describe_effects(&describe_params(&["ぐろー", "ぼかし", "存在しない効果"]))
            .unwrap();

        let names: Vec<&str> = result
            .effects
            .iter()
            .map(|effect| effect.name.as_str())
            .collect();
        assert_eq!(names, vec!["ぼかし"]);
        assert_eq!(
            result.not_found,
            vec!["ぐろー".to_string(), "存在しない効果".to_string()],
            "見つからなかった名前が要求の順で並んでいません"
        );
        // 登録の無い名前について設定項目を列挙しない。列挙の失敗は「登録が無い」
        // と区別できず、要求全体を落としてしまう。
        assert_eq!(
            adapter
                .host
                .calls()
                .iter()
                .filter(|call| **call == "effect_items")
                .count(),
            1
        );
    }

    #[test]
    fn describe_effects_keeps_every_line_of_the_help_text() {
        // 発見の鍵が 2 行目にある説明が実在する。効果の説明も項目の説明も
        // 先頭行だけに切らない。
        let adapter = adapter_with(|_| FakeHost {
            help: vec![(
                "ぼかし".to_string(),
                effect_help(
                    Some("単色の図形を作成します\nsvgファイルから読み込むことも出来ます"),
                    &[(
                        "ぼかしの項目0",
                        "図形の種類を選択します\nボタンクリックでsvgファイルを選択出来ます",
                    )],
                ),
            )],
            ..FakeHost::new()
        });
        let result = adapter
            .describe_effects(&describe_params(&["ぼかし"]))
            .unwrap();

        let effect = &result.effects[0];
        let description = effect.description.as_deref().expect("効果の説明がある");
        assert_eq!(description.lines().count(), 2);
        assert!(description.lines().nth(1).unwrap().contains("svg"));

        let item = effect.items[0]
            .description
            .as_deref()
            .expect("項目の説明がある");
        assert_eq!(item.lines().count(), 2);
        assert!(item.lines().nth(1).unwrap().contains("svg"));
    }

    #[test]
    fn describe_effects_carries_the_choices_of_each_item_with_their_source() {
        // 候補を知らないことが損失の原因である。読み取り経路へ出さなければ、
        // 要求元は候補の存在そのものに気付けない。
        let adapter = adapter_with(|_| FakeHost {
            facets: vec![(
                "グロー".to_string(),
                effect_facets(&[
                    (
                        "グローの項目1",
                        choices_facet(&["通常", "加算"], TableSource::BuiltinTable),
                    ),
                    (
                        "グローの項目0",
                        choices_facet(&["円", "四角形"], TableSource::Sidecar),
                    ),
                ]),
            )],
            ..FakeHost::new()
        });
        let result = adapter
            .describe_effects(&describe_params(&["グロー", "ぼかし"]))
            .unwrap();

        let described: Vec<(&str, Option<&ItemChoices>)> = result.effects[0]
            .items
            .iter()
            .map(|item| (item.name.as_str(), item.choices.as_ref()))
            .collect();
        assert_eq!(
            described,
            vec![
                (
                    "グローの項目1",
                    Some(&ItemChoices {
                        values: vec!["通常".to_string(), "加算".to_string()],
                        source: TableSource::BuiltinTable,
                    })
                ),
                (
                    "グローの項目0",
                    Some(&ItemChoices {
                        values: vec!["円".to_string(), "四角形".to_string()],
                        source: TableSource::Sidecar,
                    })
                ),
                // 表に無い項目は null である。空の配列で代えると、「候補が
                // 1 つも無い項目」と「表に載っていない項目」の区別が付かない。
                ("グローの項目3", None),
                ("グローの項目2", None),
            ],
            "候補が別の項目へ付いているか、由来が入れ替わっています"
        );
        // 表を持たない effect の項目も null になる。
        assert!(
            result.effects[1]
                .items
                .iter()
                .all(|item| item.choices.is_none()),
            "表に無い effect へ候補が付きました"
        );
    }

    #[test]
    fn describe_effects_carries_the_range_of_each_item_with_its_source() {
        // 値域を知らないことは、値域外の指定と値域内の取り違えの両方を招く。
        // 後者は書き戻し照合も通るため、書く前に読めることだけが手段になる。
        let adapter = adapter_with(|_| FakeHost {
            facets: vec![(
                "グロー".to_string(),
                effect_facets(&[
                    (
                        "グローの項目1",
                        range_facet(Some(1.0), Some(4000.0), Some(0), TableSource::BuiltinTable),
                    ),
                    // **測れた側だけを載せる。** 3 つの値は個別に欠ける。
                    (
                        "グローの項目0",
                        range_facet(None, Some(100.0), None, TableSource::Sidecar),
                    ),
                ]),
            )],
            ..FakeHost::new()
        });
        let result = adapter
            .describe_effects(&describe_params(&["グロー", "ぼかし"]))
            .unwrap();

        let described: Vec<(&str, Option<ItemRange>)> = result.effects[0]
            .items
            .iter()
            .map(|item| (item.name.as_str(), item.range))
            .collect();
        assert_eq!(
            described,
            vec![
                (
                    "グローの項目1",
                    Some(ItemRange {
                        min: FiniteF64::try_new(1.0),
                        max: FiniteF64::try_new(4000.0),
                        decimals: Some(0),
                        source: TableSource::BuiltinTable,
                    })
                ),
                (
                    "グローの項目0",
                    Some(ItemRange {
                        min: None,
                        max: FiniteF64::try_new(100.0),
                        decimals: None,
                        source: TableSource::Sidecar,
                    })
                ),
                // 表に無い項目は null である。**値域そのものが null であることと、
                // 上限だけが null であることは別の事実である。**
                ("グローの項目3", None),
                ("グローの項目2", None),
            ],
            "値域が別の項目へ付いているか、測れていない側が埋められています"
        );
        assert!(
            result.effects[1]
                .items
                .iter()
                .all(|item| item.range.is_none()),
            "表に無い effect へ値域が付きました"
        );
    }

    #[test]
    fn describe_effects_does_not_filter_the_facets_by_item_type() {
        // **種別で絞らない。** 表に載っていれば text の項目にも候補と値域が付く。
        // 絞ると、表が書いた記述を我々の判断で黙って落とすことになり、落とした
        // 側に「面が無い」と「面を出さないことにした」の区別は届かない。
        //
        // **数値や選択肢の項目で確かめても意味が無い**——種別で絞る実装でも
        // 通ってしまう。値域を持ちそうにない種別で見る。
        let adapter = adapter_with(|_| FakeHost {
            catalog: vec![FakeCatalogEntry {
                summary: HostEffectSummary {
                    name: "字幕".to_string(),
                    effect_type: EffectType::Filter,
                    flags: EffectFlags::from_raw(1),
                },
                items: vec![AvailableEffectItem {
                    name: "本文".to_string(),
                    item_type: EffectItemType::Text,
                }],
            }],
            facets: vec![(
                "字幕".to_string(),
                effect_facets(&[(
                    "本文",
                    ItemFacets {
                        choices: Some(ItemChoices {
                            values: vec!["既定".to_string()],
                            source: TableSource::Sidecar,
                        }),
                        range: Some(ItemRange {
                            min: None,
                            max: FiniteF64::try_new(1024.0),
                            decimals: None,
                            source: TableSource::Sidecar,
                        }),
                    },
                )]),
            )],
            ..FakeHost::new()
        });
        let result = adapter
            .describe_effects(&describe_params(&["字幕"]))
            .unwrap();

        let item = &result.effects[0].items[0];
        assert_eq!(item.item_type, EffectItemType::Text);
        assert!(item.choices.is_some(), "text の項目から候補が落ちました");
        assert_eq!(
            item.range.and_then(|range| range.max),
            FiniteF64::try_new(1024.0),
            "text の項目から値域が落ちました"
        );
    }

    #[test]
    fn describe_effects_ignores_a_table_entry_for_an_item_that_does_not_exist_here() {
        // 利用者は複数の環境で同じサイドカーを使う。この環境のホストが列挙
        // しない項目は、表に在っても応答には現れない。落とさずに失敗へ倒すと、
        // 環境をまたぐ配布物が tool を壊す。
        let adapter = adapter_with(|_| FakeHost {
            facets: vec![(
                "ぼかし".to_string(),
                effect_facets(&[
                    (
                        "ぼかしの項目0",
                        choices_facet(&["通常"], TableSource::Sidecar),
                    ),
                    (
                        "この環境に無い項目",
                        choices_facet(&["値"], TableSource::Sidecar),
                    ),
                ]),
            )],
            ..FakeHost::new()
        });
        let result = adapter
            .describe_effects(&describe_params(&["ぼかし"]))
            .unwrap();

        let names: Vec<&str> = result.effects[0]
            .items
            .iter()
            .map(|item| item.name.as_str())
            .collect();
        assert_eq!(names, vec!["ぼかしの項目0"]);
        assert!(result.effects[0].items[0].choices.is_some());
    }

    #[test]
    fn describe_effects_asks_for_the_facets_once_per_effect() {
        // 面も効果ごとに 1 度で引く。項目ごとに引くと、同じ表を項目の数だけ
        // 引き直す。
        let adapter = adapter();
        adapter
            .describe_effects(&describe_params(&["グロー", "標準描画"]))
            .unwrap();

        let asked = adapter
            .host
            .calls()
            .iter()
            .filter(|call| **call == "effect_facets")
            .count();
        assert_eq!(asked, 2, "効果の数と面を引いた回数が食い違っています");
    }

    #[test]
    fn describe_effects_asks_the_host_once_per_effect() {
        // 説明は効果ごとに 1 度で節全体を引く。項目ごとに引くと、同じ節を項目の
        // 数だけ引き直すうえ、効果の説明を指すキーを項目名として渡せてしまう。
        let adapter = adapter();
        adapter
            .describe_effects(&describe_params(&["グロー", "標準描画"]))
            .unwrap();

        let asked = adapter
            .host
            .calls()
            .iter()
            .filter(|call| **call == "effect_help")
            .count();
        assert_eq!(asked, 2, "効果の数と説明を引いた回数が食い違っています");
    }

    #[test]
    fn describe_effects_carries_the_group_of_each_item_as_the_host_returned_it() {
        // 設定項目の一覧は平らな列で返る。どこが 1 つの組かを述べるのは
        // グループだけであり、位置も所属アイテム名もホストが返した値のまま運ぶ。
        //
        // 同じグループに属する 2 件へ別々の位置を持たせ、どちらの位置も設定項目の
        // 列挙順とは一致させない。位置を取り違えた実装も、ホストの戻り値ではなく
        // 列挙順から位置を作った実装も、結果に現れる。
        let adapter = adapter_with(|_| FakeHost {
            groups: vec![
                member_group("グロー", "グローの項目1", 1, &GLOW_AXES),
                member_group("グロー", "グローの項目0", 0, &GLOW_AXES),
            ],
            ..FakeHost::new()
        });
        let result = adapter
            .describe_effects(&describe_params(&["グロー"]))
            .unwrap();

        let items = &result.effects[0].items;
        assert_eq!(items[0].name, "グローの項目1");
        assert_eq!(items[0].group, Some(item_group(1, &GLOW_AXES)));
        assert_eq!(items[1].name, "グローの項目0");
        assert_eq!(items[1].group, Some(item_group(0, &GLOW_AXES)));
    }

    #[test]
    fn describe_effects_reports_no_group_for_an_item_that_belongs_to_none() {
        // 属さないことは null で表す。空のグループを作ると、所属アイテムが
        // 1 件も無いグループに属していることになる。
        let adapter = adapter();
        let result = adapter
            .describe_effects(&describe_params(&["グロー", "ぼかし"]))
            .unwrap();

        assert!(
            result
                .effects
                .iter()
                .flat_map(|effect| &effect.items)
                .all(|item| item.group.is_none()),
            "属さない項目にグループが付きました"
        );
    }

    #[test]
    fn describe_effects_fails_when_a_group_cannot_be_read() {
        // 引けなかったことを null にすると、属さないことと同じ形で届く。要求元は
        // 「この項目はグループに属さない」と読み、区別する手段を失う。
        let adapter = adapter_with(|_| FakeHost {
            groups: vec![(
                ("グロー".to_string(), "グローの項目3".to_string()),
                FakeItemGroup::Unavailable,
            )],
            ..FakeHost::new()
        });

        let error = adapter
            .describe_effects(&describe_params(&["グロー"]))
            .expect_err("引けなかった項目が失敗として返っていません");
        assert!(
            matches!(
                error,
                ReadError::Sdk {
                    operation: "get_effect_item_group_names"
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn describe_effects_carries_a_group_that_does_not_name_the_item_it_was_asked_about() {
        // 所属アイテム名の位置番目が問い合わせた項目名と一致することは
        // 確かめない。一致しないときに採れる手は落とすことしか無く、落とせば
        // 「グループに属さない」として届いて引けなかったことと同じ形になる。
        let names = ["別の項目A", "別の項目B", "別の項目C"];
        let adapter = adapter_with(|_| FakeHost {
            groups: vec![
                member_group("グロー", "グローの項目1", 2, &names),
                // 位置が所属アイテム名の外を指す。
                member_group("グロー", "グローの項目0", 5, &["グローの項目0"]),
            ],
            ..FakeHost::new()
        });
        let result = adapter
            .describe_effects(&describe_params(&["グロー"]))
            .unwrap();

        let items = &result.effects[0].items;
        assert_eq!(
            items[0].group,
            Some(item_group(2, &names)),
            "問い合わせた項目名を含まないグループが落とされました"
        );
        assert_eq!(
            items[1].group,
            Some(item_group(5, &["グローの項目0"])),
            "所属アイテム名の外を指す位置が落とされました"
        );
    }

    #[test]
    fn describe_effects_mixes_grouped_and_ungrouped_items_in_one_effect() {
        // 1 つの effect の中で、別々のグループへ属する項目と属さない項目が
        // 混ざる。グループが隣の項目や別の effect へ漏れないことを列全体で見る。
        let adapter = adapter_with(|_| FakeHost {
            groups: mixed_groups(),
            ..FakeHost::new()
        });
        let result = adapter
            .describe_effects(&describe_params(&["グロー", "ぼかし"]))
            .unwrap();

        let described: Vec<(&str, Option<ItemGroup>)> = result.effects[0]
            .items
            .iter()
            .map(|item| (item.name.as_str(), item.group.clone()))
            .collect();
        assert_eq!(
            described,
            vec![
                ("グローの項目1", Some(item_group(1, &GLOW_AXES))),
                ("グローの項目0", Some(item_group(0, &GLOW_AXES))),
                ("グローの項目3", Some(item_group(0, &["グローの項目3"]))),
                ("グローの項目2", None),
            ],
            "グループが別の項目へ付いているか、属さない項目に現れています"
        );
        assert!(
            result.effects[1]
                .items
                .iter()
                .all(|item| item.group.is_none()),
            "別の effect の項目へグループが漏れました"
        );
    }

    #[test]
    fn describe_effects_asks_for_the_group_once_per_item() {
        // グループは項目ごとに引く。引いたグループの所属アイテム名から他の項目の
        // グループを導くと、項目名が effect の中で一意であることを前提に置く。
        //
        // **導ける材料が実際に返る状況で数える。** 属する項目が 1 件も無ければ
        // 導きようが無く、導く実装でも回数が項目数と等しくなる。
        let adapter = adapter_with(|_| FakeHost {
            groups: mixed_groups(),
            ..FakeHost::new()
        });
        let result = adapter
            .describe_effects(&describe_params(&["グロー", "ぼかし"]))
            .unwrap();

        let items: usize = result.effects.iter().map(|effect| effect.items.len()).sum();
        let asked = adapter
            .host
            .calls()
            .iter()
            .filter(|call| **call == "effect_item_group")
            .count();
        assert_eq!(
            asked, items,
            "設定項目の数とグループを引いた回数が食い違っています"
        );
    }

    #[test]
    fn the_fake_host_answers_about_a_group_in_three_ways() {
        // グループに属する・属さない・引けないの 3 通りを作り分けられる。
        // 引けないを作れなければ、失敗を「属さない」へ倒した実装を検査できない。
        let group = ItemGroup {
            index: 1,
            item_names: vec!["グローの項目1".to_string(), "グローの項目0".to_string()],
        };
        let host = FakeHost {
            groups: vec![
                (
                    ("グロー".to_string(), "グローの項目0".to_string()),
                    FakeItemGroup::Member(group.clone()),
                ),
                (
                    ("グロー".to_string(), "グローの項目2".to_string()),
                    FakeItemGroup::Unavailable,
                ),
            ],
            ..FakeHost::new()
        };

        assert_eq!(
            host.effect_item_group("グロー", "グローの項目0").unwrap(),
            Some(group)
        );
        assert_eq!(
            host.effect_item_group("グロー", "グローの項目3").unwrap(),
            None
        );
        assert!(matches!(
            host.effect_item_group("グロー", "グローの項目2"),
            Err(ReadError::Sdk {
                operation: "get_effect_item_group_names"
            })
        ));
    }

    #[test]
    fn each_operation_enters_the_read_section_at_most_once() {
        fn entries(adapter: &HostReadAdapter<FakeHost>) -> usize {
            adapter
                .host
                .calls()
                .iter()
                .filter(|call| **call == "enter_read_section")
                .count()
        }

        let edit_info = adapter();
        edit_info.get_edit_info().unwrap();
        assert_eq!(entries(&edit_info), 1, "get_edit_info");

        let current_scene = adapter();
        current_scene.get_current_scene().unwrap();
        assert_eq!(entries(&current_scene), 1, "get_current_scene");

        let layers = adapter();
        layers.list_layers(0).unwrap();
        assert_eq!(entries(&layers), 1, "list_layers");

        let objects = adapter();
        objects.list_objects_page(0, None).unwrap();
        assert_eq!(entries(&objects), 1, "list_objects");

        let object = adapter();
        let selector = sample_selector(&object);
        object.get_object(&selector).unwrap();
        assert_eq!(entries(&object), 1, "get_object");

        // effect のカタログは編集ハンドルから直接得られ、参照区間を必要としない。
        let effects = adapter();
        effects.list_available_effects_page(None).unwrap();
        assert_eq!(entries(&effects), 0, "list_available_effects");

        // 設定項目の列挙も説明の取得も編集ハンドルの外側で完結する。
        let described = adapter();
        described
            .describe_effects(&describe_params(&["グロー", "ぼかし"]))
            .unwrap();
        assert_eq!(entries(&described), 0, "describe_effects");

        // フォントとモジュールの列挙も編集ハンドルの機能である。
        let fonts = adapter();
        fonts.list_fonts().unwrap();
        assert_eq!(entries(&fonts), 0, "list_fonts");

        let modules = adapter();
        modules.list_modules(None).unwrap();
        assert_eq!(entries(&modules), 0, "list_modules");

        // パレットは色の取得に区間が要る。名前の列挙も同じ区間の内側で行うため、
        // 入る回数は 1 度で足りる。
        let palettes = adapter();
        palettes.list_palettes_page().unwrap();
        assert_eq!(entries(&palettes), 1, "list_palettes");
    }

    #[test]
    fn list_object_aliases_reports_an_unavailable_directory_instead_of_panicking() {
        // 設定ハンドルが初期化されていない環境では、データディレクトリの解決が
        // panic で打ち切られる。捕捉層まで上げると「想定外の内部失敗」になり、
        // 要求元は他の tool も動かないものと読む。
        let adapter = adapter();
        let error = adapter
            .list_object_aliases(None, &default_page_window())
            .unwrap_err();

        assert!(matches!(error, ReadError::AliasDirectoryUnavailable));
        assert_eq!(
            error.error_code(),
            aviutl2_mcp_core::ErrorCode::UnsupportedOperation
        );
        // SDK を 1 度も呼ばない。準備状態の問い合わせも参照区間も通らない。
        assert!(adapter.host.calls().is_empty(), "SDK を呼びました");
    }

    #[test]
    fn list_fonts_returns_every_registered_name() {
        let adapter = adapter();
        let snapshot = adapter.list_fonts().unwrap();
        assert_eq!(snapshot.items, fake_fonts());
    }

    #[test]
    fn list_modules_filters_by_type() {
        let adapter = adapter();
        let all = adapter.list_modules(None).unwrap();
        assert_eq!(all.items.len(), fake_modules().len());

        let inputs = adapter
            .list_modules(Some(&ModuleType::PluginInput))
            .unwrap();
        assert_eq!(inputs.items.len(), 1);
        assert_eq!(inputs.items[0].name, "入力プラグイン");

        let none = adapter
            .list_modules(Some(&ModuleType::ScriptCamera))
            .unwrap();
        assert!(none.items.is_empty());
    }

    #[test]
    fn list_palettes_returns_the_fixed_number_of_colors() {
        let adapter = adapter();
        let result = adapter.list_palettes_page().unwrap();
        assert_eq!(result.items.len(), fake_palette_names().len());
        for palette in &result.items {
            assert_eq!(
                palette.colors.len(),
                PALETTE_COLOR_COUNT,
                "{} の色数",
                palette.name
            );
            assert_eq!(palette.colors.len(), 64);
        }
    }

    #[test]
    fn list_palettes_reads_the_colors_of_the_page_only() {
        // 色は 1 件あたり 64 個ある。応答へ載せない分まで読むと、参照区間の
        // 保持時間が要求ページではなく登録数で決まってしまう。
        let adapter = adapter();
        let result = adapter
            .list_palettes(&page_request(1, 2, None).window())
            .unwrap();

        let read_colors = adapter
            .host
            .calls()
            .iter()
            .filter(|call| **call == "palette_colors")
            .count();
        assert_eq!(result.items.len(), 2);
        assert_eq!(read_colors, 2, "窓の外まで色を読んでいます");
        assert!(
            fake_palette_names().len() > 2,
            "全件と窓が同じ件数では読み過ぎを検出できません"
        );
    }

    #[test]
    fn list_palettes_drops_only_the_palette_whose_colors_are_missing() {
        // 列挙が返した名前で情報が取れないのは異常だが、その 1 件のために一覧
        // 全体を落とさない。落としたことは総件数に現れる。
        let missing = "暖色".to_string();
        let adapter = adapter_with(|_| {
            let mut host = FakeHost::new();
            host.palettes_without_colors = vec![missing.clone()];
            host
        });
        let result = adapter.list_palettes_page().unwrap();

        let names: Vec<&str> = result
            .items
            .iter()
            .map(|palette| palette.name.as_str())
            .collect();
        assert!(!names.contains(&missing.as_str()), "{names:?}");
        assert_eq!(names.len(), fake_palette_names().len() - 1);
        assert_eq!(
            result.page.total_count as usize,
            fake_palette_names().len() - 1,
            "落とした件数が総件数に反映されていません"
        );
        assert_eq!(result.page.count as usize, names.len());
    }

    #[test]
    fn list_palettes_returns_a_null_current_name_when_the_host_does_not_name_one() {
        // 現在のパレット名は付随情報である。取れないことで一覧を落とさない。
        let adapter = adapter_with(|_| {
            let mut host = FakeHost::new();
            host.current_palette = None;
            host
        });
        let result = adapter.list_palettes_page().unwrap();

        assert_eq!(result.current, None);
        assert_eq!(result.items.len(), fake_palette_names().len());
    }

    #[test]
    fn list_palettes_names_the_colors_of_the_palette_it_reports() {
        // 名前と色の組を取り違える実装は、色が名前の関数であることで現れる。
        let adapter = adapter();
        let result = adapter.list_palettes_page().unwrap();
        for palette in &result.items {
            assert_eq!(
                palette.colors,
                fake_palette_colors(&palette.name),
                "{} の色が別のパレットのものです",
                palette.name
            );
        }
    }

    #[test]
    fn read_results_do_not_expose_handles() {
        let adapter = adapter();
        let selector = sample_selector(&adapter);
        let mut documents = vec![
            serde_json::to_string(&adapter.get_edit_info().unwrap()).unwrap(),
            serde_json::to_string(&adapter.get_object(&selector).unwrap()).unwrap(),
            serde_json::to_string(&adapter.list_objects_page(0, None).unwrap().items).unwrap(),
            serde_json::to_string(&adapter.list_layers(0).unwrap().items).unwrap(),
        ];
        documents.push(serde_json::to_string(&adapter.get_current_scene().unwrap().0).unwrap());
        documents.push(serde_json::to_string(&adapter.list_fonts().unwrap().items).unwrap());
        documents.push(serde_json::to_string(&adapter.list_palettes_page().unwrap()).unwrap());
        documents.push(serde_json::to_string(&adapter.list_modules(None).unwrap().items).unwrap());
        // 選択の取得はハンドルを 2 段で受け取る唯一の読み取りである。3 件の
        // 選択とフォーカスを持つホストで確かめる。
        let selection = adapter_with(|_| selecting_host());
        documents.push(serde_json::to_string(&selection.get_selection_page(0).unwrap()).unwrap());

        for document in documents {
            let lowered = document.to_lowercase();
            for forbidden in ["handle", "pointer", "0x"] {
                assert!(
                    !lowered.contains(forbidden),
                    "{forbidden} が応答に含まれます: {document}"
                );
            }
        }
    }

    /// 一覧から算出した fingerprint と、詳細から算出した fingerprint が一致する
    /// ことを確かめる。
    ///
    /// 食い違えば、一覧が返したセレクターで詳細を引けなくなり、対象が事実上
    /// 到達不能になる。
    #[test]
    fn object_fingerprint_agrees_between_listing_and_detail() {
        let adapter = adapter();
        let summaries = adapter.list_objects_page(0, None).unwrap().items;
        assert!(!summaries.is_empty());

        for summary in summaries {
            let detail = adapter
                .get_object(&summary.selector)
                .expect("一覧が返したセレクターで詳細を引けません");
            assert_eq!(
                detail.summary.selector.fingerprint,
                summary.selector.fingerprint
            );
            assert_eq!(detail.summary.selector, summary.selector);
        }
    }

    /// 配下 effect を持つ対象でも両経路が一致することを確かめる。
    ///
    /// 一方が effect を読み、他方が読まない経路を通るため、材料が食い違えば
    /// ここで落ちる。
    #[test]
    fn object_fingerprint_agrees_for_an_object_that_has_effects() {
        let adapter = adapter();
        let summary = adapter
            .list_objects_page(0, None)
            .unwrap()
            .items
            .into_iter()
            .find(|item| item.layer == 1 && item.frame_start == 100)
            .expect("配下 effect を持つ対象がありません");

        let detail = adapter.get_object(&summary.selector).unwrap();
        assert!(!detail.effects.is_empty());
        assert_eq!(
            detail.summary.selector.fingerprint,
            summary.selector.fingerprint
        );
    }

    /// 立ち絵オブジェクトの effect を差し替えたホストを組み立てる。
    fn host_with_effects(effects: Vec<HostEffect>) -> FakeHost {
        let mut layers = fake_layers();
        layers[1].objects[0] = object_with_effects(1, 100, 200, Some("立ち絵"), effects);
        FakeHost {
            layers,
            ..FakeHost::new()
        }
    }

    /// 列挙からレイヤー 1・フレーム 100 の概要を取り出す。
    fn listed_sample(adapter: &HostReadAdapter<FakeHost>) -> ObjectSummary {
        adapter
            .list_objects_page(0, None)
            .unwrap()
            .items
            .into_iter()
            .find(|item| item.layer == 1 && item.frame_start == 100)
            .expect("対象がありません")
    }

    /// effect の設定を変えると、そのオブジェクトの fingerprint も変わることを
    /// 確かめる。
    ///
    /// 変わらなければ、effect を書き換えた後も変更前のセレクターが一致し続け、
    /// 古いセレクターでの変更を拒否できない。材料に effect の列は無く、alias が
    /// 配下 effect の設定値を含むことだけがこの性質を支えている。列挙は effect を
    /// 読まないため、ここで変わるのは alias 経由でしかあり得ない。
    ///
    /// epoch を揃えるためプロジェクト状態は共用し、差分を effect の設定だけに
    /// する。
    #[test]
    fn object_fingerprint_changes_when_an_effect_setting_changes() {
        let project = Arc::new(ProjectState::new());
        let fingerprint_of = |path: &'static str| {
            let adapter = HostReadAdapter::new(
                host_with_effects(vec![file_effect("動画ファイル", 0, path)]),
                Arc::clone(&project),
            );
            let fingerprint = listed_sample(&adapter).selector.fingerprint;
            assert!(
                !adapter.host.calls().contains(&EFFECT_LIST),
                "列挙が effect を読みました: {:?}",
                adapter.host.calls()
            );
            fingerprint
        };

        assert_ne!(
            fingerprint_of(r"C:\movie.mp4"),
            fingerprint_of(r"C:\another.mp4"),
            "effect の設定を変えても fingerprint が変わりません"
        );
    }

    /// effect のロック状態を変えると、そのオブジェクトの fingerprint も変わる
    /// ことを確かめる。
    ///
    /// ロックは alias の節へ書き出される。設定値と同じく、列挙が effect を
    /// 読まないままオブジェクトの同一性へ伝わる。
    #[test]
    fn object_fingerprint_changes_when_an_effect_lock_changes() {
        let project = Arc::new(ProjectState::new());
        let fingerprint_of = |locked: bool| {
            let effect = HostEffect {
                locked,
                ..file_effect("動画ファイル", 0, r"C:\movie.mp4")
            };
            let adapter =
                HostReadAdapter::new(host_with_effects(vec![effect]), Arc::clone(&project));
            let fingerprint = listed_sample(&adapter).selector.fingerprint;
            assert!(
                !adapter.host.calls().contains(&EFFECT_LIST),
                "列挙が effect を読みました: {:?}",
                adapter.host.calls()
            );
            fingerprint
        };

        assert_ne!(
            fingerprint_of(false),
            fingerprint_of(true),
            "effect のロックを変えても fingerprint が変わりません"
        );
    }

    /// effect の有効状態を変えると、そのオブジェクトの fingerprint も変わる
    /// ことを確かめる。
    ///
    /// 有効状態は alias の節へ書き出される。設定値やロックと同じく、列挙が
    /// effect を読まないままオブジェクトの同一性へ伝わる。
    #[test]
    fn object_fingerprint_changes_when_an_effect_enabled_changes() {
        let project = Arc::new(ProjectState::new());
        let fingerprint_of = |enabled: bool| {
            let effect = HostEffect {
                enabled,
                ..file_effect("動画ファイル", 0, r"C:\movie.mp4")
            };
            let adapter =
                HostReadAdapter::new(host_with_effects(vec![effect]), Arc::clone(&project));
            let fingerprint = listed_sample(&adapter).selector.fingerprint;
            assert!(
                !adapter.host.calls().contains(&EFFECT_LIST),
                "列挙が effect を読みました: {:?}",
                adapter.host.calls()
            );
            fingerprint
        };

        assert_ne!(
            fingerprint_of(true),
            fingerprint_of(false),
            "effect の有効状態を変えても fingerprint が変わりません"
        );
    }

    /// 列挙が配下 effect を読まないことを確かめる。
    ///
    /// 読めば 1 ページあたりの SDK 呼び出しが effect 数と設定項目数に比例して
    /// 増え、窓内の 1 件の effect が読めないだけでページ全体が失敗する。
    #[test]
    fn list_objects_does_not_read_effects() {
        let adapter = adapter();
        let page = adapter.list_objects_page(0, None).unwrap();

        assert_eq!(page.items.len(), 3);
        assert!(
            !adapter.host.calls().contains(&EFFECT_LIST),
            "列挙が effect を読みました: {:?}",
            adapter.host.calls()
        );
        assert_eq!(detail_reads(&adapter), 0);
    }

    /// 3 件を選択し、そのうち 1 件をフォーカスしたホスト。
    ///
    /// 選択はレイヤーと開始フレームの昇順とは**逆の順序**で返す。ホストが返す
    /// 順序は規定されておらず、並べ替えを我々が行うことを確かめられる。
    fn selecting_host() -> FakeHost {
        FakeHost {
            selected: vec![(1, 300), (1, 100), (0, 0)],
            focus: Some((1, 100)),
            focus_section: Some(1),
            ..FakeHost::new()
        }
    }

    /// 位置の列をレイヤー・開始フレームの昇順へ並べ替えた複製。
    ///
    /// 並べ替えを確かめる検査が、フェイクの並びで骨抜きにならないことを表明する
    /// ために用いる。
    fn ascending(positions: &[(usize, usize)]) -> Vec<(usize, usize)> {
        let mut sorted = positions.to_vec();
        sorted.sort();
        sorted
    }

    /// ホストが逆順で返しても、選択がレイヤー・開始フレームの昇順で返ることを
    /// 確かめる。
    ///
    /// ページ間で順序が変わると、取りこぼしと重複が同時に起きる。オブジェクトの
    /// 列挙と同じ並びであることも併せて確かめる。要求元は 2 つの応答を
    /// 突き合わせられる。
    #[test]
    fn the_selection_is_ordered_by_layer_and_start_frame() {
        let adapter = adapter_with(|_| selecting_host());
        // ホストが既に昇順で返していれば、並べ替えを外した実装でも同じ結果に
        // なる。フェイクの並びが期待と違うことを先に押さえる。
        assert_ne!(
            adapter.host.selected,
            ascending(&adapter.host.selected),
            "フェイクが昇順で返しています"
        );

        let snapshot = adapter.get_selection_page(0).unwrap();

        let positions: Vec<(usize, usize)> = snapshot
            .selected
            .iter()
            .map(|object| (object.layer, object.frame_start))
            .collect();
        assert_eq!(positions, vec![(0, 0), (1, 100), (1, 300)]);

        let enumerated: Vec<(usize, usize)> = adapter
            .list_objects_page(0, None)
            .unwrap()
            .items
            .iter()
            .map(|object| (object.layer, object.frame_start))
            .collect();
        assert_eq!(positions, enumerated, "列挙と並びが違います");
    }

    /// 選択の alias を読むのがページの窓に入った分だけであることを確かめる。
    ///
    /// 応答へ載せない対象まで読むと、参照区間の保持時間が要求ページではなく
    /// 選択の規模で決まってしまう。
    #[test]
    fn the_selection_reads_aliases_only_within_the_page() {
        let adapter = adapter_with(|_| selecting_host());
        // 窓の位置で並べ替えの有無を見分ける検査である。ホストが既に昇順で
        // 返していれば、並べ替えを外した実装でも同じ対象が窓に入る。
        assert_ne!(
            adapter.host.selected,
            ascending(&adapter.host.selected),
            "フェイクが昇順で返しています"
        );

        let snapshot = adapter
            .get_selection(0, &page_request(0, 1, None))
            .unwrap()
            .unwrap();

        // 総件数は選択全体の件数であり、窓の件数ではない。
        assert_eq!(snapshot.page.total_count, 3);
        assert_eq!(snapshot.page.offset, 0);
        assert!(snapshot.page.has_more);
        assert_eq!(snapshot.selected.len(), 1);
        // 並べ替えた後の先頭である。ホストが返す順序のまま切り出せば、末尾に
        // 居るはずの対象が返る。
        assert_eq!(
            (snapshot.selected[0].layer, snapshot.selected[0].frame_start),
            (0, 0)
        );

        assert_eq!(
            identity_reads(&adapter),
            1,
            "窓の外の対象まで alias を読んでいます: {:?}",
            adapter.host.calls()
        );
    }

    /// 選択の一覧が配下 effect を読まないことを確かめる。
    #[test]
    fn the_selection_does_not_read_effects() {
        let adapter = adapter_with(|_| selecting_host());
        let snapshot = adapter.get_selection_page(0).unwrap();

        assert_eq!(snapshot.selected.len(), 3);
        assert!(
            !adapter.host.calls().contains(&EFFECT_LIST),
            "選択の取得が effect を読みました: {:?}",
            adapter.host.calls()
        );
    }

    /// フォーカス対象とその区間番号が同じ組で返ることを確かめる。
    #[test]
    fn the_focused_object_carries_its_section_number() {
        let adapter = adapter_with(|_| selecting_host());
        let snapshot = adapter.get_selection_page(0).unwrap();

        let focus = snapshot.focus.expect("フォーカス対象がありません");
        assert_eq!((focus.layer, focus.frame_start), (1, 100));
        assert_eq!(snapshot.focus_section, Some(1));
    }

    /// フォーカス対象が居るのに区間番号が得られない場合を確かめる。
    ///
    /// ラッパーはホストの `-1` を `None` へ写す。番号だけが落ちても対象は返る。
    #[test]
    fn a_focused_object_without_a_section_number_still_returns_the_object() {
        let adapter = adapter_with(|_| FakeHost {
            focus_section: None,
            ..selecting_host()
        });
        let snapshot = adapter.get_selection_page(0).unwrap();

        assert!(snapshot.focus.is_some());
        assert_eq!(snapshot.focus_section, None);
    }

    /// フォーカス対象が無ければ区間番号も無いことを確かめる。
    ///
    /// 区間番号は対象の性質である。ホストが対象を返さないまま番号だけを返しても、
    /// 対象と番号の食い違った組を応答へ載せない。
    #[test]
    fn an_unfocused_selection_carries_no_section_number() {
        let adapter = adapter_with(|_| FakeHost {
            focus: None,
            focus_section: Some(3),
            ..selecting_host()
        });
        let snapshot = adapter.get_selection_page(0).unwrap();

        assert_eq!(snapshot.focus, None);
        assert_eq!(snapshot.focus_section, None);
    }

    /// フォーカス対象と区間番号を同じ参照区間の内側で読むことを確かめる。
    ///
    /// 別の区間に分けると、間に利用者の操作が入って両者が食い違った組を返し得る。
    #[test]
    fn the_focus_and_its_section_are_read_in_the_same_section() {
        let adapter = adapter_with(|_| selecting_host());
        adapter.get_selection_page(0).unwrap();

        let calls = adapter.host.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|call| **call == "enter_read_section")
                .count(),
            1,
            "参照区間へ複数回入りました: {calls:?}"
        );
        let entered = calls
            .iter()
            .position(|call| *call == "enter_read_section")
            .expect("参照区間へ入っていません");
        for call in ["focused_object", "focus_section"] {
            let at = calls
                .iter()
                .position(|recorded| *recorded == call)
                .unwrap_or_else(|| panic!("{call} が呼ばれていません: {calls:?}"));
            assert!(
                at > entered,
                "{call} が参照区間の外で呼ばれました: {calls:?}"
            );
        }
    }

    /// シーンの guard が対象を読む前に効くことを確かめる。
    #[test]
    fn get_selection_rejects_a_different_scene_before_reading_objects() {
        let adapter = adapter_with(|_| selecting_host());
        let error = adapter.get_selection_page(7).unwrap_err();

        assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
        assert_eq!(error.details()["expected_scene_id"], 7);
        assert_eq!(identity_reads(&adapter), 0);
    }

    /// 選択の取得がページ間の revision 照合を行うことを確かめる。
    ///
    /// 選択はプロジェクトの状態であり、revision に連動する。
    #[test]
    fn get_selection_rejects_a_stale_snapshot_revision() {
        let adapter = adapter_with(|_| selecting_host());
        let error = adapter
            .get_selection(0, &page_request(0, 50, Some(99)))
            .unwrap()
            .unwrap_err();

        assert_eq!(
            error,
            SnapshotRevisionMismatch {
                requested: 99,
                current: 0,
            }
        );
        assert_eq!(identity_reads(&adapter), 0);
    }

    /// 選択が 0 件でもフォーカス対象は返ることを確かめる。
    ///
    /// タイムライン上の選択とオブジェクト設定ウィンドウの選択は別物である。
    #[test]
    fn an_empty_selection_still_carries_the_focus() {
        let adapter = adapter_with(|_| FakeHost {
            selected: Vec::new(),
            ..selecting_host()
        });
        let snapshot = adapter.get_selection_page(0).unwrap();

        assert!(snapshot.selected.is_empty());
        assert_eq!(snapshot.page.total_count, 0);
        assert!(snapshot.focus.is_some());
    }

    /// 応答に現れる項目名を、入れ子をたどって全て集める。
    fn field_names(value: &serde_json::Value) -> std::collections::BTreeSet<String> {
        let mut names = std::collections::BTreeSet::new();
        match value {
            serde_json::Value::Object(map) => {
                for (key, nested) in map {
                    names.insert(key.clone());
                    names.extend(field_names(nested));
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    names.extend(field_names(item));
                }
            }
            _ => {}
        }
        names
    }

    /// 選択の応答が持つ項目を表として固定する。
    ///
    /// ハンドルは参照区間の内側で位置と同一性の材料へ写し切る。名前で探す検査は
    /// `handle` を含まない名前を付けた項目を見逃すため、項目の集合そのものを
    /// 固定して、区間の外へ持ち出す値が増えたことをここで落とす。
    #[test]
    fn the_selection_response_carries_only_position_and_identity() {
        let adapter = adapter_with(|_| selecting_host());
        let snapshot = adapter.get_selection_page(0).unwrap();
        let value = serde_json::to_value(&snapshot).expect("直列化できます");

        let expected: std::collections::BTreeSet<String> = [
            "project_revision",
            "focus",
            "focus_section",
            "selected",
            "page",
            "layer",
            "frame_start",
            "frame_end",
            "name",
            "selector",
            "fingerprint",
            "project_epoch",
            "scene_id",
            "frame",
            "total_count",
            "count",
            "offset",
            "has_more",
            "next_offset",
            "snapshot_revision",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert_eq!(field_names(&value), expected);
    }

    /// 選択が返したセレクターで対象を引けることを確かめる。
    ///
    /// 「対話利用の起点」の中身である。返ってきた対象をそのまま編集へ渡せる。
    #[test]
    fn the_selection_returns_usable_selectors() {
        let adapter = adapter_with(|_| selecting_host());
        let snapshot = adapter.get_selection_page(0).unwrap();

        let focus = snapshot.focus.expect("フォーカス対象がありません");
        let detail = adapter
            .get_object(&focus.selector)
            .expect("フォーカス対象のセレクターで引けません");
        assert_eq!(detail.summary, focus);

        for object in &snapshot.selected {
            adapter
                .get_object(&object.selector)
                .expect("選択のセレクターで引けません");
        }
    }

    /// 対象を指定する取得は配下 effect を読むことを確かめる。
    ///
    /// 応答が effect の一覧を返すため、読まなければ組み立てられない。
    #[test]
    fn get_object_reads_effects() {
        let adapter = adapter();
        let selector = sample_selector(&adapter);
        let detail = adapter.get_object(&selector).unwrap();

        assert!(!detail.effects.is_empty());
        assert!(
            adapter.host.calls().contains(&EFFECT_LIST),
            "effect を読まずに一覧を返しました: {:?}",
            adapter.host.calls()
        );
    }

    /// 配下 effect が読めなくても列挙が成功することを確かめる。
    ///
    /// 列挙が effect を読んでいれば、窓に入った 1 件の失敗がページ全体を SDK の
    /// 失敗へ落とす。対象を 1 つも指定していない要求が、応答に現れない値の
    /// 読み取り失敗で丸ごと失敗する経路である。
    #[test]
    fn list_objects_survives_a_failing_effect_read() {
        let adapter = adapter_with(|_| FakeHost {
            effects_fail_at: Some(100),
            ..FakeHost::new()
        });

        let page = adapter
            .list_objects_page(0, None)
            .expect("effect の読み取り失敗が列挙を巻き込みました");
        assert_eq!(page.items.len(), 3);
    }

    /// 配下 effect が読めなくても対象の fingerprint が変わらないことを確かめる。
    ///
    /// effect の一覧は 0 件と取得失敗を区別しない。推定が同一性の材料に入って
    /// いれば、一過性の失敗で fingerprint が揺れ、直前に返したセレクターが
    /// 拒否される。
    #[test]
    fn a_failing_effect_read_does_not_shift_the_object_fingerprint() {
        let project = Arc::new(ProjectState::new());
        let healthy = HostReadAdapter::new(FakeHost::new(), Arc::clone(&project));
        let failing = HostReadAdapter::new(
            FakeHost {
                effects_fail_at: Some(100),
                ..FakeHost::new()
            },
            Arc::clone(&project),
        );

        assert_eq!(
            listed_sample(&healthy).selector.fingerprint,
            listed_sample(&failing).selector.fingerprint,
            "effect の読み取り失敗で fingerprint が揺れました"
        );
    }

    /// 同名 effect が繰り上がった場合に、残った effect が別物として扱われる
    /// ことを確かめる。
    ///
    /// 名前と同名内の番号だけを材料にすると、繰り上がった側が取り除く前の
    /// 先頭と同じ fingerprint になり、別のインスタンスへ変更が当たる。
    #[test]
    fn effect_fingerprint_changes_when_the_preceding_effect_is_removed() {
        let adapter_for = |effects: Vec<HostEffect>| adapter_with(|_| host_with_effects(effects));
        let fingerprints_of = |adapter: &HostReadAdapter<FakeHost>| {
            let summary = listed_sample(adapter);
            adapter
                .get_object(&summary.selector)
                .unwrap()
                .effects
                .into_iter()
                .map(|effect| effect.selector.fingerprint)
                .collect::<Vec<_>>()
        };

        // 同じ設定の同名 effect が 2 つ並ぶ。
        let before = adapter_for(vec![
            file_effect("ぼかし", 0, r"C:\a.png"),
            file_effect("ぼかし", 1, r"C:\a.png"),
        ]);
        // 前方の 1 つが取り除かれ、残った側の番号が 0 へ繰り上がる。
        let after = adapter_for(vec![file_effect("ぼかし", 0, r"C:\a.png")]);

        assert_ne!(
            fingerprints_of(&before)[0],
            fingerprints_of(&after)[0],
            "繰り上がった effect が取り除く前の先頭と同じ値になりました"
        );
    }

    /// 列挙の失敗へ畳んだ後も、不在を検出した呼び出しを指すことを確かめる。
    ///
    /// 検出元を決め打ちすると、切り分けが誤った系統へ向かう。
    #[test]
    fn enumeration_failure_keeps_the_detecting_call() {
        for detected_by in ["find_object", "get_effect_list"] {
            let folded = enumeration_failure(ReadError::ObjectNotFound { detected_by });
            assert_eq!(folded.error_code(), ErrorCode::SdkError);
            assert_eq!(folded.details()["sdk_operation"], detected_by);
        }
        // 不在以外の失敗は分類を変えない。
        let untouched = enumeration_failure(ReadError::FingerprintMismatch {
            current_object: Box::new(crate::test_support::sample_object_summary()),
        });
        assert_eq!(untouched.error_code(), ErrorCode::PreconditionFailed);
    }

    #[test]
    fn layer_range_is_clamped_to_existing_layers() {
        assert_eq!(layer_range(None, 5), 0..=5);
        let filter = ObjectFilter {
            layer_min: Some(2),
            layer_max: Some(9),
        };
        assert_eq!(layer_range(Some(&filter), 5), 2..=5);
    }

    #[test]
    fn selector_fingerprint_is_canonical() {
        let adapter = adapter();
        let selector = sample_selector(&adapter);
        let parsed: Fingerprint = selector.fingerprint.as_str().parse().unwrap();
        assert_eq!(parsed, selector.fingerprint);
    }
}
