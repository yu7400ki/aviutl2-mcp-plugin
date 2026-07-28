//! descriptor と handshake が共有するワイヤ表現の生成・解釈。
//!
//! 時刻と HWND は plugin が書き、server が読む。両端が同一の関数を通ることで、
//! 書式の齟齬による同一性検証の取りこぼしを防ぐ。

use chrono::{DateTime, ParseError, Utc};

/// 100 ナノ秒を 1 とする小数部の桁数。
const SUBSEC_DIGITS: u32 = 7;

/// 秒未満を 100 ナノ秒単位へ落とす除数。
const NANOS_PER_HUNDRED_NANOS: u32 = 100;

/// 秒未満の上限（閏秒表現を飽和させるための値）。
const MAX_SUBSEC_NANOS: u32 = 999_999_999;

/// UTC 時刻を descriptor / handshake の正準書式へ整形する。
///
/// 書式は `YYYY-MM-DDTHH:MM:SS.fffffffZ`（小数部 7 桁固定、`Z` サフィックス）。
/// 小数部は 100 ナノ秒単位で、丸めずに切り捨てる。Windows のプロセス作成時刻
/// (`FILETIME`) とシステム時刻はいずれも 100 ナノ秒粒度であり、この桁数で情報を失わない。
///
/// ```
/// # use aviutl2_mcp_core::format_utc_timestamp;
/// # use chrono::{DateTime, Utc};
/// let dt = DateTime::from_timestamp(1_767_225_296, 123_456_789).unwrap();
/// assert_eq!(format_utc_timestamp(dt), "2025-12-31T23:54:56.1234567Z");
/// ```
pub fn format_utc_timestamp(value: DateTime<Utc>) -> String {
    // 閏秒は秒未満が 1 秒を超える表現を取り得るため、7 桁に収まるよう飽和させる。
    let subsec = value.timestamp_subsec_nanos().min(MAX_SUBSEC_NANOS);
    let hundred_nanos = subsec / NANOS_PER_HUNDRED_NANOS;
    format!(
        "{}.{:0width$}Z",
        value.format("%Y-%m-%dT%H:%M:%S"),
        hundred_nanos,
        width = SUBSEC_DIGITS as usize
    )
}

/// 正準書式の時刻文字列を UTC の時刻へ戻す。
///
/// [`format_utc_timestamp`] の出力は往復で完全に一致する。オフセット付きなど
/// 正準書式以外の RFC3339 表現も受理し、UTC へ正規化する。
pub fn parse_utc_timestamp(value: &str) -> Result<DateTime<Utc>, ParseError> {
    DateTime::parse_from_rfc3339(value).map(|dt| dt.with_timezone(&Utc))
}

/// ウィンドウハンドルを descriptor の正準書式へ整形する。
///
/// 書式は `0x` + 16 桁ゼロ埋め十六進（大文字）。桁数は 64bit 幅で固定し、
/// ハンドル幅が狭い環境では上位が 0 で埋まるだけで値は失われない。
///
/// ```
/// # use aviutl2_mcp_core::format_hwnd;
/// assert_eq!(format_hwnd(0x12345), "0x0000000000012345");
/// ```
pub fn format_hwnd(handle: u64) -> String {
    format!("0x{handle:016X}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64, nanos: u32) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, nanos).unwrap()
    }

    #[test]
    fn timestamp_has_fixed_seven_digit_fraction() {
        assert_eq!(
            format_utc_timestamp(at(0, 0)),
            "1970-01-01T00:00:00.0000000Z"
        );
        assert_eq!(
            format_utc_timestamp(at(0, 1_000)),
            "1970-01-01T00:00:00.0000010Z"
        );
        assert_eq!(
            format_utc_timestamp(at(0, 999_999_900)),
            "1970-01-01T00:00:00.9999999Z"
        );
    }

    #[test]
    fn timestamp_truncates_sub_hundred_nanos() {
        // 199ns は 1 (=100ns) へ切り捨てる。四捨五入なら 2 になる。
        assert_eq!(
            format_utc_timestamp(at(0, 199)),
            "1970-01-01T00:00:00.0000001Z"
        );
        // 切り上げが起きると秒が繰り上がってしまう境界。
        assert_eq!(
            format_utc_timestamp(at(0, 999_999_999)),
            "1970-01-01T00:00:00.9999999Z"
        );
    }

    #[test]
    fn timestamp_roundtrips_exactly() {
        let value = at(1_767_225_296, 123_456_700);
        let text = format_utc_timestamp(value);
        assert_eq!(parse_utc_timestamp(&text).unwrap(), value);
    }

    #[test]
    fn parse_accepts_offset_form_and_normalizes() {
        assert_eq!(
            parse_utc_timestamp("2026-01-01T09:00:00.0000000+09:00").unwrap(),
            parse_utc_timestamp("2026-01-01T00:00:00.0000000Z").unwrap()
        );
    }

    #[test]
    fn parse_rejects_non_rfc3339() {
        assert!(parse_utc_timestamp("2026-01-01 00:00:00").is_err());
        assert!(parse_utc_timestamp("").is_err());
    }

    #[test]
    fn hwnd_is_zero_padded_to_sixteen_digits() {
        assert_eq!(format_hwnd(0), "0x0000000000000000");
        assert_eq!(format_hwnd(0x12345), "0x0000000000012345");
        assert_eq!(format_hwnd(u64::MAX), "0xFFFFFFFFFFFFFFFF");
    }
}
