use std::sync::Arc;

use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditState;
use egui::{
    CentralPanel, Color32, Context, Event, FontFamily, FontId, Galley, Id, Key, Modifiers,
    PointerButton, Pos2, RawInput, Rect, Vec2,
};
use latex_conceal_spike::{build_job, conceal_editor, math_rect, Built, Render, Span, Style};

const MATH: Vec2 = Vec2::new(40.0, 30.0);

fn style() -> Style {
    Style {
        font: FontId::new(16.0, FontFamily::Proportional),
        color: Color32::WHITE,
        source_color: Color32::LIGHT_BLUE,
        source_bg: Color32::TRANSPARENT,
        error: Color32::RED,
    }
}

fn render(inner: &str, _display: bool) -> Render {
    if inner.contains("bad") {
        Render::Error("bad".into())
    } else {
        Render::Ready(MATH)
    }
}

fn layout(text: &str, sel: Option<(usize, usize)>, wrap: f32) -> (Arc<Galley>, Built) {
    let ctx = Context::default();
    let mut out = None;
    let _ = ctx.run(RawInput::default(), |ctx| {
        let built = build_job(text, sel, wrap, &style(), &render);
        let galley = ctx.fonts_mut(|f| f.layout_job(built.job.clone()));
        out = Some((galley, built));
    });
    out.unwrap()
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
    let text = r"see $\frac{a}{b}$ and $$x^2$$ ok ünï";
    let (galley, built) = layout(text, None, 300.0);
    assert_eq!(built.concealed.len(), 2);
    assert_eq!(galley.end().index, text.chars().count());
}

#[test]
fn concealed_span_reserves_rendered_width() {
    let text = "ab $x+y$ cd";
    let (galley, built) = layout(text, None, 300.0);
    let c = &built.concealed[0];
    let rect = math_rect(&galley, c);
    assert!((rect.width() - MATH.x).abs() < 0.5, "{rect:?}");
    let gap = x_at(&galley, c.span.end).x - x_at(&galley, c.span.inner_start).x;
    assert!((gap - MATH.x).abs() < 0.5, "reserved {gap}");
    // Prose after the span starts right where the image ends.
    let after = x_at(&galley, c.span.end + 1).x;
    assert!(after > rect.max.x && after < rect.max.x + 8.0);
}

#[test]
fn click_on_math_maps_into_span_proportionally() {
    let text = "ab $abcdefgh$ cd";
    let (galley, built) = layout(text, None, 300.0);
    let c = &built.concealed[0];
    let rect = math_rect(&galley, c);
    let mid = galley.cursor_from_pos(rect.center().to_vec2()).index;
    assert!(mid > c.span.start && mid < c.span.end, "mid click -> {mid}");
    let left = galley.cursor_from_pos(Vec2::new(rect.min.x + 1.0, rect.center().y)).index;
    let right = galley.cursor_from_pos(Vec2::new(rect.max.x - 1.0, rect.center().y)).index;
    assert!(left <= c.span.inner_start + 1, "left click -> {left}");
    assert!(right >= c.span.end - 2, "right click -> {right}");
}

#[test]
fn math_never_splits_across_rows() {
    // Spaces inside the source must not become wrap points.
    let text = "aaaa bbbb cccc $x + y + z + w$ dd";
    let (galley, built) = layout(text, None, 150.0);
    let c = &built.concealed[0];
    let rows: Vec<f32> = (c.span.inner_start..c.span.end)
        .map(|i| x_at(&galley, i).y)
        .collect();
    assert!(rows.windows(2).all(|w| w[0] == w[1]), "{rows:?}");
    // And with no space before the opener, the wrap still happens at the opener.
    let (galley, built) = layout("aaaaaaaaaa$x$", None, 110.0);
    let c = &built.concealed[0];
    let (first, math) = (x_at(&galley, 9), x_at(&galley, c.span.inner_start));
    assert!(first.x + 9.0 < 110.0 && first.x + 9.0 > 110.0 - MATH.x, "prose {first:?}");
    assert!(math.y > first.y && math.x < 1.0, "math {math:?}");
}

#[test]
fn row_grows_to_fit_tall_math() {
    let (galley, built) = layout("ab $x$ cd", None, 300.0);
    assert!(galley.rows[0].rect().height() >= MATH.y);
    let rect = math_rect(&galley, &built.concealed[0]);
    assert!(galley.rows[0].rect().contains_rect(rect.shrink(0.5)), "{rect:?}");
}

#[test]
fn display_math_takes_its_own_row() {
    let text = "a $$y$$ b";
    let (galley, built) = layout(text, None, 200.0);
    let c = &built.concealed[0];
    let y_a = x_at(&galley, 0).y;
    let y_m = x_at(&galley, c.span.inner_start).y;
    let y_b = x_at(&galley, text.chars().count() - 1).y;
    assert!(y_a < y_m && y_m < y_b, "{y_a} {y_m} {y_b}");
    let rect = math_rect(&galley, c);
    assert!((rect.center().x - 100.0).abs() < 2.0, "centered: {rect:?}");
}

