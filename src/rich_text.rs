//! One rich-text layout in page points, shared by view-mode paint and save.
//!
//! `build_job` / `math_rect` are the conceal galley (no editor yet). They keep
//! one glyph per source character so a later caret can land inside an equation.

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

/// Conceal galley (no editor yet). Dead in the app binary until LT3 paints it;
/// unit tests cover width, wrapping, and one glyph per source character.
#[cfg_attr(not(test), allow(dead_code))]
mod galley {
    use egui::text::{CCursor, LayoutJob, LayoutSection, TextFormat, TextWrapping};
    use egui::{Align, Color32, FontId, Galley, Pos2, Rect, Stroke, Vec2};

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
}

const TINY: f32 = 0.01;

/// Reveal when the caret is strictly inside the span, or a selection overlaps it.
pub fn is_revealed(span: &CharSpan, sel: Option<(usize, usize)>) -> bool {
    if !span.closed {
        return true;
    }
    let Some((a, b)) = sel else {
        return false;
    };
    let (lo, hi) = (a.min(b), a.max(b));
    lo < span.end && hi > span.start
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
        let Render::Ready(size) = state else {
            continue;
        };
        let open = span.inner_start - span.start;
        let body_len = span.end - span.inner_start;
        if body_len == 0 {
            continue;
        }
        let width = if span.display {
            (wrap_width - 1.0).max(1.0)
        } else {
            size.x.min((wrap_width - 1.0).max(1.0))
        };
        let hidden = TextFormat {
            font_id: FontId::new(TINY, style.font.family.clone()),
            color: Color32::TRANSPARENT,
            line_height: Some(size.y),
            valign: Align::Center,
            ..Default::default()
        };
        push(&mut job, &" ".repeat(open), hidden.clone());
        let mut body: String = "x".repeat(body_len.saturating_sub(1));
        body.push(if span.display { ' ' } else { 'x' });
        let spacing = if body_len > 1 {
            width / (body_len - 1) as f32
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
}

#[cfg(test)]
pub use galley::{build_job, math_rect, Built, Render, Style};

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
        let text = "see $x$ and $$y$$ ok";
        let (galley, built) = layout_galley(text, None, 400.0);
        assert_eq!(built.concealed.len(), 2);
        assert_eq!(built.concealed[0].inner, "x");
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
}
