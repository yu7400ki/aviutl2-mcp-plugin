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
mod tests;