#[test]
fn caret_inside_reveals_raw_and_errors_stay_raw() {
    let text = "ab $x$ and $bad$";
    let (_, built) = layout(text, Some((4, 4)), 300.0);
    let starts: Vec<usize> = built.revealed.iter().map(|s| s.start).collect();
    assert_eq!(starts, vec![3, 11]);
    assert!(built.concealed.is_empty());
    // Caret on either boundary keeps the span rendered.
    for caret in [3, 6] {
        let (_, built) = layout(text, Some((caret, caret)), 300.0);
        assert_eq!(built.concealed.len(), 1, "caret {caret}");
    }
}

/// Drives a real focused `TextEdit` through `Context::run`, recording every pass.
struct Harness {
    ctx: Context,
    text: String,
    id: Id,
    last_math: Vec<Rect>,
}

#[derive(Debug)]
struct Pass {
    revealed: Vec<Span>,
    caret: Option<(usize, usize)>,
    discarded: bool,
}

impl Harness {
    fn new(text: &str, caret: usize) -> Self {
        let ctx = Context::default();
        let id = Id::new("edit");
        let mut state = TextEditState::default();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(caret))));
        state.store(&ctx, id);
        let mut h = Self {
            ctx,
            text: text.into(),
            id,
            last_math: Vec::new(),
        };
        h.frame(Vec::new(), true);
        h
    }

    fn frame(&mut self, events: Vec<Event>, focus: bool) -> Vec<Pass> {
        let input = RawInput {
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
            ..Default::default()
        };
        let mut passes = Vec::new();
        let mut math = Vec::new();
        let (text, id) = (&mut self.text, self.id);
        let _ = self.ctx.run(input, |ctx| {
            CentralPanel::default().show(ctx, |ui| {
                let out = conceal_editor(ui, id, text, 400.0, &style(), &render);
                if focus {
                    out.response.request_focus();
                }
                math = out.math.iter().map(|(r, _, _)| *r).collect();
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

    fn key(&mut self, key: Key) -> Vec<Pass> {
        self.frame(
            vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            false,
        )
    }
}

#[test]
fn arrow_into_span_reveals_in_the_same_frame() {
    // "ab $x$ cd": span 3..6, caret just after the closing `$`.
    let mut h = Harness::new("ab $x$ cd", 6);
    assert_eq!(h.last_math.len(), 1);
    let passes = h.key(Key::ArrowLeft);
    let last = passes.last().unwrap();
    assert_eq!(last.caret, Some((5, 5)));
    assert_eq!(last.revealed.len(), 1, "{passes:?}");
    assert!(!last.discarded);
    assert_eq!(passes.len(), 2, "one discarded pass fixes the stale layout");
    assert!(h.last_math.is_empty());
    // Leaving again conceals, still without a visible stale frame.
    let passes = h.key(Key::ArrowRight);
    let last = passes.last().unwrap();
    assert_eq!(last.caret, Some((6, 6)));
    assert!(last.revealed.is_empty() && !last.discarded);
    assert_eq!(h.last_math.len(), 1);
}

#[test]
fn typing_the_closing_dollar_renders_immediately() {
    let mut h = Harness::new("ab $x cd", 5);
    assert!(h.last_math.is_empty());
    let passes = h.frame(vec![Event::Text("$".into())], false);
    assert_eq!(h.text, "ab $x$ cd");
    let last = passes.last().unwrap();
    assert_eq!(last.caret, Some((6, 6)));
    assert!(last.revealed.is_empty() && !last.discarded, "{passes:?}");
    assert_eq!(h.last_math.len(), 1);
}

#[test]
fn clicking_rendered_math_enters_it() {
    let mut h = Harness::new("ab $abcdefgh$ cd", 0);
    let target = h.last_math[0].center();
    h.frame(vec![Event::PointerMoved(target)], false);
    let press = |pressed| Event::PointerButton {
        pos: target,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    h.frame(vec![press(true)], false);
    let passes = h.frame(vec![press(false)], false);
    let last = passes.last().unwrap();
    let (caret, _) = last.caret.unwrap();
    assert!(caret > 3 && caret < 13, "caret {caret}");
    assert_eq!(last.revealed.len(), 1);
}

#[test]
fn vertical_motion_across_rendered_math_rows() {
    // Two rows: tall math row on top, prose below. Up/down must land on real indices.
    let mut h = Harness::new("ab $abcdefgh$ cd\nsecond line here", 20);
    let passes = h.key(Key::ArrowUp);
    let (caret, _) = passes.last().unwrap().caret.unwrap();
    assert!(caret <= 16, "caret {caret} on first row");
}
