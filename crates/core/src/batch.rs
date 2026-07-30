//! 一括適用の params / result と、要求内容だけで決まる入力検証。
//!
//! sub-operation は前提条件のフィールドを持たない。プロジェクト境界は
//! [`ObjectSelector::project_epoch`]、対象の同一性は selector の `fingerprint`
//! が担うため、要求全体で照合する値を別に受け取ると、同じ意味の値が 1 要求の
//! 2 か所に現れて不一致な組を作れてしまう。
//!
//! 検証は単独の編集 operation と同じ実装を通す。同じ `move_object` /
//! `set_object_item` が単独と一括で違う入力を受理すると、要求元は経路ごとに
//! 規則を持つことになる。
//!
//! 応答は読み取りの DTO（[`ObjectSummary`] / [`EffectInfo`]）を再利用する。

use crate::edit::{Destination, EditInputError, MoveObjectParams, SetObjectItemParams};
use crate::effect::EffectInfo;
use crate::error::ErrorCode;
use crate::fingerprint::Fingerprint;
use crate::item_value::ItemValue;
use crate::object::ObjectSummary;
use crate::selector::{EffectSelector, ObjectSelector};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 1 要求に含められる sub-operation の上限。
pub const MAX_BATCH_OPERATIONS: usize = 100;

/// 一括適用の 1 要素。
///
/// **受け付けるのは、変更前の値だけから逆操作を完全に組み立てられる 2 種類
/// だけである。** 作成・削除・名前や状態の変更は、逆操作が同一区間内で発行
/// できないか、成否を戻り値から判別できないため含めない。union に 2 つしか
/// variant が無いこと自体が、それらの拒否を兼ねる — 未知の判別子は復号の
/// 段で落ちるため、実行時に「一括適用に入れられない operation か」を判定する
/// 分岐を持たない。
///
/// variant は**構造体 variant** とする。判別子を内側に持つ表現では、unit
/// variant が `deny_unknown_fields` を無視して未知フィールドを読み飛ばす。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum BatchOperation {
    /// レイヤーと開始フレームを変更する。
    #[serde(rename = "move_object")]
    MoveObject {
        /// 対象オブジェクト。
        selector: ObjectSelector,
        /// 移動先。
        destination: Destination,
    },
    /// 設定項目 / track 値を変更する。
    #[serde(rename = "set_object_item")]
    SetObjectItem {
        /// 設定項目を持つ effect。
        selector: EffectSelector,
        /// 設定項目名。
        item: String,
        /// 設定する値。
        value: ItemValue,
    },
}

impl BatchOperation {
    /// 対象オブジェクトのセレクター。
    ///
    /// effect を指す sub-operation では、その effect が属するオブジェクトを
    /// 指すセレクターを返す。
    pub fn object_selector(&self) -> &ObjectSelector {
        match self {
            BatchOperation::MoveObject { selector, .. } => selector,
            BatchOperation::SetObjectItem { selector, .. } => &selector.object,
        }
    }

    /// 単独編集と同じ検証を通す。
    ///
    /// 単独の operation の params へ写してからその `validate` を呼ぶ。一括適用
    /// のために別の規則を書かないことを、実装の形で保証するためである。写す
    /// ための複製は 1 要求あたり [`MAX_BATCH_OPERATIONS`] 件までに限られる。
    fn validate(&self) -> Result<(), EditInputError> {
        match self {
            BatchOperation::MoveObject {
                selector,
                destination,
            } => MoveObjectParams {
                selector: selector.clone(),
                destination: *destination,
            }
            .validate(),
            BatchOperation::SetObjectItem {
                selector,
                item,
                value,
            } => SetObjectItemParams {
                selector: selector.clone(),
                item: item.clone(),
                value: value.clone(),
            }
            .validate(),
        }
    }

