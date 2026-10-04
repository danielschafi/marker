//! Autopair, step-over, and tab-out for math in text boxes (design R7 / R8).
//!
//! `apply` is a pure pre-filter in front of `TextEdit`: when it returns `Some`,
//! the caller writes the new buffer and caret and consumes the event so egui's
//! undoer sees one replacement per key.

use egui::text::CCursor;
use egui::text_selection::CCursorRange;
use egui::{Event, Key, Modifiers};

use crate::math_spans::{self, MathSpanRef};

/// Apply one input event as an autopair / tab-out edit.
///
/// Returns `None` when the event should fall through to `TextEdit`.
pub fn apply(text: &str, sel: CCursorRange, event: &Event) -> Option<(String, CCursorRange)> {
    match event {
        Event::Text(payload) => {
            let mut chars = payload.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            apply_char(text, sel, ch)
        }
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => match *key {
            Key::Backspace if plain_modifiers(modifiers) => apply_backspace(text, sel),
            Key::Tab if tab_modifiers(modifiers) => apply_tab(text, sel, modifiers.shift),
            _ => None,
        },
        _ => None,
    }
}

fn plain_modifiers(modifiers: &Modifiers) -> bool {
    modifiers.is_none()
}

fn tab_modifiers(modifiers: &Modifiers) -> bool {
    modifiers.is_none()
        || (modifiers.shift
            && !modifiers.alt
            && !modifiers.ctrl
            && !modifiers.mac_cmd
            && !modifiers.command)
}

fn apply_char(text: &str, sel: CCursorRange, ch: char) -> Option<(String, CCursorRange)> {
    let range = sel.as_sorted_char_range();
    if range.start != range.end {
        return (ch == '$').then(|| wrap_selection(text, range.start, range.end));
    }
    let caret = range.start;
    let byte = char_to_byte(text, caret);
    match ch {
        '$' => apply_dollar(text, caret, byte),
        '{' => apply_open_brace(text, caret, byte),
        '(' => apply_open_paren(text, caret, byte),
        '}' | ')' => step_over_char(text, caret, ch),
        _ => None,
    }
}

fn apply_dollar(text: &str, caret: usize, byte: usize) -> Option<(String, CCursorRange)> {
    if preceded_by_unescaped_backslash(text, byte) {
        return None;
    }
    if is_empty_inline_dollar_pair(text, caret) {
        let start = char_to_byte(text, caret - 1);
        let end = char_to_byte(text, caret + 1);
        return Some(splice(text, start, end, "$$$$", 2));
    }
    if next_char(text, caret) == Some('$') && math_spans::caret_inside_math(text, byte).is_some()
    {
        return Some((text.to_string(), cursor(caret + 1)));
    }
    if math_spans::caret_inside_math(text, byte).is_none() {
        return Some(splice(text, byte, byte, "$$", 1));
    }
    None
}

fn apply_open_brace(text: &str, caret: usize, byte: usize) -> Option<(String, CCursorRange)> {
    if math_spans::caret_inside_math(text, byte).is_none() {
        return None;
    }
    Some(splice(text, byte, byte, "{}", 1))
}

fn apply_open_paren(text: &str, caret: usize, byte: usize) -> Option<(String, CCursorRange)> {
    if math_spans::caret_inside_math(text, byte).is_none() {
        if ends_with_unescaped_backslash(text, byte) {
            return Some(splice(text, byte, byte, "(\\)", 1));
        }
        return None;
    }
    if text[..byte].ends_with("\\left") {
        return Some(splice(text, byte, byte, "(\\right)", 1));
    }
    None
}

fn step_over_char(text: &str, caret: usize, ch: char) -> Option<(String, CCursorRange)> {
    (next_char(text, caret) == Some(ch)).then(|| (text.to_string(), cursor(caret + 1)))
}

fn apply_backspace(text: &str, sel: CCursorRange) -> Option<(String, CCursorRange)> {
    if !sel.is_empty() {
        return None;
    }
    let caret = sel.primary.index;
    if caret == 0 {
        return None;
    }
    let byte = char_to_byte(text, caret);

    if caret >= 2
        && nth_char(text, caret.wrapping_sub(2)) == Some('$')
        && nth_char(text, caret - 1) == Some('$')
        && nth_char(text, caret) == Some('$')
        && nth_char(text, caret + 1) == Some('$')
    {
        let start = char_to_byte(text, caret - 2);
        let end = char_to_byte(text, caret + 2);
        return Some(splice(text, start, end, "", 0));
    }

    if is_empty_inline_dollar_pair(text, caret) {
        let start = char_to_byte(text, caret - 1);
        let end = char_to_byte(text, caret + 1);
        return Some(splice(text, start, end, "", 0));
    }

    if text[..byte].ends_with("\\left(") && text[byte..].starts_with("\\right)") {
        let start = byte - "\\left(".len();
        let end = byte + "\\right)".len();
        return Some(splice(text, start, end, "", 0));
    }

    if text[..byte].ends_with("\\(") && text[byte..].starts_with("\\)") {
        let start = byte - 2;
        let end = byte + 2;
        return Some(splice(text, start, end, "", 0));
    }

    if nth_char(text, caret - 1) == Some('{') && nth_char(text, caret) == Some('}') {
        let start = char_to_byte(text, caret - 1);
        let end = char_to_byte(text, caret + 1);
        return Some(splice(text, start, end, "", 0));
    }

    None
}

