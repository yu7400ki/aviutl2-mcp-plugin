//! 編集入力の構文と大きさの検証。
//!
//! ここに置くのは **OS に依存しない構文の判定だけ**である。パスの正規化・
//! ドライブ種別・到達性の確認は OS を触れる層が行う。同じ規則を要求元と
//! 実行側の双方が呼べるよう、全て純関数として公開する。

/// object alias の最大バイト数。
pub const MAX_ALIAS_BYTES: usize = 1024 * 1024;

/// パスの最大長。単位は UTF-16 code unit。
pub const MAX_PATH_UTF16_UNITS: usize = 32_767;

/// 設定項目の文字列値の最大バイト数。
///
/// 単一の設定項目が応答サイズを圧迫しないための上限であり、alias 全体の
/// 上限とは別に課す。
pub const MAX_ITEM_VALUE_BYTES: usize = 8 * 1024;

/// 名前（effect 名・設定項目名・オブジェクト名）の最大長。
/// 単位は UTF-16 code unit。
pub const MAX_NAME_UTF16_UNITS: usize = 1024;

/// 行の折り返しと字下げを表す制御文字。
///
/// 複数行を取る値にはこれらが現れる。
///
/// **CR を残すのは CRLF を通すためだけである。** CRLF の CR まで「制御文字を
/// 含む」として落とすと、行区切りが CRLF の環境で書いたテキストが理由の読めない
/// 失敗になる。単独の CR は別の規則が専用の理由で落とす。
const LAYOUT_CONTROLS: [char; 3] = ['\n', '\r', '\t'];

/// オブジェクトエイリアス名として使えない文字。
///
/// AviUtl2 の UI が登録時に拒否する集合であり、本リポジトリが決めた制約では
/// ない。Windows のファイル名禁止文字と、table 書式の区切り（`.` は節の
/// 入れ子、`=` はキーと値、`,` は値の並び）の和になっている。
const FORBIDDEN_ALIAS_NAME_CHARS: [char; 14] = [
    '\\', '/', ':', '*', '?', '"', '\'', '<', '>', '|', '%', '=', ',', '.',
];

/// 文字列の検証失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TextSyntaxError {
    /// 空文字列である。
    ///
    /// 空を「値を消す」意味へ黙って読み替える場所があるため、明示的な取り消しを
    /// 別に持つ指定では空を受け取らない。
    #[error("空文字列は指定できません")]
    Empty,
    /// NUL を含む。
    #[error("NUL を含む文字列は指定できません")]
    ContainsNul,
    /// 制御文字を含む。
    #[error("制御文字を含む文字列は指定できません")]
    ContainsControl,
    /// その用途で使えない文字を含む。
    #[error("使用できない文字を含む文字列は指定できません")]
    ForbiddenCharacter,
    /// 後ろに LF が続かない CR を含む。
    #[error("単独の復帰 (CR) を含む文字列は指定できません")]
    LoneCarriageReturn,
    /// UTF-16 code unit 数の上限を超えた。
    #[error("文字列が長すぎます: {units} UTF-16 code units (上限 {max})")]
    TooLongUtf16 {
        /// 実際の UTF-16 code unit 数。
        units: usize,
        /// 許容する UTF-16 code unit 数。
        max: usize,
    },
    /// バイト数の上限を超えた。
    #[error("文字列が長すぎます: {bytes} バイト (上限 {max})")]
    TooLongBytes {
        /// 実際のバイト数。
        bytes: usize,
        /// 許容するバイト数。
        max: usize,
    },
}

impl TextSyntaxError {
    /// 全 variant の代表値。
    ///
    /// [`TextSyntaxError::reason`] が返し得る名前を数え上げるために用いる。
    /// 値を持つ variant には代表となる値を添えてあり、名前はその値に依存しない。
    pub const ALL: &'static [TextSyntaxError] = &[
        TextSyntaxError::Empty,
        TextSyntaxError::ContainsNul,
        TextSyntaxError::ContainsControl,
        TextSyntaxError::ForbiddenCharacter,
        TextSyntaxError::LoneCarriageReturn,
        TextSyntaxError::TooLongUtf16 {
            units: MAX_NAME_UTF16_UNITS + 1,
            max: MAX_NAME_UTF16_UNITS,
        },
        TextSyntaxError::TooLongBytes {
            bytes: MAX_ITEM_VALUE_BYTES + 1,
            max: MAX_ITEM_VALUE_BYTES,
        },
    ];

    /// 失敗の種別を表す機械可読な名前を返す。
    ///
    /// 検証対象の文字列そのものを含まない。
    pub fn reason(&self) -> &'static str {
        match self {
            TextSyntaxError::Empty => "empty",
            TextSyntaxError::ContainsNul => "contains_nul",
            TextSyntaxError::ContainsControl => "contains_control",
            TextSyntaxError::ForbiddenCharacter => "forbidden_character",
            TextSyntaxError::LoneCarriageReturn => "lone_carriage_return",
            TextSyntaxError::TooLongUtf16 { .. } | TextSyntaxError::TooLongBytes { .. } => {
                "too_long"
            }
        }
    }
}

