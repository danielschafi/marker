use std::time::{Duration, Instant};

use egui::text::CCursor;
use egui::text_selection::CCursorRange;
use egui::{
    Button, Color32, CursorIcon, Event, FontFamily, FontId, Frame, Id, ImeEvent, Key, Modifiers,
    PointerButton, Pos2, Rect, Sense, Stroke, TextEdit, Vec2,
};

#[path = "../math_input.rs"]
mod math_input;

mod geometry;
mod paint;

#[allow(unused_imports)] // `TileWant` / `TileRequest` kept on the `view::` path for callers.
pub(crate) use geometry::{visible_pages, wanted_tiles, TileRequest, TileWant};
pub(crate) use paint::{cached_math_metrics, EguiMeasure};

use crate::annot::{
    click_glyph_range, glyph_at, highlight_quads, word_range, AnnotKind, Handle, MarkupStyle,
    ShapeKind,
};
use crate::app::{ContextMenu, CreateKind, DocState, Drag, MarkerApp, Tab, TabId, TextSel, Tool};
use crate::assistant::{glyphs_intersecting_rects, CaptureMode, LearningSelection};
use crate::cite;
use crate::geom::{PdfPoint, PdfRect};
use crate::math::rich_text::{
    self, conceal_editor, preview_bubble, BubbleImage, CharSpan, EditorOutput, Render, Style,
    MATH_ERROR_IDLE_SECS,
};
use crate::math::{EntryKind, MathKey};
use crate::math_spans;
use crate::pdf::{DocId, PageInfo};
use crate::platform::clipboard_has_image;
use crate::theme;

use geometry::pdf_rect_screen;
use paint::{mark_drag_quads, paint_document, paint_scrollbar, text_mark_kind};

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
    if !pane.focused && (response.clicked() || response.drag_started()) {
        app.active = pane.tab_id;
        app.view_rect = pane.rect;
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

fn handle_pointer(app: &mut MarkerApp, response: &egui::Response, pane: PaneCtx) {
    let rect = pane.rect;
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
                if let Some(tab) = app.tab_by_id(pane.tab_id) {
                    let drag = Drag::Pan {
                        scroll_x: tab.doc.scroll_x,
                        scroll_y: tab.doc.scroll_y,
                        pos,
                    };
                    if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
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
        }) = app.tab_by_id(pane.tab_id).and_then(|tab| tab.drag.clone())
        {
            if let Some(now) = middle.3 {
                if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
                    tab.doc.scroll_x = scroll_x - (now.x - pos.x);
                    tab.doc.scroll_y = scroll_y - (now.y - pos.y);
                    tab.doc.clamp_scroll(rect);
                    tab.doc.last_scroll = Instant::now();
                }
            }
        }
        return;
    }
    if middle.2
        && matches!(
            app.tab_by_id(pane.tab_id).and_then(|tab| tab.drag.as_ref()),
            Some(Drag::Pan { .. })
        )
    {
        if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
            tab.drag = None;
        }
    }

    if response.secondary_clicked() {
        let pos = response
            .interact_pointer_pos()
            .or_else(|| response.ctx.input(|input| input.pointer.hover_pos()));
        if let Some(pos) = pos {
            open_context_menu(app, pos, pane);
        }
    }

    if response.drag_started_by(PointerButton::Primary) {
        if let Some(pos) = response.interact_pointer_pos() {
            begin_primary(app, pos, pane, space);
        }
    }
    if response.dragged_by(PointerButton::Primary) {
        if let Some(pos) = response.interact_pointer_pos() {
            update_primary(app, pos, pane);
        }
    }
    if response.drag_stopped_by(PointerButton::Primary) {
        let pos = response.interact_pointer_pos();
        end_primary(app, pos, pane, response.double_clicked());
    } else if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            click(app, pos, pane, response.double_clicked());
        }
    }

    if matches!(
        app.tab_by_id(pane.tab_id).and_then(|tab| tab.drag.as_ref()),
        Some(Drag::Pan { .. })
    ) {
        response.ctx.set_cursor_icon(CursorIcon::Grabbing);
    } else if matches!(app.capture, CaptureMode::Region)
        || matches!(
            app.tab_by_id(pane.tab_id).and_then(|tab| tab.drag.as_ref()),
            Some(Drag::Marquee { .. })
        )
    {
        response.clone().on_hover_cursor(CursorIcon::Crosshair);
    } else if matches!(app.capture, CaptureMode::LearningText)
        || app.tool.is_text_mark()
        || matches!(app.tool, Tool::Text | Tool::Math)
        || matches!(
            app.tab_by_id(pane.tab_id).and_then(|tab| tab.drag.as_ref()),
            Some(Drag::TextSelect { .. })
        )
    {
        response.clone().on_hover_cursor(CursorIcon::Text);
    }
}

