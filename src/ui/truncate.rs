//! Width-aware truncation that never splits a grapheme and never panics.

use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr as _;

/// Ellipsis used when clipping (single-width safe).
pub const ELLIPSIS: char = '…';

/// `true` when every character of `text` renders as exactly one cell.
#[must_use]
pub fn is_plain_ascii(text: &str) -> bool {
    text.is_ascii()
}

/// Clip `text` to at most `width` cells, appending an ellipsis when clipped.
#[must_use]
pub fn clip(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.width() <= width {
        return text.to_string();
    }
    if width == 1 {
        return ELLIPSIS.to_string();
    }
    let budget = width - 1; // room for the ellipsis
    let mut out = String::with_capacity(text.len());
    let mut used = 0usize;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push(ELLIPSIS);
    out
}

/// Clip to exactly `width` cells (pad with spaces on the right).
#[must_use]
pub fn clip_pad(text: &str, width: usize) -> String {
    let clipped = clip(text, width);
    let missing = width.saturating_sub(clipped.width());
    let mut out = clipped;
    for _ in 0..missing {
        out.push(' ');
    }
    out
}

/// Right-align and clip to exactly `width` cells.
#[must_use]
pub fn pad_left_clip(text: &str, width: usize) -> String {
    if text.width() <= width {
        let missing = width - text.width();
        let mut out = String::new();
        for _ in 0..missing {
            out.push(' ');
        }
        out.push_str(text);
        return out;
    }
    // Keep the tail: the interesting part of "…/artist/album/title" is the end.
    let mut chars: Vec<char> = Vec::new();
    let mut used = 0usize;
    for ch in text.chars().rev() {
        let w = ch.width().unwrap_or(0);
        if used + w > width.saturating_sub(1) {
            break;
        }
        used += w;
        chars.push(ch);
    }
    let mut out = String::from(ELLIPSIS);
    out.extend(chars.into_iter().rev());
    let missing = width.saturating_sub(out.width());
    let mut final_out = String::new();
    for _ in 0..missing {
        final_out.push(' ');
    }
    final_out.push_str(&out);
    final_out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_with_ellipsis() {
        assert_eq!(clip("abcdef", 6), "abcdef");
        assert_eq!(clip("abcdef", 5), "abcd…");
        assert_eq!(clip("abcdef", 1), "…");
        assert_eq!(clip("", 3), "");
        assert_eq!(clip("abc", 0), "");
    }

    #[test]
    fn handles_wide_and_combining_characters() {
        // CJK glyphs are two cells wide.
        assert_eq!(clip("日本語です", 5), "日本…");
        assert_eq!(clip("ja\u{301}po\u{301}nai\u{301}", 3).width(), 3);
        assert_eq!(clip("\u{1F600}abc", 4).width(), 4);
    }

    #[test]
    fn pads_to_exact_width() {
        assert_eq!(clip_pad("ab", 5), "ab   ");
        assert_eq!(clip_pad("abcdef", 4), "abc…");
        assert_eq!(pad_left_clip("12:34", 7), "  12:34");
        assert_eq!(pad_left_clip("12:34", 3).width(), 3);
    }
}
