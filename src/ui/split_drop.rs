//! Tab-drag split drop zones and split viewport layout (RF12).

use egui::{CornerRadius, FontFamily, FontId, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::app::{MarkerApp, SplitDropZone, SplitState, TabId};
use crate::view::viewport_tab;

use super::palette;
use super::tab_bar::TAB_PILL_H;

/// Edge / pane drop targets while a tab is being dragged to create or adjust a split.
pub(crate) fn tab_drag_overlay(app: &mut MarkerApp, ctx: &egui::Context) {
    let Some(dragged) = app.tab_drag else {
        return;
    };
    if app.tab_index(dragged).is_none() || app.tabs.len() < 2 {
        app.tab_drag = None;
        app.tab_drag_over = None;
        return;
    }

    let pointer = ctx
        .pointer_interact_pos()
        .or_else(|| ctx.pointer_latest_pos());
    let primary_down = ctx.input(|input| input.pointer.primary_down());
    let released = ctx.input(|input| input.pointer.primary_released());

    if !primary_down && !released {
        app.tab_drag = None;
        app.tab_drag_over = None;
        return;
    }

    let zone = pointer.and_then(|pos| hit_split_drop_zone(app, pos));
    if let Some(pos) = pointer {
        paint_tab_drag_feedback(app, ctx, dragged, pos, zone);
    }

    if released {
        if let Some(zone) = zone {
            app.apply_tab_drop(dragged, zone);
        }
        app.tab_drag = None;
        app.tab_drag_over = None;
    } else {
        ctx.request_repaint();
    }
}

fn hit_split_drop_zone(app: &MarkerApp, pos: Pos2) -> Option<SplitDropZone> {
    if let Some(target) = app.tab_drag_over {
        return Some(SplitDropZone::Tab(target));
    }

    if let Some(split) = app.split {
        let other = app.split_view_rect;
        let active = app.view_rect;
        let full = if active.is_positive() && other.is_positive() {
            active.union(other)
        } else if active.is_positive() {
            active
        } else if other.is_positive() {
            other
        } else {
            return None;
        };
        if full.contains(pos) {
            // Prefer outer edges so drag can still flip side-by-side ↔ stacked.
            if let Some(edge) = edge_drop_zone(full, pos) {
                let flips = match edge {
                    SplitDropZone::Left | SplitDropZone::Right => split.stacked,
                    SplitDropZone::Top | SplitDropZone::Bottom => !split.stacked,
                    SplitDropZone::OtherPane
                    | SplitDropZone::ActivePane
                    | SplitDropZone::Tab(_) => false,
                };
                if flips {
                    return Some(edge);
                }
            }
            if other.is_positive() && other.contains(pos) {
                return Some(SplitDropZone::OtherPane);
            }
            if active.is_positive() && active.contains(pos) {
                return Some(SplitDropZone::ActivePane);
            }
        }
        return None;
    }

    let view = app.view_rect;
    if !view.is_positive() || !view.contains(pos) {
        return None;
    }
    edge_drop_zone(view, pos)
}

fn edge_drop_zone(full: Rect, pos: Pos2) -> Option<SplitDropZone> {
    let w = full.width().max(1.0);
    let h = full.height().max(1.0);
    let rel_x = ((pos.x - full.left()) / w).clamp(0.0, 1.0);
    let rel_y = ((pos.y - full.top()) / h).clamp(0.0, 1.0);
    let band_x = 0.28;
    let band_y = 0.28;
    let candidates = [
        (rel_x, band_x, SplitDropZone::Left),
        (1.0 - rel_x, band_x, SplitDropZone::Right),
        (rel_y, band_y, SplitDropZone::Top),
        (1.0 - rel_y, band_y, SplitDropZone::Bottom),
    ];
    candidates
        .into_iter()
        .filter(|(dist, band, _)| *dist <= *band)
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, _, zone)| zone)
}

fn paint_tab_drag_feedback(
    app: &MarkerApp,
    ctx: &egui::Context,
    dragged: TabId,
    pos: Pos2,
    zone: Option<SplitDropZone>,
) {
    let colors = palette(ctx);
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        Id::new("marker-tab-drag"),
    ));

    if let Some(zone) = zone {
        let highlight = drop_zone_rect(app, zone);
        if highlight.is_positive() {
            painter.rect_filled(
                highlight,
                CornerRadius::same(4),
                colors.accent.gamma_multiply(0.22),
            );
            painter.rect_stroke(
                highlight,
                CornerRadius::same(4),
                Stroke::new(1.5_f32, colors.accent.gamma_multiply(0.85)),
                StrokeKind::Inside,
            );
        }
    }

    let name = app
        .tab_by_id(dragged)
        .and_then(|tab| tab.doc.path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("document.pdf");
    let galley = painter.layout_no_wrap(
        name.to_owned(),
        FontId::new(12.5, FontFamily::Proportional),
        colors.text,
    );
    let pill = Rect::from_center_size(
        pos + Vec2::new(12.0, 14.0),
        Vec2::new(galley.size().x + 16.0, TAB_PILL_H),
    );
    painter.rect_filled(pill, CornerRadius::same(6), colors.chrome_raised);
    painter.rect_stroke(
        pill,
        CornerRadius::same(6),
        Stroke::new(1.0_f32, colors.hairline),
        StrokeKind::Inside,
    );
    painter.galley(
        Pos2::new(pill.left() + 8.0, pill.center().y - galley.size().y * 0.5),
        galley,
        colors.text,
    );
}