    /// 書き換える状態を表す、文字列としての鍵。
    fn target_key(&self) -> TargetKey<'_> {
        match self {
            BatchOperation::MoveObject { selector, .. } => TargetKey::Position {
                object: ObjectKey::of(selector),
            },
            BatchOperation::SetObjectItem { selector, item, .. } => TargetKey::Item {
                object: ObjectKey::of(&selector.object),
                effect_name: &selector.effect_name,
                effect_index: selector.effect_index,
                item,
            },
        }
    }
}

/// セレクターが名乗るオブジェクトの、文字列としての同一性。
#[derive(Debug, PartialEq, Eq, Hash)]
struct ObjectKey<'a> {
    scene_id: i32,
    layer: usize,
    frame: usize,
    name: Option<&'a str>,
    fingerprint: &'a Fingerprint,
}

impl<'a> ObjectKey<'a> {
    fn of(selector: &'a ObjectSelector) -> Self {
        Self {
            scene_id: selector.scene_id,
            layer: selector.layer,
            frame: selector.frame,
            name: selector.name.as_deref(),
            fingerprint: &selector.fingerprint,
        }
    }
}

/// sub-operation が書き換える状態の単位。
///
/// 移動はオブジェクトの位置を、設定は対象 effect の当該項目を書き換える。
/// 単位が違えば同じオブジェクトを指していても衝突しない。
#[derive(Debug, PartialEq, Eq, Hash)]
enum TargetKey<'a> {
    /// オブジェクトの位置。
    Position { object: ObjectKey<'a> },
    /// effect の設定項目。
    ///
    /// effect の fingerprint は含めない。同じ位置の同じ項目を指す 2 つの
    /// 要求は、fingerprint が違っても同じ状態を書き換える。
    Item {
        object: ObjectKey<'a>,
        effect_name: &'a str,
        effect_index: usize,
        item: &'a str,
    },
}

/// `apply_batch` の params。
///
/// **前提条件のフィールドを 1 つも持たない。** 全 sub-operation が selector を
/// 持つため、プロジェクト境界も現在シーンも対象の同一性も selector が運ぶ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyBatchParams {
    /// 配列順に適用する sub-operation。1 件以上
    /// [`MAX_BATCH_OPERATIONS`] 件以下。
    pub operations: Vec<BatchOperation>,
}

impl ApplyBatchParams {
    /// 要求内容だけで決まる検証を行う。
    ///
    /// 次の順で判定する。
    ///
    /// 1. `operations` の件数が範囲内
    /// 2. 全 sub-operation の selector が同じシーンを名乗る
    /// 3. 各 sub-operation が単独編集と同じ検証を通る
    /// 4. 同じ状態を書き換える sub-operation が複数無い
    ///
    /// **空の `operations` を拒否する。** 何も変更しない要求は、成功したのか
    /// 無視されたのかを要求元が区別できない。
    pub fn validate(&self) -> Result<(), BatchInputError> {
        if self.operations.is_empty() || self.operations.len() > MAX_BATCH_OPERATIONS {
            return Err(BatchInputError::OperationCountOutOfRange {
                count: self.operations.len(),
                max: MAX_BATCH_OPERATIONS,
            });
        }

        let expected_scene_id = self.operations[0].object_selector().scene_id;
        for (index, operation) in self.operations.iter().enumerate() {
            let scene_id = operation.object_selector().scene_id;
            if scene_id != expected_scene_id {
                return Err(BatchInputError::SceneIdMismatch {
                    index,
                    expected: expected_scene_id,
                    actual: scene_id,
                });
            }
        }

        for (index, operation) in self.operations.iter().enumerate() {
            operation
                .validate()
                .map_err(|source| BatchInputError::Operation { index, source })?;
        }

        // 文字列として同じ状態を指す組だけを見る。`name` の有無だけが違う
        // セレクターは文字列としては別物であり、ここでは重複にならない。
        // 同じ対象へ解決されるかは、対象を解決する層が判定する。
        let mut seen: HashMap<TargetKey<'_>, usize> = HashMap::new();
        for (index, operation) in self.operations.iter().enumerate() {
            if let Some(first_index) = seen.insert(operation.target_key(), index) {
                return Err(BatchInputError::DuplicateTarget { index, first_index });
            }
        }

        Ok(())
    }
}

