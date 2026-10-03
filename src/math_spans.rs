//! Parse `$...$` / `$$...$$` math islands inside text annotations.
//!
//! `\$` is a literal dollar in prose. `$$` is always checked before `$`.

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

/// Split `content` into prose and math spans.
pub fn parse_spans(content: &str) -> Vec<Span> {
    let bytes = content.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    let mut prose_start = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            i += 2;
            continue;
        }
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }

        let display = i + 1 < bytes.len() && bytes[i + 1] == b'$';
        let open_len = if display { 2 } else { 1 };
        let delim_start = i;

        if prose_start < delim_start {
            spans.push(Span::Prose {
                start: prose_start,
                end: delim_start,
            });
        }

        let inner_start = delim_start + open_len;
        let close = find_closing_delim(bytes, inner_start, display);
        match close {
            Some(close_at) => {
                let end = close_at + open_len;
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
            }
            None => {
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

fn find_closing_delim(bytes: &[u8], from: usize, display: bool) -> Option<usize> {
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
        if display {
            if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
                return Some(i);
            }
            i += 1;
        } else {
            // A `$$` while looking for single `$` starts display elsewhere; treat
            // the first `$` of `$$` as a closer only when it is a lone `$`.
            if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
                // `$$` is not a valid close for inline `$...$`.
                i += 2;
                continue;
            }
            return Some(i);
        }
    }
    None
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
    let delim = if span.display { "$$" } else { "$" };
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
}