fn drop_zone_rect(app: &MarkerApp, zone: SplitDropZone) -> Rect {
    match zone {
        SplitDropZone::OtherPane => app.split_view_rect,
        SplitDropZone::ActivePane => app.view_rect,
        // List-row highlight is painted in the open-tabs overlay.
        SplitDropZone::Tab(_) => Rect::NOTHING,
        SplitDropZone::Left | SplitDropZone::Right | SplitDropZone::Top | SplitDropZone::Bottom => {
            let full = if app.split.is_some() {
                let a = app.view_rect;
                let b = app.split_view_rect;
                if a.is_positive() && b.is_positive() {
                    a.union(b)
                } else if a.is_positive() {
                    a
                } else {
                    b
                }
            } else {
                app.view_rect
            };
            if !full.is_positive() {
                return Rect::NOTHING;
            }
            let mid_x = full.center().x;
            let mid_y = full.center().y;
            match zone {
                SplitDropZone::Left => {
                    Rect::from_min_max(full.min, Pos2::new(mid_x, full.bottom()))
                }
                SplitDropZone::Right => Rect::from_min_max(Pos2::new(mid_x, full.top()), full.max),
                SplitDropZone::Top => Rect::from_min_max(full.min, Pos2::new(full.right(), mid_y)),
                SplitDropZone::Bottom => {
                    Rect::from_min_max(Pos2::new(full.left(), mid_y), full.max)
                }
                SplitDropZone::OtherPane | SplitDropZone::ActivePane | SplitDropZone::Tab(_) => {
                    unreachable!()
                }
            }
        }
    }
}

pub(crate) fn split_viewports(app: &mut MarkerApp, ui: &mut egui::Ui, split: SplitState) {
    let colors = palette(ui.ctx());
    let full = ui.available_rect_before_wrap();
    let gap = 4.0;
    let handle = 5.0;
    let ratio = split.ratio.clamp(0.2, 0.8);

    let (first_rect, second_rect, handle_rect) = if split.stacked {
        let h = full.height();
        let top_h = ((h - handle) * ratio).clamp(80.0, (h - handle - 80.0).max(80.0));
        let a = Rect::from_min_max(full.min, Pos2::new(full.right(), full.top() + top_h));
        let hr = Rect::from_min_max(
            Pos2::new(full.left(), a.bottom()),
            Pos2::new(full.right(), a.bottom() + handle),
        );
        let b = Rect::from_min_max(Pos2::new(full.left(), hr.bottom() + gap * 0.0), full.max);
        (a, b, hr)
    } else {
        let w = full.width();
        let left_w = ((w - handle) * ratio).clamp(120.0, (w - handle - 120.0).max(120.0));
        let a = Rect::from_min_max(full.min, Pos2::new(full.left() + left_w, full.bottom()));
        let hr = Rect::from_min_max(
            Pos2::new(a.right(), full.top()),
            Pos2::new(a.right() + handle, full.bottom()),
        );
        let b = Rect::from_min_max(Pos2::new(hr.right(), full.top()), full.max);
        (a, b, hr)
    };

    let first = split.first;
    let second = split.second;
    if !split.contains(app.active) {
        app.active = first;
    }
    let focus_first = app.active == first;
    ui.scope_builder(egui::UiBuilder::new().max_rect(first_rect), |ui| {
        viewport_tab(app, ui, first, focus_first);
    });

    let response = ui.interact(handle_rect, Id::new("marker-split-handle"), Sense::drag());
    ui.painter().rect_filled(handle_rect, 0.0, colors.chrome);
    if response.hovered() || response.dragged() {
        ui.painter().rect_filled(
            handle_rect.shrink(1.0),
            CornerRadius::same(2),
            colors.accent.gamma_multiply(0.55),
        );
        ui.ctx().set_cursor_icon(if split.stacked {
            egui::CursorIcon::ResizeVertical
        } else {
            egui::CursorIcon::ResizeHorizontal
        });
    }
    if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some(split) = app.split.as_mut() {
                if split.stacked {
                    split.ratio = ((pos.y - full.top()) / full.height()).clamp(0.2, 0.8);
                } else {
                    split.ratio = ((pos.x - full.left()) / full.width()).clamp(0.2, 0.8);
                }
            }
        }
    }

    ui.scope_builder(egui::UiBuilder::new().max_rect(second_rect), |ui| {
        viewport_tab(app, ui, second, !focus_first);
    });
    app.dispatch_tiles(ui.ctx().pixels_per_point());
}
