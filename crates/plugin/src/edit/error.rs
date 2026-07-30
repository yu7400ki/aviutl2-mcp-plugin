//! 編集の失敗を表す型と、応答へ載せる安全な補助情報。

use crate::read::ReadError;
use aviutl2_mcp_core::{ErrorCode, ItemWriteError};
use serde_json::{Map, Value, json};

/// 応答の補助情報へ載せる名前の上限文字数。
///
/// effect 名・設定項目名は要求元が指定を訂正するのに要るが、長さは要求元が
/// 決めるため、そのまま反響させると応答が膨らむ。
const MAX_NAME_CHARS: usize = 1_024;

/// 同じ要求をどう作り直せば通り得るか。
///
/// 再試行可否の真偽値だけでは「そのまま再送してよい」と「読み直して作り直す」を
/// 区別できない。前提条件の不整合は再試行可能だが、同じ selector と同じ前提を
/// そのまま送り直しても永久に失敗する。区別が無いと要求元は解消しない再試行へ
/// 入る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryRequires {
    /// そのまま再送してよい。
    Resend,
    /// 対象を読み直して要求を作り直す。
    Refetch,
    /// 再試行しても解消しない。
    None,
}

impl RetryRequires {
    /// 応答へ載せる機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            RetryRequires::Resend => "resend",
            RetryRequires::Refetch => "refetch",
            RetryRequires::None => "none",
        }
    }
}

/// 対象または SDK が変更に対応しない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// 登録されていない effect 名。
    EffectNotRegistered,
    /// 出力項目・音声 effect で有効・ロックを変更できない。
    EffectStateImmutable,
    /// SDK が対応しないメディアファイル。
    MediaNotSupported,
    /// 戻り値を持たない変更 API を呼んだが、読み直した状態が要求値と異なる。
    ///
    /// ホストが無言で拒否した場合にここへ来る。成功として返してはならない。
    ChangeNotApplied,
}

impl UnsupportedReason {
    /// 応答へ載せる機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            UnsupportedReason::EffectNotRegistered => "effect_not_registered",
            UnsupportedReason::EffectStateImmutable => "effect_state_immutable",
            UnsupportedReason::MediaNotSupported => "media_not_supported",
            UnsupportedReason::ChangeNotApplied => "change_not_applied",
        }
    }
}

impl std::fmt::Display for UnsupportedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            UnsupportedReason::EffectNotRegistered => "指定された effect は登録されていません",
            UnsupportedReason::EffectStateImmutable => {
                "対象 effect の有効・ロック状態は変更できません"
            }
            UnsupportedReason::MediaNotSupported => "対応していないメディアファイルです",
            UnsupportedReason::ChangeNotApplied => "要求した変更が反映されませんでした",
        };
        f.write_str(text)
    }
}

/// 変更 API が SDK へ届かずに失敗した理由。
///
/// SDK ラッパーは対象の存在確認・整数変換・NUL 検査を呼び出しの入口で行い、
/// これらに引っ掛かった要求は SDK を呼ばずに戻る。プロジェクトは一切変わって
/// いないため、変更の発行として記録してはならない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotIssuedReason {
    /// 対象がホスト側に存在しない。
    TargetMissing,
    /// 引数を SDK の型へ写せない。
    ArgumentNotRepresentable,
}

impl NotIssuedReason {
    /// 応答へ載せる機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            NotIssuedReason::TargetMissing => "target_missing",
            NotIssuedReason::ArgumentNotRepresentable => "argument_not_representable",
        }
    }
}

impl std::fmt::Display for NotIssuedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            NotIssuedReason::TargetMissing => "変更の対象が存在しません",
            NotIssuedReason::ArgumentNotRepresentable => "指定された値を受け渡せません",
        };
        f.write_str(text)
    }
}

/// 宛先を塞いでいる既存オブジェクトが占めるフレーム範囲。
///
/// 名前も fingerprint も持たない。要求元に要るのは「どこまで塞がっているか」
/// だけであり、他人の対象の同一性を渡す理由が無い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OccupiedRange {
    /// 塞いでいる対象の開始フレーム番号。
    pub frame_start: usize,
    /// 塞いでいる対象の終了フレーム番号。
    pub frame_end: usize,
}

