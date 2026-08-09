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
//! **書き込みの検証も同じ場所に置き、符号化から迂回できない形にする。** 不正な
//! 移動方法の名前と、欠落したフラグは、ホストのプロセスを落とす。落とす形を
//! 作れるのは符号化であり、作らせない規則を別の口に置くと、その口を通らない
//! 呼び出しがそのまま落とす文字列を組み立てられる。
//!
//! **壊れ方はもう 1 つある。** フラグの 4 つ目のビットはフラグではなく、
//! パラメータ節が続くことを示す構造上の印である（[`PARAM_SECTION_BIT`]）。立った
//! 行はプロセスを落とさず、ホストが値を解けずにその項目が 1 フレームも動かなく
//! なる。**落ちないため、書いた側も読んだ側も気付かない。**

use crate::item_value::ItemWriteError;
use crate::number::FiniteF64;
use crate::validation::{
    TextSyntaxError, limit_item_value_bytes, validate_control_free, validate_item_text,
};
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

/// この型が名前を持つフラグのビット。
const NAMED_FLAGS: u32 = FLAG_ACCELERATE | FLAG_DECELERATE | FLAG_TWOPOINT;

/// パラメータ節が続くことを示す構造上の印。
///
/// **フラグではない。** ホストの UI が立て下げするのは 1 / 2 / 4 / 16 であり、
/// この位置に対応するチェックボックスは無い。立った行をホストは「パラメータ節が
/// 続く」と読み、空の節を書き、評価でその値を解けなくなる。
const PARAM_SECTION_BIT: u32 = 8;

/// 移動を持つ値が取り得る最小の要素数。区間 1 個の境界の数である。
const MIN_MOVING_VALUES: usize = 2;

/// 移動行として読み始められる最小の欄数。値 1 つ・移動方法・フラグである。
///
/// **[`MIN_MOVING_VALUES`] とは別の境界である。** 値が 1 つも無い並びは移動行と
/// して読み始められないが、値が 1 つしかない並びは読み始められたうえで書けない
/// 行である。
const MIN_MOVING_FIELDS: usize = 3;

/// トラックバー項目の値。
///
/// 区間ごとの値と、区間の間をどう補間するかをまとめて持つ。ホストは 1 本の
/// 文字列でしかこれを受け渡さないため、書き込みも読み取りもこの単位で行う。
///
/// **[`crate::effect::TrackInfo`] とは別の型である。** `TrackInfo` は読み取りが
/// 設定項目の脇へ添える情報で、所属グループ（`group_num` / `group_index` /
/// `group_name`）と時間制御の有無を持ち、区間ごとの値を持たない。いずれも
/// ホストの報告であって書き込みで指定する先が無く、1 つの型にすると書けない
/// フィールドを書き込みの入力が要求することになる。
///
/// **時間制御はこの型に無い。** 時間制御を有効にするのは移動方法の名前の変種で
/// あり、`mode` を運べば情報は失われない。`TrackInfo` が返す `timecontrol` は
/// ホストの報告であり、書き込みで指定する先が無い。
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
    /// この型が名前を持たないフラグのビット。
    ///
    /// 復号は既知の 3 ビットを除いた残りをここへ入れ、符号化は 3 つの真偽値から
    /// 組み立てたビットへ重ねる。**名前が無いことと、落としてよいことは別で
    /// ある。** ホストの UI にはこの位置へ現れるチェックボックスがあり、落とすと
    /// 読み取った値を書き戻したときにその設定が消える。
    ///
    /// 名前を持つビットの位置は、ここではなく 3 つの真偽値が表す。両方から同じ
    /// 整数を綴れる形は書き込みが拒否する（[`validate_track_value`]）。
    pub reserved_flags: u32,
}

/// ホストが受け付ける移動方法 1 件。
///
/// **名前と、その名前で書けるかどうかを 1 つの値として運ぶ。**
///
/// `writable` が偽の移動方法は、登録されていて名前としては正しいが、書き込むと
/// 読み直しがその移動を失う。移動を消すには [`TrackValue::mode`] を `None` に
/// する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Movement {
    /// 移動方法の名前。
    pub name: String,
    /// この名前で移動を書けるか。
    pub writable: bool,
}

/// トラックバーの移動を表す値の検証失敗。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
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
    UnknownMode {
        /// 判定に用いた、ホストが受け付ける移動方法の一覧。
        ///
        /// 要求元が指定した名前ではなく、[`TrackWriteTarget::movements`] が
        /// 運んでいたホストの状態である。要求元がここへ書き直せば通り得る値の
        /// 集合そのものであるため、エラー応答へ載せてよい。
        known: Vec<Movement>,
    },
    /// 移動方法は一覧に在るが、その名前では書けない。
    ///
    /// **[`TrackValueError::UnknownMode`] と別に立てる。** 名前が無いのなら
    /// 一覧から選び直せば通るが、書けない名前ではどう選び直しても通らない。
    /// 移動を消す指定（[`TrackValue::mode`] を `None`）へ持ち替えるほかない。
    ///
    /// **値を持たない。** 一覧を添えても、要求元が次に取る手は変わらない。
    #[error("この移動方法は一覧にありますが書き込めません")]
    ModeNotWritable,
    /// 移動方法の名前が数値として読める。
    #[error("移動方法の名前に数値として読める文字列は指定できません")]
    ModeReadsAsNumber,
    /// 移動を持たない値に、移動の付帯情報が指定された。
    #[error("移動を持たない値にフラグとパラメータは指定できません")]
    MovementWithoutMode,
    /// 名前を持たないビットに、この型が表せない値が指定された。
    #[error("移動のフラグに、この形では表せないビットが含まれています")]
    FlagsNotRepresentable,
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
        TrackValueError::UnknownMode { known: Vec::new() },
        TrackValueError::ModeNotWritable,
        TrackValueError::ModeReadsAsNumber,
        TrackValueError::MovementWithoutMode,
        TrackValueError::FlagsNotRepresentable,
    ];

    /// 失敗の種別を表す機械可読な名前を返す。
    ///
    /// 検証対象の値そのものを含まない。
    pub fn reason(&self) -> &'static str {
        match self {
            TrackValueError::ValueCount { .. } => "track_value_count",
            TrackValueError::UnknownMode { .. } => "track_mode_unknown",
            TrackValueError::ModeNotWritable => "track_mode_not_writable",
            TrackValueError::ModeReadsAsNumber => "track_mode_reads_as_number",
            TrackValueError::MovementWithoutMode => "track_movement_without_mode",
            TrackValueError::FlagsNotRepresentable => "track_flags_not_representable",
        }
    }
}

