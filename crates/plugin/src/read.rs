//! SDK の読み取り API を所有型 DTO へ写す層。
//!
//! 要求処理側は [`ReadAdapter`] だけを見て読み取りを発行し、SDK の型・ハンドル・
//! 参照区間には一切触れない。SDK 呼び出しは [`host::ReadHost`] の実装に閉じ、
//! 差し替えることで SDK 無しでも読み取りの手順を検証できる。
//!
//! 列挙は 1 度の参照区間で全件をスナップショット化し、その時点の revision を
//! 添えて返す。切り出しは応答側で行う。
//!
//! ただしオブジェクトの列挙だけは、ページの切り出しを参照区間の内側で行う。
//! 1 件あたりの読み取りが alias と配下 effect を含んで重く、全件を読んでから
//! 切り出すと参照区間の保持時間がプロジェクトの規模に比例して伸びるためである。

pub mod adapter;
pub mod error;
pub mod host;
pub mod resolve;
pub mod sdk;

use crate::project::ProjectState;
use aviutl2_mcp_core::{
    AvailableEffect, EditInfo, EffectItemValues, EffectType, GetEffectItemValuesParams, LayerInfo,
    ListObjectAliasesResult, ListPalettesResult, ModuleEntry, ModuleType, ObjectDetail,
    ObjectFilter, ObjectSelector, ObjectSummary, PageMeta, PageWindow, SceneInfo,
    SelectionSnapshot, SnapshotRevisionMismatch, ValidatedPageRequest,
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

/// 列挙から切り出した 1 ページ。
///
/// 切り出しを参照区間の内側で行う列挙が返す。`meta` は [`Snapshot`] から
/// 切り出した場合と同じ意味を持ち、`total_count` は列挙全体の件数である。
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    /// このページの要素。
    pub items: Vec<T>,
    /// ページのメタ情報。
    pub meta: PageMeta,
}

/// プロジェクトの状態。
///
/// SDK に触れずに読み取れる値だけで構成する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectStatus {
    /// プロジェクトの epoch。
    pub epoch: String,
    /// プロジェクトの revision。
    pub revision: u64,
    /// 最後の保存以降に変更があるか。
    ///
    /// 真は「変更があり得る」ことを表し、偽だけが「変更が無い」ことを表す。
    /// プロジェクトを開いた直後・新規作成した直後は、編集していなくても真になる。
    pub modified: bool,
}

/// 読み取り operation の実行口。
///
/// 各メソッドは 1 度の呼び出しで完結し、SDK の参照区間を跨いで状態を保持しない。
/// 戻り値は所有型のみで、opaque handle を公開しない。
pub trait ReadAdapter: Send + Sync {
    /// プロジェクトの状態を取得する。
    ///
    /// 編集ハンドルにも参照区間にも触れないため、読み取りを受け付けられない
    /// 状態でも呼び出せる。生存確認の応答へ載せるために用いる。
    fn project_status(&self) -> ProjectStatus;

    /// 現在の編集情報を取得する。
    fn get_edit_info(&self) -> Result<EditInfo, ReadError>;

    /// 現在シーンと、取得時点のプロジェクト revision を取得する。
    fn get_current_scene(&self) -> Result<(SceneInfo, u64), ReadError>;

    /// 現在シーンのレイヤーを全件列挙する。
    ///
    /// `expected_scene_id` が現在シーンと異なる場合は前提条件の不整合とする。
    fn list_layers(&self, expected_scene_id: i32) -> Result<Snapshot<LayerInfo>, ReadError>;

    /// 現在シーンのオブジェクトを列挙し、要求ページを切り出して返す。
    ///
    /// `filter` は検証済みのものだけを受け取る。絞り込み条件の妥当性は要求内容
    /// だけで決まり、読み取りを受け付けられるかにも期限にも依存しないため、
    /// 要求の復号と同じ場所で判定して不正な条件はここへ届かせない。`page` は
    /// 検証を通ったことを型が表しており、範囲外の `limit` はここへ届かない。
    ///
    /// 切り出しを呼び出し側へ委ねず、ここで行う。オブジェクト 1 件の読み取りは
    /// alias と配下 effect を含んで重く、全件を読んでから切り出すと、応答へ
    /// 載せない対象の読み取りにも参照区間を占有することになる。ページ窓を先に
    /// 確定すれば、1 要求あたりの重い読み取りが要求ページの件数で上限付きになる。
    ///
    /// スナップショット revision の不一致は参照区間の失敗ではないため、畳まずに
    /// 返す。
    fn list_objects(
        &self,
        expected_scene_id: i32,
        filter: Option<&ObjectFilter>,
        page: &ValidatedPageRequest,
    ) -> Result<Result<Page<ObjectSummary>, SnapshotRevisionMismatch>, ReadError>;

    /// セレクターが指すオブジェクトの詳細を取得する。
    fn get_object(&self, selector: &ObjectSelector) -> Result<ObjectDetail, ReadError>;

