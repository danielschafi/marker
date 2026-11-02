//! Document paint: tiles, annotations, drag preview, scrollbar (RF11a).

use std::time::Instant;

use egui::{Color32, FontFamily, FontId, Id, Pos2, Rect, Sense, Stroke, Vec2};

use super::geometry::{pdf_rect_screen, snap_rect_to_pixels, visible_pages};
use super::PaneCtx;
use crate::annot::{highlight_quads, AnnotKind, MarkupStyle, ShapeKind, HIGHLIGHT_OPACITY};
use crate::app::{DocState, Drag, MarkerApp, Tab};
use crate::geom::{PdfPoint, PdfRect, Rgb};
use crate::math::rich_text::{self, LaidRun, MathMetrics, TextMeasure};
use crate::math_spans;

pub(super) fn paint_document(
    app: &mut MarkerApp,
    painter: &egui::Painter,
    pane: PaneCtx,
    pixels_per_point: f32,
) {
    let view = pane.rect;
    let Some(tab_idx) = app.tab_index(pane.tab_id) else {
        return;
    };
    if app.tabs[tab_idx].doc.pages.is_empty() {
        return;
    }
    if let Some(tab) = app.tabs.get_mut(tab_idx) {
        tab.ensure_annot_index();
    }
    let frame = app.tile_frame;
    let highlight_color = app.settings.highlight_color;
    let page_bg = if app.settings.sepia {
        Color32::from_rgb(244, 236, 216)
    } else {
        Color32::WHITE
    };
    let (render_scale, page_range) = {
        let tab = app.tabs.get(tab_idx).expect("checked");
        (
            tab.doc.render_scale(pixels_per_point),
            visible_pages(&tab.doc, view),
        )
    };
    let (first, last) = page_range;
    for page in first..=last {
        let rect = app
            .tabs
            .get(tab_idx)
            .expect("checked")
            .doc
            .page_rect(page, view);
        let shadow = rect.expand(2.0).translate(Vec2::new(0.0, 4.0));
        painter.rect_filled(shadow, 6.0, Color32::from_black_alpha(28));
        painter.rect_filled(rect, 1.0, page_bg);
        // Highlights underpaint paper so glyphs (opaque tile ink) stay readable.
        let tab = app.tabs.get(tab_idx).expect("checked");
        paint_highlight_fills(tab, highlight_color, painter, page, view);
        if let Some(tab) = app.tabs.get_mut(tab_idx) {
            paint_tiles(
                &mut tab.doc,
                painter,
                page,
                view,
                render_scale,
                pixels_per_point,
                frame,
            );
        }
        let tab = app.tabs.get(tab_idx).expect("checked");
        paint_search_hits(tab, painter, page, view);
        paint_annotations(tab, &app.math.cache, painter, page, view);
    }
    paint_drag_preview(app, painter, pane);
    paint_learning_selection(app, painter, pane);
}

