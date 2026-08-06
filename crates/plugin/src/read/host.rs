//! SDK 側の読み取り経路を表す境界。
//!
//! [`ReadHost`] は編集ハンドルが提供する操作、[`SceneReader`] と
//! [`SceneValueReader`] は参照区間の内側で参照できるプロジェクトデータを表す。
//! opaque handle はどの境界にも現れず、対象は「レイヤー番号と開始フレーム」で
//! 指し示す。参照区間の内側でハンドルを再解決するのは実装側の責務である。

use crate::read::error::ReadError;
use aviutl2_mcp_core::{
    EffectFlags, EffectItem, EffectType, FiniteF64, GridBpm, ModuleEntry, Rgba, SectionRange,
};
use std::fmt;

/// ホストの編集状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditState {
    /// 編集中。読み取りできる。
    Edit,
    /// プレビュー再生中。
    Preview,
    /// ファイル出力中。
    Save,
}

impl EditState {
    /// 応答の補助情報へ載せる機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            EditState::Edit => "edit",
            EditState::Preview => "preview",
            EditState::Save => "save",
        }
    }
}

impl fmt::Display for EditState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            EditState::Edit => "編集中",
            EditState::Preview => "プレビュー再生中",
            EditState::Save => "ファイル出力中",
        };
        f.write_str(text)
    }
}

/// 参照区間の外で取得する編集情報。
///
/// フレームレートは約分された分子・分母として得られる。ホストが保持する生の
/// rate/scale はラッパーの時点で有理数へ畳まれており、復元できない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEditInfo {
    /// 現在シーンの ID。
    pub scene_id: i32,
    /// 画像の横幅。
    pub width: u32,
    /// 画像の高さ。
    pub height: u32,
    /// フレームレートの分子。
    pub fps_rate: i32,
    /// フレームレートの分母。
    pub fps_scale: i32,
    /// 音声のサンプリングレート。
    pub sample_rate: u32,
    /// 編集カーソルのフレーム番号。
    pub cursor_frame: usize,
    /// 編集カーソルのレイヤー番号。
    pub cursor_layer: usize,
    /// オブジェクトが存在する最大フレーム番号。
    pub frame_max: usize,
    /// オブジェクトが存在する最大レイヤー番号。
    pub layer_max: usize,
    /// タイムラインの表示開始フレーム番号。
    pub display_frame_start: usize,
    /// タイムラインの表示開始レイヤー番号。
    pub display_layer_start: usize,
    /// タイムラインの表示フレーム数。
    pub display_frame_num: usize,
    /// タイムラインの表示レイヤー数。
    pub display_layer_num: usize,
    /// フレーム範囲選択の開始フレーム番号。未選択は `None`。
    pub select_range_start: Option<usize>,
    /// フレーム範囲選択の終了フレーム番号。未選択は `None`。
    pub select_range_end: Option<usize>,
}

/// レイヤーの属性。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLayer {
    /// レイヤー名。無名は `None`。
    pub name: Option<String>,
    /// 表示が有効か。
    pub enabled: bool,
    /// ロックされているか。
    pub locked: bool,
}

/// オブジェクトの位置と名前。
///
/// 対象の絞り込みと並び順の決定に必要な最小限の材料であり、alias も effect も
/// 含まない。走査のたびにレイヤー内の全オブジェクトの alias と effect を読むと、
/// 参照区間の保持時間がプロジェクトの規模に比例して伸び、無関係な対象の
/// 読み取り失敗が走査全体を巻き込む。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostObjectPlacement {
    /// 0 始まりのレイヤー番号。
    pub layer: usize,
    /// 0 始まりの開始フレーム番号。
    pub frame_start: usize,
    /// 0 始まりの終了フレーム番号。
    pub frame_end: usize,
    /// オブジェクト名。標準名のままなら `None`。
    pub name: Option<String>,
}

/// オブジェクトの位置と同一性の材料。
///
/// alias は配下 effect の設定値を含むため、オブジェクトの fingerprint はこの型
/// だけから算出できる。effect を読まずに同一性を確定できることが、列挙で
/// effect を読まない根拠である。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostObject {
    /// 位置と名前。
    pub placement: HostObjectPlacement,
    /// 正規化前の alias。
    pub alias: String,
}

/// オブジェクトの詳細。
#[derive(Debug, Clone, PartialEq)]
pub struct HostObjectDetail {
    /// 位置と同一性の材料。
    pub object: HostObject,
    /// 付与された effect を、付与された順に並べた列。
    pub effects: Vec<HostEffect>,
    /// 中間点で区切られた区間。
    pub sections: Vec<SectionRange>,
}

