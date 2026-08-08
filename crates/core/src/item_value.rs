//! effect 設定項目の値と、書き込み時の検証。

use crate::effect::{AvailableEffectItem, EffectItemType, EvaluatedItemKind};
use crate::error::ErrorCode;
use crate::number::FiniteF64;
use crate::text_codec::encode_host_text;
use crate::track_value::{
    TrackValue, TrackValueError, TrackWriteTarget, decode_track_value, encode_track_value,
    track_read_back_matches, validate_track_syntax,
};
use crate::validation::{
    PathSyntaxError, TextSyntaxError, limit_item_value_bytes, validate_item_text,
    validate_multiline_item_text, validate_path,
};
use serde::{Deserialize, Serialize};

/// effect 設定項目の値。
///
/// 種別ごとに異なる形を持つため `type` を判別子とする tagged union で表す。
/// 読み取りでは未対応種別も破棄せず [`ItemValue::Unknown`] として生文字列を保持する。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ItemValue {
    /// 実数。
    #[serde(rename = "number")]
    Number {
        /// 値。
        value: FiniteF64,
    },
    /// 整数。
    #[serde(rename = "integer")]
    Integer {
        /// 値。
        value: i64,
    },
    /// 真偽値。
    #[serde(rename = "bool")]
    Bool {
        /// 値。
        value: bool,
    },
    /// 正規化済みの色表現。
    #[serde(rename = "color")]
    Color {
        /// 値。
        value: String,
    },
    /// 一覧からの選択。
    #[serde(rename = "choice")]
    Choice {
        /// 選択された表示文字列。
        value: String,
    },
    /// ファイルパス。
    #[serde(rename = "file")]
    File {
        /// パス。
        path: String,
    },
    /// フォルダパス。
    #[serde(rename = "folder")]
    Folder {
        /// パス。
        path: String,
    },
    /// フォント名。
    #[serde(rename = "font")]
    Font {
        /// フォント名。
        name: String,
    },
    /// テキスト。
    #[serde(rename = "text")]
    Text {
        /// 値。
        value: String,
    },
    /// トラックバーの移動を含む値。
    ///
    /// **静的なトラックバーは [`ItemValue::Number`] のままである。** 読み取りが
    /// 全トラックバーをこの形で返すと、`X` も `拡大率` も `透明度` も応答が
    /// 膨らむ一方で、移動を持つ項目は一部でしかない。書き込みは両方を受け付け、
    /// 数値を書けば静的になり、この形を書けば移動が入る。
    #[serde(rename = "track")]
    Track(TrackValue),
    /// 未対応種別の生値。
    #[serde(rename = "unknown")]
    Unknown {
        /// 生文字列。
        raw: String,
    },
}

impl ItemValue {
    /// [`ItemValue::Unknown`] が名乗る値の形。
    ///
    /// 未対応種別の生値は設定項目の種別を引く前に拒否されるため、失敗が運べる
    /// のは値の形だけである。名乗る名前を 2 か所に書かないための定数である。
    pub const UNKNOWN_KIND: &'static str = "unknown";

    /// 値の形を表す名前を返す。JSON の判別子と同じ表記である。
    ///
    /// 値そのものを含まないため、エラー応答へ載せてよい。
    pub fn kind(&self) -> &'static str {
        match self {
            ItemValue::Number { .. } => "number",
            ItemValue::Integer { .. } => "integer",
            ItemValue::Bool { .. } => "bool",
            ItemValue::Color { .. } => "color",
            ItemValue::Choice { .. } => "choice",
            ItemValue::File { .. } => "file",
            ItemValue::Folder { .. } => "folder",
            ItemValue::Font { .. } => "font",
            ItemValue::Text { .. } => "text",
            ItemValue::Track(_) => "track",
            ItemValue::Unknown { .. } => ItemValue::UNKNOWN_KIND,
        }
    }
}

/// 設定項目への書き込みの検証失敗。
///
/// 要求を直せば通るもの（`invalid_argument` 相当）と、対象が対応しないため
/// 直しても通らないもの（`unsupported_operation` 相当）を別の variant で表す。
/// 対応は [`ItemWriteError::error_code`] が持つ。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ItemWriteError {
    /// 未対応種別の生値は書き込めない。
    #[error("未対応種別の値は書き込めません")]
    UnknownValue,
    /// 指定された設定項目が対象 effect に存在しない。
    #[error("設定項目が存在しません: {item}")]
    ItemNotFound {
        /// 要求された設定項目名。
        item: String,
    },
    /// 設定項目の種別と値の形が対応しない。
    #[error("種別 {item_type} の設定項目に {value_kind} の値は指定できません")]
    ValueKindMismatch {
        /// 設定項目の種別名。
        item_type: String,
        /// 与えられた値の形。
        value_kind: &'static str,
    },
    /// 書き込みを公開していない種別。
    #[error("種別 {item_type} の設定項目への書き込みには対応していません")]
    UnsupportedItemType {
        /// 設定項目の種別名。
        item_type: String,
    },
    /// 文字列値の検証に失敗した。
    #[error(transparent)]
    Text(#[from] TextSyntaxError),
    /// パス値の検証に失敗した。
    #[error(transparent)]
    Path(#[from] PathSyntaxError),
    /// トラックバーの移動の検証に失敗した。
    #[error(transparent)]
    Track(#[from] TrackValueError),
}

impl ItemWriteError {
    /// 全 variant の代表値。
    ///
    /// [`ItemWriteError::reason`] が返し得る名前を数え上げるために用いる。
    /// `const` にできないのは、値を持つ variant が所有文字列を含むためである。
    /// 構文検証を包む variant は、包む側の全種別を並べる。
    pub fn all() -> Vec<ItemWriteError> {
        let mut all = vec![
            ItemWriteError::UnknownValue,
            ItemWriteError::ItemNotFound {
                item: "範囲".to_string(),
            },
            ItemWriteError::ValueKindMismatch {
                item_type: "integer".to_string(),
                value_kind: "text",
            },
            ItemWriteError::UnsupportedItemType {
                item_type: "scene".to_string(),
            },
        ];
        all.extend(
            TextSyntaxError::ALL
                .iter()
                .copied()
                .map(ItemWriteError::Text),
        );
        all.extend(
            PathSyntaxError::ALL
                .iter()
                .copied()
                .map(ItemWriteError::Path),
        );
        all.extend(
            TrackValueError::ALL
                .iter()
                .cloned()
                .map(ItemWriteError::Track),
        );
        all
    }

    /// 失敗の種別を表す機械可読な名前を返す。名前を持たない失敗では `None`。
    ///
    /// 名前は種別だけを表し、書き込もうとした値・パス・設定項目名を含まない。
    /// 値の形が種別と対応しないことと未対応種別の生値は、種別名と値の形を
    /// 別のキーで返せるため名前を持たない。
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            ItemWriteError::UnsupportedItemType { .. } => Some("item_type_not_writable"),
            ItemWriteError::Text(error) => Some(error.reason()),
            ItemWriteError::Path(error) => Some(error.reason()),
            ItemWriteError::Track(error) => Some(error.reason()),
            ItemWriteError::UnknownValue
            | ItemWriteError::ItemNotFound { .. }
            | ItemWriteError::ValueKindMismatch { .. } => None,
        }
    }

    /// 対応するエラーコードを返す。
    pub fn error_code(&self) -> ErrorCode {
        match self {
            ItemWriteError::ItemNotFound { .. } => ErrorCode::NotFound,
            ItemWriteError::UnsupportedItemType { .. } => ErrorCode::UnsupportedOperation,
            ItemWriteError::UnknownValue
            | ItemWriteError::ValueKindMismatch { .. }
            | ItemWriteError::Text(_)
            | ItemWriteError::Path(_)
            | ItemWriteError::Track(_) => ErrorCode::InvalidArgument,
        }
    }
}

/// 書き込む前に対象の現在値を読む必要があるか。
///
/// 読むのは、**移動を持ち得る種別へ移動を含まない値を書くときだけ**である。
/// 移動の有無は設定値の生文字列にしか現れず、他の組み合わせではその文字列を
/// 見ても答えが変わらない。
///
/// - 移動を持ち得ない種別（[`EvaluatedItemKind::Track`] として評価されない種別）
///   には移動が無い。数値を書いても消える移動が無く、移動そのものは種別と値の形
///   の照合が拒む（[`prepare_item_write`]）
/// - [`ItemValue::Track`] を書く要求は、対象がいま移動を持つかによらず通す。
///   **移動を持たないトラックバーへ移動を付ける経路がこれである**
///
/// **これは費用の判断であると同時に、拒否の条件の一部でもある。**
/// [`write_drops_existing_movement`] はこの述語をそのまま用いており、両者は
/// 同じ条件の一部と全体の関係にある。**別々に書かない**——ずれれば、読まない
/// 組み合わせが拒否されるか、読んだのに拒否されないかのどちらかになる。
/// ここを変えると拒否の範囲まで変わるため、変えるときは両方を見ることになる。
///
/// **評価の種別を引き当てて判定する。** 種別の名前を並べ直すと、移動を書ける
/// 種別と現状を確かめる種別が食い違い、確かめない側から移動が消える。
pub fn movement_check_reads_current_value(item_type: &EffectItemType, value: &ItemValue) -> bool {
    item_type.evaluated_kind() == Some(EvaluatedItemKind::Track)
        && !matches!(value, ItemValue::Track(_))
}

