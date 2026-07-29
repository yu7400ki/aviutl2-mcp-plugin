//! 読み取り operation の手順。
//!
//! SDK 呼び出しは [`ReadHost`] へ委ね、ここでは受付可否の判定・参照区間の
//! 使い方・セレクターの解決・DTO の組み立てだけを行う。SDK の型は現れない。

use crate::project::ProjectState;
use crate::read::error::ReadError;
use crate::read::host::{
    EditState, HostEditInfo, HostEffect, HostObject, HostObjectDetail, HostObjectPlacement,
    ReadHost, SceneReader,
};
use crate::read::{ProjectStatus, ReadAdapter, Snapshot};
use aviutl2_mcp_core::FingerprintAlgorithm;
use aviutl2_mcp_core::{
    AvailableEffect, Cursor, DisplayRange, EditInfo, EffectFingerprintInput, EffectInfo,
    EffectType, Extent, FiniteF64, FrameRange, LayerInfo, ObjectDetail, ObjectFilter,
    ObjectFingerprintInput, ObjectSelector, ObjectSummary, SceneInfo, effect_fingerprint,
};
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

    /// 登録済み effect のカタログを取得する。
    fn effect_catalog(&self) -> Result<Vec<AvailableEffect>, ReadError> {
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
        F: FnOnce(&dyn SceneReader) -> Result<T, ReadError> + Send,
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
            selected_range: selected_range(&info),
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
    ) -> Result<Snapshot<ObjectSummary>, ReadError> {
        self.ensure_readable()?;
        let info = self.edit_info()?;
        ensure_scene(&info, expected_scene_id)?;
        let layers = layer_range(filter, info.layer_max);
        let scene_id = info.scene_id;
        let epoch = self.project.epoch();
        let epoch = epoch.as_str();
        let project = self.project.as_ref();

        let (snapshot_revision, items) = self.read_section(move |scene| {
            let revision = project.revision();
            let mut items = Vec::new();
            for layer in layers {
                for placement in scene.object_placements(layer)? {
                    let detail = scene.object_detail(layer, placement.frame_start)?;
                    items.push(object_summary(epoch, scene_id, &detail.object));
                }
            }
            Ok((revision, items))
        })?;

        Ok(Snapshot {
            items,
            snapshot_revision,
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
        if selector.fingerprint_algorithm != FingerprintAlgorithm::GENERATED {
            return Err(ReadError::FingerprintAlgorithmMismatch {
                requested: selector.fingerprint_algorithm.to_string(),
                supported: FingerprintAlgorithm::GENERATED.to_string(),
            });
        }

        let scene_id = info.scene_id;
        let layer = selector.layer;
        let frame = selector.frame;
        let required_name = selector.name.as_deref();
        let expected_fingerprint = &selector.fingerprint;
        let epoch = epoch.as_str();
        let project = self.project.as_ref();

        self.read_section(move |scene| {
            let revision = project.revision();
            // 候補の絞り込みは位置と名前だけで決まる。ここでレイヤー内の全対象の
            // alias と effect まで読むと、無関係な対象の読み取り失敗が要求全体を
            // 巻き込み、対象自体は健全なのに取得できなくなる。
            let candidate =
                resolve_candidate(scene.object_placements(layer)?, frame, required_name)?;
            // 詳細を読み、その内容から fingerprint を組み立てて照合する。
            // 照合した対象と応答へ載せる対象が同じ読み取りに由来することが、
            // これで構造として保証される。
            let detail = scene.object_detail(layer, candidate.frame_start)?;
            let summary = object_summary(epoch, scene_id, &detail.object);
            if summary.fingerprint != *expected_fingerprint {
                return Err(ReadError::FingerprintMismatch);
            }
            Ok(object_detail(summary, revision, detail))
        })
    }

    fn list_available_effects(
        &self,
        effect_type: Option<&EffectType>,
    ) -> Result<Snapshot<AvailableEffect>, ReadError> {
        self.ensure_readable()?;
        // カタログは参照区間を必要としないため、列挙の直前の revision を採る。
        let snapshot_revision = self.project.revision();
        let mut items = self.effect_catalog()?;
        if let Some(effect_type) = effect_type {
            items.retain(|effect| effect.effect_type == *effect_type);
        }
        Ok(Snapshot {
            items,
            snapshot_revision,
        })
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

/// 開始フレームの完全一致と名前の一致で候補を 1 件へ絞る。
///
/// 「指定フレーム以降」の探索結果をそのまま候補にしない。セレクターの `frame` は
/// 対象の開始フレームであり、途中フレームでの重なりを表さない。
fn resolve_candidate(
    objects: Vec<HostObjectPlacement>,
    frame: usize,
    required_name: Option<&str>,
) -> Result<HostObjectPlacement, ReadError> {
    let mut candidates: Vec<HostObjectPlacement> = objects
        .into_iter()
        .filter(|object| object.frame_start == frame && matches_name(required_name, object))
        .collect();

    match candidates.len() {
        0 => Err(ReadError::ObjectNotFound),
        1 => Ok(candidates.remove(0)),
        candidate_count => Err(ReadError::AmbiguousObject { candidate_count }),
    }
}

/// 名前が指定されている場合に一致を必須とする。
fn matches_name(required_name: Option<&str>, object: &HostObjectPlacement) -> bool {
    match required_name {
        None => true,
        Some(name) => object.name.as_deref() == Some(name),
    }
}

/// effect 列の各要素について fingerprint の入力を組み立てる。
///
/// 列の絶対位置と総数も材料に含めるため、要素を単独では組み立てられない。
/// 一覧と詳細で同じ列から同じ入力が得られるよう、組み立てはここへ集約する。
fn effect_fingerprint_inputs(
    effects: &[HostEffect],
) -> impl Iterator<Item = EffectFingerprintInput<'_>> {
    let effect_count = effects.len();
    effects
        .iter()
        .enumerate()
        .map(move |(position, effect)| EffectFingerprintInput {
            effect_name: &effect.name,
            effect_index: effect.index,
            position,
            effect_count,
            enabled: effect.enabled,
            locked: effect.locked,
            items: &effect.items,
        })
}

/// オブジェクトの概要を組み立てる。
///
/// 配下 effect の fingerprint 列もオブジェクトの材料であるため、ここで
/// 併せて算出する。入力になる [`HostObject`] は詳細の読み取りだけが返すため、
/// 一覧も詳細も同じ材料から同じ fingerprint を得る。
fn object_summary(epoch: &str, scene_id: i32, object: &HostObject) -> ObjectSummary {
    let effect_fingerprints: Vec<_> = effect_fingerprint_inputs(&object.effects)
        .map(effect_fingerprint)
        .collect();
    ObjectSummary::new(
        epoch,
        ObjectFingerprintInput {
            scene_id,
            layer: object.placement.layer,
            frame_start: object.placement.frame_start,
            frame_end: object.placement.frame_end,
            name: object.placement.name.as_deref(),
            alias: &object.alias,
            effect_fingerprints: &effect_fingerprints,
        },
    )
}

/// オブジェクトの詳細を、算出済みの概要と組み合わせて組み立てる。
fn object_detail(summary: ObjectSummary, revision: u64, detail: HostObjectDetail) -> ObjectDetail {
    let effects = effect_fingerprint_inputs(&detail.object.effects)
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

/// シーン情報を組み立てる。
fn scene_info(info: &HostEditInfo, name: Option<String>) -> SceneInfo {
    SceneInfo {
        id: info.scene_id,
        name,
        width: info.width,
        height: info.height,
        fps: fps(info.fps_rate, info.fps_scale),
        fps_rate: info.fps_rate,
        fps_scale: info.fps_scale,
        sample_rate: info.sample_rate,
    }
}

/// フレームレートを算出する。分母が 0 の場合は算出できない。
fn fps(rate: i32, scale: i32) -> Option<FiniteF64> {
    if scale == 0 {
        return None;
    }
    FiniteF64::try_new(f64::from(rate) / f64::from(scale))
}

/// フレーム範囲選択を組み立てる。片側しか得られない場合は未選択として扱う。
fn selected_range(info: &HostEditInfo) -> Option<FrameRange> {
    match (info.select_range_start, info.select_range_end) {
        (Some(start), Some(end)) => Some(FrameRange { start, end }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read::host::HostLayer;
    use crate::test_support::with_silent_panic_hook;
    use aviutl2_mcp_core::{
        AvailableEffectItem, EffectFlags, EffectItem, EffectItemType, ErrorCode, Fingerprint,
        ItemValue, SectionRange,
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

    /// テスト用のレイヤー。
    #[derive(Debug, Clone)]
    struct FakeLayer {
        name: Option<String>,
        enabled: bool,
        locked: bool,
        objects: Vec<HostObject>,
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
        scene_name: Option<String>,
        grid_bpm: Vec<FiniteF64>,
        layers: Vec<FakeLayer>,
        catalog: Vec<AvailableEffect>,
        panic_at: Option<PanicPoint>,
        /// 詳細の読み取りを失敗させる対象の開始フレーム。
        ///
        /// 特定のオブジェクトだけが読めない状況を作り、他の対象の読み取りが
        /// 巻き込まれないことを確かめるために用いる。
        detail_fails_at: Option<usize>,
        /// 参照区間の確保そのものを失敗させる。
        section_fails: bool,
        /// 参照区間へ入る直前に進めるプロジェクト revision の回数。
        bump_on_enter: u64,
        /// 参照区間へ入る直前にプロジェクト境界を更新するか。
        ///
        /// 境界は非再入の Mutex で守られている。読み取りが区間を跨いでそれを
        /// 保持していれば、この更新で待ち合わせが解けなくなる。
        renew_boundary_on_enter: bool,
        project: Option<Arc<ProjectState>>,
        calls: Mutex<Vec<&'static str>>,
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                ready: true,
                state: EditState::Edit,
                later_state: None,
                edit_state_calls: AtomicUsize::new(0),
                info: fake_edit_info(),
                scene_name: Some("Scene 1".to_string()),
                grid_bpm: vec![FiniteF64::try_new(120.0).unwrap()],
                layers: fake_layers(),
                catalog: fake_catalog(),
                panic_at: None,
                detail_fails_at: None,
                section_fails: false,
                bump_on_enter: 0,
                renew_boundary_on_enter: false,
                project: None,
                calls: Mutex::new(Vec::new()),
            }
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
            Ok(self.info.clone())
        }

        fn effect_catalog(&self) -> Result<Vec<AvailableEffect>, ReadError> {
            self.assert_ready("get_effects");
            self.record("effect_catalog");
            Ok(self.catalog.clone())
        }

        fn enter_read_section<T, F>(&self, f: F) -> Result<T, ReadError>
        where
            T: Send + 'static,
            F: FnOnce(&dyn SceneReader) -> T + Send,
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

        fn grid_bpm(&self) -> Result<Vec<FiniteF64>, ReadError> {
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
                        .map(|object| object.placement.clone())
                        .collect()
                })
                .unwrap_or_default())
        }

        fn object_detail(
            &self,
            layer: usize,
            frame_start: usize,
        ) -> Result<HostObjectDetail, ReadError> {
            self.host.record("object_detail");
            let object = self
                .host
                .layers
                .get(layer)
                .and_then(|fake| {
                    fake.objects
                        .iter()
                        .find(|object| object.placement.frame_start == frame_start)
                })
                .ok_or(ReadError::ObjectNotFound)?;
            if self.host.detail_fails_at == Some(frame_start) {
                return Err(ReadError::Sdk {
                    operation: "get_effect_item_value",
                });
            }
            Ok(HostObjectDetail {
                sections: vec![SectionRange {
                    start: object.placement.frame_start,
                    end: object.placement.frame_end,
                }],
                object: object.clone(),
            })
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
            select_range_start: Some(10),
            select_range_end: Some(20),
        }
    }

    fn object(
        layer: usize,
        frame_start: usize,
        frame_end: usize,
        name: Option<&str>,
    ) -> HostObject {
        HostObject {
            placement: HostObjectPlacement {
                layer,
                frame_start,
                frame_end,
                name: name.map(str::to_string),
            },
            alias: format!("[{layer}:{frame_start}]"),
            effects: Vec::new(),
        }
    }

    /// 配下 effect を持つオブジェクト。
    fn object_with_effects(
        layer: usize,
        frame_start: usize,
        frame_end: usize,
        name: Option<&str>,
        effects: Vec<HostEffect>,
    ) -> HostObject {
        HostObject {
            effects,
            ..object(layer, frame_start, frame_end, name)
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

    fn fake_catalog() -> Vec<AvailableEffect> {
        vec![
            AvailableEffect {
                name: "ぼかし".to_string(),
                effect_type: EffectType::Filter,
                flags: EffectFlags::from_raw(1),
                items: vec![AvailableEffectItem {
                    name: "範囲".to_string(),
                    item_type: EffectItemType::Integer,
                }],
            },
            AvailableEffect {
                name: "動画ファイル".to_string(),
                effect_type: EffectType::Input,
                flags: EffectFlags::from_raw(3),
                items: Vec::new(),
            },
        ]
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
            adapter.list_objects(0, None).err().map(|e| e.error_code()),
            adapter.get_object(&selector).err().map(|e| e.error_code()),
            adapter
                .list_available_effects(None)
                .err()
                .map(|e| e.error_code()),
        ]
        .into_iter()
        .map(|code| code.expect("成功してしまいました"))
        .collect()
    }

    /// レイヤー 1・フレーム 100 のオブジェクトを指すセレクター。
    ///
    /// 材料はホストが保持する値から採る。配下 effect も fingerprint の材料で
    /// あるため、位置と名前だけを写した複製からは正しい値を組み立てられない。
    fn sample_selector(adapter: &HostReadAdapter<FakeHost>) -> ObjectSelector {
        let object = fake_layers()[1].objects[0].clone();
        object_summary(&adapter.project.epoch(), 0, &object).selector
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
    fn guard_converts_panic_into_internal_error() {
        let error = with_silent_panic_hook(|| {
            guard::<()>(|| panic!("参照区間の内側で panic させます")).unwrap_err()
        });
        assert_eq!(error.error_code(), ErrorCode::InternalError);
    }

    #[test]
    fn guard_passes_through_success_and_failure() {
        assert_eq!(guard(|| Ok(7)).unwrap(), 7);
        let error = guard::<()>(|| Err(ReadError::ObjectNotFound)).unwrap_err();
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
                adapter.list_objects(0, None).unwrap_err(),
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
                adapter.list_objects(0, None).unwrap_err(),
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
        assert_eq!(info.grid_bpm.len(), 1);
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
                select_range_start: None,
                select_range_end: None,
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
        // 件数のために名前と alias まで読むと、参照ロックを保持する時間が
        // オブジェクト数に比例して伸びる。
        let adapter = adapter();
        adapter.list_layers(0).unwrap();

        let calls = adapter.host.calls();
        assert!(calls.contains(&"object_count"), "{calls:?}");
        assert!(
            !calls.contains(&"objects_in_layer"),
            "件数のためにオブジェクトを列挙しています: {calls:?}"
        );
    }

    #[test]
    fn scene_guard_rejects_other_scene() {
        let adapter = adapter();
        for error in [
            adapter.list_layers(7).unwrap_err(),
            adapter.list_objects(7, None).unwrap_err(),
        ] {
            assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
            assert_eq!(error.details()["expected_scene_id"], 7);
            assert_eq!(error.details()["current_scene_id"], 0);
        }
    }

    #[test]
    fn list_objects_enumerates_every_layer_by_default() {
        let adapter = adapter();
        let snapshot = adapter.list_objects(0, None).unwrap();
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
        let snapshot = adapter.list_objects(0, Some(&filter)).unwrap();
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
            adapter.list_objects(0, Some(&filter)).unwrap().items.len(),
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
            .list_objects(0, Some(&filter))
            .expect("検証は呼び出し側の責務であり、ここでは失敗させない");
        assert!(snapshot.items.is_empty());
    }

    #[test]
    fn list_objects_selector_can_be_resolved() {
        let adapter = adapter();
        let snapshot = adapter.list_objects(0, None).unwrap();
        for summary in snapshot.items {
            let detail = adapter.get_object(&summary.selector).unwrap();
            assert_eq!(detail.summary.fingerprint, summary.fingerprint);
        }
    }

    #[test]
    fn get_object_returns_detail_for_matching_selector() {
        let adapter = adapter();
        let selector = sample_selector(&adapter);
        let detail = adapter.get_object(&selector).unwrap();

        assert_eq!(detail.summary.layer, 1);
        assert_eq!(detail.summary.frame_start, 100);
        assert_eq!(detail.alias, "[1:100]");
        assert_eq!(detail.sections.len(), 1);
        assert_eq!(detail.effects.len(), 1);
        assert_eq!(detail.effects[0].name, "動画ファイル");
        assert_eq!(detail.effects[0].selector.object, detail.summary.selector);
    }

    /// 候補の絞り込みが、候補以外の詳細を読まずに済むことを確かめる。
    #[test]
    fn get_object_reads_the_detail_of_the_candidate_only() {
        let adapter = adapter();
        let selector = sample_selector(&adapter);
        adapter.get_object(&selector).unwrap();

        let details = adapter
            .host
            .calls()
            .iter()
            .filter(|call| **call == "object_detail")
            .count();
        assert_eq!(details, 1, "候補以外の詳細まで読んでいます");
    }

    /// 同じレイヤーにある無関係な対象が読めなくても、対象の取得が成功することを
    /// 確かめる。
    ///
    /// 候補の絞り込みでレイヤー内の全対象の alias と effect を読むと、無関係な
    /// 対象の不調が対象の取得を巻き込んで失敗させる。
    #[test]
    fn get_object_is_unaffected_by_a_failing_sibling() {
        // レイヤー 1 には開始フレーム 100 と 300 の対象がある。300 の詳細だけを
        // 失敗させ、100 を取得する。
        let adapter = adapter_with(|_| FakeHost {
            detail_fails_at: Some(300),
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

    #[test]
    fn get_object_reports_not_found_when_name_differs() {
        let adapter = adapter();
        let mut selector = sample_selector(&adapter);
        selector.name = Some("別の名前".to_string());
        assert_eq!(
            adapter.get_object(&selector).unwrap_err().error_code(),
            ErrorCode::NotFound
        );
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
            layers[1].objects[0].alias = "[changed]".to_string();
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
    fn get_object_reports_precondition_failed_for_algorithm_mismatch() {
        let adapter = adapter();
        let mut selector = sample_selector(&adapter);
        selector.fingerprint_algorithm = FingerprintAlgorithm::NormalizedAliasV1;

        let error = adapter.get_object(&selector).unwrap_err();
        assert_eq!(error.error_code(), ErrorCode::PreconditionFailed);
        assert_eq!(
            error.details()["requested_fingerprint_algorithm"],
            "sha256-alias-v1"
        );
        assert!(
            !adapter.host.calls().contains(&"enter_read_section"),
            "方式不一致で参照区間へ入りました"
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
            intruder.alias = "[1:100]#2".to_string();
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
                .list_objects(0, None)
                .unwrap()
                .items
                .into_iter()
                .find(|item| item.layer == 1 && item.frame_start == frame_start)
                .unwrap_or_else(|| panic!("開始フレーム {frame_start} の対象がありません"))
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
            let objects = adapter.list_objects(0, None).unwrap();
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
        let snapshot = adapter.list_available_effects(None).unwrap();
        assert_eq!(snapshot.items.len(), 2);
    }

    #[test]
    fn list_available_effects_filters_by_type() {
        let adapter = adapter();
        let snapshot = adapter
            .list_available_effects(Some(&EffectType::Input))
            .unwrap();
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].name, "動画ファイル");

        let none = adapter
            .list_available_effects(Some(&EffectType::Output))
            .unwrap();
        assert!(none.items.is_empty());
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
        objects.list_objects(0, None).unwrap();
        assert_eq!(entries(&objects), 1, "list_objects");

        let object = adapter();
        let selector = sample_selector(&object);
        object.get_object(&selector).unwrap();
        assert_eq!(entries(&object), 1, "get_object");

        // effect のカタログは編集ハンドルから直接得られ、参照区間を必要としない。
        let effects = adapter();
        effects.list_available_effects(None).unwrap();
        assert_eq!(entries(&effects), 0, "list_available_effects");
    }

    #[test]
    fn read_results_do_not_expose_handles() {
        let adapter = adapter();
        let selector = sample_selector(&adapter);
        let mut documents = vec![
            serde_json::to_string(&adapter.get_edit_info().unwrap()).unwrap(),
            serde_json::to_string(&adapter.get_object(&selector).unwrap()).unwrap(),
            serde_json::to_string(&adapter.list_objects(0, None).unwrap().items).unwrap(),
            serde_json::to_string(&adapter.list_layers(0).unwrap().items).unwrap(),
        ];
        documents.push(serde_json::to_string(&adapter.get_current_scene().unwrap().0).unwrap());

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

    #[test]
    fn object_fingerprint_matches_between_summary_and_selector() {
        let adapter = adapter();
        for summary in adapter.list_objects(0, None).unwrap().items {
            assert_eq!(summary.fingerprint, summary.selector.fingerprint);
            assert_eq!(
                summary.fingerprint_algorithm,
                FingerprintAlgorithm::GENERATED
            );
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
        let summaries = adapter.list_objects(0, None).unwrap().items;
        assert!(!summaries.is_empty());

        for summary in summaries {
            let detail = adapter
                .get_object(&summary.selector)
                .expect("一覧が返したセレクターで詳細を引けません");
            assert_eq!(detail.summary.fingerprint, summary.fingerprint);
            assert_eq!(detail.summary.selector, summary.selector);
        }
    }

    /// 配下 effect を持つ対象でも両経路が一致することを確かめる。
    ///
    /// effect が 0 件の対象だけで検証すると、effect を材料に含めていない実装
    /// でも通ってしまう。
    #[test]
    fn object_fingerprint_agrees_for_an_object_that_has_effects() {
        let adapter = adapter();
        let summary = adapter
            .list_objects(0, None)
            .unwrap()
            .items
            .into_iter()
            .find(|item| item.layer == 1 && item.frame_start == 100)
            .expect("配下 effect を持つ対象がありません");

        let detail = adapter.get_object(&summary.selector).unwrap();
        assert!(!detail.effects.is_empty());
        assert_eq!(detail.summary.fingerprint, summary.fingerprint);
    }

    /// effect の設定を変えると、そのオブジェクトの fingerprint も変わることを
    /// 確かめる。
    ///
    /// 変わらなければ、effect を書き換えた後も変更前のセレクターが一致し続け、
    /// 古いセレクターでの変更を拒否できない。epoch を揃えるためプロジェクト
    /// 状態は共用し、差分を effect の設定だけにする。
    #[test]
    fn object_fingerprint_changes_when_an_effect_setting_changes() {
        let project = Arc::new(ProjectState::new());
        let host_with = |path: &'static str| {
            let mut layers = fake_layers();
            layers[1].objects[0] = object_with_effects(
                1,
                100,
                200,
                Some("立ち絵"),
                vec![file_effect("動画ファイル", 0, path)],
            );
            FakeHost {
                layers,
                ..FakeHost::new()
            }
        };
        let fingerprint_of = |path: &'static str| {
            HostReadAdapter::new(host_with(path), Arc::clone(&project))
                .list_objects(0, None)
                .unwrap()
                .items
                .into_iter()
                .find(|item| item.layer == 1 && item.frame_start == 100)
                .expect("対象がありません")
                .fingerprint
        };

        assert_ne!(
            fingerprint_of(r"C:\movie.mp4"),
            fingerprint_of(r"C:\another.mp4"),
            "effect の設定を変えても fingerprint が変わりません"
        );
    }

    /// 同名 effect が繰り上がった場合に、残った effect が別物として扱われる
    /// ことを確かめる。
    ///
    /// 名前と同名内の番号だけを材料にすると、繰り上がった側が取り除く前の
    /// 先頭と同じ fingerprint になり、別のインスタンスへ変更が当たる。
    #[test]
    fn effect_fingerprint_changes_when_the_preceding_effect_is_removed() {
        let adapter_for = |effects: Vec<HostEffect>| {
            let mut layers = fake_layers();
            layers[1].objects[0] = object_with_effects(1, 100, 200, Some("立ち絵"), effects);
            adapter_with(|_| FakeHost {
                layers,
                ..FakeHost::new()
            })
        };
        let fingerprints_of = |adapter: &HostReadAdapter<FakeHost>| {
            let summary = adapter
                .list_objects(0, None)
                .unwrap()
                .items
                .into_iter()
                .find(|item| item.layer == 1 && item.frame_start == 100)
                .expect("対象がありません");
            adapter
                .get_object(&summary.selector)
                .unwrap()
                .effects
                .into_iter()
                .map(|effect| effect.fingerprint)
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

    #[test]
    fn resolve_candidate_requires_exact_start_frame() {
        let objects = vec![object(1, 100, 200, None).placement];
        assert!(matches!(
            resolve_candidate(objects.clone(), 150, None),
            Err(ReadError::ObjectNotFound)
        ));
        assert_eq!(
            resolve_candidate(objects, 100, None).unwrap().frame_start,
            100
        );
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
    fn fingerprint_of_a_moved_object_differs() {
        let base = object(1, 100, 200, Some("立ち絵"));
        let moved = object(1, 101, 200, Some("立ち絵"));
        assert_ne!(
            object_summary("epoch", 0, &base).fingerprint,
            object_summary("epoch", 0, &moved).fingerprint
        );
    }

    #[test]
    fn selector_fingerprint_is_canonical() {
        let adapter = adapter();
        let selector = sample_selector(&adapter);
        let parsed: Fingerprint = selector.fingerprint.as_str().parse().unwrap();
        assert_eq!(parsed, selector.fingerprint);
    }
}
