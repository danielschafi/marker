use std::time::{Duration, Instant};

use egui::{
    Color32, CursorIcon, FontFamily, FontId, Id, PointerButton, Pos2, Rect, Sense, Stroke,
    TextEdit, Vec2,
};

use crate::annot::{glyph_at, highlight_quads, word_range, AnnotKind, Handle, ShapeKind};
use crate::app::{CreateKind, DocState, Drag, MarkerApp, Tab, Tool};
use crate::geom::{zoom_bucket, PdfPoint, PdfRect, MAX_SCALE, MIN_SCALE};
use crate::pdf::{PageInfo, TILE_PX};
use crate::ui::BACKDROP;

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
        let max_x = (max_page * self.scale + PAD * 2.0 - view.width()).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
    }

    pub(crate) fn fit_width(&mut self, view_w: f32) {
        let max_page = self.pages.iter().map(PageInfo::width).fold(1.0, f32::max);
        self.scale = ((view_w - PAD * 2.0) / max_page).clamp(MIN_SCALE, MAX_SCALE);
        self.scroll_x = 0.0;
        self.last_zoom = Instant::now();
    }

    pub(crate) fn render_scale(&self) -> f32 {
        if self.last_zoom.elapsed().as_millis() < 140 {
            zoom_bucket(self.scale)
        } else {
            self.scale
        }
    }

    fn page_origin(&self, page: usize, view: Rect) -> Pos2 {
        let width = self.pages[page].width() * self.scale;
        let x = if width + PAD * 2.0 <= view.width() {
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
                self.scroll_x = view.left() + PAD + (point.x - info.x0) * new_scale - cursor.x;
            } else {
                self.scroll_x = 0.0;
            }
        } else {
            let offset = cursor.y - view.top();
            let old = self.scale;
            self.scale = new_scale;
            self.scroll_y = crate::geom::zoom_scroll(old, new_scale, self.scroll_y, offset);
        }
        self.last_zoom = Instant::now();
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

