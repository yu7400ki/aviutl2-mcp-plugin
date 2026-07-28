//! SDK の読み取り API を所有型 DTO へ写す層。
//!
//! 要求処理側は [`ReadAdapter`] だけを見て読み取りを発行し、SDK の型・ハンドル・
//! 参照区間には一切触れない。SDK 呼び出しは [`host::ReadHost`] の実装に閉じ、
//! 差し替えることで SDK 無しでも読み取りの手順を検証できる。
//!
//! 応答に載せるページの切り出しはここでは行わない。列挙は 1 度の参照区間で
//! 全件をスナップショット化し、その時点の revision を添えて返す。

pub mod adapter;
pub mod error;
pub mod host;
pub mod sdk;

use crate::project::ProjectState;
use aviutl2_mcp_core::{
    AvailableEffect, EditInfo, EffectType, LayerInfo, ObjectDetail, ObjectFilter, ObjectSelector,
    ObjectSummary, SceneInfo,
};
use std::sync::Arc;

pub use adapter::HostReadAdapter;
pub use error::ReadError;
pub use host::EditState;

/// 一覧を 1 度の参照区間で取り切った結果。
///
/// `snapshot_revision` は列挙を始めた時点のプロジェクト revision であり、
/// 後続ページの一貫性検証に用いる。
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot<T> {
    /// 列挙結果の全件。
    pub items: Vec<T>,
    /// 列挙を始めた時点のプロジェクト revision。
    pub snapshot_revision: u64,
}

/// 読み取り operation の実行口。
///
/// 各メソッドは 1 度の呼び出しで完結し、SDK の参照区間を跨いで状態を保持しない。
/// 戻り値は所有型のみで、opaque handle を公開しない。
pub trait ReadAdapter: Send + Sync {
    /// 現在の編集情報を取得する。
    fn get_edit_info(&self) -> Result<EditInfo, ReadError>;

    /// 現在シーンと、取得時点のプロジェクト revision を取得する。
    fn get_current_scene(&self) -> Result<(SceneInfo, u64), ReadError>;

    /// 現在シーンのレイヤーを全件列挙する。
    ///
    /// `expected_scene_id` が現在シーンと異なる場合は前提条件の不整合とする。
    fn list_layers(&self, expected_scene_id: i32) -> Result<Snapshot<LayerInfo>, ReadError>;

    /// 現在シーンのオブジェクトを全件列挙する。
    fn list_objects(
        &self,
        expected_scene_id: i32,
        filter: Option<&ObjectFilter>,
    ) -> Result<Snapshot<ObjectSummary>, ReadError>;

    /// セレクターが指すオブジェクトの詳細を取得する。
    fn get_object(&self, selector: &ObjectSelector) -> Result<ObjectDetail, ReadError>;

    /// 登録済み effect を全件列挙する。
    ///
    /// 結果は登録済みプラグインの集合であり、プロジェクトの編集内容から独立して
    /// いる。返す `snapshot_revision` は列挙時点のプロジェクト revision だが、
    /// 一覧の内容はこの値に連動しない。revision の一致をページ間の一貫性検証に
    /// 用いると、無関係な編集で revision が進んだだけで後続ページが拒否される
    /// 一方、カタログ自体の変化は検出できない。この operation は revision による
    /// 一貫性検証の対象にしない。
    fn list_available_effects(
        &self,
        effect_type: Option<&EffectType>,
    ) -> Result<Snapshot<AvailableEffect>, ReadError>;
}

/// SDK を実際に呼び出す read adapter を作る。
///
/// 編集ハンドルが未初期化・未準備の間も生成でき、その状態の読み取りは
/// SDK を呼ばずに [`ReadError::NotReady`] となる。
pub fn sdk_read_adapter(project_state: Arc<ProjectState>) -> Arc<dyn ReadAdapter> {
    Arc::new(HostReadAdapter::new(sdk::SdkReadHost, project_state))
}