/// パス構文の検証失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PathSyntaxError {
    /// 空文字列。
    #[error("パスが空です")]
    Empty,
    /// NUL を含む。
    #[error("NUL を含むパスは指定できません")]
    ContainsNul,
    /// 長さの上限を超えた。
    #[error("パスが長すぎます: {units} UTF-16 code units (上限 {MAX_PATH_UTF16_UNITS})")]
    TooLong {
        /// 実際の UTF-16 code unit 数。
        units: usize,
    },
    /// device namespace のパス。
    #[error("device namespace のパスは指定できません")]
    DeviceNamespace,
    /// 代替データストリームを指すパス。
    #[error("代替データストリームを含むパスは指定できません")]
    AlternateDataStream,
    /// 絶対パスでない。
    #[error("絶対パスである必要があります")]
    NotAbsolute,
    /// UNC（ネットワーク上の場所）を指すパス。
    #[error("ネットワーク上のパスは指定できません")]
    UncPath,
}

impl PathSyntaxError {
    /// 全 variant の代表値。
    ///
    /// [`PathSyntaxError::reason`] が返し得る名前を数え上げるために用いる。
    /// 値を持つ variant には代表となる値を添えてあり、名前はその値に依存しない。
    pub const ALL: &'static [PathSyntaxError] = &[
        PathSyntaxError::Empty,
        PathSyntaxError::ContainsNul,
        PathSyntaxError::TooLong {
            units: MAX_PATH_UTF16_UNITS + 1,
        },
        PathSyntaxError::DeviceNamespace,
        PathSyntaxError::AlternateDataStream,
        PathSyntaxError::NotAbsolute,
        PathSyntaxError::UncPath,
    ];

    /// 失敗の種別を表す機械可読な名前を返す。
    ///
    /// 名前は失敗の種別ごとに異なり、パスそのものを含まない。
    pub fn reason(&self) -> &'static str {
        match self {
            PathSyntaxError::Empty => "empty_path",
            PathSyntaxError::ContainsNul => "contains_nul",
            PathSyntaxError::TooLong { .. } => "path_too_long",
            PathSyntaxError::DeviceNamespace => "device_namespace",
            PathSyntaxError::AlternateDataStream => "alternate_data_stream",
            PathSyntaxError::NotAbsolute => "not_absolute",
            PathSyntaxError::UncPath => "unc_path",
        }
    }
}

/// NUL と制御文字を含まないことを確認する。
///
/// これらは NUL 終端の文字列として渡せない、あるいは行指向の alias 表記を
/// 壊すため、値の内容として受け付けない。
pub fn validate_control_free(text: &str) -> Result<(), TextSyntaxError> {
    if text.contains('\0') {
        return Err(TextSyntaxError::ContainsNul);
    }
    if text.chars().any(char::is_control) {
        return Err(TextSyntaxError::ContainsControl);
    }
    Ok(())
}

/// 改行とタブだけを例外として、NUL と制御文字を含まないことを確認する。
///
/// 複数行を取る値のための緩和であり、[`validate_control_free`] より弱い。
/// 単一行しか取らない値には用いない。
pub fn validate_control_free_except_layout(text: &str) -> Result<(), TextSyntaxError> {
    if text.contains('\0') {
        return Err(TextSyntaxError::ContainsNul);
    }
    if text
        .chars()
        .any(|c| c.is_control() && !LAYOUT_CONTROLS.contains(&c))
    {
        return Err(TextSyntaxError::ContainsControl);
    }
    Ok(())
}

/// 複数行を取り得ない設定項目の文字列値を検証する。
///
/// NUL と制御文字を拒否し、[`MAX_ITEM_VALUE_BYTES`] を上限とする。
pub fn validate_item_text(value: &str) -> Result<(), TextSyntaxError> {
    validate_control_free(value)?;
    limit_item_value_bytes(value)
}

/// 複数行を取り得る設定項目の文字列値の構文を検証する。
///
/// 改行とタブを許すほかは [`validate_item_text`] と同じ制御文字の規則を課し、
/// 加えて単独の CR（後ろに LF が続かない CR）を拒否する。
///
/// 改行を許すのは、複数行のテキストを書く直接の手段を残すためである。改行を
/// 拒否すると、複数行の値は要求元がエスケープ表記を自分で組み立てるしか書け
/// なくなる。
///
/// 単独の CR を拒否するのは、ホストが改行として保存しながら描画では行を分け
/// ないためである。CRLF は LF へ正規化して受けられるが、単独の CR にはどちらの
/// 意図とも読める余地があり、黙って読み替えると描画の行数が変わる。
///
/// **バイト数の上限はここでは課さない。** 上限が守るのはホストへ実際に渡る
/// 文字列であり、その長さは符号化するまで決まらない。上限は
/// [`limit_item_value_bytes`] が符号化後の文字列へ課す。
pub fn validate_multiline_item_text(value: &str) -> Result<(), TextSyntaxError> {
    validate_control_free_except_layout(value)?;
    reject_lone_carriage_return(value)
}