pub(crate) fn viewport(app: &mut MarkerApp, ui: &mut egui::Ui) {
    let available = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(available, Sense::click_and_drag());
    app.view_rect = response.rect;
    let Some(tab) = app.tab_mut() else {
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
    handle_pointer(app, &response);

    let painter = ui.painter_at(response.rect);
    painter.rect_filled(response.rect, 0.0, BACKDROP);
    ensure_image_textures(app, ui.ctx());
    paint_document(app, &painter, response.rect);
    paint_scrollbar(app, ui, response.rect);
    inline_editors(app, ui.ctx(), response.rect);
    paint_menu(app, ui.ctx());
}

fn handle_scroll(app: &mut MarkerApp, response: &egui::Response) {
    if !response.hovered() {
        return;
    }
    let (raw, zoom, command, hover) = response.ctx.input(|input| {
        (
            input.raw_scroll_delta,
            input.zoom_delta(),
            input.modifiers.command,
            input.pointer.hover_pos(),
        )
    });
    let Some(tab) = app.tab_mut() else {
        return;
    };
    // Pinch keeps a cursor position on most platforms; fall back to the view
    // center if the pointer briefly drops out mid-gesture.
    let hover = hover.unwrap_or_else(|| response.rect.center());
    let pinching = (zoom - 1.0).abs() > f32::EPSILON;

    // Prefer zoom whenever egui reports a zoom delta. Trackpad pinch often
    // arrives together with a pan/scroll delta on Wayland; treating scroll
    // first made pinch feel broken.
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
    if raw != egui::Vec2::ZERO {
        // Wheel notches arrive as small pixel deltas and egui then smears them
        // across frames. Apply the raw delta immediately, scaled up so a notch
        // moves a readable chunk of the page.
        let gain = if raw.length() < 24.0 { 6.0 } else { 2.4 };
        let scroll = raw * gain;
        tab.doc.scroll_y -= scroll.y;
        tab.doc.scroll_x -= scroll.x;
        tab.doc.clamp_scroll(response.rect);
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
    } else if matches!(app.tool, Tool::Highlight | Tool::Text | Tool::Math) {
        response.clone().on_hover_cursor(CursorIcon::Text);
    }
}

fn begin_primary(app: &mut MarkerApp, pos: Pos2, view: Rect, space: bool) {
    let tool = app.tool;
    let Some(tab) = app.tab_mut() else {
        return;
    };
    if space || tool == Tool::Select {
        if let Some((page, point)) = tab.doc.screen_to_page(pos, view) {
            if tool == Tool::Select {
                if let Some((id, handle)) = resize_target(&tab.doc, tab.selected, tool, pos, view) {
                    if let Some(annot) = tab.doc.session.get(id) {
                        let origin = annot.kind.clone();
                        let page = annot.page;
                        tab.selected = Some(id);
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
                    let origin = tab.doc.session.get(id).map(|annot| annot.kind.clone());
                    tab.selected = Some(id);
                    tab.editing = None;
                    if let Some(origin) = origin {
                        tab.drag = Some(Drag::Move {
                            id,
                            origin,
                            grab: point,
                            page,
                            moved: false,
                        });
                    }
                    return;
                }
            }
        }
        if space || tool == Tool::Select {
            tab.selected = None;
            tab.editing = None;
            tab.drag = Some(Drag::Pan {
                scroll_x: tab.doc.scroll_x,
                scroll_y: tab.doc.scroll_y,
                pos,
            });
            return;
        }
    }

    let Some((page, point)) = tab.doc.screen_to_page(pos, view) else {
        tab.selected = None;
        tab.editing = None;
        return;
    };
    match tool {
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
            if let Some((id, handle)) = resize_target(&tab.doc, tab.selected, tool, pos, view) {
                if let Some(annot) = tab.doc.session.get(id) {
                    let origin = annot.kind.clone();
                    let annot_page = annot.page;
                    tab.selected = Some(id);
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
                    tab.selected = Some(id);
                    tab.editing = None;
                    tab.drag = Some(Drag::Move {
                        id,
                        origin,
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
        Tool::Select => {}
    }
}

fn begin_highlight(tab: &mut Tab, page: usize, point: PdfPoint) {
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
            id,
            origin,
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
                if let Some(annot) = tab.doc.session.get_mut(id) {
                    let mut kind = origin.clone();
                    kind.translate(point.x - grab.x, point.y - grab.y);
                    annot.kind = kind;
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
            } => commit_drag(app, drag, view),
            Drag::Create {
                page, origin, kind, ..
            } => {
                place_box(app, *page, *origin, None, *kind);
            }
            Drag::Move { id, .. } => {
                click(app, pos, view, double);
                if let Some(tab) = app.tab_mut() {
                    tab.selected = Some(*id);
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
    }
}

fn click(app: &mut MarkerApp, pos: Pos2, view: Rect, double: bool) {
    let tool = app.tool;
    let located = app.tab().and_then(|tab| tab.doc.screen_to_page(pos, view));
    let Some((page, point)) = located else {
        if let Some(tab) = app.tab_mut() {
            if tab.editing.take().is_some() {
                tab.selected = None;
            } else {
                tab.selected = None;
            }
        }
        app.end_edit_undo();
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
                    tab.selected = Some(id);
                    if open && editable {
                        tab.editing = Some(id);
                        tab.focus_edit = true;
                    } else {
                        tab.editing = None;
                    }
                }
            } else {
                app.end_edit_undo();
                if let Some(tab) = app.tab_mut() {
                    tab.selected = None;
                    tab.editing = None;
                }
            }
        }
        Tool::Highlight => {}
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
                    tab.selected = Some(id);
                    tab.editing = Some(id);
                    tab.focus_edit = true;
                }
            } else {
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
                    tab.selected = Some(id);
                    tab.editing = Some(id);
                    tab.focus_edit = true;
                }
            } else {
                place_box(app, page, point, None, CreateKind::Math);
            }
        }
        Tool::Rect | Tool::Ellipse | Tool::Line => {}
    }
}

fn is_editable(tab: &Tab, id: u64) -> bool {
    matches!(
        tab.doc.session.get(id).map(|annot| &annot.kind),
        Some(AnnotKind::Text { .. } | AnnotKind::Note { .. } | AnnotKind::Math { .. })
    )
}

