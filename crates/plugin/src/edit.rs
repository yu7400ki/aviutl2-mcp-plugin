//! SDK の編集 API を所有型 DTO へ写す層。
//!
//! 要求処理側は [`EditAdapter`] だけを見て編集を発行し、SDK の型・ハンドル・
//! 編集区間には一切触れない。SDK 呼び出しは [`host::EditHost`] の実装に閉じ、
//! 差し替えることで SDK 無しでも解決・前提条件・read-back の全手順を検証できる。
//!
//! 1 要求は 1 回の編集区間で完結し、SDK 上では 1 つの取り消し単位になる。

pub mod adapter;
pub(crate) mod batch;
pub mod error;
#[cfg(test)]
pub(crate) mod fake;
pub mod host;
pub mod precondition;
pub mod resolve;
pub mod sdk;

use crate::project::ProjectState;
use aviutl2_mcp_core::{
    AddEffectParams, ApplyBatchParams, BatchOutcome, CreateObjectParams, CreateObjectSectionParams,
    DeleteEffectParams, DeleteObjectParams, DeleteObjectSectionParams, EditOutcome, GridBpmOutcome,
    LayerStateOutcome, MoveObjectParams, MoveObjectSectionParams, ObjectSectionsOutcome,
    SceneSettingsOutcome, SelectionState, SetEffectEnabledParams, SetGridBpmParams,
    SetLayerStateParams, SetObjectItemParams, SetObjectNameParams, SetSceneSettingsParams,
    SetSelectionParams,
};
use std::sync::Arc;

pub use adapter::HostEditAdapter;
pub use error::{EditError, SectionPreconditionReason, UnsupportedReason};

/// 編集 operation の実行口。
///
/// 各メソッドは 1 度の呼び出しで完結し、SDK の編集区間を跨いで状態を保持しない。
/// 戻り値は所有型のみで、opaque handle を公開しない。
///
/// 応答は変更後の対象を selector と fingerprint ごと返すため、要求元は読み直さず
/// に次の編集を組み立てられる。
pub trait EditAdapter: Send + Sync {
    /// メディアファイルまたは alias からオブジェクトを作成する。
    fn create_object(&self, params: &CreateObjectParams) -> Result<EditOutcome, EditError>;

    /// オブジェクトのレイヤーと開始フレームを変更する。
    fn move_object(&self, params: &MoveObjectParams) -> Result<EditOutcome, EditError>;

    /// オブジェクトを削除する。
    fn delete_object(&self, params: &DeleteObjectParams) -> Result<EditOutcome, EditError>;

    /// オブジェクト名を変更する。
    fn set_object_name(&self, params: &SetObjectNameParams) -> Result<EditOutcome, EditError>;

    /// 設定項目・トラックバーの値を変更する。
    fn set_object_item(&self, params: &SetObjectItemParams) -> Result<EditOutcome, EditError>;

    /// オブジェクトへ effect を付与する。
    fn add_effect(&self, params: &AddEffectParams) -> Result<EditOutcome, EditError>;

    /// オブジェクトから effect を削除する。
    fn delete_effect(&self, params: &DeleteEffectParams) -> Result<EditOutcome, EditError>;

    /// effect の有効・無効を変更する。
    fn set_effect_enabled(&self, params: &SetEffectEnabledParams)
    -> Result<EditOutcome, EditError>;

    /// オブジェクトへ中間点を追加する。
    ///
    /// **対象のレイヤーがロックされている場合は [`EditError::LayerLocked`] になる。**
    /// ロックが止める範囲を決めるのはホストであり、中間点の編集もそこに含まれる。
    /// 中間点を変える以下の 2 つも同じ扱いになる。
    fn create_object_section(
        &self,
        params: &CreateObjectSectionParams,
    ) -> Result<ObjectSectionsOutcome, EditError>;

    /// オブジェクトの中間点を削除する。
    fn delete_object_section(
        &self,
        params: &DeleteObjectSectionParams,
    ) -> Result<ObjectSectionsOutcome, EditError>;

    /// オブジェクトの中間点を移動する。
    fn move_object_section(
        &self,
        params: &MoveObjectSectionParams,
    ) -> Result<ObjectSectionsOutcome, EditError>;

    /// レイヤーの名前・表示・ロックを変更する。
    ///
    /// **レイヤーのロックはこの operation を止めない。** 止めると、ロックされた
    /// レイヤーのロックを外す手段が無くなり、ロックが止める移動・削除・作成・
    /// 中間点の編集の行き止まりが解けなくなる。
    fn set_layer_state(&self, params: &SetLayerStateParams)
    -> Result<LayerStateOutcome, EditError>;

    /// BPM グリッドの一覧を置き換える。
    ///
    /// 部分更新ではない。要求した一覧がそのまま現在の一覧になる。
    ///
    /// 置き換えの API は戻り値を持たないため、同一区間内で読み直して件数を
    /// 照合する。値は照合しない——ホストは単精度で受け取り、並べ替えもする。
    fn set_grid_bpm(&self, params: &SetGridBpmParams) -> Result<GridBpmOutcome, EditError>;

    /// シーンの名前・解像度・サンプリングレートを変更する。
    ///
    /// **この変更は取り消せない。** 応答の `non_undoable` がその旨を運ぶ。
    ///
    /// 3 つの setter はいずれも戻り値を持たない。名前だけは編集区間の内側で
    /// 読み直して照合し、反映されていなければ残る 2 つを 1 つも発行しない。
    /// 解像度とサンプリングレートの反映値は編集情報にしか現れないため、区間を
    /// 抜けてから観測する。**要求値との差異は失敗にしない** — ホストが値を
    /// 調整し得るうえ、観測までの間に UI 操作が入り得る。
    fn set_scene_settings(
        &self,
        params: &SetSceneSettingsParams,
    ) -> Result<SceneSettingsOutcome, EditError>;

    /// カーソル・選択範囲・フォーカスを変更する。
    ///
    /// プロジェクトの内容を変えないため、revision を進めない。
    fn set_selection(&self, params: &SetSelectionParams) -> Result<SelectionState, EditError>;

    /// 複数の変更を 1 つの取り消し単位としてまとめて適用する。
    ///
    /// 変更を 1 つも発行する前に、全 sub-operation の対象解決・前提条件の照合・
    /// 逆操作の構築を終える。途中で失敗したら、既に適用した分を同一区間内で
    /// 逆順に巻き戻す。巻き戻しに失敗した場合、その事実を隠さず要求元へ伝える。
    ///
    /// **巻き戻しが保証するのは「戻せなかったことを黙認しない」ことだけである。**
    /// 戻したあとの状態が元と一字一句同じであることは保証しない。
    fn apply_batch(&self, params: &ApplyBatchParams) -> Result<BatchOutcome, EditError>;
}

/// SDK を実際に呼び出す edit adapter を作る。
///
/// 編集ハンドルが未初期化・未準備の間も生成でき、その状態の編集は SDK を
/// 呼ばずに受付できない状態として失敗する。
pub fn sdk_edit_adapter(project_state: Arc<ProjectState>) -> Arc<dyn EditAdapter> {
    Arc::new(HostEditAdapter::new(sdk::SdkEditHost, project_state))
}