/// オブジェクトに付与された effect。
#[derive(Debug, Clone, PartialEq)]
pub struct HostEffect {
    /// effect 名。
    pub name: String,
    /// 同名 effect のうち何番目か。0 始まり。
    pub index: usize,
    /// effect が有効か。
    pub enabled: bool,
    /// effect がロックされているか。
    pub locked: bool,
    /// 設定項目と値。
    pub items: Vec<EffectItem>,
}

/// 参照区間の内側で対象を解決するために参照するプロジェクトデータ。
///
/// 返す値は全て所有型であり、参照区間を抜けた後も利用できる。
///
/// **ここに置くのは「対象を解決するために読む」ものだけである。** セレクターの
/// 突き合わせ・fingerprint の照合・ガードの判定に要る読み取りは、読み取り
/// operation と編集 operation の双方が通る。応答へ載せる値を読むだけのものは
/// [`SceneValueReader`] が持つ。
///
/// **割れているのは実装の負担が役目に比例するようにするためである。** 編集区間
/// の内側だけを表す実装——[`crate::edit::host::SceneEditor::reader`] が渡す
/// 相手——はこの trait だけを備えればよく、読み取り tool が増えても到達不能な
/// メソッドが積まない。**参照区間も兼ねる実装は両方を備える**——[`ReadHost`] が
/// [`SceneValueReader`] を渡す以上、同じ型が両方の口になる場合がある。
pub trait SceneReader {
    /// 現在シーンの名前。取得できない場合は `None`。
    fn scene_name(&self) -> Option<String>;

    /// グリッドの BPM 一覧。
    ///
    /// 4 つのフィールドを揃えて返す。一部だけを返すと、読み取った一覧をそのまま
    /// 書き戻す経路で残りが失われる。
    fn grid_bpm(&self) -> Result<Vec<GridBpm>, ReadError>;

    /// レイヤーの属性。
    ///
    /// 名前・表示・ロックを揃って必要とする経路だけが使う。ロックだけを見る
    /// 経路は [`Self::layer_locked`] を使い、応答に現れない属性を読まない。
    fn layer(&self, layer: usize) -> Result<HostLayer, ReadError>;

    /// レイヤーがロックされているか。
    ///
    /// **実装の義務**: [`Self::layer`] が同じレイヤーに対して返す `locked` と
    /// 一致させる。
    fn layer_locked(&self, layer: usize) -> Result<bool, ReadError>;

    /// レイヤー内のオブジェクト数。
    ///
    /// 件数だけを必要とする経路が名前や alias を読まずに済むよう、列挙とは
    /// 別のメソッドにしてある。
    ///
    /// **一定回数の呼び出しでは求まらない。** ホストは件数を直接返さないため、
    /// 実装はレイヤーを走査して数える。費用はレイヤー内のオブジェクト数に
    /// 比例するので、件数を要しない経路では呼ばない。
    fn object_count(&self, layer: usize) -> Result<usize, ReadError>;

    /// レイヤー内のオブジェクトの位置と名前を開始フレームの昇順で全件返す。
    ///
    /// alias も effect も読まない。対象の絞り込みと並び順の決定はこの結果だけで
    /// 行い、同一性の材料は対象が確定してから [`Self::object_identity`] で読む。
    ///
    /// 途中で走査を打ち切った不完全な一覧は返さない。全件を返せない場合は失敗する。
    fn object_placements(&self, layer: usize) -> Result<Vec<HostObjectPlacement>, ReadError>;

    /// 開始フレームが完全一致するオブジェクトの同一性の材料を返す。
    ///
    /// alias を読み、配下 effect は読まない。fingerprint はこの結果か
    /// [`Self::object_detail`] が含む同じ型からだけ算出する。
    ///
    /// **実装の義務**: 同じ対象に対して [`Self::object_detail`] と同じ
    /// [`HostObject`] を返す。両者が別の材料を読めば、一覧が返した fingerprint と
    /// 詳細が返した fingerprint が食い違い、一覧の selector で詳細を引けなくなる。
    /// 一致は型では強制されないため、片方を変えるときは必ず両方を見る。
    ///
    /// 一致する対象が無い場合は [`ReadError::ObjectNotFound`] を返す。
    fn object_identity(&self, layer: usize, frame_start: usize) -> Result<HostObject, ReadError>;

    /// 開始フレームが完全一致するオブジェクトの詳細を返す。
    ///
    /// 同一性の材料に加えて配下 effect と中間点の区間を読む。effect の一覧を
    /// 必要とする経路だけがここを通る。
    ///
    /// **実装の義務**: 含める [`HostObject`] は [`Self::object_identity`] が同じ
    /// 対象に対して返すものと一致させる。
    ///
    /// 一致する対象が無い場合は [`ReadError::ObjectNotFound`] を返す。
    fn object_detail(
        &self,
        layer: usize,
        frame_start: usize,
    ) -> Result<HostObjectDetail, ReadError>;
}

