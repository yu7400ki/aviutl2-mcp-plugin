//! テストが共有する編集ホストのフェイク。
//!
//! [`EditHost`] と [`SceneEditor`] の位置に差し込むため、検証の対象は adapter の
//! 本番実装そのものになる。呼び出しは順序ごと記録し、順序自体を検証できる。
//!
//! 読み取り経路の [`ReadHost`] も同じ状態の上へ実装する。読み取りが返す
//! fingerprint と編集が照合する fingerprint が同じ材料から算出されることを、
//! 同一のフェイク状態に対して確かめられる。

use crate::edit::error::{EditError, NotIssuedReason, UnsupportedReason};
use crate::edit::host::{
    EditHost, EffectSlot, HostScene, HostSelection, ObjectPosition, ObjectSlot, SceneEditor,
};
use crate::edit::precondition::MutationTicket;
use crate::edit::resolve::{ResolvedEffect, ResolvedObject};
use crate::project::ProjectState;
use crate::read::ReadError;
use crate::read::host::{
    EditState, HostEditInfo, HostEffect, HostLayer, HostObject, HostObjectDetail,
    HostObjectPlacement, ReadHost, SceneReader, SceneValueReader,
};
use crate::test_support::alias_with_effects;
use aviutl2_mcp_core::{
    AvailableEffect, AvailableEffectItem, Cursor, DisplayRange, EffectFlags, EffectItem,
    EffectItemType, EffectType, FiniteF64, FrameRange, GridBpm, ItemValue, ModuleEntry, ModuleType,
    PALETTE_COLOR_COUNT, PaletteEntry, Rgba, SectionRange,
};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// フェイクが用いる現在シーンの ID。
pub(crate) const SCENE_ID: i32 = 0;

/// フェイクの初期状態のシーン名。
pub(crate) const SCENE_NAME: &str = "Scene 1";

/// ホストがシーンの解像度を切り詰める上限。
pub(crate) const MAX_SCENE_WIDTH: u32 = 1280;
/// ホストがシーンの解像度を切り詰める上限。
pub(crate) const MAX_SCENE_HEIGHT: u32 = 720;
/// ホストがシーンのサンプリングレートを切り詰める上限。
///
/// 初期状態の値とは別にする。応答が観測値ではなく初期値を返していても、要求値を
/// 返していても、どちらも食い違いとして現れる。
pub(crate) const MAX_SCENE_SAMPLE_RATE: u32 = 96_000;

/// 編集区間を抜けた後に UI から付け直されるシーン名。
pub(crate) const RENAMED_SCENE_NAME: &str = "UI で付け直した名前";

/// 差し込む失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    /// 編集区間の確保に失敗する。
    Section,
    /// 変更 API が失敗を返す。
    Mutation,
    /// 変更後の読み直しが失敗する。
    ReadBack,
    /// 有効・無効の変更を無言で無視する。
    IgnoreEffectState,
    /// レイヤーの状態変更を無言で無視する。
    IgnoreLayerState,
    /// 名前の変更を無言で無視する。
    IgnoreObjectName,
    /// 削除を無言で無視する。
    IgnoreDelete,
    /// 作成が何も生まない。
    CreateNothing,
    /// 作成で 2 件のオブジェクトが、別々のレイヤーへ生まれる。
    ///
    /// 複数オブジェクトを含む alias は各オブジェクトが自分のレイヤーを持てる。
    CreatePair,
    /// effect を列の先頭へ挿入する。
    PrependEffect,
    /// effect の付与で 2 件が増える。
    AddTwoEffects,
    /// 変更 API が SDK へ届かずに失敗する。
    ///
    /// ラッパーは対象の存在確認を呼び出しの入口で行い、そこで落ちた要求は
    /// SDK を呼ばずに戻る。プロジェクトは一切変わっていない。
    TargetGone,
    /// フォーカスの設定だけが SDK へ届かずに失敗する。
    FocusGone,
    /// 移動先をホストが調整する。
    AdjustMoveDestination,
    /// 解決済みトークンから位置を読めない。
    PositionUnreadable,
    /// 設定項目の値を項目名から読めない。
    ///
    /// 逆操作の材料が揃わない状況を作る。
    ItemValueUnreadable,
    /// 中間点の変更 API が理由を伝えずに拒否する。
    ///
    /// 事前確認を通ったのに `false` が返る状況を作る。
    RejectSectionChange,
    /// effect 名からの作成 API が理由を伝えずに拒否する。
    ///
    /// カタログに在る名前なのに `nullptr` が返る状況を作る。
    RejectObjectCreation,
    /// 変更を発行した後で中間点の区間を読み直せない。
    ///
    /// 事前確認の読みには掛けない。掛けると変更を発行する前に落ち、read-back の
    /// 検証にならない。
    SectionsUnreadable,
    /// シーン名の変更を無言で無視する。
    ///
    /// 区間の内側の読み直しが要求値と違う状態を作る。
    IgnoreSceneName,
    /// シーンの解像度とサンプリングレートを要求より小さい値へ切り詰める。
    ///
    /// ホストが指定を調整する状況を作る。区間を抜けた後の観測が要求値と食い違う。
    /// 2 つの軸をまとめて扱うのは、どちらも同じ「ホストが受け取った値を調整する」
    /// 挙動だからである。
    ClampSceneSettings,
    /// 編集区間を抜けた後、観測までの間にシーン名が付け直される。
    ///
    /// 区間の内側の照合は要求どおりに通る。区間を抜けてから観測するまでの間に
    /// UI 操作が入る状況であり、[`Fault::IgnoreSceneName`] とは別物である——
    /// あちらは変更そのものが入らない失敗の経路である。
    RenameSceneAfterSection,
    /// BPM グリッドの置き換えを無言で無視する。
    IgnoreGridBpm,
    /// BPM グリッドを置き換えるが、値をホストが書き換える。
    ///
    /// 件数は要求どおりであり、値だけが要求と違う。単精度への丸めと並べ替えを
    /// 失敗と誤診断しないことを確かめるために用いる。
    RewriteGridBpmValues,
}

/// panic させる位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanicPoint {
    /// 編集区間の外。準備状態の問い合わせで落ちる。
    IsReady,
    /// 編集区間へ入る呼び出しそのもの。クロージャは呼ばれない。
    EnterSection,
    /// クロージャの内側。区間内の読み取りで落ちる。
    ///
    /// 実際の SDK では、ここから漏れた巻き戻しは C の関数ポインタ境界で
    /// プロセスごと abort させる。フェイクは abort できないため、漏れたことを
    /// [`CLOSURE_ESCAPED`] として記録して伝える。
    InClosure,
    /// クロージャの内側。変更を発行した後の読み直しで落ちる。
    AfterMutation,
    /// クロージャの内側。変更を発行した後のレイヤー走査で落ちる。
    ///
    /// 一括適用は宛先の確認をここで行うため、適用の途中で落ちる状況を作れる。
    AfterMutationScan,
}

/// 配下 effect の一覧を引いたことを表す記録。
pub(crate) const EFFECT_LIST: &str = "get_effect_list";

/// 読み取り経路が参照区間へ入ったことを表す記録。
///
/// 編集の要求を組み立てるための読み直しはこの経路を通る。失敗と再要求の間に
/// 読み直しが挟まっていないことを、この記録の増減で確かめられる。
pub(crate) const READ_SECTION: &str = "enter_read_section";

/// レイヤーの属性を名前・表示・ロックまとめて読んだことを表す記録。
pub(crate) const LAYER_ATTRIBUTES: &str = "get_layer_attributes";

/// レイヤーのロック状態だけを読んだことを表す記録。
pub(crate) const LAYER_LOCK: &str = "get_layer_lock";

/// 設定項目の値を項目名で直接読んだことを表す記録。
pub(crate) const ITEM_VALUE: &str = "get_effect_item_value";

/// 中間点で区切られた区間を読み直したことを表す記録。
pub(crate) const SECTION_RANGES: &str = "get_object_section_ranges";

/// タイムライン上の選択の位置を読んだことを表す記録。
pub(crate) const SELECTED_PLACEMENTS: &str = "get_selected_object";

/// オブジェクト設定ウィンドウの選択を読んだことを表す記録。
pub(crate) const FOCUSED_OBJECT: &str = "get_focus_object";

/// フォーカス対象の区間番号を読んだことを表す記録。
pub(crate) const FOCUS_SECTION: &str = "get_focus_object_section";

/// オブジェクトが存在する最大レイヤーを読み直したことを表す記録。
pub(crate) const LAYER_MAX: &str = "get_edit_info";

/// 編集区間を抜けたあとにシーンの状態を観測したことを表す記録。
pub(crate) const OBSERVED_SCENE: &str = "observed_scene";

/// クロージャから巻き戻しが漏れたことを表す記録。
///
/// 実機ではこの位置でホストのプロセスが落ちる。記録が残る経路は、捕捉が
/// クロージャの内側に無いことを意味する。
pub(crate) const CLOSURE_ESCAPED: &str = "panic_escaped_the_callback";

/// フェイクが保持するオブジェクト。
///
/// 識別子を持たせ、変更は識別子で適用する。座標で適用すると、照合した対象と
/// 変更される対象が食い違う実装でもテストが通ってしまう。
#[derive(Debug, Clone)]
pub(crate) struct FakeObject {
    pub(crate) id: usize,
    pub(crate) placement: HostObjectPlacement,
    pub(crate) alias: String,
    pub(crate) effects: Vec<HostEffect>,
    /// 中間点のフレーム番号を昇順に並べたもの。
    ///
    /// 区間の開始フレームではなく**中間点そのもの**を持つ。区間の開始フレームを
    /// 持たせると、区間 0 の開始位置が中間点でないことが状態の形から消える。
    pub(crate) section_points: Vec<usize>,
}