fn apply_tab(text: &str, sel: CCursorRange, shift: bool) -> Option<(String, CCursorRange)> {
    if !sel.is_empty() {
        return None;
    }
    let caret = sel.primary.index;
    let byte = char_to_byte(text, caret);
    let span = math_spans::caret_inside_math(text, byte)?;
    let placeholders = empty_placeholders(text, &span);

    if shift {
        let prev = placeholders.iter().copied().rev().find(|&p| p < caret);
        return Some(match prev {
            Some(p) => (text.to_string(), cursor(p)),
            None => (text.to_string(), sel),
        });
    }

    if let Some(p) = placeholders.iter().copied().find(|&p| p > caret) {
        return Some((text.to_string(), cursor(p)));
    }

    let (next, after) = math_spans::exit_math_span(text, &span);
    let index = byte_to_char(&next, after);
    Some((next, cursor(index)))
}

fn wrap_selection(text: &str, start: usize, end: usize) -> (String, CCursorRange) {
    let a = char_to_byte(text, start);
    let b = char_to_byte(text, end);
    let mut out = String::with_capacity(text.len() + 2);
    out.push_str(&text[..a]);
    out.push('$');
    out.push_str(&text[a..b]);
    out.push('$');
    out.push_str(&text[b..]);
    (out, cursor(end + 2))
}

