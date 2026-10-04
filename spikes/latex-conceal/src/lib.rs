//! Throwaway spike: conceal-style inline math inside a stock `egui::TextEdit`.
//!
//! The buffer always holds raw source. A custom layouter swaps each concealed
//! `$…$` span for invisible, near-zero-width glyphs whose letter spacing
//! reserves the rendered equation's width, so the galley keeps one glyph per
//! source char and egui's caret, selection, click, and undo logic need no
//! index mapping. Rendered images are painted over the reserved rects.

use egui::text::{CCursor, LayoutJob, LayoutSection, TextFormat, TextWrapping};
use egui::{Align, Color32, FontFamily, FontId, Galley, Id, Pos2, Rect, Stroke, TextBuffer, Ui, Vec2};
use std::sync::Arc;

/// Char-indexed math island (`end` exclusive, delimiters included).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub inner_start: usize,
    pub inner_end: usize,
    pub display: bool,
    pub closed: bool,
}

impl Span {
    pub fn inner<'a>(&self, chars: &'a [char]) -> String {
        chars[self.inner_start..self.inner_end].iter().collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Render {
    /// Rendered; size in screen points.
    Ready(Vec2),
    Pending,
    Error(String),
}

pub fn parse(text: &str) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && chars.get(i + 1) == Some(&'$') {
            i += 2;
            continue;
        }
        if chars[i] != '$' {
            i += 1;
            continue;
        }
        let display = chars.get(i + 1) == Some(&'$');
        let open = if display { 2 } else { 1 };
        let inner_start = i + open;
        let mut j = inner_start;
        let mut close = None;
        while j < chars.len() {
            if chars[j] == '\\' && chars.get(j + 1) == Some(&'$') {
                j += 2;
                continue;
            }
            if chars[j] == '$' {
                let double = chars.get(j + 1) == Some(&'$');
                if double == display {
                    close = Some(j);
                    break;
                }
                j += if double { 2 } else { 1 };
                continue;
            }
            j += 1;
        }
        match close {
            Some(c) => {
                spans.push(Span {
                    start: i,
                    end: c + open,
                    inner_start,
                    inner_end: c,
                    display,
                    closed: true,
                });
                i = c + open;
            }
            None => {
                spans.push(Span {
                    start: i,
                    end: chars.len(),
                    inner_start,
                    inner_end: chars.len(),
                    display,
                    closed: false,
                });
                break;
            }
        }
    }
    spans
}

/// Reveal rule: the caret is strictly between the outer delimiters, or the
/// selection overlaps the span. Caret exactly at `start` or `end` keeps it rendered.
pub fn is_revealed(span: &Span, sel: Option<(usize, usize)>) -> bool {
    if !span.closed {
        return true;
    }
    let Some((a, b)) = sel else { return false };
    let (lo, hi) = (a.min(b), a.max(b));
    lo < span.end && hi > span.start
}

pub struct Style {
    pub font: FontId,
    pub color: Color32,
    pub source_color: Color32,
    pub source_bg: Color32,
    pub error: Color32,
}

#[derive(Clone, Debug)]
pub struct Concealed {
    pub span: Span,
    pub inner: String,
    pub size: Vec2,
}

pub struct Built {
    pub job: LayoutJob,
    pub concealed: Vec<Concealed>,
    pub revealed: Vec<Span>,
}

const TINY: f32 = 0.01;