impl FakeObject {
    /// 同一性の材料へ写す。
    ///
    /// alias は配下 effect の設定値を含む。effect を変える編集の後に読み直せば
    /// alias も変わり、対象の同一性が追随する。
    fn identity(&self) -> HostObject {
        HostObject {
            placement: self.placement.clone(),
            alias: alias_with_effects(&self.alias, &self.effects),
        }
    }

    /// 配下 effect と中間点を含む詳細へ写す。
    fn detail(&self) -> HostObjectDetail {
        HostObjectDetail {
            object: self.identity(),
            effects: self.effects.clone(),
            sections: self.sections(),
        }
    }

    /// 中間点で区切られた区間を組み立てる。
    ///
    /// 区間 `i` の開始フレームは、`i` が 0 ならオブジェクトの開始フレーム、
    /// 1 以上なら `i` 番目の中間点である。終端は次の区間の開始フレームの 1 つ
    /// 手前で、最後の区間だけはオブジェクトの終了フレームで閉じる。
    fn sections(&self) -> Vec<SectionRange> {
        let mut starts = vec![self.placement.frame_start];
        starts.extend(self.section_points.iter().copied());
        starts
            .iter()
            .enumerate()
            .map(|(index, &start)| SectionRange {
                start,
                end: match starts.get(index + 1) {
                    Some(next) => next.saturating_sub(1),
                    None => self.placement.frame_end,
                },
            })
            .collect()
    }
}

/// フェイクが保持するレイヤー。
#[derive(Debug, Clone)]
pub(crate) struct FakeLayer {
    /// レイヤー名。標準名のままなら `None`。
    pub(crate) name: Option<String>,
    /// 表示が有効か。
    pub(crate) enabled: bool,
    pub(crate) locked: bool,
    pub(crate) objects: Vec<FakeObject>,
}

impl FakeLayer {
    /// 標準の状態で空のレイヤーを作る。
    pub(crate) fn empty() -> Self {
        Self {
            name: None,
            enabled: true,
            locked: false,
            objects: Vec::new(),
        }
    }

    /// 標準の状態でオブジェクトを持つレイヤーを作る。
    fn with(objects: Vec<FakeObject>) -> Self {
        Self {
            objects,
            ..Self::empty()
        }
    }
}

/// フェイクが保持するプロジェクトの中身。
#[derive(Debug, Clone)]
pub(crate) struct FakeScene {
    pub(crate) layers: Vec<FakeLayer>,
    next_id: usize,
    /// 現在シーンの名前。
    pub(crate) name: String,
    /// 画像の横幅。
    pub(crate) width: u32,
    /// 画像の高さ。
    pub(crate) height: u32,
    /// 音声のサンプリングレート。
    pub(crate) sample_rate: u32,
    pub(crate) cursor: Cursor,
    pub(crate) selected_range: Option<FrameRange>,
    pub(crate) focus: Option<usize>,
    /// フォーカス対象の区間番号。
    ///
    /// フォーカス対象とは独立に持つ。対象が無いのに番号だけを返すホストでも、
    /// 読み取り側が組を揃えることを確かめられる。
    pub(crate) focus_section: Option<usize>,
    /// タイムライン上で選択されているオブジェクトの識別子。
    ///
    /// **ホストが返す順序をそのまま表す。** 並び順は規定されておらず、
    /// 読み取り側が並べ替えることを確かめられるよう、与えた順序で返す。
    pub(crate) selected: Vec<usize>,
    pub(crate) display: DisplayRange,
    pub(crate) grid_bpm: Vec<GridBpm>,
}

impl FakeScene {
    fn find(&self, layer: usize, frame_start: usize) -> Option<&FakeObject> {
        self.layers
            .get(layer)?
            .objects
            .iter()
            .find(|object| object.placement.frame_start == frame_start)
    }

    fn by_id(&self, id: usize) -> Option<&FakeObject> {
        self.layers
            .iter()
            .flat_map(|layer| layer.objects.iter())
            .find(|object| object.id == id)
    }

    fn by_id_mut(&mut self, id: usize) -> Option<&mut FakeObject> {
        self.layers
            .iter_mut()
            .flat_map(|layer| layer.objects.iter_mut())
            .find(|object| object.id == id)
    }

    fn remove(&mut self, id: usize) {
        for layer in &mut self.layers {
            layer.objects.retain(|object| object.id != id);
        }
    }

    fn insert(&mut self, layer: usize, object: FakeObject) {
        let layer = &mut self.layers[layer];
        layer.objects.push(object);
        layer
            .objects
            .sort_by_key(|object| object.placement.frame_start);
    }

    fn take_id(&mut self) -> usize {
        self.next_id += 1;
        self.next_id
    }
}

/// 実行の途中で切り替える設定。
///
/// セレクターは健全な状態の読み取りから得るため、失敗の差し込みはセレクターを
/// 得た**後**で行う。構築時に固定してしまうと、要求を組み立てる段で先に失敗する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Knobs {
    pub(crate) ready: bool,
    pub(crate) state: EditState,
    /// 2 回目以降の編集状態。区間の失敗後の読み直しに使う。
    pub(crate) later_state: Option<EditState>,
    pub(crate) fault: Option<Fault>,
    /// 指定した位置の変更 API 呼び出しにだけ差し込む失敗。
    ///
    /// 位置は編集区間へ入ってからの変更 API の呼び出し回数（0 始まり）である。
    /// 一括適用では巻き戻しも同じ区間で発行されるため、適用と巻き戻しのどちらか
    /// 一方だけを失敗させられる。
    pub(crate) fault_at: Option<(usize, Fault)>,
    pub(crate) panic_at: Option<PanicPoint>,
    /// 次の詳細の読み取りで進めるプロジェクト revision の回数。
    ///
    /// 対象の解決と変更の間に境界が変わる状況を作る。1 度だけ働く。
    pub(crate) bump_on_detail: u64,
    /// 次の詳細の読み取りでプロジェクト境界を更新するか。1 度だけ働く。
    pub(crate) renew_on_detail: bool,
    /// 変更を発行した直後に進めるプロジェクト revision の回数。
    ///
    /// ホストが plugin 発の編集にも対象更新を配送する状況を作る。
    pub(crate) bump_after_mutation: u64,
    /// 区間の内側でプロジェクト境界のロックを試みるか。
    ///
    /// 境界は再入できないロックで守られている。編集が区間を跨いでそれを保持
    /// していれば、この取得で待ち合わせが解けなくなる。
    pub(crate) probe_lock_in_section: bool,
}

impl Default for Knobs {
    fn default() -> Self {
        Self {
            ready: true,
            state: EditState::Edit,
            later_state: None,
            fault: None,
            fault_at: None,
            panic_at: None,
            bump_on_detail: 0,
            renew_on_detail: false,
            bump_after_mutation: 0,
            probe_lock_in_section: false,
        }
    }
}

/// SDK の代わりに定型データを返す編集ホスト。
pub(crate) struct FakeEditHost {
    pub(crate) info: HostEditInfo,
    pub(crate) catalog: Vec<AvailableEffect>,
    /// 登録済みフォント名。
    pub(crate) fonts: Vec<String>,
    /// 登録済みモジュール。
    pub(crate) modules: Vec<ModuleEntry>,
    /// 登録済みパレット。
    pub(crate) palettes: Vec<PaletteEntry>,
    /// 現在のパレット名。
    pub(crate) current_palette: Option<String>,
    pub(crate) scene: Mutex<FakeScene>,
    pub(crate) project: Option<Arc<ProjectState>>,
    knobs: Mutex<Knobs>,
    enter_calls: AtomicUsize,
    edit_state_calls: AtomicUsize,
    mutation_calls: AtomicUsize,
    calls: Mutex<Vec<&'static str>>,
    /// レイヤー名の設定へ渡された引数を、渡された順に覚える。
    layer_names: Mutex<Vec<Option<String>>>,
    /// 設定項目の書き込みへ渡された値を、渡された順に覚える。
    item_values: Mutex<Vec<String>>,
    /// 登録済みエイリアスを収めたデータディレクトリ。
    ///
    /// 既定は `None` である。設定ハンドルを初期化できない環境がそのまま既定で
    /// あり、解決できたことを前提にした経路を作らない。
    alias_data_dir: Mutex<Option<PathBuf>>,
}

impl FakeEditHost {
    /// 既定の状態でフェイクを作る。
    pub(crate) fn new() -> Self {
        Self {
            info: fake_edit_info(),
            catalog: fake_catalog(),
            fonts: vec!["MS UI Gothic".to_string(), "游ゴシック".to_string()],
            modules: vec![ModuleEntry {
                module_type: ModuleType::ScriptObject,
                name: "テキスト".to_string(),
                information: "標準搭載".to_string(),
            }],
            palettes: vec![PaletteEntry {
                name: "既定".to_string(),
                colors: vec![
                    Rgba {
                        r: 0,
                        g: 0,
                        b: 0,
                        a: 255
                    };
                    PALETTE_COLOR_COUNT
                ],
            }],
            current_palette: Some("[標準.既定]".to_string()),
            scene: Mutex::new(fake_scene()),
            project: None,
            knobs: Mutex::new(Knobs::default()),
            enter_calls: AtomicUsize::new(0),
            edit_state_calls: AtomicUsize::new(0),
            mutation_calls: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
            layer_names: Mutex::new(Vec::new()),
            item_values: Mutex::new(Vec::new()),
            alias_data_dir: Mutex::new(None),
        }
    }