/// 対象がいま持つ移動を、この書き込みが消すか。
///
/// **ホストは失敗として返さない。** 移動を持つ項目へ数値を書くと移動も加速も
/// 中間点無視も消え、`success` が返る。**破棄しているのはホストであり、生の
/// 文字列を渡しても同じことが起きる。** 止められる場所は要求を受け付ける側
/// しか無い。
///
/// 移動を消したい要求は表せる——`mode` が `null` の [`ItemValue::Track`] が
/// それである。**表せるのに黙って消すことを、成功として返さない。**
///
/// `current` はホストが返した生の文字列である。移動の有無はその文字列にしか
/// 現れないため、対象を読まずには判定できない。
///
/// **答えが真になり得る組み合わせは、現在値を読む組み合わせと同じである。**
/// したがって [`movement_check_reads_current_value`] をそのまま前段に置く。
/// 同じ条件を 2 通りに書けば、片方だけを直したときに読まない組み合わせを拒否
/// できる形ができる。**現在値だけでは拒否の向きは決まらない**——同じ
/// `current` に対して、数値は拒否され、移動は通る。
pub fn write_drops_existing_movement(
    item_type: &EffectItemType,
    value: &ItemValue,
    current: &str,
) -> bool {
    movement_check_reads_current_value(item_type, value)
        && decode_track_value(current).is_some_and(|track| track.mode.is_some())
}

/// 書き込んだ直後に読み直した文字列をどう扱うか。
///
/// **真偽値では「照合しない」と「照合して一致した」を同じ値で表してしまう。**
/// 照合しない種別を追加したときに、それが成功と見分けられなくなる。2 つの状態を
/// 別の variant に置き、比較の規則は照合する側だけが持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadBackCheck {
    /// 種別ごとの比較で照合する。
    Compare(ReadBackComparison),
    /// 照合しない。
    Declared {
        /// 照合しない理由。
        reason: ReadBackNotVerified,
    },
}

/// 読み直した文字列と、SDK へ渡した文字列の比べ方。
///
/// ホストは種別ごとに表記を整える。整えた結果は要求した値そのものであり、
/// バイト列の完全一致を全種別へ課すと、正しい書き込みが失敗として返る。
///
/// **数値の比較に許容誤差を置かない。** 誤差を許すと、値域への切り詰めと小数
/// 桁への丸めを一致として見逃す。`100` と `100.00` が等しいのは、どちらも同じ
/// `f64` へ解釈されるからであって、近いからではない。
///
/// トラックバーの移動も同じ理由で生の文字列では比べられない。復号して構造として
/// 比べる（[`track_read_back_matches`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadBackComparison {
    /// 文字列として完全に一致するか。
    Exact,
    /// 十進表記を数値として解釈して一致するか。
    Numeric,
    /// 真偽値として解釈して一致するか。
    Boolean,
    /// 大文字小文字を無視して一致するか。
    IgnoreAsciiCase,
    /// トラックバーの移動として解釈して一致するか。
    Track,
}

impl ReadBackComparison {
    /// SDK へ渡した文字列と読み直した文字列が、この規則で一致するか。
    ///
    /// 数値・真偽値は、どちらか一方でも解釈できなければ一致としない。解釈でき
    /// ない文字列は要求した値を得られたことを示さないためである。
    pub fn matches(self, written: &str, observed: &str) -> bool {
        match self {
            ReadBackComparison::Exact => written == observed,
            ReadBackComparison::IgnoreAsciiCase => written.eq_ignore_ascii_case(observed),
            ReadBackComparison::Numeric => {
                match (parse_number_value(written), parse_number_value(observed)) {
                    (Some(written), Some(observed)) => written == observed,
                    _ => false,
                }
            }
            ReadBackComparison::Boolean => {
                match (parse_check_value(written), parse_check_value(observed)) {
                    (Some(written), Some(observed)) => written == observed,
                    _ => false,
                }
            }
            ReadBackComparison::Track => {
                match (decode_track_value(written), decode_track_value(observed)) {
                    (Some(written), Some(observed)) => track_read_back_matches(&written, &observed),
                    _ => false,
                }
            }
        }
    }
}

/// 書き込んだ値を読み直して照合しない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadBackNotVerified {
    /// 書き込みを公開していない種別。
    ///
    /// 書き込む文字列を組み立てる前に [`encode_item_value`] が拒否するため、
    /// この理由を持つ [`ItemWrite`] は生まれない。それでも腕として書くのは、
    /// 種別を足したときに照合の規則を決めないまま既定へ落ちることを防ぐためで
    /// ある。公開の可否を後から変えるなら、ここも合わせて書き換えることになる。
    ItemTypeNotWritable,
}

/// チェックボックスの生文字列を真偽値として解釈する。
///
/// ホストが返す表記と書き込みが渡す表記の双方を受ける。解釈できない文字列では
/// `None` を返す。
pub fn parse_check_value(raw: &str) -> Option<bool> {
    match raw.trim() {
        "0" | "false" => Some(false),
        "1" | "true" => Some(true),
        _ => None,
    }
}

/// 数値の生文字列を有限な実数として解釈する。
fn parse_number_value(raw: &str) -> Option<f64> {
    raw.trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

/// 設定項目へ書き込む文字列と、書き込み後の照合のしかた。
///
/// 照合する文字列は SDK へ渡すもの**そのもの**である。要求に現れた値を別に
/// 保持しないため、照合の材料が要求側の値へすり替わる余地が無い。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemWrite {
    value: String,
    read_back: ReadBackCheck,
}

impl ItemWrite {
    /// SDK へ渡す文字列。
    pub fn value(&self) -> &str {
        &self.value
    }

    /// 書き込んだ直後の読み直しをどう扱うか。
    ///
    /// 要求内容ではなく、対象 effect が公開する種別だけで決まる。
    pub fn read_back(&self) -> ReadBackCheck {
        self.read_back
    }

    /// 読み直した文字列が、SDK へ渡した文字列と一致するか。照合しない種別では
    /// `None` を返す。
    ///
    /// 比較の規則も比較する文字列も、この [`ItemWrite`] が自分で持つものだけを
    /// 使う。**引数で規則を受け取らない。** 受け取ると、種別に対応しない規則を
    /// 渡す呼び出しが型検査を通ってしまい、照合の規則を種別が決めるという
    /// 前提が呼び出し側の書き方に依存する。
    ///
    /// 「照合しない」を `None` で返すのは、「照合して一致した」と同じ値に
    /// ならないようにするためである。
    pub fn read_back_matches(&self, observed: &str) -> Option<bool> {
        match self.read_back {
            ReadBackCheck::Compare(comparison) => Some(comparison.matches(&self.value, observed)),
            ReadBackCheck::Declared { .. } => None,
        }
    }
}

/// 設定項目名の実在確認から書き込む文字列の組み立てまでを行う。
///
/// `items` は対象 effect が公開している設定項目の一覧である。判定は次の順で
/// 行う。
///
/// 1. [`ItemValue::Unknown`] を拒否する
/// 2. `item` が `items` に存在することを確認する
/// 3. 種別への書き込みが公開されているかを確認する
/// 4. 種別と値の形が対応するかを確認する
/// 5. 書き込む文字列へ変換し、読み直しの照合のしかたを種別と値から決める
///
/// `target` は移動を書き込む対象の性質である。**省略できる形にしない。**
/// 移動の検証は対象を見なければ成立せず、渡さずに済む経路が 1 つでもあると、
/// そこがホストを落とす文字列を組み立てる経路になる。移動を含まない値では
/// 参照されないが、それでも受け取るのは「渡さない呼び出し」を書けなくする
/// ためである。
pub fn prepare_item_write(
    items: &[AvailableEffectItem],
    item: &str,
    value: &ItemValue,
    target: TrackWriteTarget<'_>,
) -> Result<ItemWrite, ItemWriteError> {
    if matches!(value, ItemValue::Unknown { .. }) {
        return Err(ItemWriteError::UnknownValue);
    }
    let entry = items
        .iter()
        .find(|candidate| candidate.name == item)
        .ok_or_else(|| ItemWriteError::ItemNotFound {
            item: item.to_string(),
        })?;
    Ok(ItemWrite {
        value: encode_item_value(&entry.item_type, value, target)?,
        read_back: read_back_check(&entry.item_type, value),
    })
}

/// 種別と値を照合し、書き込む文字列を組み立てる。
///
/// 書き込みを公開する種別かどうかを、種別と値の対応より**先に**判定する。
/// 公開しない種別は受け付ける値の形自体を定めていないため、値の形の照合が
/// 成立しないためである。
///
/// crate の外へは出さない。外から呼べると、SDK へ渡す文字列を [`ItemWrite`]
/// を経ずに組み立てられ、書き込みと照合が別の文字列を見る余地が生まれる。
pub(crate) fn encode_item_value(
    item_type: &EffectItemType,
    value: &ItemValue,
    target: TrackWriteTarget<'_>,
) -> Result<String, ItemWriteError> {
    if matches!(value, ItemValue::Unknown { .. }) {
        return Err(ItemWriteError::UnknownValue);
    }
    if !is_writable(item_type) {
        return Err(ItemWriteError::UnsupportedItemType {
            item_type: item_type.kind_name(),
        });
    }
    if !accepts(item_type, value) {
        return Err(ItemWriteError::ValueKindMismatch {
            item_type: item_type.kind_name(),
            value_kind: value.kind(),
        });
    }
    encode_value(value, target)
}