/// 応答へ載せる値を読む境界。
///
/// **[`SceneReader`] から分けてあるのは、対象の解決にもガードの判定にも要らない
/// ためである。** [`crate::edit::host::SceneEditor::reader`] が渡す相手にこれらを
/// 備えさせれば、読み取り tool を 1 つ足すたびに到達不能な委譲が 1 つ積む。
///
/// **編集 operation から呼ばれないという意味ではない。** [`Self::focused_object`]
/// は `set_selection` が観測を組み立てる経路が、編集区間を抜けた後に改めて参照
/// 区間へ入って呼ぶ。呼ぶ相手は [`ReadHost`] が渡す実装であって、編集区間の
/// 読み取り口ではない。
///
/// 実装が [`SceneReader`] も兼ねるのは、値を読む前に対象を解決する必要がある
/// ためである。読み取り operation は 1 つの参照区間の内側で解決と読み取りを
/// 続けて行うので、両方を同じ相手へ問える。
pub trait SceneValueReader: SceneReader {
    /// 登録済みパレット名を全件返す。
    ///
    /// 列挙そのものは参照区間を要しないが、ここへ置くことで名前と色を同じ区間の
    /// 内側で読める。分ければ、名前を集めてから色を読むまでの間にパレットが
    /// 差し替わり、食い違った組を返し得る。
    fn palette_names(&self) -> Result<Vec<String>, ReadError>;

    /// 現在のパレット名。取得できない場合は `None`。
    ///
    /// ラベル付きの場合は `[ラベル名.パレット名]` の形式で返る。分解しない。
    ///
    /// 一覧に対する付随情報であり、取れないことは一覧の失敗ではない。
    fn current_palette_name(&self) -> Option<String>;

    /// パレットの色を返す。情報を取得できない名前は `None`。
    ///
    /// 件数は常に [`aviutl2_mcp_core::PALETTE_COLOR_COUNT`] である。
    ///
    /// **`None` は失敗ではない。** 列挙が返した名前で情報が取れないのは異常だが、
    /// その 1 件のために一覧全体を落とさない。呼び出し側は該当の名前を一覧から
    /// 落とす。
    fn palette_colors(&self, name: &str) -> Option<Vec<Rgba>>;

    /// タイムライン上で選択されているオブジェクトの位置と名前を返す。
    ///
    /// alias も effect も読まない。位置だけを先に集めることで、並べ替えと
    /// ページの切り出しを済ませてから、応答へ載せる分の同一性の材料だけを
    /// [`SceneReader::object_identity`] で読める。
    ///
    /// **並び順を保証しない。** ホストが返す順序は規定されておらず、要求ごとに
    /// 変わり得る。並べ替えは呼び出し側が行う。
    fn selected_placements(&self) -> Result<Vec<HostObjectPlacement>, ReadError>;

    /// オブジェクト設定ウィンドウで選択されているオブジェクトを返す。未選択は `None`。
    ///
    /// タイムライン上の選択とは別の概念であり、[`Self::selected_placements`] の
    /// 結果に含まれるとは限らない。
    ///
    /// 1 件しか無くページの切り出しも掛からないため、位置と同一性の材料を分けて
    /// 読まない。
    fn focused_object(&self) -> Result<Option<HostObject>, ReadError>;

    /// フォーカス対象の区間番号を返す。ホストが番号を持たなければ `None`。
    ///
    /// **[`Self::focused_object`] との整合を保証しない。** ホストは対象を返さない
    /// まま番号だけを返し得る。ここが返すのはホストが名乗った値そのものであり、
    /// 実装は転送するだけである。
    ///
    /// **呼び出し側の責務**: 番号は対象の性質であるため、[`Self::focused_object`]
    /// と突き合わせ、対象が無ければ番号も落とす。対象と番号が食い違った組を
    /// 応答へ載せない。
    fn focus_section(&self) -> Result<Option<usize>, ReadError>;

    /// トラックバー項目を、指定フレームで評価した値を返す。
    ///
    /// `effect_position` は [`SceneReader::object_detail`] が返す effect 列での
    /// 0 始まりの位置である。**フレームは小数部を保ったまま渡す。** 小数部は
    /// フレーム間の位置を指しており、丸めると中間点の間の値を問えなくなる。
    ///
    /// **項目をまとめて受け取る。** 対象の解決は 1 度で済み、項目ごとに effect を
    /// 引き直さない。
    ///
    /// **実装の義務**: 戻り値の外側は `item_names` と、内側は `frames` と、
    /// それぞれ同じ長さ・同じ順序にする。要求元は位置で対応付けるため、並びが
    /// 崩れると別の項目や別のフレームの値を読むことになる。
    ///
    /// 値を得られなかった場合は [`ReadError::TrackValueUnavailable`] を返す。
    /// 項目の存在と種別、フレームの範囲は呼び出し側が確かめてからここへ来る。
    fn effect_track_values(
        &self,
        layer: usize,
        frame_start: usize,
        effect_position: usize,
        item_names: &[&str],
        frames: &[f64],
    ) -> Result<Vec<Vec<FiniteF64>>, ReadError>;