    /// 解決済みのデータディレクトリを差し込む。
    ///
    /// 解決そのものはこの位置より下にある。差し込むのは解決した先だけであり、
    /// 解決できない状況は `None` のまま試せる。
    pub(crate) fn set_alias_data_directory(&self, dir: Option<PathBuf>) {
        *self.alias_data_dir.lock().unwrap() = dir;
    }

    /// 設定を切り替える。
    pub(crate) fn arm(&self, configure: impl FnOnce(&mut Knobs)) {
        configure(&mut self.knobs.lock().unwrap());
    }

    /// 現在の設定を読む。
    fn knobs(&self) -> Knobs {
        *self.knobs.lock().unwrap()
    }

    /// 準備前の呼び出しを、実際の SDK と同じ失敗モードで再現する。
    fn assert_ready(&self, api: &str) {
        assert!(self.knobs().ready, "準備前に {api} が呼ばれました");
    }

    fn record(&self, call: &'static str) {
        self.calls.lock().unwrap().push(call);
    }

    /// 記録された呼び出しを順序どおり返す。
    pub(crate) fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    /// 記録を捨てる。
    ///
    /// 要求の組み立てに使う読み取りと、編集そのものの呼び出しを切り分けるために
    /// 用いる。
    pub(crate) fn clear_calls(&self) {
        self.calls.lock().unwrap().clear();
    }

    /// 編集区間へ入った回数。
    pub(crate) fn enter_calls(&self) -> usize {
        self.enter_calls.load(Ordering::Relaxed)
    }

    /// 変更 API が 1 度でも呼ばれたか。
    pub(crate) fn mutated(&self) -> bool {
        self.calls().iter().any(|call| MUTATIONS.contains(call))
    }

    /// レイヤー名の設定へ渡された引数を、渡された順に返す。
    ///
    /// `None` は「名前を渡さなかった」、`Some("")` は「空の名前を渡した」を表す。
    /// 標準名へ戻す指定が前者であることを、この記録で確かめられる。
    pub(crate) fn layer_name_arguments(&self) -> Vec<Option<String>> {
        self.layer_names.lock().unwrap().clone()
    }

    /// 設定項目の書き込みへ渡された値を、渡された順に返す。
    ///
    /// 逆操作が書き戻す文字列をそのまま検査できる。読み取り経路が解釈した値を
    /// 組み立て直していれば、ここに現れる文字列が元値と一致しなくなる。
    pub(crate) fn item_value_arguments(&self) -> Vec<String> {
        self.item_values.lock().unwrap().clone()
    }

    /// レイヤーのロック状態を切り替える。
    pub(crate) fn lock_layer(&self, layer: usize, locked: bool) {
        self.scene.lock().unwrap().layers[layer].locked = locked;
    }

    /// タイムライン上の選択を、ホストが返す順序ごと差し替える。
    ///
    /// 位置はレイヤー番号と開始フレーム番号の組で指す。与えた順序がそのまま
    /// ホストの返す順序になる。
    pub(crate) fn select_objects(&self, positions: &[(usize, usize)]) {
        let mut scene = self.scene.lock().unwrap();
        let ids = positions
            .iter()
            .map(|&(layer, frame_start)| {
                scene
                    .find(layer, frame_start)
                    .unwrap_or_else(|| {
                        panic!("レイヤー {layer} フレーム {frame_start} の対象がありません")
                    })
                    .id
            })
            .collect();
        scene.selected = ids;
    }

    /// オブジェクト設定ウィンドウの選択と、その区間番号を差し替える。
    ///
    /// 両者を独立に指定できる。対象が無いのに番号だけを返すホストも作れる。
    pub(crate) fn focus_object(&self, position: Option<(usize, usize)>, section: Option<usize>) {
        let mut scene = self.scene.lock().unwrap();
        let id = position.map(|(layer, frame_start)| {
            scene
                .find(layer, frame_start)
                .unwrap_or_else(|| {
                    panic!("レイヤー {layer} フレーム {frame_start} の対象がありません")
                })
                .id
        });
        scene.focus = id;
        scene.focus_section = section;
    }

    /// 対象が持つ中間点のフレーム番号を差し替える。
    pub(crate) fn set_section_points(&self, layer: usize, frame_start: usize, points: Vec<usize>) {
        let mut scene = self.scene.lock().unwrap();
        let object = scene.layers[layer]
            .objects
            .iter_mut()
            .find(|object| object.placement.frame_start == frame_start)
            .unwrap_or_else(|| {
                panic!("レイヤー {layer} フレーム {frame_start} の対象がありません")
            });
        object.section_points = points;
    }

    /// 対象が持つ中間点のフレーム番号を読む。
    pub(crate) fn section_points(&self, layer: usize, frame_start: usize) -> Vec<usize> {
        let scene = self.scene.lock().unwrap();
        scene
            .find(layer, frame_start)
            .unwrap_or_else(|| panic!("レイヤー {layer} フレーム {frame_start} の対象がありません"))
            .section_points
            .clone()
    }

    /// フェイクが保持する状態を読む。
    pub(crate) fn scene(&self) -> FakeScene {
        self.scene.lock().unwrap().clone()
    }
}

/// 変更 API として記録される呼び出し。
pub(crate) const MUTATIONS: &[&str] = &[
    "create_object_from_alias",
    "create_object_from_media_file",
    "create_object",
    "move_object",
    "delete_object",
    "set_object_name",
    "create_object_section",
    "delete_object_section",
    "move_object_section",
    "create_effect",
    "delete_effect",
    "set_effect_enable",
    "set_effect_item_value",
    "set_layer_name",
    "set_layer_enable",
    "set_layer_lock",
    "set_scene_name",
    "set_scene_size",
    "set_scene_sample_rate",
    "set_cursor_layer_frame",
    "set_display_layer_frame",
    "set_select_range",
    "set_focus_object",
    "set_grid_bpm_list",
];

impl EditHost for FakeEditHost {
    fn is_ready(&self) -> bool {
        let knobs = self.knobs();
        assert_ne!(
            knobs.panic_at,
            Some(PanicPoint::IsReady),
            "準備状態の問い合わせで panic させます"
        );
        knobs.ready
    }

    fn edit_state(&self) -> Result<EditState, EditError> {
        self.assert_ready("get_edit_state");
        self.record("edit_state");
        let knobs = self.knobs();
        let calls = self.edit_state_calls.fetch_add(1, Ordering::Relaxed);
        Ok(if calls == 0 {
            knobs.state
        } else {
            knobs.later_state.unwrap_or(knobs.state)
        })
    }

    fn effect_catalog(&self) -> Result<Vec<AvailableEffect>, EditError> {
        self.assert_ready("get_effects");
        self.record("effect_catalog");
        Ok(self.catalog.clone())
    }

    fn alias_data_directory(&self) -> Option<PathBuf> {
        self.record("alias_data_directory");
        self.alias_data_dir.lock().unwrap().clone()
    }

    fn observed_selection(&self) -> Result<HostSelection, EditError> {
        self.assert_ready("get_edit_info");
        self.record("observed_selection");
        let scene = self.scene.lock().unwrap();
        Ok(HostSelection {
            scene_id: self.info.scene_id,
            cursor: scene.cursor,
            selected_range: scene.selected_range,
            focus: scene
                .focus
                .and_then(|id| scene.by_id(id))
                .map(FakeObject::identity),
            display: scene.display,
        })
    }

    fn observed_scene(&self) -> Result<HostScene, EditError> {
        self.assert_ready("get_edit_info");
        self.record(OBSERVED_SCENE);
        let mut scene = self.scene.lock().unwrap();
        // 区間を抜けてから観測するまでの間に UI がシーン名を付け直す。区間の
        // 内側の照合は既に通っており、観測だけが要求値と食い違う。
        if self.knobs().fault == Some(Fault::RenameSceneAfterSection) {
            scene.name = RENAMED_SCENE_NAME.to_string();
        }
        // 解像度とサンプリングレートは編集情報として観測される。区間の内側で
        // 適用した値がここに現れる。
        Ok(HostScene {
            info: HostEditInfo {
                width: scene.width,
                height: scene.height,
                sample_rate: scene.sample_rate,
                ..self.info.clone()
            },
            name: Some(scene.name.clone()),
        })
    }

    fn enter_edit_section<T, F>(&self, f: F) -> Result<T, EditError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn SceneEditor) -> T + Send,
    {
        self.assert_ready("call_edit_section");
        self.record("enter_edit_section");
        self.enter_calls.fetch_add(1, Ordering::Relaxed);
        let knobs = self.knobs();
        // 実際の SDK は準備前の呼び出しをこの位置の表明で落とす。クロージャは
        // 呼ばれないため、渡す側を包んでも捕捉できない。
        assert_ne!(
            knobs.panic_at,
            Some(PanicPoint::EnterSection),
            "編集区間へ入る呼び出しで panic させます"
        );
        if knobs.fault == Some(Fault::Section) {
            return Err(EditError::Sdk {
                operation: "call_edit_section",
            });
        }
        if knobs.probe_lock_in_section
            && let Some(project) = &self.project
        {
            // 境界のロックを取りに行く。編集が区間を跨いで保持していれば
            // ここで止まる。
            let _ = project.epoch();
        }
        let editor = FakeSceneEditor {
            host: self,
            objects: RefCell::new(Vec::new()),
            effects: RefCell::new(Vec::new()),
        };
        // クロージャから漏れた巻き戻しを記録してから伝え直す。実際の SDK では
        // ここが C の関数ポインタ境界であり、漏れた時点でプロセスが落ちる。
        // 記録が残るかどうかで、捕捉がクロージャの内側にあるかを判別できる。
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&editor))) {
            Ok(value) => Ok(value),
            Err(payload) => {
                self.record(CLOSURE_ESCAPED);
                std::panic::resume_unwind(payload)
            }
        }
    }
}

