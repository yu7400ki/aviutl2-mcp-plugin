//! SDK 側の編集経路を表す境界。
//!
//! [`EditHost`] は編集ハンドルが提供する操作、[`SceneEditor`] は編集区間の内側で
//! 行える操作を表す。opaque handle はどちらの境界にも現れない。
//!
//! 変更 API が受け取る対象は [`ResolvedObject`] / [`ResolvedEffect`] だけであり、
//! レイヤー番号と開始フレームでは指せない。座標で渡すと、実装が変更 API を呼ぶ
//! 直前にもう一度自前で対象を探し直すことになり、fingerprint を照合した対象と
//! 実際に変更される対象が別になり得る。
//!
//! 変更 API はさらに [`MutationTicket`] を要求する。この型は前提条件の照合と、
//! 変更直前の epoch / revision 再検証を通らなければ得られず、取得の時点で変更の
//! 発行が記録される。照合を飛ばして変更へ進む経路も、記録を伴わずに発行する
//! 経路も、型として存在しない。

use crate::edit::error::EditError;
use crate::edit::precondition::MutationTicket;
use crate::edit::resolve::{ResolvedEffect, ResolvedObject};
use crate::read::host::{EditState, HostEditInfo, HostObject, SceneReader};
use aviutl2_mcp_core::{AvailableEffect, AvailableEffectItem, Cursor, FrameRange};

/// 編集区間の内側で対象オブジェクトを指す内部識別子。
///
/// これ自体では変更 API を呼べない。変更 API が受け取るのは
/// [`ResolvedObject`] であり、その生成は fingerprint の照合を済ませた解決処理に
/// 限られる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectSlot(pub(crate) usize);

/// 編集区間の内側で対象 effect を指す内部識別子。
///
/// 位置づけは [`ObjectSlot`] と同じ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectSlot(pub(crate) usize);

/// 編集の区間を抜けたあとに観測した選択状態。
///
/// カーソル・選択範囲・フォーカスの反映値は編集情報にしか現れず、編集情報は
/// 区間へ入った時点の複製である。加えてフォーカスは区間の処理が終わってから
/// 適用される。したがって反映値は区間を抜けてから読むほかない。
#[derive(Debug, Clone, PartialEq)]
pub struct HostSelection {
    /// 現在シーンの ID。
    pub scene_id: i32,
    /// 編集カーソルの位置。
    pub cursor: Cursor,
    /// フレーム範囲選択。未選択は `None`。
    pub selected_range: Option<FrameRange>,
    /// フォーカス対象。未選択は `None`。
    pub focus: Option<HostObject>,
}

/// 編集区間の内側で行える操作。
///
/// 対象の探索は行わない。与えられたトークンに対して SDK の変更 API を呼ぶことと、
/// 読み取り口を渡すことに責務を限る。
pub trait SceneEditor {
    /// 編集区間の内側での読み取り。
    ///
    /// 解決・read-back・応答の組み立てを同一区間内で完結できる。
    fn reader(&self) -> &dyn SceneReader;

    /// 区間へ入った時点の編集情報。
    ///
    /// シーンの guard はこの値と照合する。区間の外で取った編集情報を持ち込むと、
    /// 取得から区間開始までの間にシーンが切り替わった場合を検出できない。
    ///
    /// 区間内で変更を適用した後は古くなる。反映値の確認には使えない。
    fn entry_edit_info(&self) -> &HostEditInfo;

    /// 開始フレームが完全一致する対象を捕捉する。
    ///
    /// 捕捉するだけであり、前提条件は判定しない。判定は解決処理が行う。
    fn bind_object(&self, layer: usize, frame_start: usize) -> Result<ObjectSlot, EditError>;

    /// 対象オブジェクトの effect 列から、列全体での位置で 1 件を捕捉する。
    fn bind_effect(&self, object: ObjectSlot, position: usize) -> Result<EffectSlot, EditError>;

    /// 対象 effect が公開している設定項目の一覧。
    ///
    /// 設定項目名の実在確認と種別の照合に使う。
    fn effect_items(
        &self,
        effect: &ResolvedEffect<'_>,
    ) -> Result<Vec<AvailableEffectItem>, EditError>;

    /// SDK が拡張子の上でメディアファイルに対応しているか。
    ///
    /// ファイルを開かない確認に限る。編集区間はホストのメインスレッド上で走り、
    /// 割り込む手段が無いため、区間の内側でファイル I/O を行うとその間ホストの
    /// 操作が止まる。実際に読めるかどうかは作成そのものの成否に委ねる。
    fn supports_media_file(&self, path: &str) -> Result<bool, EditError>;

