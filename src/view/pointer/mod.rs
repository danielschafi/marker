//! Pointer input: begin/update/end primary drag phases (RF11b).

use std::time::{Duration, Instant};

use egui::{CursorIcon, PointerButton, Pos2, Rect};

use super::commit::{commit_drag, place_box, resize_target};
use super::PaneCtx;
use crate::annot::{glyph_at, word_range, AnnotKind, MarkupStyle, ShapeKind};
use crate::app::{CreateKind, Drag, MarkerApp, Tab, Tool};
use crate::assistant::CaptureMode;
use crate::geom::PdfPoint;

mod click;

pub(super) fn handle_pointer(app: &mut MarkerApp, response: &egui::Response, pane: PaneCtx) {
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
            super::menu::open_context_menu(app, pos, pane);
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
            click::click(app, pos, pane, response.double_clicked());
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
        Tool::Rect | Tool::Ellipse | Tool::Line | Tool::Arrow => {
            tab.drag = Some(Drag::Shape {
                page,
                kind: match tool {
                    Tool::Ellipse => ShapeKind::Ellipse,
                    Tool::Line => ShapeKind::Line,
                    Tool::Arrow => ShapeKind::Arrow,
                    _ => ShapeKind::Rect,
                },
                origin: point,
                current: point,
            });
        }
        Tool::Draw => {
            tab.drag = Some(Drag::Ink {
                page,
                points: vec![point],
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
        Drag::Ink { page, mut points } => {
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
                return;
            };
            let Some(point) = tab
                .doc
                .screen_to_page(pos, view)
                .filter(|(hit, _)| *hit == page)
                .map(|(_, point)| point)
            else {
                return;
            };
            const MIN_STEP: f32 = 1.0;
            let far_enough = points.last().is_none_or(|last| {
                (point.x - last.x).hypot(point.y - last.y) >= MIN_STEP
            });
            if far_enough {
                points.push(point);
            }
            tab.drag = Some(Drag::Ink { page, points });
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
        click::click(app, pos, pane, double);
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
                click::click(app, pos, pane, double);
                if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
                    if !ids.is_empty() {
                        tab.select_many(ids.clone());
                    }
                }
            }
            _ => click::click(app, pos, pane, double),
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
        Drag::Ink { page, points } => {
            let Some(first) = points.first() else {
                return false;
            };
            let Some(last) = points.last() else {
                return false;
            };
            let a = tab.doc.page_to_screen(*page, *first, view);
            let b = tab.doc.page_to_screen(*page, *last, view);
            points.len() >= 2 && a.distance(b) > 3.0
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
