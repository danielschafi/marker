use crate::geom::{dist_to_segment, PdfPoint, PdfRect, Rgb};

/// One selectable glyph. Highlights snap to these instead of whole words.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    pub ch: char,
    pub bounds: PdfRect,
    pub line_bounds: PdfRect,
    pub line: u32,
    pub word: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    Rect,
    Ellipse,
    Line,
}

/// Reserved for later tools. They are not created by the UI yet.
#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub enum FutureKind {
    Ink {
        color: Rgb,
        width: f32,
        strokes: Vec<Vec<PdfPoint>>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum AnnotKind {
    Highlight {
        quads: Vec<PdfRect>,
        color: Rgb,
    },
    Text {
        rect: PdfRect,
        content: String,
        size: f32,
        color: Rgb,
    },
    Note {
        rect: PdfRect,
        content: String,
        color: Rgb,
    },
    Shape {
        kind: ShapeKind,
        rect: PdfRect,
        start: PdfPoint,
        end: PdfPoint,
        stroke: Rgb,
        fill: Option<Rgb>,
        width: f32,
    },
    Math {
        rect: PdfRect,
        source: String,
        size: f32,
        color: Rgb,
        /// When set, a finished render replaces the box with the equation's natural size.
        auto_size: bool,
    },
    Image {
        rect: PdfRect,
        /// RGBA8 pixels (`width * height * 4`).
        rgba: std::sync::Arc<[u8]>,
        width: u32,
        height: u32,
    },
    /// Ink lands here later without a session rewrite.
    #[allow(dead_code)]
    Future(FutureKind),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub id: u64,
    pub page: usize,
    pub xref: Option<i32>,
    pub dirty: bool,
    /// Bumped on every edit so a save can ignore annotations changed while it was in flight.
    pub revision: u64,
    pub kind: AnnotKind,
}

impl Annotation {
    pub fn bounds(&self) -> Option<PdfRect> {
        self.kind.bounds()
    }
}

impl AnnotKind {
    pub fn bounds(&self) -> Option<PdfRect> {
        match self {
            Self::Highlight { quads, .. } => {
                let first = *quads.first()?;
                Some(quads.iter().skip(1).fold(first, |acc, q| {
                    PdfRect::new(
                        acc.x0.min(q.x0),
                        acc.y0.min(q.y0),
                        acc.x1.max(q.x1),
                        acc.y1.max(q.y1),
                    )
                }))
            }
            Self::Text { rect, .. }
            | Self::Note { rect, .. }
            | Self::Math { rect, .. }
            | Self::Image { rect, .. } => Some(*rect),
            Self::Shape {
                kind: ShapeKind::Line,
                start,
                end,
                ..
            } => Some(PdfRect::from_points(*start, *end).inflate(2.0)),
            Self::Shape { rect, .. } => Some(*rect),
            Self::Future(FutureKind::Ink { strokes, .. }) => {
                let mut pts = strokes.iter().flatten();
                let first = *pts.next()?;
                let mut rect = PdfRect::new(first.x, first.y, first.x, first.y);
                for p in pts {
                    rect.x0 = rect.x0.min(p.x);
                    rect.y0 = rect.y0.min(p.y);
                    rect.x1 = rect.x1.max(p.x);
                    rect.y1 = rect.y1.max(p.y);
                }
                Some(rect.inflate(2.0))
            }
        }
    }

    pub fn translate(&mut self, dx: f32, dy: f32) {
        match self {
            Self::Highlight { quads, .. } => {
                for q in quads {
                    *q = q.translate(dx, dy);
                }
            }
            Self::Text { rect, .. }
            | Self::Note { rect, .. }
            | Self::Math { rect, .. }
            | Self::Image { rect, .. } => {
                *rect = rect.translate(dx, dy);
            }
            Self::Shape {
                kind: ShapeKind::Line,
                start,
                end,
                rect,
                ..
            } => {
                *start = PdfPoint::new(start.x + dx, start.y + dy);
                *end = PdfPoint::new(end.x + dx, end.y + dy);
                *rect = rect.translate(dx, dy);
            }
            Self::Shape {
                rect, start, end, ..
            } => {
                *rect = rect.translate(dx, dy);
                *start = PdfPoint::new(start.x + dx, start.y + dy);
                *end = PdfPoint::new(end.x + dx, end.y + dy);
            }
            Self::Future(FutureKind::Ink { strokes, .. }) => {
                for stroke in strokes {
                    for p in stroke {
                        p.x += dx;
                        p.y += dy;
                    }
                }
            }
        }
    }

    pub fn hit(&self, p: PdfPoint, slop: f32) -> bool {
        match self {
            Self::Highlight { quads, .. } => quads.iter().any(|q| q.inflate(slop).contains(p)),
            Self::Shape {
                kind: ShapeKind::Line,
                start,
                end,
                ..
            } => dist_to_segment(p, *start, *end) <= slop.max(3.0),
            _ => self
                .bounds()
                .map(|r| r.inflate(slop).contains(p))
                .unwrap_or(false),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub annotations: Vec<Annotation>,
    pub next_id: u64,
    pub epoch: u64,
    pub pending_deletes: Vec<(usize, i32)>,
}

impl Session {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            annotations: Vec::new(),
            next_id: 1,
            epoch: 0,
            pending_deletes: Vec::new(),
        }
    }

    pub fn from_imported(annotations: Vec<Annotation>) -> Self {
        let next_id = annotations.iter().map(|a| a.id).max().unwrap_or(0) + 1;
        Self {
            annotations,
            next_id,
            epoch: 0,
            pending_deletes: Vec::new(),
        }
    }

    pub fn insert(&mut self, page: usize, kind: AnnotKind) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.epoch += 1;
        self.annotations.push(Annotation {
            id,
            page,
            xref: None,
            dirty: true,
            revision: self.epoch,
            kind,
        });
        id
    }

    pub fn get(&self, id: u64) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Annotation> {
        self.annotations.iter_mut().find(|a| a.id == id)
    }

    pub fn mark_dirty(&mut self, id: u64) {
        self.epoch += 1;
        let revision = self.epoch;
        if let Some(annot) = self.get_mut(id) {
            annot.dirty = true;
            annot.revision = revision;
        }
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let Some(index) = self.annotations.iter().position(|a| a.id == id) else {
            return false;
        };
        let annot = self.annotations.remove(index);
        if let Some(xref) = annot.xref {
            self.pending_deletes.push((annot.page, xref));
        }
        true
    }

    pub fn hit_test(&self, page: usize, p: PdfPoint, slop: f32) -> Option<u64> {
        self.annotations
            .iter()
            .rev()
            .find(|a| a.page == page && a.kind.hit(p, slop))
            .map(|a| a.id)
    }

    /// Select-tool marquee: ids on `page` whose bounds center lies inside `rect`.
    pub fn ids_centered_in(&self, page: usize, rect: PdfRect) -> Vec<u64> {
        self.annotations
            .iter()
            .filter(|annot| annot.page == page)
            .filter_map(|annot| {
                let bounds = annot.bounds()?;
                rect.contains(bounds.center()).then_some(annot.id)
            })
            .collect()
    }

    pub fn is_dirty(&self) -> bool {
        !self.pending_deletes.is_empty() || self.annotations.iter().any(|a| a.dirty)
    }

    /// After a blank page is inserted at `at`, bump later page indices.
    pub fn shift_pages_from(&mut self, at: usize) {
        for annot in &mut self.annotations {
            if annot.page >= at {
                annot.page += 1;
            }
        }
        for (page, _) in &mut self.pending_deletes {
            if *page >= at {
                *page += 1;
            }
        }
    }

    /// Inverse of [`Self::shift_pages_from`] after deleting the page at `at`.
    /// Annotations that lived on the removed page are dropped (the page is gone).
    pub fn unshift_pages_from(&mut self, at: usize) {
        self.annotations.retain(|annot| annot.page != at);
        for annot in &mut self.annotations {
            if annot.page > at {
                annot.page -= 1;
            }
        }
        self.pending_deletes.retain(|(page, _)| *page != at);
        for (page, _) in &mut self.pending_deletes {
            if *page > at {
                *page -= 1;
            }
        }
    }
}