/// 1 要求が epoch を 2 か所から受け取る場合の、食い違った側。
///
/// 出所を名乗るのは、要求が前提の epoch と focus 対象のセレクターの双方から
/// epoch を受け取る場合だけである。1 か所からしか受け取らない要求で出所を
/// 名乗ると、要求元は 1 つしか送っていない値に対して 2 つの分岐を持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochSource {
    /// 要求が前提として運ぶ epoch。
    Expected,
    /// focus 対象のセレクターが運ぶ epoch。
    Focus,
}

/// 前提条件のうち、どれが食い違ったか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mismatch {
    /// プロジェクトの epoch。出所は区別しない。
    ProjectEpoch,
    /// 前提として運ばれたプロジェクトの epoch。
    ExpectedProjectEpoch,
    /// focus 対象のセレクターが運ぶプロジェクトの epoch。
    FocusProjectEpoch,
    /// 現在シーンの ID。
    SceneId,
    /// fingerprint の算出方式。
    FingerprintAlgorithm,
    /// 対象の fingerprint。
    Fingerprint,
}

impl Mismatch {
    /// 応答へ載せる機械可読な名前。
    fn as_str(self) -> &'static str {
        match self {
            Mismatch::ProjectEpoch => "project_epoch",
            Mismatch::ExpectedProjectEpoch => "expected_project_epoch",
            Mismatch::FocusProjectEpoch => "focus_project_epoch",
            Mismatch::SceneId => "scene_id",
            Mismatch::FingerprintAlgorithm => "fingerprint_algorithm",
            Mismatch::Fingerprint => "fingerprint",
        }
    }
}

/// 編集の失敗。
///
/// 補助情報には SDK のハンドル・生ポインタ・設定値・alias・パスを含めない。
/// 含めるのは要求元が次の行動を決められる値だけである。
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// 受付判定・対象解決・read-back で生じた失敗。
    ///
    /// これらは編集区間の内側で行う読み取りであり、読み取り経路と同じ失敗分類を
    /// 持つ。別の列挙へ写し替えると、共有した解決実装の戻り値を機械的に変換する
    /// 層が要り、対応の取り違えを招く。
    #[error(transparent)]
    Read(#[from] ReadError),
    /// 作成・移動の宛先に既存オブジェクトがある。
    #[error("宛先に既存のオブジェクトがあります")]
    DestinationOccupied {
        /// 宛先のレイヤー番号。
        layer: usize,
        /// 宛先の開始フレーム番号。
        frame: usize,
        /// 宛先を塞いでいる既存オブジェクトが占める範囲。
        occupied_by: OccupiedRange,
    },
    /// epoch を 2 か所から受け取る要求で、どちらかが現在の epoch と異なる。
    ///
    /// 出所を区別しない食い違いは [`ReadError::EpochMismatch`] が表す。
    #[error("プロジェクトの epoch が要求の前提と一致しません")]
    EpochMismatch {
        /// 食い違った側。
        origin: EpochSource,
    },
    /// 対象または宛先のレイヤーがロックされている。
    #[error("レイヤーがロックされています")]
    LayerLocked {
        /// ロックされているレイヤー番号。
        layer: usize,
    },
    /// セレクターが指す effect が存在しない。
    #[error("セレクターに一致する effect がありません: {effect_name}")]
    EffectNotFound {
        /// 要求された effect 名。
        effect_name: String,
        /// 要求された同名 effect 内の位置。
        effect_index: usize,
    },
    /// 設定項目への書き込みを受け付けられない。
    #[error(transparent)]
    ItemWrite(ItemWriteError),
    /// 対象または SDK が変更に対応しない。
    #[error("{reason}")]
    UnsupportedTarget {
        /// 対応しない理由。
        reason: UnsupportedReason,
    },
    /// SDK の呼び出しが失敗した。
    #[error("SDK の呼び出しに失敗しました: {operation}")]
    Sdk {
        /// 失敗した SDK 関数の名前。
        operation: &'static str,
    },
    /// 変更 API が SDK へ届く前に失敗した。
    ///
    /// プロジェクトは変わっていないため、変更の発行として記録しない。失敗した
    /// SDK 関数名も載せない。呼ばれていない関数を名指しすると、要求元にも
    /// 運用者にも誤った手掛かりを与える。
    #[error("{reason}")]
    NotIssued {
        /// 届かなかった理由。
        reason: NotIssuedReason,
    },
    /// 編集区間の処理で panic を捕捉した。
    #[error("編集処理で panic を捕捉しました")]
    Panicked,
    /// 1 つの前提から 2 つ目の変更許可を取ろうとした。
    ///
    /// 許可はそれぞれ独立に発行を数えるため、2 つ取ると同じ前提のもとで
    /// revision が 2 度進み、応答が返す値がどちらの許可のものか定まらない。
    /// 呼び出し側の組み立ての誤りであり、同じ要求を作り直しても解消しない。
    #[error("1 つの前提から変更の許可を 2 度取ることはできません")]
    MutationPermitReissued,
    /// SDK の変更 API を発行した後に生じた失敗。
    ///
    /// エラーコードは失敗の理由を表すものを保ち、書き換えない。変更が入った
    /// という情報は補助情報の `mutation_issued` が担う。
    #[error("{source}")]
    AfterMutation {
        /// 発行後に生じた失敗そのもの。
        #[source]
        source: Box<EditError>,
        /// 加算後の revision。
        project_revision: u64,
    },
}

