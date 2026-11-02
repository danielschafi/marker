use mupdf::color::AnnotationColor;
use mupdf::{DestinationKind, Outline, Point, Quad, Rect, StructuredText, TextBlockContent};

use crate::annot::{Glyph, PageGlyphs};
use crate::geom::{PdfPoint, PdfRect, Rgb};

use super::engine::OutlineNode;

pub(crate) fn to_rect(rect: PdfRect) -> Rect {
    Rect::new(rect.x0, rect.y0, rect.x1, rect.y1)
}

pub(crate) fn from_rect(rect: Rect) -> PdfRect {
    PdfRect::new(rect.x0, rect.y0, rect.x1, rect.y1)
}

pub(crate) fn glyphs_from_structured(text: &StructuredText) -> PageGlyphs {
    let mut glyphs = Vec::new();
    let mut line_bounds = Vec::new();
    let mut line_id = 0u32;
    let mut word_id = 0u32;
    for block in &text.blocks {
        let TextBlockContent::Text { lines } = &block.content else {
            continue;
        };
        for line in lines {
            line_bounds.push(from_rect(line.bounds));
            let mut in_word = false;
            for ch in &line.chars {
                if ch.ch.is_whitespace() {
                    in_word = false;
                    continue;
                }
                if !in_word {
                    word_id = word_id.saturating_add(1);
                    in_word = true;
                }
                glyphs.push(Glyph {
                    ch: ch.ch,
                    bounds: quad_bounds(ch.quad.clone()),
                    line: line_id,
                    word: word_id,
                });
            }
            line_id = line_id.saturating_add(1);
        }
    }
    PageGlyphs::new(glyphs, line_bounds)
}

/// Exact (case-sensitive) substring search over structured page text.
///
/// MuPDF's Rust `TextPage::search` always ignores case; this walks chars in
/// reading order and unions each match's glyph quads into one hit rect.
pub(crate) fn search_structured_exact(text: &StructuredText, needle: &str) -> Vec<PdfRect> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut haystack: Vec<(char, PdfRect)> = Vec::new();
    for block in &text.blocks {
        let TextBlockContent::Text { lines } = &block.content else {
            continue;
        };
        for line in lines {
            for ch in &line.chars {
                haystack.push((ch.ch, quad_bounds(ch.quad.clone())));
            }
        }
    }
    let n = needle.len();
    let mut hits = Vec::new();
    if haystack.len() < n {
        return hits;
    }
    for start in 0..=(haystack.len() - n) {
        if haystack[start..start + n]
            .iter()
            .zip(needle.iter())
            .all(|((ch, _), want)| ch == want)
        {
            let mut xs = Vec::with_capacity(n * 2);
            let mut ys = Vec::with_capacity(n * 2);
            for (_, rect) in &haystack[start..start + n] {
                xs.push(rect.x0);
                xs.push(rect.x1);
                ys.push(rect.y0);
                ys.push(rect.y1);
            }
            hits.push(PdfRect::from_minmax(xs, ys));
        }
    }
    hits
}

pub(crate) fn to_point(point: PdfPoint) -> Point {
    Point::new(point.x, point.y)
}

pub(crate) fn rect_quad(rect: PdfRect) -> Quad {
    Quad::new(
        Point::new(rect.x0, rect.y0),
        Point::new(rect.x1, rect.y0),
        Point::new(rect.x0, rect.y1),
        Point::new(rect.x1, rect.y1),
    )
}

pub(crate) fn quad_bounds(quad: Quad) -> PdfRect {
    PdfRect::from_minmax(
        [quad.ul.x, quad.ur.x, quad.ll.x, quad.lr.x],
        [quad.ul.y, quad.ur.y, quad.ll.y, quad.lr.y],
    )
}

pub(crate) fn rgb_color(color: Rgb) -> AnnotationColor {
    let [red, green, blue] = color.to_unit();
    AnnotationColor::Rgb { red, green, blue }
}

pub(crate) fn color_rgb(color: AnnotationColor) -> Option<Rgb> {
    match color {
        AnnotationColor::Rgb { red, green, blue } => Some(Rgb::from_unit([red, green, blue])),
        AnnotationColor::Gray(value) => Some(Rgb::from_unit([value, value, value])),
        AnnotationColor::Cmyk { .. } => None,
    }
}

pub(crate) fn convert_outline(nodes: Vec<Outline>) -> Vec<OutlineNode> {
    nodes.into_iter().map(convert_outline_node).collect()
}

pub(crate) fn convert_outline_node(node: Outline) -> OutlineNode {
    let (page, y) = match node.dest {
        Some(dest) => {
            let y = match dest.kind {
                DestinationKind::XYZ { top, .. } => top,
                DestinationKind::FitH { top } | DestinationKind::FitBH { top } => top,
                DestinationKind::FitR { top, .. } => Some(top),
                _ => None,
            };
            (Some(dest.loc.page_number as usize), y)
        }
        None => (None, None),
    };
    OutlineNode {
        title: node.title,
        page,
        y,
        children: convert_outline(node.down),
    }
}