/// 種別も対象も伴わずに判定できる範囲だけを検証する。
///
/// 対象 effect の設定項目一覧を持たない層が、要求を受け付けた時点で
/// 呼ぶための入口である。種別との対応は [`encode_item_value`] が見る。
///
/// **移動については書式の規則しか見ない。** 値の個数は対象の区間の数で決まり、
/// 移動方法の名前は実行環境が受け付ける一覧で決まる。どちらも要求内容だけでは
/// 判定できないため、[`prepare_item_write`] が対象を伴って見る。**ここを通った
/// ことは、移動を書き込んでよいことを意味しない。**
pub fn validate_item_value(value: &ItemValue) -> Result<(), ItemWriteError> {
    match value {
        ItemValue::Track(track) => validate_track_syntax(track),
        ItemValue::Unknown { .. }
        | ItemValue::Integer { .. }
        | ItemValue::Number { .. }
        | ItemValue::Bool { .. }
        | ItemValue::Text { .. }
        | ItemValue::Color { .. }
        | ItemValue::Choice { .. }
        | ItemValue::Font { .. }
        | ItemValue::File { .. }
        | ItemValue::Folder { .. } => encode_value(value, NO_TRACK_TARGET).map(|_| ()),
    }
}

/// 移動を含まない値の符号化へ渡す対象。
///
/// 参照されないことは、この定数を使う腕が移動を含まない値だけであることで
/// 保証される。移動が紛れ込めば、区間数 0 と空の一覧が両方の検証を落とす。
const NO_TRACK_TARGET: TrackWriteTarget<'static> = TrackWriteTarget {
    section_count: 0,
    movements: &[],
};

/// 書き込みを公開している種別か。
///
/// 複合種別のうち `scene` / `range` / `data` と未知種別は、値の表記が確定して
/// いないため公開しない。推測した表記で書き込むと、検証を通ったのに意図と
/// 異なる値が入る。
///
/// 選択肢から選ぶ 4 種別は表記が確定しているため公開する。読み取りはいずれも
/// [`ItemValue::Choice`] で返し、有効な値を知る手段は既存のオブジェクトから
/// 読むことである。選択肢に無い値を渡したことは [`read_back_check`] が
/// 要求する照合で分かる。
fn is_writable(item_type: &EffectItemType) -> bool {
    matches!(
        item_type,
        EffectItemType::Integer
            | EffectItemType::Number
            | EffectItemType::Check
            | EffectItemType::Text
            | EffectItemType::String
            | EffectItemType::File
            | EffectItemType::Folder
            | EffectItemType::Font
            | EffectItemType::Color
            | EffectItemType::Select
            | EffectItemType::Combo
            | EffectItemType::Mask
            | EffectItemType::Figure
    )
}

/// 書き込んだ値を読み直したときの扱いを種別から決める。
///
/// ホストは書き込みの成否を返さない。値域を外れた数値は切り詰め、小数は項目の
/// 桁へ丸め、書式の合わない色は既定値へ落とし、未登録のフォント名と選択肢に
/// 無い値は黙って捨てる。**読み直さない限り、要求した値が入らなかったことを知る
/// 手段が無い。** したがって書き込みを公開する種別はすべて照合する。
///
/// 比較の規則は種別ごとに違う。ホストは受け付けた値の表記も整えるため、バイト
/// 列の完全一致を全種別へ課すと、正しい書き込みまで失敗として返る。
///
/// - 整数・実数は数値として比べる。ホストが桁を整えるため、`100` と `100.00` は
///   同じ値である
/// - チェックは真偽値として比べる
/// - 色は 16 進表記の大文字小文字を無視して比べる。ホストは受理した色を小文字で
///   返す
/// - フォント名・テキスト・パス・選択肢は完全一致で比べる。ホストはこれらを
///   正規化せず、テキストが渡すのは既に符号化済みの文字列であり、読み直しは
///   同じ表記で返る
///
/// **値の形を先に見る。** トラックバーの種別は数値と移動の 2 通りの表記を
/// 受け付けるため、種別だけでは比べ方が決まらない。移動を数値として比べると、
/// 正しい書き込みが「数値として読めない」として失敗になる。
///
/// 書き込みを公開していない種別は照合しない。公開の可否は [`is_writable`] が
/// 別に決めており、そちらで拒否されるためここへ由来する [`ItemWrite`] は生まれ
/// ない。**この対応が成り立つのは移動を含まない値についてだけである**——移動は
/// 種別を見る前に答えを返すため、公開していない種別に対しても
/// [`ReadBackCheck::Compare`] になる。その組は [`accepts`] が先に拒むため
/// [`ItemWrite`] にはならないが、この関数を直接呼べば観測できる。
///
/// **`_` を使わない網羅 `match` である。** 種別を足したときも値の形を足したとき
/// も、照合するかを決めないまま既定へ落ちることがない。
pub fn read_back_check(item_type: &EffectItemType, value: &ItemValue) -> ReadBackCheck {
    match value {
        ItemValue::Track(_) => return ReadBackCheck::Compare(ReadBackComparison::Track),
        ItemValue::Number { .. }
        | ItemValue::Integer { .. }
        | ItemValue::Bool { .. }
        | ItemValue::Color { .. }
        | ItemValue::Choice { .. }
        | ItemValue::File { .. }
        | ItemValue::Folder { .. }
        | ItemValue::Font { .. }
        | ItemValue::Text { .. }
        | ItemValue::Unknown { .. } => {}
    }
    match item_type {
        EffectItemType::Integer | EffectItemType::Number => {
            ReadBackCheck::Compare(ReadBackComparison::Numeric)
        }
        EffectItemType::Check => ReadBackCheck::Compare(ReadBackComparison::Boolean),
        EffectItemType::Color => ReadBackCheck::Compare(ReadBackComparison::IgnoreAsciiCase),
        EffectItemType::Font
        | EffectItemType::Text
        | EffectItemType::String
        | EffectItemType::File
        | EffectItemType::Folder
        | EffectItemType::Select
        | EffectItemType::Combo
        | EffectItemType::Mask
        | EffectItemType::Figure => ReadBackCheck::Compare(ReadBackComparison::Exact),
        EffectItemType::Scene
        | EffectItemType::Range
        | EffectItemType::Data
        | EffectItemType::Unknown(_) => ReadBackCheck::Declared {
            reason: ReadBackNotVerified::ItemTypeNotWritable,
        },
    }
}

/// 種別が値の形を受け付けるか。
///
/// トラックバーの移動は、任意フレームでの値を評価できる種別のうちトラックバーと
/// して評価されるものが受け付ける。**評価の種別を引き当てて判定する。** 種別の
/// 名前を並べ直すと、評価はトラックバーとして行うのに移動は書けない、という
/// 食い違いを作れてしまう。
fn accepts(item_type: &EffectItemType, value: &ItemValue) -> bool {
    if matches!(value, ItemValue::Track(_)) {
        return item_type.evaluated_kind() == Some(EvaluatedItemKind::Track);
    }
    matches!(
        (item_type, value),
        (EffectItemType::Integer, ItemValue::Integer { .. })
            | (EffectItemType::Number, ItemValue::Number { .. })
            | (EffectItemType::Check, ItemValue::Bool { .. })
            | (
                EffectItemType::Text | EffectItemType::String,
                ItemValue::Text { .. }
            )
            | (EffectItemType::File, ItemValue::File { .. })
            | (EffectItemType::Folder, ItemValue::Folder { .. })
            | (EffectItemType::Font, ItemValue::Font { .. })
            | (EffectItemType::Color, ItemValue::Color { .. })
            | (
                EffectItemType::Select
                    | EffectItemType::Combo
                    | EffectItemType::Mask
                    | EffectItemType::Figure,
                ItemValue::Choice { .. }
            )
    )
}

/// 値を書き込む文字列へ変換する。
///
/// 読み取りが返した値をそのまま書き戻せるよう、表記を独自に整形しない。
/// 整数は十進整数、実数は指数表記を用いない十進小数、真偽値は `0` / `1` と
/// する。実数は元の値へ戻せる最短の桁数で書き出す。
///
/// [`ItemValue::Track`] は区間ごとの値と移動を 1 本の文字列へ組み立てる
/// （[`encode_track_value`]）。書式を壊す指定はそこで拒否する——不正な文字列は
/// ホストのプロセスを落とす。
///
/// [`ItemValue::Text`] だけは改行とタブを許し、ホストのエスケープ表記へ
/// 符号化する（[`encode_multiline_text`]）。改行を拒否すると複数行のテキストを
/// 書く直接の手段が無くなり、符号化しないとクライアントの `\` がホストの
/// エスケープとして解釈される。色・フォント名・選択肢の値に改行が現れる余地は
/// 無く、ホストもこれらをエスケープ表記で扱わないため、どちらも緩和しない。
///
/// **色の書式も、フォント名が登録済みかも、ここでは検査しない。** ホストが
/// 受理する表記の全体像を観測できていないため、事前に弾く規則を置くと、通る
/// はずの値を我々の側が拒むことになる。受け付けられなかったことは、書き込んだ
/// 直後の読み直しが要求と違う値を返すことで分かる（[`read_back_check`]）。
/// 観測に基づく規則だけで判定できる場所へ判定を置く、ということである。
fn encode_value(value: &ItemValue, target: TrackWriteTarget<'_>) -> Result<String, ItemWriteError> {
    match value {
        ItemValue::Unknown { .. } => Err(ItemWriteError::UnknownValue),
        ItemValue::Integer { value } => Ok(value.to_string()),
        ItemValue::Number { value } => Ok(value.to_string()),
        ItemValue::Bool { value } => Ok(if *value { "1" } else { "0" }.to_string()),
        ItemValue::Text { value } => encode_multiline_text(value),
        ItemValue::Track(track) => encode_track_value(track, target),
        ItemValue::Color { value } | ItemValue::Choice { value } => encode_text(value),
        ItemValue::Font { name } => encode_text(name),
        ItemValue::File { path } | ItemValue::Folder { path } => encode_path(path),
    }
}

/// 単一行の文字列値をそのまま渡せる形か確認する。
fn encode_text(value: &str) -> Result<String, ItemWriteError> {
    validate_item_text(value)?;
    Ok(value.to_string())
}

