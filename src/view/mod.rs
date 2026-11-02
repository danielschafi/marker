//! Document viewport: pane context, scroll, and module wiring (RF11b).

use std::time::Instant;

use egui::{Rect, Sense};

mod commit;
mod editors;
mod geometry;
mod menu;
mod overlays;
mod paint;
mod pointer;

pub(crate) use commit::place_assistant_answer;
#[allow(unused_imports)] // `TileWant` / `TileRequest` kept on the `view::` path for callers.
pub(crate) use geometry::{visible_pages, wanted_tiles, TileRequest, TileWant};
pub(crate) use paint::{cached_math_metrics, EguiMeasure};

use crate::annot::AnnotKind;
use crate::app::{MarkerApp, TabId};
use crate::theme;

use editors::{inline_editors, reclaim_edit_after_completion_escape};
use menu::paint_menu;
use overlays::paint_style_bar;
use paint::{paint_document, paint_scrollbar};
use pointer::handle_pointer;

/// Per-pane paint/pointer context. Identity of the tab being drawn or hit-tested,
/// without temporarily rewriting [`MarkerApp::active`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct PaneCtx {
    pub tab_id: TabId,
    pub rect: Rect,
    pub focused: bool,
}

pub(crate) fn viewport(app: &mut MarkerApp, ui: &mut egui::Ui, focused: bool) {
    viewport_tab(app, ui, app.active, focused);
}

pub(crate) fn viewport_tab(app: &mut MarkerApp, ui: &mut egui::Ui, tab_id: TabId, focused: bool) {
    let available = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(available, Sense::click_and_drag());
    if focused {
        app.view_rect = response.rect;
    } else {
        app.split_view_rect = response.rect;
    }

    let pane = PaneCtx {
        tab_id,
        rect: response.rect,
        focused,
    };

    // Resolve the pane tab by id — never swap `app.active` on the paint path.
    // Early returns here are safe; focus for keys stays on `app.active` until a
    // click on an unfocused pane steals it below.
    let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
        return;
    };
    if !tab.doc.fitted && pane.rect.width() > 64.0 {
        tab.doc.fit_width(pane.rect.width());
        tab.doc.fitted = true;
    }
    if let Some((page, y)) = tab.pending_jump.take() {
        tab.doc.jump_to(page, y, pane.rect);
    }
    tab.doc.clamp_scroll(pane.rect);

    handle_scroll(app, &response, pane);
    if pane.focused {
        handle_pointer(app, &response, pane);
    }

    let painter = ui.painter_at(pane.rect);
    painter.rect_filled(pane.rect, 0.0, theme::palette(ui.ctx()).backdrop);
    ensure_image_textures(app, ui.ctx(), pane);
    paint_document(app, &painter, pane, ui.ctx().pixels_per_point());
    paint_scrollbar(app, ui, pane);
    if pane.focused {
        reclaim_edit_after_completion_escape(app, ui.ctx());
        inline_editors(app, ui.ctx(), pane.rect);
        paint_menu(app, ui.ctx());
        paint_style_bar(app, ui.ctx(), pane.rect);
    }

    // Clicking the unfocused split pane focuses it for keys; scroll above does not.
    // Swap rect roles in the same frame so tile dispatch / drop hit-tests stay paired.
    if !pane.focused && (response.clicked() || response.drag_started()) {
        let previous_focused = app.view_rect;
        app.active = pane.tab_id;
        app.view_rect = pane.rect;
        if previous_focused.is_positive() {
            app.split_view_rect = previous_focused;
        }
    }
}

fn handle_scroll(app: &mut MarkerApp, response: &egui::Response, pane: PaneCtx) {
    // Match egui::Scene: drive gestures from pointer-in-rect, not hovered(),
    // so a sibling (scrollbar) doesn't swallow pinch mid-gesture.
    if !response.contains_pointer() {
        return;
    }
    let (raw, zoom, command, hover) = response.ctx.input(|input| {
        (
            input.raw_scroll_delta,
            input.zoom_delta(),
            input.modifiers.command,
            input.pointer.hover_pos().or(input.pointer.latest_pos()),
        )
    });
    let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
        return;
    };
    // Pinch keeps a cursor position on most platforms; fall back to the view
    // center if the pointer briefly drops out mid-gesture.
    let hover = hover
        .filter(|pos| response.rect.contains(*pos))
        .unwrap_or_else(|| response.rect.center());
    let pinching = (zoom - 1.0).abs() > f32::EPSILON;

    // Prefer zoom whenever egui reports a zoom delta. Trackpad pinch often
    // arrives together with a pan/scroll delta on Wayland; treating scroll
    // first made pinch feel broken, and applying both felt worse than zoom-only.
    if pinching {
        tab.doc.zoom_at(zoom, hover, response.rect);
        return;
    }
    if command && raw.y.abs() > 0.0 {
        // Ctrl+wheel / Ctrl+two-finger scroll without a synthesized Zoom event.
        let factor = (1.0 + raw.y * 0.003).clamp(0.75, 1.35);
        tab.doc.zoom_at(factor, hover, response.rect);
        return;
    }
    // Skip while Ctrl is held so ctrl+scroll stays zoom-only (egui still fills
    // raw_scroll even when it converts the same event into zoom_delta).
    if !command && raw != egui::Vec2::ZERO {
        // Wheel notches arrive as small pixel deltas and egui then smears them
        // across frames. Apply the raw delta immediately, scaled up so a notch
        // moves a readable chunk of the page.
        let gain = if raw.length() < 24.0 { 6.0 } else { 2.4 };
        let scroll = raw * gain;
        tab.doc.scroll_y -= scroll.y;
        tab.doc.scroll_x -= scroll.x;
        tab.doc.clamp_scroll(response.rect);
        tab.doc.last_scroll = Instant::now();
    }
}

fn ensure_image_textures(app: &mut MarkerApp, ctx: &egui::Context, pane: PaneCtx) {
    let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
        return;
    };
    let epoch = tab.doc.session.epoch;
    if tab.image_textures_epoch == epoch {
        return;
    }
    tab.image_textures_epoch = epoch;
    type LiveImage = (u64, usize, std::sync::Arc<[u8]>, u32, u32);
    let live: Vec<LiveImage> = tab
        .doc
        .session
        .annotations
        .iter()
        .filter_map(|annot| match &annot.kind {
            AnnotKind::Image {
                rgba,
                width,
                height,
                ..
            } => Some((
                annot.id,
                std::sync::Arc::as_ptr(rgba) as *const u8 as usize,
                rgba.clone(),
                *width,
                *height,
            )),
            _ => None,
        })
        .collect();
    let live_ids: std::collections::HashSet<u64> = live.iter().map(|(id, ..)| *id).collect();
    tab.image_textures.retain(|id, _| live_ids.contains(id));
    for (id, ptr, rgba, width, height) in live {
        let stale = tab
            .image_textures
            .get(&id)
            .is_none_or(|(cached, _)| *cached != ptr);
        if !stale {
            continue;
        }
        if width == 0 || height == 0 {
            continue;
        }
        let expected = width as usize * height as usize * 4;
        if rgba.len() < expected {
            continue;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [width as usize, height as usize],
            &rgba[..expected],
        );
        let texture = ctx.load_texture(
            format!("annot-image-{id}"),
            image,
            egui::TextureOptions::LINEAR,
        );
        tab.image_textures.insert(id, (ptr, texture));
    }
}