/// ホストが返した文字列を移動として読めなかった理由。
///
/// **2 つに分ける。** 移動行かどうかを値の側でしか見分けられない場面があり、
/// そこでは「移動行ではない」と「壊れた移動行である」を別に扱う必要がある。
/// 1 つに畳むと、壊れた移動行が移動行でないものと同じ扱いになる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TrackDecodeError {
    /// 移動行として読み始められない。
    ///
    /// テキスト・色・パスなど、そもそも移動を表していない文字列がここへ来る。
    #[error("移動を表す文字列ではありません")]
    NotAMovement,
    /// 移動行として読み始められたが、表せる値にならない。
    ///
    /// 壊れた移動行である。読めたふりをすると、書き戻したときに我々が捏造した
    /// 移動がホストへ渡る。
    #[error("移動を表していますが、値として表せません: {0}")]
    NotRepresentable(#[from] UnrepresentableMovement),
}

/// 移動行として読み始められた値が、表せなかった原因。
///
/// **原因ごとに分ける。** 要求元が直す先が違う——フラグは整数を書き直し、
/// 境界の数は値を足す。**「表せたか」だけを見る呼び出し元は
/// [`TrackDecodeError::NotRepresentable`] のままで足り、原因まで見るのは
/// 失敗の名前を要求元へ返す側だけである。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UnrepresentableMovement {
    /// フラグに、この型が表せないビットが立っている。
    #[error("移動のフラグに、この形では表せないビットが含まれています")]
    Flags,
    /// 移動を持つ値の境界が足りない。
    #[error("移動の値は区間の境界の数だけ必要です")]
    ValueCount,
}

impl UnrepresentableMovement {
    /// 同じ事実に、書き込みの検証が与える失敗。
    ///
    /// **要求元が直す先は経路で変わらない。** フラグは整数を書き直すことでしか
    /// 直らず、境界は値を足すことでしか直らない。
    ///
    /// 境界が足りない行は必ず値を 1 つだけ持つ——読み始めるには値の欄が
    /// 1 つ以上要り（[`MIN_MOVING_FIELDS`]）、2 つ以上あれば表せる
    /// （[`MIN_MOVING_VALUES`]）。比べる相手は書式が要求する最小値であり、
    /// 対象の区間の数は復号が見ない。
    pub fn as_write_error(self) -> TrackValueError {
        match self {
            UnrepresentableMovement::Flags => TrackValueError::FlagsNotRepresentable,
            UnrepresentableMovement::ValueCount => TrackValueError::ValueCount {
                expected: MIN_MOVING_VALUES,
                actual: 1,
            },
        }
    }
}

/// 移動を書き込む対象の性質。
///
/// 区間の数も、ホストが受け付ける移動方法も、対象と実行環境を見なければ決まら
/// ない。値だけでは判定できないため、呼び出し側が渡す。
///
/// **省略できる形にしない。** 省略を許すと、渡さない呼び出しが検証を通らずに
/// 符号化へ届く。ホストは一覧に無い移動方法でプロセスごと落ちるため、検証を
/// 迂回できる経路が 1 つでもあれば、そこが落とす経路になる。
///
/// `section_count` は [`crate::object::ObjectDetail::sections`] の要素数と一致
/// する。値の個数が区間の境界の数（alias の `frame=` の要素数）と一致するという
/// 観測から、「区間数 + 1」を導いている。
///
/// `movements` が空であれば、移動を持つ値は 1 つも書けない。移動方法の一覧を
/// 引けない環境で移動を通すと、その場でプロセスが落ちる。
#[derive(Debug, Clone, Copy)]
pub struct TrackWriteTarget<'a> {
    /// 対象オブジェクトの区間の数。中間点が 2 個なら 3。
    pub section_count: usize,
    /// ホストが受け付ける移動方法。
    ///
    /// 失敗の応答へ載せる一覧そのものであり、書き込みの可否もここから決まる。
    pub movements: &'a [Movement],
}

/// フラグのビット列を組み立てる。
///
/// 名前を持つ 3 つのビットへ、名前を持たない分（[`TrackValue::reserved_flags`]）を
/// 重ねる。ホストが受け渡すのはこの整数 1 つであり、名前の有無は我々の側の区別で
/// しかない。
fn flag_bits(value: &TrackValue) -> u32 {
    let bit = |enabled: bool, mask: u32| if enabled { mask } else { 0 };
    bit(value.accelerate, FLAG_ACCELERATE)
        | bit(value.decelerate, FLAG_DECELERATE)
        | bit(value.twopoint, FLAG_TWOPOINT)
        | value.reserved_flags
}

