use std::time::{Duration, Instant};

use egui::{
    Color32, CursorIcon, FontFamily, FontId, Id, PointerButton, Pos2, Rect, Sense, Stroke,
    TextEdit, Vec2,
};

use crate::annot::{
    click_glyph_range, glyph_at, highlight_quads, word_range, AnnotKind, Handle, ShapeKind,
};
use crate::app::{ContextMenu, CreateKind, DocState, Drag, MarkerApp, Tab, TextSel, Tool};
use crate::assistant::{CaptureMode, LearningSelection};
use crate::geom::{tile_render_scale, PdfPoint, PdfRect, MAX_SCALE, MIN_SCALE};
use crate::pdf::{PageInfo, TILE_PX};
use crate::theme;

const GAP: f32 = 16.0;
const PAD: f32 = 24.0;

impl DocState {
    pub(crate) fn rebuild_tops(pages: &[PageInfo]) -> Vec<f32> {
        let mut y = PAD;
        let mut tops = Vec::with_capacity(pages.len());
        for page in pages {
            tops.push(y);
            y += page.height() + GAP;
        }
        tops
    }

    pub(crate) fn doc_height_pts(&self) -> f32 {
        let Some(last) = self.pages.last() else {
            return PAD;
        };
        self.tops.last().copied().unwrap_or(PAD) + last.height() + PAD
    }

    pub(crate) fn doc_height_px(&self) -> f32 {
        self.doc_height_pts() * self.scale
    }

    pub(crate) fn clamp_scroll(&mut self, view: Rect) {
        let max_y = (self.doc_height_px() - view.height()).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
        let max_page = self.pages.iter().map(PageInfo::width).fold(0.0, f32::max);
        // Only allow horizontal scroll when the page (plus padding) is wider than
        // the view. Narrow pages stay centered via page_origin, with scroll_x = 0.
        let max_x = (max_page * self.scale + PAD * 2.0 - view.width()).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
    }

    pub(crate) fn fit_width(&mut self, view_w: f32) {
        let max_page = self.pages.iter().map(PageInfo::width).fold(1.0, f32::max);
        self.scale = ((view_w - PAD * 2.0) / max_page).clamp(MIN_SCALE, MAX_SCALE);
        self.scroll_x = 0.0;
        self.last_zoom = Instant::now();
    }

    pub(crate) fn fit_height(&mut self, view_h: f32) {
        let page = self.current_page(view_h.max(1.0));
        let height = self
            .pages
            .get(page)
            .map(PageInfo::height)
            .unwrap_or(1.0)
            .max(1.0);
        self.scale = ((view_h - PAD * 2.0) / height).clamp(MIN_SCALE, MAX_SCALE);
        self.scroll_x = 0.0;
        // Keep the current page near the top of the viewport.
        if let Some(top) = self.tops.get(page) {
            self.scroll_y = top * self.scale;
        }
        self.last_zoom = Instant::now();
    }

    /// MuPDF device pixels per PDF point for tiles (bucketed zoom × display ppp).
    pub(crate) fn render_scale(&self, pixels_per_point: f32) -> f32 {
        tile_render_scale(self.scale, pixels_per_point)
    }

    fn page_origin(&self, page: usize, view: Rect) -> Pos2 {
        let width = self.pages[page].width() * self.scale;
        let x = if width + PAD * 2.0 <= view.width() {
            // Whole page fits: always centered, ignore scroll_x.
            view.left() + (view.width() - width) * 0.5
        } else {
            view.left() + PAD - self.scroll_x
        };
        let y = view.top() + self.tops[page] * self.scale - self.scroll_y;
        Pos2::new(x, y)
    }

    fn page_rect(&self, page: usize, view: Rect) -> Rect {
        let origin = self.page_origin(page, view);
        let info = self.pages[page];
        Rect::from_min_size(
            origin,
            Vec2::new(info.width() * self.scale, info.height() * self.scale),
        )
    }

    fn page_to_screen(&self, page: usize, point: PdfPoint, view: Rect) -> Pos2 {
        let origin = self.page_origin(page, view);
        let info = self.pages[page];
        Pos2::new(
            origin.x + (point.x - info.x0) * self.scale,
            origin.y + (point.y - info.y0) * self.scale,
        )
    }

    fn screen_to_page(&self, screen: Pos2, view: Rect) -> Option<(usize, PdfPoint)> {
        for page in 0..self.pages.len() {
            let rect = self.page_rect(page, view);
            if rect.contains(screen) {
                let info = self.pages[page];
                let point = PdfPoint::new(
                    info.x0 + (screen.x - rect.left()) / self.scale,
                    info.y0 + (screen.y - rect.top()) / self.scale,
                );
                return Some((page, point));
            }
        }
        None
    }

    pub(crate) fn zoom_at(&mut self, factor: f32, cursor: Pos2, view: Rect) {
        let new_scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        if (new_scale - self.scale).abs() < f32::EPSILON {
            return;
        }
        if let Some((page, point)) = self.screen_to_page(cursor, view) {
            let info = self.pages[page];
            let top = self.tops[page];
            self.scale = new_scale;
            self.scroll_y =
                view.top() + top * new_scale + (point.y - info.y0) * new_scale - cursor.y;
            let page_w = info.width() * new_scale;
            if page_w + PAD * 2.0 > view.width() {
                // Page overflows: pin the zoom point under the cursor horizontally.
                self.scroll_x = view.left() + PAD + (point.x - info.x0) * new_scale - cursor.x;
            } else {
                // Page fits: stay centered (page_origin ignores scroll_x).
                self.scroll_x = 0.0;
            }
        } else {
            let offset = cursor.y - view.top();
            let old = self.scale;
            self.scale = new_scale;
            self.scroll_y = crate::geom::zoom_scroll(old, new_scale, self.scroll_y, offset);
            // Horizontal: if the page now fits, recenter; otherwise keep scroll_x
            // and let clamp_scroll bound it to the page edges.
            let max_page = self.pages.iter().map(PageInfo::width).fold(0.0, f32::max);
            if max_page * new_scale + PAD * 2.0 <= view.width() {
                self.scroll_x = 0.0;
            }
        }
        self.last_zoom = Instant::now();
        self.last_fit = None;
        self.clamp_scroll(view);
    }

    pub(crate) fn jump_to(&mut self, page: usize, y: Option<f32>, view: Rect) {
        if page >= self.pages.len() {
            return;
        }
        let y = y.unwrap_or(0.0).max(0.0);
        self.scroll_y = (self.tops[page] + y) * self.scale - 12.0;
        self.clamp_scroll(view);
    }

    pub(crate) fn current_page(&self, view_h: f32) -> usize {
        if self.pages.is_empty() {
            return 0;
        }
        let mid = self.scroll_y + view_h * 0.35;
        let mut best = 0;
        for (index, top) in self.tops.iter().enumerate() {
            if top * self.scale <= mid {
                best = index;
            }
        }
        best
    }
}

pub(crate) fn viewport(app: &mut MarkerApp, ui: &mut egui::Ui, focused: bool) {
    viewport_tab(app, ui, app.active, focused);
}

pub(crate) fn viewport_tab(
    app: &mut MarkerApp,
    ui: &mut egui::Ui,
    tab_index: usize,
    focused: bool,
) {
    let available = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(available, Sense::click_and_drag());
    if focused {
        app.view_rect = response.rect;
    } else {
        app.split_view_rect = response.rect;
    }

    let prev = app.active;
    app.active = tab_index;

    let Some(tab) = app.tab_mut() else {
        app.active = prev;
        return;
    };
    if !tab.doc.fitted && response.rect.width() > 64.0 {
        tab.doc.fit_width(response.rect.width());
        tab.doc.fitted = true;
    }
    if let Some((page, y)) = tab.pending_jump.take() {
        tab.doc.jump_to(page, y, response.rect);
    }
    tab.doc.clamp_scroll(response.rect);

    handle_scroll(app, &response);
    if focused {
        handle_pointer(app, &response);
    }

    let painter = ui.painter_at(response.rect);
    painter.rect_filled(response.rect, 0.0, theme::palette(ui.ctx()).backdrop);
    ensure_image_textures(app, ui.ctx());
    paint_document(app, &painter, response.rect, ui.ctx().pixels_per_point());
    paint_scrollbar(app, ui, response.rect);
    if focused {
        inline_editors(app, ui.ctx(), response.rect);
        paint_menu(app, ui.ctx());
        paint_style_bar(app, ui.ctx(), response.rect);
    }

    let steal_focus = !focused && (response.clicked() || response.drag_started());
    app.active = prev;
    if steal_focus {
        app.active = tab_index;
        app.view_rect = response.rect;
    }
}

fn handle_scroll(app: &mut MarkerApp, response: &egui::Response) {
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
    let Some(tab) = app.tab_mut() else {
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

fn handle_pointer(app: &mut MarkerApp, response: &egui::Response) {
    let rect = response.rect;
    let space = response.ctx.input(|input| input.key_down(egui::Key::Space));
    let middle = response.ctx.input(|input| {
        (
            input.pointer.button_pressed(PointerButton::Middle),
            input.pointer.button_down(PointerButton::Middle),
            input.pointer.button_released(PointerButton::Middle),
            input.pointer.hover_pos(),
        )
    });

    if middle.0 {
        if let Some(pos) = middle.3 {
            if rect.contains(pos) {
                if let Some(tab) = app.tab() {
                    let drag = Drag::Pan {
                        scroll_x: tab.doc.scroll_x,
                        scroll_y: tab.doc.scroll_y,
                        pos,
                    };
                    if let Some(tab) = app.tab_mut() {
                        tab.drag = Some(drag);
                    }
                }
            }
        }
    }
    if middle.1 {
        if let Some(Drag::Pan {
            scroll_x,
            scroll_y,
            pos,
        }) = app.tab().and_then(|tab| tab.drag.clone())
        {
            if let Some(now) = middle.3 {
                if let Some(tab) = app.tab_mut() {
                    tab.doc.scroll_x = scroll_x - (now.x - pos.x);
                    tab.doc.scroll_y = scroll_y - (now.y - pos.y);
                    tab.doc.clamp_scroll(rect);
                    tab.doc.last_scroll = Instant::now();
                }
            }
        }
        return;
    }
    if middle.2 {
        if matches!(
            app.tab().and_then(|tab| tab.drag.as_ref()),
            Some(Drag::Pan { .. })
        ) {
            if let Some(tab) = app.tab_mut() {
                tab.drag = None;
            }
        }
    }

    if response.secondary_clicked() {
        let pos = response
            .interact_pointer_pos()
            .or_else(|| response.ctx.input(|input| input.pointer.hover_pos()));
        if let Some(pos) = pos {
            open_context_menu(app, pos, rect);
        }
    }

    if response.drag_started_by(PointerButton::Primary) {
        if let Some(pos) = response.interact_pointer_pos() {
            begin_primary(app, pos, rect, space);
        }
    }
    if response.dragged_by(PointerButton::Primary) {
        if let Some(pos) = response.interact_pointer_pos() {
            update_primary(app, pos, rect);
        }
    }
    if response.drag_stopped_by(PointerButton::Primary) {
        let pos = response.interact_pointer_pos();
        end_primary(app, pos, rect, response.double_clicked());
    } else if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            click(app, pos, rect, response.double_clicked());
        }
    }

    if matches!(
        app.tab().and_then(|tab| tab.drag.as_ref()),
        Some(Drag::Pan { .. })
    ) {
        response.ctx.set_cursor_icon(CursorIcon::Grabbing);
    } else if matches!(app.capture, CaptureMode::Region)
        || matches!(
            app.tab().and_then(|tab| tab.drag.as_ref()),
            Some(Drag::Marquee { .. })
        )
    {
        response.clone().on_hover_cursor(CursorIcon::Crosshair);
    } else if matches!(app.capture, CaptureMode::LearningText)
        || matches!(app.tool, Tool::Highlight | Tool::Text | Tool::Math)
        || matches!(
            app.tab().and_then(|tab| tab.drag.as_ref()),
            Some(Drag::TextSelect { .. })
        )
    {
        response.clone().on_hover_cursor(CursorIcon::Text);
    }
}

