//! operation 別の params の復号。
//!
//! 復号と同時に、要求内容だけで決まる検証を済ませる。この段を通った要求は、
//! ライフサイクル状態にも期限にも実行口の応答にも依存しない誤りを持たない。

use super::{
    batch_input_error, describe_effects_error, edit_input_error, error_object, filter_error,
    item_values_error, label_error, page_limit_error, render_input_error,
};
use aviutl2_mcp_core::{
    AddEffectParams, ApplyBatchParams, CreateObjectParams, CreateObjectSectionParams,
    DeleteEffectParams, DeleteObjectParams, DeleteObjectSectionParams, DescribeEffectsParams,
    EditOperation, ErrorCode, ErrorObject, GetCurrentSceneParams, GetEditInfoParams,
    GetEffectItemValuesParams, GetObjectParams, GetSelectionParams, ListAvailableEffectsParams,
    ListFontsParams, ListLayersParams, ListModulesParams, ListObjectAliasesParams,
    ListObjectsParams, ListPalettesParams, MoveEffectParams, MoveObjectParams,
    MoveObjectSectionParams, ReadOperation, RenderFrameParams, RenderOperation,
    SetEffectEnabledParams, SetGridBpmParams, SetLayerStateParams, SetObjectItemParams,
    SetObjectNameParams, SetSceneSettingsParams, SetSelectionParams, ValidatedPageRequest,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// 復号と検証を終えた読み取り要求。
///
/// この型を作れた時点で、要求内容だけで判定できる誤りは残っていない。ページ
/// 指定を伴う operation が検証済みのページ要求を併せて運ぶのはそのためである。
/// params が持つ生のページ指定は切り出しへ渡せない。
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ReadRequest {
    GetEditInfo,
    GetCurrentScene,
    ListLayers(ListLayersParams, ValidatedPageRequest),
    ListObjects(ListObjectsParams, ValidatedPageRequest),
    GetObject(Box<GetObjectParams>),
    ListAvailableEffects(ListAvailableEffectsParams, ValidatedPageRequest),
    DescribeEffects(DescribeEffectsParams),
    GetEffectItemValues(Box<GetEffectItemValuesParams>),
    GetSelection(GetSelectionParams, ValidatedPageRequest),
    ListFonts(ValidatedPageRequest),
    ListPalettes(ValidatedPageRequest),
    ListModules(ListModulesParams, ValidatedPageRequest),
    ListObjectAliases(ListObjectAliasesParams, ValidatedPageRequest),
}

/// operation 別の params を復号し、要求内容だけで決まる検証を済ませる。
///
/// ページ指定と絞り込み条件の検証もここで行う。いずれも要求内容だけで決まり、
/// ライフサイクル状態にも期限にも読み取り口の応答にも依存しない。
pub(super) fn decode_request(
    operation: ReadOperation,
    params: &Value,
) -> Result<ReadRequest, ErrorObject> {
    Ok(match operation {
        ReadOperation::GetEditInfo => {
            decode_params::<GetEditInfoParams>(params)?;
            ReadRequest::GetEditInfo
        }
        ReadOperation::GetCurrentScene => {
            decode_params::<GetCurrentSceneParams>(params)?;
            ReadRequest::GetCurrentScene
        }
        ReadOperation::ListLayers => {
            let params: ListLayersParams = decode_params(params)?;
            let page = params.page.validate().map_err(page_limit_error)?;
            ReadRequest::ListLayers(params, page)
        }
        ReadOperation::ListObjects => {
            let params: ListObjectsParams = decode_params(params)?;
            let page = params.page.validate().map_err(page_limit_error)?;
            if let Some(filter) = &params.filter {
                filter.validate().map_err(filter_error)?;
            }
            ReadRequest::ListObjects(params, page)
        }
        ReadOperation::GetObject => {
            ReadRequest::GetObject(Box::new(decode_params::<GetObjectParams>(params)?))
        }
        ReadOperation::ListAvailableEffects => {
            let params: ListAvailableEffectsParams = decode_params(params)?;
            let page = params.page.validate().map_err(page_limit_error)?;
            ReadRequest::ListAvailableEffects(params, page)
        }
        ReadOperation::DescribeEffects => {
            let params: DescribeEffectsParams = decode_params(params)?;
            params.validate().map_err(describe_effects_error)?;
            ReadRequest::DescribeEffects(params)
        }
        ReadOperation::GetEffectItemValues => {
            let params: GetEffectItemValuesParams = decode_params(params)?;
            params.validate().map_err(item_values_error)?;
            ReadRequest::GetEffectItemValues(Box::new(params))
        }
        ReadOperation::GetSelection => {
            let params: GetSelectionParams = decode_params(params)?;
            let page = params.page.validate().map_err(page_limit_error)?;
            ReadRequest::GetSelection(params, page)
        }
        ReadOperation::ListFonts => {
            let params: ListFontsParams = decode_params(params)?;
            ReadRequest::ListFonts(params.page.validate().map_err(page_limit_error)?)
        }
        ReadOperation::ListPalettes => {
            let params: ListPalettesParams = decode_params(params)?;
            ReadRequest::ListPalettes(params.page.validate().map_err(page_limit_error)?)
        }
        ReadOperation::ListModules => {
            let params: ListModulesParams = decode_params(params)?;
            let page = params.page.validate().map_err(page_limit_error)?;
            ReadRequest::ListModules(params, page)
        }
        ReadOperation::ListObjectAliases => {
            let params: ListObjectAliasesParams = decode_params(params)?;
            let page = params.page.validate().map_err(page_limit_error)?;
            params.validate().map_err(label_error)?;
            ReadRequest::ListObjectAliases(params, page)
        }
    })
}

/// 復号と検証を終えた編集要求。
///
/// この型を作れた時点で、要求内容だけで判定できる誤りは残っていない。
#[derive(Debug, Clone, PartialEq)]
pub(super) enum EditRequest {
    CreateObject(Box<CreateObjectParams>),
    MoveObject(Box<MoveObjectParams>),
    DeleteObject(Box<DeleteObjectParams>),
    SetObjectName(Box<SetObjectNameParams>),
    SetObjectItem(Box<SetObjectItemParams>),
    AddEffect(Box<AddEffectParams>),
    DeleteEffect(Box<DeleteEffectParams>),
    SetEffectEnabled(Box<SetEffectEnabledParams>),
    MoveEffect(Box<MoveEffectParams>),
    SetLayerState(Box<SetLayerStateParams>),
    SetSelection(Box<SetSelectionParams>),
    CreateObjectSection(Box<CreateObjectSectionParams>),
    DeleteObjectSection(Box<DeleteObjectSectionParams>),
    MoveObjectSection(Box<MoveObjectSectionParams>),
    SetGridBpm(Box<SetGridBpmParams>),
    SetSceneSettings(Box<SetSceneSettingsParams>),
    ApplyBatch(Box<ApplyBatchParams>),
}

/// operation 別の params を復号し、要求内容だけで決まる検証を済ませる。
///
/// 値の種別整合・パス構文・文字列長・変更内容の全省略はいずれも要求内容だけで
/// 決まり、ライフサイクル状態にも期限にも編集口の応答にも依存しない。検証の
/// 実体は core と共有し、server と plugin が同じ判定を行う。
///
/// 一括適用は各 sub-operation について単独編集と同じ検証を通し、加えて件数・
/// シーンの揃い・同じ状態を書き換える重複を見る。いずれも要求内容だけで決まる
/// ため、他の編集と同じくこの段で判定する。
pub(super) fn decode_edit_request(
    operation: EditOperation,
    params: &Value,
) -> Result<EditRequest, ErrorObject> {
    /// 復号と検証を済ませて要求を組み立てる。
    macro_rules! decoded {
        ($ty:ty, $variant:path) => {{
            let params: $ty = decode_params(params)?;
            params.validate().map_err(edit_input_error)?;
            $variant(Box::new(params))
        }};
    }
    Ok(match operation {
        EditOperation::CreateObject => {
            decoded!(CreateObjectParams, EditRequest::CreateObject)
        }
        EditOperation::MoveObject => decoded!(MoveObjectParams, EditRequest::MoveObject),
        EditOperation::DeleteObject => decoded!(DeleteObjectParams, EditRequest::DeleteObject),
        EditOperation::SetObjectName => {
            decoded!(SetObjectNameParams, EditRequest::SetObjectName)
        }
        EditOperation::SetObjectItem => {
            decoded!(SetObjectItemParams, EditRequest::SetObjectItem)
        }
        EditOperation::AddEffect => decoded!(AddEffectParams, EditRequest::AddEffect),
        EditOperation::DeleteEffect => decoded!(DeleteEffectParams, EditRequest::DeleteEffect),
        EditOperation::SetEffectEnabled => {
            decoded!(SetEffectEnabledParams, EditRequest::SetEffectEnabled)
        }
        EditOperation::MoveEffect => decoded!(MoveEffectParams, EditRequest::MoveEffect),
        EditOperation::SetLayerState => {
            decoded!(SetLayerStateParams, EditRequest::SetLayerState)
        }
        EditOperation::SetSelection => decoded!(SetSelectionParams, EditRequest::SetSelection),
        EditOperation::CreateObjectSection => {
            decoded!(CreateObjectSectionParams, EditRequest::CreateObjectSection)
        }
        EditOperation::DeleteObjectSection => {
            decoded!(DeleteObjectSectionParams, EditRequest::DeleteObjectSection)
        }
        EditOperation::MoveObjectSection => {
            decoded!(MoveObjectSectionParams, EditRequest::MoveObjectSection)
        }
        EditOperation::SetGridBpm => decoded!(SetGridBpmParams, EditRequest::SetGridBpm),
        EditOperation::SetSceneSettings => {
            decoded!(SetSceneSettingsParams, EditRequest::SetSceneSettings)
        }
        EditOperation::ApplyBatch => {
            let params: ApplyBatchParams = decode_params(params)?;
            params.validate().map_err(batch_input_error)?;
            EditRequest::ApplyBatch(Box::new(params))
        }
    })
}

/// 復号と検証を終えたレンダリング要求。
///
/// この型を作れた時点で、要求内容だけで判定できる誤りは残っていない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RenderRequest {
    RenderFrame(RenderFrameParams),
}

/// operation 別の params を復号し、要求内容だけで決まる検証を済ませる。
///
/// ここで見るのはフレーム番号が受け渡せる範囲に収まることだけである。シーンの
/// 長さとの比較は編集情報を要するため、実行口が判定する。
pub(super) fn decode_render_request(
    operation: RenderOperation,
    params: &Value,
) -> Result<RenderRequest, ErrorObject> {
    Ok(match operation {
        RenderOperation::RenderFrame => {
            let params: RenderFrameParams = decode_params(params)?;
            params.validate().map_err(render_input_error)?;
            RenderRequest::RenderFrame(params)
        }
    })
}

/// operation 別の params へ復号する。
///
/// 失敗の説明には、不足したフィールド名や受理できないフィールド名が含まれる。
/// いずれも要求元が送った内容と入力型の定義だけに由来し、秘匿値は含まない。
fn decode_params<T: DeserializeOwned>(params: &Value) -> Result<T, ErrorObject> {
    serde_json::from_value(params.clone()).map_err(|e| {
        error_object(
            ErrorCode::InvalidArgument,
            format!("params の解釈に失敗しました: {e}"),
        )
    })
}