/// 対象を見なくても判定できる規則だけを検証する。
///
/// ここで見るのは、書式そのものを壊す指定と、値の中で閉じた矛盾である。
///
/// - 移動を持たない値は 1 要素であり、フラグもパラメータも持たない
/// - 移動方法の名前は区切り文字を含まない。含むと値の個数の数え方が狂い、
///   末尾から移動方法を取るホストの解析が別の位置を指す
/// - 移動方法の名前は数値として読めない。**区切り文字とまったく同じ失敗を
///   起こす。** 末尾から数えて移動方法の位置に数値が来ると、ホストはそれを
///   移動方法の名前として引き当てられずに例外を投げる
/// - 移動を持つ値は 2 要素以上である。区間は必ず 1 つ以上あるためで、
///   正確な個数は区間の数を渡す [`validate_track_value`] が見る
/// - 名前を持たないビット（[`TrackValue::reserved_flags`]）が、この型で表せる
///   値である。表せないビットは 2 つに分かれる——[`PARAM_SECTION_BIT`] は
///   ホストが移動として評価できず、[`NAMED_FLAGS`] の位置は 3 つの真偽値が
///   表す。どちらも同じ理由で拒否する。**黙って落とさない。** 落とせば要求元は
///   自分が何を書いたか分からなくなり、通せば、評価の死んだ項目か、同じ整数を
///   2 通りに綴れる値ができる
pub(crate) fn validate_track_syntax(value: &TrackValue) -> Result<(), ItemWriteError> {
    let Some(mode) = value.mode.as_deref() else {
        if value.values.len() != 1 {
            return Err(TrackValueError::ValueCount {
                expected: 1,
                actual: value.values.len(),
            }
            .into());
        }
        if !value.params.is_empty() || flag_bits(value) != 0 {
            return Err(TrackValueError::MovementWithoutMode.into());
        }
        return Ok(());
    };
    if value.reserved_flags & (NAMED_FLAGS | PARAM_SECTION_BIT) != 0 {
        return Err(TrackValueError::FlagsNotRepresentable.into());
    }
    validate_item_text(mode)?;
    if mode.is_empty() {
        return Err(TextSyntaxError::Empty.into());
    }
    if mode.contains(FIELD_SEPARATOR) || mode.contains(PARAM_SEPARATOR) {
        return Err(TextSyntaxError::ForbiddenCharacter.into());
    }
    if reads_as_number(mode) {
        return Err(TrackValueError::ModeReadsAsNumber.into());
    }
    if value.values.len() < MIN_MOVING_VALUES {
        return Err(TrackValueError::ValueCount {
            expected: MIN_MOVING_VALUES,
            actual: value.values.len(),
        }
        .into());
    }
    Ok(())
}

/// 対象の性質と突き合わせて、書き込んでよい値かを判定する。
///
/// [`validate_track_syntax`] の規則に加えて次を見る。
///
/// - 移動を持つ値の要素数は「区間数 + 1」である。**多い場合をホストは拒否せず、
///   余った値は保存されるが評価に使われない**（観測）。**少ない場合の挙動は
///   観測していない。** どちらにせよ止められるのはここだけである
/// - 移動方法の名前が、呼び出し側が渡した一覧に含まれる。**含まれていても
///   書けない名前は別の失敗になる**——一覧から選び直しても通らないためである
///
/// **移動方法の検証は「選択肢はヒントであってゲートではない」という規則の例外で
/// ある。** 選択肢の候補は一覧に無い値でも通す。ホストが受け付ける値の全体像を
/// 観測できておらず、通るはずの値を我々が拒む方が害が大きいためである。移動方法
/// は事情が違う。一覧に無い名前を渡すとホストは例外を投げ、それが `extern "C"`
/// の境界を越えてプロセスごと落ちる。**通す選択肢が無い。**
///
/// **一覧は項目ごとに違い得る。** 同じ一覧で全項目を検証すると、その項目だけが
/// 持つ移動方法を拒み、逆に他の項目にしか無い名前を通す。前者は書けるはずの値が
/// 書けなくなるだけだが、後者はホストを落とす。呼び出し側は取り得る名前の全体を
/// 渡すのではなく、実行環境が受け付ける名前を渡す。
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
    let Some(movement) = target.movements.iter().find(|known| known.name == mode) else {
        return Err(TrackValueError::UnknownMode {
            known: target.movements.to_vec(),
        }
        .into());
    };
    if !movement.writable {
        return Err(TrackValueError::ModeNotWritable.into());
    }
    Ok(())
}

