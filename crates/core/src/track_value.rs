//! トラックバーの移動（キーフレーム）を表す値と、alias 書式との符号化・復号。
//!
//! ホストはトラックバー項目の値を `<値0>,…,<値N>,<移動方法>,<フラグ>[|<パラメータ…>]`
//! という 1 本の文字列で受け渡す。値の個数は区間の境界の数と一致し、移動を
//! 持たない項目は区間数に依らず単一の数値になる。
//!
//! **符号化と復号は対にして 1 か所へ置く。** 片側だけを持つと、読み取りが返した
//! 値をそのまま書き戻せなくなる。書き込みだけがこの書式を知れば読み取りは生の
//! 文字列を返し続け、読み取りだけが知れば読めた値を書く手段が無い。
//!
//! **書き込みの検証も同じ場所に置く。** 不正な移動方法の名前と、欠落した
//! フラグは、ホストのプロセスを落とす。落とす形を作れるのは符号化であり、
//! 作らせない規則が別の場所にあると、片方だけを直したときに黙って乖離する。

use crate::item_value::ItemWriteError;
use crate::number::FiniteF64;
use crate::validation::{TextSyntaxError, limit_item_value_bytes, validate_item_text};
use serde::{Deserialize, Serialize};

/// 値・移動方法・フラグを区切る文字。
const FIELD_SEPARATOR: char = ',';

/// フラグとパラメータを区切る文字。
const PARAM_SEPARATOR: char = '|';

/// 加速のビット。
const FLAG_ACCELERATE: u32 = 1;

/// 減速のビット。
const FLAG_DECELERATE: u32 = 2;

/// 中間点無視のビット。
const FLAG_TWOPOINT: u32 = 4;

/// 時間制御を表す移動方法の名前の接尾辞。
///
/// **時間制御はフラグではない。** フラグの 3 ビット目を立てても 4 つのフラグは
/// どれも変わらず、移動方法の名前の変種だけが時間制御を有効にする。
const TIME_CONTROL_SUFFIX: &str = "(時間制御)";

/// トラックバー項目の値。
///
/// 区間ごとの値と、区間の間をどう補間するかをまとめて持つ。ホストは 1 本の
/// 文字列でしかこれを受け渡さないため、書き込みも読み取りもこの単位で行う。
///
/// **[`crate::effect::TrackInfo`] とは別の型である。** `TrackInfo` は読み取りが
/// 設定項目の脇へ添える情報で、所属グループ（`group_num` / `group_index` /
/// `group_name`）を持ち、区間ごとの値を持たない。グループは項目の並びの性質で
/// あって値ではなく、書き込みでは指定する先が無い。両者を 1 つの型にすると、
/// 書けないフィールドを書き込みの入力が受け取ることになる。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackValue {
    /// 区間の境界ごとの値。
    ///
    /// 移動を持つ値では「区間数 + 1」個、移動を持たない値では 1 個になる。
    pub values: Vec<FiniteF64>,
    /// 移動方法の名前。
    ///
    /// **`null` は「移動を持たない」を表す。** このとき `values` は 1 要素で
    /// あり、書き込むと区間の数に依らない静的な値になる。移動を消す手段は
    /// これだけである。
    pub mode: Option<String>,
    /// 移動方法のパラメータ。
    ///
    /// 空にするとホストが移動方法ごとの既定値を補う。
    pub params: Vec<FiniteF64>,
    /// 加速が有効か。
    pub accelerate: bool,
    /// 減速が有効か。
    pub decelerate: bool,
    /// 中間点を無視するか。
    pub twopoint: bool,
    /// 時間制御が有効か。
    ///
    /// **書き込みの操作子ではない。** 時間制御は移動方法の名前の変種が担うため、
    /// この値を真にしても移動は変わらない。真にできるのは `mode` が時間制御の
    /// 変種を指しているときだけで、食い違いは書き込みの検証が拒否する。
    pub timecontrol: bool,
}