/// 複数行を取り得る文字列値を、ホストへ渡すエスケープ表記へ符号化する。
///
/// 段は次の順に掛かり、それぞれが次の段の前提を保証する。
///
/// 1. [`validate_multiline_item_text`] が NUL・行の折り返しと字下げ以外の
///    制御文字・単独の CR を落とす。以降に残る制御文字は LF・CRLF・タブだけになる
/// 2. CRLF を LF へ正規化する。ホストは両者を区別せず、読み直しも描画も同じに
///    なるため、往復が安定する形へ寄せる
/// 3. [`encode_host_text`] が `\` と LF をホストのエスケープ表記へ包む。ここで
///    初めて、クライアントが与えた `\` がホストのエスケープとして解釈されなく
///    なる
/// 4. [`limit_item_value_bytes`] が上限を課す
///
/// **上限は符号化の後に掛ける。** 上限が守るのは応答と設定項目の大きさであり、
/// ホストへ実際に渡って保存されるのは符号化後の文字列である。符号化の前に
/// 掛けると、`\` と改行だけ上限を超えて通る。
fn encode_multiline_text(value: &str) -> Result<String, ItemWriteError> {
    validate_multiline_item_text(value)?;
    let encoded = encode_host_text(&value.replace("\r\n", "\n"));
    limit_item_value_bytes(&encoded)?;
    Ok(encoded)
}