fn begin_primary(app: &mut MarkerApp, pos: Pos2, view: Rect, space: bool) {
    if app.capture != CaptureMode::None && !space {
        let capture = app.capture;
        let Some(tab) = app.tab_mut() else {
            return;
        };
        let Some((page, point)) = tab.doc.screen_to_page(pos, view) else {
            return;
        };
        match capture {
            CaptureMode::LearningText => {
                let index = tab
                    .doc
                    .glyphs
                    .get(&page)
                    .and_then(|glyphs| glyph_at(glyphs, point));
                tab.drag = Some(Drag::LearningSelect {
                    page,
                    anchor: index,
                    current: index,
                    origin: point,
                    current_pt: point,
                });
            }
            CaptureMode::Region => {
                tab.drag = Some(Drag::Region {
                    page,
                    origin: point,
                    current: point,
                });
            }
            CaptureMode::None => {}
        }
        return;
    }

    let tool = app.tool;
    let Some(tab) = app.tab_mut() else {
        return;
    };
    if space {
        tab.drag = Some(Drag::Pan {
            scroll_x: tab.doc.scroll_x,
            scroll_y: tab.doc.scroll_y,
            pos,
        });
        return;
    }

    let Some((page, point)) = tab.doc.screen_to_page(pos, view) else {
        tab.selected.clear();
        tab.text_sel = None;
        tab.editing = None;
        return;
    };
    match tool {
        Tool::Select => begin_select(tab, page, point, pos, view),
        Tool::Highlight => begin_highlight(tab, page, point),
        Tool::Rect | Tool::Ellipse | Tool::Line => {
            tab.drag = Some(Drag::Shape {
                page,
                kind: match tool {
                    Tool::Ellipse => ShapeKind::Ellipse,
                    Tool::Line => ShapeKind::Line,
                    _ => ShapeKind::Rect,
                },
                origin: point,
                current: point,
            });
        }
        Tool::Text | Tool::Math => {
            if let Some((id, handle)) =
                resize_target(&tab.doc, tab.primary_selected(), tool, pos, view)
            {
                if let Some(annot) = tab.doc.session.get(id) {
                    let origin = annot.kind.clone();
                    let annot_page = annot.page;
                    tab.select_only(id);
                    tab.editing = None;
                    tab.drag = Some(Drag::Resize {
                        id,
                        handle,
                        origin,
                        page: annot_page,
                    });
                    return;
                }
            }
            if let Some(id) = tab.doc.session.hit_test(page, point, 4.0 / tab.doc.scale) {
                let matches_tool = tab.doc.session.get(id).is_some_and(|annot| match tool {
                    Tool::Text => matches!(annot.kind, AnnotKind::Text { .. }),
                    Tool::Math => matches!(annot.kind, AnnotKind::Math { .. }),
                    _ => false,
                });
                if matches_tool {
                    let origin = tab.doc.session.get(id).unwrap().kind.clone();
                    tab.select_only(id);
                    tab.editing = None;
                    tab.drag = Some(Drag::Move {
                        ids: vec![id],
                        origins: vec![(id, origin)],
                        grab: point,
                        page,
                        moved: false,
                    });
                    return;
                }
            }
            tab.drag = Some(Drag::Create {
                page,
                origin: point,
                current: point,
                kind: if tool == Tool::Math {
                    CreateKind::Math
                } else {
                    CreateKind::Text
                },
            });
        }
    }
}

fn begin_select(tab: &mut Tab, page: usize, point: PdfPoint, pos: Pos2, view: Rect) {
    let tool = Tool::Select;
    if let Some((id, handle)) = resize_target(&tab.doc, tab.primary_selected(), tool, pos, view) {
        if let Some(annot) = tab.doc.session.get(id) {
            let origin = annot.kind.clone();
            let page = annot.page;
            if !tab.is_selected(id) {
                tab.select_only(id);
            }
            tab.editing = None;
            tab.drag = Some(Drag::Resize {
                id,
                handle,
                origin,
                page,
            });
            return;
        }
    }
    if let Some(id) = tab.doc.session.hit_test(page, point, 4.0 / tab.doc.scale) {
        let origins = if tab.is_selected(id) && tab.selected.len() > 1 {
            // Dragging one of a multi-selection moves the whole set.
            tab.selected
                .iter()
                .filter_map(|sid| {
                    tab.doc
                        .session
                        .get(*sid)
                        .map(|annot| (*sid, annot.kind.clone()))
                })
                .collect::<Vec<_>>()
        } else {
            tab.select_only(id);
            tab.doc
                .session
                .get(id)
                .map(|annot| vec![(id, annot.kind.clone())])
                .unwrap_or_default()
        };
        tab.editing = None;
        tab.style_bar = None;
        if !origins.is_empty() {
            let ids = origins.iter().map(|(id, _)| *id).collect();
            tab.drag = Some(Drag::Move {
                ids,
                origins,
                grab: point,
                page,
                moved: false,
            });
        }
        return;
    }
    // Empty page: text select when starting on a glyph, otherwise marquee bulk-select.
    tab.selected.clear();
    tab.editing = None;
    tab.style_bar = None;
    tab.assistant.learning = None;
    let index = tab
        .doc
        .glyphs
        .get(&page)
        .and_then(|glyphs| glyph_at(glyphs, point));
    if index.is_some() {
        tab.drag = Some(Drag::TextSelect {
            page,
            anchor: index,
            current: index,
            origin: point,
            current_pt: point,
        });
    } else {
        tab.text_sel = None;
        tab.drag = Some(Drag::Marquee {
            page,
            origin: point,
            current: point,
        });
    }
}

fn begin_highlight(tab: &mut Tab, page: usize, point: PdfPoint) {
    tab.selected.clear();
    tab.text_sel = None;
    tab.style_bar = None;
    tab.assistant.learning = None;
    let glyphs = tab.doc.glyphs.get(&page);
    let index = glyphs.and_then(|glyphs| glyph_at(glyphs, point));
    let word = index.and_then(|i| glyphs.and_then(|g| g.get(i)).map(|g| g.word));
    let previous = tab.last_hl;
    let double = previous.is_some_and(|(when, hit_page, hit_word, id)| {
        id.is_some()
            && when.elapsed() < Duration::from_millis(420)
            && hit_page == page
            && word == Some(hit_word)
    });
    let replace = if double {
        previous.and_then(|(_, _, _, id)| id)
    } else {
        None
    };
    if let (Some(index), Some(word)) = (index, word) {
        tab.last_hl = Some((Instant::now(), page, word, None));
        let (anchor, current, word_lo, word_hi) = if double {
            if let Some(glyphs) = glyphs {
                if let Some((lo, hi)) = word_range(glyphs, index) {
                    (Some(lo), Some(hi), Some(lo), Some(hi))
                } else {
                    (Some(index), Some(index), None, None)
                }
            } else {
                (Some(index), Some(index), None, None)
            }
        } else {
            (Some(index), Some(index), None, None)
        };
        tab.drag = Some(Drag::Highlight {
            page,
            anchor,
            current,
            origin: point,
            current_pt: point,
            word_lo,
            word_hi,
            replace,
        });
        return;
    }
    tab.drag = Some(Drag::Highlight {
        page,
        anchor: None,
        current: None,
        origin: point,
        current_pt: point,
        word_lo: None,
        word_hi: None,
        replace: None,
    });
}