/// トラックバーの移動を表す値の検証失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TrackValueError {
    /// 値の個数が規則の要求と合わない。
    #[error("移動の値は {expected} 個必要ですが {actual} 個です")]
    ValueCount {
        /// 判定に用いた規則が要求する個数。
        expected: usize,
        /// 実際の個数。
        actual: usize,
    },
    /// 移動方法の名前が既知の一覧に無い。
    #[error("移動方法の名前が既知の一覧にありません")]
    UnknownMode,
    /// 移動を持たない値に、移動の付帯情報が指定された。
    #[error("移動を持たない値にフラグとパラメータは指定できません")]
    MovementWithoutMode,
    /// 時間制御の指定が移動方法の名前と食い違う。
    #[error("時間制御の指定が移動方法の名前と一致しません")]
    TimeControlMismatch,
}

impl TrackValueError {
    /// 全 variant の代表値。
    ///
    /// [`TrackValueError::reason`] が返し得る名前を数え上げるために用いる。
    /// 値を持つ variant には代表となる値を添えてあり、名前はその値に依存しない。
    pub const ALL: &'static [TrackValueError] = &[
        TrackValueError::ValueCount {
            expected: 3,
            actual: 2,
        },
        TrackValueError::UnknownMode,
        TrackValueError::MovementWithoutMode,
        TrackValueError::TimeControlMismatch,
    ];

    /// 失敗の種別を表す機械可読な名前を返す。
    ///
    /// 検証対象の値そのものを含まない。
    pub fn reason(&self) -> &'static str {
        match self {
            TrackValueError::ValueCount { .. } => "track_value_count",
            TrackValueError::UnknownMode => "track_mode_unknown",
            TrackValueError::MovementWithoutMode => "track_movement_without_mode",
            TrackValueError::TimeControlMismatch => "track_time_control_mismatch",
        }
    }
}

/// 移動を書き込む対象の性質。
///
/// 区間の数も、その項目が受け付ける移動方法も、対象のオブジェクトと設定項目を
/// 見なければ決まらない。値だけでは判定できないため、呼び出し側が渡す。
#[derive(Debug, Clone, Copy)]
pub struct TrackWriteTarget<'a> {
    /// 対象オブジェクトの区間の数。中間点が 2 個なら 3。
    pub section_count: usize,
    /// 対象の設定項目が受け付ける移動方法の名前。
    pub known_modes: &'a [String],
}

/// 移動方法の名前が時間制御の変種か。
fn is_time_control(mode: &str) -> bool {
    mode.ends_with(TIME_CONTROL_SUFFIX)
}

/// フラグのビット列を組み立てる。
///
/// 時間制御は含まれない。含めても 4 つのフラグのどれも変わらないためである。
fn flag_bits(value: &TrackValue) -> u32 {
    let bit = |enabled: bool, mask: u32| if enabled { mask } else { 0 };
    bit(value.accelerate, FLAG_ACCELERATE)
        | bit(value.decelerate, FLAG_DECELERATE)
        | bit(value.twopoint, FLAG_TWOPOINT)
}

/// 対象を見なくても判定できる規則だけを検証する。
///
/// ここで見るのは、書式そのものを壊す指定と、値の中で閉じた矛盾である。
///
/// - 移動を持たない値は 1 要素であり、フラグもパラメータも持たない
/// - 移動方法の名前は区切り文字を含まない。含むと値の個数の数え方が狂い、
///   末尾から移動方法を取るホストの解析が別の位置を指す
/// - 移動を持つ値は 2 要素以上である。区間は必ず 1 つ以上あるためで、
///   正確な個数は区間の数を渡す [`validate_track_value`] が見る
/// - 時間制御の指定は移動方法の名前と一致する
pub(crate) fn validate_track_syntax(value: &TrackValue) -> Result<(), ItemWriteError> {
    let Some(mode) = value.mode.as_deref() else {
        if value.values.len() != 1 {
            return Err(TrackValueError::ValueCount {
                expected: 1,
                actual: value.values.len(),
            }
            .into());
        }
        if !value.params.is_empty() || flag_bits(value) != 0 || value.timecontrol {
            return Err(TrackValueError::MovementWithoutMode.into());
        }
        return Ok(());
    };
    validate_item_text(mode)?;
    if mode.is_empty() {
        return Err(TextSyntaxError::Empty.into());
    }
    if mode.contains(FIELD_SEPARATOR) || mode.contains(PARAM_SEPARATOR) {
        return Err(TextSyntaxError::ForbiddenCharacter.into());
    }
    if value.values.len() < MIN_MOVING_VALUES {
        return Err(TrackValueError::ValueCount {
            expected: MIN_MOVING_VALUES,
            actual: value.values.len(),
        }
        .into());
    }
    if value.timecontrol != is_time_control(mode) {
        return Err(TrackValueError::TimeControlMismatch.into());
    }
    Ok(())
}