/// パス値をそのまま渡せる形か確認する。
///
/// パスとしての構文に加えて、設定項目の値としての上限も課す。2 つは択一で
/// はなく、どちらも掛かる。パスの上限は UTF-16 code unit で数えるため、
/// どの文字集合でも設定値のバイト上限より緩い。パス側だけを見ると、単一の
/// 項目が応答サイズを圧迫しないという上限の目的が達成されない。
fn encode_path(path: &str) -> Result<String, ItemWriteError> {
    validate_path(path)?;
    validate_item_text(path)?;
    Ok(path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::REASON_VALUES;
    use crate::validation::{MAX_ITEM_VALUE_BYTES, MAX_PATH_UTF16_UNITS};

    /// 検査で使う移動方法の一覧。
    fn sample_movements() -> Vec<String> {
        vec!["直線移動".to_string(), "曲線移動".to_string()]
    }

    /// 区間 1 個の対象。標本の移動は 2 要素であり、この対象へ書ける。
    fn sample_target(movements: &[String]) -> TrackWriteTarget<'_> {
        TrackWriteTarget {
            section_count: 1,
            movements,
        }
    }

    /// 対象を伴わない呼び出しを検査の側で綴らないための包み。
    ///
    /// 製品コードは対象を必ず渡す。検査だけが同じ対象を何度も書くのを避ける。
    fn encoded_value(
        item_type: &EffectItemType,
        value: &ItemValue,
    ) -> Result<String, ItemWriteError> {
        let movements = sample_movements();
        encode_item_value(item_type, value, sample_target(&movements))
    }

    /// [`prepare_item_write`] を標本の対象で呼ぶ。
    fn prepared_write(
        items: &[AvailableEffectItem],
        item: &str,
        value: &ItemValue,
    ) -> Result<ItemWrite, ItemWriteError> {
        let movements = sample_movements();
        prepare_item_write(items, item, value, sample_target(&movements))
    }

    /// variant を表す名前を返す。
    ///
    /// 網羅 match で書く。variant を足すとここがコンパイルエラーになり、
    /// すぐ下の一覧と [`ItemWriteError::all`] へ足す必要があることが分かる。
    fn write_variant_name(error: &ItemWriteError) -> &'static str {
        match error {
            ItemWriteError::UnknownValue => "UnknownValue",
            ItemWriteError::ItemNotFound { .. } => "ItemNotFound",
            ItemWriteError::ValueKindMismatch { .. } => "ValueKindMismatch",
            ItemWriteError::UnsupportedItemType { .. } => "UnsupportedItemType",
            ItemWriteError::Text(_) => "Text",
            ItemWriteError::Path(_) => "Path",
            ItemWriteError::Track(_) => "Track",
        }
    }

    #[test]
    fn all_write_failures_cover_every_variant() {
        const VARIANTS: &[&str] = &[
            "UnknownValue",
            "ItemNotFound",
            "ValueKindMismatch",
            "UnsupportedItemType",
            "Text",
            "Path",
            "Track",
        ];
        let covered: Vec<&str> = ItemWriteError::all()
            .iter()
            .map(write_variant_name)
            .collect();
        for variant in VARIANTS {
            assert!(
                covered.contains(variant),
                "{variant} の代表値が一覧にありません"
            );
        }
        for variant in &covered {
            assert!(
                VARIANTS.contains(variant),
                "{variant} が網羅すべき variant の一覧にありません"
            );
        }
    }

    #[test]
    fn all_write_failures_cover_every_syntax_kind() {
        // 名前は包む側の種別で決まる。variant を 1 つ挙げるだけでは、
        // 包む側に種別が増えたときに一覧が追随しない。
        let reasons: Vec<Option<&str>> = ItemWriteError::all()
            .iter()
            .map(ItemWriteError::reason)
            .collect();
        for source in TextSyntaxError::ALL {
            assert!(reasons.contains(&Some(source.reason())), "{source}");
        }
        for source in PathSyntaxError::ALL {
            assert!(reasons.contains(&Some(source.reason())), "{source}");
        }
        for source in TrackValueError::ALL {
            assert!(reasons.contains(&Some(source.reason())), "{source}");
        }
    }

    #[test]
    fn write_failures_carry_the_reason_of_the_syntax_error_they_wrap() {
        // 検証の失敗種別をそのまま名乗る。写し替える層を挟むと、種別の
        // 取り違えが起きても誰も落ちない。
        for error in PathSyntaxError::ALL {
            assert_eq!(
                ItemWriteError::Path(*error).reason(),
                Some(error.reason()),
                "{error}"
            );
        }
        for error in TextSyntaxError::ALL {
            assert_eq!(
                ItemWriteError::Text(*error).reason(),
                Some(error.reason()),
                "{error}"
            );
        }
        for error in TrackValueError::ALL {
            assert_eq!(
                ItemWriteError::Track(error.clone()).reason(),
                Some(error.reason()),
                "{error}"
            );
        }
    }

    #[test]
    fn write_failures_only_name_reasons_from_the_shared_value_set() {
        let named = [
            ItemWriteError::UnsupportedItemType {
                item_type: "scene".to_string(),
            },
            ItemWriteError::Text(TextSyntaxError::ContainsNul),
            ItemWriteError::Path(PathSyntaxError::UncPath),
            ItemWriteError::Track(TrackValueError::UnknownMode { known: Vec::new() }),
        ];
        for error in named {
            let reason = error.reason().expect("名前を持つ失敗です");
            assert!(
                REASON_VALUES.contains(&reason),
                "{reason} が reason の値域にありません"
            );
        }
        // 種別名と値の形を別のキーで返せる失敗は名前を持たない。
        for error in [
            ItemWriteError::UnknownValue,
            ItemWriteError::ItemNotFound {
                item: "範囲".to_string(),
            },
            ItemWriteError::ValueKindMismatch {
                item_type: "integer".to_string(),
                value_kind: "text",
            },
        ] {
            assert_eq!(error.reason(), None, "{error}");
        }
    }

    fn sample_values() -> Vec<ItemValue> {
        vec![
            ItemValue::Number {
                value: FiniteF64::try_new(1.5).unwrap(),
            },
            ItemValue::Integer { value: -3 },
            ItemValue::Bool { value: true },
            ItemValue::Color {
                value: "#ff8800".to_string(),
            },
            ItemValue::Choice {
                value: "通常".to_string(),
            },
            ItemValue::File {
                path: r"C:\movie.mp4".to_string(),
            },
            ItemValue::Folder {
                path: r"C:\assets".to_string(),
            },
            ItemValue::Font {
                name: "Meiryo".to_string(),
            },
            ItemValue::Text {
                value: "字幕".to_string(),
            },
            ItemValue::Track(sample_track()),
            ItemValue::Unknown {
                raw: "future=1".to_string(),
            },
        ]
    }

    /// 区間 1 個分の移動を持つ値。
    fn sample_track() -> TrackValue {
        TrackValue {
            values: vec![
                FiniteF64::try_new(-500.0).unwrap(),
                FiniteF64::try_new(500.0).unwrap(),
            ],
            mode: Some("直線移動".to_string()),
            params: Vec::new(),
            accelerate: false,
            decelerate: false,
            twopoint: false,
            reserved_flags: 0,
        }
    }

    #[test]
    fn item_value_roundtrip() {
        for value in sample_values() {
            let s = serde_json::to_string(&value).unwrap();
            let restored: ItemValue = serde_json::from_str(&s).unwrap();
            assert_eq!(restored, value);
        }
    }

    #[test]
    fn item_value_tag_is_snake_case() {
        let value = ItemValue::Integer { value: 1 };
        assert_eq!(
            serde_json::to_value(value).unwrap(),
            serde_json::json!({"type": "integer", "value": 1})
        );
    }

    #[test]
    fn item_value_unknown_preserves_raw() {
        let s = r#"{"type":"unknown","raw":"opaque"}"#;
        let value: ItemValue = serde_json::from_str(s).unwrap();
        assert_eq!(
            value,
            ItemValue::Unknown {
                raw: "opaque".to_string()
            }
        );
        assert_eq!(serde_json::to_string(&value).unwrap(), s);
    }

    #[test]
    fn item_value_number_rejects_non_finite_json_literals() {
        // NaN / Infinity は JSON の字句として存在しないため、
        // FiniteF64 の検証へ到達する前にパーサが拒否する。
        for literal in ["NaN", "Infinity", "-Infinity"] {
            let s = format!(r#"{{"type":"number","value":{literal}}}"#);
            let result: Result<ItemValue, _> = serde_json::from_str(&s);
            assert!(result.is_err(), "{literal} が受理された");
        }
    }

    #[test]
    fn item_value_number_rejects_out_of_range_exponent() {
        // 表現範囲を超える指数も数値へ変換できないため拒否される。
        let result: Result<ItemValue, _> =
            serde_json::from_str(r#"{"type":"number","value":1e309}"#);
        assert!(result.is_err());
    }

    #[test]
    fn item_value_track_is_a_flat_object() {
        // 読み取りが返す形と、書き込みが受け取る形は同じである。入れ子にすると
        // 応答をそのまま送り返せない。
        assert_eq!(
            serde_json::to_value(ItemValue::Track(sample_track())).unwrap(),
            serde_json::json!({
                "type": "track",
                "values": [-500.0, 500.0],
                "mode": "直線移動",
                "params": [],
                "accelerate": false,
                "decelerate": false,
                "twopoint": false,
                "reserved_flags": 0,
            })
        );
    }

    #[test]
    fn a_movement_is_only_accepted_by_trackbar_item_types() {
        // 移動を書けるのは、任意フレームでの値をトラックバーとして評価できる
        // 種別だけである。種別を足したときに、評価と書き込みの対応が黙って
        // ずれないよう、走査して突き合わせる。
        let value = ItemValue::Track(sample_track());
        for item_type in EffectItemType::ALL
            .iter()
            .chain([&EffectItemType::Unknown(99)])
        {
            let accepted = item_type.evaluated_kind() == Some(EvaluatedItemKind::Track);
            assert_eq!(
                encoded_value(item_type, &value).is_ok(),
                accepted,
                "{item_type} が移動を受け付けるかが評価の種別と食い違います"
            );
        }
        assert_eq!(
            encoded_value(&EffectItemType::Number, &value),
            Ok("-500,500,直線移動,0".to_string())
        );
        assert_eq!(
            encoded_value(&EffectItemType::Check, &value),
            Err(ItemWriteError::ValueKindMismatch {
                item_type: "check".to_string(),
                value_kind: "track",
            })
        );
    }

    #[test]
    fn a_movement_is_compared_as_a_movement_after_the_write() {
        // 種別だけでは比べ方が決まらない。同じ種別へ数値を書けば数値として、
        // 移動を書けば移動として比べる。
        let items = vec![AvailableEffectItem {
            name: "X".to_string(),
            item_type: EffectItemType::Number,
        }];
        let write =
            prepared_write(&items, "X", &ItemValue::Track(sample_track())).expect("移動の書き込み");
        assert_eq!(
            write.read_back(),
            ReadBackCheck::Compare(ReadBackComparison::Track)
        );
        // ホストが桁を整えた読み直しは一致とする。
        assert_eq!(
            write.read_back_matches("-500.00,500.00,直線移動,0|"),
            Some(true)
        );
        // 値が切り詰められた読み直しは一致としない。
        assert_eq!(
            write.read_back_matches("0.00,500.00,直線移動,0|"),
            Some(false)
        );

        let numeric = prepared_write(
            &items,
            "X",
            &ItemValue::Number {
                value: FiniteF64::try_new(12.5).unwrap(),
            },
        )
        .expect("数値の書き込み");
        assert_eq!(
            numeric.read_back(),
            ReadBackCheck::Compare(ReadBackComparison::Numeric)
        );
    }

    /// 移動を持たない値。区間の数に依らず 1 要素である。
    fn static_track() -> TrackValue {
        TrackValue {
            values: vec![FiniteF64::try_new(0.0).unwrap()],
            mode: None,
            params: Vec::new(),
            accelerate: false,
            decelerate: false,
            twopoint: false,
            reserved_flags: 0,
        }
    }

    #[test]
    fn a_scalar_would_drop_a_movement() {
        // ホストは移動を持つ項目へ数値を書くと、移動も加速も中間点無視も捨てて
        // 成功を返す。要求元が移動を消すつもりだったかは値の形に現れない。
        let current = "-500.00,500.00,直線移動,5";
        for value in [
            ItemValue::Number {
                value: FiniteF64::try_new(0.0).unwrap(),
            },
            ItemValue::Integer { value: 0 },
        ] {
            assert!(
                write_drops_existing_movement(&EffectItemType::Number, &value, current),
                "{}",
                value.kind()
            );
        }
        // 移動を消す指定は消さない扱いにしない。**これが消す唯一の手段である。**
        assert!(!write_drops_existing_movement(
            &EffectItemType::Number,
            &ItemValue::Track(static_track()),
            current
        ));
        // 移動の書き換えも通る。読み取った値をそのまま書き戻せる。
        assert!(!write_drops_existing_movement(
            &EffectItemType::Number,
            &ItemValue::Track(sample_track()),
            current
        ));
    }

    #[test]
    fn a_movement_can_be_added_where_there_is_none() {
        // 静的なトラックバーへ移動を書くと、新しく移動が付く（実測）。
        // **アニメーションを作る経路がこれである。** 現在値で拒むと、移動は
        // 既に移動を持つ項目にしか書けなくなる。
        assert!(!write_drops_existing_movement(
            &EffectItemType::Number,
            &ItemValue::Track(sample_track()),
            "0.00"
        ));
        for value in [
            ItemValue::Number {
                value: FiniteF64::try_new(1.5).unwrap(),
            },
            ItemValue::Track(static_track()),
        ] {
            assert!(
                !write_drops_existing_movement(&EffectItemType::Number, &value, "0.00"),
                "{}",
                value.kind()
            );
        }
    }

    #[test]
    fn only_a_scalar_written_to_a_trackbar_needs_the_current_value() {
        // 現在値を読むのは、移動を持ち得る種別へ移動を含まない値を書くときだけ
        // である。移動を書く要求は現在値によらず通るため読まない。
        let scalar = ItemValue::Number {
            value: FiniteF64::try_new(0.0).unwrap(),
        };
        let moving = ItemValue::Track(sample_track());
        for item_type in EffectItemType::ALL
            .iter()
            .chain([&EffectItemType::Unknown(99)])
        {
            let holds_movement = item_type.evaluated_kind() == Some(EvaluatedItemKind::Track);
            assert_eq!(
                movement_check_reads_current_value(item_type, &scalar),
                holds_movement,
                "{item_type} の判定が評価の種別と食い違います"
            );
            assert!(
                !movement_check_reads_current_value(item_type, &moving),
                "{item_type} が移動の書き込みで現在値を読みます"
            );
            // 読まない組み合わせでは、移動を持つ現在値を渡しても拒否されない。
            assert_eq!(
                write_drops_existing_movement(item_type, &scalar, "-500.00,500.00,直線移動,0"),
                holds_movement,
                "{item_type}"
            );
        }
    }

    #[test]
    fn a_movement_written_to_a_type_that_cannot_hold_one_is_refused_by_the_kind_check() {
        // 移動を持ち得ない種別への移動は、種別と値の形の照合が拒む。ホストは
        // 多値の文字列を受け取っても先頭の値だけを使う（観測したのはチェック
        // ボックスの項目である）。判定を 2 か所へ置かない。
        assert_eq!(
            encoded_value(&EffectItemType::Check, &ItemValue::Track(sample_track())),
            Err(ItemWriteError::ValueKindMismatch {
                item_type: "check".to_string(),
                value_kind: "track",
            })
        );
        assert!(!write_drops_existing_movement(
            &EffectItemType::Check,
            &ItemValue::Track(sample_track()),
            "0"
        ));
    }

    #[test]
    fn an_unreadable_current_value_counts_as_no_movement() {
        // 読めない文字列は移動を名乗っていない。移動として扱うと、移動を持たない
        // 項目への数値の書き込みまで拒否される。
        for current in ["", "?", "0,100,直線移動"] {
            assert!(
                !write_drops_existing_movement(
                    &EffectItemType::Number,
                    &ItemValue::Number {
                        value: FiniteF64::try_new(0.0).unwrap(),
                    },
                    current,
                ),
                "{current}"
            );
        }
    }

    #[test]
    fn the_movement_comparison_needs_both_sides_to_be_readable() {
        // 読めない文字列は、要求した移動が入ったことを示さない。
        let comparison = ReadBackComparison::Track;
        assert!(comparison.matches("-500,500,直線移動,0", "-500.00,500.00,直線移動,0"));
        assert!(!comparison.matches("-500,500,直線移動,0", "0.00"));
        assert!(!comparison.matches("-500,500,直線移動,0", "壊れた値"));
    }

    #[test]
    fn item_value_rejects_unknown_tag() {
        let result: Result<ItemValue, _> = serde_json::from_str(r#"{"type":"vector","x":1}"#);
        assert!(result.is_err());
    }

    /// 書き込みを公開する種別と、受け付ける値の組。
    fn writable_pairs() -> Vec<(EffectItemType, ItemValue, &'static str)> {
        vec![
            (
                EffectItemType::Integer,
                ItemValue::Integer { value: -3 },
                "-3",
            ),
            (
                EffectItemType::Number,
                ItemValue::Number {
                    value: FiniteF64::try_new(12.5).unwrap(),
                },
                "12.5",
            ),
            (EffectItemType::Check, ItemValue::Bool { value: true }, "1"),
            (
                EffectItemType::Text,
                ItemValue::Text {
                    value: "字幕".to_string(),
                },
                "字幕",
            ),
            (
                EffectItemType::String,
                ItemValue::Text {
                    value: "文字列".to_string(),
                },
                "文字列",
            ),
            (
                EffectItemType::File,
                ItemValue::File {
                    path: r"C:\movie.mp4".to_string(),
                },
                r"C:\movie.mp4",
            ),
            (
                EffectItemType::Folder,
                ItemValue::Folder {
                    path: r"C:\assets".to_string(),
                },
                r"C:\assets",
            ),
            (
                EffectItemType::Font,
                ItemValue::Font {
                    name: "Meiryo".to_string(),
                },
                "Meiryo",
            ),
            (
                EffectItemType::Color,
                ItemValue::Color {
                    value: "#ff8800".to_string(),
                },
                "#ff8800",
            ),
            (
                EffectItemType::Select,
                ItemValue::Choice {
                    value: "通常".to_string(),
                },
                "通常",
            ),
            (
                EffectItemType::Combo,
                ItemValue::Choice {
                    value: "通常".to_string(),
                },
                "通常",
            ),
            (
                EffectItemType::Mask,
                ItemValue::Choice {
                    value: "四角形".to_string(),
                },
                "四角形",
            ),
            (
                EffectItemType::Figure,
                ItemValue::Choice {
                    value: "星型".to_string(),
                },
                "星型",
            ),
        ]
    }

    /// 選択肢から選ぶ種別。
    fn choice_item_types() -> Vec<EffectItemType> {
        vec![
            EffectItemType::Select,
            EffectItemType::Combo,
            EffectItemType::Mask,
            EffectItemType::Figure,
        ]
    }

    /// 書き込みを公開しない種別。
    fn non_writable_item_types() -> Vec<EffectItemType> {
        vec![
            EffectItemType::Scene,
            EffectItemType::Range,
            EffectItemType::Data,
            EffectItemType::Unknown(99),
        ]
    }

    /// 種別ごとに、書き込みを公開するかを述べる。
    ///
    /// [`EffectItemType`] に対する網羅 `match` であり `_` を使わない。**種別を
    /// 足すとここが落ち、公開するかを書くまでコンパイルできない。**
    fn expects_writable(item_type: &EffectItemType) -> bool {
        match item_type {
            EffectItemType::Integer
            | EffectItemType::Number
            | EffectItemType::Check
            | EffectItemType::Text
            | EffectItemType::String
            | EffectItemType::File
            | EffectItemType::Folder
            | EffectItemType::Font
            | EffectItemType::Color
            | EffectItemType::Select
            | EffectItemType::Combo
            | EffectItemType::Mask
            | EffectItemType::Figure => true,
            EffectItemType::Scene
            | EffectItemType::Range
            | EffectItemType::Data
            | EffectItemType::Unknown(_) => false,
        }
    }

    /// 書き込みが公開されているかを、変換の応答から判定する。
    ///
    /// 公開しない種別だけが [`ItemWriteError::UnsupportedItemType`] を返す。
    /// 値の形の照合は公開の判定より後に行われるため、形が合わない値を渡しても
    /// 判定は変わらない。
    fn is_exposed_for_write(item_type: &EffectItemType) -> bool {
        let probe = ItemValue::Text {
            value: "文字列".to_string(),
        };
        !matches!(
            encoded_value(item_type, &probe),
            Err(ItemWriteError::UnsupportedItemType { .. })
        )
    }

    #[test]
    fn write_accepts_the_documented_pairs() {
        for (item_type, value, encoded) in writable_pairs() {
            assert_eq!(
                encoded_value(&item_type, &value),
                Ok(encoded.to_string()),
                "{item_type}"
            );
        }
    }

    #[test]
    fn write_rejects_unknown_value() {
        let value = ItemValue::Unknown {
            raw: "future=1".to_string(),
        };
        for item_type in writable_pairs()
            .into_iter()
            .map(|(item_type, _, _)| item_type)
            .chain(non_writable_item_types())
        {
            assert_eq!(
                encoded_value(&item_type, &value),
                Err(ItemWriteError::UnknownValue),
                "{item_type}"
            );
        }
        assert_eq!(
            validate_item_value(&value),
            Err(ItemWriteError::UnknownValue)
        );
        // 設定項目の実在確認より先に拒否する。
        assert_eq!(
            prepared_write(&[], "存在しない項目", &value),
            Err(ItemWriteError::UnknownValue)
        );
    }

    #[test]
    fn write_rejects_value_kind_mismatch() {
        let mismatched = ItemValue::Text {
            value: "文字列".to_string(),
        };
        for (item_type, _, _) in writable_pairs() {
            if matches!(item_type, EffectItemType::Text | EffectItemType::String) {
                continue;
            }
            assert_eq!(
                encoded_value(&item_type, &mismatched),
                Err(ItemWriteError::ValueKindMismatch {
                    item_type: item_type.kind_name(),
                    value_kind: "text",
                }),
                "{item_type}"
            );
        }
    }

    #[test]
    fn the_exposed_types_are_the_ones_declared_writable() {
        // 公開の範囲を、種別を網羅した宣言と突き合わせる。実装だけを直した
        // 場合も、宣言だけを直した場合も落ちる。
        for item_type in EffectItemType::ALL
            .iter()
            .chain([&EffectItemType::Unknown(99)])
        {
            assert_eq!(
                is_exposed_for_write(item_type),
                expects_writable(item_type),
                "{item_type} の公開の可否が宣言と異なります"
            );
        }
    }

    #[test]
    fn every_known_item_type_is_listed_as_writable_or_not() {
        // 既知の種別が公開・非公開のどちらの一覧にも現れないまま検査を素通り
        // することを防ぐ。
        let writable: Vec<EffectItemType> = writable_pairs()
            .into_iter()
            .map(|(item_type, _, _)| item_type)
            .collect();
        let non_writable = non_writable_item_types();
        for item_type in EffectItemType::ALL {
            assert_eq!(
                writable.contains(item_type),
                expects_writable(item_type),
                "{item_type} が公開する種別の一覧と宣言で食い違います"
            );
            assert_eq!(
                non_writable.contains(item_type),
                !expects_writable(item_type),
                "{item_type} が公開しない種別の一覧と宣言で食い違います"
            );
        }
    }

    #[test]
    fn write_rejects_non_writable_item_types() {
        // 複合種別と未知種別は、値の形にかかわらず未対応として拒否する。選択肢
        // として書けるようになった種別と取り違えないよう、選択肢の値でも試す。
        let values = [
            ItemValue::Text {
                value: "文字列".to_string(),
            },
            ItemValue::Choice {
                value: "四角形".to_string(),
            },
        ];
        for item_type in non_writable_item_types() {
            for value in &values {
                let error = encoded_value(&item_type, value)
                    .expect_err("公開しない種別への書き込みが受理されました");
                assert_eq!(
                    error,
                    ItemWriteError::UnsupportedItemType {
                        item_type: item_type.kind_name(),
                    },
                    "{item_type} / {}",
                    value.kind()
                );
                assert_eq!(
                    error.error_code(),
                    ErrorCode::UnsupportedOperation,
                    "{item_type} / {}",
                    value.kind()
                );
            }
        }
    }

    #[test]
    fn write_separates_invalid_argument_from_unsupported_operation() {
        // 要求を直せば通るものと、直しても通らないものを取り違えない。
        assert_eq!(
            ItemWriteError::UnknownValue.error_code(),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ItemWriteError::ValueKindMismatch {
                item_type: "integer".to_string(),
                value_kind: "text",
            }
            .error_code(),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ItemWriteError::Text(TextSyntaxError::ContainsNul).error_code(),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ItemWriteError::Path(PathSyntaxError::NotAbsolute).error_code(),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            ItemWriteError::UnsupportedItemType {
                item_type: "scene".to_string(),
            }
            .error_code(),
            ErrorCode::UnsupportedOperation
        );
        assert_eq!(
            ItemWriteError::ItemNotFound {
                item: "X".to_string(),
            }
            .error_code(),
            ErrorCode::NotFound
        );
    }

    #[test]
    fn every_choice_type_shares_the_select_write_path() {
        // 表記が同じであることを、専用の分岐を持たないことで示す。同じ値に
        // 対して受理・拒否・変換結果のすべてが一致する。
        let cases = [
            ItemValue::Choice {
                value: "左寄せ[上]".to_string(),
            },
            ItemValue::Choice {
                value: "通常".to_string(),
            },
            // 形が対応しない値。
            ItemValue::Text {
                value: "通常".to_string(),
            },
            // 単一行の文字列として拒否される値。
            ItemValue::Choice {
                value: "通常\n".to_string(),
            },
        ];
        for value in cases {
            let select = encoded_value(&EffectItemType::Select, &value);
            for item_type in choice_item_types() {
                if item_type == EffectItemType::Select {
                    continue;
                }
                let other = encoded_value(&item_type, &value);
                // 種別名だけは異なるため、エラーはその点を除いて比べる。
                match (&select, &other) {
                    (Ok(select), Ok(other)) => {
                        assert_eq!(select, other, "{item_type} / {}", value.kind())
                    }
                    (Err(select), Err(other)) => {
                        assert_eq!(
                            select.error_code(),
                            other.error_code(),
                            "{item_type} / {}",
                            value.kind()
                        );
                        assert_eq!(
                            std::mem::discriminant(select),
                            std::mem::discriminant(other),
                            "{item_type} / {}",
                            value.kind()
                        );
                    }
                    (select, other) => {
                        panic!(
                            "{item_type} の {} で結果が分かれました: {select:?} / {other:?}",
                            value.kind()
                        )
                    }
                }
            }
        }
    }

    #[test]
    fn write_rejects_nul_and_control_characters_in_strings() {
        for value in [
            ItemValue::Text {
                value: "字幕\0".to_string(),
            },
            ItemValue::Color {
                value: "#ff8800\0".to_string(),
            },
            ItemValue::Font {
                name: "Meiryo\0".to_string(),
            },
            ItemValue::Choice {
                value: "通常\0".to_string(),
            },
            ItemValue::File {
                path: "C:\\movie\0.mp4".to_string(),
            },
        ] {
            assert_eq!(
                validate_item_value(&value).unwrap_err().error_code(),
                ErrorCode::InvalidArgument,
                "{}",
                value.kind()
            );
        }

        // テキスト以外は改行もタブも受け付けない。
        for value in [
            ItemValue::Color {
                value: "#ff8800\n".to_string(),
            },
            ItemValue::Font {
                name: "Meiryo\n".to_string(),
            },
            ItemValue::Choice {
                value: "通常\t".to_string(),
            },
        ] {
            assert_eq!(
                validate_item_value(&value),
                Err(ItemWriteError::Text(TextSyntaxError::ContainsControl)),
                "{}",
                value.kind()
            );
        }
    }

    #[test]
    fn text_values_may_span_multiple_lines() {
        // 複数行のテキストを 1 回の書き込みで設定できる。改行はホストの
        // エスケープ表記へ包まれ、CRLF は LF と同じ表記になる。タブは素通しする。
        let value = "1 行目\r\n2 行目\n\t字下げ".to_string();
        let encoded = "1 行目\\n2 行目\\n\t字下げ".to_string();
        for item_type in [EffectItemType::Text, EffectItemType::String] {
            assert_eq!(
                encoded_value(
                    &item_type,
                    &ItemValue::Text {
                        value: value.clone()
                    }
                ),
                Ok(encoded.clone()),
                "{item_type}"
            );
        }
        assert_eq!(validate_item_value(&ItemValue::Text { value }), Ok(()));
    }

    #[test]
    fn text_values_keep_backslashes_through_the_host_escape() {
        // クライアントが与えた `\` はホストのエスケープとして解釈されない。
        for (value, encoded) in [
            (r"C:\temp\note", r"C:\\temp\\note"),
            (r"^\d+\.txt$", r"^\\d+\\.txt$"),
            (r"\", r"\\"),
        ] {
            assert_eq!(
                encoded_value(
                    &EffectItemType::Text,
                    &ItemValue::Text {
                        value: value.to_string(),
                    }
                ),
                Ok(encoded.to_string()),
                "{value}"
            );
        }
    }

    #[test]
    fn text_values_reject_a_lone_carriage_return() {
        // 読み取りは改行だと報告するのに描画では消えるため、意図を推測せずに
        // 落とす。CRLF は LF として受ける。
        let rejected = ItemValue::Text {
            value: "1 行目\r2 行目".to_string(),
        };
        let error = ItemWriteError::Text(TextSyntaxError::LoneCarriageReturn);
        assert_eq!(
            encoded_value(&EffectItemType::Text, &rejected),
            Err(error.clone())
        );
        assert_eq!(validate_item_value(&rejected), Err(error.clone()));
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
        assert_eq!(error.reason(), Some("lone_carriage_return"));

        assert_eq!(
            encoded_value(
                &EffectItemType::Text,
                &ItemValue::Text {
                    value: "1 行目\r\n2 行目".to_string(),
                }
            ),
            Ok("1 行目\\n2 行目".to_string())
        );
    }

    #[test]
    fn the_item_value_limit_applies_to_the_encoded_form() {
        // ホストへ渡るのは符号化後の文字列である。符号化の前に掛けると、`\` と
        // 改行だけ上限を超えて通る。
        let value = "\\".repeat(MAX_ITEM_VALUE_BYTES / 2 + 1);
        assert!(value.len() <= MAX_ITEM_VALUE_BYTES);
        assert_eq!(
            validate_item_value(&ItemValue::Text {
                value: value.clone()
            }),
            Err(ItemWriteError::Text(TextSyntaxError::TooLongBytes {
                bytes: value.len() * 2,
                max: MAX_ITEM_VALUE_BYTES,
            }))
        );
        // 符号化しても上限に収まる長さは通る。
        let value = "\\".repeat(MAX_ITEM_VALUE_BYTES / 2);
        assert_eq!(validate_item_value(&ItemValue::Text { value }), Ok(()));
    }

    #[test]
    fn text_values_still_reject_other_control_characters() {
        // 緩和するのは行の折り返しと字下げだけで、他の制御文字は通さない。
        for control in ['\0', '\u{1}', '\u{b}', '\u{1b}', '\u{7f}', '\u{9b}'] {
            let value = ItemValue::Text {
                value: format!("字幕{control}"),
            };
            assert!(
                validate_item_value(&value).is_err(),
                "{control:?} が受理されました"
            );
        }
    }

    #[test]
    fn write_rejects_strings_over_the_limit() {
        let value = "a".repeat(MAX_ITEM_VALUE_BYTES + 1);
        assert_eq!(
            validate_item_value(&ItemValue::Text {
                value: value.clone()
            }),
            Err(ItemWriteError::Text(TextSyntaxError::TooLongBytes {
                bytes: MAX_ITEM_VALUE_BYTES + 1,
                max: MAX_ITEM_VALUE_BYTES,
            }))
        );
        assert_eq!(
            validate_item_value(&ItemValue::Text {
                value: value[..MAX_ITEM_VALUE_BYTES].to_string(),
            }),
            Ok(())
        );
    }

    #[test]
    fn write_rejects_invalid_paths() {
        for (path, expected) in [
            ("", PathSyntaxError::Empty),
            (r"..\movie.mp4", PathSyntaxError::NotAbsolute),
            (r"\\.\PhysicalDrive0", PathSyntaxError::DeviceNamespace),
            (r"C:\movie.mp4:stream", PathSyntaxError::AlternateDataStream),
            (r"\\server\share\movie.mp4", PathSyntaxError::UncPath),
        ] {
            for value in [
                ItemValue::File {
                    path: path.to_string(),
                },
                ItemValue::Folder {
                    path: path.to_string(),
                },
            ] {
                assert_eq!(
                    validate_item_value(&value),
                    Err(ItemWriteError::Path(expected)),
                    "{path}"
                );
            }
        }
    }

    #[test]
    fn write_rejects_paths_over_the_setting_value_limit() {
        // パスの上限は UTF-16 code unit で数えるため設定値のバイト上限より
        // 緩く、パス側だけを見ると設定値の上限が効かなくなる。両方を課す。
        for path in [
            // ASCII だけでも設定値の上限を超えられる。
            format!(r"C:\{}", "a".repeat(MAX_ITEM_VALUE_BYTES)),
            // 多バイト文字ではパス上限に達する前に大きく超える。
            format!(r"C:\{}", "あ".repeat(MAX_ITEM_VALUE_BYTES / 3)),
        ] {
            let bytes = path.len();
            assert!(path.encode_utf16().count() <= MAX_PATH_UTF16_UNITS);
            for value in [
                ItemValue::File { path: path.clone() },
                ItemValue::Folder { path: path.clone() },
            ] {
                assert_eq!(
                    validate_item_value(&value),
                    Err(ItemWriteError::Text(TextSyntaxError::TooLongBytes {
                        bytes,
                        max: MAX_ITEM_VALUE_BYTES,
                    })),
                    "{} が受理されました",
                    value.kind()
                );
            }
        }
    }

    #[test]
    fn write_accepts_paths_within_both_limits() {
        let path = format!(r"C:\{}", "a".repeat(MAX_ITEM_VALUE_BYTES - 3));
        assert_eq!(path.len(), MAX_ITEM_VALUE_BYTES);
        assert_eq!(validate_item_value(&ItemValue::File { path }), Ok(()));
    }

    #[test]
    fn write_encodes_numbers_without_losing_the_value() {
        // 読み取りは十進表記を f64 として解釈する。書き込みが元の値へ戻せる
        // 表記を出さなければ、読み取った値をそのまま書き戻せない。
        for raw in [
            0.0,
            -0.0,
            1.0,
            0.1,
            12.5,
            29.97,
            -1.0 / 3.0,
            1e300,
            1e-300,
            f64::MAX,
            f64::MIN_POSITIVE,
        ] {
            let value = ItemValue::Number {
                value: FiniteF64::try_new(raw).unwrap(),
            };
            let encoded = encoded_value(&EffectItemType::Number, &value).unwrap();
            assert!(
                !encoded.contains('e') && !encoded.contains('E'),
                "指数表記になりました: {encoded}"
            );
            // 0.0 と -0.0 は等値比較では区別できないため、ビット列で比べる。
            assert_eq!(
                encoded.trim().parse::<f64>().unwrap().to_bits(),
                raw.to_bits(),
                "{raw} が {encoded} になりました"
            );
        }
    }

    #[test]
    fn write_encodes_check_as_zero_or_one() {
        assert_eq!(
            encoded_value(&EffectItemType::Check, &ItemValue::Bool { value: false }),
            Ok("0".to_string())
        );
        assert_eq!(
            encoded_value(&EffectItemType::Check, &ItemValue::Bool { value: true }),
            Ok("1".to_string())
        );
    }

    #[test]
    fn prepare_item_write_looks_up_the_item_type() {
        let items = vec![
            AvailableEffectItem {
                name: "X".to_string(),
                item_type: EffectItemType::Number,
            },
            AvailableEffectItem {
                name: "シーン".to_string(),
                item_type: EffectItemType::Scene,
            },
        ];

        assert_eq!(
            prepared_write(
                &items,
                "X",
                &ItemValue::Number {
                    value: FiniteF64::try_new(1.5).unwrap(),
                },
            )
            .map(|write| write.value().to_string()),
            Ok("1.5".to_string())
        );
        assert_eq!(
            prepared_write(
                &items,
                "Y",
                &ItemValue::Number {
                    value: FiniteF64::try_new(1.5).unwrap(),
                },
            ),
            Err(ItemWriteError::ItemNotFound {
                item: "Y".to_string(),
            })
        );
        assert_eq!(
            prepared_write(
                &items,
                "シーン",
                &ItemValue::Text {
                    value: "0".to_string(),
                },
            ),
            Err(ItemWriteError::UnsupportedItemType {
                item_type: "scene".to_string(),
            })
        );
    }

    /// 種別ごとに、読み直しをどう扱うかを述べる。
    ///
    /// 照合のしかたが種別だけで決まる範囲を見るための当て木。
    ///
    /// トラックバーの移動は種別と値の両方で決まるため、この値では引けない。
    fn read_back_probe() -> ItemValue {
        ItemValue::Text {
            value: "文字列".to_string(),
        }
    }

    /// [`EffectItemType`] に対する網羅 `match` であり `_` を使わない。**種別を
    /// 足すとここが落ち、照合のしかたを書くまでコンパイルできない。**
    fn expects_read_back(item_type: &EffectItemType) -> ReadBackCheck {
        match item_type {
            EffectItemType::Integer | EffectItemType::Number => {
                ReadBackCheck::Compare(ReadBackComparison::Numeric)
            }
            EffectItemType::Check => ReadBackCheck::Compare(ReadBackComparison::Boolean),
            EffectItemType::Color => ReadBackCheck::Compare(ReadBackComparison::IgnoreAsciiCase),
            EffectItemType::Text
            | EffectItemType::String
            | EffectItemType::File
            | EffectItemType::Folder
            | EffectItemType::Font
            | EffectItemType::Select
            | EffectItemType::Combo
            | EffectItemType::Mask
            | EffectItemType::Figure => ReadBackCheck::Compare(ReadBackComparison::Exact),
            EffectItemType::Scene
            | EffectItemType::Range
            | EffectItemType::Data
            | EffectItemType::Unknown(_) => ReadBackCheck::Declared {
                reason: ReadBackNotVerified::ItemTypeNotWritable,
            },
        }
    }

    #[test]
    fn every_item_type_declares_how_its_read_back_is_compared() {
        // 既知の全種別と未知種別を走査し、宣言と突き合わせる。実装だけを直した
        // 場合も、宣言だけを直した場合も落ちる。網羅 `match` はどちらか一方の
        // 書き換え漏れを捕まえないため、走査で補う。
        for item_type in EffectItemType::ALL
            .iter()
            .chain([&EffectItemType::Unknown(99)])
        {
            assert_eq!(
                read_back_check(item_type, &read_back_probe()),
                expects_read_back(item_type),
                "{item_type} の照合のしかたが宣言と異なります"
            );
        }
    }

    #[test]
    fn every_writable_item_type_is_verified_by_reading_back() {
        // ホストは書き込みの成否を返さない。公開している種別のどれか 1 つでも
        // 照合から漏れると、その種別だけが「書けたのに入っていない」を成功として
        // 報告する。
        for item_type in EffectItemType::ALL {
            if !expects_writable(item_type) {
                continue;
            }
            assert!(
                matches!(
                    read_back_check(item_type, &read_back_probe()),
                    ReadBackCheck::Compare(_)
                ),
                "{item_type} が書き込み後に照合されません"
            );
        }
    }

    #[test]
    fn the_comparison_absorbs_the_notation_the_host_returns() {
        // ホストが整えた表記は要求した値そのものである。バイト比較を課すと
        // 正しい書き込みが失敗になる。
        let matching = [
            (ReadBackComparison::Numeric, "100", "100.00"),
            (ReadBackComparison::Numeric, "12.5", "12.500"),
            (ReadBackComparison::Numeric, "-3", "-3.00"),
            (ReadBackComparison::Numeric, "0", "-0.0"),
            (ReadBackComparison::Boolean, "1", "true"),
            (ReadBackComparison::Boolean, "0", "false"),
            (ReadBackComparison::IgnoreAsciiCase, "FF8800", "ff8800"),
            (ReadBackComparison::IgnoreAsciiCase, "ff8800", "FF8800"),
            (ReadBackComparison::Exact, "四角形", "四角形"),
            (ReadBackComparison::Exact, r"C:\movie.mp4", r"C:\movie.mp4"),
        ];
        for (comparison, written, observed) in matching {
            assert!(
                comparison.matches(written, observed),
                "{comparison:?}: {written} と {observed} が一致しません"
            );
        }
    }

    #[test]
    fn the_comparison_rejects_a_value_the_host_changed() {
        // 切り詰めも丸めも「要求した値を得ていない」点で同じである。許容誤差を
        // 置くと、そのどちらも一致として通ってしまう。
        let differing = [
            (ReadBackComparison::Numeric, "500", "100"),
            (ReadBackComparison::Numeric, "-1", "0"),
            (ReadBackComparison::Numeric, "12.345", "12.35"),
            (ReadBackComparison::Numeric, "100", "100.01"),
            // 数値として読めない読み直しは、要求した値を得たことを示さない。
            (ReadBackComparison::Numeric, "100", "ひゃく"),
            (ReadBackComparison::Boolean, "1", "0"),
            (ReadBackComparison::Boolean, "1", "はい"),
            (ReadBackComparison::IgnoreAsciiCase, "#ff0000", "ffffff"),
            (ReadBackComparison::IgnoreAsciiCase, "f00", "ff0000"),
            (ReadBackComparison::Exact, "NoSuchFont12345", "Yu Gothic UI"),
            (ReadBackComparison::Exact, "四角形", "円"),
            (ReadBackComparison::Exact, "MEIRYO", "Meiryo"),
        ];
        for (comparison, written, observed) in differing {
            assert!(
                !comparison.matches(written, observed),
                "{comparison:?}: {written} と {observed} が一致しました"
            );
        }
    }

    #[test]
    fn the_prepared_write_carries_the_comparison_of_its_item_type() {
        // 照合のしかたは要求内容ではなく、対象 effect が公開する種別で決まる。
        let items = vec![
            AvailableEffectItem {
                name: "図形の種類".to_string(),
                item_type: EffectItemType::Select,
            },
            AvailableEffectItem {
                name: "色".to_string(),
                item_type: EffectItemType::Color,
            },
        ];
        let choice = prepared_write(
            &items,
            "図形の種類",
            &ItemValue::Choice {
                value: "四角形".to_string(),
            },
        )
        .expect("選択肢の書き込み");
        assert_eq!(
            choice.read_back(),
            ReadBackCheck::Compare(ReadBackComparison::Exact)
        );
        assert_eq!(choice.read_back_matches("四角形"), Some(true));
        assert_eq!(choice.read_back_matches("円"), Some(false));

        let color = prepared_write(
            &items,
            "色",
            &ItemValue::Color {
                value: "FFAA00".to_string(),
            },
        )
        .expect("色の書き込み");
        assert_eq!(
            color.read_back(),
            ReadBackCheck::Compare(ReadBackComparison::IgnoreAsciiCase)
        );
        // 比較の規則は書き込みが自分で持つ。呼び出し側は規則を選べない。
        assert_eq!(color.read_back_matches("ffaa00"), Some(true));
        assert_eq!(color.read_back_matches("ffffff"), Some(false));
    }

    #[test]
    fn the_read_back_is_compared_against_the_encoded_value() {
        // 照合の材料は SDK へ渡す文字列であり、要求に現れた値ではない。両者が
        // 異なる種別で、渡した文字列とだけ一致することを固定する。
        let items = vec![AvailableEffectItem {
            name: "反転".to_string(),
            item_type: EffectItemType::Check,
        }];
        let write =
            prepared_write(&items, "反転", &ItemValue::Bool { value: true }).expect("書き込み");
        assert_eq!(
            write.value(),
            encoded_value(&EffectItemType::Check, &ItemValue::Bool { value: true }).expect("変換")
        );
        assert_eq!(
            write.read_back(),
            ReadBackCheck::Compare(ReadBackComparison::Boolean)
        );
        assert_eq!(write.read_back_matches("1"), Some(true));
        // 真偽値としての比較であるため、ホストが別の表記で返しても一致する。
        assert_eq!(write.read_back_matches("true"), Some(true));
        assert_eq!(write.read_back_matches("0"), Some(false));
    }

    #[test]
    fn parse_check_reads_both_notations_the_host_uses() {
        assert_eq!(parse_check_value("0"), Some(false));
        assert_eq!(parse_check_value("1"), Some(true));
        assert_eq!(parse_check_value("false"), Some(false));
        assert_eq!(parse_check_value("true"), Some(true));
        assert_eq!(parse_check_value(" 1 "), Some(true));
        assert_eq!(parse_check_value("2"), None);
        assert_eq!(parse_check_value(""), None);
    }

    #[test]
    fn write_errors_do_not_repeat_the_value() {
        // 設定値そのものは応答へ反響させない。
        let secret = "秘密の値";
        let errors = [
            encoded_value(
                &EffectItemType::Integer,
                &ItemValue::Text {
                    value: secret.to_string(),
                },
            )
            .unwrap_err(),
            encoded_value(
                &EffectItemType::Scene,
                &ItemValue::Text {
                    value: secret.to_string(),
                },
            )
            .unwrap_err(),
            validate_item_value(&ItemValue::Text {
                value: format!("{secret}\0"),
            })
            .unwrap_err(),
            validate_item_value(&ItemValue::File {
                path: format!(r"..\{secret}"),
            })
            .unwrap_err(),
            validate_item_value(&ItemValue::Unknown {
                raw: secret.to_string(),
            })
            .unwrap_err(),
        ];
        for error in errors {
            assert!(
                !error.to_string().contains(secret),
                "値が含まれます: {error}"
            );
        }
    }
}