/// Build the conceal-aware layout job. `sel` is the caret/selection in chars.
pub fn build_job(
    text: &str,
    sel: Option<(usize, usize)>,
    wrap_width: f32,
    style: &Style,
    render: &dyn Fn(&str, bool) -> Render,
) -> Built {
    let chars: Vec<char> = text.chars().collect();
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        ..Default::default()
    };
    let prose = TextFormat {
        font_id: style.font.clone(),
        color: style.color,
        valign: Align::Center,
        ..Default::default()
    };
    let push = |job: &mut LayoutJob, s: &str, format: TextFormat| {
        if s.is_empty() {
            return;
        }
        let start = job.text.len();
        job.text.push_str(s);
        job.sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: start..job.text.len(),
            format,
        });
    };
    let mut concealed = Vec::new();
    let mut revealed = Vec::new();
    let mut at = 0;
    for span in parse(text) {
        let before: String = chars[at..span.start].iter().collect();
        push(&mut job, &before, prose.clone());
        at = span.end;
        let raw: String = chars[span.start..span.end].iter().collect();
        let inner = span.inner(&chars);
        let state = if inner.trim().is_empty() {
            Render::Pending
        } else {
            render(&inner, span.display)
        };
        let reveal = is_revealed(&span, sel) || !matches!(state, Render::Ready(_));
        if reveal {
            revealed.push(span);
            let mut format = TextFormat {
                color: style.source_color,
                background: style.source_bg,
                ..prose.clone()
            };
            if let Render::Error(_) = state {
                format.underline = Stroke::new(1.0_f32, style.error);
            }
            push(&mut job, &raw, format);
            continue;
        }
        let Render::Ready(size) = state else { unreachable!() };
        let open = span.inner_start - span.start;
        let body_len = span.end - span.inner_start;
        let width = if span.display {
            wrap_width - 1.0
        } else {
            size.x.min(wrap_width - 1.0)
        };
        let hidden = TextFormat {
            font_id: FontId::new(TINY, style.font.family.clone()),
            color: Color32::TRANSPARENT,
            line_height: Some(size.y),
            valign: Align::Center,
            ..Default::default()
        };
        // Opener as spaces = a wrap point right before the math, even after prose
        // with no space (`foo$x$`). Body chars become non-breaking letters so egui
        // never wraps inside the equation; display math ends with a space so the
        // following prose wraps onto its own row.
        push(&mut job, &" ".repeat(open), hidden.clone());
        let mut body: String = "x".repeat(body_len - 1);
        body.push(if span.display { ' ' } else { 'x' });
        push(
            &mut job,
            &body,
            TextFormat {
                extra_letter_spacing: width / (body_len - 1) as f32,
                ..hidden
            },
        );
        concealed.push(Concealed {
            span,
            inner,
            size: Vec2::new(width.min(size.x), size.y),
        });
    }
    let rest: String = chars[at..].iter().collect();
    push(&mut job, &rest, prose);
    Built {
        job,
        concealed,
        revealed,
    }
}

/// Galley-space rect of a concealed equation.
pub fn math_rect(galley: &Galley, c: &Concealed) -> Rect {
    let next_row = |index| CCursor {
        index,
        prefer_next_row: true,
    };
    let left = galley.pos_from_cursor(next_row(c.span.inner_start));
    let right = galley.pos_from_cursor(next_row(c.span.end - 1));
    let center_y = left.center().y;
    let x0 = if c.span.display {
        left.min.x + ((right.min.x - left.min.x) - c.size.x).max(0.0) * 0.5
    } else {
        left.min.x
    };
    Rect::from_min_size(
        Pos2::new(x0, center_y - c.size.y * 0.5),
        c.size,
    )
}

pub struct EditorOutput {
    pub response: egui::Response,
    pub galley: Arc<Galley>,
    pub galley_pos: Pos2,
    /// Screen rects of concealed equations with their inner source.
    pub math: Vec<(Rect, String, bool)>,
    pub revealed: Vec<Span>,
    pub caret: Option<(usize, usize)>,
    /// True when the layout used a stale caret and this pass was discarded.
    pub discarded: bool,
}

fn revealed_for(text: &str, sel: Option<(usize, usize)>, render: &dyn Fn(&str, bool) -> Render) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    parse(text)
        .into_iter()
        .filter(|span| {
            let inner = span.inner(&chars);
            let state = if inner.trim().is_empty() {
                Render::Pending
            } else {
                render(&inner, span.display)
            };
            is_revealed(span, sel) || !matches!(state, Render::Ready(_))
        })
        .collect()
}

