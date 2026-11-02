//! Annotation tool strip (RF12).

use egui::{
    Align, Align2, Color32, CornerRadius, FontFamily, FontId, Layout, Pos2, Rect, Sense, Stroke,
    StrokeKind, TopBottomPanel, Vec2, WidgetInfo, WidgetType,
};

use crate::app::{MarkerApp, Tool};
use crate::theme;

use super::palette;

pub(super) const TOOL_BAR_H: f32 = 28.0;
const TOOL_CHIP: Vec2 = Vec2::new(24.0, 22.0);

pub(super) fn tool_bar(app: &mut MarkerApp, ctx: &egui::Context) {
    let colors = palette(ctx);
    if app.tab().is_none() || !app.settings.toolbar_visible {
        return;
    }
    TopBottomPanel::top("tools")
        .exact_height(TOOL_BAR_H)
        .show_separator_line(false)
        .frame(
            egui::Frame::new()
                .fill(colors.chrome)
                .stroke(Stroke::NONE)
                .inner_margin(egui::Margin::symmetric(6, 2)),
        )
        .show(ctx, |ui| {
            let width_id = ui.id().with("cluster-width");
            let known = ui.data(|data| data.get_temp::<f32>(width_id));
            let lead = known
                .map(|width| ((ui.available_width() - width) * 0.5).max(0.0))
                .unwrap_or(0.0);
            let mut cluster_width = known.unwrap_or(0.0);
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
                if lead > 0.0 {
                    ui.add_space(lead);
                }
                let cluster = ui.scope_builder(
                    egui::UiBuilder::new().layout(Layout::left_to_right(Align::Center)),
                    |ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(2.0, 0.0);
                        for tool in Tool::ALL {
                            tool_chip(ui, &mut app.tool, tool);
                        }
                        vbar(ui);
                        app.color_controls(ui);
                        app.metric_controls(ui);
                    },
                );
                cluster_width = cluster.response.rect.width();
            });
            ui.data_mut(|data| data.insert_temp(width_id, cluster_width));
        });
}

fn tool_chip(ui: &mut egui::Ui, current: &mut Tool, tool: Tool) {
    let colors = palette(ui.ctx());
    let selected = *current == tool;
    let (rect, response) = ui.allocate_exact_size(TOOL_CHIP, Sense::click());
    let fill = if selected {
        theme::accent_fill(ui.ctx(), 48)
    } else if response.hovered() {
        theme::chrome_overlay(ui.ctx(), 16)
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(5), fill);
    }
    let icon = Rect::from_center_size(rect.center() + Vec2::new(-2.5, -0.5), Vec2::splat(13.0));
    let color = if selected { colors.accent } else { colors.text };
    paint_tool_icon(ui.painter(), icon, tool, color);
    let key_color = if selected {
        colors.accent
    } else {
        colors.text_dim
    };
    ui.painter().text(
        rect.right_bottom() + Vec2::new(-1.0, 0.0),
        Align2::RIGHT_BOTTOM,
        tool.shortcut().name(),
        FontId::new(8.0, FontFamily::Proportional),
        key_color,
    );
    if response.clicked() {
        *current = tool;
    }
    response.widget_info(|| {
        WidgetInfo::selected(
            WidgetType::RadioButton,
            ui.is_enabled(),
            selected,
            tool_a11y_label(tool),
        )
    });
    response.on_hover_text(format!("{} ({})", tool.hint(), tool.shortcut().name()));
}