/// 設定項目の文字列値としてのバイト数の上限を課す。
///
/// 単一の設定項目が応答サイズを圧迫しないための上限であり、単位はバイトである。
pub fn limit_item_value_bytes(value: &str) -> Result<(), TextSyntaxError> {
    limit_bytes(value, MAX_ITEM_VALUE_BYTES)
}

/// 後ろに LF が続かない CR を含まないことを確認する。
fn reject_lone_carriage_return(value: &str) -> Result<(), TextSyntaxError> {
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() != Some(&'\n') {
            return Err(TextSyntaxError::LoneCarriageReturn);
        }
    }
    Ok(())
}

/// 名前（effect 名・設定項目名・オブジェクト名）を検証する。
///
/// NUL を拒否し、[`MAX_NAME_UTF16_UNITS`] を上限とする。上限の単位が
/// バイトではなく UTF-16 code unit であるのは、ホストが名前を UTF-16 で
/// 扱うためである。
pub fn validate_name(name: &str) -> Result<(), TextSyntaxError> {
    if name.contains('\0') {
        return Err(TextSyntaxError::ContainsNul);
    }
    limit_utf16(name, MAX_NAME_UTF16_UNITS)
}

/// オブジェクトエイリアス名を検証する。
///
/// 判定は次の順で行う。いずれも OS へ問い合わせずに決まる規則である。
///
/// 1. 空文字列を拒否する
/// 2. NUL と制御文字を拒否する
/// 3. `\ / : * ? " ' < > | % = , .` のいずれかを含む名前を拒否する
/// 4. [`MAX_NAME_UTF16_UNITS`] を上限とする
///
/// **この規則は AviUtl2 の UI が登録時に課すものであり、本リポジトリが決めた
/// 制約ではない。** 緩めれば UI から登録できない名前を作れてしまうため、
/// 調整の対象にはしない。禁止する集合は Windows のファイル名禁止文字と、
/// table 書式の区切り（`.` は節の入れ子、`=` はキーと値、`,` は値の並び）の
/// 和である。
///
/// **名前をファイル名の一部にする前に呼ぶ。** `\` `/` `:` を拒めば区切りと
/// ドライブ指定が入らず、`.` を拒めば `..` が綴れないため、ディレクトリの外を
/// 指す名前はここで残らず落ちる。連結してから判定する形にすると、判定対象が
/// 呼び出し元の与えた文字列ではなくなる。
pub fn validate_object_alias_name(name: &str) -> Result<(), TextSyntaxError> {
    if name.is_empty() {
        return Err(TextSyntaxError::Empty);
    }
    validate_control_free(name)?;
    if name.contains(FORBIDDEN_ALIAS_NAME_CHARS) {
        return Err(TextSyntaxError::ForbiddenCharacter);
    }
    limit_utf16(name, MAX_NAME_UTF16_UNITS)
}

/// object alias を検証する。
///
/// NUL を拒否し、[`MAX_ALIAS_BYTES`] を上限とする。alias は行区切りを含む
/// 書式であるため、制御文字は拒否しない。
pub fn validate_alias(alias: &str) -> Result<(), TextSyntaxError> {
    if alias.contains('\0') {
        return Err(TextSyntaxError::ContainsNul);
    }
    limit_bytes(alias, MAX_ALIAS_BYTES)
}

/// パスの構文を検証する。
///
/// 判定は次の順で行う。いずれも OS へ問い合わせずに決まる規則である。
///
/// 1. 空文字列を拒否する
/// 2. NUL を含む文字列を拒否する
/// 3. [`MAX_PATH_UTF16_UNITS`] を上限とする
/// 4. device namespace（`\\.\` / `\\?\`）を拒否する
/// 5. 代替データストリーム（ドライブレター以外の位置に現れる `:`）を拒否する
/// 6. 絶対パスであることを要求する
/// 7. UNC（`\\server\share\...`）を拒否する
///
/// 6 は、相対パスの基準ディレクトリが要求元と実行側で異なるためである。
///
/// 7 は、渡されたパスがそのまま接続先になるためである。到達性を確かめずに
/// 任意のホストを指す UNC を渡すと、応答しないホストでは操作が戻らず、
/// 利用者の資格情報で認証が行われ、接続した事実が相手に残る。
///
/// **割り当て済みのネットワークドライブは受け付ける。** ドライブレターが何を
/// 指すかは利用者が事前に決めたものであり、パスを組み立てただけでは到達
/// できない。届く先が同じでも、宛先を選んだのが誰かが違う。
///
/// 6 と 7 は根の分類（[`path_root`]）で同時に決まるが、理由は畳まない。
/// 「絶対パスへ直せば通る」と「ネットワーク上の場所は受け付けない」は、
/// 呼び出し元にとって別の対処になる。
///
/// 判定は文字列に閉じ、`..` の解決も短縮名の展開も行わない。`..` では
/// ローカルパスがネットワーク上の場所へ化けないため、正規化した値へ
/// 再適用する必要はない。
///
/// **限界: ローカルに見えるパスの解決先は追わない。** シンボリックリンクや
/// ジャンクション、`subst` で割り当てたドライブレターを介してネットワーク上の
/// 場所へ届く形は、文字列だけでは見分けられないため素通りする。ここでの判定は
/// 誤った指定を防ぐためのものであり、意図的な差し替えは対象にしない。
pub fn validate_path(path: &str) -> Result<(), PathSyntaxError> {
    if path.is_empty() {
        return Err(PathSyntaxError::Empty);
    }
    if path.contains('\0') {
        return Err(PathSyntaxError::ContainsNul);
    }
    let units = path.encode_utf16().count();
    if units > MAX_PATH_UTF16_UNITS {
        return Err(PathSyntaxError::TooLong { units });
    }

    // 区切りは `\` と `/` のどちらでも同じ意味を持つため、判定前に揃える。
    let path: String = path
        .chars()
        .map(|c| if c == '/' { '\\' } else { c })
        .collect();

    if is_device_namespace(&path) {
        return Err(PathSyntaxError::DeviceNamespace);
    }
    if has_stream_separator(&path) {
        return Err(PathSyntaxError::AlternateDataStream);
    }
    match path_root(&path) {
        PathRoot::Drive => Ok(()),
        PathRoot::Network => Err(PathSyntaxError::UncPath),
        PathRoot::None => Err(PathSyntaxError::NotAbsolute),
    }
}