/// Marker-tint fill used for on-screen highlights (underpainted before tiles).
fn highlight_fill(color: crate::geom::Rgb) -> Color32 {
    let c = color.to_color32();
    let a = (HIGHLIGHT_OPACITY * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

fn paint_highlight_fills(
    tab: &Tab,
    highlight_color: crate::geom::Rgb,
    painter: &egui::Painter,
    page: usize,
    view: Rect,
) {
    let page_rect = tab.doc.page_rect(page, view);
    let painter = painter.with_clip_rect(page_rect.intersect(view));
    let indices = tab
        .annot_by_page
        .get(page)
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    for &index in indices {
        let annot = &tab.doc.session.annotations[index];
        if let AnnotKind::Highlight { quads, color } = &annot.kind {
            let fill = highlight_fill(*color);
            for quad in quads {
                painter.rect_filled(pdf_rect_screen(&tab.doc, page, *quad, view), 1.0, fill);
            }
        }
    }
    // Drag preview for the highlight tool also belongs under the ink.
    // Stroke tools preview on top of the tiles instead.
    if let Some(Drag::Highlight {
        page: drag_page,
        anchor,
        current,
        origin,
        current_pt,
        word_lo,
        word_hi,
        style: None,
        ..
    }) = &tab.drag
    {
        if *drag_page != page {
            return;
        }
        let fill = highlight_fill(highlight_color);
        for rect in mark_drag_quads(
            tab,
            *drag_page,
            *anchor,
            *current,
            *origin,
            *current_pt,
            (*word_lo, *word_hi),
        ) {
            painter.rect_filled(pdf_rect_screen(&tab.doc, page, rect, view), 1.0, fill);
        }
    }
}

pub(crate) fn text_mark_kind(
    style: Option<MarkupStyle>,
    quads: Vec<PdfRect>,
    color: Rgb,
) -> AnnotKind {
    match style {
        Some(style) => AnnotKind::Markup {
            style,
            quads,
            color,
        },
        None => AnnotKind::Highlight { quads, color },
    }
}

pub(crate) fn mark_drag_quads(
    tab: &Tab,
    page: usize,
    anchor: Option<usize>,
    current: Option<usize>,
    origin: PdfPoint,
    current_pt: PdfPoint,
    word: (Option<usize>, Option<usize>),
) -> Vec<PdfRect> {
    let (word_lo, word_hi) = word;
    let range = match (anchor, current, word_lo, word_hi) {
        (Some(a), Some(c), Some(wlo), Some(whi)) => Some((a.min(c).min(wlo), a.max(c).max(whi))),
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
}

fn paint_markup_strokes(
    painter: &egui::Painter,
    doc: &DocState,
    page: usize,
    view: Rect,
    style: MarkupStyle,
    quads: &[PdfRect],
    color: Rgb,
) {
    let color32 = color.to_color32();
    let width = (1.45 * doc.scale).max(1.15);
    let stroke = Stroke::new(width, color32);
    for quad in quads {
        let screen = pdf_rect_screen(doc, page, *quad, view);
        if screen.width() < 0.5 {
            continue;
        }
        match style {
            MarkupStyle::Underline => {
                let y = screen.bottom() - width * 0.35;
                painter.line_segment(
                    [Pos2::new(screen.left(), y), Pos2::new(screen.right(), y)],
                    stroke,
                );
            }
            MarkupStyle::StrikeOut => {
                let y = screen.center().y;
                painter.line_segment(
                    [Pos2::new(screen.left(), y), Pos2::new(screen.right(), y)],
                    stroke,
                );
            }
            MarkupStyle::Squiggly => {
                paint_squiggle(
                    painter,
                    screen.left(),
                    screen.right(),
                    screen.bottom() - width * 0.35,
                    doc.scale,
                    stroke,
                );
            }
        }
    }
}

fn paint_squiggle(
    painter: &egui::Painter,
    left: f32,
    right: f32,
    y: f32,
    scale: f32,
    stroke: Stroke,
) {
    let amp = (1.45 * scale).max(1.1);
    let wavelength = (8.0 * scale).max(5.0);
    let step = (wavelength / 8.0).max(1.0);
    let mut pts = Vec::new();
    let mut x = left;
    while x < right {
        let t = (x - left) / wavelength * std::f32::consts::TAU;
        pts.push(Pos2::new(x, y + amp * t.sin()));
        x += step;
    }
    let t = (right - left) / wavelength * std::f32::consts::TAU;
    pts.push(Pos2::new(right, y + amp * t.sin()));
    if pts.len() >= 2 {
        painter.add(egui::Shape::line(pts, stroke));
    }
}

fn paint_tiles(
    doc: &mut DocState,
    painter: &egui::Painter,
    page: usize,
    view: Rect,
    render_scale: f32,
    pixels_per_point: f32,
    frame: u64,
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
    let mut draws = Vec::new();
    for (key, tile) in tiles {
        let x0 = tile.x as f32 / tile.scale;
        let y0 = tile.y as f32 / tile.scale;
        let x1 = (tile.x as f32 + tile.w as f32) / tile.scale;
        let y1 = (tile.y as f32 + tile.h as f32) / tile.scale;
        let min = doc.page_to_screen(page, PdfPoint::new(x0, y0), view);
        let max = doc.page_to_screen(page, PdfPoint::new(x1, y1), view);
        let dest = snap_rect_to_pixels(Rect::from_min_max(min, max), pixels_per_point);
        if dest.intersects(view) {
            draws.push((*key, dest, tile.texture.id()));
        }
    }
    for (key, dest, texture) in draws {
        if let Some(tile) = doc.tiles.get_mut(&key) {
            tile.last_used_frame = frame;
        }
        painter.image(
            texture,
            dest,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
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

fn paint_annotations(
    tab: &Tab,
    cache: &crate::math::MathCache<crate::math::InlineReady>,
    painter: &egui::Painter,
    page: usize,
    view: Rect,
) {
    let page_rect = tab.doc.page_rect(page, view);
    let painter = painter.with_clip_rect(page_rect.intersect(view));
    let indices = tab
        .annot_by_page
        .get(page)
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    for &index in indices {
        let annot = &tab.doc.session.annotations[index];
        let selected = tab.is_selected(annot.id);
        let editing = tab.editing == Some(annot.id);
        match &annot.kind {
            // Highlight fills are underpainted before tiles; only selection chrome remains here.
            AnnotKind::Highlight { .. } => {}
            AnnotKind::Markup {
                style,
                quads,
                color,
            } => {
                paint_markup_strokes(&painter, &tab.doc, page, view, *style, quads, *color);
            }
            AnnotKind::Text {
                rect,
                content,
                size,
                color,
            } => {
                // While editing, the text field paints this box, math included.
                if !editing {
                    let screen = pdf_rect_screen(&tab.doc, page, *rect, view);
                    let text_painter = painter.with_clip_rect(screen);
                    if math_spans::has_math(content) {
                        paint_rich_text(
                            &text_painter,
                            painter.ctx(),
                            cache,
                            screen,
                            content,
                            *size,
                            *color,
                            tab.doc.scale,
                            tab.doc.last_zoom,
                        );
                    } else {
                        paint_wrapped(
                            &text_painter,
                            painter.ctx(),
                            screen,
                            content,
                            *size * tab.doc.scale,
                            color.to_color32(),
                            tab.doc.last_zoom,
                        );
                    }
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
                            tab.doc.last_zoom,
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
                        tab.doc.last_zoom,
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
            AnnotKind::Foreign {
                kind,
                rect,
                quads,
                strokes,
                color,
                ..
            } => {
                paint_foreign(
                    &painter, &tab.doc, page, view, *kind, *rect, quads, strokes, *color,
                );
            }
        }
        if selected {
            if let Some(bounds) = annot.bounds() {
                let screen = pdf_rect_screen(&tab.doc, page, bounds, view);
                painter.rect_stroke(
                    screen,
                    1.0,
                    Stroke::new(1.0_f32, Color32::from_rgb(70, 130, 220)),
                    egui::StrokeKind::Outside,
                );
                for (_, point) in annot.kind.handles() {
                    let at = tab.doc.page_to_screen(page, point, view);
                    let handle = Rect::from_center_size(at, Vec2::splat(8.0));
                    painter.rect_filled(handle, 1.0, Color32::WHITE);
                    painter.rect_stroke(
                        handle,
                        1.0,
                        Stroke::new(1.0_f32, Color32::from_rgb(70, 130, 220)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
    }
}

// RF11 packs foreign annot paint args into a context struct.
#[allow(clippy::too_many_arguments)]
fn paint_foreign(
    painter: &egui::Painter,
    doc: &DocState,
    page: usize,
    view: Rect,
    kind: crate::annot::ForeignKind,
    rect: PdfRect,
    quads: &[PdfRect],
    strokes: &[Vec<PdfPoint>],
    color: crate::geom::Rgb,
) {
    use crate::annot::ForeignKind;
    let color32 = color.to_color32();
    let stroke = Stroke::new((1.25 * doc.scale).max(1.0), color32);
    let boxes: Vec<PdfRect> = if quads.is_empty() {
        vec![rect]
    } else {
        quads.to_vec()
    };
    match kind {
        ForeignKind::Underline | ForeignKind::StrikeOut | ForeignKind::Squiggly => {
            for quad in boxes {
                let screen = pdf_rect_screen(doc, page, quad, view);
                let y = if kind == ForeignKind::StrikeOut {
                    screen.center().y
                } else {
                    screen.bottom()
                };
                if kind == ForeignKind::Squiggly {
                    let mut pts = Vec::new();
                    let mut x = screen.left();
                    let mut up = true;
                    while x < screen.right() {
                        let y2 = if up { y - 2.0 } else { y + 1.0 };
                        pts.push(Pos2::new(x, y2));
                        x += 4.0;
                        up = !up;
                    }
                    pts.push(Pos2::new(screen.right(), y));
                    if pts.len() >= 2 {
                        painter.add(egui::Shape::line(pts, stroke));
                    }
                } else {
                    painter.line_segment(
                        [Pos2::new(screen.left(), y), Pos2::new(screen.right(), y)],
                        stroke,
                    );
                }
            }
        }
        ForeignKind::Ink | ForeignKind::Polygon | ForeignKind::PolyLine => {
            if strokes.is_empty() {
                let screen = pdf_rect_screen(doc, page, rect, view);
                painter.rect_stroke(screen, 0.0, stroke, egui::StrokeKind::Inside);
            }
            for path in strokes {
                let pts: Vec<Pos2> = path
                    .iter()
                    .map(|point| doc.page_to_screen(page, *point, view))
                    .collect();
                if pts.len() >= 2 {
                    let mut draw = pts;
                    if kind == ForeignKind::Polygon {
                        draw.push(draw[0]);
                    }
                    painter.add(egui::Shape::line(draw, stroke));
                }
            }
        }
        ForeignKind::Caret | ForeignKind::FileAttachment => {
            let screen = pdf_rect_screen(doc, page, rect, view);
            painter.rect_stroke(screen, 2.0, stroke, egui::StrokeKind::Inside);
            if kind == ForeignKind::FileAttachment {
                painter.text(
                    screen.center(),
                    egui::Align2::CENTER_CENTER,
                    "📎",
                    FontId::new((screen.height() * 0.6).max(10.0), FontFamily::Proportional),
                    color32,
                );
            }
        }
    }
}

pub(crate) fn fit_math(box_rect: Rect, nat_w: f32, nat_h: f32, scale: f32) -> Rect {
    if nat_w <= 1.0 || nat_h <= 1.0 {
        return box_rect;
    }
    let nat = Vec2::new(nat_w * scale, nat_h * scale);
    let fit = (box_rect.width() / nat.x).min(box_rect.height() / nat.y);
    let size = nat * fit.max(0.01);
    Rect::from_min_size(box_rect.min, size)
}

fn paint_learning_selection(app: &MarkerApp, painter: &egui::Painter, pane: PaneCtx) {
    let view = pane.rect;
    let Some(tab) = app.tab_by_id(pane.tab_id) else {
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

fn paint_drag_preview(app: &MarkerApp, painter: &egui::Painter, pane: PaneCtx) {
    let view = pane.rect;
    let Some(tab) = app.tab_by_id(pane.tab_id) else {
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
            let stroke = Stroke::new(1.5_f32, Color32::from_rgb(80, 160, 255));
            let rect = pdf_rect_screen(
                &tab.doc,
                *page,
                PdfRect::from_points(*origin, *current),
                view,
            );
            painter.rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
            painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(80, 160, 255, 40));
        }
        Some(Drag::Highlight {
            style: Some(style),
            page,
            anchor,
            current,
            origin,
            current_pt,
            word_lo,
            word_hi,
            ..
        }) => {
            let quads = mark_drag_quads(
                tab,
                *page,
                *anchor,
                *current,
                *origin,
                *current_pt,
                (*word_lo, *word_hi),
            );
            paint_markup_strokes(
                painter,
                &tab.doc,
                *page,
                view,
                *style,
                &quads,
                app.settings.highlight_color,
            );
        }
        Some(Drag::Highlight { .. }) => {
            // Highlight fill is painted in `paint_highlight_fills` under the page tiles.
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
                Stroke::new(1.0_f32, Color32::from_rgb(70, 130, 220)),
                egui::StrokeKind::Inside,
            );
        }
        _ => {}
    }
}

/// While a zoom gesture is active, snap paint font sizes to a coarse step so
/// egui does not rasterize a new glyph atlas entry every frame. Exact size
/// returns once `last_zoom` ages past the interacting window in `update`.
const ZOOM_FONT_STEP_PX: f32 = 2.0;
const ZOOM_FONT_COARSE_MS: u128 = 150;

pub(crate) fn paint_font_px(size_px: f32, last_zoom: Instant) -> f32 {
    if last_zoom.elapsed().as_millis() < ZOOM_FONT_COARSE_MS {
        (size_px / ZOOM_FONT_STEP_PX).round() * ZOOM_FONT_STEP_PX
    } else {
        size_px
    }
}

pub(crate) fn paint_wrapped(
    painter: &egui::Painter,
    ctx: &egui::Context,
    rect: Rect,
    text: &str,
    size_px: f32,
    color: Color32,
    last_zoom: Instant,
) {
    if text.is_empty() || rect.width() < 2.0 {
        return;
    }
    let size_px = paint_font_px(size_px, last_zoom);
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

/// Egui font widths in PDF points. Paint and save share this so a 5 pt
/// annotation is not measured at 8 px on screen and 6 pt in the file.
pub(crate) struct EguiMeasure<'a> {
    pub(crate) ctx: &'a egui::Context,
}

impl TextMeasure for EguiMeasure<'_> {
    fn width(&self, text: &str, size_pt: f32) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        self.ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    text.to_owned(),
                    FontId::new(size_pt.max(0.5), FontFamily::Proportional),
                    Color32::PLACEHOLDER,
                )
                .size()
                .x
        })
    }

    fn line_height(&self, size_pt: f32) -> f32 {
        self.font_metrics(size_pt).0
    }

    fn ascent(&self, size_pt: f32) -> f32 {
        self.font_metrics(size_pt).1
    }
}

impl EguiMeasure<'_> {
    /// `(line_height, ascent)` from one glyph so both callers use the same font.
    fn font_metrics(&self, size_pt: f32) -> (f32, f32) {
        self.ctx.fonts_mut(|fonts| {
            let galley = fonts.layout_no_wrap(
                "x".to_owned(),
                FontId::new(size_pt.max(0.5), FontFamily::Proportional),
                Color32::PLACEHOLDER,
            );
            match galley.rows.first().and_then(|row| row.glyphs.first()) {
                Some(glyph) => (glyph.font_height, glyph.font_ascent),
                None => (size_pt * 1.25, size_pt * 0.8),
            }
        })
    }
}

pub(crate) fn cached_math_metrics(
    cache: &crate::math::MathCache<crate::math::InlineReady>,
    inner: &str,
    display: bool,
    size_pt: f32,
    color: crate::geom::Rgb,
) -> Option<MathMetrics> {
    let key = crate::math::MathKey::new(inner, display, size_pt, color);
    match cache.get(&key) {
        Some(crate::math::EntryKind::Ready { value, .. })
            if value.w_pt > 1.0 && value.h_pt > 1.0 =>
        {
            Some(MathMetrics {
                w_pt: value.w_pt,
                h_pt: value.h_pt,
                baseline_pt: value.baseline_pt,
            })
        }
        Some(
            crate::math::EntryKind::Ready { .. }
            | crate::math::EntryKind::Pending { .. }
            | crate::math::EntryKind::Error { .. },
        )
        | None => None,
    }
}

// RF11 packs rich-text paint style into a context struct.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_rich_text(
    painter: &egui::Painter,
    ctx: &egui::Context,
    cache: &crate::math::MathCache<crate::math::InlineReady>,
    screen: Rect,
    content: &str,
    size_pt: f32,
    color: crate::geom::Rgb,
    scale: f32,
    last_zoom: Instant,
) {
    let ink = color.to_color32();
    let scale = scale.max(1e-3);
    // Layout stays in PDF points (shared with save). Only the paint FontId is
    // coarsened while zooming so glyph atlas churn stays bounded mid-gesture.
    let font_px = paint_font_px(size_pt * scale, last_zoom);
    let measure = EguiMeasure { ctx };
    let width_pt = screen.width() / scale;
    let runs =
        rich_text::layout_rich_text(content, size_pt, width_pt, &measure, &|inner, display| {
            cached_math_metrics(cache, inner, display, size_pt, color)
        });
    for run in runs {
        match run {
            LaidRun::Prose { text, x, y, .. } => {
                if text.is_empty() {
                    continue;
                }
                let galley = ctx.fonts_mut(|fonts| {
                    fonts.layout_no_wrap(
                        text,
                        FontId::new(font_px.max(0.5), FontFamily::Proportional),
                        ink,
                    )
                });
                painter.galley(
                    Pos2::new(screen.min.x + x * scale, screen.min.y + y * scale),
                    galley,
                    ink,
                );
            }
            LaidRun::Math {
                inner,
                display,
                x,
                y,
                w,
                h,
                ..
            } => {
                let dest = Rect::from_min_size(
                    Pos2::new(screen.min.x + x * scale, screen.min.y + y * scale),
                    Vec2::new(w * scale, h * scale),
                );
                let ready = cache.get(&crate::math::MathKey::new(&inner, display, size_pt, color));
                if let Some(crate::math::EntryKind::Ready { value, .. }) = ready {
                    if let Some(texture) = value.texture.as_ref() {
                        let fitted = fit_math(dest, value.w_pt, value.h_pt, scale);
                        painter.image(
                            texture.id(),
                            fitted,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                        continue;
                    }
                }
                let fallback = if display {
                    format!("$${inner}$$")
                } else {
                    format!("${inner}$")
                };
                paint_wrapped(
                    painter,
                    ctx,
                    dest,
                    &fallback,
                    font_px.max(0.5) * 0.85,
                    ink,
                    last_zoom,
                );
            }
        }
    }
}

pub(super) fn paint_scrollbar(app: &mut MarkerApp, ui: &mut egui::Ui, pane: PaneCtx) {
    let view = pane.rect;
    let scroll_id = pane.tab_id;
    let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
        return;
    };
    let height = tab.doc.doc_height_px();
    if height <= view.height() + 1.0 {
        return;
    }

    // Match egui's floating ScrollArea look (same as the empty homepage).
    let scroll_style = ui.spacing().scroll;
    let hovering_view = ui.rect_contains_pointer(view);
    let outer_margin = 4.0;
    let max_w = scroll_style
        .bar_width
        .max(scroll_style.floating_width)
        .max(6.0);
    let track = Rect::from_min_max(
        Pos2::new(
            view.right() - max_w - outer_margin,
            view.top() + outer_margin,
        ),
        Pos2::new(view.right() - outer_margin, view.bottom() - outer_margin),
    );

    let response = ui.interact(
        track,
        Id::new(("marker-scroll", scroll_id)),
        Sense::click_and_drag(),
    );
    let hovering_bar = response.hovered() || response.dragged();

    let bar_t = ui
        .ctx()
        .animate_bool_responsive(Id::new(("marker-scroll-bar", scroll_id)), hovering_bar);
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
        visuals
            .extreme_bg_color
            .gamma_multiply(background_opacity * show_t),
    );

    // Small tick marks for pages that have a Find hit (FT12). Skip when the
    // search UI is closed so the bar stays clean while browsing.
    if tab.search.open && !tab.search.hits.is_empty() && height > 1.0 {
        let tick = Color32::from_rgba_unmultiplied(255, 170, 60, (150.0 * show_t) as u8);
        let mut marked = Vec::new();
        for (page, _) in &tab.search.hits {
            if marked.contains(page) {
                continue;
            }
            marked.push(*page);
            let Some(top) = tab.doc.tops.get(*page).copied() else {
                continue;
            };
            let y = bar_rect.top()
                + ((top * tab.doc.scale) / height).clamp(0.0, 1.0) * bar_rect.height();
            ui.painter().line_segment(
                [
                    Pos2::new(bar_rect.left(), y),
                    Pos2::new(bar_rect.right(), y),
                ],
                Stroke::new(1.0_f32, tick),
            );
        }
    }

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
