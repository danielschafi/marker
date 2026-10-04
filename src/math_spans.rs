//! Parse `$...$` / `$$...$$`, `\(...\)`, `\[...\]` math islands in text annotations.
//!
//! `\$` is a literal dollar in prose. `$$` is checked before `$` except immediately
//! after a closing inline `$`, where adjacent `$$` opens a second inline span (Pandoc).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delim {
    DollarInline,
    DollarDisplay,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Span {
    Prose {
        /// Byte range in the source string (includes `\$` → `$` mapping is not
        /// expanded here; [`display_prose`] expands escapes for painting/PDF).
        start: usize,
        end: usize,
    },
    Math {
        /// Inclusive byte range of the full island, including delimiters.
        start: usize,
        end: usize,
        /// Inner LaTeX (between delimiters), not trimmed of internal whitespace.
        inner_start: usize,
        inner_end: usize,
        display: bool,
        /// True when a closing delimiter was found.
        closed: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MathSpanRef {
    pub index: usize,
    pub start: usize,
    pub end: usize,
    pub inner_start: usize,
    pub inner_end: usize,
    pub display: bool,
    pub closed: bool,
}

/// Map byte offsets in `content` to character indexes (for galley / caret mapping).
#[allow(dead_code)] // public for LT0; only unit tests call this on the bin target today
pub fn byte_range_char_indices(content: &str, start: usize, end: usize) -> (usize, usize) {
    let start = start.min(content.len());
    let end = end.min(content.len());
    (
        content[..start].chars().count(),
        content[..end].chars().count(),
    )
}

impl Span {
    /// Character indexes for this span's byte range in `content`.
    #[allow(dead_code)]
    pub fn char_range(&self, content: &str) -> (usize, usize) {
        match self {
            Span::Prose { start, end } | Span::Math { start, end, .. } => {
                byte_range_char_indices(content, *start, *end)
            }
        }
    }
}

/// Split `content` into prose and math spans.
pub fn parse_spans(content: &str) -> Vec<Span> {
    let bytes = content.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    let mut prose_start = 0;
    let mut after_inline_dollar_close = false;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if next == b'$' {
                i += 2;
                after_inline_dollar_close = false;
                continue;
            }
            if next == b'(' || next == b'[' {
                let display = next == b'[';
                let open_len = 2;
                let delim_start = i;
                if prose_start < delim_start {
                    spans.push(Span::Prose {
                        start: prose_start,
                        end: delim_start,
                    });
                }
                let inner_start = delim_start + open_len;
                if let Some(close_at) = find_latex_close(bytes, inner_start, display) {
                    let close_len = 2;
                    let end = close_at + close_len;
                    spans.push(Span::Math {
                        start: delim_start,
                        end,
                        inner_start,
                        inner_end: close_at,
                        display,
                        closed: true,
                    });
                    i = end;
                    prose_start = end;
                    after_inline_dollar_close = false;
                } else {
                    spans.push(Span::Math {
                        start: delim_start,
                        end: bytes.len(),
                        inner_start,
                        inner_end: bytes.len(),
                        display,
                        closed: false,
                    });
                    return spans;
                }
                continue;
            }
        }

        if bytes[i] != b'$' {
            i += 1;
            after_inline_dollar_close = false;
            continue;
        }

        let (delim, _open_len, inner_start) =
            match dollar_open_at(bytes, i, after_inline_dollar_close) {
                Some(parsed) => parsed,
                None => {
                    i += 1;
                    after_inline_dollar_close = false;
                    continue;
                }
            };
        let display = matches!(delim, Delim::DollarDisplay);
        let delim_start = i;

        if prose_start < delim_start {
            spans.push(Span::Prose {
                start: prose_start,
                end: delim_start,
            });
        }

        let close = find_dollar_close(bytes, inner_start, delim);
        match close {
            Some(close_at) => {
                let close_len = dollar_close_len(delim);
                let end = close_at + close_len;
                spans.push(Span::Math {
                    start: delim_start,
                    end,
                    inner_start,
                    inner_end: close_at,
                    display,
                    closed: true,
                });
                i = end;
                prose_start = end;
                after_inline_dollar_close =
                    matches!(delim, Delim::DollarInline) && close_len == 1;
            }
            None => {
                if matches!(delim, Delim::DollarInline) {
                    // No valid closing `$` (Pandoc price rules) — treat opener as prose.
                    i = delim_start + 1;
                    after_inline_dollar_close = false;
                    continue;
                }
                spans.push(Span::Math {
                    start: delim_start,
                    end: bytes.len(),
                    inner_start,
                    inner_end: bytes.len(),
                    display,
                    closed: false,
                });
                return spans;
            }
        }
    }

    if prose_start < bytes.len() {
        spans.push(Span::Prose {
            start: prose_start,
            end: bytes.len(),
        });
    }
    spans
}

