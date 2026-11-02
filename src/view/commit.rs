//! Drag commit, placement, and resize hit-testing (RF11b).

use std::time::Instant;

use egui::{Pos2, Rect};

use super::paint::{mark_drag_quads, text_mark_kind};
use super::PaneCtx;
use crate::annot::{highlight_quads, AnnotKind, Handle, MarkupStyle, ShapeKind};
use crate::app::{CreateKind, DocState, Drag, MarkerApp, Tab, TextSel, Tool};
use crate::assistant::{CaptureMode, LearningSelection};
use crate::geom::{PdfPoint, PdfRect};
use crate::pdf::PageInfo;

pub(super) fn commit_drag(app: &mut MarkerApp, drag: Drag, pane: PaneCtx) {
    match drag {
        Drag::LearningSelect {
            page,
            anchor,
            current,
            ..
        } => commit_learning_select(app, pane, page, anchor, current),
        Drag::TextSelect {
            page,
            anchor,
            current,
            ..
        } => commit_text_select(app, pane, page, anchor, current),
        Drag::Marquee {
            page,
            origin,
            current,
        } => commit_marquee(app, pane, page, origin, current),
        Drag::Region {
            page,
            origin,
            current,
        } => commit_region(app, pane, page, origin, current),
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
        } => commit_highlight(
            app, pane, page, anchor, current, origin, current_pt, word_lo, word_hi, replace, style,
        ),
        Drag::Shape {
            page,
            kind,
            origin,
            current,
        } => commit_shape(app, pane, page, kind, origin, current),
        Drag::Ink { page, points } => commit_ink(app, pane, page, points),
        Drag::Create {
            page,
            origin,
            current,
            kind,
        } => commit_create(app, pane, page, origin, current, kind),
        Drag::Move {
            ids,
            origins,
            moved,
            ..
        } => commit_move(app, pane, ids, origins, moved),
        Drag::Resize { id, .. } => commit_resize(app, pane, id),
        Drag::Pan { .. } => commit_pan(),
    }
}

fn commit_learning_select(
    app: &mut MarkerApp,
    pane: PaneCtx,
    page: usize,
    anchor: Option<usize>,
    current: Option<usize>,
) {
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

fn commit_text_select(
    app: &mut MarkerApp,
    pane: PaneCtx,
    page: usize,
    anchor: Option<usize>,
    current: Option<usize>,
) {
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

fn commit_marquee(
    app: &mut MarkerApp,
    pane: PaneCtx,
    page: usize,
    origin: PdfPoint,
    current: PdfPoint,
) {
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

fn commit_region(
    app: &mut MarkerApp,
    _pane: PaneCtx,
    page: usize,
    origin: PdfPoint,
    current: PdfPoint,
) {
    let rect = PdfRect::from_points(origin, current);
    if rect.is_empty() {
        return;
    }
    app.capture = CaptureMode::None;
    app.set_assistant_open(true);
    app.request_crop(page, rect);
}

#[allow(clippy::too_many_arguments)] // mirrors Drag::Highlight fields
fn commit_highlight(
    app: &mut MarkerApp,
    pane: PaneCtx,
    page: usize,
    anchor: Option<usize>,
    current: Option<usize>,
    origin: PdfPoint,
    current_pt: PdfPoint,
    word_lo: Option<usize>,
    word_hi: Option<usize>,
    replace: Option<u64>,
    style: Option<MarkupStyle>,
) {
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

fn commit_shape(
    app: &mut MarkerApp,
    pane: PaneCtx,
    page: usize,
    kind: ShapeKind,
    origin: PdfPoint,
    current: PdfPoint,
) {
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

fn commit_ink(app: &mut MarkerApp, pane: PaneCtx, page: usize, points: Vec<PdfPoint>) {
    if points.len() < 2 {
        return;
    }
    let Some(first) = points.first() else {
        return;
    };
    let Some(last) = points.last() else {
        return;
    };
    if (first.x - last.x).hypot(first.y - last.y) < 4.0 && points.len() < 3 {
        return;
    }
    let color = app.settings.shape_color;
    let width = app.settings.shape_width;
    app.seal_then_arm();
    let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
        return;
    };
    let id = tab.doc.session.insert(
        page,
        AnnotKind::Ink {
            strokes: vec![points],
            color,
            width,
        },
    );
    tab.select_only(id);
    tab.save.touch();
    app.seal_undo();
    app.open_style_bar(id, true);
}

fn commit_create(
    app: &mut MarkerApp,
    pane: PaneCtx,
    page: usize,
    origin: PdfPoint,
    current: PdfPoint,
    kind: CreateKind,
) {
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

fn commit_move(
    app: &mut MarkerApp,
    pane: PaneCtx,
    ids: Vec<u64>,
    origins: Vec<(u64, AnnotKind)>,
    moved: bool,
) {
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

fn commit_resize(app: &mut MarkerApp, pane: PaneCtx, id: u64) {
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

fn commit_pan() {}

pub(super) fn place_box(
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
        | AnnotKind::Ink { .. }
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

pub(super) fn resize_target(
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
        | Tool::Line
        | Tool::Draw => false,
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

#[cfg(test)]
mod place_answer_tests {
    use super::rect_beside_bounds;
    use crate::geom::PdfRect;
    use crate::pdf::PageInfo;

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