/// 値をホストへ渡す文字列へ符号化する。
///
/// **検証を内側で行う。** 対象の性質を受け取らなければ符号化できないため、
/// 検証を通っていない文字列がホストへ届く経路が型として存在しない。
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
/// を出す。
///
/// **空でない `params` は `|` を伴って書く。この形の要求が受理されることは観測
/// していない。** 書けると判断した根拠は 2 つで、どちらも推論である——ホストが
/// 保存した形がまさに `ランダム移動,0|15` であること、および SDK が設定値の
/// setter へ渡す文字列をエイリアスファイルの設定値と同じ書式と定めていること。
/// 書かない選択肢は無い。読み取ったパラメータを落とすと、読めた値を書き戻せない。
///
/// 移動を持たない値は単一の数値になる。移動方法もフラグも書かない。
pub fn encode_track_value(
    value: &TrackValue,
    target: TrackWriteTarget<'_>,
) -> Result<String, ItemWriteError> {
    validate_track_value(value, target)?;
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
/// **解析できない文字列では [`TrackDecodeError`] を返す。** 呼び出し側は生の
/// 文字列を [`ItemValue::Unknown`](crate::item_value::ItemValue::Unknown) として
/// 保つ。推測して部分的に埋めた値を返すと、それを書き戻したときにホストへ渡るの
/// は我々が捏造した移動になる。**読めなかったことは、読めたふりより安全である。**
///
/// 判定は次の順に進む。
///
/// - 区切りが無く 1 つの有限な数値であれば、移動を持たない値とする
/// - 3 つ以上の欄があれば、末尾をフラグ、その 1 つ前を移動方法の名前、残りを
///   値とする。フラグは非負整数、名前は空でなく、数値として読めず、制御文字を
///   含まないことを要求する。**ここまでが「移動行として読み始められるか」の
///   判定であり、届かなかった文字列は [`TrackDecodeError::NotAMovement`] に
///   なる**
/// - フラグに [`PARAM_SECTION_BIT`] が立った行は表せない。ホストはその行を
///   「パラメータ節が続く」と読んで評価で値を解けなくなり、項目は動かない。
///   フラグとして運べば符号化がその行を書き戻せてしまう
/// - 値と、`|` より後ろのパラメータを有限な数値として読む。**読めない欄が
///   あれば移動行ではない**——`こんにちは,さようなら,0` のように、末尾が
///   整数でその手前が数値として読めないだけのテキストがこの形になる。`|` の
///   後ろが空のときはパラメータ無しとする。ホストはパラメータを持たない
///   移動方法を `直線移動,0|` の形で返す
/// - 値が 1 つしかない移動は表せない。区間は必ず 1 つ以上あるため、移動を持つ
///   値の境界は 2 つ以上になる
///
/// **フラグの判定は値を読むより先に置く。** その行をホストがパラメータ節の印と
/// 読むかはフラグの整数だけで決まり、値が数として読めるかに依らない。
///
/// **境界の数の判定は値を読んだ後に置く。** 先に置くと、値の欄が数として読めない
/// テキストが境界の数を理由に拒否される。
///
/// 読み始められたうえで表せなかった行は
/// [`TrackDecodeError::NotRepresentable`] になる。**この境界は移動行かどうかの
/// 見分けに使われる。** 広く取れば壊れた移動行が移動行でない値として素通しし、
/// 狭く取ればテキストや色が壊れた移動行として拒否される。
///
/// **個数・移動方法の位置・文字種は符号化の定義域へ揃えてある。** 読めた値は
/// 書式としては書き戻せる。**揃っていないのは長さの上限だけである**——上限は
/// 符号化後の文字列に掛かるため、ホストが上限を超える文字列を返した場合は、
/// 読めても書き戻せない。移動方法の一覧と区間の数も、対象を見なければ判定でき
/// ないため復号は見ない。
///
/// 名前を持つ 3 つのビット以外は [`TrackValue::reserved_flags`] へ入れる。
pub fn decode_track_value(raw: &str) -> Result<TrackValue, TrackDecodeError> {
    let (head, tail) = match raw.split_once(PARAM_SEPARATOR) {
        Some((head, tail)) => (head, Some(tail)),
        None => (raw, None),
    };
    let fields: Vec<&str> = head.split(FIELD_SEPARATOR).collect();
    if fields.len() == 1 {
        // 移動を持たない値はパラメータを取らない。
        if tail.is_some() {
            return Err(TrackDecodeError::NotAMovement);
        }
        let value = parse_finite(fields[0]).ok_or(TrackDecodeError::NotAMovement)?;
        return Ok(TrackValue {
            values: vec![value],
            mode: None,
            params: Vec::new(),
            accelerate: false,
            decelerate: false,
            twopoint: false,
            reserved_flags: 0,
        });
    }
    if fields.len() < MIN_MOVING_FIELDS {
        return Err(TrackDecodeError::NotAMovement);
    }
    let flags: u32 = fields[fields.len() - 1]
        .trim()
        .parse()
        .map_err(|_| TrackDecodeError::NotAMovement)?;
    let mode = fields[fields.len() - 2];
    if mode.is_empty() || reads_as_number(mode) || validate_control_free(mode).is_err() {
        return Err(TrackDecodeError::NotAMovement);
    }
    if flags & PARAM_SECTION_BIT != 0 {
        return Err(UnrepresentableMovement::Flags.into());
    }
    let fields = &fields[..fields.len() - 2];
    let values = fields
        .iter()
        .copied()
        .map(parse_finite)
        .collect::<Option<Vec<FiniteF64>>>()
        .ok_or(TrackDecodeError::NotAMovement)?;
    if values.len() < MIN_MOVING_VALUES {
        return Err(UnrepresentableMovement::ValueCount.into());
    }
    let params = match tail {
        None | Some("") => Vec::new(),
        Some(tail) => tail
            .split(FIELD_SEPARATOR)
            .map(parse_finite)
            .collect::<Option<Vec<FiniteF64>>>()
            .ok_or(TrackDecodeError::NotAMovement)?,
    };
    Ok(TrackValue {
        values,
        mode: Some(mode.to_string()),
        params,
        accelerate: flags & FLAG_ACCELERATE != 0,
        decelerate: flags & FLAG_DECELERATE != 0,
        twopoint: flags & FLAG_TWOPOINT != 0,
        reserved_flags: flags & !NAMED_FLAGS,
    })
}

/// 十進表記を有限な実数として読む。
fn parse_finite(raw: &str) -> Option<FiniteF64> {
    raw.trim().parse::<f64>().ok().and_then(FiniteF64::try_new)
}

/// 移動方法の名前の位置にある文字列が、数値として読めるか。
///
/// 符号化と復号が同じ判定を用いる。片方だけが数値として読むと、書けるのに
/// 読めない名前ができ、書き戻しの照合が必ず不一致になる。
fn reads_as_number(mode: &str) -> bool {
    parse_finite(mode).is_some()
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
/// 成功した書き込みが失敗として返る。
///
/// **空でないときに比べるのは、観測 1 件からの推論である。** 観測したのは
/// 「パラメータを 1 つも書かない要求に既定値が補われる」ことだけで、足りない分
/// だけを補う移動方法があるかは観測していない。あれば、要求より長い読み直しが
/// 不一致になり、成功した書き込みが失敗として返る。それでもこの規則を採るのは、
/// 比べない側の誤りが「要求と違うパラメータが入ったことを黙って見逃す」ことだから
/// である。**偽の失敗は、黙った破壊より安全な側である。**
///
/// **フラグは組み立てた整数どうしで比べる。** ホストが受け渡すのはその整数 1 つ
/// であり、どのビットに名前が付いているかは我々の側の区別でしかない。
pub(crate) fn track_read_back_matches(written: &TrackValue, observed: &TrackValue) -> bool {
    if written.values != observed.values
        || written.mode != observed.mode
        || flag_bits(written) != flag_bits(observed)
    {
        return false;
    }
    written.params.is_empty() || written.params == observed.params
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

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
            reserved_flags: 0,
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
            reserved_flags: 0,
        }
    }

    fn writable(name: &str) -> Movement {
        Movement {
            name: name.to_string(),
            writable: true,
        }
    }

    fn unwritable(name: &str) -> Movement {
        Movement {
            name: name.to_string(),
            writable: false,
        }
    }

    fn movements() -> Vec<Movement> {
        [
            "直線移動",
            "曲線移動",
            "ランダム移動",
            "直線移動(時間制御)",
            "再生範囲",
        ]
        .iter()
        .map(|name| writable(name))
        .collect()
    }

    fn target(section_count: usize, movements: &[Movement]) -> TrackWriteTarget<'_> {
        TrackWriteTarget {
            section_count,
            movements,
        }
    }

    /// 生成した値をそのまま受け入れる対象。符号化そのものを見るときに使う。
    fn matching_target<'a>(value: &TrackValue, movements: &'a [Movement]) -> TrackWriteTarget<'a> {
        TrackWriteTarget {
            section_count: value.values.len().saturating_sub(1),
            movements,
        }
    }

    fn encoded(value: &TrackValue) -> Result<String, ItemWriteError> {
        let movements = movements();
        encode_track_value(value, matching_target(value, &movements))
    }

    #[test]
    fn the_encoding_matches_the_alias_notation() {
        assert_eq!(
            encoded(&moving(&[-500.0, 500.0], "直線移動")),
            Ok("-500,500,直線移動,0".to_string())
        );
        assert_eq!(
            encoded(&moving(&[0.0, 100.0, 0.0], "曲線移動")),
            Ok("0,100,0,曲線移動,0".to_string())
        );
        // 移動を持たない値は単一の数値になる。区間の数に依らない。
        assert_eq!(encoded(&static_value(0.0)), Ok("0".to_string()));
    }

    #[test]
    fn each_flag_owns_its_own_bit() {
        // ビットの割り当てを 1 つずつ固定する。複数を同時に立てた組み合わせ
        // だけを見ると、bit0 と bit2 を入れ替えても検査が通ってしまう。
        let cases = [
            (
                TrackValue {
                    accelerate: true,
                    ..moving(&[0.0, 100.0], "直線移動")
                },
                "0,100,直線移動,1",
            ),
            (
                TrackValue {
                    decelerate: true,
                    ..moving(&[0.0, 100.0], "直線移動")
                },
                "0,100,直線移動,2",
            ),
            (
                TrackValue {
                    twopoint: true,
                    ..moving(&[0.0, 100.0], "直線移動")
                },
                "0,100,直線移動,4",
            ),
        ];
        for (value, expected) in cases {
            assert_eq!(encoded(&value), Ok(expected.to_string()));
            // 復号も同じ割り当てで読む。往復だけでは割り当てを固定できない。
            assert_eq!(decode_track_value(expected), Ok(value));
        }
    }

    #[test]
    fn the_flag_bits_are_the_ones_the_host_reports() {
        // 実測: `,5` は加速と中間点無視、`,7` は加速・減速・中間点無視。
        let mut value = moving(&[-500.0, 500.0], "直線移動");
        value.accelerate = true;
        value.twopoint = true;
        assert_eq!(encoded(&value), Ok("-500,500,直線移動,5".to_string()));
        assert_eq!(decode_track_value("-500,500,直線移動,5"), Ok(value.clone()));

        value.decelerate = true;
        assert_eq!(encoded(&value), Ok("-500,500,直線移動,7".to_string()));
        assert_eq!(decode_track_value("-500,500,直線移動,7"), Ok(value));
    }

    #[test]
    fn the_fourth_bit_is_refused_by_the_decoding_because_the_host_cannot_evaluate_it() {
        // 実測: 末尾フラグだけを 8 にした項目は 1 フレームも動かない。同じ
        // オブジェクトの別項目を 0 にした場合は動く。ホストの UI が立て下げる
        // ビットは 1 / 2 / 4 / 16 であり、この位置は含まれない——立てるとホストは
        // 空のパラメータ節を書き、評価で値を解けなくなる。
        //
        // 3 つのフラグを偽として読むと、動かない項目を「移動を持つ」と報告し、
        // 書き戻しでその状態が黙って直る。**移動行として読み始められはするため、
        // 移動行ではない値とは別の理由で失敗する。**
        for raw in [
            "-600,600,直線移動,8",
            "-600,600,直線移動,8|",
            "0,100,ランダム移動,8|30",
            // 値の個数が書けない形であっても、先にこのビットで落ちる。
            "100,直線移動,8",
            "100,直線移動,8|",
            // 値が数値として読めなくても落ちる。その行をホストがパラメータ節の
            // 印と読むかは、フラグの整数だけで決まる。
            "あ,い,直線移動,8",
        ] {
            assert_eq!(
                decode_track_value(raw),
                Err(UnrepresentableMovement::Flags.into()),
                "{raw}"
            );
        }
    }

    #[test]
    fn the_named_bits_read_and_write_as_they_did() {
        for (raw, accelerate, decelerate, twopoint) in [
            ("-600,600,直線移動,0", false, false, false),
            ("-600,600,直線移動,4", false, false, true),
            ("-600,600,直線移動,7", true, true, true),
        ] {
            let decoded = decode_track_value(raw).expect("解析できる");
            assert_eq!(
                (decoded.accelerate, decoded.decelerate, decoded.twopoint),
                (accelerate, decelerate, twopoint),
                "{raw}"
            );
            assert_eq!(decoded.reserved_flags, 0, "{raw}");
            assert_eq!(encoded(&decoded), Ok(raw.to_string()));
        }
    }

    #[test]
    fn the_bits_without_a_name_survive_the_round_trip() {
        // 実測: `直線移動,16` は `|` を足されずに保存され、評価も生きている。
        // ホストの UI が持つ 4 つ目のチェックボックスであり、どの設定かは特定
        // できていない。落とせば、読み取った値を書き戻したときに消える。
        let decoded = decode_track_value("-600.00,600.00,直線移動,16").expect("解析できる");
        assert_eq!(decoded.reserved_flags, 16);
        assert!(!decoded.accelerate);
        assert!(!decoded.decelerate);
        assert!(!decoded.twopoint);
        assert_eq!(encoded(&decoded), Ok("-600,600,直線移動,16".to_string()));

        // 名前を持つ 3 つと重なっても、同じ整数へ戻る。
        let mixed = decode_track_value("-600.00,600.00,直線移動,23").expect("解析できる");
        assert_eq!(mixed.reserved_flags, 16);
        assert!(mixed.accelerate);
        assert!(mixed.decelerate);
        assert!(mixed.twopoint);
        assert_eq!(encoded(&mixed), Ok("-600,600,直線移動,23".to_string()));
    }

    #[test]
    fn time_control_is_a_variant_of_the_mode_name() {
        // 実測: `直線移動(時間制御)` を書くと時間制御が有効になる。フラグの欄は
        // 0 のままであり、時間制御を表すビットは無い。名前を運べば足りる。
        let decoded = decode_track_value("0,100,直線移動(時間制御),0").expect("解析できる");
        assert_eq!(decoded.mode.as_deref(), Some("直線移動(時間制御)"));
        assert_eq!(
            encoded(&decoded),
            Ok("0,100,直線移動(時間制御),0".to_string())
        );
    }

    #[test]
    fn the_parameters_follow_the_flags_after_a_bar() {
        let mut value = moving(&[0.0, 100.0], "ランダム移動");
        value.params = finite(&[30.0]);
        assert_eq!(encoded(&value), Ok("0,100,ランダム移動,0|30".to_string()));
        assert_eq!(decode_track_value("0,100,ランダム移動,0|30"), Ok(value));

        let mut two = moving(&[0.0, 100.0], "ランダム移動");
        two.params = finite(&[30.0, -1.5]);
        assert_eq!(
            encoded(&two),
            Ok("0,100,ランダム移動,0|30,-1.5".to_string())
        );
        assert_eq!(decode_track_value("0,100,ランダム移動,0|30,-1.5"), Ok(two));
    }

    #[test]
    fn an_empty_parameter_list_is_written_without_the_bar() {
        // `|` 無しの形は受理されることを観測している。`|` だけを書いた形は
        // 観測していないため出さない。
        let encoded = encoded(&moving(&[0.0, 100.0], "ランダム移動")).expect("符号化");
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
            Ok(moving(&[-600.0, 600.0], "直線移動"))
        );
        assert_eq!(decode_track_value("0.00"), Ok(static_value(0.0)));
    }

    #[test]
    fn what_cannot_be_read_as_a_movement_at_all_is_refused_as_such() {
        // 移動行として読み始められない。末尾がフラグ整数で、その手前がモード名
        // として読めるところまで進めなかったものである。
        for raw in [
            // 移動方法の名前が無い。
            "-600.00,600.00",
            // フラグが無い。
            "-600.00,600.00,直線移動",
            // 値の欄が 1 つも無い。
            "直線移動,0",
            "あ,0",
            // 移動方法の位置が数値である。
            "1,2,3,4",
            "0,100,1e3,0",
            // 移動方法の名前に制御文字が含まれる。
            "0,100,直線\u{1b}移動,0",
            // フラグが整数でない。
            "0,100,直線移動,x",
            "あ,い,う",
            // 移動を持たない値にパラメータが付いている。
            "0.00|15",
            // 移動を表していない生値。
            "ffffff",
            r"C:\a.png",
            // 空文字列。
            "",
            // 値の欄が数値として読めない。**末尾が整数でその手前が数値として
            // 読めないだけのテキストがこの形になる**——移動行として扱うと、
            // フラグの問題でもない値がフラグを理由に拒否される。
            "こんにちは,さようなら,0",
            "第1章,序,0",
            "A,B,1",
            "0,あ,直線移動,0",
            ",直線移動,0",
            // パラメータの欄が数値として読めない。
            "0,100,直線移動,0|x",
        ] {
            assert_eq!(
                decode_track_value(raw),
                Err(TrackDecodeError::NotAMovement),
                "{raw} が解析されました"
            );
        }
    }

    #[test]
    fn a_movement_row_whose_boundaries_do_not_reach_a_section_is_refused_as_a_movement_row() {
        // 移動行として読み始められ、値も読めたうえで、区間を 1 つも表していない
        // ものである。移動行ではない値と同じ理由に畳むと、壊れた行を素通しする
        // 判定を書ける。
        for raw in ["100,直線移動,0", "100,直線移動,0|15"] {
            assert_eq!(
                decode_track_value(raw),
                Err(UnrepresentableMovement::ValueCount.into()),
                "{raw} が解析されました"
            );
        }
    }

    #[test]
    fn the_cause_of_an_unrepresentable_row_names_what_the_write_would_name() {
        // 要求元が直す先は経路で変わらない。ホストが返した文字列として読んでも、
        // 要求として受け取っても、同じ事実には同じ名前が付く。
        assert_eq!(
            UnrepresentableMovement::Flags.as_write_error().reason(),
            "track_flags_not_representable"
        );
        assert_eq!(
            UnrepresentableMovement::ValueCount
                .as_write_error()
                .reason(),
            "track_value_count"
        );
        // 境界が足りない行は必ず値を 1 つだけ持つ。
        assert_eq!(
            UnrepresentableMovement::ValueCount.as_write_error(),
            TrackValueError::ValueCount {
                expected: MIN_MOVING_VALUES,
                actual: 1,
            }
        );
    }

    #[test]
    fn the_value_count_must_match_the_number_of_sections() {
        let movements = movements();
        // 区間 3 個なら値は 4 個。
        let value = moving(&[0.0, 1.0, 2.0, 3.0], "直線移動");
        assert_eq!(validate_track_value(&value, target(3, &movements)), Ok(()));
        assert_eq!(
            validate_track_value(&value, target(2, &movements)),
            Err(TrackValueError::ValueCount {
                expected: 3,
                actual: 4,
            }
            .into())
        );
        // ホストは多い側を拒否しない。止められるのはここだけである。
        assert_eq!(
            validate_track_value(&moving(&[0.0, 1.0, 2.0], "直線移動"), target(1, &movements)),
            Err(TrackValueError::ValueCount {
                expected: 2,
                actual: 3,
            }
            .into())
        );
        // 符号化は検証を内側で行う。個数が合わない値は文字列にならない。
        assert_eq!(
            encode_track_value(&value, target(2, &movements)),
            Err(TrackValueError::ValueCount {
                expected: 3,
                actual: 4,
            }
            .into())
        );
    }

    #[test]
    fn a_value_without_movement_holds_exactly_one_number() {
        let movements = movements();
        // 区間の数に依らず 1 個である。
        for section_count in [1, 3, 8] {
            assert_eq!(
                validate_track_value(&static_value(0.0), target(section_count, &movements)),
                Ok(())
            );
        }
        let two = TrackValue {
            values: finite(&[0.0, 1.0]),
            ..static_value(0.0)
        };
        assert_eq!(
            validate_track_value(&two, target(1, &movements)),
            Err(TrackValueError::ValueCount {
                expected: 1,
                actual: 2,
            }
            .into())
        );
    }

    #[test]
    fn movement_details_need_a_mode() {
        let movements = movements();
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
                params: finite(&[15.0]),
                ..static_value(0.0)
            },
        ];
        for value in cases {
            assert_eq!(
                validate_track_value(&value, target(1, &movements)),
                Err(TrackValueError::MovementWithoutMode.into())
            );
            assert_eq!(
                encode_track_value(&value, target(1, &movements)),
                Err(TrackValueError::MovementWithoutMode.into())
            );
        }
    }

    #[test]
    fn a_mode_outside_the_known_set_is_rejected() {
        let movements = movements();
        // 拒否は、判定に使った一覧をそのまま運ぶ。要求元はここへ書き直せば
        // 通り得る名前を、対象を読み直さずに知れる。
        assert_eq!(
            validate_track_value(
                &moving(&[0.0, 1.0], "存在しない移動"),
                target(1, &movements)
            ),
            Err(TrackValueError::UnknownMode {
                known: movements.clone()
            }
            .into())
        );
        // 一覧を引けなければ移動は 1 つも書けない。通す選択肢は無い。一覧が
        // 空であることも、その空の一覧としてそのまま運ぶ。
        assert_eq!(
            validate_track_value(&moving(&[0.0, 1.0], "直線移動"), target(1, &[])),
            Err(TrackValueError::UnknownMode { known: Vec::new() }.into())
        );
        assert_eq!(
            encode_track_value(
                &moving(&[0.0, 1.0], "存在しない移動"),
                target(1, &movements)
            ),
            Err(TrackValueError::UnknownMode {
                known: movements.clone()
            }
            .into())
        );
        assert_eq!(
            validate_track_value(&moving(&[0.0, 1.0], "直線移動"), target(1, &movements)),
            Ok(())
        );
    }

    #[test]
    fn a_mode_name_that_reads_as_a_number_is_rejected_before_the_host_sees_it() {
        // 区切り文字と同じ失敗を起こす。ホストは末尾から数えた位置に数値を
        // 見つけて例外を投げる。一覧に載っていても通さない。
        let movements: Vec<Movement> = ["12", "-1.5", "1e3", " 7 "]
            .iter()
            .map(|name| writable(name))
            .collect();
        for mode in movements.iter().map(|movement| movement.name.as_str()) {
            let value = moving(&[0.0, 1.0], mode);
            assert_eq!(
                encode_track_value(&value, target(1, &movements)),
                Err(TrackValueError::ModeReadsAsNumber.into()),
                "{mode}"
            );
            // 復号も同じ名前を読まない。片側だけが読むと照合が必ず外れる。
            assert_eq!(
                decode_track_value(&format!("0,1,{mode},0")),
                Err(TrackDecodeError::NotAMovement),
                "{mode}"
            );
        }
    }

    #[test]
    fn a_mode_name_may_not_carry_the_separators() {
        let movements = movements();
        // 区切りを含む名前は値の個数の数え方を狂わせる。
        for mode in ["直線移動,0", "直線|移動"] {
            assert_eq!(
                encode_track_value(&moving(&[0.0, 1.0], mode), target(1, &movements)),
                Err(TextSyntaxError::ForbiddenCharacter.into()),
                "{mode}"
            );
        }
        assert_eq!(
            encode_track_value(&moving(&[0.0, 1.0], ""), target(1, &movements)),
            Err(TextSyntaxError::Empty.into())
        );
        assert_eq!(
            encode_track_value(
                &moving(&[0.0, 1.0], "直線\u{1b}移動"),
                target(1, &movements)
            ),
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
    fn the_read_back_compares_the_flag_integer_the_host_stores() {
        // 名前を持たないビットが読み直しで消えていれば、要求した移動は入って
        // いない。3 つの真偽値だけを比べると、その欠落を成功として返す。
        let written = decode_track_value("-500,500,直線移動,16").expect("解析できる");
        assert!(!track_read_back_matches(
            &written,
            &decode_track_value("-500.00,500.00,直線移動,0").expect("解析できる")
        ));
        assert!(track_read_back_matches(
            &written,
            &decode_track_value("-500.00,500.00,直線移動,16").expect("解析できる")
        ));
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
        for value in [
            static_value(0.0),
            static_value(-12.5),
            moving(&[0.0, 100.0], "直線移動"),
            moving(&[0.0, 100.0, 50.0, 0.0], "直線移動(時間制御)"),
            moving(&[0.0, 0.92], "再生範囲"),
            flagged,
        ] {
            let encoded = encoded(&value).expect("符号化");
            assert_eq!(
                decode_track_value(&encoded),
                Ok(value.clone()),
                "{encoded} が往復しません"
            );
        }
    }

    #[test]
    fn no_value_the_decoding_produces_can_encode_the_fourth_bit() {
        // 復号が 4 つ目のビットを保持すると、読んだ値を書き戻すだけで評価の
        // 死んだ行を作れる。フラグの全域を走査し、読めた値が同じ整数へ戻ること
        // と、その整数がこのビットを含まないことを確かめる。
        for flags in 0..64u32 {
            let raw = format!("0,100,直線移動,{flags}");
            let Ok(decoded) = decode_track_value(&raw) else {
                continue;
            };
            let encoded = encoded(&decoded).expect("符号化");
            let written: u32 = encoded
                .rsplit(FIELD_SEPARATOR)
                .next()
                .expect("フラグの欄がある")
                .parse()
                .expect("フラグは整数");
            assert_eq!(
                written & PARAM_SECTION_BIT,
                0,
                "{raw} から {encoded} が作られました"
            );
            assert_eq!(written, flags, "{raw} のフラグが変わりました");
        }
    }

    #[test]
    fn a_bit_a_named_flag_already_owns_is_refused_in_the_unnamed_flags() {
        // 名前を持つ位置を名前の無い側へ綴ると、同じ整数を 2 通りに表せる。
        // 符号化してから復号すると必ず名前のある側へ寄るため、この値は復号の
        // 出力にならない——型の定義域から外す。
        let movements = movements();
        for bit in [FLAG_ACCELERATE, FLAG_DECELERATE, FLAG_TWOPOINT] {
            let mut value = moving(&[0.0, 100.0], "直線移動");
            value.reserved_flags = bit;
            assert_eq!(
                validate_track_value(&value, target(1, &movements)),
                Err(TrackValueError::FlagsNotRepresentable.into()),
                "{bit}"
            );
            assert_eq!(
                encode_track_value(&value, target(1, &movements)),
                Err(TrackValueError::FlagsNotRepresentable.into()),
                "{bit}"
            );
        }
        // 名前の無い位置だけを立てた値は通る。拒否が広がると、読み取りが返した
        // 値をそのまま書き戻せなくなる。
        let mut carried = moving(&[0.0, 100.0], "直線移動");
        carried.reserved_flags = 16;
        assert_eq!(
            encode_track_value(&carried, target(1, &movements)),
            Ok("0,100,直線移動,16".to_string())
        );
    }

    #[test]
    fn spelling_the_fourth_bit_into_the_unnamed_flags_is_refused() {
        let movements = movements();
        let mut value = moving(&[0.0, 100.0], "直線移動");
        value.reserved_flags = PARAM_SECTION_BIT;
        // 検証と符号化の両方が拒否する。符号化だけが拒めば、検証を先に呼ぶ
        // 経路が書き込みを発行してしまう。
        assert_eq!(
            validate_track_value(&value, target(1, &movements)),
            Err(TrackValueError::FlagsNotRepresentable.into())
        );
        assert_eq!(
            encode_track_value(&value, target(1, &movements)),
            Err(TrackValueError::FlagsNotRepresentable.into())
        );
        // 拒否の名前と階級が、要求元が分岐に使う材料である。
        let error: ItemWriteError = TrackValueError::FlagsNotRepresentable.into();
        assert_eq!(error.reason(), Some("track_flags_not_representable"));
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);

        // 名前を持つビットや他の名前の無いビットと重ねても変わらない。
        let mut mixed = moving(&[0.0, 100.0], "直線移動");
        mixed.accelerate = true;
        mixed.reserved_flags = PARAM_SECTION_BIT | 16;
        assert_eq!(
            encode_track_value(&mixed, target(1, &movements)),
            Err(TrackValueError::FlagsNotRepresentable.into())
        );

        // 移動を持たない値では、フラグを持つこと自体が拒否の理由である。
        let mut still = static_value(0.0);
        still.reserved_flags = PARAM_SECTION_BIT;
        assert_eq!(
            encode_track_value(&still, target(1, &movements)),
            Err(TrackValueError::MovementWithoutMode.into())
        );
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
                "track_mode_not_writable",
                "track_mode_reads_as_number",
                "track_movement_without_mode",
                "track_flags_not_representable",
            ]
        );
    }

    #[test]
    fn track_failures_do_not_repeat_the_value() {
        // 移動方法の名前も値も応答へ反響させない。
        let secret = "秘密の移動";
        let movements = movements();
        let error = validate_track_value(&moving(&[0.0, 1.0], secret), target(1, &movements))
            .expect_err("拒否されます");
        assert!(!error.to_string().contains(secret), "{error}");
        // `known` が運ぶのは判定に使った一覧であり、拒否した要求の名前では
        // ない。一覧に無いからこそ拒否されているのだから、含まれるはずがない。
        let ItemWriteError::Track(TrackValueError::UnknownMode { known }) = error else {
            panic!("UnknownMode ではありません: {error:?}");
        };
        assert!(
            !known.iter().any(|movement| movement.name == secret),
            "{known:?}"
        );
        assert_eq!(known, movements);
    }

    #[test]
    fn a_mode_the_list_marks_as_unwritable_is_refused_under_its_own_name() {
        // 一覧に在って書けない名前は、一覧に無い名前とは別の失敗になる。
        // 同じ理由に畳むと、要求元は名前を選び直すという通らない手を打つ。
        let movements = vec![writable("直線移動"), unwritable("移動無し")];
        assert_eq!(
            validate_track_value(&moving(&[0.0, 1.0], "移動無し"), target(1, &movements)),
            Err(TrackValueError::ModeNotWritable.into())
        );
        assert_eq!(
            encode_track_value(&moving(&[0.0, 1.0], "移動無し"), target(1, &movements)),
            Err(TrackValueError::ModeNotWritable.into())
        );

        let error: ItemWriteError = TrackValueError::ModeNotWritable.into();
        assert_eq!(error.reason(), Some("track_mode_not_writable"));
        assert_eq!(error.error_code(), ErrorCode::InvalidArgument);
        assert_ne!(
            error.reason(),
            ItemWriteError::from(TrackValueError::UnknownMode { known: movements }).reason()
        );
    }

    #[test]
    fn a_mode_the_list_marks_as_unwritable_stays_in_the_list_it_carries() {
        // 一覧から外すと、実在する移動方法が「無い」として拒否される。名前は
        // 正しく、使い方が違うだけである。
        let movements = vec![writable("直線移動"), unwritable("移動無し")];
        let error = validate_track_value(
            &moving(&[0.0, 1.0], "存在しない移動"),
            target(1, &movements),
        )
        .expect_err("拒否されます");
        let ItemWriteError::Track(TrackValueError::UnknownMode { known }) = error else {
            panic!("UnknownMode ではありません: {error:?}");
        };
        assert_eq!(known, movements);
    }
}
