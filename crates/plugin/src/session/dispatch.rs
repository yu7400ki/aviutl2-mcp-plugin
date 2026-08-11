//! 復号済みの要求を実行口へ発行する。

use super::decode::{EditRequest, ReadRequest, RenderRequest};
use super::{edit_error, error_object, read_error, render_error, snapshot_revision_error};
use crate::edit::EditAdapter;
use crate::read::ReadAdapter;
use crate::render::RenderAdapter;
use aviutl2_mcp_core::{
    ErrorCode, ErrorObject, GetCurrentSceneResult, ListFontsResult, ListLayersResult,
    ListModulesResult, ListObjectsResult, PageWindow, RenderFrameResult, SelectionSnapshot,
    ValidatedPageRequest, take_page, take_window,
};
use serde::Serialize;
use serde_json::Value;

/// 読み取りを実行し、応答へ載せる result を組み立てる。
///
/// 読み取り口は SDK の参照区間を抜けてから所有型の DTO を返す。JSON への変換は
/// その外側で行い、参照区間の内側には持ち込まない。
///
/// ページの切り出しも原則ここで行うが、オブジェクトの列挙だけは読み取り口が
/// 参照区間の内側で切り出す。1 件の読み取りが重く、応答へ載せない対象まで読むと
/// 参照区間の保持時間がプロジェクトの規模で決まってしまうためである。
pub(super) fn dispatch_read(
    adapter: &dyn ReadAdapter,
    request: ReadRequest,
) -> Result<Value, ErrorObject> {
    match request {
        ReadRequest::GetEditInfo => to_result(&adapter.get_edit_info().map_err(read_error)?),
        ReadRequest::GetCurrentScene => {
            let (scene, project_revision) = adapter.get_current_scene().map_err(read_error)?;
            to_result(&GetCurrentSceneResult {
                scene,
                project_revision,
            })
        }
        ReadRequest::ListLayers(params, request) => {
            let snapshot = adapter
                .list_layers(params.expected_scene_id)
                .map_err(read_error)?;
            let (items, page) = take_page(&snapshot.items, &request, snapshot.snapshot_revision)
                .map_err(snapshot_revision_error)?;
            to_result(&ListLayersResult { items, page })
        }
        ReadRequest::ListObjects(params, request) => {
            // 切り出しは読み取り口が済ませている。参照区間の失敗と、ページ要求
            // そのものの不整合は別の失敗であり、対応するエラーも異なる。
            let page = adapter
                .list_objects(params.expected_scene_id, params.filter.as_ref(), &request)
                .map_err(read_error)?
                .map_err(snapshot_revision_error)?;
            to_result(&ListObjectsResult {
                items: page.items,
                page: page.meta,
            })
        }
        ReadRequest::GetObject(params) => {
            to_result(&adapter.get_object(&params.selector).map_err(read_error)?)
        }
        ReadRequest::ListAvailableEffects(params, request) => {
            // 切り出しは読み取り口が済ませている。設定項目を数えるのが窓に入った
            // 分だけであることを、切り出しと同じ場所で保証する必要がある。
            let result = adapter
                .list_available_effects(
                    params.effect_type.as_ref(),
                    &catalog_page_request(&request),
                )
                .map_err(read_error)?;
            to_result(&result)
        }
        ReadRequest::DescribeEffects(params) => {
            to_result(&adapter.describe_effects(&params).map_err(read_error)?)
        }
        ReadRequest::GetEffectItemValues(params) => to_result(
            &adapter
                .get_effect_item_values(&params)
                .map_err(read_error)?,
        ),
        ReadRequest::GetSelection(params, request) => {
            // 切り出しは読み取り口が済ませている。参照区間の失敗と、ページ要求
            // そのものの不整合は別の失敗であり、対応するエラーも異なる。
            let snapshot: SelectionSnapshot = adapter
                .get_selection(params.expected_scene_id, &request)
                .map_err(read_error)?
                .map_err(snapshot_revision_error)?;
            to_result(&snapshot)
        }
        ReadRequest::ListFonts(request) => {
            let snapshot = adapter.list_fonts().map_err(read_error)?;
            let (items, page) = take_window(
                &snapshot.items,
                &catalog_page_request(&request),
                snapshot.snapshot_revision,
            );
            to_result(&ListFontsResult { items, page })
        }
        ReadRequest::ListPalettes(request) => {
            // 切り出しは読み取り口が済ませている。色を読むのが窓に入った分だけで
            // あることを、参照区間の内側で保証する必要がある。
            let result = adapter
                .list_palettes(&catalog_page_request(&request))
                .map_err(read_error)?;
            to_result(&result)
        }
        ReadRequest::ListModules(params, request) => {
            let snapshot = adapter
                .list_modules(params.module_type.as_ref())
                .map_err(read_error)?;
            let (items, page) = take_window(
                &snapshot.items,
                &catalog_page_request(&request),
                snapshot.snapshot_revision,
            );
            to_result(&ListModulesResult { items, page })
        }
        ReadRequest::ListObjectAliases(params, request) => {
            // 切り出しは読み取り口が済ませている。ファイルを開くのが窓に入った
            // 分だけであることを、切り出しと同じ場所で保証する必要がある。
            let result = adapter
                .list_object_aliases(params.label.as_deref(), &catalog_page_request(&request))
                .map_err(read_error)?;
            to_result(&result)
        }
    }
}