/// 一括適用の要求内容だけで決まる検証の失敗。
///
/// どの sub-operation で落ちたかを必ず伴う（件数の誤りを除く）。100 件の要求
/// に対して位置の分からない失敗を返すのは、訂正の手掛かりとして足りない。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BatchInputError {
    /// `operations` の件数が範囲外。
    #[error("operations は 1 件以上 {max} 件以下である必要があります: {count}")]
    OperationCountOutOfRange {
        /// 指定された件数。
        count: usize,
        /// 許容する最大件数。
        max: usize,
    },
    /// selector が名乗るシーンが揃っていない。
    #[error("operations[{index}] のシーンが他と異なります: {actual} != {expected}")]
    SceneIdMismatch {
        /// 食い違った sub-operation の位置。
        index: usize,
        /// 先頭の sub-operation が名乗るシーン。
        expected: i32,
        /// 食い違った sub-operation が名乗るシーン。
        actual: i32,
    },
    /// 同じ状態を 2 つの sub-operation が書き換える。
    #[error("operations[{index}] は operations[{first_index}] と同じ対象を書き換えます")]
    DuplicateTarget {
        /// 後から現れた sub-operation の位置。
        index: usize,
        /// 同じ状態を指す最初の sub-operation の位置。
        first_index: usize,
    },
    /// sub-operation の内容が単独編集と同じ検証に通らない。
    #[error("operations[{index}]: {source}")]
    Operation {
        /// 失敗した sub-operation の位置。
        index: usize,
        /// 失敗の内容。
        #[source]
        source: EditInputError,
    },
}

impl BatchInputError {
    /// 失敗した sub-operation の位置。要求全体の誤りでは `None`。
    pub fn failed_index(&self) -> Option<usize> {
        match self {
            BatchInputError::OperationCountOutOfRange { .. } => None,
            BatchInputError::SceneIdMismatch { index, .. }
            | BatchInputError::DuplicateTarget { index, .. }
            | BatchInputError::Operation { index, .. } => Some(*index),
        }
    }

    /// 対応するエラーコードを返す。
    ///
    /// sub-operation の失敗は単独編集と同じコードへ写す。同じ入力が経路に
    /// よって違うコードで返ることを避ける。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            BatchInputError::Operation { source, .. } => source.error_code(),
            BatchInputError::OperationCountOutOfRange { .. }
            | BatchInputError::SceneIdMismatch { .. }
            | BatchInputError::DuplicateTarget { .. } => ErrorCode::InvalidArgument,
        }
    }
}

/// 一括適用の結果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchOutcome {
    /// 変更後のプロジェクトの epoch。
    pub project_epoch: String,
    /// 変更を反映したあとの revision。
    ///
    /// 一括適用は全体で 1 つの取り消し単位を作るため、この値も要求全体で
    /// 1 つだけ持つ。
    pub project_revision: u64,
    /// 入力と同じ位置で並ぶ各 sub-operation の結果。
    pub results: Vec<BatchStepOutcome>,
}