/// 移動を持つ値が取り得る最小の要素数。区間 1 個の境界の数である。
const MIN_MOVING_VALUES: usize = 2;

/// 対象の性質と突き合わせて、書き込んでよい値かを判定する。
///
/// [`validate_track_syntax`] の規則に加えて次を見る。
///
/// - 移動を持つ値の要素数は「区間数 + 1」である。ホストは個数の不一致を拒否せず、
///   余った値は保存されるが評価に使われない。**止められるのはここだけである**
/// - 移動方法の名前が、呼び出し側が渡した一覧に含まれる
///
/// **移動方法の検証は「選択肢はヒントであってゲートではない」という規則の例外で
/// ある。** 選択肢の候補は一覧に無い値でも通す。ホストが受け付ける値の全体像を
/// 観測できておらず、通るはずの値を我々が拒む方が害が大きいためである。移動方法
/// は事情が違う。一覧に無い名前を渡すとホストは例外を投げ、それが `extern "C"`
/// の境界を越えてプロセスごと落ちる。**通す選択肢が無い。**
///
/// **一覧は項目ごとに違う。** 同じ名前の一覧で全項目を検証すると、その項目だけが
/// 持つ移動方法を拒み、逆に他の項目にしか無い名前を通す。前者は書けるはずの値が
/// 書けなくなるだけだが、後者はホストを落とす。一覧は対象の設定項目から引いた
/// ものを渡す。
pub fn validate_track_value(
    value: &TrackValue,
    target: TrackWriteTarget<'_>,
) -> Result<(), ItemWriteError> {
    validate_track_syntax(value)?;
    let Some(mode) = value.mode.as_deref() else {
        return Ok(());
    };
    let expected = target.section_count + 1;
    if value.values.len() != expected {
        return Err(TrackValueError::ValueCount {
            expected,
            actual: value.values.len(),
        }
        .into());
    }
    if !target.known_modes.iter().any(|known| known == mode) {
        return Err(TrackValueError::UnknownMode.into());
    }
    Ok(())
}