impl EditError {
    /// 変更 API を発行した後の失敗として包み直す。
    ///
    /// 既に包まれている場合は最初の revision を保つ。1 要求で revision が
    /// 進むのは 1 度だけであり、後から観測した値で上書きすると要求元へ返す
    /// 値が発行時点のものでなくなる。
    pub fn after_mutation(self, project_revision: u64) -> Self {
        match self {
            EditError::AfterMutation { .. } => self,
            source => EditError::AfterMutation {
                source: Box::new(source),
                project_revision,
            },
        }
    }

    /// 応答へ載せるエラーコードを返す。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            EditError::Read(error) => error.error_code(),
            EditError::DestinationOccupied { .. }
            | EditError::LayerLocked { .. }
            | EditError::EpochMismatch { .. } => ErrorCode::PreconditionFailed,
            EditError::EffectNotFound { .. } => ErrorCode::NotFound,
            EditError::ItemWrite(error) => error.error_code(),
            EditError::UnsupportedTarget { .. } => ErrorCode::UnsupportedOperation,
            EditError::Sdk { .. } => ErrorCode::SdkError,
            EditError::NotIssued { reason } => match reason {
                NotIssuedReason::TargetMissing => ErrorCode::NotFound,
                NotIssuedReason::ArgumentNotRepresentable => ErrorCode::InvalidArgument,
            },
            EditError::Panicked | EditError::MutationPermitReissued => ErrorCode::InternalError,
            EditError::AfterMutation { source, .. } => source.error_code(),
        }
    }

    /// 同じ要求をそのまま再送して成功し得るか。
    pub fn retryable(&self) -> bool {
        self.error_code().default_retryable()
    }

    /// 前提条件のうち食い違ったものを返す。前提条件以外の失敗では `None`。
    fn mismatch(&self) -> Option<Mismatch> {
        match self {
            EditError::Read(ReadError::EpochMismatch) => Some(Mismatch::ProjectEpoch),
            EditError::EpochMismatch { origin } => Some(match origin {
                EpochSource::Expected => Mismatch::ExpectedProjectEpoch,
                EpochSource::Focus => Mismatch::FocusProjectEpoch,
            }),
            EditError::Read(ReadError::SceneMismatch { .. }) => Some(Mismatch::SceneId),
            EditError::Read(ReadError::FingerprintAlgorithmMismatch { .. }) => {
                Some(Mismatch::FingerprintAlgorithm)
            }
            EditError::Read(ReadError::FingerprintMismatch) => Some(Mismatch::Fingerprint),
            EditError::AfterMutation { source, .. } => source.mismatch(),
            _ => None,
        }
    }

    /// 要求元が取るべき再試行のしかたを返す。
    fn retry_requires(&self) -> RetryRequires {
        match self {
            // 変更が入った可能性がある以上、そのまま再送してよい状況は無い。
            EditError::AfterMutation { .. } => RetryRequires::Refetch,
            EditError::Read(ReadError::NotReady)
            | EditError::Read(ReadError::EditBlocked { .. }) => RetryRequires::Resend,
            // 読み直せば宛先の空きが分かる。
            EditError::DestinationOccupied { .. } => RetryRequires::Refetch,
            // ロックの解除は別の operation であり、同じ要求を送り直しても対象を
            // 読み直しても解消しない。3 値のどれにも当たらないため、値を増やさず
            // 「再試行では解消しない」とする。次に取るべき操作の案内は本文が担う。
            EditError::LayerLocked { .. } => RetryRequires::None,
            other => match other.error_code() {
                ErrorCode::PreconditionFailed => RetryRequires::Refetch,
                _ => RetryRequires::None,
            },
        }
    }

    /// 応答へ載せる補助情報を組み立てる。
    ///
    /// 設定値・alias・パスは含めない。effect 名と設定項目名は登録済みの識別子で
    /// あり要求の訂正に要るため、長さを切り詰めた上で載せる。
    pub fn details(&self) -> Value {
        let mut details = Map::new();
        self.fill_details(&mut details);
        if let Some(mismatch) = self.mismatch() {
            details.insert("mismatch".to_string(), json!(mismatch.as_str()));
        }
        details.insert(
            "retry_requires".to_string(),
            json!(self.retry_requires().as_str()),
        );
        Value::Object(details)
    }

    /// 失敗の種類ごとの補助情報を書き込む。
    fn fill_details(&self, details: &mut Map<String, Value>) {
        match self {
            EditError::Read(error) => merge(details, error.details()),
            EditError::DestinationOccupied {
                layer,
                frame,
                occupied_by,
            } => {
                details.insert("reason".to_string(), json!("destination_occupied"));
                details.insert("layer".to_string(), json!(layer));
                details.insert("frame".to_string(), json!(frame));
                details.insert(
                    "occupied_by".to_string(),
                    json!({
                        "frame_start": occupied_by.frame_start,
                        "frame_end": occupied_by.frame_end,
                    }),
                );
            }
            EditError::LayerLocked { layer } => {
                details.insert("reason".to_string(), json!("layer_locked"));
                details.insert("layer".to_string(), json!(layer));
            }
            // 食い違った側は `mismatch` が名乗る。
            EditError::EpochMismatch { .. } => {}
            EditError::EffectNotFound {
                effect_name,
                effect_index,
            } => {
                details.insert("effect_name".to_string(), json!(truncate(effect_name)));
                details.insert("effect_index".to_string(), json!(effect_index));
            }
            EditError::ItemWrite(error) => fill_item_write_details(details, error),
            EditError::UnsupportedTarget { reason } => {
                details.insert("reason".to_string(), json!(reason.as_str()));
            }
            EditError::Sdk { operation } => {
                details.insert("sdk_operation".to_string(), json!(operation));
            }
            EditError::NotIssued { reason } => {
                details.insert("reason".to_string(), json!(reason.as_str()));
            }
            EditError::Panicked | EditError::MutationPermitReissued => {}
            EditError::AfterMutation {
                source,
                project_revision,
            } => {
                source.fill_details(details);
                details.insert("mutation_issued".to_string(), json!(true));
                details.insert(
                    "current_project_revision".to_string(),
                    json!(project_revision),
                );
            }
        }
    }
}