fn dollar_open_at(
    bytes: &[u8],
    i: usize,
    after_inline_dollar_close: bool,
) -> Option<(Delim, usize, usize)> {
    if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
        if after_inline_dollar_close {
            // `$` immediately after an inline close: twin `$` opens the next inline span.
            let inner_start = i + 2;
            if inner_start <= bytes.len() && valid_inline_dollar_open(bytes, i) {
                return Some((Delim::DollarInline, 2, inner_start));
            }
            return None;
        }
        return Some((Delim::DollarDisplay, 2, i + 2));
    }
    if valid_inline_dollar_open(bytes, i) {
        return Some((Delim::DollarInline, 1, i + 1));
    }
    None
}

fn dollar_close_len(delim: Delim) -> usize {
    match delim {
        Delim::DollarDisplay => 2,
        Delim::DollarInline => 1,
    }
}

fn valid_inline_dollar_open(bytes: &[u8], i: usize) -> bool {
    let after = i + 1;
    after < bytes.len()
        && !bytes[after].is_ascii_whitespace()
        && !bytes[after].is_ascii_digit()
}

fn valid_inline_dollar_close(bytes: &[u8], at: usize) -> bool {
    if at == 0 || bytes[at - 1].is_ascii_whitespace() {
        return false;
    }
    let after = at + 1;
    after >= bytes.len() || !bytes[after].is_ascii_digit()
}

fn find_dollar_close(bytes: &[u8], from: usize, delim: Delim) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            i += 2;
            continue;
        }
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        match delim {
            Delim::DollarDisplay => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
                    return Some(i);
                }
                i += 1;
            }
            Delim::DollarInline => {
                if valid_inline_dollar_close(bytes, i) {
                    return Some(i);
                }
                if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
                    i += 2;
                    continue;
                }
                i += 1;
            }
        }
    }
    None
}