pub fn conceal_editor(
    ui: &mut Ui,
    id: Id,
    text: &mut String,
    width: f32,
    style: &Style,
    render: &dyn Fn(&str, bool) -> Render,
) -> EditorOutput {
    // The layouter only sees the buffer, so feed it last pass's caret. egui
    // re-lays out after edits but not after pure caret moves; the discard
    // below covers that gap.
    let sel = egui::text_edit::TextEditState::load(ui.ctx(), id)
        .and_then(|state| state.cursor.char_range())
        .map(|range| (range.primary.index, range.secondary.index));
    let mut last: Option<(Vec<Span>, Vec<Concealed>)> = None;
    let mut layouter = |ui: &Ui, buf: &dyn TextBuffer, wrap: f32| {
        let built = build_job(buf.as_str(), sel, wrap, style, render);
        last = Some((built.revealed, built.concealed));
        ui.fonts_mut(|fonts| fonts.layout_job(built.job))
    };
    let output = egui::TextEdit::multiline(text)
        .id(id)
        .font(style.font.clone())
        .desired_width(width)
        .desired_rows(1)
        .layouter(&mut layouter)
        .show(ui);
    let (used_revealed, concealed) = last.unwrap_or_default();
    let caret = output
        .cursor_range
        .map(|range| (range.primary.index, range.secondary.index));
    let want = revealed_for(text, caret, render);
    let discarded = want != used_revealed;
    if discarded {
        ui.ctx().request_discard("conceal reveal set changed");
    }
    let math = concealed
        .iter()
        .map(|c| {
            (
                math_rect(&output.galley, c).translate(output.galley_pos.to_vec2()),
                c.inner.clone(),
                c.span.display,
            )
        })
        .collect();
    EditorOutput {
        response: output.response,
        galley: output.galley,
        galley_pos: output.galley_pos,
        math,
        revealed: want,
        caret,
        discarded,
    }
}

/// Stand-in for Typst: LaTeX-ish source → Unicode text + a "tall" flag.
pub fn fake_tex(inner: &str) -> Result<(String, bool), String> {
    if inner.contains(r"\bad") {
        return Err(r"unknown command \bad".into());
    }
    let depth = inner.chars().try_fold(0i32, |d, ch| {
        let d = d + match ch {
            '{' => 1,
            '}' => -1,
            _ => 0,
        };
        (d >= 0).then_some(d)
    });
    if depth != Some(0) {
        return Err("unbalanced braces".into());
    }
    let tall = inner.contains(r"\frac") || inner.contains(r"\sum") || inner.contains(r"\int");
    let mut s = inner.to_string();
    for (from, to) in [
        (r"\alpha", "α"),
        (r"\beta", "β"),
        (r"\gamma", "γ"),
        (r"\pi", "π"),
        (r"\sum", "∑"),
        (r"\int", "∫"),
        (r"\infty", "∞"),
        (r"\sqrt", "√"),
        (r"\cdot", "·"),
        (r"\le", "≤"),
        (r"\to", "→"),
        ("^2", "²"),
        ("^n", "ⁿ"),
        ("_0", "₀"),
        ("_1", "₁"),
        ("_i", "ᵢ"),
        ("_n", "ₙ"),
    ] {
        s = s.replace(from, to);
    }
    if let Some(rest) = s.strip_prefix(r"\frac") {
        s = rest.replacen("}{", "⁄", 1);
    }
    let s: String = s.chars().filter(|ch| !matches!(ch, '{' | '}' | '\\')).collect();
    Ok((s, tall))
}

pub fn fake_size(ui_fonts: &mut egui::epaint::text::FontsView<'_>, text: &str, tall: bool, font_px: f32) -> Vec2 {
    let galley = ui_fonts.layout_no_wrap(
        text.to_owned(),
        FontId::new(font_px, FontFamily::Proportional),
        Color32::WHITE,
    );
    let mut size = galley.size() + Vec2::new(6.0, 2.0);
    if tall {
        size.y *= 1.9;
    }
    size
}
