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
    EditState, HostEditInfo, HostEffect, HostEffectFacets, HostEffectHelp, HostEffectSummary,
    HostLayer, HostObject, HostObjectDetail, HostObjectPlacement, ReadHost, SceneReader,
    SceneValueReader,
};
use crate::test_support::alias_with_effects;
use aviutl2_mcp_core::{
    AvailableEffectItem, Cursor, DisplayRange, EffectFlags, EffectItem, EffectItemType, EffectType,
    EvaluatedItemKind, FiniteF64, FrameRange, GridBpm, ItemFacets, ItemGroup, ItemValue,
    ModuleEntry, ModuleType, Movement, PALETTE_COLOR_COUNT, PaletteEntry, Rgba, SectionRange,
    TrackDecodeError, TrackInfo, TrackValue, decode_host_text, decode_track_value,
    encode_host_text,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
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
    /// effect の順序の移動を無言で無視する。
    IgnoreEffectMove,
    /// 1 度目の移動だけ、effect を要求した位置ではなく列の末尾へ動かす。
    ///
    /// 2 度目以降は要求どおりに動かす。移動先を切り詰めるホストは、受け付け
    /// られる位置を指した移動までは拒まない。
    AppendMovedEffect,
    /// 1 度目の移動で effect を列の末尾へ動かし、2 度目以降の移動を無言で
    /// 無視する。
    ///
    /// **戻す移動だけが効かない状況を作る。** 移動の要求は 1 度目が前向きの
    /// 移動、2 度目が巻き戻しである。ホストは移動の成否を返さないため、列が
    /// 戻らなかったことは読み直してからでないと分からない。
    ///
    /// 数えるのは編集区間ごとであり、要求をまたいで積み上がらない。
    IgnoreEffectMoveRestore,
    /// effect は要求した位置へ動かすが、戻り値だけ別の数を名乗る。
    MisreportEffectPosition,
    /// effect の順序の移動が、動かした 1 件とは別の 1 件を列から落とす。
    ///
    /// 動かした 1 件は要求どおりの位置へ入る。移動先にも元の位置にも食い違いが
    /// 現れないため、**列の長さだけが変化を示す。**
    DropAnotherEffect,
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
    /// 設定項目の値を、変更を発行した後にだけ読めない。
    ///
    /// 書き込みの前の読み取りは通り、書き込んだ後の照合だけが落ちる。**巻き
    /// 戻しの材料は手元にあるのに、書き込みが適用されたかを確かめられない状況**
    /// であり、[`Fault::ItemValueUnreadable`] では作れない——あちらは書き込みの
    /// 前の読み取りごと落とすため、変更が発行される前に要求が終わる。
    ///
    /// 差し込む条件は [`Fault::ReadBack`] と同じである。
    ItemValueUnreadableAfterMutation,
    /// 設定項目への 2 回目以降の書き込みを無言で無視する。
    ///
    /// **戻す書き込みだけが効かない状況を作る。** 単独の書き込みは、前向きの
    /// 書き込みが 1 回目、巻き戻しが 2 回目である。ホストが失敗を返さないまま
    /// 元へ戻らない経路は、書き込み API の戻り値では観測できない。
    ///
    /// 数えるのは編集区間ごとであり、要求をまたいで積み上がらない。
    IgnoreItemRestore,
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

    fn find_mut(&mut self, layer: usize, frame_start: usize) -> Option<&mut FakeObject> {
        self.layers
            .get_mut(layer)?
            .objects
            .iter_mut()
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
    pub(crate) catalog: Vec<FakeCatalogEntry>,
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
    /// ホストが受け付ける移動方法と、その名前で書けるかどうか。
    movements: Mutex<Vec<Movement>>,
    /// 実機ならプロセスを落としていた移動方法を、渡された順に覚える。
    ///
    /// **panic だけに頼らない。** 編集の入口は panic を捕捉して失敗の応答へ
    /// 畳むため、adapter を通す経路では「落ちる入力が通り抜けたこと」が
    /// 内部失敗と見分けられなくなる。記録は捕捉に飲まれない。
    fatal_movements: Mutex<Vec<String>>,
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
            fonts: FONT_NAMES.iter().map(|name| name.to_string()).collect(),
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
            movements: Mutex::new(
                TRACK_MODES
                    .iter()
                    .map(|name| Movement {
                        name: name.to_string(),
                        writable: true,
                    })
                    .collect(),
            ),
            fatal_movements: Mutex::new(Vec::new()),
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

    /// ホストが受け付ける移動方法を差し替える。
    ///
    /// 空にすると「一覧を引けない環境」になる。実機では設定ファイルを読めない
    /// 場合がこれにあたる。
    ///
    /// 可否を偽にした名前は、一覧に載るのに書けない移動方法になる。
    pub(crate) fn set_movements(&self, movements: Vec<Movement>) {
        *self.movements.lock().unwrap() = movements;
    }

    /// 実機ならプロセスを落としていた移動方法を、渡された順に返す。
    ///
    /// **空でなければ、検証を通り抜けた入力がホストへ届いている。** panic は
    /// 編集の入口が捕捉して失敗の応答へ畳むため、応答だけを見る検査では
    /// 内部失敗と区別できない。この記録は捕捉に飲まれない。
    pub(crate) fn fatal_movement_writes(&self) -> Vec<String> {
        self.fatal_movements.lock().unwrap().clone()
    }

    /// 実機ならプロセスを落としていた移動方法を覚える。
    fn record_fatal_movement(&self, mode: String) {
        self.fatal_movements.lock().unwrap().push(mode);
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
    "move_effect",
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

    fn effect_catalog(&self) -> Result<Vec<HostEffectSummary>, EditError> {
        self.assert_ready("get_effects");
        self.record("effect_catalog");
        Ok(self.catalog.iter().map(FakeCatalogEntry::summary).collect())
    }

    fn effect_item_catalog(
        &self,
        effect_name: &str,
    ) -> Result<Vec<AvailableEffectItem>, EditError> {
        self.assert_ready("enum_effect_item");
        self.record("effect_item_catalog");
        // カタログに無い名前は列挙そのものが失敗する。SDK は「その名前の効果が
        // 無い」と「列挙に失敗した」を同じ形で返す。
        self.catalog
            .iter()
            .find(|entry| entry.name == effect_name)
            .map(|entry| entry.items.clone())
            .ok_or(EditError::Sdk {
                operation: "enum_effect_item",
            })
    }

    fn alias_data_directory(&self) -> Option<PathBuf> {
        self.record("alias_data_directory");
        self.alias_data_dir.lock().unwrap().clone()
    }

    fn movements(&self) -> Vec<Movement> {
        self.movements.lock().unwrap().clone()
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
            item_writes: Cell::new(0),
            effect_moves: Cell::new(0),
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

    fn effect_catalog(&self) -> Result<Vec<HostEffectSummary>, EditError> {
        self.as_ref().effect_catalog()
    }

    fn effect_item_catalog(
        &self,
        effect_name: &str,
    ) -> Result<Vec<AvailableEffectItem>, EditError> {
        self.as_ref().effect_item_catalog(effect_name)
    }

    fn alias_data_directory(&self) -> Option<PathBuf> {
        self.as_ref().alias_data_directory()
    }

    fn movements(&self) -> Vec<Movement> {
        self.as_ref().movements()
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

    fn effect_catalog(&self) -> Result<Vec<HostEffectSummary>, ReadError> {
        Ok(self
            .0
            .catalog
            .iter()
            .map(FakeCatalogEntry::summary)
            .collect())
    }

    fn effect_item_count(&self, effect_name: &str) -> Result<usize, ReadError> {
        Ok(self.effect_items(effect_name)?.len())
    }

    fn effect_items(&self, effect_name: &str) -> Result<Vec<AvailableEffectItem>, ReadError> {
        self.0
            .catalog
            .iter()
            .find(|entry| entry.name == effect_name)
            .map(|entry| entry.items.clone())
            .ok_or(ReadError::Sdk {
                operation: "enum_effect_item",
            })
    }

    fn effect_item_group(
        &self,
        _effect_name: &str,
        _item_name: &str,
    ) -> Result<Option<ItemGroup>, ReadError> {
        Ok(None)
    }

    fn effect_help(&self, _effect_name: &str) -> HostEffectHelp {
        // 編集経路は説明を読まない。説明の供給源を持たない環境を写す。
        HostEffectHelp::default()
    }

    fn effect_facets(&self, effect_name: &str) -> HostEffectFacets {
        HostEffectFacets {
            items: self
                .0
                .catalog
                .iter()
                .find(|entry| entry.name == effect_name)
                .map(|entry| entry.facets.clone())
                .unwrap_or_default(),
        }
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
            item_writes: Cell::new(0),
            effect_moves: Cell::new(0),
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
    /// この区間で設定項目へ書き込んだ回数。
    ///
    /// [`Fault::IgnoreItemRestore`] が「戻す書き込み」を見分ける唯一の手掛かり
    /// である。区間ごとに数えるため、要求をまたいで積み上がらない。
    item_writes: Cell<usize>,
    /// この区間で effect の順序を動かした回数。
    ///
    /// [`Fault::AppendMovedEffect`] と [`Fault::IgnoreEffectMoveRestore`] が
    /// 前向きの移動と戻す移動を見分ける手掛かりである。
    effect_moves: Cell<usize>,
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
        let mut scene = self.host.scene.lock().unwrap();
        let object = scene
            .find_mut(layer, frame_start)
            .ok_or(ReadError::ObjectNotFound {
                detected_by: "find_object",
            })?;
        reinterpret_saved_values(object);
        Ok(object.detail())
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
            .find(|entry| entry.name == *name)
            .map(|entry| entry.items.clone())
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
        let unreadable = match self.host.knobs().fault {
            Some(Fault::ItemValueUnreadable) => true,
            // 変更を発行した後にだけ落とす。書き込みの前の読み取りは通るため、
            // 巻き戻しの材料は手元に残る。
            Some(Fault::ItemValueUnreadableAfterMutation) => self.host.mutated(),
            _ => false,
        };
        if unreadable {
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

    fn move_effect(
        &self,
        _ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        effect: &ResolvedEffect<'_>,
        position: usize,
    ) -> Result<usize, EditError> {
        self.mutation("move_effect")?;
        let knobs = self.host.knobs();
        if knobs.fault == Some(Fault::IgnoreEffectMove) {
            return Ok(position);
        }
        let moves = self.effect_moves.get();
        self.effect_moves.set(moves + 1);
        // 2 度目以降の移動だけが効かない。発行そのものは成功として返す——
        // ホストは移動の成否を返さないため、効かなかったことは読み直して
        // からでないと分からない。
        if moves > 0 && knobs.fault == Some(Fault::IgnoreEffectMoveRestore) {
            return Ok(position);
        }
        let id = self.object_id(object.slot())?;
        let (_, from) = self.effect_ref(effect.slot())?;
        let mut scene = self.host.scene.lock().unwrap();
        let object = scene.by_id_mut(id).ok_or(EditError::Sdk {
            operation: "move_effect",
        })?;
        let moved = object.effects.remove(from);
        // 抜いた後の列に対する挿し込みであり、末尾までを受け付ける。
        let appends = matches!(
            knobs.fault,
            Some(Fault::AppendMovedEffect | Fault::IgnoreEffectMoveRestore)
        );
        let to = if appends && moves == 0 {
            object.effects.len()
        } else {
            position.min(object.effects.len())
        };
        object.effects.insert(to, moved);
        if knobs.fault == Some(Fault::DropAnotherEffect) {
            // 落とすのは移動先より後ろの 1 件である。
            let victim = to + 1;
            if victim < object.effects.len() {
                object.effects.remove(victim);
            }
        }
        renumber(&mut object.effects);
        if knobs.fault == Some(Fault::MisreportEffectPosition) {
            return Ok(to + 1);
        }
        Ok(to)
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
        let writes = self.item_writes.get();
        self.item_writes.set(writes + 1);
        // 2 回目以降の書き込みだけが効かない。発行そのものは成功として返す——
        // ホストは書き込みの成否を返さないため、効かなかったことは読み直して
        // からでないと分からない。
        if writes > 0 && self.host.knobs().fault == Some(Fault::IgnoreItemRestore) {
            return Ok(());
        }
        let item = item.to_string();
        let value = value.to_string();
        // フォント名の妥当性は登録済みの一覧で決まる。種別だけでは書き換えの
        // 結果が決まらない設定項目がある。
        let fonts = self.host.fonts.clone();
        let host = self.host;
        self.with_effect(effect, move |effect| {
            let Some(entry) = effect.items.iter_mut().find(|entry| entry.name == item) else {
                return;
            };
            // 落ちる入力がここへ届いたことを、panic とは別に記録する。編集の
            // 入口は panic を捕捉するため、記録が無いと応答からは見分けられない。
            if let Some(mode) = fatal_movement(&entry.item_type, &value) {
                host.record_fatal_movement(mode);
            }
            if let Some(written) = host_write(&entry.item_type, &value, &fonts) {
                entry.track = host_track(&written, entry.track.as_ref());
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
/// ホストが設定項目の値を切り詰める上限。
pub(crate) const MAX_ITEM_VALUE: i64 = 100;

/// ホストが設定項目の値を切り詰める下限。
///
/// 上限とは別に置く。実機は下限側も同じように切り詰めるため、片側だけを模すと
/// 負の値の扱いを取り違えても気付けない。
pub(crate) const MIN_ITEM_VALUE: i64 = 0;

/// ホストが実数の設定項目を丸める小数桁数。
///
/// 読み直しの表記もこの桁で揃う。書いた `100` が `100.00` として読めることが、
/// 書き込み後の照合を数値として行わなければならない理由である。
pub(crate) const ITEM_VALUE_DECIMALS: usize = 2;

/// ホストが書式の合わない色に対して入れる既定値。
///
/// **変更前の値ではない。** 不正な色を書くと、要求した色にならないだけでなく
/// 元の色も失われる。
pub(crate) const DEFAULT_COLOR: &str = "ffffff";

/// ホストが受け付ける選択肢の値。
///
/// SDK は選択肢を列挙する手段を持たないため、フェイクもこの一覧を公開しない。
/// 一覧に無い値の書き込みは黙って無視される。
pub(crate) const CHOICE_VALUES: [&str; 2] = ["円", "四角形"];

/// 登録済みのフォント名。
///
/// フォント種別の設定項目はこの一覧の値だけを受け付ける。
pub(crate) const FONT_NAMES: [&str; 2] = ["MS UI Gothic", "游ゴシック"];

/// フォント種別の設定項目が初期状態で持つ値。
pub(crate) const DEFAULT_FONT: &str = FONT_NAMES[0];

/// ホストが項目名に対して返す生の設定値。
///
/// **ホストは書いた文字列をそのまま返すとは限らない。** 実数は項目の小数桁数へ
/// 揃えた表記で返り、書いた `100` は `100.00` として読める。書き込み後の照合が
/// バイト比較のままだと、この一点だけで正しい書き込みが失敗として返る。
///
/// **テキスト種別はエスケープ表記へ包んで返す。** ホストは `\` と改行を包んだ
/// 表記でしか値を返さない。[`host_write`] が同じ表記を解いて保持するため、2 つは
/// 対になっており、包みが増えることも減ることもない。
pub(crate) fn raw_item_value(value: &ItemValue) -> String {
    match value {
        ItemValue::Unknown { raw } => raw.clone(),
        ItemValue::Integer { value } => value.to_string(),
        ItemValue::Number { value } => format!("{:.*}", ITEM_VALUE_DECIMALS, value.get()),
        ItemValue::Bool { value } => if *value { "1" } else { "0" }.to_string(),
        ItemValue::Text { value } => encode_host_text(value),
        ItemValue::Track(track) => raw_track_value(track),
        ItemValue::Color { value } | ItemValue::Choice { value } => value.clone(),
        ItemValue::Font { name } => name.clone(),
        ItemValue::File { path } | ItemValue::Folder { path } => path.clone(),
    }
}

/// ホストが書き込みの結果として保持する値。
///
/// **ホストは素直ではない。** 要求を受け付けない形は 2 つあり、どちらも失敗を
/// 返さない。
///
/// - **元の値のまま動かない**もの。選択肢に無い値と、登録されていないフォント名。
///   `None` がこれを表し、呼び出し側は状態を変えない
/// - **別の値へ倒れる**もの。書式の合わない色は既定値の白へ落ち、値域を外れた
///   数値は境界へ切り詰められ、桁の多い小数は項目の桁へ丸められる。`Some` が
///   倒れた先の値を運ぶ
///
/// 色の書式は 16 進 6 桁だけである。`#` を伴う表記も 3 桁の省略形もホストから
/// 見れば不正であり、元の値ではなく白へ落ちる。受理した色は小文字で保持する。
///
/// テキスト種別は渡された表記を解いて保持する。ホストは `\\` を 1 つの `\` へ、
/// `\n` を改行へ戻してから保存し、[`raw_item_value`] が読み取りへ返すときに同じ
/// 表記へ包み直す。解かずに保持すると、書いた `\` がそのまま残り、実機で起きる
/// 「`\t` が素通りして `\n` だけ改行になる」形を再現できない。
///
/// `data` と未知種別は生の文字列のまま保つ。読み取り経路がそれらを生値として
/// 返すため、書き込みだけが別の形へ写ると場面の状態が種別と食い違う。
fn host_write(item_type: &EffectItemType, value: &str, fonts: &[String]) -> Option<ItemValue> {
    // トラックバーの項目は数値と移動の 2 通りの表記を受ける。移動として読める
    // 文字列を数値として解釈すると、0 へ落ちて書き込みが黙って壊れる。
    if item_type.evaluated_kind() == Some(EvaluatedItemKind::Track) {
        match decode_track_value(value) {
            Ok(track) if track.mode.is_some() => return Some(host_write_track(&track)),
            // 移動を表しているが我々が表せない行は、渡された文字列のまま保つ。
            // ホストは壊れた行も保存して読み取りへ返す。数値へ倒すと、既に
            // 壊れている項目を読む場面をこの上に作れない。
            Err(TrackDecodeError::NotRepresentable(_)) => {
                return Some(ItemValue::Unknown {
                    raw: value.to_string(),
                });
            }
            Ok(_) | Err(TrackDecodeError::NotAMovement) => {}
        }
    }
    match item_type {
        EffectItemType::Select
        | EffectItemType::Combo
        | EffectItemType::Mask
        | EffectItemType::Figure => CHOICE_VALUES.contains(&value).then(|| ItemValue::Choice {
            value: value.to_string(),
        }),
        EffectItemType::Color => Some(ItemValue::Color {
            value: match is_host_color(value) {
                true => value.to_ascii_lowercase(),
                false => DEFAULT_COLOR.to_string(),
            },
        }),
        EffectItemType::Number => Some(ItemValue::Number {
            value: FiniteF64::try_new(round_to_item_decimals(clamp_item_value(
                value.parse::<f64>().unwrap_or_default(),
            )))
            .expect("有限値"),
        }),
        EffectItemType::Text | EffectItemType::String => Some(ItemValue::Text {
            value: decode_host_text(value),
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
        // 登録されていないフォント名は黙殺され、元の値も fingerprint も動かない。
        EffectItemType::Font => fonts
            .iter()
            .any(|font| font == value)
            .then(|| ItemValue::Font {
                name: value.to_string(),
            }),
        EffectItemType::Integer => Some(ItemValue::Integer {
            value: value
                .parse::<i64>()
                .unwrap_or_default()
                .clamp(MIN_ITEM_VALUE, MAX_ITEM_VALUE),
        }),
        // 値域も小数桁も持たない。書かれた十進整数がそのまま入り、数値として
        // 読めない綴りは 0 になる。
        EffectItemType::Scene | EffectItemType::Range => Some(ItemValue::Integer {
            value: value.parse::<i64>().unwrap_or_default(),
        }),
        EffectItemType::Data | EffectItemType::Unknown(_) => Some(ItemValue::Unknown {
            raw: value.to_string(),
        }),
    }
}

/// ホストが受け付ける移動方法の名前。
///
/// **一覧に無い名前を書くと実機はプロセスごと落ちる。** ホストが投げた C++ の
/// 例外が `extern "C"` の境界を越えて入り、巻き戻せずに abort する。フェイクは
/// これを記録と panic の 2 つで模す。落ちる形を「黙って無視される」として模すと、
/// 名前の検証を外しても検査が緑のまま通る。
pub(crate) const TRACK_MODES: [&str; 4] = [
    "直線移動",
    "曲線移動",
    TRACK_DEFAULT_PARAM.0,
    TRACK_TIME_CONTROL_MODE,
];

/// 時間制御を有効にする移動方法の名前の接尾辞。
///
/// 時間制御はフラグではなく移動方法の名前の変種が担う。フラグの欄は変種を
/// 書いても 0 のままである。
const TIME_CONTROL_SUFFIX: &str = "(時間制御)";

/// ホストが受け付ける時間制御の変種。
pub(crate) const TRACK_TIME_CONTROL_MODE: &str = "直線移動(時間制御)";

/// パラメータを取る移動方法と、その既定値。
///
/// 実機で `ランダム移動,0` を書くと `ランダム移動,0|15` として保存される。
/// **既定値の並びがそのまま、この移動方法が取るパラメータの個数でもある。**
pub(crate) const TRACK_DEFAULT_PARAM: (&str, f64) = ("ランダム移動", 15.0);

/// [`TRACK_DEFAULT_PARAM`] の移動方法へホストが入れる既定のパラメータ。
fn track_default_params() -> Vec<FiniteF64> {
    vec![FiniteF64::try_new(TRACK_DEFAULT_PARAM.1).expect("有限値")]
}

/// 保存済みの移動が、ホストの解釈し直しを受ける綴りか。
///
/// パラメータを取る移動方法へ個数の合わない綴りを書くと、ホストは受理したうえで
/// 保存値を既定値へ差し替える。
fn track_params_get_replaced(track: &TrackValue) -> bool {
    track.mode.as_deref() == Some(TRACK_DEFAULT_PARAM.0)
        && track.params.len() != track_default_params().len()
}

/// ホストが保存値を解釈し直し、個数の合わない移動パラメータを既定値へ差し替える。
///
/// **差し替えは書き込みの時点では起きない。** 設定値を書いた直後に読むと書いた
/// とおりが返り、オブジェクトの詳細を読み直した後の読みで既定値が返る。書き込みの
/// 時点で差し替える形に模すと、書いた直後の読みだけで照合する実装も緑になる。
fn reinterpret_saved_values(object: &mut FakeObject) {
    for effect in &mut object.effects {
        for item in &mut effect.items {
            let ItemValue::Track(track) = &item.value else {
                continue;
            };
            if !track_params_get_replaced(track) {
                continue;
            }
            item.value = ItemValue::Track(TrackValue {
                params: track_default_params(),
                ..track.clone()
            });
            item.track = host_track(&item.value, item.track.as_ref());
        }
    }
}

/// 書き込まれた文字列が、実機ならプロセスを落とす移動方法を名乗るか。
///
/// 落とす名前を返す。落とさない書き込みでは `None`。
///
/// **ホストが本当に知っている名前は [`TRACK_MODES`] であり、検証へ渡す一覧
/// （[`FakeEditHost::set_movements`]）とは別である。** 両者を同じ一覧にすると、
/// 検証を通り抜けた書き込みがホストへ届く経路をフェイクの上に作れなくなり、
/// この記録に入る道が 1 つも無くなる。実機でも 2 つは別の出所を持つ——検証が
/// 見るのは設定ファイルの内容であり、落ちるかどうかを決めるのはホストの実装で
/// ある。
fn fatal_movement(item_type: &EffectItemType, value: &str) -> Option<String> {
    if item_type.evaluated_kind() != Some(EvaluatedItemKind::Track) {
        return None;
    }
    let mode = decode_track_value(value).ok()?.mode?;
    (!TRACK_MODES.contains(&mode.as_str())).then_some(mode)
}

/// ホストが移動を含む値として読み取りへ返す生の文字列。
///
/// 値の桁は設定項目の小数桁へ揃う。実機で `-600,600,直線移動,0` を書くと
/// 読み直しは `-600.00,600.00,直線移動,0` になる。
///
/// 値を 1 つも持たない移動はホストの状態としてあり得ないが、フェイクの初期状態を
/// 直接組み立てれば作れる。空の文字列を返して落とさない——落ちる形を模すのは
/// 一覧に無い移動方法だけであり、それ以外で落ちると原因を取り違える。
fn raw_track_value(track: &TrackValue) -> String {
    let decimals = |value: &FiniteF64| format!("{:.*}", ITEM_VALUE_DECIMALS, value.get());
    let Some(mode) = track.mode.as_deref() else {
        return track.values.first().map(decimals).unwrap_or_default();
    };
    let mut fields: Vec<String> = track.values.iter().map(decimals).collect();
    fields.push(mode.to_string());
    let flags = u32::from(track.accelerate)
        | (u32::from(track.decelerate) << 1)
        | (u32::from(track.twopoint) << 2)
        | (u32::from(track.expression.is_some()) << 3)
        | track.reserved_flags;
    fields.push(flags.to_string());
    let mut raw = fields.join(",");
    if let Some(expression) = track.expression.as_deref() {
        raw.push('|');
        raw.push_str(expression);
    }
    raw.push('|');
    raw.push_str(
        &track
            .params
            .iter()
            .map(decimals)
            .collect::<Vec<String>>()
            .join(","),
    );
    raw
}

/// 移動を含む書き込みをホストが保持する形へ写す。
///
/// 値は数値と同じ規則で切り詰めと丸めを受ける。パラメータを渡さない書き込みには
/// 移動方法ごとの既定値が入る。一覧に無い移動方法では panic する（[`TRACK_MODES`]）。
fn host_write_track(track: &TrackValue) -> ItemValue {
    let mode = track.mode.as_deref().expect("移動を持つ値");
    assert!(
        TRACK_MODES.contains(&mode),
        "存在しない移動方法 {mode} が書き込まれました。実機ではプロセスが落ちます"
    );
    let adjust = |value: &FiniteF64| {
        FiniteF64::try_new(round_to_item_decimals(clamp_item_value(value.get()))).expect("有限値")
    };
    let params = match (track.params.is_empty(), mode == TRACK_DEFAULT_PARAM.0) {
        (true, true) => track_default_params(),
        (true, false) => Vec::new(),
        (false, _) => track.params.iter().map(adjust).collect(),
    };
    ItemValue::Track(TrackValue {
        values: track.values.iter().map(adjust).collect(),
        params,
        ..track.clone()
    })
}

/// ホストが保持する値に対応する移動情報。
///
/// **移動を持たない項目に移動情報は返らない。** SDK は移動が無いトラックバーの
/// 移動方法の名前を返さず、ラッパーはそれを「移動情報なし」へ畳む。
/// 値と移動情報が食い違う状態は実機に無いため、書き込みのたびに値から組み直す。
/// 食い違ったままにすると、移動の有無を移動情報から判定する実装も、値から判定
/// する実装も、どちらも同じように緑になる。
///
/// 所属グループはホストの報告であり、書き込みでは指定できない。書き換えの前後で
/// 変わらないものとして引き継ぐ。時間制御は移動方法の名前が決める。
///
/// **時間制御の変種では、保存値が持つパラメータを 0 件として報告する。** 保存も
/// 評価も書いた綴りで行われるのに、報告だけが空になる。報告の件数を照合の材料に
/// すれば、正しい綴りが 1 つ残らず失敗になる。
fn host_track(value: &ItemValue, before: Option<&TrackInfo>) -> Option<TrackInfo> {
    let ItemValue::Track(track) = value else {
        return None;
    };
    let mode = track.mode.clone()?;
    let timecontrol = mode.ends_with(TIME_CONTROL_SUFFIX);
    Some(TrackInfo {
        timecontrol,
        mode,
        params: match timecontrol {
            true => Vec::new(),
            false => track.params.clone(),
        },
        accelerate: track.accelerate,
        decelerate: track.decelerate,
        twopoint: track.twopoint,
        group_num: before.map_or(1, |track| track.group_num),
        group_index: before.map_or(0, |track| track.group_index),
        group_name: before.and_then(|track| track.group_name.clone()),
    })
}

/// ホストが色として受け付ける表記か。16 進 6 桁だけを受ける。
fn is_host_color(value: &str) -> bool {
    value.len() == 6 && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// 数値を設定項目の値域へ切り詰める。
fn clamp_item_value(value: f64) -> f64 {
    value.clamp(MIN_ITEM_VALUE as f64, MAX_ITEM_VALUE as f64)
}

/// 実数を設定項目の小数桁数へ丸める。
fn round_to_item_decimals(value: f64) -> f64 {
    format!("{value:.ITEM_VALUE_DECIMALS$}")
        .parse::<f64>()
        .expect("十進表記")
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
        selected_range: None,
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

/// 選択肢から選ぶ設定項目と、ホストが値を書き換える設定項目を並べて持つ effect。
///
/// 選択肢から選ぶ種別は 1 つに絞らない。ホストの挙動は種別によらず同じであり、
/// 1 種別だけを置くと種別ごとに経路が分かれても気付けない。
///
/// 色・フォント・実数・テキストを併せて持つ。ホストが値を書き換える形は種別
/// ごとに違い、1 種別だけでは書き込み後の照合を種別ごとに定めた意味が見えない。
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
                    value: DEFAULT_COLOR.to_string(),
                },
                track: None,
            },
            EffectItem {
                name: "フォント".to_string(),
                item_type: EffectItemType::Font,
                value: ItemValue::Font {
                    name: DEFAULT_FONT.to_string(),
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
pub(crate) fn shape_catalog_entry() -> FakeCatalogEntry {
    FakeCatalogEntry {
        name: SHAPE.to_string(),
        effect_type: EffectType::Filter,
        flags: EffectFlags::from_raw(1),
        items: item_definitions(shape(0).items),
        facets: HashMap::new(),
    }
}

/// [`shape`] の effect 名。
pub(crate) const SHAPE: &str = "図形";

/// 移動を持つ項目と持たない項目を並べたトラックバーの effect。
///
/// **移動の有無は同じ種別の中で分かれる。** 片方だけを置くと、移動の有無で
/// 判定する実装と種別で判定する実装を見分けられない。
///
/// `values` は移動を持つ項目の区間ごとの値である。個数は対象の区間の数で決まる
/// ため、付与する対象に合わせて渡す。
///
/// 既定の状態には含めない。移動を要する試験だけが
/// [`coordinate_catalog_entry`] と対にして差し込む。
pub(crate) fn coordinate(index: usize, values: &[f64]) -> HostEffect {
    let track = TrackValue {
        values: values
            .iter()
            .map(|value| FiniteF64::try_new(*value).expect("有限値"))
            .collect(),
        mode: Some(TRACK_MODES[0].to_string()),
        params: Vec::new(),
        accelerate: false,
        decelerate: false,
        twopoint: false,
        reserved_flags: 0,
        expression: None,
    };
    let moving = ItemValue::Track(track);
    HostEffect {
        name: COORDINATE.to_string(),
        index,
        enabled: true,
        locked: false,
        items: vec![
            EffectItem {
                name: MOVING_ITEM.to_string(),
                item_type: EffectItemType::Number,
                track: host_track(&moving, None),
                value: moving,
            },
            EffectItem {
                name: STATIC_ITEM.to_string(),
                item_type: EffectItemType::Number,
                value: ItemValue::Number {
                    value: FiniteF64::try_new(1.0).expect("有限値"),
                },
                track: None,
            },
        ],
    }
}

/// [`coordinate`] をカタログへ載せる形。
pub(crate) fn coordinate_catalog_entry() -> FakeCatalogEntry {
    FakeCatalogEntry {
        name: COORDINATE.to_string(),
        effect_type: EffectType::Filter,
        flags: EffectFlags::from_raw(1),
        items: item_definitions(coordinate(0, &[0.0, 1.0]).items),
        facets: HashMap::new(),
    }
}

/// [`coordinate`] の effect 名。
pub(crate) const COORDINATE: &str = "座標";

/// [`coordinate`] が持つ、移動を持つ設定項目の名前。
pub(crate) const MOVING_ITEM: &str = "X";

/// [`coordinate`] が持つ、移動を持たない設定項目の名前。
pub(crate) const STATIC_ITEM: &str = "拡大率";

/// 参照先を十進整数 1 個で指す設定項目を並べた effect。
///
/// レイヤー範囲とシーン参照を併せて持つ。どちらも値域も選択肢も持たず、値の形は
/// 整数 1 つだけである。**片方だけを置くと、2 種別が同じ経路を通ることを試験群が
/// 確かめられない。**
///
/// 既定の状態には含めない。この 2 種別を要する試験だけが
/// [`group_control_catalog_entry`] と対にして差し込む。
pub(crate) fn group_control(index: usize) -> HostEffect {
    HostEffect {
        name: GROUP_CONTROL.to_string(),
        index,
        enabled: true,
        locked: false,
        items: vec![
            EffectItem {
                name: LAYER_RANGE_ITEM.to_string(),
                item_type: EffectItemType::Range,
                value: ItemValue::Integer { value: 0 },
                track: None,
            },
            EffectItem {
                name: SCENE_ITEM.to_string(),
                item_type: EffectItemType::Scene,
                value: ItemValue::Integer { value: 0 },
                track: None,
            },
        ],
    }
}

/// [`group_control`] をカタログへ載せる形。
pub(crate) fn group_control_catalog_entry() -> FakeCatalogEntry {
    FakeCatalogEntry {
        name: GROUP_CONTROL.to_string(),
        effect_type: EffectType::Filter,
        flags: EffectFlags::from_raw(1),
        items: item_definitions(group_control(0).items),
        facets: HashMap::new(),
    }
}

/// [`group_control`] の effect 名。
pub(crate) const GROUP_CONTROL: &str = "グループ制御";

/// [`group_control`] が持つ、レイヤー範囲の設定項目の名前。
pub(crate) const LAYER_RANGE_ITEM: &str = "対象レイヤー数";

/// [`group_control`] が持つ、シーン参照の設定項目の名前。
pub(crate) const SCENE_ITEM: &str = "シーン";

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

/// フェイクのカタログ 1 件。
///
/// 見出しと設定項目の定義を対にして持つ。応答へ載る見出しは項目を持たないが、
/// 項目定義を引く経路は同じ effect について同じ一覧を返さなければならない。
/// 別々に持つと、カタログへ足した effect の項目が引けないフェイクを作れてしまう。
#[derive(Debug, Clone)]
pub(crate) struct FakeCatalogEntry {
    pub(crate) name: String,
    pub(crate) effect_type: EffectType,
    pub(crate) flags: EffectFlags,
    pub(crate) items: Vec<AvailableEffectItem>,
    /// 設定項目名から引く面の組。
    ///
    /// **ホストが受け付ける値とは別物である。** 候補も値域も読み取り経路へ出す
    /// ヒントであり、書き込みの可否を決めるのは [`host_write`] が写すホストの
    /// 挙動の側である。**既定は空である**——表を持たない環境がそのまま既定で
    /// あり、面が得られることを前提にした経路を作らない。
    pub(crate) facets: HashMap<String, ItemFacets>,
}

impl FakeCatalogEntry {
    /// 項目を読まずに得られる見出しへ写す。
    fn summary(&self) -> HostEffectSummary {
        HostEffectSummary {
            name: self.name.clone(),
            effect_type: self.effect_type.clone(),
            flags: self.flags,
        }
    }
}

/// 設定項目の現在値付きの一覧から、定義だけを取り出す。
fn item_definitions(items: Vec<EffectItem>) -> Vec<AvailableEffectItem> {
    items
        .into_iter()
        .map(|item| AvailableEffectItem {
            name: item.name,
            item_type: item.item_type,
        })
        .collect()
}

/// フェイクの effect カタログ。
pub(crate) fn fake_catalog() -> Vec<FakeCatalogEntry> {
    vec![
        FakeCatalogEntry {
            name: "ぼかし".to_string(),
            effect_type: EffectType::Filter,
            flags: EffectFlags::from_raw(1),
            items: vec![AvailableEffectItem {
                name: "範囲".to_string(),
                item_type: EffectItemType::Integer,
            }],
            facets: HashMap::new(),
        },
        FakeCatalogEntry {
            name: "動画ファイル".to_string(),
            effect_type: EffectType::Input,
            flags: EffectFlags::from_raw(3),
            items: Vec::new(),
            facets: HashMap::new(),
        },
        FakeCatalogEntry {
            name: "音声フェード".to_string(),
            effect_type: EffectType::Filter,
            // 音声だけを扱う。画像のフラグは立たない。
            flags: EffectFlags::from_raw(2),
            items: Vec::new(),
            facets: HashMap::new(),
        },
        FakeCatalogEntry {
            name: "標準描画".to_string(),
            effect_type: EffectType::Output,
            flags: EffectFlags::from_raw(1),
            items: Vec::new(),
            facets: HashMap::new(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use aviutl2_mcp_core::{TrackWriteTarget, prepare_item_write};

    /// テキスト種別の設定項目を 1 つだけ公開する一覧。
    fn text_item() -> Vec<AvailableEffectItem> {
        vec![AvailableEffectItem {
            name: "メモ".to_string(),
            item_type: EffectItemType::Text,
        }]
    }

    /// 登録済みフォントの一覧。
    fn fonts() -> Vec<String> {
        FONT_NAMES.iter().map(|name| name.to_string()).collect()
    }

    /// ホストへ書いた結果として読み取りへ返る生の文字列。
    fn written_back(item_type: &EffectItemType, value: &str, before: &ItemValue) -> String {
        match host_write(item_type, value, &fonts()) {
            Some(written) => raw_item_value(&written),
            // 受け付けない値では状態が変わらない。読み取りには変更前の値が返る。
            None => raw_item_value(before),
        }
    }

    #[test]
    fn the_fake_host_rewrites_the_values_the_real_one_rewrites() {
        // 実機で観測した書き換えを並べる。**ホストが素直だという前提を捨てる
        // ことが、この一覧の目的である。**
        let before_color = ItemValue::Color {
            value: "66ccff".to_string(),
        };
        let before_font = ItemValue::Font {
            name: DEFAULT_FONT.to_string(),
        };
        let before_number = ItemValue::Number {
            value: FiniteF64::try_new(1.0).expect("有限値"),
        };
        let cases = [
            // 書式の合わない色は、変更前の値ではなく白へ落ちる。
            (EffectItemType::Color, "#ff0000", &before_color, "ffffff"),
            (EffectItemType::Color, "f00", &before_color, "ffffff"),
            // 受理される色は小文字で返る。バイト比較では偽の不一致になる。
            (EffectItemType::Color, "FF8800", &before_color, "ff8800"),
            // 未登録のフォント名は黙殺され、変更前の値が残る。
            (
                EffectItemType::Font,
                "NoSuchFont12345",
                &before_font,
                DEFAULT_FONT,
            ),
            (
                EffectItemType::Font,
                FONT_NAMES[1],
                &before_font,
                FONT_NAMES[1],
            ),
            // 値域を外れた数値は両側とも切り詰められる。
            (EffectItemType::Number, "500", &before_number, "100.00"),
            (EffectItemType::Number, "-1", &before_number, "0.00"),
            // 桁の多い小数は項目の桁へ丸められる。
            (EffectItemType::Number, "1.2345", &before_number, "1.23"),
            // 受理される数値も、桁を整えた表記で返る。
            (EffectItemType::Number, "100", &before_number, "100.00"),
            (EffectItemType::Number, "12.5", &before_number, "12.50"),
            (EffectItemType::Integer, "500", &before_number, "100"),
            (EffectItemType::Integer, "-1", &before_number, "0"),
        ];
        for (item_type, value, before, expected) in cases {
            assert_eq!(
                written_back(&item_type, value, before),
                expected,
                "{item_type} へ {value} を書いた結果が違います"
            );
        }
    }

    #[test]
    fn the_fake_host_holds_a_movement_row_it_cannot_represent() {
        // ホストは壊れた移動行も保存し、読み取りへそのまま返す。数値へ倒すと、
        // 既に壊れている項目を読む場面をこの上に作れなくなる。
        let before = ItemValue::Number {
            value: FiniteF64::try_new(1.0).expect("有限値"),
        };
        for raw in ["-600.00,600.00,直線移動,8", "-600.00,600.00,直線移動,8|"] {
            assert_eq!(
                written_back(&EffectItemType::Number, raw, &before),
                raw,
                "{raw} が保たれません"
            );
        }
    }

    /// トラックバーの数値項目 1 つだけを公開する一覧。
    fn track_item() -> Vec<AvailableEffectItem> {
        vec![AvailableEffectItem {
            name: "X".to_string(),
            item_type: EffectItemType::Number,
        }]
    }

    /// 区間 1 個分の移動を持つ値。
    fn sample_track(mode: &str, params: &[f64]) -> ItemValue {
        let finite = |value: f64| FiniteF64::try_new(value).expect("有限値");
        ItemValue::Track(TrackValue {
            values: vec![finite(0.0), finite(100.0)],
            mode: Some(mode.to_string()),
            params: params.iter().copied().map(finite).collect(),
            accelerate: false,
            decelerate: false,
            twopoint: false,
            reserved_flags: 0,
            expression: None,
        })
    }

    /// フェイクが受け付ける移動方法。
    fn track_movements() -> Vec<Movement> {
        TRACK_MODES
            .iter()
            .map(|name| Movement {
                name: name.to_string(),
                writable: true,
            })
            .collect()
    }

    /// 移動を含まない値を渡すときの対象。
    fn no_track_target() -> TrackWriteTarget<'static> {
        TrackWriteTarget {
            section_count: 0,
            movements: &[],
        }
    }

    #[test]
    fn the_fake_host_evens_out_the_digits_of_a_movement() {
        // 実機は移動の値も項目の小数桁へ揃えて返す。生の文字列の比較では
        // 正しい書き込みが失敗になる。
        let before = ItemValue::Number {
            value: FiniteF64::try_new(1.0).expect("有限値"),
        };
        assert_eq!(
            written_back(&EffectItemType::Number, "0,100,直線移動,0", &before),
            "0.00,100.00,直線移動,0|"
        );
        // 移動の値も値域へ切り詰められる。
        assert_eq!(
            written_back(&EffectItemType::Number, "-1,500,直線移動,5", &before),
            "0.00,100.00,直線移動,5|"
        );
        // パラメータを渡さない書き込みには既定値が入る。
        assert_eq!(
            written_back(&EffectItemType::Number, "0,100,ランダム移動,0", &before),
            "0.00,100.00,ランダム移動,0|15.00"
        );
    }

    #[test]
    #[should_panic(expected = "存在しない移動方法")]
    fn the_fake_host_dies_on_a_mode_it_does_not_know() {
        // 実機は一覧に無い移動方法でプロセスごと落ちる。黙って無視される形で
        // 模すと、名前の検証を外しても検査が緑のまま通る。
        host_write(&EffectItemType::Number, "0,100,存在しない移動,0", &fonts());
    }

    #[test]
    fn a_movement_survives_the_write_and_the_read_back() {
        for (mode, params) in [("直線移動", &[][..]), ("ランダム移動", &[30.0][..])] {
            let value = sample_track(mode, params);
            let movements = track_movements();
            let target = TrackWriteTarget {
                section_count: 1,
                movements: &movements,
            };
            let write = prepare_item_write(&track_item(), "X", &value, target).expect("書き込み");
            let stored =
                host_write(&EffectItemType::Number, write.value(), &fonts()).expect("受理される");
            assert_eq!(
                write.read_back_matches(&raw_item_value(&stored)),
                Some(true),
                "{mode} の読み直しが要求と一致しません"
            );
        }
    }

    #[test]
    fn the_fake_host_drops_the_movement_when_a_scalar_is_written() {
        // **破棄しているのはホストである。** 移動を持つ項目へ数値を書くと、
        // 移動も加速も中間点無視も消え、失敗は返らない。符号化が値を落として
        // いるのではないため、書き込みを拒む規則が要る。
        let before = sample_track("直線移動", &[]);
        assert_eq!(
            written_back(&EffectItemType::Number, "0", &before),
            "0.00",
            "移動が残りました"
        );
        // 移動情報も同時に消える。値だけが変わる状態は実機に無い。
        let stored = host_write(&EffectItemType::Number, "0", &fonts()).expect("受理される");
        assert_eq!(
            host_track(&stored, host_track(&before, None).as_ref()),
            None
        );
    }

    #[test]
    fn the_movement_information_follows_the_value() {
        // 移動情報は値から組み直す。食い違ったままにすると、移動の有無を
        // 移動情報から判定する実装も値から判定する実装も同じように緑になる。
        let stored =
            host_write(&EffectItemType::Number, "0,100,曲線移動,5", &fonts()).expect("受理される");
        let track = host_track(&stored, None).expect("移動情報");
        assert_eq!(track.mode, "曲線移動");
        assert!(track.accelerate);
        assert!(track.twopoint);
        assert!(!track.decelerate);
        // 時間制御はフラグではなく移動方法の名前の変種が担う。
        assert!(!track.timecontrol);
        let controlled = host_write(
            &EffectItemType::Number,
            "0,100,直線移動(時間制御),0",
            &fonts(),
        )
        .expect("受理される");
        assert!(host_track(&controlled, None).expect("移動情報").timecontrol);
    }

    #[test]
    fn a_rejected_choice_value_leaves_the_previous_one() {
        let before = ItemValue::Choice {
            value: CHOICE_VALUES[0].to_string(),
        };
        assert_eq!(
            written_back(&EffectItemType::Select, "存在しない形", &before),
            CHOICE_VALUES[0]
        );
        assert_eq!(
            written_back(&EffectItemType::Select, CHOICE_VALUES[1], &before),
            CHOICE_VALUES[1]
        );
    }

    #[test]
    fn the_fake_host_wraps_backslashes_and_line_feeds_like_the_real_one() {
        // 書いた文字列を解いて保持し、読み取りへは包み直して返す。
        let written =
            host_write(&EffectItemType::Text, r"C:\\temp\nの先", &fonts()).expect("受理される");
        assert_eq!(
            written,
            ItemValue::Text {
                value: "C:\\temp\nの先".to_string(),
            }
        );
        assert_eq!(raw_item_value(&written), r"C:\\temp\nの先");
    }

    #[test]
    fn the_fake_host_reports_back_exactly_what_the_write_path_handed_it() {
        // 書き込みが渡す文字列と、読み直しが返す文字列が一致する。ここが一致
        // しない限り、テキスト種別は書き込み後の照合を課せない。
        for value in [
            r"C:\temp\note",
            "1 行目\n2 行目",
            "\t字下げ",
            r"^\d+\.txt$",
            "字幕",
        ] {
            let write = prepare_item_write(
                &text_item(),
                "メモ",
                &ItemValue::Text {
                    value: value.to_string(),
                },
                no_track_target(),
            )
            .expect("書き込み");
            let stored =
                host_write(&EffectItemType::Text, write.value(), &fonts()).expect("受理される");
            assert_eq!(
                stored,
                ItemValue::Text {
                    value: value.to_string(),
                },
                "{value:?} がホストの保持で崩れました"
            );
            assert_eq!(
                write.read_back_matches(&raw_item_value(&stored)),
                Some(true),
                "{value:?} の読み直しが渡した文字列と一致しません"
            );
        }
    }
}
