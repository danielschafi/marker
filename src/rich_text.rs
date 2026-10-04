//! One rich-text layout in page points, shared by view-mode paint, save, and
//! the conceal editor.
//!
//! `build_job` / `math_rect` keep one glyph per source character so the caret
//! lands inside an equation without a caret map.

use crate::math_spans::{self, Span};

#[cfg(test)]
use egui::text::CCursor;
#[cfg(test)]
use egui::{Color32, FontId, Galley, Pos2, Vec2};

/// Rendered equation size in PDF points. `baseline_pt` is the distance from
/// the top of that box to the baseline. `None` means the painter centers it.
#[derive(Clone, Copy, Debug)]
pub struct MathMetrics {
    pub w_pt: f32,
    pub h_pt: f32,
    pub baseline_pt: Option<f32>,
}

/// Widths in PDF points. The egui implementation lives next to the painter.
pub trait TextMeasure {
    fn width(&self, text: &str, size_pt: f32) -> f32;
    fn line_height(&self, size_pt: f32) -> f32;
    fn ascent(&self, size_pt: f32) -> f32;
}

#[derive(Clone, Debug, PartialEq)]
pub enum LaidRun {
    Prose {
        text: String,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    Math {
        inner: String,
        display: bool,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        baseline_pt: Option<f32>,
    },
}

struct Piece {
    prose: Option<String>,
    inner: String,
    display: bool,
    x: f32,
    y_off: f32,
    w: f32,
    h: f32,
    baseline_pt: Option<f32>,
}

/// Lay out prose and math in page points. Callers pass the annotation's real
/// size and width — there is no 8 px screen clamp and no 6 pt save clamp.
pub fn layout_rich_text(
    content: &str,
    size_pt: f32,
    width_pt: f32,
    measure: &dyn TextMeasure,
    preview: &dyn Fn(&str, bool) -> Option<MathMetrics>,
) -> Vec<LaidRun> {
    let width_pt = width_pt.max(1.0);
    let line_height = measure.line_height(size_pt).max(1.0);
    let ascent = measure.ascent(size_pt).clamp(0.0, line_height);
    let mut runs = Vec::new();
    let mut line: Vec<Piece> = Vec::new();
    let mut x = 0.0_f32;
    let mut y = 0.0_f32;

    let flush = |line: &mut Vec<Piece>, y: &mut f32, x: &mut f32, runs: &mut Vec<LaidRun>, line_height: f32| {
        if line.is_empty() {
            *y += line_height;
            *x = 0.0;
            return;
        }
        let mut min_off = 0.0_f32;
        let mut bottom = line_height;
        for piece in line.iter() {
            min_off = min_off.min(piece.y_off);
            bottom = bottom.max(piece.y_off + piece.h);
        }
        let shift = -min_off.min(0.0);
        for piece in line.drain(..) {
            let top = *y + piece.y_off + shift;
            if let Some(text) = piece.prose {
                if text.is_empty() {
                    continue;
                }
                runs.push(LaidRun::Prose {
                    text,
                    x: piece.x,
                    y: top,
                    w: piece.w,
                    h: piece.h,
                });
            } else {
                runs.push(LaidRun::Math {
                    inner: piece.inner,
                    display: piece.display,
                    x: piece.x,
                    y: top,
                    w: piece.w,
                    h: piece.h,
                    baseline_pt: piece.baseline_pt,
                });
            }
        }
        *y += bottom + shift;
        *x = 0.0;
    };

    for span in math_spans::parse_spans(content) {
        match span {
            Span::Prose { start, end } => {
                let raw = math_spans::display_prose(&content[start..end]);
                for token in split_tokens(&raw) {
                    if token == "\n" {
                        flush(&mut line, &mut y, &mut x, &mut runs, line_height);
                        continue;
                    }
                    let w = measure.width(&token, size_pt);
                    if x > 0.0 && x + w > width_pt {
                        flush(&mut line, &mut y, &mut x, &mut runs, line_height);
                    }
                    line.push(Piece {
                        prose: Some(token),
                        inner: String::new(),
                        display: false,
                        x,
                        y_off: 0.0,
                        w,
                        h: line_height,
                        baseline_pt: None,
                    });
                    x += w;
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
                let (w, h, baseline) = math_box(&inner, display, size_pt, width_pt, preview);
                if display {
                    if x > 0.0 || !line.is_empty() {
                        flush(&mut line, &mut y, &mut x, &mut runs, line_height);
                    }
                    let x_off = ((width_pt - w) * 0.5).max(0.0);
                    line.push(Piece {
                        prose: None,
                        inner,
                        display: true,
                        x: x_off,
                        y_off: math_y_off(h, baseline, line_height, ascent),
                        w,
                        h,
                        baseline_pt: baseline,
                    });
                    flush(&mut line, &mut y, &mut x, &mut runs, line_height);
                } else {
                    if x > 0.0 && x + w > width_pt {
                        flush(&mut line, &mut y, &mut x, &mut runs, line_height);
                    }
                    line.push(Piece {
                        prose: None,
                        inner,
                        display: false,
                        x,
                        y_off: math_y_off(h, baseline, line_height, ascent),
                        w,
                        h,
                        baseline_pt: baseline,
                    });
                    x += w;
                }
            }
        }
    }
    if !line.is_empty() {
        flush(&mut line, &mut y, &mut x, &mut runs, line_height);
    }
    runs
}

fn math_y_off(h: f32, baseline: Option<f32>, line_height: f32, ascent: f32) -> f32 {
    match baseline {
        Some(baseline) => ascent - baseline,
        None => (line_height - h) * 0.5,
    }
}

fn math_box(
    inner: &str,
    display: bool,
    size_pt: f32,
    width_pt: f32,
    preview: &dyn Fn(&str, bool) -> Option<MathMetrics>,
) -> (f32, f32, Option<f32>) {
    let metrics = preview(inner, display).unwrap_or_else(|| {
        let w = (inner.chars().count() as f32 * size_pt * 0.45).max(size_pt);
        let h = if display {
            size_pt * 1.8
        } else {
            size_pt * 1.2
        };
        MathMetrics {
            w_pt: w,
            h_pt: h,
            baseline_pt: None,
        }
    });
    let mut w = metrics.w_pt.max(1.0);
    let mut h = metrics.h_pt.max(1.0);
    let mut baseline = metrics.baseline_pt;
    if w > width_pt {
        let scale = width_pt / w;
        w = width_pt;
        h *= scale;
        if let Some(value) = baseline.as_mut() {
            *value *= scale;
        }
    }
    (w, h, baseline)
}

fn split_tokens(text: &str) -> Vec<String> {
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

/// Conceal galley. One glyph per source character; concealed spans are
/// invisible placeholders whose letter-spacing reserves the equation width.
mod galley {
    use egui::text::{CCursor, LayoutJob, LayoutSection, TextFormat, TextWrapping};
    use egui::{Align, Color32, Event, FontId, Galley, Id, ImeEvent, Pos2, Rect, TextBuffer, Ui, Vec2};

    use crate::math_spans::{self, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CharSpan {
    pub start: usize,
    pub end: usize,
    pub inner_start: usize,
    pub inner_end: usize,
    pub display: bool,
    pub closed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Render {
    Ready(Vec2),
    Pending,
    Error(String),
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
    pub span: CharSpan,
    pub inner: String,
    pub size: Vec2,
}

pub struct Built {
    pub job: LayoutJob,
    pub concealed: Vec<Concealed>,
    pub revealed: Vec<CharSpan>,
    /// Invalid spans that stayed raw. The editor paints a dotted underline.
    pub errors: Vec<(CharSpan, String)>,
}

const TINY: f32 = 0.01;

/// R1–R2. A closed span is revealed when the caret, or either selection
/// endpoint, is strictly inside it. A boundary caret stays rendered. A span
/// that sits entirely inside a selection stays rendered.
pub fn is_revealed(span: &CharSpan, sel: Option<(usize, usize)>) -> bool {
    if !span.closed {
        return true;
    }
    let Some((a, b)) = sel else {
        return false;
    };
    let inside = |index: usize| index > span.start && index < span.end;
    inside(a) || inside(b)
}

fn char_spans(text: &str) -> Vec<CharSpan> {
    let mut out = Vec::new();
    for span in math_spans::parse_spans(text) {
        let Span::Math {
            start,
            end,
            inner_start,
            inner_end,
            display,
            closed,
        } = span
        else {
            continue;
        };
        let (start, end) = math_spans::byte_range_char_indices(text, start, end);
        let (inner_start, inner_end) = math_spans::byte_range_char_indices(text, inner_start, inner_end);
        out.push(CharSpan {
            start,
            end,
            inner_start,
            inner_end,
            display,
            closed,
        });
    }
    out
}

/// Conceal-aware layout job. `sel` is the caret or selection in characters.
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
    let mut errors = Vec::new();
    let mut at = 0usize;
    for span in char_spans(text) {
        let before: String = chars[at..span.start].iter().collect();
        push(&mut job, &before, prose.clone());
        at = span.end;
        let raw: String = chars[span.start..span.end].iter().collect();
        let inner: String = chars[span.inner_start..span.inner_end].iter().collect();
        let state = if inner.trim().is_empty() {
            Render::Pending
        } else {
            render(&inner, span.display)
        };
        let reveal = is_revealed(&span, sel) || !matches!(state, Render::Ready(_));
        if reveal {
            revealed.push(span);
            if let Render::Error(message) = &state {
                errors.push((span, message.clone()));
            }
            let format = TextFormat {
                color: style.source_color,
                background: style.source_bg,
                ..prose.clone()
            };
            push(&mut job, &raw, format);
            continue;
        }
        let Render::Ready(size) = state else {
            continue;
        };
        let open = span.inner_start - span.start;
        let body_len = span.end - span.inner_start;
        if body_len == 0 {
            continue;
        }
        // The row reserves `row_w`. Display math takes the whole row so the
        // following prose wraps; the image itself stays `draw` and is centered.
        // An equation wider than the box scales down, keeping its aspect.
        let max_w = (wrap_width - 1.0).max(1.0);
        let draw = fitted_math(size, max_w);
        let row_w = if span.display { max_w } else { draw.x };
        let hidden = TextFormat {
            font_id: FontId::new(TINY, style.font.family.clone()),
            color: Color32::TRANSPARENT,
            line_height: Some(draw.y),
            valign: Align::Center,
            ..Default::default()
        };
        push(&mut job, &" ".repeat(open), hidden.clone());
        let mut body: String = "x".repeat(body_len.saturating_sub(1));
        body.push(if span.display { ' ' } else { 'x' });
        let spacing = if body_len > 1 {
            row_w / (body_len - 1) as f32
        } else {
            0.0
        };
        push(
            &mut job,
            &body,
            TextFormat {
                extra_letter_spacing: spacing,
                ..hidden
            },
        );
        concealed.push(Concealed {
            span,
            inner,
            size: draw,
        });
    }
    let rest: String = chars[at..].iter().collect();
    push(&mut job, &rest, prose);
    Built {
        job,
        concealed,
        revealed,
        errors,
    }
}

fn fitted_math(size: Vec2, max_w: f32) -> Vec2 {
    if size.x <= max_w || size.x <= f32::EPSILON {
        return Vec2::new(size.x.max(1.0), size.y.max(1.0));
    }
    let scale = max_w / size.x;
    Vec2::new(max_w, (size.y * scale).max(1.0))
}

/// Galley-space rect of a concealed equation.
pub fn math_rect(galley: &Galley, concealed: &Concealed) -> Rect {
    let next_row = |index| CCursor {
        index,
        prefer_next_row: true,
    };
    let left = galley.pos_from_cursor(next_row(concealed.span.inner_start));
    let right = galley.pos_from_cursor(next_row(concealed.span.end.saturating_sub(1)));
    let center_y = left.center().y;
    let x0 = if concealed.span.display {
        left.min.x + ((right.min.x - left.min.x) - concealed.size.x).max(0.0) * 0.5
    } else {
        left.min.x
    };
    Rect::from_min_size(Pos2::new(x0, center_y - concealed.size.y * 0.5), concealed.size)
}

pub struct EditorOutput {
    pub response: egui::Response,
    pub galley: std::sync::Arc<Galley>,
    pub galley_pos: Pos2,
    /// Screen rects of concealed equations.
    pub math: Vec<(Rect, String, bool, CharSpan)>,
    /// Spans shown as source. The editor applies this before paint; tests read it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub revealed: Vec<CharSpan>,
    pub errors: Vec<(CharSpan, String)>,
    pub caret: Option<(usize, usize)>,
    /// True when this pass laid out a stale caret and asked for another.
    #[cfg_attr(not(test), allow(dead_code))]
    pub discarded: bool,
}

fn revealed_for(text: &str, sel: Option<(usize, usize)>, render: &dyn Fn(&str, bool) -> Render) -> Vec<CharSpan> {
    let chars: Vec<char> = text.chars().collect();
    char_spans(text)
        .into_iter()
        .filter(|span| {
            let inner: String = chars[span.inner_start..span.inner_end].iter().collect();
            let state = if inner.trim().is_empty() {
                Render::Pending
            } else {
                render(&inner, span.display)
            };
            is_revealed(span, sel) || !matches!(state, Render::Ready(_))
        })
        .collect()
}

fn ime_preedit(ui: &Ui) -> bool {
    ui.input(|input| {
        input.events.iter().any(|event| {
            matches!(
                event,
                Event::Ime(ImeEvent::Preedit(_)) | Event::Ime(ImeEvent::Enabled)
            )
        })
    })
}

/// One `TextEdit` over the raw source. The layouter reads the previous caret;
/// a caret-only move discards the pass so the reveal updates before paint.
/// A frame that is mid IME preedit is not discarded.
pub fn conceal_editor(
    ui: &mut Ui,
    id: Id,
    text: &mut String,
    width: f32,
    style: &Style,
    render: &dyn Fn(&str, bool) -> Render,
) -> EditorOutput {
    let sel = egui::widgets::text_edit::TextEditState::load(ui.ctx(), id)
        .and_then(|state| state.cursor.char_range())
        .map(|range| (range.primary.index, range.secondary.index));
    let mut last: Option<Built> = None;
    let mut layouter = |ui: &Ui, buf: &dyn TextBuffer, wrap: f32| {
        let built = build_job(buf.as_str(), sel, wrap, style, render);
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(built.job.clone()));
        last = Some(built);
        galley
    };
    let output = egui::TextEdit::multiline(text)
        .id(id)
        .font(style.font.clone())
        .text_color(style.color)
        .desired_width(width)
        .desired_rows(1)
        .frame(false)
        .margin(egui::Margin::ZERO)
        .layouter(&mut layouter)
        .show(ui);
    let built = last.unwrap_or(Built {
        job: LayoutJob::default(),
        concealed: Vec::new(),
        revealed: Vec::new(),
        errors: Vec::new(),
    });
    let caret = output
        .cursor_range
        .map(|range| (range.primary.index, range.secondary.index));
    let want = revealed_for(text, caret, render);
    let discarded = want != built.revealed && !ime_preedit(ui);
    if discarded {
        ui.ctx().request_discard("conceal reveal set changed");
    }
    let math = built
        .concealed
        .iter()
        .map(|concealed| {
            (
                math_rect(&output.galley, concealed).translate(output.galley_pos.to_vec2()),
                concealed.inner.clone(),
                concealed.span.display,
                concealed.span,
            )
        })
        .collect();
    EditorOutput {
        response: output.response,
        galley: output.galley,
        galley_pos: output.galley_pos,
        math,
        revealed: want,
        errors: built.errors,
        caret,
        discarded,
    }
}
}

pub use galley::{conceal_editor, CharSpan, EditorOutput, Render, Style};

#[cfg(test)]
pub use galley::{build_job, math_rect, Built};

/// How long typing must pause before an invalid equation shows its error.
pub const MATH_ERROR_IDLE_SECS: f64 = 0.4;

/// What the preview bubble draws for the span under the caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BubbleImage {
    /// The current source has a ready render.
    Current,
    /// The current source is pending or invalid; keep the previous good render.
    LastGoodDimmed,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bubble {
    pub image: BubbleImage,
    pub error: Option<String>,
}

/// R4–R5. A ready span shows its render. While the source is in flux, the last
/// good render stays, dimmed, and the error line waits until typing pauses.
pub fn preview_bubble(state: &Render, has_last_good: bool, idle_secs: f64) -> Bubble {
    let paused = idle_secs >= MATH_ERROR_IDLE_SECS;
    match state {
        Render::Ready(_) => Bubble {
            image: BubbleImage::Current,
            error: None,
        },
        Render::Pending => Bubble {
            image: if has_last_good {
                BubbleImage::LastGoodDimmed
            } else {
                BubbleImage::None
            },
            error: None,
        },
        Render::Error(message) => Bubble {
            image: if has_last_good {
                BubbleImage::LastGoodDimmed
            } else {
                BubbleImage::None
            },
            error: paused.then(|| latex_error_message(message)),
        },
    }
}

/// Typst says `unknown variable: mitexsqrt`. Show a LaTeX command until LT6
/// maps the full rewrite table.
pub fn latex_error_message(raw: &str) -> String {
    let trimmed = raw.trim();
    let Some(name) = trimmed.strip_prefix("unknown variable:") else {
        return trimmed.to_string();
    };
    let name = name
        .trim()
        .trim_matches(|ch: char| ch == '`' || ch == '"' || ch == '\'');
    let command = match name {
        "mitexsqrt" => "sqrt",
        "mitexmathbf" => "mathbf",
        "mitexdisplaystyle" | "mitexdisplay" => "displaystyle",
        "mitexoverbrace" => "overbrace",
        "mitexunderbrace" => "underbrace",
        other => other.strip_prefix("mitex").unwrap_or(other),
    };
    if command.is_empty() {
        return trimmed.to_string();
    }
    format!("unknown command \\{command}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Fixed {
        em: f32,
    }

    impl TextMeasure for Fixed {
        fn width(&self, text: &str, size_pt: f32) -> f32 {
            text.chars().count() as f32 * size_pt * self.em
        }

        fn line_height(&self, size_pt: f32) -> f32 {
            size_pt * 1.25
        }

        fn ascent(&self, size_pt: f32) -> f32 {
            size_pt * 0.8
        }
    }

    fn metrics(w: f32, h: f32, baseline: Option<f32>) -> impl Fn(&str, bool) -> Option<MathMetrics> {
        move |_, _| {
            Some(MathMetrics {
                w_pt: w,
                h_pt: h,
                baseline_pt: baseline,
            })
        }
    }

    fn math_at(runs: &[LaidRun]) -> Vec<(f32, f32, f32, f32)> {
        runs.iter()
            .filter_map(|run| match run {
                LaidRun::Math { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
                LaidRun::Prose { .. } => None,
            })
            .collect()
    }

    #[test]
    fn paint_and_save_rects_match_in_page_points() {
        // 5pt is below both old clamps (paint 8px, save 6pt). One layout
        // uses the real size, so the two callers cannot diverge.
        let content = "energy $x^2$ grows";
        let size_pt = 5.0;
        let width_pt = 200.0;
        let measure = Fixed { em: 0.5 };
        let preview = metrics(18.0, 10.0, None);
        let paint = layout_rich_text(content, size_pt, width_pt, &measure, &preview);
        let save = layout_rich_text(content, size_pt, width_pt, &measure, &preview);
        assert_eq!(math_at(&paint), math_at(&save));
        let LaidRun::Prose { w, .. } = &paint[0] else {
            panic!("expected leading prose");
        };
        // "energy " is 7 chars × 5pt × 0.5. A 6pt clamp would be 21.
        assert!((w - 17.5).abs() < 0.01, "prose width {w}");
        let LaidRun::Math { x, .. } = &paint[1] else {
            panic!("expected math");
        };
        assert!((x - w).abs() < 0.01, "math x {x}");
    }

    #[test]
    fn wraps_at_the_opener_when_math_does_not_fit() {
        let measure = Fixed { em: 0.5 };
        let runs = layout_rich_text(
            "aaaaaaaaaa$x$",
            10.0,
            60.0,
            &measure,
            &metrics(40.0, 12.0, None),
        );
        let LaidRun::Math { x, y, .. } = runs.last().unwrap() else {
            panic!("expected math");
        };
        assert!(*y > 0.0, "math should wrap onto the next row, y={y}");
        assert!(*x < 1.0, "wrapped math starts at the opener, x={x}");
    }

    #[test]
    fn display_math_takes_its_own_row() {
        let measure = Fixed { em: 0.5 };
        let runs = layout_rich_text(
            "a $$y$$ b",
            10.0,
            200.0,
            &measure,
            &metrics(30.0, 20.0, None),
        );
        let mut ys = Vec::new();
        let mut math_x = 0.0;
        for run in &runs {
            match run {
                LaidRun::Prose { y, .. } => ys.push(*y),
                LaidRun::Math { x, y, display, .. } => {
                    assert!(*display);
                    ys.push(*y);
                    math_x = *x;
                }
            }
        }
        assert!(ys.len() >= 3, "{ys:?}");
        assert!(ys[0] < ys[1] && ys[1] < ys[2], "{ys:?}");
        assert!((math_x - (200.0 - 30.0) * 0.5).abs() < 0.5, "centered {math_x}");
    }

    #[test]
    fn missing_baseline_centers_math_on_the_line() {
        let measure = Fixed { em: 0.5 };
        let runs = layout_rich_text("a $x$", 10.0, 200.0, &measure, &metrics(20.0, 10.0, None));
        let LaidRun::Math { y, .. } = runs.iter().find(|run| matches!(run, LaidRun::Math { .. })).unwrap() else {
            unreachable!();
        };
        // line height 12.5, math height 10, no baseline → (12.5 - 10) / 2
        assert!((*y - 1.25).abs() < 0.05, "centered y={y}");
    }

    #[test]
    fn baseline_aligns_math_to_the_text_baseline() {
        let measure = Fixed { em: 0.5 };
        let runs = layout_rich_text(
            "a $x$",
            10.0,
            200.0,
            &measure,
            &metrics(20.0, 10.0, Some(6.0)),
        );
        let LaidRun::Math { y, baseline_pt, .. } =
            runs.iter().find(|run| matches!(run, LaidRun::Math { .. })).unwrap()
        else {
            unreachable!();
        };
        // ascent is 8pt; image top sits at ascent - baseline.
        assert_eq!(*baseline_pt, Some(6.0));
        assert!((*y - 2.0).abs() < 0.05, "aligned y={y}");
    }

    fn style() -> Style {
        Style {
            font: FontId::new(16.0, egui::FontFamily::Proportional),
            color: Color32::WHITE,
            source_color: Color32::LIGHT_BLUE,
            source_bg: Color32::TRANSPARENT,
            error: Color32::RED,
        }
    }

    fn render_ready(inner: &str, _display: bool) -> Render {
        if inner.contains("bad") {
            Render::Error("bad".into())
        } else {
            Render::Ready(Vec2::new(40.0, 30.0))
        }
    }

    fn layout_galley(text: &str, sel: Option<(usize, usize)>, wrap: f32) -> (Arc<Galley>, Built) {
        let ctx = egui::Context::default();
        let mut out = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let built = build_job(text, sel, wrap, &style(), &render_ready);
            let galley = ctx.fonts_mut(|fonts| fonts.layout_job(built.job.clone()));
            out = Some((galley, built));
        });
        out.expect("galley")
    }

    fn x_at(galley: &Galley, index: usize) -> Pos2 {
        galley
            .pos_from_cursor(CCursor {
                index,
                prefer_next_row: true,
            })
            .min
    }

    #[test]
    fn galley_keeps_one_glyph_per_source_char() {
        let text = "see $\\frac{a}{b}$ and $$x^2$$ ok ünï";
        let (galley, built) = layout_galley(text, None, 400.0);
        assert_eq!(built.concealed.len(), 2);
        assert_eq!(built.concealed[0].inner, "\\frac{a}{b}");
        assert!(built.revealed.is_empty());
        assert_eq!(galley.end().index, text.chars().count());
    }

    #[test]
    fn concealed_span_reserves_rendered_width() {
        let text = "ab $x+y$ cd";
        let (galley, built) = layout_galley(text, None, 400.0);
        let concealed = &built.concealed[0];
        let rect = math_rect(&galley, concealed);
        assert!((rect.width() - 40.0).abs() < 0.5, "{rect:?}");
        let gap = x_at(&galley, concealed.span.end).x - x_at(&galley, concealed.span.inner_start).x;
        assert!((gap - 40.0).abs() < 0.5, "reserved {gap}");
        let after = x_at(&galley, concealed.span.end + 1).x;
        assert!(after > rect.max.x && after < rect.max.x + 8.0, "prose {after}");
    }

    #[test]
    fn foo_dollar_wraps_at_the_opener() {
        let (galley, built) = layout_galley("aaaaaaaaaa$x$", None, 110.0);
        let concealed = &built.concealed[0];
        let prose = x_at(&galley, 9);
        let math = x_at(&galley, concealed.span.inner_start);
        assert!(
            prose.x + 9.0 < 110.0 && prose.x + 9.0 > 110.0 - 40.0,
            "prose {prose:?}"
        );
        assert!(math.y > prose.y && math.x < 1.0, "math {math:?}");
    }

    #[test]
    fn display_math_galley_is_its_own_row() {
        let text = "a $$y$$ b";
        let (galley, built) = layout_galley(text, None, 200.0);
        let concealed = &built.concealed[0];
        let y_a = x_at(&galley, 0).y;
        let y_m = x_at(&galley, concealed.span.inner_start).y;
        let y_b = x_at(&galley, text.chars().count() - 1).y;
        assert!(y_a < y_m && y_m < y_b, "{y_a} {y_m} {y_b}");
        let rect = math_rect(&galley, concealed);
        assert!(
            (rect.center().x - 100.0).abs() < 2.0,
            "centered: {rect:?}"
        );
    }

    #[test]
    fn click_on_math_maps_into_span_proportionally() {
        let text = "ab $abcdefgh$ cd";
        let (galley, built) = layout_galley(text, None, 400.0);
        let concealed = &built.concealed[0];
        let rect = math_rect(&galley, concealed);
        let mid = galley.cursor_from_pos(rect.center().to_vec2()).index;
        assert!(mid > concealed.span.start && mid < concealed.span.end, "mid {mid}");
        let left = galley
            .cursor_from_pos(Vec2::new(rect.min.x + 1.0, rect.center().y))
            .index;
        let right = galley
            .cursor_from_pos(Vec2::new(rect.max.x - 1.0, rect.center().y))
            .index;
        assert!(left <= concealed.span.inner_start + 1, "left {left}");
        assert!(right >= concealed.span.end - 2, "right {right}");
    }

    #[test]
    fn math_never_splits_across_rows() {
        let text = "aaaa bbbb cccc $x + y + z + w$ dd";
        let (galley, built) = layout_galley(text, None, 150.0);
        let concealed = &built.concealed[0];
        let rows: Vec<f32> = (concealed.span.inner_start..concealed.span.end)
            .map(|index| x_at(&galley, index).y)
            .collect();
        assert!(rows.windows(2).all(|pair| pair[0] == pair[1]), "{rows:?}");
    }

    #[test]
    fn row_grows_to_fit_tall_math() {
        let (galley, built) = layout_galley("ab $x$ cd", None, 400.0);
        assert!(galley.rows[0].rect().height() >= 30.0);
        let rect = math_rect(&galley, &built.concealed[0]);
        assert!(galley.rows[0].rect().contains_rect(rect.shrink(0.5)), "{rect:?}");
    }

    #[test]
    fn caret_inside_reveals_boundary_stays_rendered() {
        let text = "ab $x$ and $bad$";
        let (_, built) = layout_galley(text, Some((4, 4)), 400.0);
        let starts: Vec<usize> = built.revealed.iter().map(|span| span.start).collect();
        assert_eq!(starts, vec![3, 11]);
        assert!(built.concealed.is_empty());
        for caret in [3usize, 6] {
            let (_, built) = layout_galley(text, Some((caret, caret)), 400.0);
            assert_eq!(built.concealed.len(), 1, "caret {caret}");
        }
        // A span lying entirely inside the selection stays rendered.
        let covered = "ab $x$ cd";
        let (_, built) = layout_galley(covered, Some((0, 9)), 400.0);
        assert_eq!(built.concealed.len(), 1);
        assert!(built.revealed.is_empty());
        // An endpoint strictly inside still reveals.
        let (_, built) = layout_galley(covered, Some((4, 9)), 400.0);
        assert_eq!(built.revealed.len(), 1);
    }

    #[test]
    fn wide_equation_scales_down_keeping_aspect() {
        let (_, built) = layout_galley("ab $x$ cd", None, 21.0);
        let size = built.concealed[0].size;
        assert!((size.x - 20.0).abs() < 0.5, "{size:?}");
        assert!((size.y - 15.0).abs() < 0.5, "{size:?}");
    }

    #[test]
    fn error_idle_keeps_last_good_then_shows_latex_words() {
        let pending = preview_bubble(&Render::Pending, true, 0.0);
        assert_eq!(pending.image, BubbleImage::LastGoodDimmed);
        assert!(pending.error.is_none());
        let typing = preview_bubble(&Render::Error("unknown variable: mitexsqrt".into()), true, 0.1);
        assert_eq!(typing.image, BubbleImage::LastGoodDimmed);
        assert!(typing.error.is_none());
        let paused = preview_bubble(
            &Render::Error("unknown variable: mitexsqrt".into()),
            true,
            MATH_ERROR_IDLE_SECS,
        );
        assert_eq!(paused.error.as_deref(), Some("unknown command \\sqrt"));
        let fresh = preview_bubble(&Render::Error("unknown variable: foo".into()), false, 0.0);
        assert_eq!(fresh.image, BubbleImage::None);
        assert!(fresh.error.is_none());
        let ready = preview_bubble(&Render::Ready(Vec2::new(10.0, 10.0)), true, 0.0);
        assert_eq!(ready.image, BubbleImage::Current);
        assert_eq!(
            latex_error_message("unbalanced braces"),
            "unbalanced braces"
        );
    }

    struct Harness {
        ctx: egui::Context,
        text: String,
        id: egui::Id,
        last_math: Vec<egui::Rect>,
    }

    struct Pass {
        revealed: Vec<CharSpan>,
        caret: Option<(usize, usize)>,
        discarded: bool,
    }

    impl Harness {
        fn new(text: &str, caret: usize) -> Self {
            let ctx = egui::Context::default();
            let id = egui::Id::new("edit");
            let mut state = egui::widgets::text_edit::TextEditState::default();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(CCursor::new(caret))));
            state.store(&ctx, id);
            let mut harness = Self {
                ctx,
                text: text.into(),
                id,
                last_math: Vec::new(),
            };
            harness.frame(Vec::new(), true);
            harness
        }

        fn frame(&mut self, events: Vec<egui::Event>, focus: bool) -> Vec<Pass> {
            let input = egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(800.0, 600.0),
                )),
                ..Default::default()
            };
            let mut passes = Vec::new();
            let mut math = Vec::new();
            let (text, id) = (&mut self.text, self.id);
            let _ = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let out = conceal_editor(ui, id, text, 400.0, &style(), &render_ready);
                    if focus {
                        out.response.request_focus();
                    }
                    math = out.math.iter().map(|(rect, _, _, _)| *rect).collect();
                    passes.push(Pass {
                        revealed: out.revealed,
                        caret: out.caret,
                        discarded: out.discarded,
                    });
                });
            });
            self.last_math = math;
            passes
        }

        fn key(&mut self, key: egui::Key) -> Vec<Pass> {
            self.frame(
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                false,
            )
        }
    }

    #[test]
    fn arrow_into_span_reveals_in_the_same_frame() {
        let mut harness = Harness::new("ab $x$ cd", 6);
        assert_eq!(harness.last_math.len(), 1);
        let passes = harness.key(egui::Key::ArrowLeft);
        let last = passes.last().unwrap();
        assert_eq!(last.caret, Some((5, 5)));
        assert_eq!(last.revealed.len(), 1, "{:?}", last.revealed.len());
        assert!(!last.discarded);
        assert_eq!(passes.len(), 2, "one discarded pass fixes the stale layout");
        assert!(harness.last_math.is_empty());
        let passes = harness.key(egui::Key::ArrowRight);
        let last = passes.last().unwrap();
        assert_eq!(last.caret, Some((6, 6)));
        assert!(last.revealed.is_empty() && !last.discarded);
        assert_eq!(harness.last_math.len(), 1);
    }

    #[test]
    fn typing_the_closing_dollar_renders_immediately() {
        let mut harness = Harness::new("ab $x cd", 5);
        assert!(harness.last_math.is_empty());
        let passes = harness.frame(vec![egui::Event::Text("$".into())], false);
        assert_eq!(harness.text, "ab $x$ cd");
        let last = passes.last().unwrap();
        assert_eq!(last.caret, Some((6, 6)));
        assert!(last.revealed.is_empty() && !last.discarded);
        assert_eq!(harness.last_math.len(), 1);
    }

    #[test]
    fn clicking_rendered_math_enters_it() {
        let mut harness = Harness::new("ab $abcdefgh$ cd", 0);
        let target = harness.last_math[0].center();
        harness.frame(vec![egui::Event::PointerMoved(target)], false);
        let press = |pressed| egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        harness.frame(vec![press(true)], false);
        let passes = harness.frame(vec![press(false)], false);
        let last = passes.last().unwrap();
        let (caret, _) = last.caret.unwrap();
        assert!(caret > 3 && caret < 13, "caret {caret}");
        assert_eq!(last.revealed.len(), 1);
    }

    #[test]
    fn vertical_motion_across_rendered_math_rows() {
        let mut harness = Harness::new("ab $abcdefgh$ cd\nsecond line here", 20);
        let passes = harness.key(egui::Key::ArrowUp);
        let (caret, _) = passes.last().unwrap().caret.unwrap();
        assert!(caret <= 16, "caret {caret} on first row");
    }
}