fn find_latex_close(bytes: &[u8], from: usize, display: bool) -> Option<usize> {
    let close = if display { b']' } else { b')' };
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == close {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn closing_delim_text(content: &str, span: &MathSpanRef) -> &'static str {
    if span.start + 1 < content.len() {
        if content[span.start..].starts_with("\\[") {
            return "\\]";
        }
        if content[span.start..].starts_with("\\(") {
            return "\\)";
        }
    }
    if span.display {
        "$$"
    } else {
        "$"
    }
}

/// Expand `\$` → `$` for display / FreeText fallbacks.
pub fn display_prose(slice: &str) -> String {
    let mut out = String::with_capacity(slice.len());
    let mut chars = slice.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek() == Some(&'$') {
            chars.next();
            out.push('$');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Byte caret index → math span containing it, if any (delimiters count).
pub fn math_span_at(content: &str, caret: usize) -> Option<MathSpanRef> {
    let caret = caret.min(content.len());
    for (index, span) in parse_spans(content).into_iter().enumerate() {
        if let Span::Math {
            start,
            end,
            inner_start,
            inner_end,
            display,
            closed,
        } = span
        {
            // `end` is exclusive (byte after closing delim). Include the
            // delimiters themselves, but not the position just after.
            if caret >= start && caret < end {
                return Some(MathSpanRef {
                    index,
                    start,
                    end,
                    inner_start,
                    inner_end,
                    display,
                    closed,
                });
            }
        }
    }
    None
}

/// Caret inside a math span for typing aids (R7 / R8).
///
/// Closed spans use `start < caret < end` (boundaries count as outside). An
/// unclosed span also accepts `caret == end` (EOF while still typing).
pub fn caret_inside_math(content: &str, caret: usize) -> Option<MathSpanRef> {
    let caret = caret.min(content.len());
    for (index, span) in parse_spans(content).into_iter().enumerate() {
        if let Span::Math {
            start,
            end,
            inner_start,
            inner_end,
            display,
            closed,
        } = span
        {
            let inside = if closed {
                caret > start && caret < end
            } else {
                caret > start && caret <= end
            };
            if inside {
                return Some(MathSpanRef {
                    index,
                    start,
                    end,
                    inner_start,
                    inner_end,
                    display,
                    closed,
                });
            }
        }
    }
    None
}

/// Replace the inner source of a math span, preserving delimiters / openness.
pub fn replace_math_inner(content: &str, span: &MathSpanRef, new_inner: &str) -> String {
    let mut out = String::with_capacity(content.len() + new_inner.len());
    out.push_str(&content[..span.inner_start]);
    out.push_str(new_inner);
    if span.closed {
        out.push_str(&content[span.inner_end..]);
    } else {
        // Keep any trailing prose after an unclosed open (none by parse rules).
        out.push_str(&content[span.inner_end..]);
    }
    out
}

/// Ensure the math span is closed and return the caret byte index just after it.
pub fn exit_math_span(content: &str, span: &MathSpanRef) -> (String, usize) {
    if span.closed {
        return (content.to_string(), span.end);
    }
    let delim = closing_delim_text(content, span);
    let mut out = String::with_capacity(content.len() + delim.len());
    out.push_str(&content[..span.end]);
    out.push_str(delim);
    out.push_str(&content[span.end..]);
    let caret = span.end + delim.len();
    (out, caret)
}

/// Stable cache key for an inline math island.
pub fn span_key(inner: &str, display: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    inner.hash(&mut hasher);
    display.hash(&mut hasher);
    hasher.finish()
}

/// Prose-only FreeText fallback: math islands dropped, `\$` → `$`.
pub fn prose_only(content: &str) -> String {
    let mut out = String::new();
    for span in parse_spans(content) {
        match span {
            Span::Prose { start, end } => {
                out.push_str(&display_prose(&content[start..end]));
            }
            Span::Math { closed, .. } if !closed => {
                // Unclosed: leave nothing (still editing).
            }
            Span::Math { .. } => {
                if !out.is_empty() && !out.ends_with([' ', '\n']) {
                    out.push(' ');
                }
            }
        }
    }
    out
}

pub fn has_math(content: &str) -> bool {
    parse_spans(content)
        .iter()
        .any(|span| matches!(span, Span::Math { .. }))
}

pub fn closed_math_spans(content: &str) -> Vec<MathSpanRef> {
    parse_spans(content)
        .into_iter()
        .enumerate()
        .filter_map(|(index, span)| match span {
            Span::Math {
                start,
                end,
                inner_start,
                inner_end,
                display,
                closed,
            } if closed => Some(MathSpanRef {
                index,
                start,
                end,
                inner_start,
                inner_end,
                display,
                closed,
            }),
            _ => None,
        })
        .collect()
}

/// A laid-out run relative to the top-left of the text box (y down, points).
///
/// Paint and save use `rich_text::layout_rich_text`. This stays so existing
/// callers keep compiling.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug)]
pub enum LaidRun {
    Prose {
        text: String,
        x: f32,
        y: f32,
        #[allow(dead_code)]
        w: f32,
        #[allow(dead_code)]
        h: f32,
    },
    Math {
        inner: String,
        display: bool,
        key: u64,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
}

/// Flow prose + math into lines within `max_width`.
///
/// `measure_prose(text) -> (width, height)` and `measure_math(inner, display) -> (w, h)`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn layout_runs(
    content: &str,
    max_width: f32,
    line_height: f32,
    measure_prose: &dyn Fn(&str) -> (f32, f32),
    measure_math: &dyn Fn(&str, bool) -> (f32, f32),
) -> (Vec<LaidRun>, f32) {
    let max_width = max_width.max(8.0);
    let mut runs = Vec::new();
    let mut x = 0.0_f32;
    let mut y = 0.0_f32;
    let mut row_h = line_height;

    let new_line = |x: &mut f32, y: &mut f32, row_h: &mut f32| {
        *y += *row_h;
        *x = 0.0;
        *row_h = line_height;
    };

    for span in parse_spans(content) {
        match span {
            Span::Prose { start, end } => {
                let raw = display_prose(&content[start..end]);
                if raw.is_empty() {
                    continue;
                }
                // Word-wrap on spaces/newlines.
                for piece in split_prose_tokens(&raw) {
                    if piece == "\n" {
                        new_line(&mut x, &mut y, &mut row_h);
                        continue;
                    }
                    let (pw, ph) = measure_prose(&piece);
                    let ph = ph.max(line_height);
                    if x > 0.0 && x + pw > max_width {
                        new_line(&mut x, &mut y, &mut row_h);
                    }
                    runs.push(LaidRun::Prose {
                        text: piece,
                        x,
                        y,
                        w: pw.min(max_width),
                        h: ph,
                    });
                    x += pw;
                    row_h = row_h.max(ph);
                }
            }
            Span::Math {
                inner_start,
                inner_end,
                display,
                closed,
                ..
            } => {
                if !closed {
                    continue;
                }
                let inner = content[inner_start..inner_end].to_string();
                let key = span_key(&inner, display);
                let (mw, mh) = measure_math(&inner, display);
                let mw = mw.max(line_height * 0.5).min(max_width);
                let mh = mh.max(line_height);
                if display {
                    if x > 0.0 {
                        new_line(&mut x, &mut y, &mut row_h);
                    }
                    runs.push(LaidRun::Math {
                        inner,
                        display,
                        key,
                        x: 0.0,
                        y,
                        w: mw,
                        h: mh,
                    });
                    y += mh;
                    x = 0.0;
                    row_h = line_height;
                } else {
                    if x > 0.0 && x + mw > max_width {
                        new_line(&mut x, &mut y, &mut row_h);
                    }
                    runs.push(LaidRun::Math {
                        inner,
                        display,
                        key,
                        x,
                        y,
                        w: mw,
                        h: mh,
                    });
                    x += mw;
                    row_h = row_h.max(mh);
                }
            }
        }
    }
    let height = y + row_h;
    (runs, height)
}

#[cfg_attr(not(test), allow(dead_code))]
fn split_prose_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for ch in text.chars() {
        if ch == '\n' {
            if !buf.is_empty() {
                out.push(std::mem::take(&mut buf));
            }
            out.push("\n".into());
        } else if ch.is_whitespace() {
            buf.push(ch);
            out.push(std::mem::take(&mut buf));
        } else {
            buf.push(ch);
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

/// Wrap inner source so the Typst path treats `$$` islands as display math.
pub fn source_for_render(inner: &str, display: bool) -> String {
    if display {
        format!("$${inner}$$")
    } else {
        format!("${inner}$")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_and_display() {
        let spans = parse_spans(r"hi $x^2$ and $$\frac{a}{b}$$ done");
        assert_eq!(spans.len(), 5);
        assert!(matches!(spans[0], Span::Prose { .. }));
        assert!(matches!(
            spans[1],
            Span::Math {
                display: false,
                closed: true,
                ..
            }
        ));
        match &spans[1] {
            Span::Math {
                inner_start,
                inner_end,
                ..
            } => assert_eq!(&"hi $x^2$ and $$\\frac{a}{b}$$ done"[*inner_start..*inner_end], "x^2"),
            _ => panic!("expected math"),
        }
        assert!(matches!(
            spans[3],
            Span::Math {
                display: true,
                closed: true,
                ..
            }
        ));
        match &spans[3] {
            Span::Math {
                inner_start,
                inner_end,
                ..
            } => assert_eq!(
                &r"hi $x^2$ and $$\frac{a}{b}$$ done"[*inner_start..*inner_end],
                r"\frac{a}{b}"
            ),
            _ => panic!("expected display math"),
        }
    }

    #[test]
    fn escapes_literal_dollar() {
        let content = r"price \$5 and $x$ ok";
        let spans = parse_spans(content);
        assert_eq!(spans.len(), 3);
        match &spans[0] {
            Span::Prose { start, end } => {
                assert_eq!(display_prose(&content[*start..*end]), "price $5 and ");
            }
            _ => panic!("expected prose"),
        }
        assert!(matches!(
            spans[1],
            Span::Math {
                display: false,
                closed: true,
                ..
            }
        ));
        match &spans[2] {
            Span::Prose { start, end } => assert_eq!(&content[*start..*end], " ok"),
            _ => panic!("expected trailing prose"),
        }
    }

    #[test]
    fn unclosed_math_to_end() {
        let spans = parse_spans(r"pre $$\\alpha");
        assert_eq!(spans.len(), 2);
        assert!(matches!(
            spans[1],
            Span::Math {
                display: true,
                closed: false,
                ..
            }
        ));
    }

    #[test]
    fn caret_inside_math() {
        let content = "a $b$ c";
        assert!(math_span_at(content, 2).is_some()); // on `$`
        assert!(math_span_at(content, 3).is_some()); // on `b`
        assert!(math_span_at(content, 4).is_some()); // on closing `$`
        assert!(math_span_at(content, 0).is_none());
        assert!(math_span_at(content, 5).is_none());
    }

    #[test]
    fn caret_inside_math_strict_boundaries() {
        let content = "a $b$ c";
        assert!(super::caret_inside_math(content, 2).is_none()); // on opener
        assert!(super::caret_inside_math(content, 3).is_some()); // on `b`
        assert!(super::caret_inside_math(content, 4).is_some()); // on closer char
        assert!(super::caret_inside_math(content, 5).is_none()); // after span
        // Unclosed inline `$` is prose (Pandoc); unclosed `$$` / `\(` stay math to EOF.
        assert!(super::caret_inside_math(r"$\alpha", 3).is_none());
        let open = r"$$\alpha";
        assert!(super::caret_inside_math(open, open.len()).is_some());
        let paren = r"\(x";
        assert!(super::caret_inside_math(paren, paren.len()).is_some());
    }

    #[test]
    fn exit_closes_unclosed() {
        let content = r"x $$\alpha";
        let span = math_span_at(content, 3).unwrap();
        let (next, caret) = exit_math_span(content, &span);
        assert_eq!(next, r"x $$\alpha$$");
        assert_eq!(caret, next.len());
    }

    #[test]
    fn replace_inner_keeps_delims() {
        let content = r"go $a$";
        let span = math_span_at(content, 4).unwrap();
        let next = replace_math_inner(content, &span, r"\langle x\rangle");
        assert_eq!(next, r"go $\langle x\rangle$");
    }

    #[test]
    fn prose_only_strips_math() {
        assert_eq!(prose_only(r"hi $x$ there"), "hi  there");
        assert_eq!(prose_only(r"cost \$5"), "cost $5");
    }

    #[test]
    fn prefers_display_delim() {
        let spans = parse_spans("$$a$$");
        assert_eq!(spans.len(), 1);
        assert!(matches!(
            spans[0],
            Span::Math {
                display: true,
                closed: true,
                ..
            }
        ));
        match &spans[0] {
            Span::Math {
                inner_start,
                inner_end,
                ..
            } => assert_eq!(&"$$a$$"[*inner_start..*inner_end], "a"),
            _ => panic!("expected math"),
        }
    }

    #[test]
    fn prices_stay_prose() {
        let content = "$5 and $10";
        let spans = parse_spans(content);
        assert_eq!(spans.len(), 1);
        assert!(matches!(spans[0], Span::Prose { .. }));
        assert!(!has_math(content));
    }

    #[test]
    fn adjacent_inline_dollars() {
        let content = "$a$$b$";
        let spans = parse_spans(content);
        assert_eq!(spans.len(), 2);
        assert!(matches!(
            spans[0],
            Span::Math {
                display: false,
                closed: true,
                ..
            }
        ));
        assert!(matches!(
            spans[1],
            Span::Math {
                display: false,
                closed: true,
                ..
            }
        ));
        match (&spans[0], &spans[1]) {
            (
                Span::Math {
                    inner_start: a0,
                    inner_end: a1,
                    ..
                },
                Span::Math {
                    inner_start: b0,
                    inner_end: b1,
                    ..
                },
            ) => {
                assert_eq!(&content[*a0..*a1], "a");
                assert_eq!(&content[*b0..*b1], "b");
            }
            _ => panic!("expected two inline math spans"),
        }
    }

    #[test]
    fn latex_paren_delimiters() {
        let content = r"see \(a+b\) and \[x\]";
        let spans = parse_spans(content);
        assert_eq!(spans.len(), 4);
        assert!(matches!(
            spans[1],
            Span::Math {
                display: false,
                closed: true,
                ..
            }
        ));
        assert!(matches!(
            spans[3],
            Span::Math {
                display: true,
                closed: true,
                ..
            }
        ));
        match &spans[1] {
            Span::Math {
                inner_start,
                inner_end,
                ..
            } => assert_eq!(&content[*inner_start..*inner_end], "a+b"),
            _ => panic!("expected inline latex"),
        }
        match &spans[3] {
            Span::Math {
                inner_start,
                inner_end,
                ..
            } => assert_eq!(&content[*inner_start..*inner_end], "x"),
            _ => panic!("expected display latex"),
        }
    }

    #[test]
    fn unclosed_latex_paren() {
        let spans = parse_spans(r"tail \( \alpha");
        assert_eq!(spans.len(), 2);
        assert!(matches!(
            spans[1],
            Span::Math {
                display: false,
                closed: false,
                ..
            }
        ));
    }

    #[test]
    fn exit_closes_unclosed_latex_paren() {
        let content = r"q \(x";
        let span = math_span_at(content, 2).unwrap();
        let (next, caret) = exit_math_span(content, &span);
        assert_eq!(next, r"q \(x\)");
        assert_eq!(caret, next.len());
    }

    #[test]
    fn mixed_prices_and_equation() {
        let content = "Item $5, solve $x^2$ for x.";
        let spans = parse_spans(content);
        assert!(has_math(content));
        let math: Vec<_> = spans
            .iter()
            .filter(|s| matches!(s, Span::Math { .. }))
            .collect();
        assert_eq!(math.len(), 1);
        match math[0] {
            Span::Math {
                inner_start,
                inner_end,
                display: false,
                ..
            } => assert_eq!(&content[*inner_start..*inner_end], "x^2"),
            _ => panic!("expected one inline equation"),
        }
    }

    #[test]
    fn layout_runs_still_places_inline_math() {
        let (runs, height) = layout_runs(
            "ab $x$",
            200.0,
            12.0,
            &|text| (text.chars().count() as f32 * 5.0, 12.0),
            &|inner, _| ((inner.chars().count() as f32 * 8.0).max(8.0), 14.0),
        );
        assert!(height >= 12.0);
        let mut saw_math = false;
        for run in &runs {
            match run {
                LaidRun::Prose { text, x, y, .. } => {
                    assert!(!text.is_empty());
                    assert!(*x >= 0.0 && *y >= 0.0);
                }
                LaidRun::Math {
                    inner,
                    display,
                    key,
                    x,
                    y,
                    w,
                    h,
                } => {
                    assert!(!display);
                    assert_eq!(inner, "x");
                    assert_eq!(*key, span_key(inner, false));
                    assert!(*x > 0.0 && *y >= 0.0 && *w > 0.0 && *h > 0.0);
                    saw_math = true;
                }
            }
        }
        assert!(saw_math);
    }

    #[test]
    fn byte_range_char_indices_counts_utf8() {
        let content = "é $x$";
        let spans = parse_spans(content);
        let (c0, c1) = spans[0].char_range(content);
        assert_eq!(c0, 0);
        assert_eq!(c1, 2); // "é " is two chars
    }
}