fn update_primary(app: &mut MarkerApp, pos: Pos2, view: Rect) {
    let drag = app.tab().and_then(|tab| tab.drag.clone());
    let Some(drag) = drag else {
        return;
    };
    match drag {
        Drag::Pan {
            scroll_x,
            scroll_y,
            pos: start,
        } => {
            if let Some(tab) = app.tab_mut() {
                tab.doc.scroll_x = scroll_x - (pos.x - start.x);
                tab.doc.scroll_y = scroll_y - (pos.y - start.y);
                tab.doc.clamp_scroll(view);
                tab.doc.last_scroll = Instant::now();
            }
        }
        Drag::Highlight {
            page,
            anchor,
            origin,
            word_lo,
            word_hi,
            replace,
            ..
        } => {
            let Some(tab) = app.tab_mut() else {
                return;
            };
            let point = tab
                .doc
                .screen_to_page(pos, view)
                .filter(|(hit, _)| *hit == page)
                .map(|(_, point)| point)
                .unwrap_or(origin);
            let current = tab
                .doc
                .glyphs
                .get(&page)
                .and_then(|glyphs| glyph_at(glyphs, point));
            tab.drag = Some(Drag::Highlight {
                page,
                anchor,
                current,
                origin,
                current_pt: point,
                word_lo,
                word_hi,
                replace,
            });
        }
        Drag::LearningSelect {
            page,
            anchor,
            origin,
            ..
        } => {
            let Some(tab) = app.tab_mut() else {
                return;
            };
            let point = tab
                .doc
                .screen_to_page(pos, view)
                .filter(|(hit, _)| *hit == page)
                .map(|(_, point)| point)
                .unwrap_or(origin);
            let current = tab
                .doc
                .glyphs
                .get(&page)
                .and_then(|glyphs| glyph_at(glyphs, point));
            tab.drag = Some(Drag::LearningSelect {
                page,
                anchor,
                current,
                origin,
                current_pt: point,
            });
        }
        Drag::TextSelect {
            page,
            anchor,
            origin,
            ..
        } => {
            let Some(tab) = app.tab_mut() else {
                return;
            };
            let point = tab
                .doc
                .screen_to_page(pos, view)
                .filter(|(hit, _)| *hit == page)
                .map(|(_, point)| point)
                .unwrap_or(origin);
            let current = tab
                .doc
                .glyphs
                .get(&page)
                .and_then(|glyphs| glyph_at(glyphs, point));
            tab.drag = Some(Drag::TextSelect {
                page,
                anchor,
                current,
                origin,
                current_pt: point,
            });
        }
        Drag::Marquee {
            page,
            origin,
            ..
        } => {
            let Some(tab) = app.tab_mut() else {
                return;
            };
            let point = tab
                .doc
                .screen_to_page(pos, view)
                .filter(|(hit, _)| *hit == page)
                .map(|(_, point)| point)
                .unwrap_or(origin);
            tab.drag = Some(Drag::Marquee {
                page,
                origin,
                current: point,
            });
        }
        Drag::Region {
            page,
            origin,
            ..
        } => {
            let Some(tab) = app.tab_mut() else {
                return;
            };
            let point = tab
                .doc
                .screen_to_page(pos, view)
                .filter(|(hit, _)| *hit == page)
                .map(|(_, point)| point)
                .unwrap_or(origin);
            tab.drag = Some(Drag::Region {
                page,
                origin,
                current: point,
            });
        }
        Drag::Shape {
            page, kind, origin, ..
        } => {
            let Some(tab) = app.tab_mut() else {
                return;
            };
            let current = tab
                .doc
                .screen_to_page(pos, view)
                .filter(|(hit, _)| *hit == page)
                .map(|(_, point)| point)
                .unwrap_or(origin);
            tab.drag = Some(Drag::Shape {
                page,
                kind,
                origin,
                current,
            });
        }
        Drag::Create {
            page, origin, kind, ..
        } => {
            let Some(tab) = app.tab_mut() else {
                return;
            };
            let current = tab
                .doc
                .screen_to_page(pos, view)
                .filter(|(hit, _)| *hit == page)
                .map(|(_, point)| point)
                .unwrap_or(origin);
            tab.drag = Some(Drag::Create {
                page,
                origin,
                current,
                kind,
            });
        }
        Drag::Move {
            origins,
            grab,
            page,
            ..
        } => {
            app.seal_then_arm();
            {
                let Some(tab) = app.tab_mut() else {
                    return;
                };
                let Some((_, point)) = tab
                    .doc
                    .screen_to_page(pos, view)
                    .filter(|(hit, _)| *hit == page)
                else {
                    return;
                };
                let dx = point.x - grab.x;
                let dy = point.y - grab.y;
                for (id, origin) in &origins {
                    if let Some(annot) = tab.doc.session.get_mut(*id) {
                        let mut kind = origin.clone();
                        kind.translate(dx, dy);
                        annot.kind = kind;
                    }
                }
            }
            if let Some(Drag::Move { moved, .. }) = app.tab_mut().and_then(|tab| tab.drag.as_mut())
            {
                *moved = true;
            }
        }
        Drag::Resize {
            id,
            handle,
            origin,
            page,
        } => {
            app.seal_then_arm();
            let Some(tab) = app.tab_mut() else {
                return;
            };
            let Some((_, point)) = tab
                .doc
                .screen_to_page(pos, view)
                .filter(|(hit, _)| *hit == page)
            else {
                return;
            };
            if let Some(annot) = tab.doc.session.get_mut(id) {
                let mut kind = origin.clone();
                kind.resize(handle, point);
                annot.kind = kind;
            }
        }
    }
}

fn end_primary(app: &mut MarkerApp, pos: Option<Pos2>, view: Rect, double: bool) {
    let drag = app.tab_mut().and_then(|tab| tab.drag.take());
    let Some(pos) = pos else {
        if let Some(drag) = drag {
            commit_drag(app, drag, view);
        }
        return;
    };
    let Some(drag) = drag else {
        click(app, pos, view, double);
        return;
    };
    if !drag_moved(&drag, app, pos, view) {
        match &drag {
            Drag::Highlight {
                anchor: Some(_), ..
            }
            | Drag::LearningSelect {
                anchor: Some(_), ..
            }
            | Drag::TextSelect {
                anchor: Some(_), ..
            } => commit_drag(app, drag, view),
            Drag::Region { .. } | Drag::Marquee { .. } => {
                // Tiny click — ignore empty crop / marquee.
            }
            Drag::Create {
                page, origin, kind, ..
            } => {
                place_box(app, *page, *origin, None, *kind);
            }
            Drag::Move { ids, .. } => {
                click(app, pos, view, double);
                if let Some(tab) = app.tab_mut() {
                    if !ids.is_empty() {
                        tab.select_many(ids.clone());
                    }
                }
            }
            _ => click(app, pos, view, double),
        }
        return;
    }
    commit_drag(app, drag, view);
}

fn drag_moved(drag: &Drag, app: &MarkerApp, pos: Pos2, view: Rect) -> bool {
    let Some(tab) = app.tab() else {
        return false;
    };
    match drag {
        Drag::Pan { pos: start, .. } => start.distance(pos) > 3.0,
        Drag::Highlight {
            page,
            origin,
            current_pt,
            ..
        } => {
            let a = tab.doc.page_to_screen(*page, *origin, view);
            let b = tab.doc.page_to_screen(*page, *current_pt, view);
            a.distance(b) > 3.0
        }
        Drag::Shape {
            page,
            origin,
            current,
            ..
        }
        | Drag::Create {
            page,
            origin,
            current,
            ..
        } => {
            let a = tab.doc.page_to_screen(*page, *origin, view);
            let b = tab.doc.page_to_screen(*page, *current, view);
            a.distance(b) > 3.0
        }
        Drag::Move { moved, .. } => *moved,
        Drag::Resize { .. } => true,
        Drag::Region {
            page,
            origin,
            current,
        } => {
            let a = tab.doc.page_to_screen(*page, *origin, view);
            let b = tab.doc.page_to_screen(*page, *current, view);
            a.distance(b) > 3.0
        }
        Drag::LearningSelect {
            page,
            origin,
            current_pt,
            ..
        }
        | Drag::TextSelect {
            page,
            origin,
            current_pt,
            ..
        } => {
            let a = tab.doc.page_to_screen(*page, *origin, view);
            let b = tab.doc.page_to_screen(*page, *current_pt, view);
            a.distance(b) > 3.0
        }
        Drag::Marquee {
            page,
            origin,
            current,
        } => {
            let a = tab.doc.page_to_screen(*page, *origin, view);
            let b = tab.doc.page_to_screen(*page, *current, view);
            a.distance(b) > 3.0
        }
    }
}

fn click(app: &mut MarkerApp, pos: Pos2, view: Rect, double: bool) {
    // True clicks never start a drag (`Sense::click_and_drag` waits for movement), so
    // text / highlight / learning selection must be handled here — including double-click
    // whole-word selection (#34).
    if app.capture == CaptureMode::LearningText {
        let Some((page, point)) = app.tab().and_then(|tab| tab.doc.screen_to_page(pos, view))
        else {
            return;
        };
        select_glyphs_at_point(app, page, point, double, GlyphClick::Learning);
        return;
    }
    if app.capture == CaptureMode::Region {
        return;
    }

    let tool = app.tool;
    let located = app.tab().and_then(|tab| tab.doc.screen_to_page(pos, view));
    let Some((page, point)) = located else {
        app.end_edit_undo();
        app.clear_page_selection();
        return;
    };
    let slop = app.tab().map(|tab| 4.0 / tab.doc.scale).unwrap_or(4.0);
    let hit = app
        .tab()
        .and_then(|tab| tab.doc.session.hit_test(page, point, slop));
    match tool {
        Tool::Select => {
            if let Some(id) = hit {
                let open = double
                    || app.tab().is_some_and(|tab| {
                        matches!(
                            tab.doc.session.get(id).map(|annot| &annot.kind),
                            Some(AnnotKind::Note { .. })
                        )
                    });
                let editable = app.tab().is_some_and(|tab| is_editable(tab, id));
                if open && editable {
                    app.begin_edit_undo(id);
                } else if app.tab().is_some_and(|tab| tab.editing.is_some()) {
                    app.end_edit_undo();
                }
                if let Some(tab) = app.tab_mut() {
                    tab.select_only(id);
                    tab.assistant.learning = None;
                    tab.style_bar = None;
                    if open && editable {
                        tab.editing = Some(id);
                        tab.focus_edit = true;
                    } else {
                        tab.editing = None;
                    }
                }
            } else if double
                && select_glyphs_at_point(app, page, point, true, GlyphClick::TextSelect)
            {
                app.end_edit_undo();
            } else {
                app.end_edit_undo();
                app.clear_page_selection();
            }
        }
        Tool::Highlight => {
            if select_glyphs_at_point(app, page, point, double, GlyphClick::Highlight) {
                // Highlight created (single glyph, or whole word on double-click).
            } else if hit.is_none() {
                app.clear_page_selection();
            }
        }
        Tool::Text => {
            let text = hit.filter(|id| {
                app.tab().is_some_and(|tab| {
                    matches!(
                        tab.doc.session.get(*id).map(|annot| &annot.kind),
                        Some(AnnotKind::Text { .. })
                    )
                })
            });
            if let Some(id) = text {
                app.begin_edit_undo(id);
                if let Some(tab) = app.tab_mut() {
                    tab.select_only(id);
                    tab.assistant.learning = None;
                    tab.editing = Some(id);
                    tab.focus_edit = true;
                }
            } else {
                if hit.is_none() {
                    app.clear_page_selection();
                }
                place_box(app, page, point, None, CreateKind::Text);
            }
        }
        Tool::Math => {
            let math = hit.filter(|id| {
                app.tab().is_some_and(|tab| {
                    matches!(
                        tab.doc.session.get(*id).map(|annot| &annot.kind),
                        Some(AnnotKind::Math { .. })
                    )
                })
            });
            if let Some(id) = math {
                app.begin_edit_undo(id);
                if let Some(tab) = app.tab_mut() {
                    tab.select_only(id);
                    tab.assistant.learning = None;
                    tab.editing = Some(id);
                    tab.focus_edit = true;
                }
            } else {
                if hit.is_none() {
                    app.clear_page_selection();
                }
                place_box(app, page, point, None, CreateKind::Math);
            }
        }
        Tool::Rect | Tool::Ellipse | Tool::Line => {
            if hit.is_none() {
                app.clear_page_selection();
            }
        }
    }
}

fn is_editable(tab: &Tab, id: u64) -> bool {
    matches!(
        tab.doc.session.get(id).map(|annot| &annot.kind),
        Some(AnnotKind::Text { .. } | AnnotKind::Note { .. } | AnnotKind::Math { .. })
    )
}

