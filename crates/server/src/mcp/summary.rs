//! tool result の text content を上限内で組み立てる。
//!
//! 完全な機械可読値は `structuredContent` が運ぶため、text content は主要結果と
//! 次の操作に必要な値だけを短く示す。

/// 1 応答の text content に許す最大文字数。
pub const MAX_TEXT_CHARS: usize = 25_000;

/// 1 行に許す最大文字数。
///
/// 行の内容にはシーン名やレイヤー名など長さを制御できない値が入るため、
/// 行単位で切り詰めて 1 行が予算を食い尽くさないようにする。
const MAX_LINE_CHARS: usize = 200;

/// 打ち切りを示す末尾表記。
pub const TRUNCATION_NOTICE: &str = "…（text content の上限に達したため以降を省略しました。全件は structuredContent を参照してください）";

/// 切り詰めた行の末尾に付ける表記。
const ELLIPSIS: char = '…';

/// 上限を超えない text content を組み立てる。
///
/// 行を追加できるのは、末尾表記を足しても [`MAX_TEXT_CHARS`] を超えない間だけである。
/// 予算を超えた時点で以降の行は捨て、[`TextBuilder::finish`] が打ち切りを明示する。
pub struct TextBuilder {
    buffer: String,
    chars: usize,
    truncated: bool,
}

impl TextBuilder {
    /// 空の builder を作る。
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            chars: 0,
            truncated: false,
        }
    }

    /// 行を 1 つ追加する。予算を超える場合は追加せず打ち切りとして記録する。
    pub fn push_line(&mut self, line: impl AsRef<str>) {
        if self.truncated {
            return;
        }

        let line = clamp_line(line.as_ref());
        let separator = usize::from(!self.buffer.is_empty());
        let added = separator + line.chars().count();
        if self.chars + added > budget() {
            self.truncated = true;
            return;
        }

        if separator == 1 {
            self.buffer.push('\n');
        }
        self.buffer.push_str(&line);
        self.chars += added;
    }

    /// 組み立てた text content を返す。
    pub fn finish(mut self) -> String {
        if self.truncated {
            if !self.buffer.is_empty() {
                self.buffer.push('\n');
            }
            self.buffer.push_str(TRUNCATION_NOTICE);
        }
        self.buffer
    }
}

impl Default for TextBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// 行を追加してよい文字数の上限。
///
/// 打ち切り表記とその直前の改行を必ず書けるだけ残す。
fn budget() -> usize {
    MAX_TEXT_CHARS - TRUNCATION_NOTICE.chars().count() - 1
}

/// 1 行を [`MAX_LINE_CHARS`] 以内へ切り詰める。
fn clamp_line(line: &str) -> String {
    let sanitized: String = line
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    if sanitized.chars().count() <= MAX_LINE_CHARS {
        return sanitized;
    }
    let mut clamped: String = sanitized.chars().take(MAX_LINE_CHARS - 1).collect();
    clamped.push(ELLIPSIS);
    clamped
}

/// 任意の文字列を指定文字数以内へ切り詰める。
pub fn clamp_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut clamped: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    clamped.push(ELLIPSIS);
    clamped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_builder_produces_empty_text() {
        assert_eq!(TextBuilder::new().finish(), "");
    }

    #[test]
    fn lines_are_joined_with_newline() {
        let mut builder = TextBuilder::new();
        builder.push_line("1 行目");
        builder.push_line("2 行目");
        assert_eq!(builder.finish(), "1 行目\n2 行目");
    }

    #[test]
    fn long_line_is_clamped() {
        let mut builder = TextBuilder::new();
        builder.push_line("あ".repeat(MAX_LINE_CHARS * 3));
        let text = builder.finish();
        assert_eq!(text.chars().count(), MAX_LINE_CHARS);
        assert!(text.ends_with(ELLIPSIS));
    }

    #[test]
    fn newlines_inside_a_line_do_not_create_extra_lines() {
        let mut builder = TextBuilder::new();
        builder.push_line("前\n後");
        assert_eq!(builder.finish(), "前 後");
    }

    #[test]
    fn text_never_exceeds_limit() {
        let mut builder = TextBuilder::new();
        for i in 0..10_000 {
            builder.push_line(format!("{i}: {}", "名".repeat(MAX_LINE_CHARS * 2)));
        }
        let text = builder.finish();
        assert!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "上限を超えています: {}",
            text.chars().count()
        );
        assert!(
            text.ends_with(TRUNCATION_NOTICE),
            "打ち切りが示されていません"
        );
    }

    #[test]
    fn text_within_limit_has_no_truncation_notice() {
        let mut builder = TextBuilder::new();
        for i in 0..10 {
            builder.push_line(format!("行 {i}"));
        }
        let text = builder.finish();
        assert!(!text.contains(TRUNCATION_NOTICE));
    }

    #[test]
    fn lines_after_truncation_are_dropped() {
        let mut builder = TextBuilder::new();
        for _ in 0..1_000 {
            builder.push_line("あ".repeat(MAX_LINE_CHARS));
        }
        builder.push_line("この行は入らない");
        let text = builder.finish();
        assert!(!text.contains("この行は入らない"));
        assert!(text.chars().count() <= MAX_TEXT_CHARS);
    }

    #[test]
    fn clamp_chars_keeps_short_value() {
        assert_eq!(clamp_chars("短い", 10), "短い");
    }

    #[test]
    fn clamp_chars_shortens_long_value() {
        let clamped = clamp_chars(&"長".repeat(20), 5);
        assert_eq!(clamped.chars().count(), 5);
        assert!(clamped.ends_with(ELLIPSIS));
    }
}
