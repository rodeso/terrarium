//! Emoji detection for display.
//!
//! Text is split into grapheme clusters (what a reader sees as one character),
//! so multi-codepoint emoji such as 👨‍👩‍👧 (ZWJ family), 👍🏽 (skin tone),
//! 🇵🇹 (flag) and 1️⃣ (keycap) are kept whole and can be wrapped in a single
//! `<span class="emoji">`. That lets CSS give them a color emoji font,
//! consistent sizing, and no inherited text effects.

use unicode_segmentation::UnicodeSegmentation;

pub enum Segment<'a> {
    Text(&'a str),
    Emoji(&'a str),
}

/// Splits `s` into runs of plain text and individual emoji.
pub fn segments(s: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut text_start: Option<usize> = None;

    for (i, g) in s.grapheme_indices(true) {
        if is_emoji(g) {
            if let Some(start) = text_start.take() {
                out.push(Segment::Text(&s[start..i]));
            }
            out.push(Segment::Emoji(g));
        } else if text_start.is_none() {
            text_start = Some(i);
        }
    }
    if let Some(start) = text_start {
        out.push(Segment::Text(&s[start..]));
    }
    out
}

/// True if `s` is nothing but 1..=`max` emoji (whitespace allowed).
/// Used to show short emoji-only messages large.
pub fn is_emoji_only(s: &str, max: usize) -> bool {
    let mut count = 0;
    for g in s.graphemes(true) {
        if g.chars().all(char::is_whitespace) {
            continue;
        }
        if !is_emoji(g) {
            return false;
        }
        count += 1;
        if count > max {
            return false;
        }
    }
    count > 0
}

/// Cheap pre-check: could `s` contain any emoji at all?
pub fn may_contain_emoji(s: &str) -> bool {
    s.chars().any(|c| is_pictographic(c) || is_regional_indicator(c) || c == '\u{20E3}')
}

/// HTML-escapes `s`, wrapping each emoji in `<span class="emoji">`.
pub fn to_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 32);
    for seg in segments(s) {
        match seg {
            Segment::Text(t) => escape_into(&mut out, t),
            Segment::Emoji(e) => {
                out.push_str("<span class=\"emoji\">");
                escape_into(&mut out, e);
                out.push_str("</span>");
            }
        }
    }
    out
}

fn escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

/// Decides whether one grapheme cluster should display as an emoji.
pub fn is_emoji(g: &str) -> bool {
    let Some(first) = g.chars().next() else { return false };

    // Keycaps: 1️⃣ #️⃣ *️⃣
    if g.contains('\u{20E3}') {
        return matches!(first, '0'..='9' | '#' | '*');
    }
    // Flags are pairs of regional indicators; a lone one is just a letter.
    if is_regional_indicator(first) {
        return g.chars().count() >= 2;
    }
    if !is_pictographic(first) {
        return false;
    }
    // Characters like © ™ ↔ ☺ are text unless explicitly asked to be emoji
    // (U+FE0F) or combined into a larger emoji sequence.
    g.chars().any(|c| {
        c == '\u{FE0F}'                          // emoji presentation selector
            || c == '\u{200D}'                   // zero-width joiner sequence
            || ('\u{1F3FB}'..='\u{1F3FF}').contains(&c) // skin tone
            || ('\u{E0020}'..='\u{E007F}').contains(&c) // tag sequence (e.g. 🏴󠁧󠁢󠁳󠁣󠁴󠁿)
    }) || default_emoji_presentation(first)
}

fn is_regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// Approximates Unicode's Extended_Pictographic property.
fn is_pictographic(c: char) -> bool {
    matches!(c as u32,
        0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139
        | 0x2194..=0x2199 | 0x21A9..=0x21AA | 0x231A..=0x231B | 0x2328 | 0x23CF
        | 0x23E9..=0x23F3 | 0x23F8..=0x23FA | 0x24C2 | 0x25AA..=0x25AB | 0x25B6
        | 0x25C0 | 0x25FB..=0x25FE | 0x2600..=0x27BF | 0x2934..=0x2935
        | 0x2B05..=0x2B07 | 0x2B1B..=0x2B1C | 0x2B50 | 0x2B55 | 0x3030 | 0x303D
        | 0x3297 | 0x3299 | 0x1F000..=0x1FAFF)
}

/// Characters that display as emoji even without U+FE0F
/// (Unicode's Emoji_Presentation property).
fn default_emoji_presentation(c: char) -> bool {
    matches!(c as u32,
        0x231A..=0x231B | 0x23E9..=0x23EC | 0x23F0 | 0x23F3 | 0x25FD..=0x25FE
        | 0x2614..=0x2615 | 0x2648..=0x2653 | 0x267F | 0x2693 | 0x26A1
        | 0x26AA..=0x26AB | 0x26BD..=0x26BE | 0x26C4..=0x26C5 | 0x26CE | 0x26D4
        | 0x26EA | 0x26F2..=0x26F3 | 0x26F5 | 0x26FA | 0x26FD | 0x2705
        | 0x270A..=0x270B | 0x2728 | 0x274C | 0x274E | 0x2753..=0x2755 | 0x2757
        | 0x2795..=0x2797 | 0x27B0 | 0x27BF | 0x2B1B..=0x2B1C | 0x2B50 | 0x2B55
        | 0x1F004 | 0x1F0CF | 0x1F18E | 0x1F191..=0x1F19A | 0x1F201 | 0x1F21A
        | 0x1F22F | 0x1F232..=0x1F236 | 0x1F238..=0x1F23A | 0x1F250..=0x1F251
        | 0x1F300..=0x1FAFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emoji_list(s: &str) -> Vec<&str> {
        segments(s)
            .into_iter()
            .filter_map(|seg| match seg {
                Segment::Emoji(e) => Some(e),
                Segment::Text(_) => None,
            })
            .collect()
    }

    #[test]
    fn keeps_complex_emoji_whole() {
        let s = "a 😊 b 👍🏽 🇵🇹 👨‍👩‍👧 1️⃣ ❤️ 🏴󠁧󠁢󠁳󠁣󠁴󠁿";
        assert_eq!(emoji_list(s), ["😊", "👍🏽", "🇵🇹", "👨‍👩‍👧", "1️⃣", "❤️", "🏴󠁧󠁢󠁳󠁣󠁴󠁿"]);
    }

    #[test]
    fn leaves_text_symbols_and_scripts_alone() {
        assert!(emoji_list("© 2026 ™ ↔ 1 # café ñ 日本 منாக؟ ☺").is_empty());
    }

    #[test]
    fn emoji_only_detection() {
        assert!(is_emoji_only("😊", 3));
        assert!(is_emoji_only(" 👍🏽 🇵🇹 ", 3));
        assert!(!is_emoji_only("😊😊😊😊", 3));
        assert!(!is_emoji_only("ok 😊", 3));
        assert!(!is_emoji_only("", 3));
    }

    #[test]
    fn html_is_escaped_and_wrapped() {
        assert_eq!(to_html("<b>😊</b>"), "&lt;b&gt;<span class=\"emoji\">😊</span>&lt;/b&gt;");
    }
}