#[derive(Clone, Copy)]
enum GlyphClick {
    TextSelect,
    Highlight,
    Learning,
}

/// Select / highlight glyphs under a click. Double-click expands to the whole word.
/// Returns false when no glyph is under the point.
fn select_glyphs_at_point(
    app: &mut MarkerApp,
    page: usize,
    point: PdfPoint,
    whole_word: bool,
    kind: GlyphClick,
) -> bool {
    let (lo, hi, word) = {
        let Some(tab) = app.tab() else {
            return false;
        };
        let Some(glyphs) = tab.doc.glyphs.get(&page) else {
            return false;
        };
        let Some(index) = glyph_at(glyphs, point) else {
            return false;
        };
        let Some((lo, hi)) = click_glyph_range(glyphs, index, whole_word) else {
            return false;
        };
        let word = glyphs.get(index).map(|g| g.word);
        (lo, hi, word)
    };

    match kind {
        GlyphClick::TextSelect => {
            let Some(tab) = app.tab_mut() else {
                return false;
            };
            tab.selected.clear();
            tab.editing = None;
            tab.style_bar = None;
            tab.assistant.learning = None;
            tab.text_sel = Some(TextSel {
                page,
                glyph_lo: lo,
                glyph_hi: hi,
            });
            true
        }
        GlyphClick::Learning => {
            let Some(tab) = app.tab_mut() else {
                return false;
            };
            tab.assistant.learning = Some(LearningSelection {
                page,
                glyph_lo: lo,
                glyph_hi: hi,
            });
            app.capture = CaptureMode::None;
            app.assistant_open = true;
            app.attach_learning_text();
            true
        }
        GlyphClick::Highlight => {
            let color = app.settings.highlight_color;
            let replace = {
                let Some(tab) = app.tab() else {
                    return false;
                };
                if !whole_word {
                    None
                } else {
                    tab.last_hl.and_then(|(when, hit_page, hit_word, id)| {
                        (when.elapsed() < Duration::from_millis(420)
                            && hit_page == page
                            && word == Some(hit_word))
                        .then_some(id)
                        .flatten()
                    })
                }
            };
            let quads = {
                let Some(tab) = app.tab() else {
                    return false;
                };
                let Some(glyphs) = tab.doc.glyphs.get(&page) else {
                    return false;
                };
                highlight_quads(glyphs, lo, hi)
            };
            if quads.is_empty() {
                return false;
            }
            app.seal_then_arm();
            let Some(tab) = app.tab_mut() else {
                return false;
            };
            if let Some(old) = replace {
                tab.doc.session.remove(old);
            }
            let id = tab
                .doc
                .session
                .insert(page, AnnotKind::Highlight { quads, color });
            tab.select_only(id);
            if let Some(w) = word {
                // Remember single-glyph marks so a quick second click can upgrade to the word.
                let remembered = (!whole_word).then_some(id);
                tab.last_hl = Some((Instant::now(), page, w, remembered));
            }
            if !matches!(tab.save, crate::app::SaveState::Saving) {
                tab.save = crate::app::SaveState::Dirty {
                    since: Instant::now(),
                };
            }
            app.seal_undo();
            app.open_style_bar(id, true);
            true
        }
    }
}

fn commit_drag(app: &mut MarkerApp, drag: Drag, _view: Rect) {
    match drag {
        Drag::LearningSelect {
            page,
            anchor,
            current,
            ..
        } => {
            let Some(tab) = app.tab_mut() else {
                return;
            };
            if let (Some(a), Some(c)) = (anchor, current) {
                tab.assistant.learning = Some(LearningSelection {
                    page,
                    glyph_lo: a.min(c),
                    glyph_hi: a.max(c),
                });
                app.capture = CaptureMode::None;
                app.assistant_open = true;
                app.attach_learning_text();
            }
        }
        Drag::TextSelect {
            page,
            anchor,
            current,
            ..
        } => {
            let Some(tab) = app.tab_mut() else {
                return;
            };
            if let (Some(a), Some(c)) = (anchor, current) {
                tab.selected.clear();
                tab.text_sel = Some(TextSel {
                    page,
                    glyph_lo: a.min(c),
                    glyph_hi: a.max(c),
                });
            }
        }
        Drag::Marquee {
            page,
            origin,
            current,
        } => {
            let rect = PdfRect::from_points(origin, current);
            if rect.is_empty() {
                return;
            }
            let style_id = {
                let Some(tab) = app.tab_mut() else {
                    return;
                };
                let ids = tab.doc.session.ids_centered_in(page, rect);
                tab.text_sel = None;
                tab.select_many(ids);
                // Prefer anchoring the style strip on a colorable item; images still
                // get a delete-only strip when they are the whole selection.
                tab.selected
                    .iter()
                    .copied()
                    .find(|&id| {
                        tab.doc.session.get(id).is_some_and(|annot| {
                            !matches!(annot.kind, AnnotKind::Image { .. } | AnnotKind::Future(_))
                        })
                    })
                    .or_else(|| tab.primary_selected())
            };
            if let Some(id) = style_id {
                app.open_style_bar(id, false);
            }
        }
        Drag::Region {
            page,
            origin,
            current,
        } => {
            let rect = PdfRect::from_points(origin, current);
            if rect.is_empty() {
                return;
            }
            app.capture = CaptureMode::None;
            app.assistant_open = true;
            app.request_crop(page, rect);
        }
        Drag::Highlight {
            page,
            anchor,
            current,
            origin,
            current_pt,
            word_lo,
            word_hi,
            replace,
        } => {
            let color = app.settings.highlight_color;
            let Some(tab) = app.tab_mut() else {
                return;
            };
            let quads = {
                let range = match (anchor, current, word_lo, word_hi) {
                    (Some(a), Some(c), Some(wlo), Some(whi)) => {
                        Some((a.min(c).min(wlo), a.max(c).max(whi)))
                    }
                    (Some(a), Some(c), _, _) => Some((a.min(c), a.max(c))),
                    _ => None,
                };
                if let (Some((lo, hi)), Some(glyphs)) = (range, tab.doc.glyphs.get(&page)) {
                    highlight_quads(glyphs, lo, hi)
                } else {
                    let rect = PdfRect::from_points(origin, current_pt);
                    if rect.is_empty() {
                        Vec::new()
                    } else {
                        vec![rect]
                    }
                }
            };
            if quads.is_empty() {
                return;
            }
            app.seal_then_arm();
            let Some(tab) = app.tab_mut() else {
                return;
            };
            if let Some(old) = replace {
                tab.doc.session.remove(old);
            }
            let id = tab
                .doc
                .session
                .insert(page, AnnotKind::Highlight { quads, color });
            tab.select_only(id);
            if let Some(glyphs) = tab.doc.glyphs.get(&page) {
                if let Some(index) = anchor.or(current) {
                    if let Some(glyph) = glyphs.get(index) {
                        let remembered = (anchor == current).then_some(id);
                        tab.last_hl = Some((Instant::now(), page, glyph.word, remembered));
                    }
                }
            }
            if !matches!(tab.save, crate::app::SaveState::Saving) {
                tab.save = crate::app::SaveState::Dirty {
                    since: Instant::now(),
                };
            }
            app.seal_undo();
            app.open_style_bar(id, true);
        }
        Drag::Shape {
            page,
            kind,
            origin,
            current,
        } => {
            let rect = PdfRect::from_points(origin, current);
            let long_enough = (origin.x - current.x).hypot(origin.y - current.y) >= 4.0;
            if kind != ShapeKind::Line && rect.is_empty() {
                return;
            }
            if kind == ShapeKind::Line && !long_enough {
                return;
            }
            let stroke = app.settings.shape_color;
            let width = app.settings.shape_width;
            app.seal_then_arm();
            let Some(tab) = app.tab_mut() else {
                return;
            };
            let id = tab.doc.session.insert(
                page,
                AnnotKind::Shape {
                    kind,
                    rect,
                    start: origin,
                    end: current,
                    stroke,
                    fill: None,
                    width,
                },
            );
            tab.select_only(id);
            if !matches!(tab.save, crate::app::SaveState::Saving) {
                tab.save = crate::app::SaveState::Dirty {
                    since: Instant::now(),
                };
            }
            app.seal_undo();
            app.open_style_bar(id, true);
        }
        Drag::Create {
            page,
            origin,
            current,
            kind,
        } => {
            let rect = PdfRect::from_points(origin, current);
            let sized = rect.width() >= 12.0 && rect.height() >= 8.0;
            place_box(app, page, origin, sized.then_some(rect), kind);
        }
        Drag::Move {
            ids,
            origins,
            moved,
            ..
        } => {
            if moved {
                let mut dirty = Vec::new();
                if let Some(tab) = app.tab_mut() {
                    for (id, origin) in &origins {
                        if tab
                            .doc
                            .session
                            .get(*id)
                            .is_some_and(|annot| annot.kind != *origin)
                        {
                            tab.doc.session.mark_dirty(*id);
                            dirty.push(*id);
                        }
                    }
                    if !dirty.is_empty() && !matches!(tab.save, crate::app::SaveState::Saving) {
                        tab.save = crate::app::SaveState::Dirty {
                            since: Instant::now(),
                        };
                    }
                }
                for id in dirty {
                    app.queue_math(id);
                }
            }
            if let Some(tab) = app.tab_mut() {
                tab.select_many(ids);
            }
            app.seal_undo();
        }
        Drag::Resize { id, .. } => {
            if let Some(tab) = app.tab_mut() {
                tab.doc.session.mark_dirty(id);
                if !tab.is_selected(id) {
                    tab.select_only(id);
                }
                if !matches!(tab.save, crate::app::SaveState::Saving) {
                    tab.save = crate::app::SaveState::Dirty {
                        since: Instant::now(),
                    };
                }
            }
            app.queue_math(id);
            app.seal_undo();
        }
        Drag::Pan { .. } => {}
    }
}

fn place_box(
    app: &mut MarkerApp,
    page: usize,
    point: PdfPoint,
    rect: Option<PdfRect>,
    kind: CreateKind,
) {
    let settings = app.settings.clone();
    app.seal_then_arm();
    let id = {
        let Some(tab) = app.tab_mut() else {
            return;
        };
        place_box_tab(tab, page, point, rect, kind, &settings);
        tab.primary_selected()
    };
    if let Some(id) = id {
        app.tag_undo_edit(id);
        if kind == CreateKind::Math {
            app.queue_math(id);
        }
        app.open_style_bar(id, true);
    }
}