/// Put `snap` back in place of `current`, keeping xrefs learned since the snapshot
/// and scheduling deletes for annotations that disappeared.
pub fn restore_session(current: &Session, mut snap: Session) -> Session {
    for annot in &current.annotations {
        let Some(xref) = annot.xref else { continue };
        let kept = snap.annotations.iter().any(|item| item.id == annot.id);
        if !kept && !snap.pending_deletes.contains(&(annot.page, xref)) {
            snap.pending_deletes.push((annot.page, xref));
        }
    }
    for annot in &mut snap.annotations {
        match current.annotations.iter().find(|item| item.id == annot.id) {
            Some(live) => {
                annot.xref = live.xref.or(annot.xref);
                if live.kind != annot.kind || live.page != annot.page {
                    annot.dirty = true;
                }
            }
            None => annot.dirty = true,
        }
    }
    snap.epoch = snap.epoch.max(current.epoch).saturating_add(1);
    snap
}

pub fn glyph_at(glyphs: &[Glyph], point: PdfPoint) -> Option<usize> {
    glyphs
        .iter()
        .enumerate()
        .rev()
        .find(|(_, glyph)| glyph.bounds.inflate(1.0).contains(point))
        .map(|(index, _)| index)
        .or_else(|| nearest_glyph(glyphs, point))
}