fn commit_drag(app: &mut MarkerApp, drag: Drag, _view: Rect) {
    match drag {
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
            tab.selected = Some(id);
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
            tab.selected = Some(id);
            if !matches!(tab.save, crate::app::SaveState::Saving) {
                tab.save = crate::app::SaveState::Dirty {
                    since: Instant::now(),
                };
            }
            app.seal_undo();
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
            id, origin, moved, ..
        } => {
            let changed = moved
                && app
                    .tab()
                    .and_then(|tab| tab.doc.session.get(id))
                    .is_some_and(|annot| annot.kind != origin);
            if changed {
                if let Some(tab) = app.tab_mut() {
                    tab.doc.session.mark_dirty(id);
                    if !matches!(tab.save, crate::app::SaveState::Saving) {
                        tab.save = crate::app::SaveState::Dirty {
                            since: Instant::now(),
                        };
                    }
                }
                app.queue_math(id);
            }
            if let Some(tab) = app.tab_mut() {
                tab.selected = Some(id);
            }
            app.seal_undo();
        }
        Drag::Resize { id, .. } => {
            if let Some(tab) = app.tab_mut() {
                tab.doc.session.mark_dirty(id);
                tab.selected = Some(id);
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
        tab.selected
    };
    if let Some(id) = id {
        app.tag_undo_edit(id);
        if kind == CreateKind::Math {
            app.queue_math(id);
        }
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
    tab.selected = Some(id);
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

fn paint_document(app: &MarkerApp, painter: &egui::Painter, view: Rect) {
    let Some(tab) = app.tab() else {
        return;
    };
    let render_scale = tab.doc.render_scale();
    let (first, last) = visible_pages(&tab.doc, view);
    for page in first..=last {
        let rect = tab.doc.page_rect(page, view);
        let shadow = rect.expand(2.0).translate(Vec2::new(0.0, 4.0));
        painter.rect_filled(shadow, 6.0, Color32::from_black_alpha(28));
        painter.rect_filled(rect, 1.0, Color32::WHITE);
        paint_tiles(&tab.doc, painter, page, view, render_scale);
        paint_search_hits(tab, painter, page, view);
        paint_annotations(app, painter, page, view);
    }
    paint_drag_preview(app, painter, view);
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
) {
    let target_bits = render_scale.to_bits();
    let mut tiles: Vec<_> = doc
        .tiles
        .iter()
        .filter(|(key, _)| key.page == page)
        .collect();
    tiles.sort_by_key(|(key, _)| (key.scale_bits == target_bits, key.scale_bits));
    for (_key, tile) in tiles {
        let x0 = tile.x as f32 / tile.scale;
        let y0 = tile.y as f32 / tile.scale;
        let x1 = (tile.x as f32 + tile.w as f32) / tile.scale;
        let y1 = (tile.y as f32 + tile.h as f32) / tile.scale;
        let min = doc.page_to_screen(page, PdfPoint::new(x0, y0), view);
        let max = doc.page_to_screen(page, PdfPoint::new(x1, y1), view);
        let dest = Rect::from_min_max(min, max);
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
        let selected = tab.selected == Some(annot.id);
        let editing = tab.editing == Some(annot.id);
        match &annot.kind {
            AnnotKind::Highlight { quads, color } => {
                let mut fill = color.to_color32();
                fill = Color32::from_rgba_unmultiplied(fill.r(), fill.g(), fill.b(), 96);
                for quad in quads {
                    painter.rect_filled(pdf_rect_screen(&tab.doc, page, *quad, view), 1.0, fill);
                }
            }
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

fn paint_drag_preview(app: &MarkerApp, painter: &egui::Painter, view: Rect) {
    let Some(tab) = app.tab() else {
        return;
    };
    match &tab.drag {
        Some(Drag::Highlight {
            page,
            anchor,
            current,
            origin,
            current_pt,
            word_lo,
            word_hi,
            ..
        }) => {
            let mut fill = app.settings.highlight_color.to_color32();
            fill = Color32::from_rgba_unmultiplied(fill.r(), fill.g(), fill.b(), 96);
            let range = match (anchor, current, word_lo, word_hi) {
                (Some(a), Some(c), Some(wlo), Some(whi)) => {
                    Some(((*a).min(*c).min(*wlo), (*a).max(*c).max(*whi)))
                }
                (Some(a), Some(c), _, _) => Some(((*a).min(*c), (*a).max(*c))),
                _ => None,
            };
            if let (Some((lo, hi)), Some(glyphs)) = (range, tab.doc.glyphs.get(page)) {
                for rect in highlight_quads(glyphs, lo, hi) {
                    painter.rect_filled(pdf_rect_screen(&tab.doc, *page, rect, view), 1.0, fill);
                }
                return;
            }
            let rect = PdfRect::from_points(*origin, *current_pt);
            painter.rect_filled(pdf_rect_screen(&tab.doc, *page, rect, view), 1.0, fill);
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
                            egui::RichText::new("Shift+Tab cycles templates · math mode")
                                .weak()
                                .size(10.0),
                        );
                    });
                    let response = ui.add(
                        TextEdit::multiline(source)
                            .font(FontId::new(13.0, FontFamily::Monospace))
                            .desired_width(screen.width().max(280.0))
                            .desired_rows(3)
                            .hint_text(r"\frac{1}{2}  or  Shift+Tab for templates"),
                    );
                    if focus {
                        response.request_focus();
                    }
                    changed = response.changed();
                    if response.has_focus() {
                        // Tab keeps normal focus traversal; Shift+Tab cycles presets.
                        let tabbed = ui.input_mut(|input| {
                            if input.key_pressed(egui::Key::Tab)
                                && input.modifiers.shift
                                && !input.modifiers.command
                            {
                                input.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab);
                                true
                            } else {
                                false
                            }
                        });
                        if tabbed {
                            *source = crate::math::cycle_math_template(source).to_string();
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
                // Keep focus after replacing the source via Shift+Tab.
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
    let hit = app.tab().and_then(|tab| {
        let (page, point) = tab.doc.screen_to_page(pos, view)?;
        tab.doc.session.hit_test(page, point, 6.0 / tab.doc.scale)
    });
    if let Some(tab) = app.tab_mut() {
        if let Some(id) = hit {
            tab.selected = Some(id);
        }
        tab.menu = Some((pos, hit));
    }
}

fn paint_menu(app: &mut MarkerApp, ctx: &egui::Context) {
    let Some((pos, id)) = app.tab().and_then(|tab| tab.menu) else {
        return;
    };
    let mut delete = false;
    let mut paste = false;
    let area = egui::Area::new(Id::new("marker-context"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos)
        .interactable(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(120.0);
                if ui.button("Paste image").clicked() {
                    paste = true;
                }
                if id.is_some() && ui.button("Delete").clicked() {
                    delete = true;
                }
            });
        });
    let right_click = ctx.input(|input| input.pointer.button_pressed(PointerButton::Secondary));
    let outside = ctx.input(|input| input.pointer.any_click())
        && !area.response.hovered()
        && !area.response.clicked()
        && !right_click;
    if paste {
        if let Some(tab) = app.tab_mut() {
            tab.menu = None;
        }
        let _ = app.paste_clipboard_image();
    } else if delete {
        if let Some(tab) = app.tab_mut() {
            if let Some(id) = id {
                tab.selected = Some(id);
            }
            tab.menu = None;
        }
        app.delete_selected();
    } else if outside {
        if let Some(tab) = app.tab_mut() {
            tab.menu = None;
        }
    }
}

fn paint_scrollbar(app: &mut MarkerApp, ui: &mut egui::Ui, view: Rect) {
    let Some(tab) = app.tab_mut() else {
        return;
    };
    let height = tab.doc.doc_height_px();
    if height <= view.height() + 1.0 {
        return;
    }
    let track = Rect::from_min_max(
        Pos2::new(view.right() - 10.0, view.top() + 4.0),
        Pos2::new(view.right() - 4.0, view.bottom() - 4.0),
    );
    let thumb_h = (track.height() * (view.height() / height)).max(28.0);
    let travel = (track.height() - thumb_h).max(1.0);
    let t = tab.doc.scroll_y / (height - view.height());
    let thumb = Rect::from_min_size(
        Pos2::new(track.left(), track.top() + travel * t),
        Vec2::new(track.width(), thumb_h),
    );
    ui.painter()
        .rect_filled(thumb, 3.0, Color32::from_white_alpha(80));
    let response = ui.interact(track, Id::new("marker-scroll"), Sense::click_and_drag());
    if response.dragged() {
        tab.doc.scroll_y += response.drag_delta().y / travel * (height - view.height());
        tab.doc.clamp_scroll(view);
    } else if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let t = ((pos.y - track.top() - thumb_h * 0.5) / travel).clamp(0.0, 1.0);
            tab.doc.scroll_y = t * (height - view.height());
            tab.doc.clamp_scroll(view);
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
) -> TileWant {
    let mut want = TileWant {
        tiles: Vec::new(),
        words: Vec::new(),
    };
    if doc.pages.is_empty() {
        return want;
    }
    let render_scale = doc.render_scale();
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