    /// チェックボックス項目を、指定フレームで評価した値を返す。
    ///
    /// **フレームは整数である。** 区間ごとのチェックボックスはフレーム間の位置を
    /// 持たない。
    ///
    /// 義務と失敗の扱いは [`Self::effect_track_values`] と同じである。
    fn effect_check_values(
        &self,
        layer: usize,
        frame_start: usize,
        effect_position: usize,
        item_names: &[&str],
        frames: &[usize],
    ) -> Result<Vec<Vec<bool>>, ReadError>;

    /// トラックバーグループの所属アイテム名を返す。
    ///
    /// effect をハンドルではなく名前と同名内の位置で指す。この取得だけは
    /// ハンドルを取る口が無い。
    ///
    /// **0 件は失敗ではない。** 指定したグループが無い場合の戻り値であり、
    /// 呼び出しの失敗とは区別される。
    fn track_group_item_names(
        &self,
        layer: usize,
        frame_start: usize,
        effect_name: &str,
        effect_index: usize,
        group_name: &str,
    ) -> Result<Vec<String>, ReadError>;
}

/// 登録済み effect 1 件の見出し。
///
/// effect 名の列挙だけで得られる値をまとめたものであり、設定項目を 1 つも
/// 含まない。項目の列挙は effect ごとに別の呼び出しを要するため、見出しを
/// 集める段では踏み込まない。
#[derive(Debug, Clone, PartialEq)]
pub struct HostEffectSummary {
    /// effect 名。
    pub name: String,
    /// effect の種別。
    pub effect_type: EffectType,
    /// 対応内容を表すフラグ。
    pub flags: EffectFlags,
}

/// 編集ハンドルが提供する読み取り経路。
///
/// [`Self::enter_read_section`] は与えられたクロージャを SDK の参照区間の内側で
/// 1 度だけ呼ぶ。クロージャの panic を捕捉するのは呼び出し側の責務であり、
/// 実装はクロージャをそのまま参照区間へ渡す。
pub trait ReadHost: Send + Sync {
    /// 読み取り API を呼び出せる状態か。
    ///
    /// これが偽の間、他のメソッドを呼んではならない。
    fn is_ready(&self) -> bool;

    /// 現在の編集状態。
    fn edit_state(&self) -> Result<EditState, ReadError>;

    /// 参照区間の外で取得する編集情報。
    fn edit_info(&self) -> Result<HostEditInfo, ReadError>;

    /// 登録済み effect の見出しを全件返す。参照区間を必要としない。
    ///
    /// **設定項目は読まない。** 名前・種別・フラグは effect 名の列挙だけで
    /// 得られる。項目を要する値は [`Self::effect_item_count`] が別に返す。
    fn effect_catalog(&self) -> Result<Vec<HostEffectSummary>, ReadError>;

    /// effect 1 件の設定項目数を返す。参照区間を必要としない。
    ///
    /// **effect ごとに列挙を 1 度行う呼び出しである。** 全件について呼ぶと費用が
    /// 登録数で決まるため、呼び出し側は応答へ載せる分に限る。
    fn effect_item_count(&self, effect_name: &str) -> Result<usize, ReadError>;

    /// 登録済みフォント名を全件返す。参照区間を必要としない。
    ///
    /// **列挙を打ち切れない。** ホストの列挙は途中で止める手段を持たないため、
    /// 1 ページを返す要求でも毎回全件が返る。
    fn font_names(&self) -> Result<Vec<String>, ReadError>;

    /// 登録済みモジュールを全件返す。参照区間を必要としない。
    ///
    /// **既知の種別だけが返る。** 種別値の解釈はより低い層が行い、解釈できない
    /// 値を持つ項目はそこで落ちる。欠落し得ることは tool の説明が述べる。
    fn modules(&self) -> Result<Vec<ModuleEntry>, ReadError>;

    /// 参照区間へ 1 度だけ入り、クロージャの結果を持ち出す。
    ///
    /// クロージャが受け取るのは [`SceneValueReader`] である。この境界を通るのは
    /// 読み取り operation だけであり、解決だけを要する呼び出しは上位トレイトへ
    /// 落として使える。
    fn enter_read_section<T, F>(&self, f: F) -> Result<T, ReadError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn SceneValueReader) -> T + Send;
}
