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
const LAYOUT_CONTROLS: [char; 3] = ['\n', '\r', '\t'];

/// 文字列の検証失敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TextSyntaxError {
    /// NUL を含む。
    #[error("NUL を含む文字列は指定できません")]
    ContainsNul,
    /// 制御文字を含む。
    #[error("制御文字を含む文字列は指定できません")]
    ContainsControl,
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
    /// 失敗の種別を表す機械可読な名前を返す。
    ///
    /// 検証対象の文字列そのものは応答へ載せられないため、理由の識別には
    /// この名前を用いる。
    pub fn reason(&self) -> &'static str {
        match self {
            TextSyntaxError::ContainsNul => "contains_nul",
            TextSyntaxError::ContainsControl => "contains_control",
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
}

impl PathSyntaxError {
    /// 失敗の種別を表す機械可読な名前を返す。
    ///
    /// パスそのものは応答へ載せないため、理由の識別にはこの名前を用いる。
    pub fn reason(&self) -> &'static str {
        match self {
            PathSyntaxError::Empty => "empty_path",
            PathSyntaxError::ContainsNul => "contains_nul",
            PathSyntaxError::TooLong { .. } => "path_too_long",
            PathSyntaxError::DeviceNamespace => "device_namespace",
            PathSyntaxError::AlternateDataStream => "alternate_data_stream",
            PathSyntaxError::NotAbsolute => "not_absolute",
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
    limit_bytes(value, MAX_ITEM_VALUE_BYTES)
}

/// 複数行を取り得る設定項目の文字列値を検証する。
///
/// 改行とタブを許すほかは [`validate_item_text`] と同じである。
///
/// 読み取りが返した値をそのまま書き戻せることを保つための緩和である。
/// 複数行のテキストを持つ設定項目では、読み取りが改行を含む値を返し得る。
/// これを拒否すると、読める値が書き戻せないという非対称が生じ、複数行の
/// テキストを設定する経路が丸ごと失われる。
///
/// 値の中で改行が実際の改行として現れるか、エスケープされた表記として
/// 現れるかは経路によって異なり得る。エスケープ表記なら本緩和は何も
/// 変えず、実際の改行なら本緩和が無ければ書き戻せない。**どちらであっても
/// 許す側が安全である。**
pub fn validate_multiline_item_text(value: &str) -> Result<(), TextSyntaxError> {
    validate_control_free_except_layout(value)?;
    limit_bytes(value, MAX_ITEM_VALUE_BYTES)
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
///
/// 6 は、相対パスの基準ディレクトリが要求元と実行側で異なるためである。
/// UNC パス（`\\server\share\...`）は絶対パスとして受け付ける。
///
/// **正規化後の値へ再度適用すること。** `..` の解決や短縮名の展開は本関数の
/// 範囲外であり、正規化前だけを検証すると `..` で制限を回避できる。
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
    if !is_absolute(&path) {
        return Err(PathSyntaxError::NotAbsolute);
    }
    Ok(())
}

/// device namespace を指すか。
///
/// `\\.\` / `\\?\` で始まるパスはファイルシステム以外の名前空間へ到達でき、
/// 以降の構文規則も適用されないため受け付けない。`\\server\share` の形は
/// これに当たらない。
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

/// 絶対パスか。
///
/// ドライブレター起点（`X:\...`）と UNC（`\\server\share...`）のみを認める。
/// 先頭が区切りだけのパス（`\dir`）はカレントドライブ基準であり、基準が
/// 要求元と実行側で一致しないため認めない。
fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\' {
        return true;
    }
    let Some(rest) = path.strip_prefix(r"\\") else {
        return false;
    };
    let mut parts = rest.split('\\');
    let server = parts.next().unwrap_or_default();
    let share = parts.next().unwrap_or_default();
    !server.is_empty() && !share.is_empty()
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
    fn multiline_item_text_keeps_the_byte_limit() {
        let value = "\n".repeat(MAX_ITEM_VALUE_BYTES + 1);
        assert_eq!(
            validate_multiline_item_text(&value),
            Err(TextSyntaxError::TooLongBytes {
                bytes: MAX_ITEM_VALUE_BYTES + 1,
                max: MAX_ITEM_VALUE_BYTES,
            })
        );
    }

    #[test]
    fn name_rejects_nul_only() {
        assert_eq!(validate_name("立ち絵\0"), Err(TextSyntaxError::ContainsNul));
        assert_eq!(validate_name("立ち絵"), Ok(()));
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
    fn path_accepts_unc() {
        // UNC は device namespace ではなく、構文としては受け付ける。
        for path in [
            r"\\server\share",
            r"\\server\share\dir\movie.mp4",
            "//server/share/movie.mp4",
        ] {
            assert_eq!(validate_path(path), Ok(()), "{path} が拒否されました");
        }
    }

    #[test]
    fn path_rejects_incomplete_unc() {
        for path in [r"\\", r"\\server", r"\\server\"] {
            assert_eq!(
                validate_path(path),
                Err(PathSyntaxError::NotAbsolute),
                "{path} が受理されました"
            );
        }
    }

    #[test]
    fn path_rejects_empty_and_nul() {
        assert_eq!(validate_path(""), Err(PathSyntaxError::Empty));
        assert_eq!(
            validate_path("C:\\movie\0.mp4"),
            Err(PathSyntaxError::ContainsNul)
        );
    }

    #[test]
    fn path_rejects_over_the_length_limit() {
        let path = format!(r"C:\{}", "a".repeat(MAX_PATH_UTF16_UNITS));
        assert_eq!(
            validate_path(&path),
            Err(PathSyntaxError::TooLong {
                units: MAX_PATH_UTF16_UNITS + 3
            })
        );

        let path = format!(r"C:\{}", "a".repeat(MAX_PATH_UTF16_UNITS - 3));
        assert_eq!(validate_path(&path), Ok(()));
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
    fn path_rejects_device_namespace() {
        for path in [
            r"\\.\PhysicalDrive0",
            r"\\?\C:\movie.mp4",
            r"\\?\UNC\server\share",
            "//./pipe/name",
            r"\\.",
            r"\\?",
        ] {
            assert_eq!(
                validate_path(path),
                Err(PathSyntaxError::DeviceNamespace),
                "{path} が受理されました"
            );
        }
    }

    #[test]
    fn path_rejects_alternate_data_stream() {
        for path in [
            r"C:\movie.mp4:stream",
            r"C:\dir\file:$DATA",
            r"\\server\share\file:stream",
        ] {
            assert_eq!(
                validate_path(path),
                Err(PathSyntaxError::AlternateDataStream),
                "{path} が受理されました"
            );
        }
    }

    #[test]
    fn path_rejects_relative_forms() {
        for path in [
            "movie.mp4",
            r"dir\movie.mp4",
            r"..\movie.mp4",
            r"\movie.mp4",
            "C:movie.mp4",
            "C:",
        ] {
            assert_eq!(
                validate_path(path),
                Err(PathSyntaxError::NotAbsolute),
                "{path} が受理されました"
            );
        }
    }

    #[test]
    fn path_does_not_resolve_dot_segments() {
        // 正規化は OS 層の担当であり、構文としては通る。呼び出し側は
        // 正規化後の値へ再度適用する。
        assert_eq!(validate_path(r"C:\dir\..\movie.mp4"), Ok(()));
    }

    #[test]
    fn errors_do_not_repeat_the_input() {
        for error in [
            PathSyntaxError::Empty,
            PathSyntaxError::ContainsNul,
            PathSyntaxError::TooLong { units: 40_000 },
            PathSyntaxError::DeviceNamespace,
            PathSyntaxError::AlternateDataStream,
            PathSyntaxError::NotAbsolute,
        ] {
            assert!(!error.reason().is_empty());
            assert!(!error.to_string().contains("C:\\"));
        }
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