fn paint_tool_icon(painter: &egui::Painter, rect: Rect, tool: Tool, color: Color32) {
    let stroke = Stroke::new(1.35_f32, color);
    let origin = rect.left_top();
    match tool {
        Tool::Select => {
            let points = vec![
                origin + Vec2::new(1.5, 1.0),
                origin + Vec2::new(1.5, 13.5),
                origin + Vec2::new(4.8, 10.0),
                origin + Vec2::new(7.6, 14.2),
                origin + Vec2::new(9.6, 13.2),
                origin + Vec2::new(6.8, 9.0),
                origin + Vec2::new(11.2, 8.6),
            ];
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        Tool::Highlight => {
            let bar = Rect::from_min_max(
                origin + Vec2::new(0.5, 11.0),
                origin + Vec2::new(14.5, 14.2),
            );
            painter.rect_filled(bar, 1.5, color.gamma_multiply(0.4));
            let points = vec![
                origin + Vec2::new(1.5, 7.5),
                origin + Vec2::new(7.5, 1.0),
                origin + Vec2::new(13.5, 5.5),
                origin + Vec2::new(7.5, 12.0),
            ];
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        Tool::Underline => {
            painter.line_segment(
                [
                    origin + Vec2::new(1.0, 12.4),
                    origin + Vec2::new(13.2, 12.4),
                ],
                Stroke::new(1.7_f32, color),
            );
            for x in [3.0, 7.0, 11.0] {
                painter.line_segment(
                    [origin + Vec2::new(x, 2.2), origin + Vec2::new(x, 8.6)],
                    stroke,
                );
            }
        }
        Tool::StrikeOut => {
            painter.line_segment(
                [origin + Vec2::new(0.8, 7.0), origin + Vec2::new(13.4, 7.0)],
                Stroke::new(1.7_f32, color),
            );
            for x in [3.2, 7.0, 10.8] {
                painter.line_segment(
                    [origin + Vec2::new(x, 2.4), origin + Vec2::new(x, 11.6)],
                    stroke,
                );
            }
        }
        Tool::Squiggly => {
            let base = origin.y + 11.2;
            let mut pts = Vec::new();
            let mut x = origin.x + 0.6;
            let end = origin.x + 13.4;
            while x <= end {
                let t = (x - origin.x) / 4.2 * std::f32::consts::TAU;
                pts.push(Pos2::new(x, base + 1.7 * t.sin()));
                x += 1.2;
            }
            painter.add(egui::Shape::line(pts, Stroke::new(1.35_f32, color)));
        }
        Tool::Text => {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "T",
                FontId::new(14.0, FontFamily::Proportional),
                color,
            );
        }
        Tool::Note => {
            let note = Rect::from_center_size(rect.center(), Vec2::splat(11.0));
            painter.rect_filled(note, 2.0, color.gamma_multiply(0.85));
            painter.text(
                note.center(),
                Align2::CENTER_CENTER,
                "✎",
                FontId::new(9.0, FontFamily::Proportional),
                Color32::from_rgb(40, 40, 40),
            );
        }
        Tool::Rect => {
            painter.rect_stroke(rect.shrink(1.5), 2.0, stroke, StrokeKind::Inside);
        }
        Tool::Ellipse => {
            painter.circle_stroke(rect.center(), rect.width() * 0.38, stroke);
        }
        Tool::Line => {
            painter.line_segment(
                [
                    rect.left_bottom() + Vec2::new(1.5, -1.5),
                    rect.right_top() + Vec2::new(-1.5, 1.5),
                ],
                stroke,
            );
        }
        Tool::Math => {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "∑",
                FontId::new(15.0, FontFamily::Proportional),
                color,
            );
        }
    }
}

fn vbar(ui: &mut egui::Ui) {
    let colors = palette(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(Vec2::new(7.0, 14.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.top()..=rect.bottom(),
        Stroke::new(1.0_f32, colors.hairline),
    );
}

fn tool_a11y_label(tool: Tool) -> &'static str {
    match tool {
        Tool::Select => "Select tool",
        Tool::Highlight => "Highlight tool",
        Tool::Underline => "Underline tool",
        Tool::StrikeOut => "Strikeout tool",
        Tool::Squiggly => "Squiggly tool",
        Tool::Text => "Text tool",
        Tool::Note => "Note tool",
        Tool::Rect => "Rectangle tool",
        Tool::Ellipse => "Ellipse tool",
        Tool::Line => "Line tool",
        Tool::Math => "Equation tool",
    }
}