fn place_box_tab(
    tab: &mut Tab,
    page: usize,
    point: PdfPoint,
    rect: Option<PdfRect>,
    kind: CreateKind,
    settings: &crate::settings::Settings,
) {
    let size = settings.text_size;
    let color = settings.text_color;
    let rect = rect.unwrap_or_else(|| match kind {
        CreateKind::Text => PdfRect::new(
            point.x,
            point.y,
            point.x + 200.0,
            point.y + size * 1.8 + 4.0,
        ),
        CreateKind::Math => {
            PdfRect::new(point.x, point.y, point.x + 88.0, point.y + size * 1.6 + 8.0)
        }
    });
    let id = match kind {
        CreateKind::Text => tab.doc.session.insert(
            page,
            AnnotKind::Text {
                rect,
                content: String::new(),
                size,
                color,
            },
        ),
        CreateKind::Math => tab.doc.session.insert(
            page,
            AnnotKind::Math {
                rect,
                source: String::new(),
                size,
                color,
                auto_size: true,
            },
        ),
    };
    tab.select_only(id);
    tab.editing = Some(id);
    tab.focus_edit = true;
    if !matches!(tab.save, crate::app::SaveState::Saving) {
        tab.save = crate::app::SaveState::Dirty {
            since: Instant::now(),
        };
    }
}

fn handle_at(doc: &DocState, id: u64, screen: Pos2, view: Rect, radius: f32) -> Option<Handle> {
    let annot = doc.session.get(id)?;
    annot
        .kind
        .handles()
        .into_iter()
        .find_map(|(handle, point)| {
            let at = doc.page_to_screen(annot.page, point, view);
            (at.distance(screen) <= radius).then_some(handle)
        })
}

fn resize_target(
    doc: &DocState,
    selected: Option<u64>,
    tool: Tool,
    pos: Pos2,
    view: Rect,
) -> Option<(u64, Handle)> {
    let allows = |kind: &AnnotKind| match tool {
        Tool::Text => matches!(kind, AnnotKind::Text { .. }),
        Tool::Math => matches!(kind, AnnotKind::Math { .. }),
        Tool::Select => matches!(
            kind,
            AnnotKind::Text { .. }
                | AnnotKind::Math { .. }
                | AnnotKind::Image { .. }
                | AnnotKind::Shape { .. }
        ),
        _ => false,
    };
    let radius = 16.0;
    if let Some(id) = selected {
        if doc.session.get(id).is_some_and(|annot| allows(&annot.kind)) {
            if let Some(handle) = handle_at(doc, id, pos, view, radius) {
                return Some((id, handle));
            }
        }
    }
    let (page, _) = doc.screen_to_page(pos, view)?;
    for annot in doc.session.annotations.iter().rev() {
        if annot.page != page || !allows(&annot.kind) {
            continue;
        }
        if let Some(handle) = handle_at(doc, annot.id, pos, view, radius) {
            return Some((annot.id, handle));
        }
    }
    None
}

fn paint_document(app: &MarkerApp, painter: &egui::Painter, view: Rect, pixels_per_point: f32) {
    let Some(tab) = app.tab() else {
        return;
    };
    let render_scale = tab.doc.render_scale(pixels_per_point);
    let (first, last) = visible_pages(&tab.doc, view);
    for page in first..=last {
        let rect = tab.doc.page_rect(page, view);
        let shadow = rect.expand(2.0).translate(Vec2::new(0.0, 4.0));
        painter.rect_filled(shadow, 6.0, Color32::from_black_alpha(28));
        painter.rect_filled(rect, 1.0, Color32::WHITE);
        // Highlights underpaint paper so glyphs (opaque tile ink) stay readable.
        paint_highlight_fills(app, painter, page, view);
        paint_tiles(&tab.doc, painter, page, view, render_scale, pixels_per_point);
        paint_search_hits(tab, painter, page, view);
        paint_annotations(app, painter, page, view);
    }
    paint_drag_preview(app, painter, view);
    paint_learning_selection(app, painter, view);
}

/// Marker-tint fill used for on-screen highlights (matches PDF opacity ~0.45).
fn highlight_fill(color: crate::geom::Rgb) -> Color32 {
    let c = color.to_color32();
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 115)
}

fn paint_highlight_fills(app: &MarkerApp, painter: &egui::Painter, page: usize, view: Rect) {
    let Some(tab) = app.tab() else {
        return;
    };
    let page_rect = tab.doc.page_rect(page, view);
    let painter = painter.with_clip_rect(page_rect.intersect(view));
    for annot in tab
        .doc
        .session
        .annotations
        .iter()
        .filter(|annot| annot.page == page)
    {
        if let AnnotKind::Highlight { quads, color } = &annot.kind {
            let fill = highlight_fill(*color);
            for quad in quads {
                painter.rect_filled(pdf_rect_screen(&tab.doc, page, *quad, view), 1.0, fill);
            }
        }
    }
    // Drag preview for the highlight tool also belongs under the ink.
    if let Some(Drag::Highlight {
        page: drag_page,
        anchor,
        current,
        origin,
        current_pt,
        word_lo,
        word_hi,
        ..
    }) = &tab.drag
    {
        if *drag_page != page {
            return;
        }
        let fill = highlight_fill(app.settings.highlight_color);
        let range = match (anchor, current, word_lo, word_hi) {
            (Some(a), Some(c), Some(wlo), Some(whi)) => {
                Some(((*a).min(*c).min(*wlo), (*a).max(*c).max(*whi)))
            }
            (Some(a), Some(c), _, _) => Some(((*a).min(*c), (*a).max(*c))),
            _ => None,
        };
        if let (Some((lo, hi)), Some(glyphs)) = (range, tab.doc.glyphs.get(&page)) {
            for rect in highlight_quads(glyphs, lo, hi) {
                painter.rect_filled(pdf_rect_screen(&tab.doc, page, rect, view), 1.0, fill);
            }
        } else {
            let rect = PdfRect::from_points(*origin, *current_pt);
            painter.rect_filled(pdf_rect_screen(&tab.doc, page, rect, view), 1.0, fill);
        }
    }
}

fn visible_pages(doc: &DocState, view: Rect) -> (usize, usize) {
    if doc.pages.is_empty() {
        return (0, 0);
    }
    let mut first = None;
    let mut last = 0;
    let top = doc.scroll_y - view.height();
    let bottom = doc.scroll_y + view.height() * 2.0;
    for index in 0..doc.pages.len() {
        let y0 = doc.tops[index] * doc.scale;
        let y1 = y0 + doc.pages[index].height() * doc.scale;
        if y1 >= top && y0 <= bottom {
            first.get_or_insert(index);
            last = index;
        }
    }
    (first.unwrap_or(0), last)
}