    /// alias からオブジェクトを作成する。
    ///
    /// 作成された対象は返さない。SDK は複数オブジェクトを含む alias でも先頭
    /// しか返さず、長さと挿入位置はホストが決める。作成された全件は呼び出し側が
    /// 作成前後のレイヤー列挙の差分から特定する。
    fn create_object_from_alias(
        &self,
        ticket: MutationTicket<'_>,
        alias: &str,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError>;

    /// メディアファイルからオブジェクトを作成する。
    ///
    /// 作成された対象を返さない理由は [`Self::create_object_from_alias`] と同じ。
    fn create_object_from_media_file(
        &self,
        ticket: MutationTicket<'_>,
        path: &str,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError>;

    /// オブジェクトのレイヤーと開始フレームを変更する。
    fn move_object(
        &self,
        ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError>;

    /// オブジェクトを削除する。
    fn delete_object(
        &self,
        ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
    ) -> Result<(), EditError>;

    /// オブジェクト名を設定する。`None` で標準名へ戻す。
    fn set_object_name(
        &self,
        ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        name: Option<&str>,
    ) -> Result<(), EditError>;

    /// オブジェクトへ effect を付与する。
    ///
    /// 付与位置は返さない。SDK が位置を伝えないため、呼び出し側が付与前後の
    /// 名前の列の差分から確定する。
    fn create_effect(
        &self,
        ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        effect_name: &str,
    ) -> Result<(), EditError>;

    /// オブジェクトから effect を削除する。
    fn delete_effect(
        &self,
        ticket: MutationTicket<'_>,
        object: &ResolvedObject<'_>,
        effect: &ResolvedEffect<'_>,
    ) -> Result<(), EditError>;

    /// effect の有効・無効を設定する。
    fn set_effect_enabled(
        &self,
        ticket: MutationTicket<'_>,
        effect: &ResolvedEffect<'_>,
        enabled: bool,
    ) -> Result<(), EditError>;

    /// effect のロック状態を設定する。
    fn set_effect_locked(
        &self,
        ticket: MutationTicket<'_>,
        effect: &ResolvedEffect<'_>,
        locked: bool,
    ) -> Result<(), EditError>;

    /// effect の設定項目へ値を書き込む。
    fn set_effect_item(
        &self,
        ticket: MutationTicket<'_>,
        effect: &ResolvedEffect<'_>,
        item: &str,
        value: &str,
    ) -> Result<(), EditError>;

    /// 編集カーソルの位置を設定する。ホストが範囲外の値をクランプする。
    fn set_cursor(
        &self,
        ticket: MutationTicket<'_>,
        layer: usize,
        frame: usize,
    ) -> Result<(), EditError>;

    /// フレーム範囲選択を設定する。`None` で解除する。
    fn set_select_range(
        &self,
        ticket: MutationTicket<'_>,
        range: Option<FrameRange>,
    ) -> Result<(), EditError>;

    /// フォーカス対象を設定する。`None` で解除する。
    fn set_focus_object(
        &self,
        ticket: MutationTicket<'_>,
        object: Option<&ResolvedObject<'_>>,
    ) -> Result<(), EditError>;
}

/// 編集ハンドルが提供する編集経路。
///
/// [`Self::enter_edit_section`] は与えられたクロージャを SDK の編集区間の内側で
/// 1 度だけ呼ぶ。クロージャの panic を捕捉するのは呼び出し側の責務であり、
/// 実装はクロージャをそのまま編集区間へ渡す。
pub trait EditHost: Send + Sync {
    /// 編集 API を呼び出せる状態か。
    ///
    /// これが偽の間、他のメソッドを呼んではならない。
    fn is_ready(&self) -> bool;

    /// 現在の編集状態。
    fn edit_state(&self) -> Result<EditState, EditError>;

    /// 登録済み effect のカタログ。編集区間を必要としない。
    fn effect_catalog(&self) -> Result<Vec<AvailableEffect>, EditError>;

    /// 編集区間を抜けたあとに観測する選択状態。
    fn observed_selection(&self) -> Result<HostSelection, EditError>;

    /// 編集区間へ 1 度だけ入り、クロージャの結果を持ち出す。
    ///
    /// 1 要求につき 1 度だけ呼ぶ。SDK は 1 回の区間で行った変更をまとめて 1 つの
    /// 取り消し単位として登録するため、複数回に分けると利用者が 1 回の取り消しで
    /// 戻せない状態が生じる。
    fn enter_edit_section<T, F>(&self, f: F) -> Result<T, EditError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn SceneEditor) -> T + Send;
}