fn nearest_glyph(glyphs: &[Glyph], point: PdfPoint) -> Option<usize> {
    let mut best = None;
    let mut best_d = 8.0_f32;
    for (index, glyph) in glyphs.iter().enumerate() {
        let cx = (glyph.bounds.x0 + glyph.bounds.x1) * 0.5;
        let cy = (glyph.bounds.y0 + glyph.bounds.y1) * 0.5;
        let d = (point.x - cx).hypot(point.y - cy);
        if d < best_d {
            best_d = d;
            best = Some(index);
        }
    }
    best
}

pub fn word_range(glyphs: &[Glyph], index: usize) -> Option<(usize, usize)> {
    let word = glyphs.get(index)?.word;
    let lo = glyphs.iter().position(|glyph| glyph.word == word)?;
    let hi = glyphs.iter().rposition(|glyph| glyph.word == word)?;
    Some((lo, hi))
}

/// Merge selected glyphs into one quad per line so highlights read as a marker stroke.
pub fn highlight_quads(glyphs: &[Glyph], a: usize, b: usize) -> Vec<PdfRect> {
    if glyphs.is_empty() || a >= glyphs.len() || b >= glyphs.len() {
        return Vec::new();
    }
    let (lo, hi) = (a.min(b), a.max(b));
    let mut quads = Vec::new();
    let mut i = lo;
    while i <= hi {
        let line = glyphs[i].line;
        let mut x0 = glyphs[i].bounds.x0;
        let mut x1 = glyphs[i].bounds.x1;
        let mut y0 = glyphs[i].line_bounds.y0;
        let mut y1 = glyphs[i].line_bounds.y1;
        i += 1;
        while i <= hi && glyphs[i].line == line {
            x0 = x0.min(glyphs[i].bounds.x0);
            x1 = x1.max(glyphs[i].bounds.x1);
            y0 = y0.min(glyphs[i].line_bounds.y0);
            y1 = y1.max(glyphs[i].line_bounds.y1);
            i += 1;
        }
        quads.push(PdfRect::new(x0, y0, x1, y1));
    }
    quads
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Nw,
    Ne,
    Sw,
    Se,
    LineStart,
    LineEnd,
}

impl AnnotKind {
    pub fn handles(&self) -> Vec<(Handle, PdfPoint)> {
        match self {
            Self::Shape {
                kind: ShapeKind::Line,
                start,
                end,
                ..
            } => vec![(Handle::LineStart, *start), (Handle::LineEnd, *end)],
            Self::Text { rect, .. }
            | Self::Math { rect, .. }
            | Self::Image { rect, .. }
            | Self::Shape { rect, .. } => {
                vec![
                    (Handle::Nw, PdfPoint::new(rect.x0, rect.y0)),
                    (Handle::Ne, PdfPoint::new(rect.x1, rect.y0)),
                    (Handle::Sw, PdfPoint::new(rect.x0, rect.y1)),
                    (Handle::Se, PdfPoint::new(rect.x1, rect.y1)),
                ]
            }
            _ => Vec::new(),
        }
    }