fn begin_primary(app: &mut MarkerApp, pos: Pos2, pane: PaneCtx, space: bool) {
    let view = pane.rect;
    if app.capture != CaptureMode::None && !space {
        let capture = app.capture;
        let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
    let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
        Tool::Highlight | Tool::Underline | Tool::StrikeOut | Tool::Squiggly => {
            begin_highlight(tab, page, point, tool.markup_style());
        }
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
        Tool::Text | Tool::Note | Tool::Math => {
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
                    Tool::Note => matches!(annot.kind, AnnotKind::Note { .. }),
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
                kind: match tool {
                    Tool::Math => CreateKind::Math,
                    Tool::Note => CreateKind::Note,
                    _ => CreateKind::Text,
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

fn begin_highlight(tab: &mut Tab, page: usize, point: PdfPoint, style: Option<MarkupStyle>) {
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
            style,
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
        style,
    });
}

fn update_primary(app: &mut MarkerApp, pos: Pos2, pane: PaneCtx) {
    let view = pane.rect;
    let drag = app.tab_by_id(pane.tab_id).and_then(|tab| tab.drag.clone());
    let Some(drag) = drag else {
        return;
    };
    match drag {
        Drag::Pan {
            scroll_x,
            scroll_y,
            pos: start,
        } => {
            if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
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
            style,
            ..
        } => {
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
                style,
            });
        }
        Drag::LearningSelect {
            page,
            anchor,
            origin,
            ..
        } => {
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
        Drag::Marquee { page, origin, .. } => {
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
        Drag::Region { page, origin, .. } => {
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
                let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
            if let Some(Drag::Move { moved, .. }) = app
                .tab_by_id_mut(pane.tab_id)
                .and_then(|tab| tab.drag.as_mut())
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
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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

fn end_primary(app: &mut MarkerApp, pos: Option<Pos2>, pane: PaneCtx, double: bool) {
    let drag = app
        .tab_by_id_mut(pane.tab_id)
        .and_then(|tab| tab.drag.take());
    let Some(pos) = pos else {
        if let Some(drag) = drag {
            commit_drag(app, drag, pane);
        }
        return;
    };
    let Some(drag) = drag else {
        click(app, pos, pane, double);
        return;
    };
    if !drag_moved(&drag, app, pos, pane) {
        match &drag {
            Drag::Highlight {
                anchor: Some(_), ..
            }
            | Drag::LearningSelect {
                anchor: Some(_), ..
            }
            | Drag::TextSelect {
                anchor: Some(_), ..
            } => commit_drag(app, drag, pane),
            Drag::Region { .. } | Drag::Marquee { .. } => {
                // Tiny click — ignore empty crop / marquee.
            }
            Drag::Create {
                page, origin, kind, ..
            } => {
                place_box(app, pane, *page, *origin, None, *kind);
            }
            Drag::Move { ids, .. } => {
                click(app, pos, pane, double);
                if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
                    if !ids.is_empty() {
                        tab.select_many(ids.clone());
                    }
                }
            }
            _ => click(app, pos, pane, double),
        }
        return;
    }
    commit_drag(app, drag, pane);
}

fn drag_moved(drag: &Drag, app: &MarkerApp, pos: Pos2, pane: PaneCtx) -> bool {
    let view = pane.rect;
    let Some(tab) = app.tab_by_id(pane.tab_id) else {
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

fn click(app: &mut MarkerApp, pos: Pos2, pane: PaneCtx, double: bool) {
    let view = pane.rect;
    // True clicks never start a drag (`Sense::click_and_drag` waits for movement), so
    // text / highlight / learning selection must be handled here — including double-click
    // whole-word selection (#34).
    if app.capture == CaptureMode::LearningText {
        let Some((page, point)) = app
            .tab_by_id(pane.tab_id)
            .and_then(|tab| tab.doc.screen_to_page(pos, view))
        else {
            return;
        };
        select_glyphs_at_point(app, pane, page, point, double, GlyphClick::Learning);
        return;
    }
    if app.capture == CaptureMode::Region {
        return;
    }

    let tool = app.tool;
    let located = app
        .tab_by_id(pane.tab_id)
        .and_then(|tab| tab.doc.screen_to_page(pos, view));
    let Some((page, point)) = located else {
        app.end_edit_undo();
        app.clear_page_selection();
        return;
    };
    let slop = app
        .tab_by_id(pane.tab_id)
        .map(|tab| 4.0 / tab.doc.scale)
        .unwrap_or(4.0);
    let hit = app
        .tab_by_id(pane.tab_id)
        .and_then(|tab| tab.doc.session.hit_test(page, point, slop));
    match tool {
        Tool::Select => {
            if let Some(id) = hit {
                let open = double
                    || app.tab_by_id(pane.tab_id).is_some_and(|tab| {
                        matches!(
                            tab.doc.session.get(id).map(|annot| &annot.kind),
                            Some(AnnotKind::Note { .. })
                        )
                    });
                let editable = app
                    .tab_by_id(pane.tab_id)
                    .is_some_and(|tab| is_editable(tab, id));
                if open && editable {
                    app.begin_edit_undo(id);
                } else if app
                    .tab_by_id(pane.tab_id)
                    .is_some_and(|tab| tab.editing.is_some())
                {
                    app.end_edit_undo();
                }
                if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
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
                && select_glyphs_at_point(app, pane, page, point, true, GlyphClick::TextSelect)
            {
                app.end_edit_undo();
            } else {
                app.end_edit_undo();
                app.clear_page_selection();
            }
        }
        Tool::Highlight | Tool::Underline | Tool::StrikeOut | Tool::Squiggly => {
            if select_glyphs_at_point(
                app,
                pane,
                page,
                point,
                double,
                GlyphClick::Mark(tool.markup_style()),
            ) {
                // Mark created (single glyph, or whole word on double-click).
            } else if hit.is_none() {
                app.clear_page_selection();
            }
        }
        Tool::Text => {
            let text = hit.filter(|id| {
                app.tab_by_id(pane.tab_id).is_some_and(|tab| {
                    matches!(
                        tab.doc.session.get(*id).map(|annot| &annot.kind),
                        Some(AnnotKind::Text { .. })
                    )
                })
            });
            if let Some(id) = text {
                app.begin_edit_undo(id);
                if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
                    tab.select_only(id);
                    tab.assistant.learning = None;
                    tab.editing = Some(id);
                    tab.focus_edit = true;
                }
            } else {
                if hit.is_none() {
                    app.clear_page_selection();
                }
                place_box(app, pane, page, point, None, CreateKind::Text);
            }
        }
        Tool::Note => {
            let note = hit.filter(|id| {
                app.tab_by_id(pane.tab_id).is_some_and(|tab| {
                    matches!(
                        tab.doc.session.get(*id).map(|annot| &annot.kind),
                        Some(AnnotKind::Note { .. })
                    )
                })
            });
            if let Some(id) = note {
                app.begin_edit_undo(id);
                if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
                    tab.select_only(id);
                    tab.assistant.learning = None;
                    tab.editing = Some(id);
                    tab.focus_edit = true;
                }
            } else {
                if hit.is_none() {
                    app.clear_page_selection();
                }
                // Click places a default-size note; notes are not drag-to-size.
                place_box(app, pane, page, point, None, CreateKind::Note);
            }
        }
        Tool::Math => {
            let math = hit.filter(|id| {
                app.tab_by_id(pane.tab_id).is_some_and(|tab| {
                    matches!(
                        tab.doc.session.get(*id).map(|annot| &annot.kind),
                        Some(AnnotKind::Math { .. })
                    )
                })
            });
            if let Some(id) = math {
                app.begin_edit_undo(id);
                if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
                    tab.select_only(id);
                    tab.assistant.learning = None;
                    tab.editing = Some(id);
                    tab.focus_edit = true;
                }
            } else {
                if hit.is_none() {
                    app.clear_page_selection();
                }
                place_box(app, pane, page, point, None, CreateKind::Math);
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
    /// `None` is a highlight fill. A style is underline, strikeout, or squiggly.
    Mark(Option<MarkupStyle>),
    Learning,
}

/// Select / highlight glyphs under a click. Double-click expands to the whole word.
/// Returns false when no glyph is under the point.
fn select_glyphs_at_point(
    app: &mut MarkerApp,
    pane: PaneCtx,
    page: usize,
    point: PdfPoint,
    whole_word: bool,
    kind: GlyphClick,
) -> bool {
    let (lo, hi, word) = {
        let Some(tab) = app.tab_by_id(pane.tab_id) else {
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
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
        GlyphClick::Mark(style) => {
            let color = app.settings.highlight_color;
            let replace = {
                let Some(tab) = app.tab_by_id(pane.tab_id) else {
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
                let Some(tab) = app.tab_by_id(pane.tab_id) else {
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
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
                return false;
            };
            if let Some(old) = replace {
                tab.doc.session.remove(old);
            }
            let id = tab
                .doc
                .session
                .insert(page, text_mark_kind(style, quads, color));
            // Leave the new mark unselected so the user can keep marking.
            tab.selected.clear();
            tab.text_sel = None;
            if let Some(w) = word {
                // Remember single-glyph marks so a quick second click can upgrade to the word.
                let remembered = (!whole_word).then_some(id);
                tab.last_hl = Some((Instant::now(), page, w, remembered));
            }
            tab.save.touch();
            app.seal_undo();
            true
        }
    }
}

fn commit_drag(app: &mut MarkerApp, drag: Drag, pane: PaneCtx) {
    match drag {
        Drag::LearningSelect {
            page,
            anchor,
            current,
            ..
        } => {
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
                return;
            };
            if let (Some(a), Some(c)) = (anchor, current) {
                tab.assistant.learning = Some(LearningSelection {
                    page,
                    glyph_lo: a.min(c),
                    glyph_hi: a.max(c),
                });
                app.capture = CaptureMode::None;
                app.set_assistant_open(true);
                app.attach_learning_text();
            }
        }
        Drag::TextSelect {
            page,
            anchor,
            current,
            ..
        } => {
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
                let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
                        tab.doc
                            .session
                            .get(id)
                            .is_some_and(|annot| annot.kind.is_styleable())
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
            app.set_assistant_open(true);
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
            style,
        } => {
            let color = app.settings.highlight_color;
            let quads = {
                let Some(tab) = app.tab_by_id(pane.tab_id) else {
                    return;
                };
                mark_drag_quads(
                    tab,
                    page,
                    anchor,
                    current,
                    origin,
                    current_pt,
                    (word_lo, word_hi),
                )
            };
            if quads.is_empty() {
                return;
            }
            app.seal_then_arm();
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
                return;
            };
            if let Some(old) = replace {
                tab.doc.session.remove(old);
            }
            let id = tab
                .doc
                .session
                .insert(page, text_mark_kind(style, quads, color));
            // Leave the new mark unselected so the user can keep highlighting.
            tab.selected.clear();
            tab.text_sel = None;
            if let Some(glyphs) = tab.doc.glyphs.get(&page) {
                if let Some(index) = anchor.or(current) {
                    if let Some(glyph) = glyphs.get(index) {
                        let remembered = (anchor == current).then_some(id);
                        tab.last_hl = Some((Instant::now(), page, glyph.word, remembered));
                    }
                }
            }
            tab.save.touch();
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
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
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
            tab.save.touch();
            app.seal_undo();
            app.open_style_bar(id, true);
        }
        Drag::Create {
            page,
            origin,
            current,
            kind,
        } => {
            // Notes are click-placed at a fixed icon size; ignore drag extent.
            let rect = if kind == CreateKind::Note {
                None
            } else {
                let rect = PdfRect::from_points(origin, current);
                let sized = rect.width() >= 12.0 && rect.height() >= 8.0;
                sized.then_some(rect)
            };
            place_box(app, pane, page, origin, rect, kind);
        }
        Drag::Move {
            ids,
            origins,
            moved,
            ..
        } => {
            if moved {
                let mut dirty = Vec::new();
                if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
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
                    if !dirty.is_empty() {
                        tab.save.touch();
                    }
                }
                for id in dirty {
                    app.queue_math(id);
                }
            }
            if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
                tab.select_many(ids);
            }
            app.seal_undo();
        }
        Drag::Resize { id, .. } => {
            if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
                tab.doc.session.mark_dirty(id);
                if !tab.is_selected(id) {
                    tab.select_only(id);
                }
                tab.save.touch();
            }
            app.queue_math(id);
            app.seal_undo();
        }
        Drag::Pan { .. } => {}
    }
}

fn place_box(
    app: &mut MarkerApp,
    pane: PaneCtx,
    page: usize,
    point: PdfPoint,
    rect: Option<PdfRect>,
    kind: CreateKind,
) {
    let settings = app.settings.clone();
    app.seal_then_arm();
    let id = {
        let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
            return;
        };
        place_box_tab(tab, page, point, rect, kind, &settings, String::new());
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

/// Insert a Text or Note annotation filled with `content` (ST6).
///
/// Places beside the current selection when one exists; otherwise at the
/// viewport center (falling back to the current page center).
pub(crate) fn place_assistant_answer(
    tab: &mut Tab,
    view: Rect,
    kind: CreateKind,
    settings: &crate::settings::Settings,
    content: String,
) -> Option<u64> {
    if !matches!(kind, CreateKind::Text | CreateKind::Note) {
        return None;
    }
    if tab.doc.pages.is_empty() {
        return None;
    }
    let size = settings.text_size;
    let (page, point, rect) = match selection_bounds(tab) {
        Some((page, bounds)) => {
            let info = tab.doc.pages.get(page)?;
            let (point, rect) = match kind {
                CreateKind::Note => {
                    let point = point_beside_bounds(bounds, info, crate::annot::NOTE_SIZE);
                    (point, None)
                }
                CreateKind::Text => {
                    let height = size * 1.8 + 4.0;
                    let rect = rect_beside_bounds(bounds, info, 200.0, height);
                    (PdfPoint::new(rect.x0, rect.y0), Some(rect))
                }
                CreateKind::Math => return None,
            };
            (page, point, rect)
        }
        None => {
            let (page, point) = viewport_anchor(&tab.doc, view)?;
            (page, point, None)
        }
    };
    place_box_tab(tab, page, point, rect, kind, settings, content);
    tab.primary_selected()
}

fn selection_bounds(tab: &Tab) -> Option<(usize, PdfRect)> {
    if let Some(sel) = &tab.assistant.learning {
        let glyphs = tab.doc.glyphs.get(&sel.page)?;
        let quads = highlight_quads(glyphs, sel.glyph_lo, sel.glyph_hi);
        return union_pdf_rects(&quads).map(|bounds| (sel.page, bounds));
    }
    if let Some(sel) = &tab.text_sel {
        let glyphs = tab.doc.glyphs.get(&sel.page)?;
        let quads = highlight_quads(glyphs, sel.glyph_lo, sel.glyph_hi);
        return union_pdf_rects(&quads).map(|bounds| (sel.page, bounds));
    }
    let id = tab.primary_selected()?;
    let annot = tab.doc.session.get(id)?;
    match &annot.kind {
        AnnotKind::Highlight { quads, .. } | AnnotKind::Markup { quads, .. } => {
            union_pdf_rects(quads).map(|bounds| (annot.page, bounds))
        }
        AnnotKind::Text { .. }
        | AnnotKind::Note { .. }
        | AnnotKind::Shape { .. }
        | AnnotKind::Math { .. }
        | AnnotKind::Image { .. }
        | AnnotKind::Foreign { .. } => annot.bounds().map(|bounds| (annot.page, bounds)),
    }
}

fn viewport_anchor(doc: &DocState, view: Rect) -> Option<(usize, PdfPoint)> {
    if let Some(hit) = doc.screen_to_page(view.center(), view) {
        return Some(hit);
    }
    let page = doc.current_page(view.height().max(1.0));
    let info = doc.pages.get(page)?;
    Some((
        page,
        PdfPoint::new((info.x0 + info.x1) * 0.5, (info.y0 + info.y1) * 0.5),
    ))
}

fn point_beside_bounds(bounds: PdfRect, page: &PageInfo, width: f32) -> PdfPoint {
    let rect = rect_beside_bounds(bounds, page, width, width);
    PdfPoint::new(rect.x0, rect.y0)
}

fn rect_beside_bounds(bounds: PdfRect, page: &PageInfo, width: f32, height: f32) -> PdfRect {
    const GAP: f32 = 8.0;
    let right = bounds.x1 + GAP;
    if right + width <= page.x1 + 0.5 {
        return PdfRect::new(right, bounds.y0, right + width, bounds.y0 + height);
    }
    let max_x = (page.x1 - width).max(page.x0);
    let x = bounds.x0.clamp(page.x0, max_x);
    let y = bounds.y1 + GAP;
    PdfRect::new(x, y, x + width, y + height)
}

fn union_pdf_rects(rects: &[PdfRect]) -> Option<PdfRect> {
    let first = *rects.first()?;
    Some(rects.iter().skip(1).fold(first, |acc, r| {
        PdfRect::new(
            acc.x0.min(r.x0),
            acc.y0.min(r.y0),
            acc.x1.max(r.x1),
            acc.y1.max(r.y1),
        )
    }))
}

fn place_box_tab(
    tab: &mut Tab,
    page: usize,
    point: PdfPoint,
    rect: Option<PdfRect>,
    kind: CreateKind,
    settings: &crate::settings::Settings,
    content: String,
) {
    let size = settings.text_size;
    let color = settings.text_color;
    let rect = match kind {
        // Sticky notes always use the icon size; drag extent is ignored.
        CreateKind::Note => PdfRect::new(
            point.x,
            point.y,
            point.x + crate::annot::NOTE_SIZE,
            point.y + crate::annot::NOTE_SIZE,
        ),
        CreateKind::Text => rect.unwrap_or_else(|| {
            PdfRect::new(
                point.x,
                point.y,
                point.x + 200.0,
                point.y + size * 1.8 + 4.0,
            )
        }),
        CreateKind::Math => rect.unwrap_or_else(|| {
            PdfRect::new(point.x, point.y, point.x + 88.0, point.y + size * 1.6 + 8.0)
        }),
    };
    let id = match kind {
        CreateKind::Text => tab.doc.session.insert(
            page,
            AnnotKind::Text {
                rect,
                content,
                size,
                color,
            },
        ),
        CreateKind::Note => tab.doc.session.insert(
            page,
            AnnotKind::Note {
                rect,
                content,
                color,
            },
        ),
        CreateKind::Math => {
            let _ = content;
            tab.doc.session.insert(
                page,
                AnnotKind::Math {
                    rect,
                    source: String::new(),
                    size,
                    color,
                    auto_size: true,
                },
            )
        }
    };
    tab.select_only(id);
    tab.editing = Some(id);
    tab.focus_edit = true;
    tab.save.touch();
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
        Tool::Note
        | Tool::Highlight
        | Tool::Underline
        | Tool::StrikeOut
        | Tool::Squiggly
        | Tool::Rect
        | Tool::Ellipse
        | Tool::Line => false,
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

// RF11 packs the edit target into an EditTarget struct.
#[allow(clippy::too_many_arguments)]
fn edit_text_annot(
    app: &mut MarkerApp,
    ctx: &egui::Context,
    view: Rect,
    id: u64,
    page: usize,
    doc_id: DocId,
    scale: f32,
    focus: bool,
    rect: PdfRect,
    size: f32,
    color: crate::geom::Rgb,
) {
    let screen = {
        let Some(tab) = app.tab() else {
            return;
        };
        pdf_rect_screen(&tab.doc, page, rect, view)
    };
    let area_id = Id::new(("marker-text-area", doc_id, id));
    let text_edit_id = Id::new(("marker-text", doc_id, id));
    let mut changed = false;
    let mut height = screen.height();

    // A previous Tab exit can still hand us a caret. The key itself is handled
    // in the same frame below via `apply_math_input`.
    if let Some(tab) = app.tab_mut() {
        if let Some(byte) = tab.pending_text_caret.take() {
            if let Some(AnnotKind::Text { content, .. }) =
                tab.doc.session.get(id).map(|annot| &annot.kind)
            {
                let index = byte_to_char_index(content, byte);
                let mut state = TextEdit::load_state(ctx, text_edit_id).unwrap_or_default();
                state
                    .cursor
                    .set_char_range(Some(CCursorRange::one(CCursor::new(index))));
                state.store(ctx, text_edit_id);
                tab.live_math = None;
            }
        }
    }

    egui::Area::new(area_id)
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .constrain(false)
        .show(ctx, |ui| {
            ui.set_max_width(screen.width().max(24.0));
            ui.visuals_mut().text_cursor.stroke = Stroke::new(2.0_f32, Color32::BLACK);
            let font_px = (size * scale).max(8.0);
            let style = math_edit_style(color.to_color32(), font_px);
            let autosnippets = app.settings.math_autosnippets;
            let bubble = {
                let Some((cache, tab)) = app.math_cache_and_tab_mut() else {
                    return;
                };
                let (plan, live_start) = {
                    let Some(AnnotKind::Text { content, .. }) =
                        tab.doc.session.get_mut(id).map(|annot| &mut annot.kind)
                    else {
                        return;
                    };
                    if let Some(text_changed) =
                        apply_math_input(ctx, ui, text_edit_id, content, autosnippets, id)
                    {
                        changed |= text_changed;
                    }
                    let render = |inner: &str, display: bool| {
                        cache_render(cache, inner, display, size, color, scale)
                    };
                    let output = conceal_editor(
                        ui,
                        text_edit_id,
                        content,
                        screen.width().max(24.0),
                        &style,
                        &render,
                    );
                    if focus {
                        output.response.request_focus();
                    }
                    changed |= output.response.changed();
                    height = output.response.rect.height().max(size * scale);
                    paint_concealed_math(ui, cache, &output, size, color);
                    paint_math_errors(ui, &output, style.error);
                    paint_math_completion(ctx, ui, text_edit_id, id, content, &output);
                    let plan = bubble_plan(ctx, content, &output, cache, size, color, changed);
                    let live_start = output.caret.and_then(|(index, _)| {
                        let byte = char_index_to_byte(content, index);
                        math_spans::math_span_at(content, byte).and_then(|span| {
                            (span.closed && byte > span.start && byte < span.end)
                                .then_some(span.start)
                        })
                    });
                    (plan, live_start)
                };
                tab.live_math = live_start.map(|start| (id, start));
                plan
            };
            if let Some(plan) = bubble {
                paint_math_bubble(app, ctx, view, doc_id, id, &plan, scale);
            }
        });

    let mut queue_keys: Vec<u64> = Vec::new();
    if changed {
        if let Some(tab) = app.tab() {
            if let Some(AnnotKind::Text { content, .. }) =
                tab.doc.session.get(id).map(|annot| &annot.kind)
            {
                for span in math_spans::closed_math_spans(content) {
                    let inner = &content[span.inner_start..span.inner_end];
                    if !inner.trim().is_empty() {
                        queue_keys.push(math_spans::span_key(inner, span.display));
                    }
                }
            }
        }
    } else if let Some((edit_id, start)) = app.tab().and_then(|tab| tab.live_math) {
        if edit_id == id {
            if let Some(tab) = app.tab() {
                if let Some(AnnotKind::Text { content, .. }) =
                    tab.doc.session.get(id).map(|annot| &annot.kind)
                {
                    if let Some(span) = math_spans::math_span_at(content, start) {
                        let inner = &content[span.inner_start..span.inner_end];
                        if span.closed && !inner.trim().is_empty() {
                            let key = math_spans::span_key(inner, span.display);
                            let needs = !app.inline_preview(id, key).is_some_and(|preview| {
                                !preview.pending
                                    && (preview.texture.is_some() || preview.error.is_some())
                            });
                            if needs {
                                queue_keys.push(key);
                            }
                        }
                    }
                }
            }
        }
    }

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
            tab.save.touch();
        }
    }
    queue_keys.sort_unstable();
    queue_keys.dedup();
    for key in queue_keys {
        app.queue_inline_math(id, key);
    }
}

fn byte_to_char_index(text: &str, byte: usize) -> usize {
    text.char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .take_while(|&i| i <= byte.min(text.len()))
        .count()
        .saturating_sub(1)
}

fn char_index_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

struct BubblePlan {
    anchor: Rect,
    state: Render,
    current: Option<(egui::TextureHandle, f32, f32)>,
    ordinal: usize,
    idle_secs: f64,
}

fn math_edit_style(ink: Color32, font_px: f32) -> Style {
    Style {
        font: FontId::new(font_px, FontFamily::Proportional),
        color: ink,
        source_color: Color32::from_rgb(ink.r() / 2 + 36, ink.g() / 2 + 64, ink.b() / 2 + 110),
        source_bg: Color32::from_rgba_unmultiplied(80, 120, 220, 36),
        error: Color32::from_rgb(210, 70, 60),
    }
}

fn cache_render(
    cache: &crate::math::MathCache<crate::app::InlineReady>,
    inner: &str,
    display: bool,
    size_pt: f32,
    color: crate::geom::Rgb,
    scale: f32,
) -> Render {
    let key = MathKey::new(inner, display, size_pt, color);
    match cache.get(&key) {
        Some(EntryKind::Ready { value, .. }) if value.w_pt > 0.5 && value.h_pt > 0.5 => {
            Render::Ready(Vec2::new(value.w_pt * scale, value.h_pt * scale))
        }
        Some(EntryKind::Error { message }) => Render::Error(message.clone()),
        Some(EntryKind::Pending { .. } | EntryKind::Ready { .. }) | None => Render::Pending,
    }
}

fn ready_texture(
    cache: &crate::math::MathCache<crate::app::InlineReady>,
    inner: &str,
    display: bool,
    size_pt: f32,
    color: crate::geom::Rgb,
) -> Option<(egui::TextureHandle, f32, f32)> {
    let key = MathKey::new(inner, display, size_pt, color);
    match cache.get(&key)? {
        EntryKind::Ready { value, .. } => value
            .texture
            .clone()
            .map(|texture| (texture, value.w_pt, value.h_pt)),
        EntryKind::Pending { .. } | EntryKind::Error { .. } => None,
    }
}

fn ready_white_texture(
    cache: &crate::math::MathCache<crate::app::InlineReady>,
    inner: &str,
    display: bool,
    size_pt: f32,
    color: crate::geom::Rgb,
) -> Option<(egui::TextureHandle, f32, f32)> {
    let key = MathKey::new(inner, display, size_pt, color);
    match cache.get(&key)? {
        EntryKind::Ready { value, .. } => value
            .white_texture
            .clone()
            .map(|texture| (texture, value.w_pt, value.h_pt)),
        EntryKind::Pending { .. } | EntryKind::Error { .. } => None,
    }
}

/// Autopair / tab-out / completion pre-filter (R7–R9). Runs before
/// `conceal_editor` so `TextEdit` never sees a handled event. Skips while IME
/// is composing. When the completion popup is open, Tab/Enter accept and
/// arrows move the highlight; Esc is handled via `reclaim_edit_after_completion_escape`.
fn apply_math_input(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    id: Id,
    content: &mut String,
    autosnippets: bool,
    annot_id: u64,
) -> Option<bool> {
    if ime_composing(ui) {
        return None;
    }
    let mut state = TextEdit::load_state(ctx, id)?;
    let range = state.cursor.char_range()?;
    let caret = range.primary.index;

    if let Some(result) = apply_completion_keys(ctx, ui, id, content, range, annot_id) {
        let text_changed = result.0 != *content;
        *content = result.0;
        state.cursor.set_char_range(Some(result.1));
        state.store(ctx, id);
        return Some(text_changed);
    }

    // While a completion list is visible, Tab/Enter are reserved for accept
    // (handled above). Skip the tab-out path so they don't fall through.
    let popup_open = completion_popup_open(ctx, id, content, caret);

    let mut hit: Option<(usize, (String, CCursorRange))> = None;
    ui.input(|input| {
        for (index, event) in input.events.iter().enumerate() {
            if popup_open
                && matches!(
                    event,
                    Event::Key {
                        key: Key::Tab | Key::Enter | Key::ArrowUp | Key::ArrowDown | Key::Escape,
                        pressed: true,
                        ..
                    }
                )
            {
                continue;
            }
            if let Some(result) = math_input::apply(content, range, event, autosnippets) {
                hit = Some((index, result));
                break;
            }
        }
    });
    let (index, (next, new_range)) = hit?;
    ui.input_mut(|input| {
        if index < input.events.len() {
            input.events.remove(index);
        }
    });
    let text_changed = next != *content;
    *content = next;
    state.cursor.set_char_range(Some(new_range));
    state.store(ctx, id);
    Some(text_changed)
}

fn completion_dismissed_id(text_edit_id: Id) -> Id {
    text_edit_id.with("math-completion-dismissed")
}

fn completion_selected_id(text_edit_id: Id) -> Id {
    text_edit_id.with("math-completion-selected")
}

#[derive(Clone)]
struct CompletionSelection {
    prefix: String,
    index: usize,
}

fn completion_session_id() -> Id {
    Id::new("marker-math-completion-session")
}

#[derive(Clone)]
struct CompletionSession {
    annot_id: u64,
    text_edit_id: Id,
    query: String,
}

fn completion_hits(
    content: &str,
    caret: usize,
) -> Option<(
    math_input::CompletionQuery,
    Vec<&'static math_input::LatexCommand>,
)> {
    let query = math_input::completion_query(content, caret)?;
    let hits = math_input::rank_completions(&query.prefix);
    if hits.is_empty() {
        None
    } else {
        Some((query, hits))
    }
}

fn completion_popup_open(
    ctx: &egui::Context,
    text_edit_id: Id,
    content: &str,
    caret: usize,
) -> bool {
    let Some((query, _)) = completion_hits(content, caret) else {
        return false;
    };
    let dismissed = ctx.data(|data| data.get_temp::<String>(completion_dismissed_id(text_edit_id)));
    dismissed.as_ref() != Some(&query.prefix)
}

fn apply_completion_keys(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    text_edit_id: Id,
    content: &str,
    range: CCursorRange,
    annot_id: u64,
) -> Option<(String, CCursorRange)> {
    if !range.is_empty() {
        return None;
    }
    let caret = range.primary.index;
    let (query, hits) = completion_hits(content, caret)?;
    let dismissed = ctx.data(|data| data.get_temp::<String>(completion_dismissed_id(text_edit_id)));
    if dismissed.as_ref() == Some(&query.prefix) {
        return None;
    }

    // Clear a stale dismiss when the query changes.
    if dismissed.is_some() {
        ctx.data_mut(|data| {
            data.remove::<String>(completion_dismissed_id(text_edit_id));
        });
    }

    let mut selected = ctx
        .data(|data| data.get_temp::<CompletionSelection>(completion_selected_id(text_edit_id)))
        .map(|sel| {
            if sel.prefix == query.prefix {
                sel.index
            } else {
                0
            }
        })
        .unwrap_or(0);
    if selected >= hits.len() {
        selected = 0;
    }

    let mut accept = false;
    let mut handled_index: Option<usize> = None;
    let mut navigated = false;
    ui.input(|input| {
        for (index, event) in input.events.iter().enumerate() {
            let Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            if !modifiers.is_none() {
                continue;
            }
            match *key {
                Key::ArrowDown => {
                    selected = (selected + 1) % hits.len();
                    handled_index = Some(index);
                    navigated = true;
                    break;
                }
                Key::ArrowUp => {
                    selected = (selected + hits.len() - 1) % hits.len();
                    handled_index = Some(index);
                    navigated = true;
                    break;
                }
                Key::Tab | Key::Enter => {
                    accept = true;
                    handled_index = Some(index);
                    break;
                }
                _ => {}
            }
        }
    });

    if let Some(index) = handled_index {
        ui.input_mut(|input| {
            if index < input.events.len() {
                input.events.remove(index);
            }
            if accept {
                input.consume_key(Modifiers::NONE, Key::Tab);
                input.consume_key(Modifiers::NONE, Key::Enter);
            } else if navigated {
                input.consume_key(Modifiers::NONE, Key::ArrowUp);
                input.consume_key(Modifiers::NONE, Key::ArrowDown);
            }
        });
    }

    ctx.data_mut(|data| {
        data.insert_temp(
            completion_selected_id(text_edit_id),
            CompletionSelection {
                prefix: query.prefix.clone(),
                index: selected,
            },
        );
        data.insert_temp(
            completion_session_id(),
            CompletionSession {
                annot_id,
                text_edit_id,
                query: query.prefix.clone(),
            },
        );
    });

    if accept {
        let cmd = hits[selected];
        ctx.data_mut(|data| {
            data.remove::<CompletionSelection>(completion_selected_id(text_edit_id));
            data.remove::<CompletionSession>(completion_session_id());
            data.remove::<String>(completion_dismissed_id(text_edit_id));
        });
        return Some(math_input::accept_completion(content, &query, cmd));
    }

    None
}

/// `handle_keys` runs before the editor and clears `editing` on Escape. When a
/// completion popup was open, put the annotation back into edit mode and treat
/// Escape as "dismiss the list" only.
fn reclaim_edit_after_completion_escape(app: &mut MarkerApp, ctx: &egui::Context) {
    if !ctx.input(|input| input.key_pressed(Key::Escape)) {
        return;
    }
    let Some(session) =
        ctx.data(|data| data.get_temp::<CompletionSession>(completion_session_id()))
    else {
        return;
    };
    let Some(tab) = app.tab_mut() else {
        return;
    };
    if tab.editing.is_some() {
        return;
    }
    tab.editing = Some(session.annot_id);
    tab.focus_edit = true;
    let text_edit_id = session.text_edit_id;
    let query = session.query.clone();
    let annot_id = session.annot_id;
    ctx.data_mut(|data| {
        data.insert_temp(completion_dismissed_id(text_edit_id), query);
        data.remove::<CompletionSession>(completion_session_id());
        data.remove::<CompletionSelection>(completion_selected_id(text_edit_id));
    });
    ctx.input_mut(|input| {
        input.consume_key(Modifiers::NONE, Key::Escape);
    });
    app.begin_edit_undo(annot_id);
}

fn paint_math_completion(
    ctx: &egui::Context,
    ui: &egui::Ui,
    text_edit_id: Id,
    annot_id: u64,
    content: &str,
    output: &EditorOutput,
) {
    let Some((index, _)) = output.caret else {
        ctx.data_mut(|data| data.remove::<CompletionSession>(completion_session_id()));
        return;
    };
    let Some((query, hits)) = completion_hits(content, index) else {
        ctx.data_mut(|data| {
            data.remove::<CompletionSession>(completion_session_id());
            data.remove::<CompletionSelection>(completion_selected_id(text_edit_id));
        });
        return;
    };
    let dismissed = ctx.data(|data| data.get_temp::<String>(completion_dismissed_id(text_edit_id)));
    if dismissed.as_ref() == Some(&query.prefix) {
        ctx.data_mut(|data| data.remove::<CompletionSession>(completion_session_id()));
        return;
    }
    if dismissed.is_some() {
        ctx.data_mut(|data| {
            data.remove::<String>(completion_dismissed_id(text_edit_id));
        });
    }

    let selected = ctx
        .data(|data| data.get_temp::<CompletionSelection>(completion_selected_id(text_edit_id)))
        .map(|sel| {
            if sel.prefix == query.prefix {
                sel.index.min(hits.len().saturating_sub(1))
            } else {
                0
            }
        })
        .unwrap_or(0);

    ctx.data_mut(|data| {
        data.insert_temp(
            completion_session_id(),
            CompletionSession {
                annot_id,
                text_edit_id,
                query: query.prefix.clone(),
            },
        );
    });

    let caret_rect = output.galley.pos_from_cursor(CCursor {
        index,
        prefer_next_row: true,
    });
    let anchor = Pos2::new(
        output.galley_pos.x + caret_rect.min.x,
        output.galley_pos.y + caret_rect.max.y + 2.0,
    );

    let visible = hits.len().min(8);
    egui::Area::new(text_edit_id.with("math-completion-popup"))
        .order(egui::Order::Tooltip)
        .fixed_pos(anchor)
        .interactable(false)
        .show(ctx, |ui| {
            Frame::popup(ui.style())
                .inner_margin(egui::Margin::symmetric(6, 4))
                .show(ui, |ui| {
                    ui.set_min_width(120.0);
                    ui.spacing_mut().item_spacing.y = 1.0;
                    for (i, cmd) in hits.iter().take(visible).enumerate() {
                        let row = format!("{}  \\{}", cmd.preview, cmd.name);
                        let rich = if i == selected {
                            egui::RichText::new(row).strong()
                        } else {
                            egui::RichText::new(row)
                        };
                        ui.label(rich);
                    }
                    if hits.len() > visible {
                        ui.label(
                            egui::RichText::new(format!("+{} more", hits.len() - visible)).weak(),
                        );
                    }
                });
        });
    // Keep the TextEdit focused; the popup must not steal keyboard focus.
    let _ = ui;
}

fn ime_composing(ui: &egui::Ui) -> bool {
    ui.input(|input| {
        input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Ime(ImeEvent::Preedit(_)) | egui::Event::Ime(ImeEvent::Enabled)
            )
        })
    })
}

fn paint_concealed_math(
    ui: &egui::Ui,
    cache: &crate::math::MathCache<crate::app::InlineReady>,
    output: &EditorOutput,
    size_pt: f32,
    color: crate::geom::Rgb,
) {
    let painter = ui.painter();
    for (rect, inner, display, span) in &output.math {
        let Some((texture, _, _)) = ready_texture(cache, inner, *display, size_pt, color) else {
            continue;
        };
        painter.image(
            texture.id(),
            *rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        if let Some((a, b)) = output.caret {
            if a != b {
                let (lo, hi) = (a.min(b), a.max(b));
                if lo <= span.start && hi >= span.end {
                    painter.rect_filled(
                        *rect,
                        0.0,
                        ui.visuals().selection.bg_fill.gamma_multiply(0.45),
                    );
                }
            }
        }
    }
}

fn paint_math_errors(ui: &egui::Ui, output: &EditorOutput, color: Color32) {
    let pointer = ui.input(|input| input.pointer.hover_pos());
    let caret = output.caret.map(|(index, _)| index);
    for (span, message) in &output.errors {
        paint_dotted_span(ui.painter(), &output.galley, output.galley_pos, span, color);
        let inside = caret.is_some_and(|index| index > span.start && index < span.end);
        if inside {
            continue;
        }
        let rect = span_screen_rect(&output.galley, output.galley_pos, span);
        if pointer.is_some_and(|pos| rect.contains(pos)) {
            egui::Tooltip::always_open(
                ui.ctx().clone(),
                ui.layer_id(),
                Id::new(("marker-math-tip", span.start, span.end)),
                rect,
            )
            .gap(6.0)
            .show(|ui| {
                ui.label(rich_text::latex_error_message(message));
            });
        }
    }
}

fn paint_dotted_span(
    painter: &egui::Painter,
    galley: &egui::Galley,
    origin: Pos2,
    span: &CharSpan,
    color: Color32,
) {
    let stroke = Stroke::new(1.0_f32, color);
    for index in span.start..span.end {
        let glyph = galley
            .pos_from_cursor(CCursor {
                index,
                prefer_next_row: true,
            })
            .translate(origin.to_vec2());
        if (index - span.start) % 2 != 0 {
            continue;
        }
        let y = glyph.max.y - 1.0;
        let x1 = (glyph.min.x + 2.0).min(glyph.max.x);
        if x1 > glyph.min.x {
            painter.line_segment([Pos2::new(glyph.min.x, y), Pos2::new(x1, y)], stroke);
        }
    }
}

fn span_screen_rect(galley: &egui::Galley, origin: Pos2, span: &CharSpan) -> Rect {
    let left = galley.pos_from_cursor(CCursor {
        index: span.start,
        prefer_next_row: true,
    });
    let right = galley.pos_from_cursor(CCursor {
        index: span.end.saturating_sub(1),
        prefer_next_row: true,
    });
    Rect::from_min_max(
        origin + left.min.to_vec2(),
        origin + Vec2::new(right.max.x, right.max.y.max(left.max.y)),
    )
}

fn bubble_plan(
    ctx: &egui::Context,
    content: &str,
    output: &EditorOutput,
    cache: &crate::math::MathCache<crate::app::InlineReady>,
    size_pt: f32,
    color: crate::geom::Rgb,
    typed: bool,
) -> Option<BubblePlan> {
    let (index, _) = output.caret?;
    let byte = char_index_to_byte(content, index);
    let span = math_spans::math_span_at(content, byte)?;
    if byte <= span.start || byte >= span.end {
        return None;
    }
    let inner = &content[span.inner_start..span.inner_end];
    let state = if inner.trim().is_empty() {
        Render::Pending
    } else {
        cache_render(cache, inner, span.display, size_pt, color, 1.0)
    };
    let now = ctx.input(|input| input.time);
    let idle_id = Id::new(("marker-math-idle", output.response.id));
    if typed || output.response.changed() {
        ctx.data_mut(|data| data.insert_temp(idle_id, now));
    }
    let idle_secs = match ctx.data(|data| data.get_temp::<f64>(idle_id)) {
        Some(start) => now - start,
        None => MATH_ERROR_IDLE_SECS,
    };
    if matches!(state, Render::Error(_)) && idle_secs < MATH_ERROR_IDLE_SECS {
        ctx.request_repaint_after(Duration::from_secs_f64(MATH_ERROR_IDLE_SECS - idle_secs));
    }
    Some(BubblePlan {
        anchor: span_screen_rect(
            &output.galley,
            output.galley_pos,
            &char_span_of(&span, content),
        ),
        state,
        current: ready_white_texture(cache, inner, span.display, size_pt, color),
        ordinal: span.index,
        idle_secs,
    })
}

fn char_span_of(span: &math_spans::MathSpanRef, content: &str) -> CharSpan {
    let (start, end) = math_spans::byte_range_char_indices(content, span.start, span.end);
    let (inner_start, inner_end) =
        math_spans::byte_range_char_indices(content, span.inner_start, span.inner_end);
    CharSpan {
        start,
        end,
        inner_start,
        inner_end,
        display: span.display,
        closed: span.closed,
    }
}

fn paint_math_bubble(
    app: &MarkerApp,
    ctx: &egui::Context,
    view: Rect,
    doc_id: DocId,
    id: u64,
    plan: &BubblePlan,
    scale: f32,
) {
    let last = app.last_good_inline(id, plan.ordinal);
    let has_last = last
        .as_ref()
        .is_some_and(|preview| preview.white_texture.is_some());
    let bubble = preview_bubble(&plan.state, has_last, plan.idle_secs);
    if bubble.image == BubbleImage::None && bubble.error.is_none() {
        return;
    }
    let shown = match bubble.image {
        BubbleImage::Current => plan
            .current
            .clone()
            .map(|(texture, w, h)| (texture, w, h, Color32::WHITE)),
        BubbleImage::LastGoodDimmed => last.and_then(|preview| {
            preview.white_texture.map(|texture| {
                (
                    texture,
                    preview.width_pt,
                    preview.height_pt,
                    Color32::from_white_alpha(150),
                )
            })
        }),
        BubbleImage::None => None,
    };
    let img_h = shown
        .as_ref()
        .map(|(_, w, h, _)| {
            let nat = Vec2::new(*w * scale, *h * scale);
            let fit = (240.0 / nat.x.max(1.0)).min(72.0 / nat.y.max(1.0)).min(1.0);
            nat.y * fit
        })
        .unwrap_or(0.0);
    let extra = if bubble.error.is_some() { 22.0 } else { 0.0 };
    let est = img_h + 16.0 + extra;
    let above = plan.anchor.min.y - est > view.min.y + 4.0;
    let pos = if above {
        Pos2::new(plan.anchor.min.x, plan.anchor.min.y - est)
    } else {
        Pos2::new(plan.anchor.min.x, plan.anchor.max.y + 4.0)
    };
    egui::Area::new(Id::new(("marker-math-bubble", doc_id, id)))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos)
        .constrain(false)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(Color32::BLACK)
                .show(ui, |ui| {
                    if let Some((texture, w, h, tint)) = shown {
                        let nat = Vec2::new(w * scale, h * scale);
                        let fit = (240.0 / nat.x.max(1.0)).min(72.0 / nat.y.max(1.0)).min(1.0);
                        let (rect, _) = ui.allocate_exact_size(nat * fit, Sense::hover());
                        ui.painter().image(
                            texture.id(),
                            rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            tint,
                        );
                    }
                    if let Some(error) = &bubble.error {
                        ui.label(
                            egui::RichText::new(error)
                                .color(Color32::from_rgb(210, 70, 60))
                                .size(12.0),
                        );
                    }
                });
        });
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
            tab.doc.doc_id,
            tab.doc.scale,
            tab.focus_edit,
            tab.previews
                .get(&id)
                .and_then(|preview| preview.error.clone()),
            annot.kind.clone(),
        )
    };
    let (id, page, doc_id, scale, focus, error, kind) = snapshot;
    match kind {
        AnnotKind::Text {
            rect, size, color, ..
        } => {
            edit_text_annot(
                app, ctx, view, id, page, doc_id, scale, focus, rect, size, color,
            );
        }
        AnnotKind::Math { rect, .. } => {
            let screen = {
                let Some(tab) = app.tab() else {
                    return;
                };
                pdf_rect_screen(&tab.doc, page, rect, view)
            };
            let mut changed = false;
            egui::Area::new(Id::new(("marker-math", doc_id, id)))
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
                    ui.label(egui::RichText::new("LaTeX").weak().size(11.0));
                    let response = ui.add(
                        TextEdit::multiline(source)
                            .font(FontId::new(13.0, FontFamily::Monospace))
                            .desired_width(screen.width().max(280.0))
                            .desired_rows(3)
                            .hint_text(r"\frac{1}{2}"),
                    );
                    if focus {
                        response.request_focus();
                    }
                    changed = response.changed();
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
                tab.live_math = None;
                if changed {
                    tab.doc.session.mark_dirty(id);
                    tab.save.touch();
                }
            }
            if changed {
                app.queue_math(id);
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
            egui::Area::new(Id::new(("marker-note", doc_id, id)))
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
                tab.live_math = None;
                if changed {
                    tab.doc.session.mark_dirty(id);
                    tab.save.touch();
                }
            }
        }
        _ => {
            if let Some(tab) = app.tab_mut() {
                tab.editing = None;
                tab.live_math = None;
            }
        }
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