/// 一括適用の 1 sub-operation の結果。
///
/// 内容は**全 sub-operation の適用を終えたあとに読み直したもの**である。
/// 同じ対象を指す複数の sub-operation は同一の値を持ち、返す selector は
/// いずれも適用後の状態に対応するため、そのまま次の要求へ使える。
///
/// 単独編集の結果型を再利用せず、次の 3 つを持たない形にしている。
///
/// - **revision を要素ごとに持たせない。** 要求全体で 1 しか進まない値を
///   要素ごとに持たせると、sub-operation それぞれが自分の世代を持つように
///   読め、1 つの取り消し単位であることと矛盾する説明になる。
/// - **作成されたオブジェクトの一覧を持たせない。** 一括適用に作成は
///   入らないため常に空になる。持たなければ、空のはずの配列へ何かが
///   混入する余地も、それを防ぐ仕掛けも要らない。
/// - **`object` を省略可能にしない。** 一括適用に削除は入らないため対象は
///   必ず生き残る。型で言い切れることを省略可能にすると、応答を扱う側に
///   到達しない分岐が生まれる。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchStepOutcome {
    /// 変更後の対象オブジェクト。
    pub object: ObjectSummary,
    /// 設定項目を変更した sub-operation でのみ設定する。移動では null。
    pub effect: Option<EffectInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{EffectItem, EffectItemType};
    use crate::fingerprint::{EffectFingerprintInput, ObjectFingerprintInput};
    use crate::item_value::ItemWriteError;
    use crate::number::FiniteF64;
    use crate::validation::{MAX_NAME_UTF16_UNITS, TextSyntaxError};
    use serde_json::{Value, json};

    const EPOCH: &str = "78be92d1-c8c9-44c6-ae52-387548971468";

    fn object_summary(layer: usize, name: Option<&str>) -> ObjectSummary {
        ObjectSummary::new(
            EPOCH,
            ObjectFingerprintInput {
                scene_id: 0,
                layer,
                frame_start: 120,
                frame_end: 240,
                name,
                alias: "alias",
            },
        )
    }

    fn object_selector(layer: usize) -> ObjectSelector {
        object_summary(layer, Some("立ち絵")).selector
    }

    fn effect_info(layer: usize) -> EffectInfo {
        EffectInfo::new(
            object_selector(layer),
            EffectFingerprintInput {
                effect_name: "動画ファイル",
                effect_index: 0,
                position: 0,
                effect_count: 1,
                enabled: true,
                locked: false,
                items: &[],
            },
        )
    }

    fn effect_selector(layer: usize) -> EffectSelector {
        effect_info(layer).selector
    }

    fn move_object(layer: usize) -> BatchOperation {
        BatchOperation::MoveObject {
            selector: object_selector(layer),
            destination: Destination {
                layer: 3,
                frame: 240,
            },
        }
    }

    fn set_object_item(layer: usize, item: &str) -> BatchOperation {
        BatchOperation::SetObjectItem {
            selector: effect_selector(layer),
            item: item.to_string(),
            value: ItemValue::Number {
                value: FiniteF64::try_new(12.5).unwrap(),
            },
        }
    }

    fn params(operations: Vec<BatchOperation>) -> ApplyBatchParams {
        ApplyBatchParams { operations }
    }

    #[test]
    fn operations_roundtrip() {
        for operation in [move_object(2), set_object_item(2, "X")] {
            let s = serde_json::to_string(&operation).unwrap();
            let restored: BatchOperation = serde_json::from_str(&s).unwrap();
            assert_eq!(restored, operation);
        }

        let params = params(vec![move_object(2), set_object_item(3, "X")]);
        let s = serde_json::to_string(&params).unwrap();
        assert_eq!(
            serde_json::from_str::<ApplyBatchParams>(&s).unwrap(),
            params
        );
    }

    #[test]
    fn operations_carry_the_type_discriminator() {
        let value = serde_json::to_value(move_object(2)).unwrap();
        assert_eq!(value["type"], json!("move_object"));
        let value = serde_json::to_value(set_object_item(2, "X")).unwrap();
        assert_eq!(value["type"], json!("set_object_item"));
    }

    /// 一括適用に入れない編集 operation が復号の段で落ちることを固定する。
    ///
    /// 一覧は編集 operation のうち一括適用の対象でないものを網羅する。
    /// `assert_eq!` の件数比較により、編集 operation が増えたときに一覧の
    /// 更新を促す。
    #[test]
    fn excluded_operation_types_are_rejected_by_the_decoder() {
        let excluded = [
            crate::operation::OPERATION_CREATE_OBJECT,
            crate::operation::OPERATION_DELETE_OBJECT,
            crate::operation::OPERATION_SET_OBJECT_NAME,
            crate::operation::OPERATION_ADD_EFFECT,
            crate::operation::OPERATION_DELETE_EFFECT,
            crate::operation::OPERATION_SET_EFFECT_ENABLED,
            crate::operation::OPERATION_SET_LAYER_STATE,
            crate::operation::OPERATION_SET_SELECTION,
        ];

        // 一括適用の対象 2 種と、一括適用そのものを除いた残りが除外対象である。
        let accepted = [
            crate::operation::OPERATION_MOVE_OBJECT,
            crate::operation::OPERATION_SET_OBJECT_ITEM,
            crate::operation::OPERATION_APPLY_BATCH,
        ];
        assert_eq!(
            excluded.len() + accepted.len(),
            crate::operation::EditOperation::ALL.len(),
            "編集 operation の一覧と除外の一覧が食い違っています"
        );

        for name in excluded {
            let mut value = serde_json::to_value(move_object(2)).unwrap();
            value["type"] = json!(name);
            assert!(
                serde_json::from_value::<BatchOperation>(value).is_err(),
                "{name} が sub-operation として受理されました"
            );
        }
    }

    #[test]
    fn operations_reject_unknown_fields() {
        for operation in [move_object(2), set_object_item(2, "X")] {
            let mut value = serde_json::to_value(&operation).unwrap();
            value
                .as_object_mut()
                .unwrap()
                .insert("future".to_string(), json!(1));
            assert!(serde_json::from_value::<BatchOperation>(value).is_err());
        }
    }

    #[test]
    fn nested_selectors_still_accept_unknown_fields() {
        // 往復型である selector の扱いは sub-operation の内側でも変わらない。
        let mut value = serde_json::to_value(move_object(2)).unwrap();
        value["selector"]
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(1));
        let restored: BatchOperation = serde_json::from_value(value).unwrap();
        assert_eq!(restored, move_object(2));
    }

    #[test]
    fn params_reject_the_fields_that_preconditions_used_to_use() {
        // epoch も現在シーンも revision も selector が運ぶ。要求の直下で
        // 受け取ると、同じ意味の値が 1 要求の 2 か所に現れる。
        for key in ["expected", "expected_scene_id", "project_revision"] {
            let mut value = serde_json::to_value(params(vec![move_object(2)])).unwrap();
            value
                .as_object_mut()
                .unwrap()
                .insert(key.to_string(), json!(0));
            assert!(
                serde_json::from_value::<ApplyBatchParams>(value).is_err(),
                "{key} が受理されました"
            );
        }
    }

    #[test]
    fn params_require_operations() {
        assert!(serde_json::from_str::<ApplyBatchParams>("{}").is_err());
    }

    #[test]
    fn params_accept_a_batch_within_the_limits() {
        assert_eq!(params(vec![move_object(2)]).validate(), Ok(()));

        let operations: Vec<BatchOperation> = (0..MAX_BATCH_OPERATIONS).map(move_object).collect();
        assert_eq!(params(operations).validate(), Ok(()));
    }

    #[test]
    fn params_reject_an_empty_or_oversized_batch() {
        assert_eq!(
            params(Vec::new()).validate(),
            Err(BatchInputError::OperationCountOutOfRange {
                count: 0,
                max: MAX_BATCH_OPERATIONS,
            })
        );

        let operations: Vec<BatchOperation> = (0..=MAX_BATCH_OPERATIONS).map(move_object).collect();
        let error = params(operations).validate().unwrap_err();
        assert_eq!(
            error,
            BatchInputError::OperationCountOutOfRange {
                count: MAX_BATCH_OPERATIONS + 1,
                max: MAX_BATCH_OPERATIONS,
            }
        );
        assert_eq!(error.failed_index(), None);
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    }

    #[test]
    fn params_reject_selectors_naming_different_scenes() {
        let mut other = object_selector(3);
        other.scene_id = 1;
        let error = params(vec![
            move_object(2),
            BatchOperation::MoveObject {
                selector: other,
                destination: Destination { layer: 4, frame: 0 },
            },
        ])
        .validate()
        .unwrap_err();
        assert_eq!(
            error,
            BatchInputError::SceneIdMismatch {
                index: 1,
                expected: 0,
                actual: 1,
            }
        );
        assert_eq!(error.failed_index(), Some(1));
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
    }

    #[test]
    fn params_reject_two_operations_writing_the_same_state() {
        // 同じオブジェクトを 2 回動かす要求は、2 つ目の逆操作が 1 つ目の
        // 結果を指すため、事前に組み立てられない。
        let error = params(vec![move_object(2), move_object(2)])
            .validate()
            .unwrap_err();
        assert_eq!(
            error,
            BatchInputError::DuplicateTarget {
                index: 1,
                first_index: 0,
            }
        );
        assert_eq!(error.failed_index(), Some(1));

        // 同じ項目を 2 回書く要求も、逆操作がどちらの元値を指すか定まらない。
        assert_eq!(
            params(vec![set_object_item(2, "X"), set_object_item(2, "X")])
                .validate()
                .unwrap_err(),
            BatchInputError::DuplicateTarget {
                index: 1,
                first_index: 0,
            }
        );
    }

    #[test]
    fn params_accept_operations_writing_different_states() {
        // 単位が違えば、同じオブジェクトを指していても衝突しない。
        assert_eq!(
            params(vec![move_object(2), set_object_item(2, "X")]).validate(),
            Ok(())
        );
        // 同じ effect の別項目も衝突しない。
        assert_eq!(
            params(vec![set_object_item(2, "X"), set_object_item(2, "Y")]).validate(),
            Ok(())
        );
    }

    #[test]
    fn a_selector_that_only_differs_by_its_name_is_not_a_string_duplicate() {
        // 名前を名乗らないセレクターは文字列としては別物である。同じ対象へ
        // 解決され得るが、それを判定できるのは対象を解決する層だけであり、
        // ここで拒否すると解決できたはずの要求まで落とす。
        let mut anonymous = object_selector(2);
        anonymous.name = None;
        let operations = vec![
            move_object(2),
            BatchOperation::MoveObject {
                selector: anonymous,
                destination: Destination { layer: 4, frame: 0 },
            },
        ];
        assert_eq!(params(operations).validate(), Ok(()));
    }

    #[test]
    fn params_report_the_index_of_the_operation_that_failed() {
        let error = params(vec![
            move_object(2),
            set_object_item(3, "項\0目"),
            move_object(4),
        ])
        .validate()
        .unwrap_err();
        assert_eq!(error.failed_index(), Some(1));
        assert!(matches!(
            error,
            BatchInputError::Operation {
                index: 1,
                source: EditInputError::Text {
                    source: TextSyntaxError::ContainsNul,
                    ..
                },
            }
        ));
    }

    #[test]
    fn sub_operations_are_validated_like_the_standalone_edits() {
        // 単独編集の検証をそのまま通すため、受理する入力の集合は経路によって
        // 変わらない。
        let over = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2 + 1);
        let operation = set_object_item(2, &over);
        let expected = match &operation {
            BatchOperation::SetObjectItem {
                selector,
                item,
                value,
            } => SetObjectItemParams {
                selector: selector.clone(),
                item: item.clone(),
                value: value.clone(),
            }
            .validate()
            .unwrap_err(),
            _ => unreachable!(),
        };
        assert_eq!(
            params(vec![operation]).validate(),
            Err(BatchInputError::Operation {
                index: 0,
                source: expected,
            })
        );

        // 未対応種別の生値は書き戻せないため受け付けない。
        let error = params(vec![BatchOperation::SetObjectItem {
            selector: effect_selector(2),
            item: "X".to_string(),
            value: ItemValue::Unknown {
                raw: "0".to_string(),
            },
        }])
        .validate()
        .unwrap_err();
        assert_eq!(
            error,
            BatchInputError::Operation {
                index: 0,
                source: EditInputError::ItemValue(ItemWriteError::UnknownValue),
            }
        );
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);

        // 位置指定の範囲もセレクターの範囲も単独編集と同じ規則で見る。
        let error = params(vec![BatchOperation::MoveObject {
            selector: object_selector(2),
            destination: Destination {
                layer: i32::MAX as u32 + 1,
                frame: 0,
            },
        }])
        .validate()
        .unwrap_err();
        assert!(matches!(
            error,
            BatchInputError::Operation {
                index: 0,
                source: EditInputError::PositionOutOfRange { .. },
            }
        ));

        let mut selector = object_selector(2);
        selector.layer = i32::MAX as usize + 1;
        let error = params(vec![BatchOperation::MoveObject {
            selector,
            destination: Destination { layer: 0, frame: 0 },
        }])
        .validate()
        .unwrap_err();
        assert!(matches!(
            error,
            BatchInputError::Operation {
                index: 0,
                source: EditInputError::IndexOutOfRange { .. },
            }
        ));
    }

    #[test]
    fn step_outcome_carries_the_effect_only_for_item_changes() {
        let outcome = BatchOutcome {
            project_epoch: EPOCH.to_string(),
            project_revision: 43,
            results: vec![
                BatchStepOutcome {
                    object: object_summary(3, Some("立ち絵")),
                    effect: None,
                },
                BatchStepOutcome {
                    object: object_summary(2, Some("立ち絵")),
                    effect: Some(EffectInfo::new(
                        object_selector(2),
                        EffectFingerprintInput {
                            effect_name: "動画ファイル",
                            effect_index: 0,
                            position: 0,
                            effect_count: 1,
                            enabled: true,
                            locked: false,
                            items: &[EffectItem {
                                name: "X".to_string(),
                                item_type: EffectItemType::Number,
                                value: ItemValue::Number {
                                    value: FiniteF64::try_new(12.5).unwrap(),
                                },
                                track: None,
                            }],
                        },
                    )),
                },
            ],
        };

        let value = serde_json::to_value(&outcome).unwrap();
        assert_eq!(value["results"][0]["effect"], Value::Null);
        assert!(value["results"][1]["effect"].is_object());

        let s = serde_json::to_string(&outcome).unwrap();
        assert_eq!(serde_json::from_str::<BatchOutcome>(&s).unwrap(), outcome);
    }

    #[test]
    fn step_outcome_has_no_revision_and_no_created_objects() {
        let value = serde_json::to_value(BatchStepOutcome {
            object: object_summary(2, None),
            effect: None,
        })
        .unwrap();
        let object = value.as_object().unwrap();
        assert!(object.get("project_revision").is_none());
        assert!(object.get("created").is_none());
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["effect", "object"]);
    }

    #[test]
    fn the_outcome_accepts_unknown_optional_fields() {
        let outcome = BatchOutcome {
            project_epoch: EPOCH.to_string(),
            project_revision: 43,
            results: Vec::new(),
        };
        let mut value = serde_json::to_value(&outcome).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), json!(1));
        assert_eq!(
            serde_json::from_value::<BatchOutcome>(value).unwrap(),
            outcome
        );
    }

    #[test]
    fn the_outcome_does_not_expose_handles_or_secrets() {
        let outcome = BatchOutcome {
            project_epoch: EPOCH.to_string(),
            project_revision: 43,
            results: vec![BatchStepOutcome {
                object: object_summary(3, Some("立ち絵")),
                effect: Some(effect_info(3)),
            }],
        };
        let s = serde_json::to_string(&outcome).unwrap();
        for forbidden in ["handle", "pointer", "0x", "secret", "alias"] {
            assert!(
                !s.contains(forbidden),
                "応答に {forbidden} が現れています: {s}"
            );
        }
    }
}