/// カタログの一覧に対するページ要求から revision の照合指定を落とす。
///
/// 登録済み effect・フォント・パレット・モジュールはいずれも、プロジェクトの
/// 編集内容から独立した登録物の集合である。要求元が前ページの revision を送り
/// 返しても照合しない。照合すると、一覧と無関係な編集で値が進んだだけでページ間の
/// 照合が食い違い、要求元は先頭からの取り直しを強いられる。一方でカタログ自身の
/// 変化はその値に現れないため、照合しても取りこぼしは防げない。
///
/// 応答へ載せる revision は落とさない。それは列挙を始めた時点のプロジェクト
/// revision であり、ページのメタ情報が表す意味そのものである。照合に使えない
/// ことを表す固定値へ置き換えても、実在し得る revision と区別が付かない。
///
/// 落とした結果は取り出し範囲であり、切り出しは失敗しない。照合しないと決めた
/// ことが、失敗の種類が 0 であることとして型に現れる。
fn catalog_page_request(page: &ValidatedPageRequest) -> PageWindow {
    page.window()
}

/// 編集を実行し、応答へ載せる result を組み立てる。
///
/// 編集口は SDK の編集区間を抜けてから所有型の DTO を返す。JSON への変換は
/// その外側で行い、区間の内側には持ち込まない。
pub(super) fn dispatch_edit(
    adapter: &dyn EditAdapter,
    request: EditRequest,
) -> Result<Value, ErrorObject> {
    match request {
        EditRequest::CreateObject(params) => {
            to_result(&adapter.create_object(&params).map_err(edit_error)?)
        }
        EditRequest::MoveObject(params) => {
            to_result(&adapter.move_object(&params).map_err(edit_error)?)
        }
        EditRequest::DeleteObject(params) => {
            to_result(&adapter.delete_object(&params).map_err(edit_error)?)
        }
        EditRequest::SetObjectName(params) => {
            to_result(&adapter.set_object_name(&params).map_err(edit_error)?)
        }
        EditRequest::SetObjectItem(params) => {
            to_result(&adapter.set_object_item(&params).map_err(edit_error)?)
        }
        EditRequest::AddEffect(params) => {
            to_result(&adapter.add_effect(&params).map_err(edit_error)?)
        }
        EditRequest::DeleteEffect(params) => {
            to_result(&adapter.delete_effect(&params).map_err(edit_error)?)
        }
        EditRequest::SetEffectEnabled(params) => {
            to_result(&adapter.set_effect_enabled(&params).map_err(edit_error)?)
        }
        EditRequest::MoveEffect(params) => {
            to_result(&adapter.move_effect(&params).map_err(edit_error)?)
        }
        EditRequest::SetLayerState(params) => {
            to_result(&adapter.set_layer_state(&params).map_err(edit_error)?)
        }
        EditRequest::SetSelection(params) => {
            to_result(&adapter.set_selection(&params).map_err(edit_error)?)
        }
        EditRequest::CreateObjectSection(params) => {
            to_result(&adapter.create_object_section(&params).map_err(edit_error)?)
        }
        EditRequest::DeleteObjectSection(params) => {
            to_result(&adapter.delete_object_section(&params).map_err(edit_error)?)
        }
        EditRequest::MoveObjectSection(params) => {
            to_result(&adapter.move_object_section(&params).map_err(edit_error)?)
        }
        EditRequest::SetGridBpm(params) => {
            to_result(&adapter.set_grid_bpm(&params).map_err(edit_error)?)
        }
        EditRequest::SetSceneSettings(params) => {
            to_result(&adapter.set_scene_settings(&params).map_err(edit_error)?)
        }
        EditRequest::ApplyBatch(params) => {
            to_result(&adapter.apply_batch(&params).map_err(edit_error)?)
        }
    }
}

/// レンダリングを実行し、応答へ載せる result を組み立てる。
///
/// JSON への変換はここでは行わない。応答を送れなかったときに引き渡し用ファイル
/// を消せるよう、識別子を持つ所有型のまま呼び出し元へ返す。
pub(super) fn dispatch_render(
    adapter: &dyn RenderAdapter,
    request: RenderRequest,
) -> Result<RenderFrameResult, ErrorObject> {
    match request {
        RenderRequest::RenderFrame(params) => adapter.render_frame(&params).map_err(render_error),
    }
}

/// 読み取り結果を応答へ載せる JSON へ変換する。
///
/// 変換できるかは DTO の定義だけで決まり、要求元には手立てが無い。失敗の詳細は
/// ローカルのログにのみ残す。
pub(super) fn to_result<T: Serialize>(value: &T) -> Result<Value, ErrorObject> {
    serde_json::to_value(value).map_err(|e| {
        tracing::error!("読み取り結果の JSON 変換に失敗しました: {e}");
        error_object(
            ErrorCode::InternalError,
            "読み取り結果を応答へ変換できませんでした",
        )
    })
}