/// 共有したフェイクをそのまま編集ホストとして使えるようにする。
///
/// テストは同じ状態を読み取り経路と編集経路の双方へ渡す。
impl EditHost for Arc<FakeEditHost> {
    fn is_ready(&self) -> bool {
        self.as_ref().is_ready()
    }

    fn edit_state(&self) -> Result<EditState, EditError> {
        self.as_ref().edit_state()
    }

    fn effect_catalog(&self) -> Result<Vec<AvailableEffect>, EditError> {
        self.as_ref().effect_catalog()
    }

    fn alias_data_directory(&self) -> Option<PathBuf> {
        self.as_ref().alias_data_directory()
    }

    fn observed_selection(&self) -> Result<HostSelection, EditError> {
        self.as_ref().observed_selection()
    }

    fn observed_scene(&self) -> Result<HostScene, EditError> {
        self.as_ref().observed_scene()
    }

    fn enter_edit_section<T, F>(&self, f: F) -> Result<T, EditError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn SceneEditor) -> T + Send,
    {
        self.as_ref().enter_edit_section(f)
    }
}

/// 同じ状態の上に読み取り経路を実装するフェイク。
pub(crate) struct FakeReadHost(pub(crate) Arc<FakeEditHost>);

impl ReadHost for FakeReadHost {
    fn is_ready(&self) -> bool {
        self.0.knobs().ready
    }

    fn edit_state(&self) -> Result<EditState, ReadError> {
        Ok(self.0.knobs().state)
    }

    fn edit_info(&self) -> Result<HostEditInfo, ReadError> {
        Ok(self.0.info.clone())
    }

    fn effect_catalog(&self) -> Result<Vec<AvailableEffect>, ReadError> {
        Ok(self.0.catalog.clone())
    }

    fn font_names(&self) -> Result<Vec<String>, ReadError> {
        Ok(self.0.fonts.clone())
    }

    fn modules(&self) -> Result<Vec<ModuleEntry>, ReadError> {
        Ok(self.0.modules.clone())
    }

    fn enter_read_section<T, F>(&self, f: F) -> Result<T, ReadError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn SceneValueReader) -> T + Send,
    {
        self.0.record(READ_SECTION);
        let editor = FakeSceneEditor {
            host: &self.0,
            objects: RefCell::new(Vec::new()),
            effects: RefCell::new(Vec::new()),
        };
        Ok(f(&editor))
    }
}

/// 編集区間の内側を表すフェイク。
///
/// 解決したオブジェクトは識別子で、effect は識別子と列の位置で覚える。変更は
/// 覚えた対象へ適用するため、座標から探し直す実装では対象が食い違う。
struct FakeSceneEditor<'a> {
    host: &'a FakeEditHost,
    objects: RefCell<Vec<usize>>,
    effects: RefCell<Vec<(usize, usize)>>,
}

impl FakeSceneEditor<'_> {
    fn object_id(&self, slot: ObjectSlot) -> Result<usize, EditError> {
        self.objects
            .borrow()
            .get(slot.0)
            .copied()
            .ok_or(EditError::Sdk {
                operation: "get_object_layer_frame",
            })
    }

    fn effect_ref(&self, slot: EffectSlot) -> Result<(usize, usize), EditError> {
        self.effects
            .borrow()
            .get(slot.0)
            .copied()
            .ok_or(EditError::Sdk {
                operation: "get_effect_list",
            })
    }

    /// 変更 API の失敗を差し込む。
    ///
    /// 位置を指定した仕込みがあり、位置が一致する呼び出しではそちらを優先する。
    /// 一致しない呼び出しには、区間全体へ掛けた仕込みがそのまま働く。
    fn mutation(&self, call: &'static str) -> Result<(), EditError> {
        self.host.record(call);
        let position = self.host.mutation_calls.fetch_add(1, Ordering::Relaxed);
        let knobs = self.host.knobs();
        let injected = match knobs.fault_at {
            Some((at, fault)) if at == position => Some(fault),
            _ => knobs.fault,
        };
        match injected {
            Some(Fault::Mutation) => Err(EditError::Sdk { operation: call }),
            Some(Fault::TargetGone) => Err(EditError::NotIssued {
                reason: NotIssuedReason::TargetMissing,
            }),
            // 中間点の 3 つは `bool` を返し、`false` は理由を伝えない。SDK 境界の
            // 実装はそれを専用の失敗へ写す。
            Some(Fault::RejectSectionChange) => {
                Err(EditError::SectionChangeRejected { operation: call })
            }
            // effect 名からの作成は `nullptr` を返し得る。SDK 境界の実装はそれを
            // 専用の理由へ写す。
            Some(Fault::RejectObjectCreation) => Err(EditError::UnsupportedTarget {
                reason: UnsupportedReason::EffectNotCreatable,
            }),
            _ => Ok(()),
        }
    }

    /// 解決済みトークンが指すオブジェクトへ変更を適用する。
    fn with_object(
        &self,
        object: &ResolvedObject<'_>,
        operation: &'static str,
        apply: impl FnOnce(&mut FakeObject) -> Result<(), EditError>,
    ) -> Result<(), EditError> {
        let id = self.object_id(object.slot())?;
        let mut scene = self.host.scene.lock().unwrap();
        let found = scene.by_id_mut(id).ok_or(EditError::Sdk { operation })?;
        apply(found)
    }
}

impl SceneReader for FakeSceneEditor<'_> {
    fn scene_name(&self) -> Option<String> {
        Some(self.host.scene.lock().unwrap().name.clone())
    }

    fn grid_bpm(&self) -> Result<Vec<GridBpm>, ReadError> {
        Ok(self.host.scene.lock().unwrap().grid_bpm.clone())
    }

    fn layer(&self, layer: usize) -> Result<HostLayer, ReadError> {
        self.host.record(LAYER_ATTRIBUTES);
        let scene = self.host.scene.lock().unwrap();
        let fake = scene.layers.get(layer).ok_or(ReadError::Sdk {
            operation: "get_layer_name",
        })?;
        Ok(HostLayer {
            name: fake.name.clone(),
            enabled: fake.enabled,
            locked: fake.locked,
        })
    }

    fn layer_locked(&self, layer: usize) -> Result<bool, ReadError> {
        self.host.record(LAYER_LOCK);
        let scene = self.host.scene.lock().unwrap();
        let fake = scene.layers.get(layer).ok_or(ReadError::Sdk {
            operation: "get_layer_lock",
        })?;
        Ok(fake.locked)
    }

    fn object_count(&self, layer: usize) -> Result<usize, ReadError> {
        let scene = self.host.scene.lock().unwrap();
        Ok(scene
            .layers
            .get(layer)
            .map(|layer| layer.objects.len())
            .unwrap_or_default())
    }

    fn object_placements(&self, layer: usize) -> Result<Vec<HostObjectPlacement>, ReadError> {
        self.host.record("object_placements");
        if self.host.mutated() {
            // 一括適用は宛先の確認をここで行う。適用の途中で落ちる状況を作れる。
            assert_ne!(
                self.host.knobs().panic_at,
                Some(PanicPoint::AfterMutationScan),
                "変更を発行した後のレイヤー走査で panic させます"
            );
        }
        let scene = self.host.scene.lock().unwrap();
        Ok(scene
            .layers
            .get(layer)
            .map(|layer| {
                layer
                    .objects
                    .iter()
                    .map(|object| object.placement.clone())
                    .collect()
            })
            .unwrap_or_default())
    }

    fn object_identity(&self, layer: usize, frame_start: usize) -> Result<HostObject, ReadError> {
        self.host.record("object_identity");
        self.on_object_read()?;
        let scene = self.host.scene.lock().unwrap();
        Ok(scene
            .find(layer, frame_start)
            .ok_or(ReadError::ObjectNotFound {
                detected_by: "find_object",
            })?
            .identity())
    }

    fn object_detail(
        &self,
        layer: usize,
        frame_start: usize,
    ) -> Result<HostObjectDetail, ReadError> {
        self.host.record("object_detail");
        // effect の一覧を引くのはこの経路だけである。記録しておくことで、
        // effect を必要としない operation が読んでいないことを確かめられる。
        self.host.record(EFFECT_LIST);
        self.on_object_read()?;
        let scene = self.host.scene.lock().unwrap();
        Ok(scene
            .find(layer, frame_start)
            .ok_or(ReadError::ObjectNotFound {
                detected_by: "find_object",
            })?
            .detail())
    }
}

