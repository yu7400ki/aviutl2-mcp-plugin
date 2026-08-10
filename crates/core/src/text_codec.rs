//! テキスト設定値の符号化と復号。
//!
//! ホストはテキスト種別の設定値をエスケープ表記で受け取り、同じ表記で返す。
//! エスケープの集合は `\` と改行の 2 つだけで、タブは両方向で素通しする。
//!
//! **符号化と復号は対にして 1 か所へ置く。** 片側だけを持つと、読み取りが
//! 返した値をそのまま書き戻したときに包みが増減する。書き込みだけが包めば
//! 読み取りはエスケープ表記を返し続け、読み取りだけが解けば書き込んだ
//! `\` がホストのエスケープとして解釈される。

/// クライアントの文字列を、ホストへ渡す表記へ符号化する。
///
/// 変換は 2 つだけである。
///
/// - `\` を `\\` へ包む
/// - LF を `\n`（`\` と `n` の 2 文字）へ包む
///
/// **タブは包まない。** ホストはタブをエスケープとして解釈せず素通しするため、
/// 包むと `\` が余分に残る。
///
/// **CR は入力に現れない前提である。** 呼び出し元が CRLF を LF へ正規化し、
/// 単独の CR を拒否した後の文字列を渡す。
pub fn encode_host_text(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => encoded.push_str(r"\\"),
            '\n' => encoded.push_str(r"\n"),
            _ => encoded.push(c),
        }
    }
    encoded
}

/// ホストが返した表記を、クライアントへ返す文字列へ復号する。
///
/// [`encode_host_text`] の逆変換である。`\\` を `\` へ、`\n` を LF へ戻す。
///
/// **`\` の次が `\` でも `n` でもない場合は、`\` ごとそのまま残す。** 末尾の
/// `\` も同じく残す。ホストは返す表記で `\` を必ず包むためこの並びは現れない
/// が、現れたときに `\` を黙って落とすと元の文字列を復元できなくなる。残す形
/// なら、ホストが与えた文字はどの並びでも失われない。
pub fn decode_host_text(raw: &str) -> String {
    let mut decoded = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            decoded.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => decoded.push('\\'),
            Some('n') => decoded.push('\n'),
            Some(other) => {
                decoded.push('\\');
                decoded.push(other);
            }
            None => decoded.push('\\'),
        }
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ホストが書き込まれた表記を解いて保存する規則を模す。
    ///
    /// `\\` を `\` へ、`\n` を LF へ戻し、それ以外の `\` は次の文字ごとそのまま
    /// 保つ。`C:\temp\note` を書くと `C:` + `\` + `temp` + LF + `ote` が保存され、
    /// `\t` が素通りすることから導かれる規則である。**codec を呼ばずに書く。**
    /// 呼ぶと、codec が規則からずれたときに検査も一緒にずれる。
    fn host_store(written: &str) -> String {
        let chars: Vec<char> = written.chars().collect();
        let mut stored = String::new();
        let mut index = 0;
        while index < chars.len() {
            match (chars[index], chars.get(index + 1)) {
                ('\\', Some('\\')) => {
                    stored.push('\\');
                    index += 2;
                }
                ('\\', Some('n')) => {
                    stored.push('\n');
                    index += 2;
                }
                (c, _) => {
                    stored.push(c);
                    index += 1;
                }
            }
        }
        stored
    }

    /// ホストが保存した値を読み取りへ返す規則を模す。理由は [`host_store`] と同じ。
    fn host_report(stored: &str) -> String {
        let mut reported = String::new();
        for c in stored.chars() {
            match c {
                '\\' => reported.push_str(r"\\"),
                '\n' => reported.push_str(r"\n"),
                _ => reported.push(c),
            }
        }
        reported
    }

    #[test]
    fn the_encoding_wraps_only_backslashes_and_line_feeds() {
        assert_eq!(encode_host_text(r"C:\temp"), r"C:\\temp");
        assert_eq!(encode_host_text("a\nb"), r"a\nb");
        assert_eq!(encode_host_text("a\tb"), "a\tb");
        assert_eq!(encode_host_text("字幕"), "字幕");
    }

    #[test]
    fn an_unknown_escape_keeps_its_backslash() {
        // 落とすと元の文字列を復元できない。
        assert_eq!(decode_host_text(r"\t"), r"\t");
        assert_eq!(decode_host_text(r"C:\temp"), r"C:\temp");
        assert_eq!(decode_host_text(r"末尾\"), r"末尾\");
        assert_eq!(decode_host_text(r"\r\n"), "\\r\n");
    }

    #[test]
    fn a_windows_path_survives_the_hosts_interpretation() {
        // 符号化 → ホストの解釈 → 復号 で元へ戻る。ホストが保存する値そのものが
        // 与えた文字列と一致するため、描画も要求どおりになる。
        for value in [r"C:\temp\note", r"^\d+\.txt$", r"\begin{align}", "a\nb\tc"] {
            let stored = host_store(&encode_host_text(value));
            assert_eq!(stored, value, "{value:?} が保存の時点で崩れました");
            assert_eq!(
                decode_host_text(&host_report(&stored)),
                value,
                "{value:?} が読み直しで崩れました"
            );
        }
    }
}
