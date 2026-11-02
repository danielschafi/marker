//! Document view geometry: scroll, zoom, page mapping, and tile wants (RF11a).

use std::time::Instant;

use egui::{Pos2, Rect, Vec2};

use crate::app::{DocState, Tool};
use crate::cite;
use crate::geom::{tile_render_scale, PdfPoint, PdfRect, MAX_SCALE, MIN_SCALE};
use crate::pdf::{PageInfo, TILE_PX};

pub(crate) const GAP: f32 = 16.0;
pub(crate) const PAD: f32 = 24.0;

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

    pub(crate) fn page_origin(&self, page: usize, view: Rect) -> Pos2 {
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

    pub(crate) fn page_rect(&self, page: usize, view: Rect) -> Rect {
        let origin = self.page_origin(page, view);
        let info = self.pages[page];
        Rect::from_min_size(
            origin,
            Vec2::new(info.width() * self.scale, info.height() * self.scale),
        )
    }

    pub(crate) fn page_to_screen(&self, page: usize, point: PdfPoint, view: Rect) -> Pos2 {
        let origin = self.page_origin(page, view);
        let info = self.pages[page];
        Pos2::new(
            origin.x + (point.x - info.x0) * self.scale,
            origin.y + (point.y - info.y0) * self.scale,
        )
    }

    pub(crate) fn screen_to_page(&self, screen: Pos2, view: Rect) -> Option<(usize, PdfPoint)> {
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

pub(crate) fn visible_pages(doc: &DocState, view: Rect) -> (usize, usize) {
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

/// Align tile quads to the physical pixel grid to avoid subpixel resampling blur.
pub(crate) fn snap_rect_to_pixels(rect: Rect, pixels_per_point: f32) -> Rect {
    let ppp = pixels_per_point.max(0.5);
    let snap = |v: f32| (v * ppp).round() / ppp;
    Rect::from_min_max(
        Pos2::new(snap(rect.min.x), snap(rect.min.y)),
        Pos2::new(snap(rect.max.x), snap(rect.max.y)),
    )
}

pub(crate) fn pdf_rect_screen(doc: &DocState, page: usize, rect: PdfRect, view: Rect) -> Rect {
    let min = doc.page_to_screen(page, PdfPoint::new(rect.x0, rect.y0), view);
    let max = doc.page_to_screen(page, PdfPoint::new(rect.x1, rect.y1), view);
    Rect::from_min_max(min, max)
}

pub(crate) struct TileRequest {
    pub key: crate::app::TileKey,
    pub scale: f32,
    pub prefetch: bool,
    pub distance: u32,
}

pub(crate) struct TileWant {
    pub tiles: Vec<TileRequest>,
    pub words: Vec<usize>,
}

fn tile_distance_sq(
    doc: &DocState,
    page: usize,
    col: i32,
    row: i32,
    scale: f32,
    view: Rect,
) -> u32 {
    let rect = doc.page_rect(page, view);
    let cx = view.center().x;
    let cy = view.center().y;
    let tile_cx =
        rect.left() + (col as f32 * TILE_PX as f32 + TILE_PX as f32 * 0.5) / scale * doc.scale;
    let tile_cy =
        rect.top() + (row as f32 * TILE_PX as f32 + TILE_PX as f32 * 0.5) / scale * doc.scale;
    let dx = tile_cx - cx;
    let dy = tile_cy - cy;
    dx.mul_add(dx, dy * dy).min(f32::MAX) as u32
}

// RF11 packs band/tile collection params into a context struct.
#[allow(clippy::too_many_arguments)]
fn collect_tiles_in_band(
    doc: &DocState,
    inflight: &std::collections::HashSet<crate::app::TileKey>,
    view: Rect,
    band: Rect,
    render_scale: f32,
    bits: u32,
    prefetch: bool,
    seen: &mut std::collections::HashSet<crate::app::TileKey>,
    out: &mut Vec<TileRequest>,
) {
    if band.width() < 1.0 || band.height() < 1.0 {
        return;
    }
    for page in 0..doc.pages.len() {
        let rect = doc.page_rect(page, view);
        let visible = rect.intersect(band);
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
                if doc.tiles.contains_key(&key) || inflight.contains(&key) || !seen.insert(key) {
                    continue;
                }
                out.push(TileRequest {
                    key,
                    scale,
                    prefetch,
                    distance: tile_distance_sq(doc, page, col, row, scale, view),
                });
            }
        }
    }
}

pub(crate) fn wanted_tiles(
    doc: &mut DocState,
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
    let scroll_dir = doc.scroll_y - doc.scroll_y_prev;
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
    }
    // ST9: citation menu needs page-0 text for DOI/arXiv; request via the
    // existing glyph job when the menu has asked (no app.rs plumbing).
    if cite::page0_glyphs_needed() && !doc.glyphs.contains_key(&0) && !want.words.contains(&0) {
        want.words.push(0);
    }
    if scroll_dir < 0.0 {
        if first > 0 && !doc.glyphs.contains_key(&(first - 1)) && !want.words.contains(&(first - 1))
        {
            want.words.push(first - 1);
        }
    } else if last + 1 < doc.pages.len()
        && !doc.glyphs.contains_key(&(last + 1))
        && !want.words.contains(&(last + 1))
    {
        want.words.push(last + 1);
    }
    let mut seen = std::collections::HashSet::new();
    collect_tiles_in_band(
        doc,
        inflight,
        view,
        view,
        render_scale,
        bits,
        false,
        &mut seen,
        &mut want.tiles,
    );
    let prefetch_band = if scroll_dir < 0.0 {
        view.translate(Vec2::new(0.0, -view.height()))
    } else {
        view.translate(Vec2::new(0.0, view.height()))
    };
    collect_tiles_in_band(
        doc,
        inflight,
        view,
        prefetch_band,
        render_scale,
        bits,
        true,
        &mut seen,
        &mut want.tiles,
    );
    doc.scroll_y_prev = doc.scroll_y;
    want
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annot::Session;
    use crate::pdf::DocId;
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::path::PathBuf;

    fn page(w: f32, h: f32) -> PageInfo {
        PageInfo {
            x0: 0.0,
            y0: 0.0,
            x1: w,
            y1: h,
        }
    }

    fn fake_doc(pages: Vec<PageInfo>, scale: f32) -> DocState {
        let tops = DocState::rebuild_tops(&pages);
        DocState {
            path: PathBuf::from("test.pdf"),
            doc_id: DocId(1),
            pages,
            tops,
            outline: Vec::new(),
            session: Session::new(),
            scale,
            scroll_x: 0.0,
            scroll_y: 0.0,
            scroll_y_prev: 0.0,
            fitted: true,
            last_zoom: Instant::now(),
            last_scroll: Instant::now(),
            last_fit: None,
            glyphs: HashMap::new(),
            glyph_order: VecDeque::new(),
            tiles: HashMap::new(),
        }
    }

    fn view_rect(w: f32, h: f32) -> Rect {
        Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(w, h))
    }

    #[test]
    fn clamp_scroll_bounds_to_doc() {
        let mut doc = fake_doc(vec![page(200.0, 400.0)], 2.0);
        let view = view_rect(100.0, 100.0);
        doc.scroll_y = 10_000.0;
        doc.scroll_x = 10_000.0;
        doc.clamp_scroll(view);
        let max_y = (doc.doc_height_px() - view.height()).max(0.0);
        let max_page = doc.pages.iter().map(PageInfo::width).fold(0.0, f32::max);
        let max_x = (max_page * doc.scale + PAD * 2.0 - view.width()).max(0.0);
        assert!((doc.scroll_y - max_y).abs() < 1e-3);
        assert!((doc.scroll_x - max_x).abs() < 1e-3);
        doc.scroll_y = -50.0;
        doc.scroll_x = -50.0;
        doc.clamp_scroll(view);
        assert_eq!(doc.scroll_y, 0.0);
        assert_eq!(doc.scroll_x, 0.0);
    }

    #[test]
    fn current_page_tracks_scroll_midpoint() {
        let doc = fake_doc(
            vec![page(100.0, 200.0), page(100.0, 200.0), page(100.0, 200.0)],
            1.0,
        );
        // tops: PAD, PAD+200+GAP, ...
        assert_eq!(doc.current_page(100.0), 0);
        let mut doc = doc;
        doc.scroll_y = doc.tops[1] * doc.scale + 10.0;
        assert_eq!(doc.current_page(100.0), 1);
        doc.scroll_y = doc.tops[2] * doc.scale + 10.0;
        assert_eq!(doc.current_page(100.0), 2);
    }

    #[test]
    fn jump_to_sets_scroll_near_page() {
        let mut doc = fake_doc(vec![page(100.0, 300.0), page(100.0, 300.0)], 1.0);
        let view = view_rect(200.0, 200.0);
        doc.jump_to(1, Some(40.0), view);
        let expected = (doc.tops[1] + 40.0) * doc.scale - 12.0;
        // clamp may reduce if near end; with two tall pages it should stick.
        assert!(
            (doc.scroll_y - expected.clamp(0.0, (doc.doc_height_px() - view.height()).max(0.0)))
                .abs()
                < 1e-3
        );
    }

    #[test]
    fn screen_to_page_round_trips_page_to_screen() {
        let doc = fake_doc(vec![page(300.0, 400.0)], 1.5);
        let view = view_rect(500.0, 600.0);
        let pdf = PdfPoint::new(30.0, 50.0);
        let screen = doc.page_to_screen(0, pdf, view);
        let (page, back) = doc.screen_to_page(screen, view).expect("hit");
        assert_eq!(page, 0);
        assert!((back.x - pdf.x).abs() < 1e-3);
        assert!((back.y - pdf.y).abs() < 1e-3);
    }

    #[test]
    fn zoom_at_keeps_page_point_under_cursor() {
        // Narrow view so the page overflows both before and after zoom; the
        // centered-fit branch would otherwise rewrite scroll_x independently.
        let mut doc = fake_doc(vec![page(400.0, 500.0)], 1.0);
        let view = view_rect(200.0, 200.0);
        doc.scroll_x = 40.0;
        doc.scroll_y = 30.0;
        let cursor = doc.page_to_screen(0, PdfPoint::new(80.0, 120.0), view);
        let before = doc.screen_to_page(cursor, view).expect("hit");
        doc.zoom_at(2.0, cursor, view);
        assert!((doc.scale - 2.0).abs() < 1e-3);
        let after = doc.screen_to_page(cursor, view).expect("still hit");
        assert_eq!(after.0, before.0);
        assert!(
            (after.1.x - before.1.x).abs() < 0.05,
            "x drift {} -> {}",
            before.1.x,
            after.1.x
        );
        assert!(
            (after.1.y - before.1.y).abs() < 0.05,
            "y drift {} -> {}",
            before.1.y,
            after.1.y
        );
    }

    #[test]
    fn wanted_tiles_requests_visible_page_words_and_tiles() {
        let mut doc = fake_doc(vec![page(612.0, 792.0)], 1.0);
        let view = view_rect(400.0, 500.0);
        let inflight = HashSet::new();
        let want = wanted_tiles(&mut doc, &inflight, Tool::Select, view, 1.0);
        assert!(want.words.contains(&0), "visible page without glyphs");
        assert!(!want.tiles.is_empty(), "visible page should want tiles");
        assert!(want.tiles.iter().all(|t| !t.prefetch));
        assert_eq!(doc.scroll_y_prev, doc.scroll_y);
    }
}