fn empty_placeholders(text: &str, span: &MathSpanRef) -> Vec<usize> {
    let inner = &text[span.inner_start..span.inner_end];
    let bytes = inner.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'}' {
            out.push(byte_to_char(text, span.inner_start + i + 1));
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

fn is_empty_inline_dollar_pair(text: &str, caret: usize) -> bool {
    nth_char(text, caret.wrapping_sub(1)) == Some('$')
        && nth_char(text, caret) == Some('$')
        && nth_char(text, caret.wrapping_sub(2)) != Some('$')
        && nth_char(text, caret + 1) != Some('$')
}

fn preceded_by_unescaped_backslash(text: &str, byte: usize) -> bool {
    ends_with_unescaped_backslash(text, byte)
}

fn ends_with_unescaped_backslash(text: &str, byte: usize) -> bool {
    let bytes = text.as_bytes();
    let mut n = 0usize;
    let mut i = byte;
    while i > 0 && bytes[i - 1] == b'\\' {
        n += 1;
        i -= 1;
    }
    n % 2 == 1
}

fn splice(
    text: &str,
    start_byte: usize,
    end_byte: usize,
    insert: &str,
    caret_offset_in_insert: usize,
) -> (String, CCursorRange) {
    let mut out = String::with_capacity(text.len() + insert.len());
    out.push_str(&text[..start_byte]);
    out.push_str(insert);
    out.push_str(&text[end_byte..]);
    let caret = byte_to_char(&out, start_byte + caret_offset_in_insert);
    (out, cursor(caret))
}

fn cursor(index: usize) -> CCursorRange {
    CCursorRange::one(CCursor::new(index))
}

fn next_char(text: &str, caret: usize) -> Option<char> {
    text.chars().nth(caret)
}

fn nth_char(text: &str, index: usize) -> Option<char> {
    text.chars().nth(index)
}

fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret(index: usize) -> CCursorRange {
        CCursorRange::one(CCursor::new(index))
    }

    fn sel(start: usize, end: usize) -> CCursorRange {
        CCursorRange::two(CCursor::new(start), CCursor::new(end))
    }

    fn text_event(ch: &str) -> Event {
        Event::Text(ch.into())
    }

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn apply_text(text: &str, index: usize, ch: &str) -> (String, usize) {
        let (out, range) = apply(text, caret(index), &text_event(ch)).unwrap();
        (out, range.primary.index)
    }

    #[test]
    fn dollar_outside_inserts_pair() {
        let (out, c) = apply_text("ab", 2, "$");
        assert_eq!(out, "ab$$");
        assert_eq!(c, 3);
    }

    #[test]
    fn second_dollar_in_empty_pair_makes_display() {
        let (out, c) = apply_text("$$", 1, "$");
        assert_eq!(out, "$$$$");
        assert_eq!(c, 2);
    }

    #[test]
    fn dollar_steps_over_closer() {
        let (out, c) = apply_text("$x$", 2, "$");
        assert_eq!(out, "$x$");
        assert_eq!(c, 3);
    }

    #[test]
    fn brace_and_paren_step_over() {
        let (out, c) = apply_text("$x{}$", 3, "}");
        assert_eq!(out, "$x{}$");
        assert_eq!(c, 4);

        let (out, c) = apply_text("$()$", 2, ")");
        assert_eq!(out, "$()$");
        assert_eq!(c, 3);
    }

    #[test]
    fn backspace_deletes_empty_pairs() {
        let (out, range) = apply("$$", caret(1), &key(Key::Backspace, Modifiers::NONE)).unwrap();
        assert_eq!((out.as_str(), range.primary.index), ("", 0));

        let (out, range) =
            apply("$$$$", caret(2), &key(Key::Backspace, Modifiers::NONE)).unwrap();
        assert_eq!((out.as_str(), range.primary.index), ("", 0));

        assert!(apply("$*$", caret(2), &key(Key::Backspace, Modifiers::NONE)).is_none());

        let (out, range) =
            apply("$({})$", caret(3), &key(Key::Backspace, Modifiers::NONE)).unwrap();
        assert_eq!((out.as_str(), range.primary.index), ("$()$", 2));

        let (out, range) =
            apply(r"\(\)", caret(2), &key(Key::Backspace, Modifiers::NONE)).unwrap();
        assert_eq!((out.as_str(), range.primary.index), ("", 0));

        let (out, range) = apply(
            r"$\left(\right)$",
            caret(7),
            &key(Key::Backspace, Modifiers::NONE),
        )
        .unwrap();
        assert_eq!((out.as_str(), range.primary.index), ("$$", 1));
    }

    #[test]
    fn latex_paren_autopair_outside_math() {
        let (out, c) = apply_text(r"\", 1, "(");
        assert_eq!(out, r"\(\)");
        assert_eq!(c, 2);
    }

    #[test]
    fn brace_autopair_inside_math() {
        let (out, c) = apply_text("$x$", 2, "{");
        assert_eq!(out, "$x{}$");
        assert_eq!(c, 3);
    }

    #[test]
    fn left_paren_inserts_right() {
        let (out, c) = apply_text(r"$a\left", 7, "(");
        assert_eq!(out, r"$a\left(\right)");
        assert_eq!(c, 8);
    }

    #[test]
    fn dollar_wraps_selection() {
        let (out, range) = apply("hello", sel(0, 5), &text_event("$")).unwrap();
        assert_eq!(out, "$hello$");
        assert_eq!(range.primary.index, 7);
    }

    #[test]
    fn escaped_dollar_stays_literal() {
        assert!(apply(r"\", caret(1), &text_event("$")).is_none());
        let (out, c) = apply_text(r"\\", 2, "$");
        assert_eq!(out, r"\\$$");
        assert_eq!(c, 3);
    }

    #[test]
    fn tab_jumps_placeholders_then_tabout() {
        // $\frac{}{}$ — placeholders at char 7 and 9; closer at 10.
        let text = r"$\frac{}{}$";
        let (out, range) = apply(text, caret(6), &key(Key::Tab, Modifiers::NONE)).unwrap();
        assert_eq!(out, text);
        assert_eq!(range.primary.index, 7);

        let (out, range) = apply(text, caret(7), &key(Key::Tab, Modifiers::NONE)).unwrap();
        assert_eq!(out, text);
        assert_eq!(range.primary.index, 9);

        let (out, range) = apply(text, caret(9), &key(Key::Tab, Modifiers::NONE)).unwrap();
        assert_eq!(out, text);
        assert_eq!(range.primary.index, 10);

        let (out, range) = apply(text, caret(9), &key(Key::Tab, Modifiers::SHIFT)).unwrap();
        assert_eq!(out, text);
        assert_eq!(range.primary.index, 7);
    }

    #[test]
    fn tab_outside_math_falls_through() {
        assert!(apply("plain", caret(2), &key(Key::Tab, Modifiers::NONE)).is_none());
    }

    #[test]
    fn tab_closes_unclosed_span() {
        let (out, range) = apply(r"$\alpha", caret(3), &key(Key::Tab, Modifiers::NONE)).unwrap();
        assert_eq!(out, r"$\alpha$");
        assert_eq!(range.primary.index, 7);
    }

    #[test]
    fn autopair_is_one_string_replacement() {
        let (out, range) = apply("", caret(0), &text_event("$")).unwrap();
        assert_eq!(out, "$$");
        assert_eq!(range.primary.index, 1);
        let (out, range) = apply(&out, range, &text_event("$")).unwrap();
        assert_eq!(out, "$$$$");
        assert_eq!(range.primary.index, 2);
    }
}