/// 値をホストへ渡す文字列へ符号化する。
///
/// **数値は [`ItemValue::Number`](crate::item_value::ItemValue::Number) と同じ
/// 表記で書く。** 指数表記を用いず、元の値へ戻せる最短の桁数で書き出す。ホストは
/// 受け取った値を項目の小数桁へ整えて返すため（`-600` は `-600.00` として
/// 読める）、こちらが何桁で書いても読み直しの表記は変わらない。桁を合わせにいく
/// 意味は無く、合わせるべき桁数を知る手段も無い。同じ数値を静的な値として書いた
/// ときと同じ文字列になる方が、表記の由来が 1 つで済む。
///
/// **フラグは常に書く。** 省略するとホストは末尾から数えた位置を移動方法の名前
/// として読み、数値をそこに見つけて例外を投げ、プロセスごと落ちる。
///
/// **`params` が空のときは `|` を書かない。** パラメータを持たない移動方法を
/// `|` 無しで書いた要求が受理され、ホストが既定値を補って保存することは観測して
/// いる。`|` だけを書いた要求が受理されるかは観測していないため、観測した形だけ
/// を出す。受理されない形を出すと落ちるのはホストのプロセスである。
///
/// 移動を持たない値は単一の数値になる。移動方法もフラグも書かない。
pub fn encode_track_value(value: &TrackValue) -> Result<String, ItemWriteError> {
    validate_track_syntax(value)?;
    let encoded = match value.mode.as_deref() {
        None => value.values[0].to_string(),
        Some(mode) => {
            let mut encoded = String::new();
            for number in &value.values {
                encoded.push_str(&number.to_string());
                encoded.push(FIELD_SEPARATOR);
            }
            encoded.push_str(mode);
            encoded.push(FIELD_SEPARATOR);
            encoded.push_str(&flag_bits(value).to_string());
            if !value.params.is_empty() {
                encoded.push(PARAM_SEPARATOR);
                for (index, param) in value.params.iter().enumerate() {
                    if index > 0 {
                        encoded.push(FIELD_SEPARATOR);
                    }
                    encoded.push_str(&param.to_string());
                }
            }
            encoded
        }
    };
    limit_item_value_bytes(&encoded)?;
    Ok(encoded)
}

/// ホストが返した文字列を値へ復号する。
///
/// 値の個数は可変であるため、移動方法とフラグは**末尾から**取る。区切りの
/// 数え方はホストの解析と同じであり、これが値の個数を数える唯一の手掛かりで
/// ある。
///
/// **解析できない文字列では `None` を返す。** 呼び出し側は生の文字列を
/// [`ItemValue::Unknown`](crate::item_value::ItemValue::Unknown) として保つ。
/// 推測して部分的に埋めた値を返すと、それを書き戻したときにホストへ渡るのは
/// 我々が捏造した移動になる。**読めなかったことは、読めたふりより安全である。**
///
/// 判定は次のとおり。
///
/// - 区切りが無く 1 つの有限な数値であれば、移動を持たない値とする
/// - 4 つ以上の欄があれば、末尾をフラグ、その 1 つ前を移動方法の名前、残りを
///   値とする。フラグは非負整数、名前は空でなく**数値として読めない**ことを
///   要求する。名前が数値として読めることを許すと `1,2,3,4` のような並びが
///   移動として解釈される
/// - 値が 1 つしかない移動は読まない。**符号化が書けない形を読めてしまうと、
///   読めた値を書き戻せなくなる。** 区間は必ず 1 つ以上あるため、移動を持つ値の
///   境界は 2 つ以上になる
/// - `|` より後ろはパラメータとする。`|` の後ろが空のときはパラメータ無しと
///   する。ホストはパラメータを持たない移動方法を `直線移動,0|` の形で返す
///
/// フラグの 3 ビット目以降は捨てる。**捨てても失われる情報は無い**——ホストの
/// 4 つのフラグはどれも 3 ビット目に対応せず、そのビットを立てても移動は
/// 変わらない。時間制御は移動方法の名前の変種が担うため、名前から決める。
pub fn decode_track_value(raw: &str) -> Option<TrackValue> {
    let (head, tail) = match raw.split_once(PARAM_SEPARATOR) {
        Some((head, tail)) => (head, Some(tail)),
        None => (raw, None),
    };
    let params = match tail {
        None | Some("") => Vec::new(),
        Some(tail) => tail
            .split(FIELD_SEPARATOR)
            .map(parse_finite)
            .collect::<Option<Vec<FiniteF64>>>()?,
    };
    let fields: Vec<&str> = head.split(FIELD_SEPARATOR).collect();
    if fields.len() == 1 {
        // 移動を持たない値はパラメータを取らない。
        if tail.is_some() {
            return None;
        }
        return Some(TrackValue {
            values: vec![parse_finite(fields[0])?],
            mode: None,
            params: Vec::new(),
            accelerate: false,
            decelerate: false,
            twopoint: false,
            timecontrol: false,
        });
    }
    if fields.len() < MIN_MOVING_VALUES + 2 {
        return None;
    }
    let flags: u32 = fields[fields.len() - 1].trim().parse().ok()?;
    let mode = fields[fields.len() - 2];
    if mode.is_empty() || parse_finite(mode).is_some() {
        return None;
    }
    let values = fields[..fields.len() - 2]
        .iter()
        .copied()
        .map(parse_finite)
        .collect::<Option<Vec<FiniteF64>>>()?;
    Some(TrackValue {
        values,
        mode: Some(mode.to_string()),
        params,
        accelerate: flags & FLAG_ACCELERATE != 0,
        decelerate: flags & FLAG_DECELERATE != 0,
        twopoint: flags & FLAG_TWOPOINT != 0,
        timecontrol: is_time_control(mode),
    })
}