impl SceneValueReader for FakeSceneEditor<'_> {
    fn palette_names(&self) -> Result<Vec<String>, ReadError> {
        Ok(self
            .host
            .palettes
            .iter()
            .map(|palette| palette.name.clone())
            .collect())
    }

    fn current_palette_name(&self) -> Option<String> {
        self.host.current_palette.clone()
    }

    fn palette_colors(&self, name: &str) -> Option<Vec<Rgba>> {
        self.host
            .palettes
            .iter()
            .find(|palette| palette.name == name)
            .map(|palette| palette.colors.clone())
    }

    fn selected_placements(&self) -> Result<Vec<HostObjectPlacement>, ReadError> {
        self.host.record(SELECTED_PLACEMENTS);
        let scene = self.host.scene.lock().unwrap();
        scene
            .selected
            .iter()
            .map(|&id| {
                scene
                    .by_id(id)
                    .map(|object| object.placement.clone())
                    .ok_or(ReadError::Sdk {
                        operation: "get_selected_object",
                    })
            })
            .collect()
    }

    fn focused_object(&self) -> Result<Option<HostObject>, ReadError> {
        self.host.record(FOCUSED_OBJECT);
        let scene = self.host.scene.lock().unwrap();
        Ok(scene
            .focus
            .and_then(|id| scene.by_id(id))
            .map(FakeObject::identity))
    }

    fn focus_section(&self) -> Result<Option<usize>, ReadError> {
        self.host.record(FOCUS_SECTION);
        Ok(self.host.scene.lock().unwrap().focus_section)
    }

    fn effect_track_values(
        &self,
        layer: usize,
        frame_start: usize,
        effect_position: usize,
        item_names: &[&str],
        frames: &[f64],
    ) -> Result<Vec<Vec<FiniteF64>>, ReadError> {
        // 設定値をどのフレームでも同じ値として返す。編集経路は補間の結果を
        // 見ないため、値が読めることだけを満たす。
        const CALL: &str = "get_effect_track_value";
        let mut items = Vec::with_capacity(item_names.len());
        for item_name in item_names {
            let value = match self
                .item_at(layer, frame_start, effect_position, item_name, CALL)?
                .value
            {
                ItemValue::Integer { value } => FiniteF64::try_new(value as f64),
                ItemValue::Number { value } => Some(value),
                _ => None,
            }
            .ok_or(ReadError::TrackValueUnavailable { operation: CALL })?;
            items.push(vec![value; frames.len()]);
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
        const CALL: &str = "get_effect_check_value";
        let mut items = Vec::with_capacity(item_names.len());
        for item_name in item_names {
            let ItemValue::Bool { value } = self
                .item_at(layer, frame_start, effect_position, item_name, CALL)?
                .value
            else {
                return Err(ReadError::TrackValueUnavailable { operation: CALL });
            };
            items.push(vec![value; frames.len()]);
        }
        Ok(items)
    }

    /// **所属を `TrackInfo.group_name` から導いている。実物は別の一覧を返す。**
    ///
    /// ホストの `get_object_track_group_names` は設定項目の移動情報とは別に
    /// 保持された一覧であり、`TrackInfo.group_num` と件数が一致する保証も無い。
    /// この実装は両者が必ず整合する状態しか作れないため、**食い違いを扱う経路の
    /// 検証には使えない。** 読み取り経路のフェイクが両者を食い違わせている。
    fn track_group_item_names(
        &self,
        layer: usize,
        frame_start: usize,
        effect_name: &str,
        effect_index: usize,
        group_name: &str,
    ) -> Result<Vec<String>, ReadError> {
        let scene = self.host.scene.lock().unwrap();
        let Some(object) = scene.find(layer, frame_start) else {
            return Ok(Vec::new());
        };
        let Some(effect) = object
            .effects
            .iter()
            .find(|effect| effect.name == effect_name && effect.index == effect_index)
        else {
            return Ok(Vec::new());
        };
        // グループが無ければ 0 件になる。失敗ではない。
        Ok(effect
            .items
            .iter()
            .filter(|item| {
                item.track
                    .as_ref()
                    .and_then(|track| track.group_name.as_deref())
                    == Some(group_name)
            })
            .map(|item| item.name.clone())
            .collect())
    }
}

impl FakeSceneEditor<'_> {
    /// effect 列の位置と項目名で設定項目を引く。
    ///
    /// `call` には呼び出し元の SDK 関数名を渡す。失敗の出所を伝える値であり、
    /// 種別が違えば名乗る関数も違う。
    fn item_at(
        &self,
        layer: usize,
        frame_start: usize,
        effect_position: usize,
        item_name: &str,
        call: &'static str,
    ) -> Result<EffectItem, ReadError> {
        let scene = self.host.scene.lock().unwrap();
        scene
            .find(layer, frame_start)
            .and_then(|object| object.effects.get(effect_position))
            .and_then(|effect| effect.items.iter().find(|item| item.name == item_name))
            .cloned()
            .ok_or(ReadError::TrackValueUnavailable { operation: call })
    }

    /// 対象を読むたびに働く仕込みを適用する。
    ///
    /// 同一性の材料だけを読む経路と詳細を読む経路のどちらも通る。片方だけに
    /// 置くと、仕込みが働くかどうかが読み取りの粒度で変わってしまう。
    fn on_object_read(&self) -> Result<(), ReadError> {
        if self.host.mutated() {
            assert_ne!(
                self.host.knobs().panic_at,
                Some(PanicPoint::AfterMutation),
                "変更を発行した後の読み直しで panic させます"
            );
            // ホストが plugin 発の編集にも対象更新を配送する状況を作る。応答の
            // revision を加算時点の値ではなく読み直した値で組み立てていれば、
            // ここで進めた分だけ食い違う。1 度だけ働かせる。
            let mut bumps = 0;
            self.host.arm(|knobs| {
                bumps = knobs.bump_after_mutation;
                knobs.bump_after_mutation = 0;
            });
            if let Some(project) = &self.host.project {
                for _ in 0..bumps {
                    project.on_object_updated();
                }
            }
        }
        // 対象の解決と変更の間に境界が変わる状況は 1 度だけ再現する。仕込みを
        // 消費しておかないと、後続の読み直しでも繰り返し働いてしまう。
        let mut armed = Knobs::default();
        self.host.arm(|knobs| {
            armed = *knobs;
            knobs.bump_on_detail = 0;
            knobs.renew_on_detail = false;
        });
        if let Some(project) = &self.host.project {
            for _ in 0..armed.bump_on_detail {
                project.on_object_updated();
            }
            if armed.renew_on_detail {
                project.on_project_load(Some(r"C:\projects\reopened.aup2"));
            }
        }
        // 読み直しの失敗は、変更を発行した後にだけ差し込む。解決の段で失敗させて
        // しまうと、read-back の検証にならない。
        if armed.fault == Some(Fault::ReadBack) && self.host.mutated() {
            return Err(ReadError::Sdk {
                operation: "get_object_alias",
            });
        }
        Ok(())
    }
}

