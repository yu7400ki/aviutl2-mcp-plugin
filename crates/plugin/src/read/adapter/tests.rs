//! 読み取り手順の統合テスト。

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

    fn object_identity(&self, layer: usize, frame_start: usize) -> Result<HostObject, ReadError> {
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
            Some((layer, frame_start)) => Ok(Some(self.find(layer, frame_start)?.identity.clone())),
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

fn object(layer: usize, frame_start: usize, frame_end: usize, name: Option<&str>) -> FakeObject {
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
    fn get_selection_page(&self, expected_scene_id: i32) -> Result<SelectionSnapshot, ReadError> {
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
fn adapter_with(host: impl FnOnce(&Arc<ProjectState>) -> FakeHost) -> HostReadAdapter<FakeHost> {
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
            expression: None,
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
            EvaluatedItem::Track { name, .. } | EvaluatedItem::Check { name, .. } => name.as_str(),
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

/// 参照区間へ入った回数。
fn section_entries(adapter: &HostReadAdapter<FakeHost>) -> usize {
    adapter
        .host
        .calls()
        .iter()
        .filter(|call| **call == "enter_read_section")
        .count()
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

/// 利用可能な effect の一覧の統合テスト。
mod available_effects;
/// effect の中身の説明の統合テスト。
mod describe_effects;
/// 編集情報の取得の統合テスト。
mod edit_info;
/// 補間後の設定項目の値の取得の統合テスト。
mod effect_item_values;
/// 対象と effect の fingerprint の統合テスト。
mod fingerprint;
/// オブジェクト取得の統合テスト。
mod get_object;
/// オブジェクト列挙の統合テスト。
mod list_objects;
/// 全 read operation に共通する契約の統合テスト。
mod read_contract;
/// 受付可否の判定と参照区間の使い方の統合テスト。
mod read_section;
/// エイリアス・フォント・モジュール・パレットの一覧の統合テスト。
mod resources;
/// 現在シーンとレイヤー一覧の取得の統合テスト。
mod scene;
/// 選択状態の取得の統合テスト。
mod selection;
