//! One rich-text layout in page points, shared by view-mode paint, save, and
//! the conceal editor.
//!
//! The egui conceal galley lives in the binary (`rich_text_edit`); this module
//! is the headless layout used by paint, save, and tests.

use crate::math_spans::{self, Span};

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

    let flush = |line: &mut Vec<Piece>,
                 y: &mut f32,
                 x: &mut f32,
                 runs: &mut Vec<LaidRun>,
                 line_height: f32| {
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

/// How long typing must pause before an invalid equation shows its error.
pub const MATH_ERROR_IDLE_SECS: f64 = 0.4;

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

    fn metrics(
        w: f32,
        h: f32,
        baseline: Option<f32>,
    ) -> impl Fn(&str, bool) -> Option<MathMetrics> {
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
        assert!(
            (math_x - (200.0 - 30.0) * 0.5).abs() < 0.5,
            "centered {math_x}"
        );
    }

    #[test]
    fn missing_baseline_centers_math_on_the_line() {
        let measure = Fixed { em: 0.5 };
        let runs = layout_rich_text("a $x$", 10.0, 200.0, &measure, &metrics(20.0, 10.0, None));
        let LaidRun::Math { y, .. } = runs
            .iter()
            .find(|run| matches!(run, LaidRun::Math { .. }))
            .unwrap()
        else {
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
        let LaidRun::Math { y, baseline_pt, .. } = runs
            .iter()
            .find(|run| matches!(run, LaidRun::Math { .. }))
            .unwrap()
        else {
            unreachable!();
        };
        // ascent is 8pt; image top sits at ascent - baseline.
        assert_eq!(*baseline_pt, Some(6.0));
        assert!((*y - 2.0).abs() < 0.05, "aligned y={y}");
    }
}