/// 十進表記を有限な実数として読む。
fn parse_finite(raw: &str) -> Option<FiniteF64> {
    raw.trim().parse::<f64>().ok().and_then(FiniteF64::try_new)
}

/// 書き込んだ値と読み直した値が、同じ移動を表すか。
///
/// **生の文字列では比べられない。** ホストは値の桁を項目の小数桁へ整えて返す
/// ため、`-600,600,直線移動,0` と書いた結果は `-600.00,600.00,直線移動,0` として
/// 読める。バイト比較を課すと、正しい書き込みが失敗として返る。復号してから
/// 構造として比べれば、整形は吸収され、値の切り詰めと丸めは検出できる。
///
/// **パラメータは、書いた側が空のときだけ比べない。** 空のパラメータはホストの
/// 既定値を求める指定であり、読み直しが返すのはホストが選んだ値である。比べると
/// 成功した書き込みが失敗として返る。空でないパラメータは要求した値であるため
/// 比べる。
pub(crate) fn track_read_back_matches(written: &TrackValue, observed: &TrackValue) -> bool {
    if written.values != observed.values
        || written.mode != observed.mode
        || written.accelerate != observed.accelerate
        || written.decelerate != observed.decelerate
        || written.twopoint != observed.twopoint
        || written.timecontrol != observed.timecontrol
    {
        return false;
    }
    written.params.is_empty() || written.params == observed.params
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finite(values: &[f64]) -> Vec<FiniteF64> {
        values
            .iter()
            .map(|value| FiniteF64::try_new(*value).expect("有限値"))
            .collect()
    }

    fn moving(values: &[f64], mode: &str) -> TrackValue {
        TrackValue {
            values: finite(values),
            mode: Some(mode.to_string()),
            params: Vec::new(),
            accelerate: false,
            decelerate: false,
            twopoint: false,
            timecontrol: false,
        }
    }

    fn static_value(value: f64) -> TrackValue {
        TrackValue {
            values: finite(&[value]),
            mode: None,
            params: Vec::new(),
            accelerate: false,
            decelerate: false,
            twopoint: false,
            timecontrol: false,
        }
    }

    fn known_modes() -> Vec<String> {
        ["直線移動", "曲線移動", "ランダム移動", "直線移動(時間制御)"]
            .iter()
            .map(|name| name.to_string())
            .collect()
    }

    fn target(section_count: usize, modes: &[String]) -> TrackWriteTarget<'_> {
        TrackWriteTarget {
            section_count,
            known_modes: modes,
        }
    }

    #[test]
    fn the_encoding_matches_the_alias_notation() {
        assert_eq!(
            encode_track_value(&moving(&[-500.0, 500.0], "直線移動")),
            Ok("-500,500,直線移動,0".to_string())
        );
        assert_eq!(
            encode_track_value(&moving(&[0.0, 100.0, 0.0], "曲線移動")),
            Ok("0,100,0,曲線移動,0".to_string())
        );
        // 移動を持たない値は単一の数値になる。区間の数に依らない。
        assert_eq!(encode_track_value(&static_value(0.0)), Ok("0".to_string()));
    }

    #[test]
    fn the_flag_bits_are_the_ones_the_host_reports() {
        // 実測: `,5` は加速と中間点無視、`,15` は加速・減速・中間点無視。
        let mut value = moving(&[-500.0, 500.0], "直線移動");
        value.accelerate = true;
        value.twopoint = true;
        assert_eq!(
            encode_track_value(&value),
            Ok("-500,500,直線移動,5".to_string())
        );
        assert_eq!(
            decode_track_value("-500,500,直線移動,5"),
            Some(value.clone())
        );

        value.decelerate = true;
        assert_eq!(
            encode_track_value(&value),
            Ok("-500,500,直線移動,7".to_string())
        );
        // 15 は 3 ビット目も立っているが、読める 3 つのフラグは 7 と同じである。
        assert_eq!(decode_track_value("-500,500,直線移動,15"), Some(value));
    }

    #[test]
    fn the_fourth_bit_maps_to_none_of_the_flags() {
        // 実測: `,8` を書いても 4 つのフラグはすべて偽のままである。
        let decoded = decode_track_value("-600.00,600.00,直線移動,8").expect("解析できる");
        assert!(!decoded.accelerate);
        assert!(!decoded.decelerate);
        assert!(!decoded.twopoint);
        assert!(!decoded.timecontrol);
        // 符号化はそのビットを立てる手段を持たない。
        assert_eq!(
            encode_track_value(&decoded),
            Ok("-600,600,直線移動,0".to_string())
        );
    }

    #[test]
    fn time_control_is_a_variant_of_the_mode_name_and_not_a_flag() {
        let decoded = decode_track_value("0,100,直線移動(時間制御),0").expect("解析できる");
        assert!(decoded.timecontrol);
        assert_eq!(decoded.mode.as_deref(), Some("直線移動(時間制御)"));
        // フラグの欄は 0 のままである。時間制御はビットで表さない。
        assert_eq!(
            encode_track_value(&decoded),
            Ok("0,100,直線移動(時間制御),0".to_string())
        );

        // 名前が変種でないのに時間制御を名乗る値は書けない。
        let mut mismatched = moving(&[0.0, 100.0], "直線移動");
        mismatched.timecontrol = true;
        assert_eq!(
            encode_track_value(&mismatched),
            Err(TrackValueError::TimeControlMismatch.into())
        );
    }

    #[test]
    fn the_parameters_follow_the_flags_after_a_bar() {
        let mut value = moving(&[0.0, 100.0], "ランダム移動");
        value.params = finite(&[30.0]);
        assert_eq!(
            encode_track_value(&value),
            Ok("0,100,ランダム移動,0|30".to_string())
        );
        assert_eq!(decode_track_value("0,100,ランダム移動,0|30"), Some(value));

        let mut two = moving(&[0.0, 100.0], "ランダム移動");
        two.params = finite(&[30.0, -1.5]);
        assert_eq!(
            encode_track_value(&two),
            Ok("0,100,ランダム移動,0|30,-1.5".to_string())
        );
        assert_eq!(
            decode_track_value("0,100,ランダム移動,0|30,-1.5"),
            Some(two)
        );
    }

    #[test]
    fn an_empty_parameter_list_is_written_without_the_bar() {
        // `|` 無しの形は受理されることを観測している。`|` だけを書いた形は
        // 観測していないため出さない。
        let encoded = encode_track_value(&moving(&[0.0, 100.0], "ランダム移動")).expect("符号化");
        assert_eq!(encoded, "0,100,ランダム移動,0");
        assert!(!encoded.contains(PARAM_SEPARATOR));
        // ホストは既定値を補った形で返す。読み直しはその形も読める。
        assert_eq!(
            decode_track_value("0.00,100.00,ランダム移動,0|15")
                .expect("解析できる")
                .params,
            finite(&[15.0])
        );
        // ホストが返す「パラメータ無し」の形も読める。
        assert_eq!(
            decode_track_value("0.00,100.00,直線移動,0|")
                .expect("解析できる")
                .params,
            Vec::new()
        );
    }

    #[test]
    fn the_decoding_absorbs_the_digits_the_host_adds() {
        assert_eq!(
            decode_track_value("-600.00,600.00,直線移動,0"),
            Some(moving(&[-600.0, 600.0], "直線移動"))
        );
        assert_eq!(decode_track_value("0.00"), Some(static_value(0.0)));
    }

    #[test]
    fn the_decoding_refuses_what_it_cannot_read() {
        for raw in [
            // 移動方法の名前が無い。
            "-600.00,600.00",
            // フラグが無い。
            "-600.00,600.00,直線移動",
            // 値が 1 つも無い。
            ",直線移動,0",
            // 移動方法の位置が数値である。
            "1,2,3,4",
            // 移動を持つのに値が 1 つしかない。符号化が書けない形は読まない。
            "100,直線移動,0",
            // フラグが整数でない。
            "0,100,直線移動,x",
            // 値が数値でない。
            "0,あ,直線移動,0",
            // パラメータが数値でない。
            "0,100,直線移動,0|x",
            // 移動を持たない値にパラメータが付いている。
            "0.00|15",
            // 空文字列。
            "",
        ] {
            assert_eq!(decode_track_value(raw), None, "{raw} が解析されました");
        }
    }

    #[test]
    fn the_value_count_must_match_the_number_of_sections() {
        let modes = known_modes();
        // 区間 3 個なら値は 4 個。
        let value = moving(&[0.0, 1.0, 2.0, 3.0], "直線移動");
        assert_eq!(validate_track_value(&value, target(3, &modes)), Ok(()));
        assert_eq!(
            validate_track_value(&value, target(2, &modes)),
            Err(TrackValueError::ValueCount {
                expected: 3,
                actual: 4,
            }
            .into())
        );
        // ホストは個数の不一致を拒否しない。止められるのはここだけである。
        assert_eq!(
            validate_track_value(&moving(&[0.0, 1.0, 2.0], "直線移動"), target(1, &modes)),
            Err(TrackValueError::ValueCount {
                expected: 2,
                actual: 3,
            }
            .into())
        );
    }

    #[test]
    fn a_value_without_movement_holds_exactly_one_number() {
        let modes = known_modes();
        // 区間の数に依らず 1 個である。
        for section_count in [1, 3, 8] {
            assert_eq!(
                validate_track_value(&static_value(0.0), target(section_count, &modes)),
                Ok(())
            );
        }
        let two = TrackValue {
            values: finite(&[0.0, 1.0]),
            ..static_value(0.0)
        };
        assert_eq!(
            validate_track_value(&two, target(1, &modes)),
            Err(TrackValueError::ValueCount {
                expected: 1,
                actual: 2,
            }
            .into())
        );
    }

    #[test]
    fn movement_details_need_a_mode() {
        let modes = known_modes();
        let cases = [
            TrackValue {
                accelerate: true,
                ..static_value(0.0)
            },
            TrackValue {
                decelerate: true,
                ..static_value(0.0)
            },
            TrackValue {
                twopoint: true,
                ..static_value(0.0)
            },
            TrackValue {
                timecontrol: true,
                ..static_value(0.0)
            },
            TrackValue {
                params: finite(&[15.0]),
                ..static_value(0.0)
            },
        ];
        for value in cases {
            assert_eq!(
                validate_track_value(&value, target(1, &modes)),
                Err(TrackValueError::MovementWithoutMode.into())
            );
            assert_eq!(
                encode_track_value(&value),
                Err(TrackValueError::MovementWithoutMode.into())
            );
        }
    }

    #[test]
    fn a_mode_outside_the_known_set_is_rejected() {
        let modes = known_modes();
        assert_eq!(
            validate_track_value(&moving(&[0.0, 1.0], "存在しない移動"), target(1, &modes)),
            Err(TrackValueError::UnknownMode.into())
        );
        // 一覧は項目ごとに違う。空の一覧はどの名前も通さない。
        assert_eq!(
            validate_track_value(&moving(&[0.0, 1.0], "直線移動"), target(1, &[])),
            Err(TrackValueError::UnknownMode.into())
        );
        assert_eq!(
            validate_track_value(&moving(&[0.0, 1.0], "直線移動"), target(1, &modes)),
            Ok(())
        );
    }

    #[test]
    fn a_mode_name_may_not_carry_the_separators() {
        // 区切りを含む名前は値の個数の数え方を狂わせる。
        for mode in ["直線移動,0", "直線|移動"] {
            assert_eq!(
                encode_track_value(&moving(&[0.0, 1.0], mode)),
                Err(TextSyntaxError::ForbiddenCharacter.into()),
                "{mode}"
            );
        }
        assert_eq!(
            encode_track_value(&moving(&[0.0, 1.0], "")),
            Err(TextSyntaxError::Empty.into())
        );
        assert_eq!(
            encode_track_value(&moving(&[0.0, 1.0], "直線\u{1b}移動")),
            Err(TextSyntaxError::ContainsControl.into())
        );
    }

    #[test]
    fn the_read_back_absorbs_the_notation_but_not_a_changed_value() {
        let written = decode_track_value("-600,600,直線移動,0").expect("解析できる");
        let observed = decode_track_value("-600.00,600.00,直線移動,0").expect("解析できる");
        assert!(track_read_back_matches(&written, &observed));

        let clamped = decode_track_value("-600.00,100.00,直線移動,0").expect("解析できる");
        assert!(!track_read_back_matches(&written, &clamped));

        let other_mode = decode_track_value("-600.00,600.00,曲線移動,0").expect("解析できる");
        assert!(!track_read_back_matches(&written, &other_mode));
    }

    #[test]
    fn the_read_back_leaves_the_defaults_the_host_filled_in_alone() {
        // 空のパラメータは既定値を求める指定である。返ってきた既定値を
        // 食い違いとして扱うと、成功した書き込みが失敗になる。
        let written = decode_track_value("0,100,ランダム移動,0").expect("解析できる");
        let observed = decode_track_value("0.00,100.00,ランダム移動,0|15").expect("解析できる");
        assert!(track_read_back_matches(&written, &observed));

        // 指定したパラメータは比べる。
        let requested = decode_track_value("0,100,ランダム移動,0|30").expect("解析できる");
        assert!(!track_read_back_matches(&requested, &observed));
        assert!(track_read_back_matches(
            &requested,
            &decode_track_value("0.00,100.00,ランダム移動,0|30").expect("解析できる")
        ));
    }

    #[test]
    fn the_codec_round_trips_the_values_it_accepts() {
        let mut flagged = moving(&[1.5, -2.25, 0.0], "曲線移動");
        flagged.accelerate = true;
        flagged.twopoint = true;
        flagged.params = finite(&[15.0, -1.0]);
        let time_control = TrackValue {
            timecontrol: true,
            ..moving(&[0.0, 100.0, 50.0, 0.0], "直線移動(時間制御)")
        };
        for value in [
            static_value(0.0),
            static_value(-12.5),
            moving(&[0.0, 100.0], "直線移動"),
            time_control,
            flagged,
        ] {
            let encoded = encode_track_value(&value).expect("符号化");
            assert_eq!(
                decode_track_value(&encoded),
                Some(value.clone()),
                "{encoded} が往復しません"
            );
        }
    }

    #[test]
    fn every_track_failure_has_a_machine_readable_name() {
        let names: Vec<&str> = TrackValueError::ALL
            .iter()
            .map(TrackValueError::reason)
            .collect();
        assert_eq!(
            names,
            vec![
                "track_value_count",
                "track_mode_unknown",
                "track_movement_without_mode",
                "track_time_control_mismatch",
            ]
        );
    }

    #[test]
    fn track_failures_do_not_repeat_the_value() {
        // 移動方法の名前も値も応答へ反響させない。
        let secret = "秘密の移動";
        let error = validate_track_value(&moving(&[0.0, 1.0], secret), target(1, &[]))
            .expect_err("拒否されます");
        assert!(!error.to_string().contains(secret), "{error}");
    }
}
