//! SDK 側の読み取り経路を表す境界。
//!
//! [`ReadHost`] は編集ハンドルが提供する操作、[`SceneReader`] は参照区間の内側で
//! 参照できるプロジェクトデータを表す。opaque handle はどちらの境界にも現れず、
//! 対象は「レイヤー番号と開始フレーム」で指し示す。参照区間の内側でハンドルを
//! 再解決するのは実装側の責務である。

use crate::read::error::ReadError;
use aviutl2_mcp_core::{AvailableEffect, EffectItem, FiniteF64, SectionRange};
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

/// 参照区間の内側で参照できるプロジェクトデータ。
///
/// 返す値は全て所有型であり、参照区間を抜けた後も利用できる。
pub trait SceneReader {
    /// 現在シーンの名前。取得できない場合は `None`。
    fn scene_name(&self) -> Option<String>;

    /// グリッドの BPM 一覧。
    fn grid_bpm(&self) -> Result<Vec<FiniteF64>, ReadError>;

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

    /// 登録済み effect のカタログ。参照区間を必要としない。
    fn effect_catalog(&self) -> Result<Vec<AvailableEffect>, ReadError>;

    /// 参照区間へ 1 度だけ入り、クロージャの結果を持ち出す。
    fn enter_read_section<T, F>(&self, f: F) -> Result<T, ReadError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn SceneReader) -> T + Send;
}