impl SceneEditor for FakeSceneEditor<'_> {
    fn reader(&self) -> &dyn SceneReader {
        self
    }

    fn occupied_layer_max(&self) -> Result<usize, EditError> {
        self.host.record(LAYER_MAX);
        let scene = self.host.scene.lock().unwrap();
        // 「オブジェクトが存在する最大のレイヤー番号」であり、レイヤーの本数
        // ではない。作成で伸び、削除で縮む。
        Ok(scene
            .layers
            .iter()
            .rposition(|layer| !layer.objects.is_empty())
            .unwrap_or(0))
    }

    fn entry_edit_info(&self) -> &HostEditInfo {
        // クロージャの内側で落ちる。区間内の処理は算術や添字でも落ち得るため、
        // 捕捉がクロージャの内側に無ければ実機ではプロセスごと落ちる。全ての
        // operation が区間の先頭でここを通る。
        assert_ne!(
            self.host.knobs().panic_at,
            Some(PanicPoint::InClosure),
            "編集区間の内側で panic させます"
        );
        &self.host.info
    }

    fn bind_object(&self, layer: usize, frame_start: usize) -> Result<ObjectSlot, EditError> {
        self.host.record("bind_object");
        let id = {
            let scene = self.host.scene.lock().unwrap();
            scene
                .find(layer, frame_start)
                .ok_or(ReadError::ObjectNotFound {
                    detected_by: "find_object",
                })?
                .id
        };
        let mut objects = self.objects.borrow_mut();
        objects.push(id);
        Ok(ObjectSlot(objects.len() - 1))
    }

    fn bind_effect(&self, object: ObjectSlot, position: usize) -> Result<EffectSlot, EditError> {
        self.host.record("bind_effect");
        let id = self.object_id(object)?;
        {
            let scene = self.host.scene.lock().unwrap();
            let object = scene.by_id(id).ok_or(EditError::Sdk {
                operation: "get_effect_list",
            })?;
            if object.effects.len() <= position {
                return Err(EditError::Sdk {
                    operation: "get_effect_list",
                });
            }
        }
        let mut effects = self.effects.borrow_mut();
        effects.push((id, position));
        Ok(EffectSlot(effects.len() - 1))
    }

    fn effect_items(
        &self,
        effect: &ResolvedEffect<'_>,
    ) -> Result<Vec<AvailableEffectItem>, EditError> {
        self.host.record("effect_items");
        let name = &effect.info().name;
        self.host
            .catalog
            .iter()
            .find(|available| available.name == *name)
            .map(|available| available.items.clone())
            .ok_or(EditError::Sdk {
                operation: "enum_effect_item",
            })
    }

    fn effect_item_value(
        &self,
        effect: &ResolvedEffect<'_>,
        item: &str,
    ) -> Result<String, EditError> {
        self.host.record(ITEM_VALUE);
        if self.host.knobs().fault == Some(Fault::ItemValueUnreadable) {
            return Err(EditError::Sdk {
                operation: "get_effect_item_value",
            });
        }
        let (id, position) = self.effect_ref(effect.slot())?;
        let scene = self.host.scene.lock().unwrap();
        scene
            .by_id(id)
            .and_then(|object| object.effects.get(position))
            .and_then(|effect| effect.items.iter().find(|entry| entry.name == item))
            .map(|entry| raw_item_value(&entry.value))
            .ok_or(EditError::Sdk {
                operation: "get_effect_item_value",
            })
    }

    fn supports_media_file(&self, path: &str) -> Result<bool, EditError> {
        self.host.record("is_support_media_file");
        Ok(path.ends_with(".mp4"))
    }

    fn create_object_from_alias(
        &self,
        _ticket: MutationTicket<'_>,
        alias: &str,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.mutation("create_object_from_alias")?;
        self.create(layer, frame, alias.to_string())
    }

    fn create_object_from_media_file(
        &self,
        _ticket: MutationTicket<'_>,
        path: &str,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.mutation("create_object_from_media_file")?;
        self.create(layer, frame, format!("[{path}]"))
    }

    fn create_object_from_effect(
        &self,
        _ticket: MutationTicket<'_>,
        name: &str,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.mutation("create_object")?;
        self.create(layer, frame, format!("[{name}]"))
    }

    fn object_position(&self, object: &ResolvedObject<'_>) -> Result<ObjectPosition, EditError> {
        self.host.record("get_object_layer_frame");
        if self.host.knobs().fault == Some(Fault::PositionUnreadable) {
            return Err(EditError::Sdk {
                operation: "get_object_layer_frame",
            });
        }
        let id = self.object_id(object.slot())?;
        let scene = self.host.scene.lock().unwrap();
        let found = scene.by_id(id).ok_or(EditError::Sdk {
            operation: "get_object_layer_frame",
        })?;
        Ok(ObjectPosition {
            layer: found.placement.layer,
            frame_start: found.placement.frame_start,
        })
    }

    fn move_object(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.mutation("move_object")?;
        // ホストは宛先を調整し得る。要求値をそのまま応答へ載せる実装では、
        // 成功した移動が対象の不在として返る。
        let frame = match self.host.knobs().fault {
            Some(Fault::AdjustMoveDestination) => frame + MOVE_FRAME_SHIFT,
            _ => frame,
        };
        let id = self.object_id(object.slot())?;
        let mut scene = self.host.scene.lock().unwrap();
        // 移動先に別のオブジェクトが居れば失敗する。巻き戻しは宛先を事前に
        // 確かめないため、順序を誤った巻き戻しはここで落ちる。
        let occupied = scene
            .layers
            .get(layer)
            .map(|target| {
                target.objects.iter().any(|other| {
                    other.id != id
                        && other.placement.frame_start <= frame
                        && frame <= other.placement.frame_end
                })
            })
            .unwrap_or_default();
        if occupied {
            return Err(EditError::Sdk {
                operation: "move_object",
            });
        }
        let mut moved = scene
            .by_id(id)
            .ok_or(EditError::Sdk {
                operation: "move_object",
            })?
            .clone();
        let length = moved.placement.frame_end - moved.placement.frame_start;
        moved.placement.layer = layer;
        moved.placement.frame_start = frame;
        moved.placement.frame_end = frame + length;
        scene.remove(id);
        scene.insert(layer, moved);
        Ok(())
    }

    fn delete_object(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
    ) -> Result<(), EditError> {
        self.mutation("delete_object")?;
        if self.host.knobs().fault == Some(Fault::IgnoreDelete) {
            return Ok(());
        }
        let id = self.object_id(object.slot())?;
        self.host.scene.lock().unwrap().remove(id);
        Ok(())
    }

    fn object_sections(&self, object: &ResolvedObject<'_>) -> Result<Vec<SectionRange>, EditError> {
        self.host.record(SECTION_RANGES);
        if self.host.knobs().fault == Some(Fault::SectionsUnreadable) && self.host.mutated() {
            return Err(EditError::Sdk {
                operation: "get_object_section_frame",
            });
        }
        let id = self.object_id(object.slot())?;
        let scene = self.host.scene.lock().unwrap();
        Ok(scene
            .by_id(id)
            .ok_or(EditError::Sdk {
                operation: "get_object_section_frame",
            })?
            .sections())
    }

    fn create_object_section(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        frame: usize,
    ) -> Result<(), EditError> {
        self.mutation("create_object_section")?;
        self.with_object(object, "create_object_section", |found| {
            found.section_points.push(frame);
            found.section_points.sort_unstable();
            Ok(())
        })
    }

    fn delete_object_section(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        section: usize,
    ) -> Result<(), EditError> {
        self.mutation("delete_object_section")?;
        self.with_object(object, "delete_object_section", |found| {
            // 区間 `i` の開始位置は `i` 番目の中間点である。1 つずらして
            // 添字を引く実装は、ここで別の中間点を消す。
            let point = section
                .checked_sub(1)
                .filter(|index| *index < found.section_points.len())
                .ok_or(EditError::SectionChangeRejected {
                    operation: "delete_object_section",
                })?;
            found.section_points.remove(point);
            Ok(())
        })
    }

    fn move_object_section(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        section: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.mutation("move_object_section")?;
        self.with_object(object, "move_object_section", |found| {
            let point = section
                .checked_sub(1)
                .filter(|index| *index < found.section_points.len())
                .ok_or(EditError::SectionChangeRejected {
                    operation: "move_object_section",
                })?;
            found.section_points[point] = frame;
            found.section_points.sort_unstable();
            Ok(())
        })
    }

    fn set_object_name(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        name: Option<&str>,
    ) -> Result<(), EditError> {
        self.mutation("set_object_name")?;
        if self.host.knobs().fault == Some(Fault::IgnoreObjectName) {
            return Ok(());
        }
        let id = self.object_id(object.slot())?;
        let mut scene = self.host.scene.lock().unwrap();
        let object = scene.by_id_mut(id).ok_or(EditError::Sdk {
            operation: "set_object_name",
        })?;
        object.placement.name = name.map(str::to_string);
        Ok(())
    }

    fn create_effect(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        effect_name: &str,
    ) -> Result<(), EditError> {
        self.mutation("create_effect")?;
        let knobs = self.host.knobs();
        let id = self.object_id(object.slot())?;
        let mut scene = self.host.scene.lock().unwrap();
        let object = scene.by_id_mut(id).ok_or(EditError::Sdk {
            operation: "create_effect",
        })?;
        let added = HostEffect {
            name: effect_name.to_string(),
            index: 0,
            enabled: true,
            locked: false,
            items: Vec::new(),
        };
        // 付与位置はホストが決める。末尾に限るとは定められていない。
        if knobs.fault == Some(Fault::PrependEffect) {
            object.effects.insert(0, added);
        } else {
            if knobs.fault == Some(Fault::AddTwoEffects) {
                object.effects.push(added.clone());
            }
            object.effects.push(added);
        }
        // 同名の順序は列の出現順で決まる。
        renumber(&mut object.effects);
        Ok(())
    }

    fn delete_effect(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        effect: &ResolvedEffect<'_>,
    ) -> Result<(), EditError> {
        self.mutation("delete_effect")?;
        let id = self.object_id(object.slot())?;
        let (_, position) = self.effect_ref(effect.slot())?;
        let mut scene = self.host.scene.lock().unwrap();
        let object = scene.by_id_mut(id).ok_or(EditError::Sdk {
            operation: "delete_effect",
        })?;
        object.effects.remove(position);
        renumber(&mut object.effects);
        Ok(())
    }

    fn set_effect_enabled(
        &self,
        _ticket: MutationTicket<'_>,
        effect: &ResolvedEffect<'_>,
        enabled: bool,
    ) -> Result<(), EditError> {
        self.mutation("set_effect_enable")?;
        if self.host.knobs().fault == Some(Fault::IgnoreEffectState) {
            return Ok(());
        }
        self.with_effect(effect, |effect| effect.enabled = enabled)
    }

    fn set_effect_item(
        &self,
        _ticket: MutationTicket<'_>,
        effect: &ResolvedEffect<'_>,
        item: &str,
        value: &str,
    ) -> Result<(), EditError> {
        self.mutation("set_effect_item_value")?;
        // 渡された文字列をそのまま覚える。逆操作が書き戻す値が、ホストが返した
        // 文字列そのものであることを検査できる。
        self.host
            .item_values
            .lock()
            .unwrap()
            .push(value.to_string());
        let item = item.to_string();
        let value = value.to_string();
        self.with_effect(effect, move |effect| {
            if let Some(entry) = effect.items.iter_mut().find(|entry| entry.name == item)
                && let Some(written) = host_write(&entry.item_type, &value)
            {
                entry.value = written;
            }
        })
    }

    fn set_layer_name(
        &self,
        _ticket: MutationTicket<'_>,
        layer: usize,
        name: Option<&str>,
    ) -> Result<(), EditError> {
        self.mutation("set_layer_name")?;
        // 渡された引数をそのまま覚える。`None`（名前を渡さない）と `Some("")`
        // （空の名前を渡す）を畳むと、標準名へ戻す指定が本当に名前を渡して
        // いないことを確かめられなくなる。
        self.host
            .layer_names
            .lock()
            .unwrap()
            .push(name.map(str::to_string));
        let name = name.map(str::to_string);
        self.with_layer(layer, "set_layer_name", |fake| fake.name = name)
    }

    fn set_layer_enabled(
        &self,
        _ticket: MutationTicket<'_>,
        layer: usize,
        enabled: bool,
    ) -> Result<(), EditError> {
        self.mutation("set_layer_enable")?;
        self.with_layer(layer, "set_layer_enable", |fake| fake.enabled = enabled)
    }

    fn set_layer_locked(
        &self,
        _ticket: MutationTicket<'_>,
        layer: usize,
        locked: bool,
    ) -> Result<(), EditError> {
        self.mutation("set_layer_lock")?;
        self.with_layer(layer, "set_layer_lock", |fake| fake.locked = locked)
    }

    fn set_scene_name(&self, _ticket: MutationTicket<'_>, name: &str) -> Result<(), EditError> {
        self.mutation("set_scene_name")?;
        // ホストが要求を黙って捨てる。区間の内側の読み直しだけが気付ける。
        if self.host.knobs().fault == Some(Fault::IgnoreSceneName) {
            return Ok(());
        }
        self.host.scene.lock().unwrap().name = name.to_string();
        Ok(())
    }

    fn set_scene_size(
        &self,
        _ticket: MutationTicket<'_>,
        width: usize,
        height: usize,
    ) -> Result<(), EditError> {
        self.mutation("set_scene_size")?;
        let (width, height) = (width as u32, height as u32);
        // ホストが指定を調整し得る。区間を抜けた後の観測だけがその値を見る。
        let (width, height) = match self.host.knobs().fault {
            Some(Fault::ClampSceneSettings) => {
                (width.min(MAX_SCENE_WIDTH), height.min(MAX_SCENE_HEIGHT))
            }
            _ => (width, height),
        };
        let mut scene = self.host.scene.lock().unwrap();
        scene.width = width;
        scene.height = height;
        Ok(())
    }

    fn set_scene_sample_rate(
        &self,
        _ticket: MutationTicket<'_>,
        sample_rate: usize,
    ) -> Result<(), EditError> {
        self.mutation("set_scene_sample_rate")?;
        let sample_rate = sample_rate as u32;
        let sample_rate = match self.host.knobs().fault {
            Some(Fault::ClampSceneSettings) => sample_rate.min(MAX_SCENE_SAMPLE_RATE),
            _ => sample_rate,
        };
        self.host.scene.lock().unwrap().sample_rate = sample_rate;
        Ok(())
    }

    fn set_grid_bpm_list(
        &self,
        _ticket: MutationTicket<'_>,
        entries: &[GridBpm],
    ) -> Result<(), EditError> {
        self.mutation("set_grid_bpm_list")?;
        match self.host.knobs().fault {
            // ホストが要求を黙って捨てる。件数の照合だけが気付ける。
            Some(Fault::IgnoreGridBpm) => return Ok(()),
            // ホストは単精度で受け取り、並べ替えもする。件数は変わらない。
            Some(Fault::RewriteGridBpmValues) => {
                let mut rewritten: Vec<GridBpm> = entries
                    .iter()
                    .map(|entry| GridBpm {
                        tempo: FiniteF64::try_new(entry.tempo.get() + 1.0).unwrap(),
                        beat: entry.beat + 1,
                        start: entry.start,
                        offset: FiniteF64::try_new(entry.offset.get() + 1.0).unwrap(),
                    })
                    .collect();
                rewritten.reverse();
                self.host.scene.lock().unwrap().grid_bpm = rewritten;
                return Ok(());
            }
            _ => {}
        }
        self.host.scene.lock().unwrap().grid_bpm = entries.to_vec();
        Ok(())
    }

    fn set_cursor(
        &self,
        _ticket: MutationTicket<'_>,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.mutation("set_cursor_layer_frame")?;
        // ホストは範囲外の値をクランプする。
        let mut scene = self.host.scene.lock().unwrap();
        scene.cursor = Cursor {
            layer: layer.min(MAX_LAYER),
            frame: frame.min(MAX_FRAME),
        };
        Ok(())
    }

    fn set_display_start(
        &self,
        _ticket: MutationTicket<'_>,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError> {
        self.mutation("set_display_layer_frame")?;
        // ホストは範囲外の値をクランプする。表示フレーム数・レイヤー数は開始位置
        // とは無関係にホストが決めるため、要求値と一致しない値を返す。
        let mut scene = self.host.scene.lock().unwrap();
        scene.display = DisplayRange {
            frame_start: frame.min(MAX_FRAME),
            layer_start: layer.min(MAX_LAYER),
            frame_num: DISPLAY_FRAME_NUM,
            layer_num: DISPLAY_LAYER_NUM,
        };
        Ok(())
    }

    fn set_select_range(
        &self,
        _ticket: MutationTicket<'_>,
        range: Option<FrameRange>,
    ) -> Result<(), EditError> {
        self.mutation("set_select_range")?;
        self.host.scene.lock().unwrap().selected_range = range;
        Ok(())
    }

    fn set_focus_object(
        &self,
        _ticket: MutationTicket<'_>,
        object: Option<&ResolvedObject<'_>>,
    ) -> Result<(), EditError> {
        self.mutation("set_focus_object")?;
        if self.host.knobs().fault == Some(Fault::FocusGone) {
            return Err(EditError::NotIssued {
                reason: NotIssuedReason::TargetMissing,
            });
        }
        let id = object
            .map(|object| self.object_id(object.slot()))
            .transpose()?;
        self.host.scene.lock().unwrap().focus = id;
        Ok(())
    }
}

