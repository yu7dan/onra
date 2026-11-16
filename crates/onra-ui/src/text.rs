//! Measuring and fitting styled text. ANSI escapes take no columns; CJK
//! characters take two.

use unicode_width::UnicodeWidthChar;

/// Columns `s` occupies on screen, ignoring ANSI escapes.
pub fn width(s: &str) -> usize {
    let mut w = 0;
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            skip_escape(&mut chars);
        } else {
            w += ch.width().unwrap_or(0);
        }
    }
    w
}

/// Cut `s` to at most `max` columns, keeping escapes intact so colors don't
/// bleed. A cut line ends with a reset.
pub fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            out.push(ch);
            while let Some(&c) = chars.peek() {
                out.push(c);
                chars.next();
                if c.is_ascii_alphabetic() && c != '[' {
                    break;
                }
            }
            continue;
        }
        let cw = ch.width().unwrap_or(0);
        if w + cw > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push_str("\x1b[0m");
    out
}

/// Pad `s` with spaces to `cols` columns.
pub fn pad(s: &str, cols: usize) -> String {
    format!("{s}{}", " ".repeat(cols.saturating_sub(width(s))))
}

fn skip_escape(chars: &mut std::str::Chars<'_>) {
    for c in chars.by_ref() {
        if c.is_ascii_alphabetic() && c != '[' {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_escapes_and_counts_wide_chars() {
        assert_eq!(width("\x1b[38;2;1;2;3mabc\x1b[0m"), 3);
        assert_eq!(width("查行情"), 6);
    }

    #[test]
    fn truncates_by_columns() {
        assert_eq!(truncate("abcdef", 3), "abc\x1b[0m");
        assert_eq!(truncate("\x1b[1mabcdef", 2), "\x1b[1mab\x1b[0m");
        assert_eq!(truncate("查行情", 3), "查\x1b[0m");
        assert_eq!(truncate("ab", 5), "ab");
    }
}