/// device namespace を指すか。
///
/// `\\.\` / `\\?\` で始まるパスはファイルシステム以外の名前空間へ到達でき、
/// 以降の構文規則も適用されないため受け付けない。`\\server\share` の形は
/// これに当たらず、起点の分類（[`path_root`]）が拒否する。
fn is_device_namespace(path: &str) -> bool {
    match path.strip_prefix(r"\\") {
        Some(rest) => {
            rest == "." || rest == "?" || rest.starts_with(r".\") || rest.starts_with(r"?\")
        }
        None => false,
    }
}

/// ドライブレター以外の位置に `:` を含むか。
///
/// `X:` の 1 個所だけを許し、それ以外の `:` は代替データストリームの指定と
/// みなす。ドライブレター直後に区切りが無い形（`X:name`）はドライブごとの
/// カレントディレクトリ基準であり、絶対パスの判定で落ちる。
fn has_stream_separator(path: &str) -> bool {
    path.char_indices()
        .any(|(index, c)| c == ':' && !(index == 1 && starts_with_drive_letter(path)))
}

/// 先頭がドライブレターか。
fn starts_with_drive_letter(path: &str) -> bool {
    path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
}

/// パスの起点。
///
/// 構文の判定に要るのは「どこを起点として解釈されるか」だけであり、この 3 つが
/// そのまま受理と拒否の理由になる。
enum PathRoot {
    /// ドライブレター起点（`X:\...`）。
    Drive,
    /// 区切り 2 つで始まるネットワーク上の場所（`\\server\share\...`）。
    Network,
    /// 起点を持たない。相対パスと、カレントドライブ基準のパス（`\dir`）。
    None,
}

/// パスの起点を判定する。
///
/// device namespace は本関数より前に弾く。残る `\\` 始まりはすべて
/// ネットワーク上の場所を指すため、`\\server` のように共有名を欠く形も
/// [`PathRoot::Network`] とする。共有名を足しても受け付けないので、
/// 「絶対パスにすれば通る」と読める理由を返さない。
///
/// 先頭が区切り 1 つだけのパス（`\dir`）はカレントドライブ基準であり、その
/// 基準が呼び出し元と実行側で一致しないため起点として認めない。
fn path_root(path: &str) -> PathRoot {
    if path.starts_with(r"\\") {
        return PathRoot::Network;
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\' {
        return PathRoot::Drive;
    }
    PathRoot::None
}

/// UTF-16 code unit 数の上限を課す。
fn limit_utf16(text: &str, max: usize) -> Result<(), TextSyntaxError> {
    let units = text.encode_utf16().count();
    if units > max {
        return Err(TextSyntaxError::TooLongUtf16 { units, max });
    }
    Ok(())
}

/// バイト数の上限を課す。
fn limit_bytes(text: &str, max: usize) -> Result<(), TextSyntaxError> {
    let bytes = text.len();
    if bytes > max {
        return Err(TextSyntaxError::TooLongBytes { bytes, max });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::REASON_VALUES;

    /// variant を表す名前を返す。
    ///
    /// 網羅 match で書く。variant を足すとここがコンパイルエラーになり、
    /// すぐ下の一覧と [`TextSyntaxError::ALL`] へ足す必要があることが分かる。
    fn text_variant_name(error: &TextSyntaxError) -> &'static str {
        match error {
            TextSyntaxError::Empty => "Empty",
            TextSyntaxError::ContainsNul => "ContainsNul",
            TextSyntaxError::ContainsControl => "ContainsControl",
            TextSyntaxError::ForbiddenCharacter => "ForbiddenCharacter",
            TextSyntaxError::LoneCarriageReturn => "LoneCarriageReturn",
            TextSyntaxError::TooLongUtf16 { .. } => "TooLongUtf16",
            TextSyntaxError::TooLongBytes { .. } => "TooLongBytes",
        }
    }

    /// variant を表す名前を返す。網羅 match で書く理由は同上。
    fn path_variant_name(error: &PathSyntaxError) -> &'static str {
        match error {
            PathSyntaxError::Empty => "Empty",
            PathSyntaxError::ContainsNul => "ContainsNul",
            PathSyntaxError::TooLong { .. } => "TooLong",
            PathSyntaxError::DeviceNamespace => "DeviceNamespace",
            PathSyntaxError::AlternateDataStream => "AlternateDataStream",
            PathSyntaxError::NotAbsolute => "NotAbsolute",
            PathSyntaxError::UncPath => "UncPath",
        }
    }

    #[test]
    fn text_syntax_all_covers_every_variant() {
        const VARIANTS: &[&str] = &[
            "Empty",
            "ContainsNul",
            "ContainsControl",
            "ForbiddenCharacter",
            "LoneCarriageReturn",
            "TooLongUtf16",
            "TooLongBytes",
        ];
        let covered: Vec<&str> = TextSyntaxError::ALL.iter().map(text_variant_name).collect();
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
    fn path_syntax_all_covers_every_variant() {
        const VARIANTS: &[&str] = &[
            "Empty",
            "ContainsNul",
            "TooLong",
            "DeviceNamespace",
            "AlternateDataStream",
            "NotAbsolute",
            "UncPath",
        ];
        let covered: Vec<&str> = PathSyntaxError::ALL.iter().map(path_variant_name).collect();
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
    fn syntax_reasons_belong_to_the_shared_value_set() {
        // 一覧に無い名前は、誰にも気付かれないままワイヤへ出る。
        for error in TextSyntaxError::ALL {
            let reason = error.reason();
            assert!(
                REASON_VALUES.contains(&reason),
                "{reason} が reason の値域にありません"
            );
        }
        for error in PathSyntaxError::ALL {
            let reason = error.reason();
            assert!(
                REASON_VALUES.contains(&reason),
                "{reason} が reason の値域にありません"
            );
        }
    }

    #[test]
    fn path_syntax_reasons_differ_by_variant() {
        // 種別ごとに別の名前を返す。畳むと要求元は訂正のしかたを選べない。
        let mut reasons: Vec<&str> = PathSyntaxError::ALL
            .iter()
            .map(PathSyntaxError::reason)
            .collect();
        let count = reasons.len();
        reasons.sort_unstable();
        reasons.dedup();
        assert_eq!(reasons.len(), count, "パス検証の名前が重複しています");
    }

    #[test]
    fn syntax_reasons_do_not_depend_on_the_inspected_value() {
        // 名前は種別だけを表す。長さや位置が名前に混ざると、応答へ載せた
        // 時点で検証対象の内容が漏れる。
        assert_eq!(
            PathSyntaxError::TooLong { units: 1 }.reason(),
            PathSyntaxError::TooLong { units: 999_999 }.reason()
        );
        assert_eq!(
            TextSyntaxError::TooLongUtf16 { units: 1, max: 0 }.reason(),
            TextSyntaxError::TooLongBytes {
                bytes: 999_999,
                max: 0
            }
            .reason()
        );
    }

    #[test]
    fn limits_have_the_documented_units() {
        // バイト単位の上限。
        assert_eq!(MAX_ALIAS_BYTES, 1_048_576);
        assert_eq!(MAX_ITEM_VALUE_BYTES, 8_192);
        // UTF-16 code unit 単位の上限。
        assert_eq!(MAX_PATH_UTF16_UNITS, 32_767);
        assert_eq!(MAX_NAME_UTF16_UNITS, 1_024);
    }

    #[test]
    fn item_text_limit_counts_bytes() {
        // 多バイト文字では文字数ではなくバイト数で上限に達する。
        let value = "あ".repeat(MAX_ITEM_VALUE_BYTES / 3);
        assert_eq!(value.chars().count(), MAX_ITEM_VALUE_BYTES / 3);
        assert_eq!(validate_item_text(&value), Ok(()));

        let value = "あ".repeat(MAX_ITEM_VALUE_BYTES / 3 + 1);
        assert_eq!(
            validate_item_text(&value),
            Err(TextSyntaxError::TooLongBytes {
                bytes: value.len(),
                max: MAX_ITEM_VALUE_BYTES,
            })
        );
    }

    #[test]
    fn name_limit_counts_utf16_code_units() {
        // BMP 外の文字は 1 文字で 2 code unit を占める。
        let name = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2);
        assert_eq!(name.chars().count(), MAX_NAME_UTF16_UNITS / 2);
        assert_eq!(validate_name(&name), Ok(()));

        let name = "🎬".repeat(MAX_NAME_UTF16_UNITS / 2 + 1);
        assert_eq!(
            validate_name(&name),
            Err(TextSyntaxError::TooLongUtf16 {
                units: MAX_NAME_UTF16_UNITS + 2,
                max: MAX_NAME_UTF16_UNITS,
            })
        );
    }

    #[test]
    fn name_at_the_byte_limit_is_accepted() {
        // 上限の単位はバイトではないため、UTF-8 で上限を超える名前も通る。
        let name = "あ".repeat(MAX_NAME_UTF16_UNITS);
        assert!(name.len() > MAX_NAME_UTF16_UNITS);
        assert_eq!(validate_name(&name), Ok(()));
    }

    #[test]
    fn alias_limit_counts_bytes() {
        let alias = "a".repeat(MAX_ALIAS_BYTES);
        assert_eq!(validate_alias(&alias), Ok(()));

        let alias = "a".repeat(MAX_ALIAS_BYTES + 1);
        assert_eq!(
            validate_alias(&alias),
            Err(TextSyntaxError::TooLongBytes {
                bytes: MAX_ALIAS_BYTES + 1,
                max: MAX_ALIAS_BYTES,
            })
        );
    }

    #[test]
    fn alias_keeps_line_breaks_but_rejects_nul() {
        assert_eq!(validate_alias("[vo]\n_name=立ち絵\n"), Ok(()));
        assert_eq!(validate_alias("[vo]\0"), Err(TextSyntaxError::ContainsNul));
    }

    #[test]
    fn item_text_rejects_nul_and_control_characters() {
        assert_eq!(
            validate_item_text("字幕\0"),
            Err(TextSyntaxError::ContainsNul)
        );
        for control in ['\n', '\r', '\t', '\u{7f}', '\u{1b}'] {
            assert_eq!(
                validate_item_text(&format!("字幕{control}")),
                Err(TextSyntaxError::ContainsControl),
                "{control:?} が受理されました"
            );
        }
        assert_eq!(validate_item_text("字幕 テキスト"), Ok(()));
    }

    #[test]
    fn multiline_item_text_allows_line_breaks_and_tabs() {
        assert_eq!(
            validate_multiline_item_text("1 行目\r\n2 行目\n\t字下げ"),
            Ok(())
        );
    }

    #[test]
    fn multiline_item_text_rejects_other_control_characters() {
        assert_eq!(
            validate_multiline_item_text("字幕\0"),
            Err(TextSyntaxError::ContainsNul)
        );
        for control in ['\u{1}', '\u{b}', '\u{c}', '\u{1b}', '\u{7f}', '\u{9b}'] {
            assert_eq!(
                validate_multiline_item_text(&format!("字幕{control}")),
                Err(TextSyntaxError::ContainsControl),
                "{control:?} が受理されました"
            );
        }
    }

    #[test]
    fn multiline_item_text_rejects_a_lone_carriage_return() {
        // CRLF は行区切りとして通し、単独の CR だけを落とす。
        assert_eq!(validate_multiline_item_text("1 行目\r\n2 行目"), Ok(()));
        for value in ["1 行目\r2 行目", "末尾\r", "\r", "\n\r"] {
            assert_eq!(
                validate_multiline_item_text(value),
                Err(TextSyntaxError::LoneCarriageReturn),
                "{value:?} が受理されました"
            );
        }
    }

    #[test]
    fn multiline_item_text_leaves_the_byte_limit_to_the_encoded_form() {
        // 上限が守るのはホストへ渡る文字列であり、その長さは符号化するまで
        // 決まらない。構文の検証は長さを見ない。
        let value = "\n".repeat(MAX_ITEM_VALUE_BYTES + 1);
        assert_eq!(validate_multiline_item_text(&value), Ok(()));
        assert_eq!(
            limit_item_value_bytes(&value),
            Err(TextSyntaxError::TooLongBytes {
                bytes: MAX_ITEM_VALUE_BYTES + 1,
                max: MAX_ITEM_VALUE_BYTES,
            })
        );
    }

    #[test]
    fn object_alias_name_rejects_every_forbidden_character() {
        // 集合は AviUtl2 の UI の観測そのものであり、1 文字ずつが単独で落ちる。
        for forbidden in FORBIDDEN_ALIAS_NAME_CHARS {
            let name = format!("立ち絵{forbidden}");
            assert_eq!(
                validate_object_alias_name(&name),
                Err(TextSyntaxError::ForbiddenCharacter),
                "{forbidden:?} が受理されました"
            );
        }
    }

    #[test]
    fn object_alias_name_rejects_path_syntax_before_any_join() {
        // 区切り・ドライブ指定・相対参照は、パスを組み立てる前に落ちる。
        for name in ["..", r"..\..\x", r"a\b", "a/b", r"C:\x", "."] {
            assert_eq!(
                validate_object_alias_name(name),
                Err(TextSyntaxError::ForbiddenCharacter),
                "{name:?} が受理されました"
            );
        }
    }

    #[test]
    fn object_alias_name_rejects_control_characters() {
        assert_eq!(
            validate_object_alias_name("立ち絵\0"),
            Err(TextSyntaxError::ContainsNul)
        );
        for control in ['\n', '\r', '\t', '\u{1b}', '\u{7f}'] {
            assert_eq!(
                validate_object_alias_name(&format!("立ち絵{control}")),
                Err(TextSyntaxError::ContainsControl),
                "{control:?} が受理されました"
            );
        }
    }

    #[test]
    fn object_alias_name_reports_each_rule_by_its_own_reason() {
        assert_eq!(
            validate_object_alias_name("").unwrap_err().reason(),
            "empty"
        );
        assert_eq!(
            validate_object_alias_name("立ち絵\0").unwrap_err().reason(),
            "contains_nul"
        );
        assert_eq!(
            validate_object_alias_name("..").unwrap_err().reason(),
            "forbidden_character"
        );
        let name = "a".repeat(MAX_NAME_UTF16_UNITS + 1);
        assert_eq!(
            validate_object_alias_name(&name),
            Err(TextSyntaxError::TooLongUtf16 {
                units: MAX_NAME_UTF16_UNITS + 1,
                max: MAX_NAME_UTF16_UNITS,
            })
        );
        assert_eq!(
            validate_object_alias_name(&name).unwrap_err().reason(),
            "too_long"
        );
    }

    #[test]
    fn object_alias_name_accepts_usable_names() {
        for name in [
            "立ち絵",
            "alias-01",
            "立ち絵 (通常)",
            "＃タグ付き",
            &"a".repeat(MAX_NAME_UTF16_UNITS),
        ] {
            assert_eq!(
                validate_object_alias_name(name),
                Ok(()),
                "{name:?} が拒否されました"
            );
        }
    }

    #[test]
    fn object_alias_name_accepts_the_yen_sign() {
        // U+00A5 は表示上 `\`(U+005C) に見えるだけの別の符号位置であり、パスの
        // 区切りでも table 書式の区切りでもない。落とす理由が実在しない文字を
        // 落とすと、AviUtl2 が登録を許す名前を使えなくすることになる。
        assert!(!FORBIDDEN_ALIAS_NAME_CHARS.contains(&'\u{a5}'));
        assert_eq!(validate_object_alias_name("\u{a5}"), Ok(()));
        assert_eq!(validate_object_alias_name("立ち絵\u{a5}2"), Ok(()));
    }

    #[test]
    fn name_rejects_nul_only() {
        assert_eq!(validate_name("立ち絵\0"), Err(TextSyntaxError::ContainsNul));
        assert_eq!(validate_name("立ち絵"), Ok(()));
    }

    /// 規則 1 つ分の入力。
    struct PathRuleInputs {
        /// 規則を指す名前。失敗時の目印にする。
        rule: &'static str,
        /// この規則に当たって拒否される入力。
        rejected: Vec<String>,
        /// この規則に当たらず受理される入力。
        accepted: Vec<String>,
    }

    /// 理由ごとの入力を返す。
    ///
    /// `PathSyntaxError` に対する網羅 `match` であり `_` を使わない。**理由を
    /// 足すとここが落ち、拒否される入力と受理される入力を書くまでコンパイル
    /// できない。** 規則ごとの入力の置き場所はここだけである。
    fn path_rule_inputs(reason: &PathSyntaxError) -> PathRuleInputs {
        let absolute = || vec![r"C:\movie.mp4".to_string()];
        match reason {
            PathSyntaxError::Empty => PathRuleInputs {
                rule: "空文字列",
                rejected: vec![String::new()],
                accepted: absolute(),
            },
            PathSyntaxError::ContainsNul => PathRuleInputs {
                rule: "NUL",
                rejected: vec!["C:\\movie\0.mp4".to_string(), "\0".to_string()],
                accepted: absolute(),
            },
            PathSyntaxError::TooLong { units } => PathRuleInputs {
                rule: "長さの上限",
                // 理由が名乗る code unit 数を入力から導き、両者がずれないようにする。
                rejected: vec![format!(r"C:\{}", "a".repeat(units - 3))],
                accepted: vec![format!(r"C:\{}", "a".repeat(MAX_PATH_UTF16_UNITS - 3))],
            },
            PathSyntaxError::DeviceNamespace => PathRuleInputs {
                rule: "device namespace",
                rejected: vec![
                    r"\\.\PhysicalDrive0".to_string(),
                    r"\\?\C:\movie.mp4".to_string(),
                    r"\\?\UNC\server\share".to_string(),
                    "//./pipe/name".to_string(),
                    r"\\.".to_string(),
                    r"\\?".to_string(),
                ],
                accepted: vec![r"C:\pipe\name".to_string()],
            },
            PathSyntaxError::AlternateDataStream => PathRuleInputs {
                rule: "代替データストリーム",
                rejected: vec![
                    r"C:\movie.mp4:stream".to_string(),
                    r"C:\dir\file:$DATA".to_string(),
                    r"\\server\share\file:stream".to_string(),
                ],
                // ドライブレターの `:` だけは許す。
                accepted: vec![r"C:\movie.mp4".to_string(), r"Z:\".to_string()],
            },
            PathSyntaxError::NotAbsolute => PathRuleInputs {
                rule: "絶対パス",
                rejected: vec![
                    "movie.mp4".to_string(),
                    r"dir\movie.mp4".to_string(),
                    r"..\movie.mp4".to_string(),
                    r"\movie.mp4".to_string(),
                    "C:movie.mp4".to_string(),
                    "C:".to_string(),
                ],
                accepted: vec![r"C:\dir\movie.mp4".to_string()],
            },
            PathSyntaxError::UncPath => PathRuleInputs {
                rule: "UNC",
                rejected: vec![
                    r"\\server\share".to_string(),
                    r"\\server\share\dir\movie.mp4".to_string(),
                    "//server/share/movie.mp4".to_string(),
                    r"//server\share\movie.mp4".to_string(),
                    // 共有名を欠く形も同じ理由で拒否する。
                    r"\\".to_string(),
                    r"\\server".to_string(),
                    r"\\server\".to_string(),
                ],
                // 割り当て済みのドライブレターは指す先を利用者が選んでいる。
                accepted: vec![r"Z:\share\movie.mp4".to_string()],
            },
        }
    }

    /// 検証する理由の一覧。
    ///
    /// **この一覧の網羅は型では強制できない。** 理由を足したときに落ちるのは
    /// [`path_rule_inputs`] であり、その行を書くときに本一覧へも足す。
    fn path_rule_reasons() -> Vec<PathSyntaxError> {
        vec![
            PathSyntaxError::Empty,
            PathSyntaxError::ContainsNul,
            PathSyntaxError::TooLong {
                units: MAX_PATH_UTF16_UNITS + 3,
            },
            PathSyntaxError::DeviceNamespace,
            PathSyntaxError::AlternateDataStream,
            PathSyntaxError::NotAbsolute,
            PathSyntaxError::UncPath,
        ]
    }

    #[test]
    fn every_path_rule_rejects_and_accepts_its_inputs() {
        for reason in path_rule_reasons() {
            let inputs = path_rule_inputs(&reason);
            for path in &inputs.rejected {
                assert_eq!(
                    validate_path(path),
                    Err(reason),
                    "{} が {path:?} を拒否しませんでした",
                    inputs.rule
                );
            }
            for path in &inputs.accepted {
                assert_eq!(
                    validate_path(path),
                    Ok(()),
                    "{} の対になる {path:?} が拒否されました",
                    inputs.rule
                );
            }
        }
    }

    #[test]
    fn path_accepts_absolute_forms() {
        for path in [
            r"C:\movie.mp4",
            r"c:\dir\movie.mp4",
            "C:/dir/movie.mp4",
            r"Z:\",
            r"C:\日本語\動画.mp4",
        ] {
            assert_eq!(validate_path(path), Ok(()), "{path} が拒否されました");
        }
    }

    #[test]
    fn path_distinguishes_a_network_path_from_a_relative_path() {
        // 対処が別であるため、理由も分ける。
        assert_eq!(
            validate_path(r"\\server\share\movie.mp4"),
            Err(PathSyntaxError::UncPath)
        );
        assert_eq!(
            validate_path(r"\server\share\movie.mp4"),
            Err(PathSyntaxError::NotAbsolute)
        );
        assert_eq!(
            PathSyntaxError::UncPath.reason(),
            "unc_path",
            "理由が機械可読な名前で区別できません"
        );
        assert_ne!(
            PathSyntaxError::UncPath.reason(),
            PathSyntaxError::NotAbsolute.reason()
        );
    }

    #[test]
    fn path_length_limit_counts_utf16_code_units() {
        // BMP 外の文字は 1 文字で 2 code unit を占めるため、文字数が上限の
        // 半分でも超過する。
        let path = format!(r"C:\{}", "🎬".repeat(MAX_PATH_UTF16_UNITS / 2));
        assert_eq!(
            validate_path(&path),
            Err(PathSyntaxError::TooLong {
                units: MAX_PATH_UTF16_UNITS / 2 * 2 + 3
            })
        );
    }

    #[test]
    fn path_does_not_resolve_dot_segments() {
        // `..` を解決しても起点は変わらないため、構文としては通す。
        assert_eq!(validate_path(r"C:\dir\..\movie.mp4"), Ok(()));
        // ローカルパスは `..` を重ねてもネットワーク上の場所へは化けない。
        assert_eq!(validate_path(r"C:\..\..\..\movie.mp4"), Ok(()));
    }

    #[test]
    fn errors_do_not_repeat_the_input() {
        let mut reasons = Vec::new();
        for error in path_rule_reasons() {
            assert!(!error.reason().is_empty());
            assert!(!error.to_string().contains("C:\\"));
            reasons.push(error.reason());
        }
        // 理由の名前は失敗の種別ごとに異なる。
        reasons.sort_unstable();
        let count = reasons.len();
        reasons.dedup();
        assert_eq!(reasons.len(), count, "同じ名前を名乗る理由があります");
        assert_eq!(TextSyntaxError::ContainsNul.reason(), "contains_nul");
        assert_eq!(
            TextSyntaxError::ContainsControl.reason(),
            "contains_control"
        );
        assert_eq!(
            TextSyntaxError::TooLongBytes { bytes: 1, max: 0 }.reason(),
            "too_long"
        );
        assert_eq!(
            TextSyntaxError::TooLongUtf16 { units: 1, max: 0 }.reason(),
            "too_long"
        );
    }
}
