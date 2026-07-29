//! SDK の編集 API を所有型 DTO へ写す層。
//!
//! 要求処理側は [`EditAdapter`] だけを見て編集を発行し、SDK の型・ハンドル・
//! 編集区間には一切触れない。SDK 呼び出しは [`host::EditHost`] の実装に閉じ、
//! 差し替えることで SDK 無しでも解決・前提条件・read-back の全手順を検証できる。
//!
//! 1 要求は 1 回の編集区間で完結し、SDK 上では 1 つの取り消し単位になる。

pub mod adapter;
pub mod error;
#[cfg(test)]
pub(crate) mod fake;
pub mod host;
pub mod precondition;
pub mod resolve;
pub mod sdk;

use crate::project::ProjectState;
use aviutl2_mcp_core::{
    AddEffectParams, CreateObjectParams, DeleteEffectParams, DeleteObjectParams, EditOutcome,
    MoveObjectParams, SelectionState, SetEffectStateParams, SetObjectItemParams,
    SetObjectNameParams, SetSelectionParams,
};
use std::sync::Arc;

pub use adapter::HostEditAdapter;
pub use error::{EditError, UnsupportedReason};

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

    /// effect の有効・ロック状態を変更する。
    fn set_effect_state(&self, params: &SetEffectStateParams) -> Result<EditOutcome, EditError>;

    /// カーソル・選択範囲・フォーカスを変更する。
    ///
    /// プロジェクトの内容を変えないため、revision を進めない。
    fn set_selection(&self, params: &SetSelectionParams) -> Result<SelectionState, EditError>;
}

/// SDK を実際に呼び出す edit adapter を作る。
///
/// 編集ハンドルが未初期化・未準備の間も生成でき、その状態の編集は SDK を
/// 呼ばずに受付できない状態として失敗する。
pub fn sdk_edit_adapter(project_state: Arc<ProjectState>) -> Arc<dyn EditAdapter> {
    Arc::new(HostEditAdapter::new(sdk::SdkEditHost, project_state))
}