impl FakeSceneEditor<'_> {
    /// レイヤーへ変更を適用する。
    ///
    /// 状態を無言で保たせる仕込みもここで働かせる。戻り値を持たない setter が
    /// 無視されたときに read-back が捕まえられることを確かめる。
    fn with_layer(
        &self,
        layer: usize,
        operation: &'static str,
        apply: impl FnOnce(&mut FakeLayer),
    ) -> Result<(), EditError> {
        if self.host.knobs().fault == Some(Fault::IgnoreLayerState) {
            return Ok(());
        }
        let mut scene = self.host.scene.lock().unwrap();
        let fake = scene
            .layers
            .get_mut(layer)
            .ok_or(EditError::Sdk { operation })?;
        apply(fake);
        Ok(())
    }

    /// 解決済み effect へ変更を適用する。
    fn with_effect(
        &self,
        effect: &ResolvedEffect<'_>,
        apply: impl FnOnce(&mut HostEffect),
    ) -> Result<(), EditError> {
        let (id, position) = self.effect_ref(effect.slot())?;
        let mut scene = self.host.scene.lock().unwrap();
        let object = scene.by_id_mut(id).ok_or(EditError::Sdk {
            operation: "get_effect_list",
        })?;
        let effect = object.effects.get_mut(position).ok_or(EditError::Sdk {
            operation: "get_effect_list",
        })?;
        apply(effect);
        Ok(())
    }

    /// 作成をフェイクの状態へ反映する。
    ///
    /// 長さと挿入位置はホストが決める。要求した位置と異なる配置になる状況を
    /// 再現するため、開始フレームを 1 つ後ろへずらす。
    fn create(&self, layer: usize, frame: usize, alias: String) -> Result<(), EditError> {
        if self.host.knobs().fault == Some(Fault::CreateNothing) {
            return Ok(());
        }
        let mut scene = self.host.scene.lock().unwrap();
        let id = scene.take_id();
        scene.insert(
            layer,
            FakeObject {
                id,
                placement: HostObjectPlacement {
                    layer,
                    frame_start: frame + CREATE_FRAME_SHIFT,
                    frame_end: frame + CREATE_FRAME_SHIFT + 59,
                    name: None,
                },
                alias: alias.clone(),
                effects: Vec::new(),
                section_points: Vec::new(),
            },
        );
        if self.host.knobs().fault == Some(Fault::CreatePair) {
            // 2 件目は配置先とは別のレイヤーへ置く。配置先だけを走査していると
            // 応答に現れず、要求元から到達できなくなる。
            let sibling = layer + 1;
            let id = scene.take_id();
            scene.insert(
                sibling,
                FakeObject {
                    id,
                    placement: HostObjectPlacement {
                        layer: sibling,
                        frame_start: frame + CREATE_FRAME_SHIFT + 60,
                        frame_end: frame + CREATE_FRAME_SHIFT + 119,
                        name: None,
                    },
                    alias,
                    effects: Vec::new(),
                    section_points: Vec::new(),
                },
            );
        }
        Ok(())
    }
}

/// ホストが作成位置を自動調整する量。
pub(crate) const CREATE_FRAME_SHIFT: usize = 5;

/// ホストが移動先を自動調整する量。
pub(crate) const MOVE_FRAME_SHIFT: usize = 7;

/// カーソルと表示開始位置がクランプされる上限。
pub(crate) const MAX_LAYER: usize = 9;
/// カーソルと表示開始位置がクランプされる上限。
pub(crate) const MAX_FRAME: usize = 999;
/// ホストが返す表示フレーム数。表示開始フレームとは一致しない。
pub(crate) const DISPLAY_FRAME_NUM: usize = 600;
/// ホストが返す表示レイヤー数。表示開始レイヤーとは一致しない。
pub(crate) const DISPLAY_LAYER_NUM: usize = 4;
/// ホストが設定項目の値を丸める上限。
pub(crate) const MAX_ITEM_VALUE: i64 = 100;

/// ホストが受け付ける選択肢の値。
///
/// SDK は選択肢を列挙する手段を持たないため、フェイクもこの一覧を公開しない。
/// 一覧に無い値の書き込みは黙って無視される。
pub(crate) const CHOICE_VALUES: [&str; 2] = ["円", "四角形"];

/// ホストが項目名に対して返す生の設定値。
///
/// 保持している値をそのまま文字列にする。種別ごとに別の表記へ写すと、書き込みが
/// 渡した文字列と読み直した文字列を比べる検査が成立しない。
fn raw_item_value(value: &ItemValue) -> String {
    match value {
        ItemValue::Unknown { raw } => raw.clone(),
        ItemValue::Integer { value } => value.to_string(),
        ItemValue::Number { value } => value.to_string(),
        ItemValue::Bool { value } => if *value { "1" } else { "0" }.to_string(),
        ItemValue::Color { value } | ItemValue::Choice { value } | ItemValue::Text { value } => {
            value.clone()
        }
        ItemValue::Font { name } => name.clone(),
        ItemValue::File { path } | ItemValue::Folder { path } => path.clone(),
    }
}