/// 設定項目への書き込み失敗の補助情報を書き込む。
///
/// 載せるのは項目名と、書き込みを公開しない種別であることだけである。値そのもの
/// と、種別の照合に用いた表記は要求元の内容であり、応答へ反響させない。
fn fill_item_write_details(details: &mut Map<String, Value>, error: &ItemWriteError) {
    match error {
        ItemWriteError::ItemNotFound { item } => {
            details.insert("item".to_string(), json!(truncate(item)));
        }
        ItemWriteError::UnsupportedItemType { .. } => {
            details.insert("reason".to_string(), json!("item_type_not_writable"));
        }
        ItemWriteError::UnknownValue
        | ItemWriteError::ValueKindMismatch { .. }
        | ItemWriteError::Text(_)
        | ItemWriteError::Path(_) => {}
    }
}

/// 別に組み立てた補助情報を取り込む。
fn merge(details: &mut Map<String, Value>, source: Value) {
    if let Value::Object(source) = source {
        details.extend(source);
    }
}

/// 名前を応答へ載せられる長さへ切り詰める。
fn truncate(name: &str) -> String {
    name.chars().take(MAX_NAME_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read::EditState;
    use aviutl2_mcp_core::{EffectItemType, PathSyntaxError, TextSyntaxError};

    /// 全 variant の代表値。新しい variant を足したらここへも足す。
    fn all_errors() -> Vec<EditError> {
        vec![
            EditError::Read(ReadError::NotReady),
            EditError::Read(ReadError::EditBlocked {
                state: EditState::Preview,
            }),
            EditError::Read(ReadError::SceneMismatch {
                expected: 0,
                current: 3,
            }),
            EditError::Read(ReadError::EpochMismatch),
            EditError::Read(ReadError::FingerprintAlgorithmMismatch {
                requested: "sha256-future-v9".to_string(),
                supported: "sha256-raw-v1".to_string(),
            }),
            EditError::Read(ReadError::FingerprintMismatch),
            EditError::Read(ReadError::ObjectNotFound {
                detected_by: "find_object",
            }),
            EditError::Read(ReadError::AmbiguousObject { candidate_count: 2 }),
            EditError::Read(ReadError::Sdk {
                operation: "get_object_alias",
            }),
            EditError::Read(ReadError::Panicked),
            EditError::DestinationOccupied {
                layer: 3,
                frame: 240,
                occupied_by: OccupiedRange {
                    frame_start: 200,
                    frame_end: 260,
                },
            },
            EditError::EpochMismatch {
                origin: EpochSource::Expected,
            },
            EditError::EpochMismatch {
                origin: EpochSource::Focus,
            },
            EditError::LayerLocked { layer: 3 },
            EditError::EffectNotFound {
                effect_name: "ぼかし".to_string(),
                effect_index: 1,
            },
            EditError::ItemWrite(ItemWriteError::ItemNotFound {
                item: "範囲".to_string(),
            }),
            EditError::ItemWrite(ItemWriteError::UnsupportedItemType {
                item_type: EffectItemType::Figure.kind_name(),
            }),
            EditError::ItemWrite(ItemWriteError::UnknownValue),
            EditError::ItemWrite(ItemWriteError::ValueKindMismatch {
                item_type: EffectItemType::Integer.kind_name(),
                value_kind: "text",
            }),
            EditError::ItemWrite(ItemWriteError::Text(TextSyntaxError::ContainsNul)),
            EditError::ItemWrite(ItemWriteError::Path(PathSyntaxError::NotAbsolute)),
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::EffectNotRegistered,
            },
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::EffectStateImmutable,
            },
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::MediaNotSupported,
            },
            EditError::UnsupportedTarget {
                reason: UnsupportedReason::ChangeNotApplied,
            },
            EditError::NotIssued {
                reason: NotIssuedReason::TargetMissing,
            },
            EditError::NotIssued {
                reason: NotIssuedReason::ArgumentNotRepresentable,
            },
            EditError::Sdk {
                operation: "create_effect",
            },
            EditError::Panicked,
            EditError::MutationPermitReissued,
            EditError::Sdk {
                operation: "create_effect",
            }
            .after_mutation(44),
        ]
    }

    /// variant を表す名前を返す。
    ///
    /// 網羅 match で書く。variant を足すとここがコンパイルエラーになり、すぐ下の
    /// 一覧と [`all_errors`] へ足す必要があることが分かる。名前だけを返すのは、
    /// 代表値の作り方が variant ごとに違うためである。
    fn variant_name(error: &EditError) -> &'static str {
        match error {
            EditError::Read(_) => "Read",
            EditError::DestinationOccupied { .. } => "DestinationOccupied",
            EditError::EpochMismatch { .. } => "EpochMismatch",
            EditError::LayerLocked { .. } => "LayerLocked",
            EditError::EffectNotFound { .. } => "EffectNotFound",
            EditError::ItemWrite(_) => "ItemWrite",
            EditError::UnsupportedTarget { .. } => "UnsupportedTarget",
            EditError::Sdk { .. } => "Sdk",
            EditError::NotIssued { .. } => "NotIssued",
            EditError::Panicked => "Panicked",
            EditError::MutationPermitReissued => "MutationPermitReissued",
            EditError::AfterMutation { .. } => "AfterMutation",
        }
    }

    #[test]
    fn all_errors_covers_every_variant() {
        // 代表値の一覧は手書きであり、足し忘れても他のテストは緑のまま通る。
        // 落ちたものは応答コードも再試行の案内も許可キーも守られない。
        const VARIANTS: &[&str] = &[
            "Read",
            "DestinationOccupied",
            "EpochMismatch",
            "LayerLocked",
            "EffectNotFound",
            "ItemWrite",
            "UnsupportedTarget",
            "Sdk",
            "NotIssued",
            "Panicked",
            "MutationPermitReissued",
            "AfterMutation",
        ];
        let covered: Vec<&str> = all_errors().iter().map(variant_name).collect();
        for variant in VARIANTS {
            assert!(
                covered.contains(variant),
                "{variant} の代表値が一覧にありません"
            );
        }
        // 逆向きも見る。variant を消したときに VARIANTS だけが取り残されると、
        // 一覧は無くなった variant を数え続け、残りの網羅を主張できなくなる。
        for variant in &covered {
            assert!(
                VARIANTS.contains(variant),
                "{variant} が網羅すべき variant の一覧にありません"
            );
        }
    }

    #[test]
    fn all_errors_covers_every_reason() {
        // 理由ごとに応答の補助情報が変わるため、variant を 1 つ挙げるだけでは
        // 足りない。網羅 match を添えて、理由を足したときに気付ける形にする。
        let unsupported = [
            UnsupportedReason::EffectNotRegistered,
            UnsupportedReason::EffectStateImmutable,
            UnsupportedReason::MediaNotSupported,
            UnsupportedReason::ChangeNotApplied,
        ];
        for reason in unsupported {
            match reason {
                UnsupportedReason::EffectNotRegistered
                | UnsupportedReason::EffectStateImmutable
                | UnsupportedReason::MediaNotSupported
                | UnsupportedReason::ChangeNotApplied => {}
            }
        }
        let not_issued = [
            NotIssuedReason::TargetMissing,
            NotIssuedReason::ArgumentNotRepresentable,
        ];
        for reason in not_issued {
            match reason {
                NotIssuedReason::TargetMissing | NotIssuedReason::ArgumentNotRepresentable => {}
            }
        }

        let reasons: Vec<String> = all_errors()
            .iter()
            .filter_map(|error| {
                error
                    .details()
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        for expected in unsupported
            .iter()
            .map(|reason| reason.as_str())
            .chain(not_issued.iter().map(|reason| reason.as_str()))
        {
            assert!(
                reasons.iter().any(|reason| reason == expected),
                "{expected} の代表値が一覧にありません"
            );
        }
    }

    #[test]
    fn error_codes_match_the_edit_mapping() {
        let mapped: Vec<ErrorCode> = all_errors().iter().map(EditError::error_code).collect();
        assert_eq!(
            mapped,
            vec![
                ErrorCode::HostBusy,
                ErrorCode::EditBlocked,
                ErrorCode::PreconditionFailed,
                ErrorCode::PreconditionFailed,
                ErrorCode::PreconditionFailed,
                ErrorCode::PreconditionFailed,
                ErrorCode::NotFound,
                ErrorCode::AmbiguousSelector,
                ErrorCode::SdkError,
                ErrorCode::InternalError,
                ErrorCode::PreconditionFailed,
                // 前提の epoch と focus の epoch を別々に名乗る 2 つ。
                ErrorCode::PreconditionFailed,
                ErrorCode::PreconditionFailed,
                ErrorCode::PreconditionFailed,
                ErrorCode::NotFound,
                ErrorCode::NotFound,
                ErrorCode::UnsupportedOperation,
                ErrorCode::InvalidArgument,
                ErrorCode::InvalidArgument,
                ErrorCode::InvalidArgument,
                ErrorCode::InvalidArgument,
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                ErrorCode::UnsupportedOperation,
                // 対象が失われていた。要求の対象が無いのだから見つからない。
                ErrorCode::NotFound,
                // 引数を写せなかった。SDK の失敗ではなく要求の誤りである。
                ErrorCode::InvalidArgument,
                ErrorCode::SdkError,
                ErrorCode::InternalError,
                // 前提の作り方の誤りであり、要求を作り直しても解消しない。
                ErrorCode::InternalError,
                ErrorCode::SdkError,
            ]
        );
    }

    #[test]
    fn cancelled_is_never_produced() {
        for error in all_errors() {
            assert_ne!(
                error.error_code(),
                ErrorCode::Cancelled,
                "{error} が cancelled になりました"
            );
        }
    }

    #[test]
    fn precondition_failures_ask_for_a_refetch_unless_a_reread_cannot_help() {
        for error in all_errors() {
            if error.error_code() != ErrorCode::PreconditionFailed {
                continue;
            }
            assert!(error.retryable(), "{error} が再試行不可になりました");
            let details = error.details();
            // ロックの解除は別の operation である。読み直しを案内すると、
            // 要求元は解消しない読み直しを繰り返す。
            let expected = if details["reason"] == json!("layer_locked") {
                "none"
            } else {
                "refetch"
            };
            assert_eq!(
                details["retry_requires"],
                json!(expected),
                "{error} が案内する再試行のしかたが想定と異なります"
            );
        }
    }

    #[test]
    fn an_occupied_destination_reports_how_far_it_is_blocked() {
        // 塞いでいる範囲を返さないと、要求元は次の宛先を選ぶために読み直す
        // ほかない。走査は事前確認で済んでおり、追加の呼び出しは要らない。
        let error = EditError::DestinationOccupied {
            layer: 3,
            frame: 240,
            occupied_by: OccupiedRange {
                frame_start: 200,
                frame_end: 260,
            },
        };
        let details = error.details();
        assert_eq!(
            details["occupied_by"],
            json!({"frame_start": 200, "frame_end": 260})
        );
        // 塞いでいる対象の同一性は渡さない。要求元に要るのは範囲だけである。
        let mut keys: Vec<&str> = details["occupied_by"]
            .as_object()
            .expect("occupied_by がオブジェクトではありません")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["frame_end", "frame_start"]);
    }

    #[test]
    fn an_occupied_destination_still_asks_for_a_refetch() {
        // 宛先の空きは読み直せば分かる。ロックと同じ扱いにはしない。
        let error = EditError::DestinationOccupied {
            layer: 3,
            frame: 240,
            occupied_by: OccupiedRange {
                frame_start: 200,
                frame_end: 260,
            },
        };
        assert_eq!(error.details()["retry_requires"], json!("refetch"));
    }

    #[test]
    fn transient_states_allow_a_plain_resend() {
        assert_eq!(
            EditError::Read(ReadError::NotReady).details()["retry_requires"],
            json!("resend")
        );
        assert_eq!(
            EditError::Read(ReadError::EditBlocked {
                state: EditState::Save
            })
            .details()["retry_requires"],
            json!("resend")
        );
    }

    #[test]
    fn precondition_failures_name_the_failing_check() {
        let expected = [
            (EditError::Read(ReadError::EpochMismatch), "project_epoch"),
            (
                EditError::Read(ReadError::SceneMismatch {
                    expected: 0,
                    current: 1,
                }),
                "scene_id",
            ),
            (
                EditError::Read(ReadError::FingerprintAlgorithmMismatch {
                    requested: "x".to_string(),
                    supported: "y".to_string(),
                }),
                "fingerprint_algorithm",
            ),
            (
                EditError::Read(ReadError::FingerprintMismatch),
                "fingerprint",
            ),
        ];
        for (error, mismatch) in expected {
            assert_eq!(error.details()["mismatch"], json!(mismatch), "{error}");
        }
    }

    #[test]
    fn no_failure_names_the_revision_as_the_failing_check() {
        // revision は照合しない。食い違いとして名乗る失敗が現れれば、照合が
        // 戻ったか、要求元へ訂正できない再送を促していることになる。
        for error in all_errors() {
            assert_ne!(
                error.details().get("mismatch"),
                Some(&json!("project_revision")),
                "{error} が revision の食い違いを名乗りました"
            );
        }
    }

    #[test]
    fn failures_outside_preconditions_do_not_name_a_mismatch() {
        for error in all_errors() {
            if error.error_code() == ErrorCode::PreconditionFailed {
                continue;
            }
            assert!(
                error.details().get("mismatch").is_none(),
                "{error} が前提条件の食い違いを名乗りました"
            );
        }
    }

    #[test]
    fn failures_after_a_mutation_keep_the_original_code() {
        let error = EditError::Sdk {
            operation: "get_effect_list",
        }
        .after_mutation(44);
        assert_eq!(error.error_code(), ErrorCode::SdkError);
        let details = error.details();
        assert_eq!(details["mutation_issued"], json!(true));
        assert_eq!(details["current_project_revision"], json!(44));
        assert_eq!(details["retry_requires"], json!("refetch"));
        assert_eq!(details["sdk_operation"], json!("get_effect_list"));
    }

    #[test]
    fn wrapping_twice_keeps_the_revision_of_the_first_issue() {
        let error = EditError::Panicked.after_mutation(44).after_mutation(99);
        assert_eq!(error.details()["current_project_revision"], json!(44));
    }

    #[test]
    fn details_only_use_allowed_keys() {
        // 補助情報のキーはここで列挙したものに限る。新しいキーを足す際は
        // ハンドル・生ポインタ・設定値・alias・パスでないことを確かめる。
        const ALLOWED: &[&str] = &[
            "retry_after_ms",
            "edit_state",
            "expected_scene_id",
            "current_scene_id",
            "current_project_revision",
            "mismatch",
            "requested_fingerprint_algorithm",
            "supported_fingerprint_algorithm",
            "candidate_count",
            "reason",
            "layer",
            "frame",
            "occupied_by",
            "effect_name",
            "effect_index",
            "item",
            "sdk_operation",
            "retry_requires",
            "mutation_issued",
            "change_applied",
            "mutation_origin",
        ];
        for error in all_errors() {
            let details = error.details();
            let object = details
                .as_object()
                .unwrap_or_else(|| panic!("{error} の補助情報がオブジェクトではありません"));
            for key in object.keys() {
                assert!(
                    ALLOWED.contains(&key.as_str()),
                    "{error} の補助情報に未許可のキー {key} が含まれています"
                );
            }
        }
    }

    #[test]
    fn details_and_messages_do_not_expose_values_or_pointers() {
        // 設定値・alias・パスを含む失敗を作り、応答へ現れないことを確かめる。
        let secrets = [
            EditError::ItemWrite(ItemWriteError::Text(TextSyntaxError::TooLongBytes {
                bytes: 9_000,
                max: 8_192,
            })),
            EditError::ItemWrite(ItemWriteError::Path(PathSyntaxError::DeviceNamespace)),
            EditError::ItemWrite(ItemWriteError::ValueKindMismatch {
                item_type: EffectItemType::Integer.kind_name(),
                value_kind: "text",
            }),
        ];
        for error in all_errors().into_iter().chain(secrets) {
            let text = format!("{} {}", error, error.details());
            assert!(!text.contains("0x"), "{text}");
            assert!(!text.to_lowercase().contains("handle"), "{text}");
            assert!(!text.to_lowercase().contains("pointer"), "{text}");
            assert!(!text.contains(r"C:\"), "{text}");
        }
    }

    #[test]
    fn names_are_truncated_before_they_reach_the_response() {
        let error = EditError::EffectNotFound {
            effect_name: "あ".repeat(MAX_NAME_CHARS * 2),
            effect_index: 0,
        };
        let details = error.details();
        let name = details["effect_name"].as_str().unwrap();
        assert_eq!(name.chars().count(), MAX_NAME_CHARS);
    }
}