fn open_context_menu(app: &mut MarkerApp, pos: Pos2, pane: PaneCtx) {
    let view = pane.rect;
    let located = app.tab_by_id(pane.tab_id).and_then(|tab| {
        let (page, point) = tab.doc.screen_to_page(pos, view)?;
        let hit = tab.doc.session.hit_test(page, point, 6.0 / tab.doc.scale);
        Some((page, point, hit))
    });
    let Some((page, point, hit)) = located else {
        return;
    };
    let can_paste = clipboard_has_image();
    if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
        tab.style_bar = None;
        if let Some(id) = hit {
            // Right-click on an annotation: target that object (keep multi if already in it).
            if !tab.is_selected(id) {
                tab.select_only(id);
            }
        }
        // Empty-page / text clicks keep any existing selection for keyboard shortcuts,
        // but the menu itself only offers actions for the click target.
        tab.menu = Some(ContextMenu {
            pos,
            page,
            point,
            hit,
            can_paste,
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

fn paint_menu(app: &mut MarkerApp, ctx: &egui::Context) {
    let Some(menu) = app.tab().and_then(|tab| tab.menu.clone()) else {
        return;
    };

    let on_annot = menu.hit.is_some();
    let selected_len = app.tab().map(|tab| tab.selected.len()).unwrap_or(0);
    let multi = menu
        .hit
        .is_some_and(|id| selected_len > 1 && app.tab().is_some_and(|tab| tab.is_selected(id)));
    let hit_kind = menu.hit.and_then(|id| {
        app.tab()
            .and_then(|tab| tab.doc.session.get(id))
            .map(|annot| annot.kind.clone())
    });

    // Object actions follow the click target only — leftover selection on empty
    // page must not surface Delete/Style for a prior multi-select.
    let can_style = if multi {
        app.tab().is_some_and(|tab| {
            tab.selected.iter().any(|id| {
                tab.doc
                    .session
                    .get(*id)
                    .is_some_and(|annot| annot.kind.is_styleable())
            })
        })
    } else {
        hit_kind.as_ref().is_some_and(|kind| kind.is_styleable())
    };
    let can_edit = !multi
        && matches!(
            hit_kind,
            Some(AnnotKind::Text { .. } | AnnotKind::Note { .. } | AnnotKind::Math { .. })
        );
    let can_delete = on_annot;
    let delete_count = if multi { selected_len } else { 1 };

    let has_learning = app
        .tab()
        .is_some_and(|tab| tab.assistant.learning.is_some());
    let has_text_sel = app.tab().is_some_and(|tab| tab.text_sel.is_some());
    let copy_text = if on_annot {
        // Annotation under the pointer (now selected) — highlight / text / note body.
        app.copyable_selection_text()
    } else {
        app.copyable_selection_text()
            .or_else(|| app.word_at_point(menu.page, menu.point))
    };
    // Assistant only when there is page/highlight text to work with — not for
    // every annotation that happens to have a copyable body (text/note).
    let on_highlight = matches!(
        hit_kind,
        Some(AnnotKind::Highlight { .. } | AnnotKind::Markup { .. })
    );
    let show_assistant_actions =
        has_learning || has_text_sel || on_highlight || (!on_annot && copy_text.is_some());
    let show_assistant_invite = !on_annot && !show_assistant_actions;
    let show_assistant = show_assistant_actions || show_assistant_invite;
    let can_attach_shot =
        has_learning || has_text_sel || on_highlight || (!on_annot && copy_text.is_some());

    let header = menu_context_header(
        hit_kind.as_ref(),
        multi,
        selected_len,
        !on_annot && copy_text.is_some() && !has_text_sel && !has_learning,
        copy_text.as_deref(),
    );

    let mut action = MenuAction::None;
    let area = egui::Area::new(Id::new("marker-context"))
        .order(egui::Order::Tooltip)
        .fixed_pos(menu.pos)
        .interactable(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(180.0);

                if let Some(header) = &header {
                    ui.label(egui::RichText::new(header).small().weak());
                    ui.add_space(2.0);
                }

                let mut wrote = false;
                if can_edit {
                    if menu_item(ui, edit_menu_label(&hit_kind), None) {
                        action = MenuAction::Edit;
                    }
                    wrote = true;
                }
                if can_style {
                    if wrote {
                        ui.separator();
                    }
                    ui.label(egui::RichText::new("Format").small().weak());
                    ui.horizontal(|ui| {
                        let _ = app.style_bar_contents(ui, false);
                    });
                    wrote = true;
                }
                if copy_text.is_some() {
                    if menu_item(ui, "Copy text", Some("Ctrl+C")) {
                        action = MenuAction::Copy;
                    }
                    let cite_ready = app.tab().is_some_and(|tab| tab.doc.glyphs.contains_key(&0));
                    if !cite_ready {
                        cite::need_page0_glyphs();
                    } else {
                        cite::clear_page0_request();
                    }
                    if menu_item_enabled(ui, "Copy quote with citation", None, cite_ready) {
                        action = MenuAction::CopyCite;
                    }
                    wrote = true;
                }
                if can_delete {
                    let label = if delete_count > 1 {
                        format!("Delete {delete_count} annotations")
                    } else {
                        "Delete".into()
                    };
                    if menu_item(ui, &label, Some("Del")) {
                        action = MenuAction::Delete;
                    }
                    wrote = true;
                }

                if menu.can_paste {
                    if wrote {
                        ui.separator();
                    }
                    if menu_item(ui, "Paste image", Some("Ctrl+Shift+V")) {
                        action = MenuAction::Paste;
                    }
                    wrote = true;
                }

                if show_assistant {
                    if wrote {
                        ui.separator();
                    }
                    ui.label(egui::RichText::new("Assistant").small().weak());
                    if show_assistant_actions {
                        if menu_item(ui, "Attach text", Some("Ctrl+Shift+A")) {
                            action = MenuAction::AttachText;
                        }
                        if can_attach_shot && menu_item(ui, "Attach screenshot", None) {
                            action = MenuAction::AttachShot;
                        }
                        if menu_item(ui, "Explain with Cursor", None) {
                            action = MenuAction::Explain;
                        }
                        if menu_item(ui, "Look up in browser", None) {
                            action = MenuAction::Lookup;
                        }
                    } else if menu_item(ui, "Select text for Assistant", Some("Ctrl+Shift+A")) {
                        action = MenuAction::AttachText;
                    }
                    wrote = true;
                }

                if wrote {
                    ui.separator();
                }
                if menu_item(ui, "Delete page", Some("Ctrl+Shift+Backspace")) {
                    action = MenuAction::DeletePage;
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
                cite::clear_page0_request();
            }
        }
        MenuAction::Paste => {
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
            let _ = app.paste_clipboard_image();
        }
        MenuAction::Copy => {
            if let Some(text) = copy_text {
                ctx.copy_text(text);
            }
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
        }
        MenuAction::CopyCite => {
            if let Some(quote) = copy_text.as_deref() {
                match quoted_citation_clipboard(app, &menu, quote) {
                    Some(payload) => ctx.copy_text(payload),
                    None => {
                        // Page-0 glyphs raced out; fall back to bare quote so the
                        // click still does something visible while glyphs reload.
                        ctx.copy_text(cite::wrap_quote(quote));
                        cite::need_page0_glyphs();
                    }
                }
            }
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
        }
        MenuAction::Edit => {
            if let Some(id) = menu.hit {
                if let Some(tab) = app.tab_mut() {
                    tab.menu = None;
                }
                cite::clear_page0_request();
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
            cite::clear_page0_request();
            app.delete_selected();
        }
        MenuAction::AttachText => {
            ensure_learning_from_menu(app, &menu);
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
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
            ensure_learning_from_menu(app, &menu);
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
            app.attach_learning_screenshot();
        }
        MenuAction::Explain => {
            ensure_learning_from_menu(app, &menu);
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
            app.explain_selection();
        }
        MenuAction::Lookup => {
            ensure_learning_from_menu(app, &menu);
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
            app.lookup_selection_in_browser();
        }
        MenuAction::DeletePage => {
            let page = menu.page;
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
            app.delete_page_at(page);
        }
    }
}

fn menu_item(ui: &mut egui::Ui, label: &str, shortcut: Option<&str>) -> bool {
    menu_item_enabled(ui, label, shortcut, true)
}

fn menu_item_enabled(
    ui: &mut egui::Ui,
    label: &str,
    shortcut: Option<&str>,
    enabled: bool,
) -> bool {
    let mut button = Button::new(label);
    if let Some(shortcut) = shortcut {
        button = button.shortcut_text(shortcut);
    }
    ui.add_enabled(enabled, button).clicked()
}

/// Build `quote` + citation from page-0 glyphs and the file name (no info dict yet).
fn quoted_citation_clipboard(app: &MarkerApp, menu: &ContextMenu, quote: &str) -> Option<String> {
    let tab = app.tab()?;
    let glyphs = tab.doc.glyphs.get(&0)?;
    let page0 = cite::page_text(glyphs);
    let file_name = tab
        .doc
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document.pdf");
    let page = citation_page(tab, menu);
    let citation = cite::citation_line(file_name, page + 1, &page0);
    Some(cite::format_quoted_citation(quote, &citation))
}

fn citation_page(tab: &Tab, menu: &ContextMenu) -> usize {
    if let Some(sel) = &tab.text_sel {
        return sel.page;
    }
    if tab.selected.len() == 1 {
        if let Some(annot) = tab.doc.session.get(tab.selected[0]) {
            return annot.page;
        }
    }
    menu.page
}

fn menu_context_header(
    kind: Option<&AnnotKind>,
    multi: bool,
    selected_len: usize,
    word_under_cursor: bool,
    copy_text: Option<&str>,
) -> Option<String> {
    if multi {
        return Some(format!("{selected_len} annotations"));
    }
    if let Some(kind) = kind {
        return Some(annot_menu_label(kind).into());
    }
    if word_under_cursor {
        if let Some(text) = copy_text {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                let preview: String = trimmed.chars().take(28).collect();
                let ellipsis = if trimmed.chars().count() > 28 {
                    "…"
                } else {
                    ""
                };
                return Some(format!("“{preview}{ellipsis}”"));
            }
        }
        return Some("Page text".into());
    }
    None
}

fn edit_menu_label(kind: &Option<AnnotKind>) -> &'static str {
    match kind {
        Some(AnnotKind::Text { .. }) => "Edit text",
        Some(AnnotKind::Note { .. }) => "Edit note",
        Some(AnnotKind::Math { .. }) => "Edit equation",
        _ => "Edit",
    }
}

/// Promote click-target text into a learning selection so Assistant actions
/// work from highlights, Select-tool ranges, or a word under the cursor.
fn ensure_learning_from_menu(app: &mut MarkerApp, menu: &ContextMenu) {
    let Some(tab) = app.tab_mut() else {
        return;
    };
    if tab.assistant.learning.is_some() {
        return;
    }
    if let Some(sel) = tab.text_sel.clone() {
        tab.assistant.learning = Some(LearningSelection {
            page: sel.page,
            glyph_lo: sel.glyph_lo,
            glyph_hi: sel.glyph_hi,
        });
        return;
    }
    if let Some(id) = menu.hit {
        let page = tab.doc.session.get(id).map(|annot| annot.page);
        let quads = tab.doc.session.get(id).and_then(|annot| match &annot.kind {
            AnnotKind::Highlight { quads, .. } | AnnotKind::Markup { quads, .. } => {
                Some(quads.clone())
            }
            _ => None,
        });
        if let (Some(page), Some(quads)) = (page, quads) {
            if let Some(glyphs) = tab.doc.glyphs.get(&page) {
                let indices = glyphs_intersecting_rects(glyphs, &quads);
                if let (Some(&lo), Some(&hi)) = (indices.first(), indices.last()) {
                    tab.assistant.learning = Some(LearningSelection {
                        page,
                        glyph_lo: lo,
                        glyph_hi: hi,
                    });
                    return;
                }
            }
        }
    }
    if menu.hit.is_none() {
        if let Some(glyphs) = tab.doc.glyphs.get(&menu.page) {
            if let Some(index) = glyph_at(glyphs, menu.point) {
                if let Some((lo, hi)) = word_range(glyphs, index) {
                    tab.assistant.learning = Some(LearningSelection {
                        page: menu.page,
                        glyph_lo: lo,
                        glyph_hi: hi,
                    });
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    None,
    Paste,
    Copy,
    CopyCite,
    Edit,
    Delete,
    DeletePage,
    AttachText,
    AttachShot,
    Explain,
    Lookup,
}

fn annot_menu_label(kind: &AnnotKind) -> &'static str {
    match kind {
        AnnotKind::Highlight { .. } => "Highlight",
        AnnotKind::Markup { style, .. } => match style {
            MarkupStyle::Underline => "Underline",
            MarkupStyle::StrikeOut => "Strikeout",
            MarkupStyle::Squiggly => "Squiggly",
        },
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
        AnnotKind::Foreign { kind, .. } => match kind {
            crate::annot::ForeignKind::Underline => "Underline",
            crate::annot::ForeignKind::StrikeOut => "Strikeout",
            crate::annot::ForeignKind::Squiggly => "Squiggly",
            crate::annot::ForeignKind::Ink => "Ink",
            crate::annot::ForeignKind::Polygon => "Polygon",
            crate::annot::ForeignKind::PolyLine => "Polyline",
            crate::annot::ForeignKind::Caret => "Caret",
            crate::annot::ForeignKind::FileAttachment => "Attachment",
        },
    }
}

#[cfg(test)]
mod place_answer_tests {
    use super::{rect_beside_bounds, PageInfo, PdfRect};

    #[test]
    fn beside_prefers_right_when_space_allows() {
        let page = PageInfo {
            x0: 0.0,
            y0: 0.0,
            x1: 600.0,
            y1: 800.0,
        };
        let bounds = PdfRect::new(40.0, 100.0, 120.0, 140.0);
        let rect = rect_beside_bounds(bounds, &page, 200.0, 30.0);
        assert!((rect.x0 - 128.0).abs() < f32::EPSILON);
        assert!((rect.y0 - 100.0).abs() < f32::EPSILON);
        assert!((rect.width() - 200.0).abs() < f32::EPSILON);
    }

    #[test]
    fn beside_falls_below_when_right_overflows() {
        let page = PageInfo {
            x0: 0.0,
            y0: 0.0,
            x1: 200.0,
            y1: 800.0,
        };
        let bounds = PdfRect::new(40.0, 100.0, 180.0, 140.0);
        let rect = rect_beside_bounds(bounds, &page, 200.0, 30.0);
        assert!((rect.x0 - 0.0).abs() < f32::EPSILON);
        assert!((rect.y0 - 148.0).abs() < f32::EPSILON);
    }
}