/// ホストが書き込む値。受け付けない値では `None` を返し、状態を変えない。
///
/// 選択肢から選ぶ種別は、選択肢に無い値を失敗を返さずに無視する。ほかの種別は
/// 種別に対応する値として受け付け、一部は表記を正規化する——整数と実数の上限
/// への丸め、色の小文字化、改行のエスケープ表記である。**正規化した値は書いた
/// 文字列と一致しない。**
///
/// 書き込みを公開していない種別は生の文字列のまま保つ。読み取り経路がそれらを
/// 生値として返すため、書き込みだけが別の形へ写ると場面の状態が種別と食い違う。
fn host_write(item_type: &EffectItemType, value: &str) -> Option<ItemValue> {
    match item_type {
        EffectItemType::Select
        | EffectItemType::Combo
        | EffectItemType::Mask
        | EffectItemType::Figure => CHOICE_VALUES.contains(&value).then(|| ItemValue::Choice {
            value: value.to_string(),
        }),
        EffectItemType::Color => Some(ItemValue::Color {
            value: value.to_lowercase(),
        }),
        EffectItemType::Number => Some(ItemValue::Number {
            value: value
                .parse::<f64>()
                .ok()
                .map(|parsed| parsed.min(MAX_ITEM_VALUE as f64))
                .and_then(FiniteF64::try_new)
                .unwrap_or_else(|| FiniteF64::try_new(0.0).expect("有限値")),
        }),
        EffectItemType::Text | EffectItemType::String => Some(ItemValue::Text {
            value: value.replace('\n', "\\n"),
        }),
        EffectItemType::Check => Some(ItemValue::Bool {
            value: value != "0",
        }),
        EffectItemType::File => Some(ItemValue::File {
            path: value.to_string(),
        }),
        EffectItemType::Folder => Some(ItemValue::Folder {
            path: value.to_string(),
        }),
        EffectItemType::Font => Some(ItemValue::Font {
            name: value.to_string(),
        }),
        EffectItemType::Integer => Some(ItemValue::Integer {
            value: value.parse::<i64>().unwrap_or_default().min(MAX_ITEM_VALUE),
        }),
        EffectItemType::Scene
        | EffectItemType::Range
        | EffectItemType::Data
        | EffectItemType::Unknown(_) => Some(ItemValue::Unknown {
            raw: value.to_string(),
        }),
    }
}

/// 同名 effect の順序を出現順で振り直す。
fn renumber(effects: &mut [HostEffect]) {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for effect in effects.iter_mut() {
        let next = seen.entry(effect.name.clone()).or_insert(0);
        effect.index = *next;
        *next += 1;
    }
}

/// フェイクの編集情報。
pub(crate) fn fake_edit_info() -> HostEditInfo {
    HostEditInfo {
        scene_id: SCENE_ID,
        width: 1920,
        height: 1080,
        fps_rate: 30,
        fps_scale: 1,
        sample_rate: 48000,
        cursor_frame: 0,
        cursor_layer: 0,
        frame_max: 3600,
        layer_max: 2,
        display_frame_start: 0,
        display_layer_start: 0,
        display_frame_num: 600,
        display_layer_num: 10,
        select_range_start: None,
        select_range_end: None,
    }
}

/// 設定項目を 1 つ持つ effect。
pub(crate) fn blur(index: usize, range: i64) -> HostEffect {
    HostEffect {
        name: "ぼかし".to_string(),
        index,
        enabled: true,
        locked: false,
        items: vec![EffectItem {
            name: "範囲".to_string(),
            item_type: EffectItemType::Integer,
            value: ItemValue::Integer { value: range },
            track: None,
        }],
    }
}

/// 選択肢から選ぶ設定項目と、ホストが表記を正規化する設定項目を並べて持つ effect。
///
/// 選択肢から選ぶ種別は 1 つに絞らない。ホストの挙動は種別によらず同じであり、
/// 1 種別だけを置くと種別ごとに経路が分かれても気付けない。
///
/// 既定の状態には含めない。選択肢の検証を要する試験だけが
/// [`shape_catalog_entry`] と対にして差し込む。
pub(crate) fn shape(index: usize) -> HostEffect {
    HostEffect {
        name: SHAPE.to_string(),
        index,
        enabled: true,
        locked: false,
        items: vec![
            EffectItem {
                name: "図形の種類".to_string(),
                item_type: EffectItemType::Select,
                value: ItemValue::Choice {
                    value: CHOICE_VALUES[0].to_string(),
                },
                track: None,
            },
            EffectItem {
                name: "マスクの種類".to_string(),
                item_type: EffectItemType::Mask,
                value: ItemValue::Choice {
                    value: CHOICE_VALUES[0].to_string(),
                },
                track: None,
            },
            EffectItem {
                name: "形状".to_string(),
                item_type: EffectItemType::Figure,
                value: ItemValue::Choice {
                    value: CHOICE_VALUES[0].to_string(),
                },
                track: None,
            },
            EffectItem {
                name: "色".to_string(),
                item_type: EffectItemType::Color,
                value: ItemValue::Color {
                    value: "#ffffff".to_string(),
                },
                track: None,
            },
            EffectItem {
                name: "サイズ".to_string(),
                item_type: EffectItemType::Number,
                value: ItemValue::Number {
                    value: FiniteF64::try_new(1.0).expect("有限値"),
                },
                track: None,
            },
            EffectItem {
                name: "メモ".to_string(),
                item_type: EffectItemType::Text,
                value: ItemValue::Text {
                    value: String::new(),
                },
                track: None,
            },
        ],
    }
}

/// [`shape`] をカタログへ載せる形。
pub(crate) fn shape_catalog_entry() -> AvailableEffect {
    AvailableEffect {
        name: SHAPE.to_string(),
        effect_type: EffectType::Filter,
        flags: EffectFlags::from_raw(1),
        items: shape(0)
            .items
            .into_iter()
            .map(|item| AvailableEffectItem {
                name: item.name,
                item_type: item.item_type,
            })
            .collect(),
    }
}

/// [`shape`] の effect 名。
pub(crate) const SHAPE: &str = "図形";

/// 入力 effect。
fn video() -> HostEffect {
    HostEffect {
        name: "動画ファイル".to_string(),
        index: 0,
        enabled: true,
        locked: false,
        items: Vec::new(),
    }
}

/// フェイクの初期状態。
pub(crate) fn fake_scene() -> FakeScene {
    FakeScene {
        layers: vec![
            FakeLayer::with(vec![FakeObject {
                id: 1,
                placement: HostObjectPlacement {
                    layer: 0,
                    frame_start: 0,
                    frame_end: 99,
                    name: None,
                },
                alias: "[0:0]".to_string(),
                effects: Vec::new(),
                section_points: Vec::new(),
            }]),
            FakeLayer::with(vec![
                FakeObject {
                    id: 2,
                    placement: HostObjectPlacement {
                        layer: 1,
                        frame_start: 100,
                        frame_end: 200,
                        name: Some("立ち絵".to_string()),
                    },
                    alias: "[1:100]".to_string(),
                    effects: vec![video(), blur(0, 20)],
                    // 中間点を 1 つ持つ。区間は 2 つになり、区間番号 1 が
                    // この中間点を指す。
                    section_points: vec![150],
                },
                FakeObject {
                    id: 3,
                    placement: HostObjectPlacement {
                        layer: 1,
                        frame_start: 300,
                        frame_end: 400,
                        name: Some("字幕".to_string()),
                    },
                    alias: "[1:300]".to_string(),
                    effects: vec![blur(0, 20)],
                    section_points: Vec::new(),
                },
            ]),
            FakeLayer {
                locked: true,
                ..FakeLayer::with(vec![FakeObject {
                    id: 4,
                    placement: HostObjectPlacement {
                        layer: 2,
                        frame_start: 0,
                        frame_end: 99,
                        name: None,
                    },
                    alias: "[2:0]".to_string(),
                    effects: Vec::new(),
                    section_points: Vec::new(),
                }])
            },
            // 編集情報が名乗る layer_max より先にある空レイヤー。ここへ作ると
            // オブジェクトの存在する最大レイヤーが伸びる。
            FakeLayer::empty(),
            FakeLayer::empty(),
        ],
        next_id: 100,
        // 編集情報が名乗る値と揃える。シーン設定の変更だけがこちらを動かし、
        // 区間を抜けた後の観測はその値を返す。
        name: SCENE_NAME.to_string(),
        width: fake_edit_info().width,
        height: fake_edit_info().height,
        sample_rate: fake_edit_info().sample_rate,
        cursor: Cursor { frame: 0, layer: 0 },
        selected_range: None,
        focus: None,
        focus_section: None,
        selected: Vec::new(),
        display: DisplayRange {
            frame_start: 0,
            layer_start: 0,
            frame_num: DISPLAY_FRAME_NUM,
            layer_num: DISPLAY_LAYER_NUM,
        },
        grid_bpm: vec![GridBpm {
            tempo: FiniteF64::try_new(120.0).unwrap(),
            beat: 4,
            start: FiniteF64::try_new(0.0).unwrap(),
            offset: FiniteF64::try_new(0.0).unwrap(),
        }],
    }
}

/// フェイクの effect カタログ。
pub(crate) fn fake_catalog() -> Vec<AvailableEffect> {
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
        AvailableEffect {
            name: "音声フェード".to_string(),
            effect_type: EffectType::Filter,
            // 音声だけを扱う。画像のフラグは立たない。
            flags: EffectFlags::from_raw(2),
            items: Vec::new(),
        },
        AvailableEffect {
            name: "標準描画".to_string(),
            effect_type: EffectType::Output,
            flags: EffectFlags::from_raw(1),
            items: Vec::new(),
        },
    ]
}
