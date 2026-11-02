//! Style-bar overlay (scrollbar lives in paint) (RF11b).

use std::time::Instant;

use egui::{Id, PointerButton, Pos2, Rect, Vec2};

use super::geometry::pdf_rect_screen;
use crate::app::MarkerApp;

pub(super) fn paint_style_bar(app: &mut MarkerApp, ctx: &egui::Context, view: Rect) {
    let Some(bar) = app.tab().and_then(|tab| tab.style_bar) else {
        return;
    };
    if app
        .tab()
        .is_none_or(|tab| tab.doc.session.get(bar.id).is_none())
    {
        if let Some(tab) = app.tab_mut() {
            tab.style_bar = None;
        }
        return;
    }

    let anchor = app.tab().and_then(|tab| {
        let annot = tab.doc.session.get(bar.id)?;
        let bounds = annot.bounds()?;
        let screen = pdf_rect_screen(&tab.doc, annot.page, bounds, view);
        Some(screen)
    });
    let Some(mark) = anchor else {
        if let Some(tab) = app.tab_mut() {
            tab.style_bar = None;
        }
        return;
    };

    // Prefer just below the mark; flip above if that would leave the view.
    let mut pos = Pos2::new(mark.center().x, mark.bottom() + 6.0);
    // Dual palettes (highlight + ink) need a bit more width when both show.
    let estimated = Vec2::new(260.0, 28.0);
    if pos.y + estimated.y > view.bottom() - 4.0 {
        pos.y = mark.top() - estimated.y - 6.0;
    }
    pos.x = (pos.x - estimated.x * 0.5).clamp(
        view.left() + 4.0,
        (view.right() - estimated.x - 4.0).max(view.left() + 4.0),
    );

    let mut delete = false;
    let area = egui::Area::new(Id::new("marker-style-bar"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .interactable(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::symmetric(6, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        delete = app.style_bar_contents(ui, true);
                    });
                });
        });

    let over_bar = area.response.hovered() || area.response.contains_pointer();
    if let Some(until) = bar.until {
        if Instant::now() >= until && !over_bar {
            // Auto-hide the strip but keep selection until Escape / click-away.
            if let Some(tab) = app.tab_mut() {
                tab.style_bar = None;
            }
            return;
        }
    }

    let right_click = ctx.input(|input| input.pointer.button_pressed(PointerButton::Secondary));
    // Use press (not release) so dismissing isn't triggered by the mouse-up that finished drawing.
    let left_press = ctx.input(|input| input.pointer.button_pressed(PointerButton::Primary));
    let left_outside = left_press && !over_bar && !area.response.clicked() && !right_click;
    if delete {
        if let Some(tab) = app.tab_mut() {
            if !tab.is_selected(bar.id) {
                tab.select_only(bar.id);
            }
            tab.style_bar = None;
        }
        app.delete_selected();
    } else if left_outside {
        let pointer = ctx
            .pointer_interact_pos()
            .or_else(|| ctx.pointer_latest_pos());
        let hit_self = pointer.and_then(|pos| {
            let tab = app.tab()?;
            let (page, point) = tab.doc.screen_to_page(pos, view)?;
            tab.doc.session.hit_test(page, point, 6.0 / tab.doc.scale)
        }) == Some(bar.id);
        if hit_self {
            // Clicked the annotation under the strip — keep selection, dismiss strip.
            if let Some(tab) = app.tab_mut() {
                tab.style_bar = None;
            }
        } else {
            app.clear_page_selection();
        }
    }
}