    pub fn resize(&mut self, handle: Handle, to: PdfPoint) {
        let min = 8.0;
        match self {
            Self::Shape {
                kind: ShapeKind::Line,
                start,
                end,
                rect,
                ..
            } => {
                match handle {
                    Handle::LineStart => *start = to,
                    Handle::LineEnd => *end = to,
                    _ => {}
                }
                *rect = PdfRect::from_points(*start, *end);
            }
            Self::Text { rect, .. }
            | Self::Math { rect, .. }
            | Self::Image { rect, .. }
            | Self::Shape { rect, .. } => {
                let mut x0 = rect.x0;
                let mut y0 = rect.y0;
                let mut x1 = rect.x1;
                let mut y1 = rect.y1;
                match handle {
                    Handle::Nw => {
                        x0 = to.x.min(x1 - min);
                        y0 = to.y.min(y1 - min);
                    }
                    Handle::Ne => {
                        x1 = to.x.max(x0 + min);
                        y0 = to.y.min(y1 - min);
                    }
                    Handle::Sw => {
                        x0 = to.x.min(x1 - min);
                        y1 = to.y.max(y0 + min);
                    }
                    Handle::Se => {
                        x1 = to.x.max(x0 + min);
                        y1 = to.y.max(y0 + min);
                    }
                    Handle::LineStart | Handle::LineEnd => {}
                }
                *rect = PdfRect::new(x0, y0, x1, y1);
                if let Self::Math { auto_size, .. } = self {
                    *auto_size = false;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(ch: char, x: f32, line: u32, word: u32) -> Glyph {
        Glyph {
            ch,
            bounds: PdfRect::new(x, 10.0, x + 6.0, 20.0),
            line_bounds: PdfRect::new(0.0, 8.0, 200.0, 22.0),
            line,
            word,
        }
    }

    #[test]
    fn highlights_merge_into_one_quad_per_line() {
        let glyphs = vec![
            glyph('a', 0.0, 0, 1),
            glyph('b', 6.0, 0, 1),
            glyph('c', 14.0, 0, 2),
            glyph('d', 0.0, 1, 3),
        ];
        let quads = highlight_quads(&glyphs, 0, 3);
        assert_eq!(quads.len(), 2);
        assert!((quads[0].x0 - 0.0).abs() < f32::EPSILON);
        assert!((quads[0].x1 - 20.0).abs() < f32::EPSILON);
        assert_eq!(quads[1].x0, 0.0);
        assert_eq!(word_range(&glyphs, 2), Some((2, 2)));
        assert_eq!(word_range(&glyphs, 1), Some((0, 1)));
    }

    #[test]
    fn undo_restores_text_and_records_a_delete() {
        let mut session = Session::new();
        let before = session.clone();
        let id = session.insert(
            0,
            AnnotKind::Text {
                rect: PdfRect::new(0.0, 0.0, 40.0, 20.0),
                content: "hi".into(),
                size: 12.0,
                color: Rgb::new(0, 0, 0),
            },
        );
        session.get_mut(id).unwrap().xref = Some(9);
        let restored = restore_session(&session, before);
        assert!(restored.annotations.is_empty());
        assert_eq!(restored.pending_deletes, vec![(0, 9)]);
    }

    #[test]
    fn marquee_selects_by_center_point() {
        let mut session = Session::new();
        // Center (25, 25) — inside marquee.
        let inside = session.insert(
            0,
            AnnotKind::Text {
                rect: PdfRect::new(0.0, 0.0, 50.0, 50.0),
                content: "in".into(),
                size: 12.0,
                color: Rgb::new(0, 0, 0),
            },
        );
        // Overlaps marquee but center (75, 25) is outside.
        let _overlap = session.insert(
            0,
            AnnotKind::Shape {
                kind: ShapeKind::Rect,
                rect: PdfRect::new(50.0, 0.0, 100.0, 50.0),
                start: PdfPoint::new(50.0, 0.0),
                end: PdfPoint::new(100.0, 50.0),
                stroke: Rgb::new(0, 0, 0),
                fill: None,
                width: 1.0,
            },
        );
        // Far away.
        let _out = session.insert(
            0,
            AnnotKind::Text {
                rect: PdfRect::new(200.0, 200.0, 220.0, 220.0),
                content: "out".into(),
                size: 12.0,
                color: Rgb::new(0, 0, 0),
            },
        );
        // Image whose center is inside — selectable for move/delete.
        let image = session.insert(
            0,
            AnnotKind::Image {
                rect: PdfRect::new(10.0, 10.0, 30.0, 30.0),
                rgba: std::sync::Arc::from([0u8; 4].as_slice()),
                width: 1,
                height: 1,
            },
        );
        // Same geometry, other page — ignored.
        let _other_page = session.insert(
            1,
            AnnotKind::Text {
                rect: PdfRect::new(0.0, 0.0, 50.0, 50.0),
                content: "page1".into(),
                size: 12.0,
                color: Rgb::new(0, 0, 0),
            },
        );

        let marquee = PdfRect::new(0.0, 0.0, 60.0, 60.0);
        assert_eq!(session.ids_centered_in(0, marquee), vec![inside, image]);
        assert!(session.ids_centered_in(0, PdfRect::new(90.0, 90.0, 95.0, 95.0)).is_empty());
    }

    #[test]
    fn page_shift_and_unshift_roundtrip() {
        let mut session = Session::new();
        session.insert(
            0,
            AnnotKind::Text {
                rect: PdfRect::new(0.0, 0.0, 10.0, 10.0),
                content: "a".into(),
                size: 12.0,
                color: Rgb::new(0, 0, 0),
            },
        );
        session.insert(
            1,
            AnnotKind::Text {
                rect: PdfRect::new(0.0, 0.0, 10.0, 10.0),
                content: "b".into(),
                size: 12.0,
                color: Rgb::new(0, 0, 0),
            },
        );
        session.shift_pages_from(1);
        assert_eq!(session.annotations[0].page, 0);
        assert_eq!(session.annotations[1].page, 2);
        session.insert(
            1,
            AnnotKind::Text {
                rect: PdfRect::new(0.0, 0.0, 10.0, 10.0),
                content: "new".into(),
                size: 12.0,
                color: Rgb::new(0, 0, 0),
            },
        );
        session.unshift_pages_from(1);
        assert_eq!(session.annotations.len(), 2);
        assert_eq!(session.annotations[0].page, 0);
        assert_eq!(session.annotations[1].page, 1);
        match &session.annotations[1].kind {
            AnnotKind::Text { content, .. } => assert_eq!(content, "b"),
            other => panic!("expected text annot, got {other:?}"),
        }
    }
}