fn paint_tiles(
    doc: &DocState,
    painter: &egui::Painter,
    page: usize,
    view: Rect,
    render_scale: f32,
    pixels_per_point: f32,
) {
    let target_bits = render_scale.to_bits();
    let page_tiles: Vec<_> = doc
        .tiles
        .iter()
        .filter(|(key, _)| key.page == page)
        .collect();
    // Prefer the current zoom bucket; only fall back to other scales while
    // waiting for matching tiles (avoids lasting mixed font appearance).
    let has_target = page_tiles
        .iter()
        .any(|(key, _)| key.scale_bits == target_bits);
    let mut tiles: Vec<_> = page_tiles
        .into_iter()
        .filter(|(key, _)| !has_target || key.scale_bits == target_bits)
        .collect();
    tiles.sort_by_key(|(key, _)| (key.scale_bits == target_bits, key.scale_bits));
    for (_key, tile) in tiles {
        let x0 = tile.x as f32 / tile.scale;
        let y0 = tile.y as f32 / tile.scale;
        let x1 = (tile.x as f32 + tile.w as f32) / tile.scale;
        let y1 = (tile.y as f32 + tile.h as f32) / tile.scale;
        let min = doc.page_to_screen(page, PdfPoint::new(x0, y0), view);
        let max = doc.page_to_screen(page, PdfPoint::new(x1, y1), view);
        let dest = snap_rect_to_pixels(Rect::from_min_max(min, max), pixels_per_point);
        if dest.intersects(view) {
            painter.image(
                tile.texture.id(),
                dest,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }
}

/// Align tile quads to the physical pixel grid to avoid subpixel resampling blur.
fn snap_rect_to_pixels(rect: Rect, pixels_per_point: f32) -> Rect {
    let ppp = pixels_per_point.max(0.5);
    let snap = |v: f32| (v * ppp).round() / ppp;
    Rect::from_min_max(
        Pos2::new(snap(rect.min.x), snap(rect.min.y)),
        Pos2::new(snap(rect.max.x), snap(rect.max.y)),
    )
}

fn paint_search_hits(tab: &Tab, painter: &egui::Painter, page: usize, view: Rect) {
    if !tab.search.open && tab.search.hits.is_empty() {
        return;
    }
    let current = tab.search.hits.get(tab.search.current);
    for (index, (hit_page, quads)) in tab.search.hits.iter().enumerate() {
        if *hit_page != page {
            continue;
        }
        let active = current.is_some_and(|(p, _)| *p == page) && index == tab.search.current;
        let fill = if active {
            Color32::from_rgba_unmultiplied(255, 160, 40, 110)
        } else {
            Color32::from_rgba_unmultiplied(255, 214, 80, 70)
        };
        for quad in quads {
            painter.rect_filled(pdf_rect_screen(&tab.doc, page, *quad, view), 1.0, fill);
        }
    }
}

fn paint_annotations(app: &MarkerApp, painter: &egui::Painter, page: usize, view: Rect) {
    let Some(tab) = app.tab() else {
        return;
    };
    let page_rect = tab.doc.page_rect(page, view);
    let painter = painter.with_clip_rect(page_rect.intersect(view));
    for annot in tab
        .doc
        .session
        .annotations
        .iter()
        .filter(|annot| annot.page == page)
    {
        let selected = tab.is_selected(annot.id);
        let editing = tab.editing == Some(annot.id);
        match &annot.kind {
            // Highlight fills are underpainted before tiles; only selection chrome remains here.
            AnnotKind::Highlight { .. } => {}
            AnnotKind::Text {
                rect,
                content,
                size,
                color,
            } => {
                if !editing {
                    let screen = pdf_rect_screen(&tab.doc, page, *rect, view);
                    let text_painter = painter.with_clip_rect(screen);
                    paint_wrapped(
                        &text_painter,
                        painter.ctx(),
                        screen,
                        content,
                        *size * tab.doc.scale,
                        color.to_color32(),
                    );
                }
            }
            AnnotKind::Note { rect, color, .. } => {
                let screen = pdf_rect_screen(&tab.doc, page, *rect, view);
                painter.rect_filled(screen, 4.0, color.to_color32());
                painter.text(
                    screen.center(),
                    egui::Align2::CENTER_CENTER,
                    "✎",
                    FontId::new(screen.height() * 0.55, FontFamily::Proportional),
                    Color32::from_rgb(40, 40, 40),
                );
            }
            AnnotKind::Shape {
                kind,
                rect,
                start,
                end,
                stroke,
                fill,
                width,
            } => {
                let color = stroke.to_color32();
                let stroke = Stroke::new((*width * tab.doc.scale).max(1.0), color);
                match kind {
                    ShapeKind::Rect => {
                        let screen = pdf_rect_screen(&tab.doc, page, *rect, view);
                        if let Some(fill) = fill {
                            painter.rect_filled(
                                screen,
                                0.0,
                                fill.to_color32().gamma_multiply(0.25),
                            );
                        }
                        painter.rect_stroke(screen, 0.0, stroke, egui::StrokeKind::Inside);
                    }
                    ShapeKind::Ellipse => {
                        let screen = pdf_rect_screen(&tab.doc, page, *rect, view);
                        if let Some(fill) = fill {
                            painter.add(egui::Shape::ellipse_filled(
                                screen.center(),
                                screen.size() * 0.5,
                                fill.to_color32().gamma_multiply(0.25),
                            ));
                        }
                        painter.add(egui::Shape::ellipse_stroke(
                            screen.center(),
                            screen.size() * 0.5,
                            stroke,
                        ));
                    }
                    ShapeKind::Line => {
                        let a = tab.doc.page_to_screen(page, *start, view);
                        let b = tab.doc.page_to_screen(page, *end, view);
                        painter.line_segment([a, b], stroke);
                    }
                }
            }
            AnnotKind::Math { rect, source, .. } => {
                let screen = pdf_rect_screen(&tab.doc, page, *rect, view);
                if let Some(preview) = tab.previews.get(&annot.id) {
                    if let Some(texture) = preview.texture.as_ref() {
                        let dest =
                            fit_math(screen, preview.width_pt, preview.height_pt, tab.doc.scale);
                        painter.image(
                            texture.id(),
                            dest,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    } else if !source.is_empty() && !editing {
                        paint_wrapped(
                            &painter,
                            painter.ctx(),
                            screen,
                            source,
                            13.0,
                            Color32::from_rgb(40, 40, 40),
                        );
                    }
                } else if !source.is_empty() && !editing {
                    paint_wrapped(
                        &painter,
                        painter.ctx(),
                        screen,
                        source,
                        13.0,
                        Color32::from_rgb(40, 40, 40),
                    );
                }
            }
            AnnotKind::Image { rect, .. } => {
                let screen = pdf_rect_screen(&tab.doc, page, *rect, view);
                if let Some((_, texture)) = tab.image_textures.get(&annot.id) {
                    painter.image(
                        texture.id(),
                        screen,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
            }
            AnnotKind::Future(_) => {}
        }
        if selected {
            if let Some(bounds) = annot.bounds() {
                let screen = pdf_rect_screen(&tab.doc, page, bounds, view);
                painter.rect_stroke(
                    screen,
                    1.0,
                    Stroke::new(1.0, Color32::from_rgb(70, 130, 220)),
                    egui::StrokeKind::Outside,
                );
                for (_, point) in annot.kind.handles() {
                    let at = tab.doc.page_to_screen(page, point, view);
                    let handle = Rect::from_center_size(at, Vec2::splat(8.0));
                    painter.rect_filled(handle, 1.0, Color32::WHITE);
                    painter.rect_stroke(
                        handle,
                        1.0,
                        Stroke::new(1.0, Color32::from_rgb(70, 130, 220)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
    }
}

fn fit_math(box_rect: Rect, nat_w: f32, nat_h: f32, scale: f32) -> Rect {
    if nat_w <= 1.0 || nat_h <= 1.0 {
        return box_rect;
    }
    let nat = Vec2::new(nat_w * scale, nat_h * scale);
    let fit = (box_rect.width() / nat.x).min(box_rect.height() / nat.y);
    let size = nat * fit.max(0.01);
    Rect::from_min_size(box_rect.min, size)
}

fn paint_learning_selection(app: &MarkerApp, painter: &egui::Painter, view: Rect) {
    let Some(tab) = app.tab() else {
        return;
    };
    // Skip while actively dragging a new learning / text select.
    if matches!(
        tab.drag,
        Some(Drag::LearningSelect { .. } | Drag::TextSelect { .. })
    ) {
        return;
    }
    let fill = Color32::from_rgba_unmultiplied(80, 160, 255, 56);
    if let Some(sel) = &tab.text_sel {
        if let Some(glyphs) = tab.doc.glyphs.get(&sel.page) {
            for rect in highlight_quads(glyphs, sel.glyph_lo, sel.glyph_hi) {
                painter.rect_filled(pdf_rect_screen(&tab.doc, sel.page, rect, view), 1.0, fill);
            }
        }
    }
    let Some(sel) = &tab.assistant.learning else {
        return;
    };
    let Some(glyphs) = tab.doc.glyphs.get(&sel.page) else {
        return;
    };
    for rect in highlight_quads(glyphs, sel.glyph_lo, sel.glyph_hi) {
        painter.rect_filled(pdf_rect_screen(&tab.doc, sel.page, rect, view), 1.0, fill);
    }
}

fn paint_drag_preview(app: &MarkerApp, painter: &egui::Painter, view: Rect) {
    let Some(tab) = app.tab() else {
        return;
    };
    match &tab.drag {
        Some(Drag::LearningSelect {
            page,
            anchor,
            current,
            origin,
            current_pt,
        })
        | Some(Drag::TextSelect {
            page,
            anchor,
            current,
            origin,
            current_pt,
        }) => {
            let fill = Color32::from_rgba_unmultiplied(80, 160, 255, 72);
            if let (Some(a), Some(c), Some(glyphs)) = (*anchor, *current, tab.doc.glyphs.get(page))
            {
                for rect in highlight_quads(glyphs, a.min(c), a.max(c)) {
                    painter.rect_filled(pdf_rect_screen(&tab.doc, *page, rect, view), 1.0, fill);
                }
            } else {
                let rect = PdfRect::from_points(*origin, *current_pt);
                painter.rect_filled(pdf_rect_screen(&tab.doc, *page, rect, view), 1.0, fill);
            }
        }
        Some(Drag::Region {
            page,
            origin,
            current,
        })
        | Some(Drag::Marquee {
            page,
            origin,
            current,
        }) => {
            let stroke = Stroke::new(1.5, Color32::from_rgb(80, 160, 255));
            let rect = pdf_rect_screen(
                &tab.doc,
                *page,
                PdfRect::from_points(*origin, *current),
                view,
            );
            painter.rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
            painter.rect_filled(
                rect,
                0.0,
                Color32::from_rgba_unmultiplied(80, 160, 255, 40),
            );
        }
        Some(Drag::Highlight { .. }) => {
            // Filled in `paint_highlight_fills` under the page tiles.
        }
        Some(Drag::Shape {
            page,
            kind,
            origin,
            current,
        }) => {
            let stroke = Stroke::new(
                (app.settings.shape_width * tab.doc.scale).max(1.0),
                app.settings.shape_color.to_color32(),
            );
            match kind {
                ShapeKind::Line => {
                    let a = tab.doc.page_to_screen(*page, *origin, view);
                    let b = tab.doc.page_to_screen(*page, *current, view);
                    painter.line_segment([a, b], stroke);
                }
                ShapeKind::Rect => {
                    let rect = pdf_rect_screen(
                        &tab.doc,
                        *page,
                        PdfRect::from_points(*origin, *current),
                        view,
                    );
                    painter.rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
                }
                ShapeKind::Ellipse => {
                    let rect = pdf_rect_screen(
                        &tab.doc,
                        *page,
                        PdfRect::from_points(*origin, *current),
                        view,
                    );
                    painter.add(egui::Shape::ellipse_stroke(
                        rect.center(),
                        rect.size() * 0.5,
                        stroke,
                    ));
                }
            }
        }
        Some(Drag::Create {
            page,
            origin,
            current,
            ..
        }) => {
            let rect = pdf_rect_screen(
                &tab.doc,
                *page,
                PdfRect::from_points(*origin, *current),
                view,
            );
            painter.rect_stroke(
                rect,
                1.0,
                Stroke::new(1.0, Color32::from_rgb(70, 130, 220)),
                egui::StrokeKind::Inside,
            );
        }
        _ => {}
    }
}

fn pdf_rect_screen(doc: &DocState, page: usize, rect: PdfRect, view: Rect) -> Rect {
    let min = doc.page_to_screen(page, PdfPoint::new(rect.x0, rect.y0), view);
    let max = doc.page_to_screen(page, PdfPoint::new(rect.x1, rect.y1), view);
    Rect::from_min_max(min, max)
}

fn paint_wrapped(
    painter: &egui::Painter,
    ctx: &egui::Context,
    rect: Rect,
    text: &str,
    size_px: f32,
    color: Color32,
) {
    if text.is_empty() || rect.width() < 2.0 {
        return;
    }
    let galley = ctx.fonts_mut(|fonts| {
        fonts.layout(
            text.to_owned(),
            FontId::new(size_px.max(1.0), FontFamily::Proportional),
            color,
            rect.width(),
        )
    });
    painter.galley(rect.min, galley, color);
}

fn inline_editors(app: &mut MarkerApp, ctx: &egui::Context, view: Rect) {
    let snapshot = {
        let Some(tab) = app.tab() else {
            return;
        };
        let Some(id) = tab.editing else {
            return;
        };
        let Some(annot) = tab.doc.session.get(id) else {
            return;
        };
        (
            id,
            annot.page,
            tab.doc.gen,
            tab.doc.scale,
            tab.focus_edit,
            tab.previews
                .get(&id)
                .and_then(|preview| preview.error.clone()),
            annot.kind.clone(),
        )
    };
    let (id, page, gen, scale, focus, error, kind) = snapshot;
    match kind {
        AnnotKind::Text {
            rect, size, color, ..
        } => {
            let screen = {
                let Some(tab) = app.tab() else {
                    return;
                };
                pdf_rect_screen(&tab.doc, page, rect, view)
            };
            let mut changed = false;
            let mut height = screen.height();
            egui::Area::new(Id::new(("marker-text", gen, id)))
                .order(egui::Order::Foreground)
                .fixed_pos(screen.min)
                .constrain(false)
                .show(ctx, |ui| {
                    ui.set_max_width(screen.width().max(24.0));
                    let Some(tab) = app.tab_mut() else {
                        return;
                    };
                    let Some(AnnotKind::Text { content, .. }) =
                        tab.doc.session.get_mut(id).map(|annot| &mut annot.kind)
                    else {
                        return;
                    };
                    let response = ui.add(
                        TextEdit::multiline(content)
                            .font(FontId::new(
                                (size * scale).max(8.0),
                                FontFamily::Proportional,
                            ))
                            .text_color(color.to_color32())
                            .desired_width(screen.width().max(24.0))
                            .desired_rows(1)
                            .frame(false)
                            .margin(egui::Margin::ZERO),
                    );
                    if focus {
                        response.request_focus();
                    }
                    changed = response.changed();
                    height = response.rect.height().max(size * scale);
                });
            if let Some(tab) = app.tab_mut() {
                tab.focus_edit = false;
                if let Some(AnnotKind::Text { rect, size, .. }) =
                    tab.doc.session.get_mut(id).map(|annot| &mut annot.kind)
                {
                    let min_h = (height / scale).max(*size * 1.35);
                    if rect.height() + 0.5 < min_h {
                        rect.y1 = rect.y0 + min_h;
                        changed = true;
                    }
                }
                if changed {
                    tab.doc.session.mark_dirty(id);
                    if !matches!(tab.save, crate::app::SaveState::Saving) {
                        tab.save = crate::app::SaveState::Dirty {
                            since: Instant::now(),
                        };
                    }
                }
            }
        }
        AnnotKind::Math { rect, .. } => {
            let screen = {
                let Some(tab) = app.tab() else {
                    return;
                };
                pdf_rect_screen(&tab.doc, page, rect, view)
            };
            let mut changed = false;
            let mut cycled = false;
            egui::Area::new(Id::new(("marker-math", gen, id)))
                .order(egui::Order::Foreground)
                .fixed_pos(Pos2::new(screen.min.x, screen.max.y + 4.0))
                .constrain(false)
                .show(ctx, |ui| {
                    ui.set_max_width(screen.width().max(180.0).max(280.0));
                    let Some(tab) = app.tab_mut() else {
                        return;
                    };
                    let Some(AnnotKind::Math { source, .. }) =
                        tab.doc.session.get_mut(id).map(|annot| &mut annot.kind)
                    else {
                        return;
                    };
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("LaTeX").weak().size(11.0));
                        ui.label(
                            egui::RichText::new("Tab / Shift+Tab cycle templates · math mode")
                                .weak()
                                .size(10.0),
                        );
                    });
                    let response = ui.add(
                        TextEdit::multiline(source)
                            .font(FontId::new(13.0, FontFamily::Monospace))
                            .desired_width(screen.width().max(280.0))
                            .desired_rows(3)
                            .hint_text(r"\frac{1}{2}  or  Tab for templates"),
                    );
                    if focus {
                        response.request_focus();
                    }
                    changed = response.changed();
                    if response.has_focus() {
                        // While editing math, Tab / Shift+Tab cycle presets (not focus).
                        // Ctrl+Tab is left alone for document-tab switching. Focus is
                        // reclaimed below after egui's default Tab traversal runs.
                        let cycle = ui.input_mut(|input| {
                            if !input.key_pressed(egui::Key::Tab) || input.modifiers.command {
                                return None;
                            }
                            let backward = input.modifiers.shift;
                            if backward {
                                input.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab);
                            } else {
                                input.consume_key(egui::Modifiers::NONE, egui::Key::Tab);
                            }
                            Some(if backward { -1 } else { 1 })
                        });
                        if let Some(delta) = cycle {
                            *source =
                                crate::math::cycle_math_template_by(source, delta).to_string();
                            changed = true;
                            cycled = true;
                        }
                    }
                    if let Some(error) = &error {
                        ui.label(
                            egui::RichText::new(error)
                                .color(Color32::from_rgb(220, 110, 100))
                                .size(11.0),
                        );
                    }
                });
            if let Some(tab) = app.tab_mut() {
                tab.focus_edit = false;
                if changed {
                    tab.doc.session.mark_dirty(id);
                    if !matches!(tab.save, crate::app::SaveState::Saving) {
                        tab.save = crate::app::SaveState::Dirty {
                            since: Instant::now(),
                        };
                    }
                }
            }
            if changed {
                app.queue_math(id);
            }
            if cycled {
                // Keep focus after replacing the source via Tab / Shift+Tab.
                if let Some(tab) = app.tab_mut() {
                    tab.focus_edit = true;
                    tab.editing = Some(id);
                }
            }
        }
        AnnotKind::Note { rect, .. } => {
            let screen = {
                let Some(tab) = app.tab() else {
                    return;
                };
                pdf_rect_screen(&tab.doc, page, rect, view)
            };
            let mut changed = false;
            egui::Area::new(Id::new(("marker-note", gen, id)))
                .order(egui::Order::Foreground)
                .fixed_pos(Pos2::new(screen.max.x + 8.0, screen.min.y))
                .constrain(false)
                .show(ctx, |ui| {
                    ui.set_min_width(200.0);
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label(egui::RichText::new("Note").weak().size(11.0));
                        let Some(tab) = app.tab_mut() else {
                            return;
                        };
                        let Some(AnnotKind::Note { content, .. }) =
                            tab.doc.session.get_mut(id).map(|annot| &mut annot.kind)
                        else {
                            return;
                        };
                        let response = ui.add(
                            TextEdit::multiline(content)
                                .desired_width(220.0)
                                .desired_rows(4),
                        );
                        if focus {
                            response.request_focus();
                        }
                        changed = response.changed();
                    });
                });
            if let Some(tab) = app.tab_mut() {
                tab.focus_edit = false;
                if changed {
                    tab.doc.session.mark_dirty(id);
                    if !matches!(tab.save, crate::app::SaveState::Saving) {
                        tab.save = crate::app::SaveState::Dirty {
                            since: Instant::now(),
                        };
                    }
                }
            }
        }
        _ => {
            if let Some(tab) = app.tab_mut() {
                tab.editing = None;
            }
        }
    }
}

fn ensure_image_textures(app: &mut MarkerApp, ctx: &egui::Context) {
    let Some(tab) = app.tab_mut() else {
        return;
    };
    let live: Vec<(u64, usize, std::sync::Arc<[u8]>, u32, u32)> = tab
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

fn open_context_menu(app: &mut MarkerApp, pos: Pos2, view: Rect) {
    let located = app.tab().and_then(|tab| {
        let (page, point) = tab.doc.screen_to_page(pos, view)?;
        let hit = tab.doc.session.hit_test(page, point, 6.0 / tab.doc.scale);
        Some((page, point, hit))
    });
    let Some((page, point, hit)) = located else {
        return;
    };
    if let Some(tab) = app.tab_mut() {
        tab.style_bar = None;
        if let Some(id) = hit {
            if !tab.is_selected(id) {
                tab.select_only(id);
            }
        }
        tab.menu = Some(ContextMenu {
            pos,
            page,
            point,
            hit,
        });
    }
}

fn paint_style_bar(app: &mut MarkerApp, ctx: &egui::Context, view: Rect) {
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
    pos.x = (pos.x - estimated.x * 0.5)
        .clamp(view.left() + 4.0, (view.right() - estimated.x - 4.0).max(view.left() + 4.0));

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
                        delete = app.style_bar_contents(ui);
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
        let pointer = ctx.pointer_interact_pos().or_else(|| ctx.pointer_latest_pos());
        let hit_self = pointer.and_then(|pos| {
            let tab = app.tab()?;
            let (page, point) = tab.doc.screen_to_page(pos, view)?;
            tab.doc
                .session
                .hit_test(page, point, 6.0 / tab.doc.scale)
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

fn paint_menu(app: &mut MarkerApp, ctx: &egui::Context) {
    let Some(menu) = app.tab().and_then(|tab| tab.menu.clone()) else {
        return;
    };
    let has_learning = app
        .tab()
        .is_some_and(|tab| tab.assistant.learning.is_some());
    let has_text_sel = app.tab().is_some_and(|tab| tab.text_sel.is_some());
    let copy_text = app
        .copyable_selection_text()
        .or_else(|| {
            if menu.hit.is_some() {
                None
            } else {
                app.word_at_point(menu.page, menu.point)
            }
        });
    let hit_kind = menu.hit.and_then(|id| {
        app.tab()
            .and_then(|tab| tab.doc.session.get(id))
            .map(|annot| annot.kind.clone())
    });
    let multi = app
        .tab()
        .is_some_and(|tab| tab.selected.len() > 1);
    let can_style = hit_kind.as_ref().is_some_and(|kind| {
        !matches!(kind, AnnotKind::Image { .. } | AnnotKind::Future(_))
    }) || app.tab().is_some_and(|tab| {
        tab.selected.iter().any(|id| {
            tab.doc.session.get(*id).is_some_and(|annot| {
                !matches!(annot.kind, AnnotKind::Image { .. } | AnnotKind::Future(_))
            })
        })
    });
    let can_edit = matches!(
        hit_kind,
        Some(AnnotKind::Text { .. } | AnnotKind::Note { .. } | AnnotKind::Math { .. })
    ) && !multi;
    let can_delete = menu.hit.is_some()
        || app.tab().is_some_and(|tab| !tab.selected.is_empty());

    let mut action = MenuAction::None;
    let area = egui::Area::new(Id::new("marker-context"))
        .order(egui::Order::Tooltip)
        .fixed_pos(menu.pos)
        .interactable(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(168.0);
                if let Some(kind) = &hit_kind {
                    ui.label(
                        egui::RichText::new(annot_menu_label(kind, multi))
                            .small()
                            .weak(),
                    );
                    ui.add_space(2.0);
                }
                if can_edit {
                    if ui.button("Edit").clicked() {
                        action = MenuAction::Edit;
                    }
                }
                if can_style {
                    if ui
                        .button(if multi {
                            "Style selected…"
                        } else {
                            "Style…"
                        })
                        .clicked()
                    {
                        action = MenuAction::Style;
                    }
                }
                if let Some(text) = &copy_text {
                    let label = if text.chars().count() > 24 {
                        "Copy text"
                    } else {
                        "Copy"
                    };
                    if ui.button(label).clicked() {
                        action = MenuAction::Copy;
                    }
                }
                if can_delete {
                    if ui.button("Delete").clicked() {
                        action = MenuAction::Delete;
                    }
                }
                if hit_kind.is_some() || can_delete || copy_text.is_some() || can_style || can_edit
                {
                    ui.separator();
                }
                if ui.button("Paste image").clicked() {
                    action = MenuAction::Paste;
                }
                ui.separator();
                ui.label(egui::RichText::new("Assistant").small().weak());
                if has_learning || has_text_sel {
                    if ui.button("Attach selected text").clicked() {
                        action = MenuAction::AttachText;
                    }
                    if has_learning && ui.button("Attach screenshot of selection").clicked() {
                        action = MenuAction::AttachShot;
                    }
                    if (has_learning || has_text_sel) && ui.button("Explain with Cursor").clicked()
                    {
                        action = MenuAction::Explain;
                    }
                    if (has_learning || has_text_sel) && ui.button("Look up in browser").clicked() {
                        action = MenuAction::Lookup;
                    }
                } else if ui.button("Select text for Assistant").clicked() {
                    action = MenuAction::AttachText;
                }
            });
        });
    // Dismiss only on primary click outside. The opening right-click is a
    // secondary `any_click` on the same frame the Area first appears (hovered
    // is still false), which would otherwise flash the menu for one frame.
    let outside = ctx.input(|input| input.pointer.button_clicked(PointerButton::Primary))
        && !area.response.hovered()
        && !area.response.clicked();
    match action {
        MenuAction::None => {
            if outside {
                if let Some(tab) = app.tab_mut() {
                    tab.menu = None;
                }
            }
        }
        MenuAction::Paste => {
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            let _ = app.paste_clipboard_image();
        }
        MenuAction::Copy => {
            if let Some(text) = copy_text {
                ctx.copy_text(text);
            }
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
        }
        MenuAction::Style => {
            let id = menu.hit.or_else(|| app.tab().and_then(|tab| tab.primary_selected()));
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            if let Some(id) = id {
                app.open_style_bar(id, false);
            }
        }
        MenuAction::Edit => {
            if let Some(id) = menu.hit {
                if let Some(tab) = app.tab_mut() {
                    tab.menu = None;
                }
                app.begin_edit_undo(id);
                if let Some(tab) = app.tab_mut() {
                    tab.select_only(id);
                    tab.editing = Some(id);
                    tab.focus_edit = true;
                }
            }
        }
        MenuAction::Delete => {
            if let Some(tab) = app.tab_mut() {
                if let Some(id) = menu.hit {
                    if !tab.is_selected(id) {
                        tab.select_only(id);
                    }
                }
                tab.menu = None;
            }
            app.delete_selected();
        }
        MenuAction::AttachText => {
            if let Some(tab) = app.tab_mut() {
                // Prefer Select-tool text for assistant attach when present.
                if tab.assistant.learning.is_none() {
                    if let Some(sel) = tab.text_sel.clone() {
                        tab.assistant.learning = Some(LearningSelection {
                            page: sel.page,
                            glyph_lo: sel.glyph_lo,
                            glyph_hi: sel.glyph_hi,
                        });
                    }
                }
                tab.menu = None;
            }
            if app
                .tab()
                .is_some_and(|tab| tab.assistant.learning.is_some())
            {
                app.attach_learning_text();
            } else {
                app.begin_learning_select();
            }
        }
        MenuAction::AttachShot => {
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            app.attach_learning_screenshot();
        }
        MenuAction::Explain => {
            // Promote text_sel into learning selection when needed.
            if let Some(tab) = app.tab_mut() {
                if tab.assistant.learning.is_none() {
                    if let Some(sel) = tab.text_sel.clone() {
                        tab.assistant.learning = Some(LearningSelection {
                            page: sel.page,
                            glyph_lo: sel.glyph_lo,
                            glyph_hi: sel.glyph_hi,
                        });
                    }
                }
                tab.menu = None;
            }
            app.explain_selection();
        }
        MenuAction::Lookup => {
            if let Some(tab) = app.tab_mut() {
                if tab.assistant.learning.is_none() {
                    if let Some(sel) = tab.text_sel.clone() {
                        tab.assistant.learning = Some(LearningSelection {
                            page: sel.page,
                            glyph_lo: sel.glyph_lo,
                            glyph_hi: sel.glyph_hi,
                        });
                    }
                }
                tab.menu = None;
            }
            app.lookup_selection_in_browser();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    None,
    Paste,
    Copy,
    Style,
    Edit,
    Delete,
    AttachText,
    AttachShot,
    Explain,
    Lookup,
}

fn annot_menu_label(kind: &AnnotKind, multi: bool) -> &'static str {
    if multi {
        return "Selected annotations";
    }
    match kind {
        AnnotKind::Highlight { .. } => "Highlight",
        AnnotKind::Text { .. } => "Text",
        AnnotKind::Note { .. } => "Note",
        AnnotKind::Math { .. } => "Equation",
        AnnotKind::Shape {
            kind: ShapeKind::Rect,
            ..
        } => "Rectangle",
        AnnotKind::Shape {
            kind: ShapeKind::Ellipse,
            ..
        } => "Ellipse",
        AnnotKind::Shape {
            kind: ShapeKind::Line,
            ..
        } => "Line",
        AnnotKind::Image { .. } => "Image",
        AnnotKind::Future(_) => "Annotation",
    }
}

fn paint_scrollbar(app: &mut MarkerApp, ui: &mut egui::Ui, view: Rect) {
    let scroll_id = app.active;
    let Some(tab) = app.tab_mut() else {
        return;
    };
    let height = tab.doc.doc_height_px();
    if height <= view.height() + 1.0 {
        return;
    }

    // Match egui's floating ScrollArea look (same as the empty homepage).
    let scroll_style = ui.spacing().scroll.clone();
    let hovering_view = ui.rect_contains_pointer(view);
    let outer_margin = 4.0;
    let max_w = scroll_style.bar_width.max(scroll_style.floating_width).max(6.0);
    let track = Rect::from_min_max(
        Pos2::new(view.right() - max_w - outer_margin, view.top() + outer_margin),
        Pos2::new(view.right() - outer_margin, view.bottom() - outer_margin),
    );

    let response = ui.interact(
        track,
        Id::new(("marker-scroll", scroll_id)),
        Sense::click_and_drag(),
    );
    let hovering_bar = response.hovered() || response.dragged();

    let bar_t = ui.ctx().animate_bool_responsive(
        Id::new(("marker-scroll-bar", scroll_id)),
        hovering_bar,
    );
    let show_t = ui.ctx().animate_bool_responsive(
        Id::new(("marker-scroll-show", scroll_id)),
        hovering_view || hovering_bar,
    );
    if show_t <= 0.001 && !hovering_bar {
        return;
    }

    let width = egui::lerp(
        scroll_style.floating_width.max(2.0)..=scroll_style.bar_width.max(6.0),
        bar_t,
    );
    let inset = ((max_w - width) * 0.5).max(0.0);
    let bar_rect = Rect::from_min_max(
        Pos2::new(track.left() + inset, track.top()),
        Pos2::new(track.right() - inset, track.bottom()),
    );

    let thumb_h = (bar_rect.height() * (view.height() / height))
        .max(scroll_style.handle_min_length.max(28.0));
    let travel = (bar_rect.height() - thumb_h).max(1.0);
    let t = (tab.doc.scroll_y / (height - view.height())).clamp(0.0, 1.0);
    let thumb = Rect::from_min_size(
        Pos2::new(bar_rect.left(), bar_rect.top() + travel * t),
        Vec2::new(bar_rect.width(), thumb_h),
    );

    let visuals = ui.visuals();
    let widget = if response.dragged() {
        &visuals.widgets.active
    } else if response.hovered()
        && ui.input(|input| {
            input
                .pointer
                .latest_pos()
                .is_some_and(|p| thumb.contains(p))
        })
    {
        &visuals.widgets.hovered
    } else {
        &visuals.widgets.inactive
    };
    let handle_opacity = if hovering_bar {
        scroll_style.interact_handle_opacity
    } else {
        egui::lerp(
            scroll_style.dormant_handle_opacity..=scroll_style.active_handle_opacity,
            show_t,
        )
    };
    let background_opacity = if hovering_bar {
        scroll_style.interact_background_opacity
    } else if hovering_view {
        scroll_style.active_background_opacity
    } else {
        scroll_style.dormant_background_opacity
    };
    let handle_color = if scroll_style.foreground_color {
        widget.fg_stroke.color
    } else {
        widget.bg_fill
    };

    ui.painter().rect_filled(
        bar_rect,
        widget.corner_radius,
        visuals.extreme_bg_color.gamma_multiply(background_opacity * show_t),
    );
    ui.painter().rect_filled(
        thumb,
        widget.corner_radius,
        handle_color.gamma_multiply(handle_opacity * show_t),
    );

    if response.dragged() {
        tab.doc.scroll_y += response.drag_delta().y / travel * (height - view.height());
        tab.doc.clamp_scroll(view);
        tab.doc.last_scroll = Instant::now();
    } else if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = ((pos.y - bar_rect.top() - thumb_h * 0.5) / travel).clamp(0.0, 1.0);
            tab.doc.scroll_y = t * (height - view.height());
            tab.doc.clamp_scroll(view);
            tab.doc.last_scroll = Instant::now();
        }
    }
}

pub(crate) struct TileWant {
    pub tiles: Vec<(crate::app::TileKey, f32)>,
    pub words: Vec<usize>,
}

pub(crate) fn wanted_tiles(
    doc: &DocState,
    inflight: &std::collections::HashSet<crate::app::TileKey>,
    _tool: Tool,
    view: Rect,
    pixels_per_point: f32,
) -> TileWant {
    let mut want = TileWant {
        tiles: Vec::new(),
        words: Vec::new(),
    };
    if doc.pages.is_empty() {
        return want;
    }
    let render_scale = doc.render_scale(pixels_per_point);
    let bits = render_scale.to_bits();
    let (first, last) = visible_pages(doc, view);
    let view_top = doc.scroll_y;
    let view_bot = doc.scroll_y + view.height();
    for page in first..=last {
        let y0 = doc.tops[page] * doc.scale;
        let y1 = y0 + doc.pages[page].height() * doc.scale;
        let on_screen = y1 >= view_top && y0 <= view_bot;
        if on_screen && !doc.glyphs.contains_key(&page) {
            want.words.push(page);
        }
        let rect = doc.page_rect(page, view);
        let visible = rect.intersect(view);
        if visible.width() < 1.0 || visible.height() < 1.0 {
            continue;
        }
        let info = doc.pages[page];
        let scale = render_scale;
        let x0 = (info.x0 * scale).floor() as i32;
        let y0 = (info.y0 * scale).floor() as i32;
        let left =
            ((visible.left() - rect.left()) / doc.scale * scale + info.x0 * scale).floor() as i32;
        let top =
            ((visible.top() - rect.top()) / doc.scale * scale + info.y0 * scale).floor() as i32;
        let right =
            ((visible.right() - rect.left()) / doc.scale * scale + info.x0 * scale).ceil() as i32;
        let bottom =
            ((visible.bottom() - rect.top()) / doc.scale * scale + info.y0 * scale).ceil() as i32;
        let col0 = (left - x0).div_euclid(TILE_PX);
        let col1 = (right - x0 - 1).div_euclid(TILE_PX);
        let row0 = (top - y0).div_euclid(TILE_PX);
        let row1 = (bottom - y0 - 1).div_euclid(TILE_PX);
        for col in col0..=col1 {
            for row in row0..=row1 {
                let key = crate::app::TileKey {
                    page,
                    scale_bits: bits,
                    col,
                    row,
                };
                if doc.tiles.contains_key(&key) || inflight.contains(&key) {
                    continue;
                }
                want.tiles.push((key, scale));
            }
        }
    }
    want
}