    /// フォーカス対象・その区間番号・選択中オブジェクトの一覧を取得する。
    ///
    /// `expected_scene_id` が現在シーンと異なる場合は前提条件の不整合とする。
    /// 選択は現在シーンの内側の概念であり、シーンが変わった後の選択を前の
    /// シーンのものとして返さない。
    ///
    /// 切り出しは [`Self::list_objects`] と同じ理由でここで行う。選択件数だけ
    /// alias を読むと、1 要求あたりの重い読み取りが要求ページではなく選択の
    /// 規模で決まってしまう。ページングが掛かるのは選択の一覧だけであり、
    /// フォーカス対象は 1 件しか無い。
    ///
    /// フォーカス対象と区間番号は同じ参照区間の内側で読む。別の呼び出しに
    /// 分けると、間に利用者の操作が入って両者が食い違った組を返し得る。
    ///
    /// スナップショット revision の不一致は参照区間の失敗ではないため、畳まずに
    /// 返す。
    fn get_selection(
        &self,
        expected_scene_id: i32,
        page: &ValidatedPageRequest,
    ) -> Result<Result<SelectionSnapshot, SnapshotRevisionMismatch>, ReadError>;

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

    /// 登録済みフォント名を全件列挙する。
    ///
    /// 参照区間へ入らない。フォント名の列挙は編集ハンドルの機能であり、
    /// プロジェクトデータの参照を要しない。同じ理由でシーンの guard も掛けない。
    ///
    /// revision の扱いは [`Self::list_available_effects`] と同じである。返す
    /// `snapshot_revision` は列挙時点のプロジェクト revision だが、一覧の内容は
    /// この値に連動しない。
    fn list_fonts(&self) -> Result<Snapshot<String>, ReadError>;

    /// 登録済みパレットを列挙し、要求ページを切り出して返す。
    ///
    /// 名前の列挙と色の取得を 1 度の参照区間の内側で行う。分けると、名前を
    /// 集めてから色を読むまでの間にパレットが差し替わり、食い違った組を返し得る。
    ///
    /// 切り出しをここで行うのは [`Self::list_objects`] と同じ理由である。色は
    /// パレット 1 件あたり [`aviutl2_mcp_core::PALETTE_COLOR_COUNT`] 件あり、
    /// 応答へ載せない分まで読むと参照区間の保持時間が登録数で決まってしまう。
    ///
    /// 参照区間へ入るが、シーンの guard は掛けない。区間へ入ることと、シーンに
    /// 紐づく値であることは別である。
    ///
    /// 受け取るのは取り出し範囲であり、ページ要求そのものではない。この一覧は
    /// revision を照合しないため、切り出しが失敗することはない。
    fn list_palettes(&self, page: &PageWindow) -> Result<ListPalettesResult, ReadError>;

    /// 登録済みモジュールを全件列挙する。
    ///
    /// 参照区間へ入らない理由も revision の扱いも [`Self::list_fonts`] と同じ
    /// である。
    fn list_modules(
        &self,
        module_type: Option<&ModuleType>,
    ) -> Result<Snapshot<ModuleEntry>, ReadError>;

    /// 登録済みオブジェクトエイリアスを列挙し、要求ページを切り出して返す。
    ///
    /// **SDK を 1 度も呼ばない。** 参照区間にも編集区間にも入らず、ホストの
    /// メインスレッドを 1 度も保持しない。読むのは AviUtl2 のデータディレクトリ
    /// 配下のファイルだけであり、プロジェクトの状態は 1 つも観測しない。
    ///
    /// 切り出しをここで行うのは [`Self::list_palettes`] と同じ理由である。
    /// エントリ 1 件の読み取りはファイルを開いてパースする分だけ重く、応答へ
    /// 載せない対象まで読むと費用がディレクトリの中身で決まってしまう。
    ///
    /// 返す `snapshot_revision` は列挙を始めた時点のプロジェクト revision だが、
    /// 一覧の内容はこの値に連動しない。扱いは
    /// [`Self::list_available_effects`] と同じである。切り出しが失敗しない理由も
    /// [`Self::list_palettes`] と同じである。
    ///
    /// データディレクトリを解決できない場合は、この AviUtl2 では機能が使えない
    /// ことを述べる失敗を返す。要求そのものは正しい。
    fn list_object_aliases(
        &self,
        label: Option<&str>,
        page: &PageWindow,
    ) -> Result<ListObjectAliasesResult, ReadError>;

    /// effect の設定項目を、要求されたフレームで評価した値を返す。
    ///
    /// `params` は件数と項目名の検証済みのものだけを受け取る。いずれも要求内容
    /// だけで決まり、読み取りを受け付けられるかにも期限にも依存しないため、
    /// 要求の復号と同じ場所で判定して不正な要求はここへ届かせない。
    fn get_effect_item_values(
        &self,
        params: &GetEffectItemValuesParams,
    ) -> Result<EffectItemValues, ReadError>;
}

/// SDK を実際に呼び出す read adapter を作る。
///
/// 編集ハンドルが未初期化・未準備の間も生成でき、その状態の読み取りは
/// SDK を呼ばずに [`ReadError::NotReady`] となる。
pub fn sdk_read_adapter(project_state: Arc<ProjectState>) -> Arc<dyn ReadAdapter> {
    Arc::new(HostReadAdapter::new(sdk::SdkReadHost, project_state))
}
