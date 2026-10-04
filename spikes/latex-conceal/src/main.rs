use std::cell::RefCell;
use std::collections::HashMap;

use eframe::egui;
use egui::{Color32, FontFamily, FontId, Id, Vec2};
use latex_conceal_spike::{conceal_editor, fake_size, fake_tex, Render, Style};

/// Simulated Typst latency so the Pending path is visible.
const RENDER_DELAY_S: f64 = 0.12;

struct Entry {
    ready_at: f64,
    result: Result<(String, bool, Vec2), String>,
}

struct App {
    text: String,
    cache: RefCell<HashMap<(String, bool), Entry>>,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = ctx.input(|i| i.time);
        let font_px = 18.0;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("Conceal spike: caret inside $…$ reveals source; caret at a boundary or outside renders it.");
            ui.add_space(8.0);
            let style = Style {
                font: FontId::new(font_px, FontFamily::Proportional),
                color: ui.visuals().text_color(),
                source_color: Color32::from_rgb(120, 170, 255),
                source_bg: Color32::from_rgba_unmultiplied(80, 120, 255, 28),
                error: Color32::from_rgb(230, 90, 80),
            };
            let render = |inner: &str, display: bool| -> Render {
                let key = (inner.to_string(), display);
                let mut cache = self.cache.borrow_mut();
                let entry = cache.entry(key).or_insert_with(|| Entry {
                    ready_at: now + RENDER_DELAY_S,
                    result: fake_tex(inner).map(|(s, tall)| {
                        let size = ctx.fonts_mut(|f| fake_size(f, &s, tall || display, font_px));
                        (s, tall, size)
                    }),
                });
                if now < entry.ready_at {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(entry.ready_at - now));
                    return Render::Pending;
                }
                match &entry.result {
                    Ok((_, _, size)) => Render::Ready(*size),
                    Err(e) => Render::Error(e.clone()),
                }
            };
            let out = conceal_editor(ui, Id::new("conceal"), &mut self.text, 460.0, &style, &render);
            let painter = ui.painter();
            for (rect, inner, display) in &out.math {
                let cache = self.cache.borrow();
                let Some(Entry { result: Ok((s, tall, _)), .. }) = cache.get(&(inner.clone(), *display)) else {
                    continue;
                };
                painter.rect_filled(*rect, 3.0, Color32::from_rgba_unmultiplied(255, 200, 80, 40));
                let px = if *tall { font_px * 1.1 } else { font_px };
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    s,
                    FontId::new(px, FontFamily::Proportional),
                    Color32::from_rgb(255, 200, 80),
                );
            }
            // Live preview + error for the span under the caret.
            if let (Some((caret, _)), Some(span)) = (
                out.caret,
                out.revealed.iter().find(|s| out.caret.is_some_and(|(c, _)| c > s.start && c < s.end)),
            ) {
                let _ = caret;
                let chars: Vec<char> = self.text.chars().collect();
                let inner = span.inner(&chars);
                let anchor = out.galley.pos_from_cursor(egui::text::CCursor::new(span.start));
                let pos = out.galley_pos + anchor.left_bottom().to_vec2() + Vec2::new(0.0, 6.0);
                let label = match fake_tex(&inner) {
                    Ok((s, _)) => egui::RichText::new(s).size(font_px).color(Color32::from_rgb(255, 200, 80)),
                    Err(e) => egui::RichText::new(e).size(12.0).color(style.error),
                };
                egui::Area::new(Id::new("preview"))
                    .order(egui::Order::Tooltip)
                    .fixed_pos(pos)
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.label(label);
                        });
                    });
            }
            ui.add_space(16.0);
            ui.monospace(format!(
                "caret={:?} revealed={:?} pass={} discarded_this_pass={}",
                out.caret,
                out.revealed.iter().map(|s| (s.start, s.end)).collect::<Vec<_>>(),
                ctx.current_pass_index(),
                out.discarded,
            ));
        });
    }
}

fn main() -> eframe::Result {
    let text = std::env::args().nth(1).unwrap_or_else(|| {
        r"Energy $E = mc^2$ and the series $\sum_{i} x_i \le \infty$ converge; also $\alpha+\beta$. Display: $$\frac{a}{b}$$ then prose after, and a broken $\bad{x}$ one.".into()
    });
    eframe::run_native(
        "latex-conceal spike",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([560.0, 360.0]),
            ..Default::default()
        },
        Box::new(|_| {
            Ok(Box::new(App {
                text,
                cache: RefCell::new(HashMap::new()),
            }))
        }),
    )
}
