use crate::annot::Glyph;
use crate::geom::PdfRect;

pub const MAX_TEXT_CHARS: usize = 20_000;

/// Reconstruct reading-order text from a contiguous glyph index range.
pub fn reconstruct_text(glyphs: &[Glyph], lo: usize, hi: usize) -> String {
    if glyphs.is_empty() || lo >= glyphs.len() || hi >= glyphs.len() {
        return String::new();
    }
    let (lo, hi) = (lo.min(hi), lo.max(hi));
    let mut out = String::new();
    let mut prev_line = glyphs[lo].line;
    let mut prev_word = glyphs[lo].word;
    for (i, glyph) in glyphs[lo..=hi].iter().enumerate() {
        if i > 0 {
            if glyph.line != prev_line {
                out.push('\n');
            } else if glyph.word != prev_word {
                out.push(' ');
            }
        }
        if !glyph.ch.is_control() || glyph.ch == '\n' || glyph.ch == '\t' {
            out.push(glyph.ch);
        }
        prev_line = glyph.line;
        prev_word = glyph.word;
    }
    out
}

/// Glyphs whose bounds intersect any of the given rectangles.
#[allow(dead_code)]
pub fn glyphs_intersecting_rects(glyphs: &[Glyph], rects: &[PdfRect]) -> Vec<usize> {
    glyphs
        .iter()
        .enumerate()
        .filter(|(_, g)| rects.iter().any(|r| rects_overlap(g.bounds, *r)))
        .map(|(i, _)| i)
        .collect()
}

#[allow(dead_code)]
fn rects_overlap(a: PdfRect, b: PdfRect) -> bool {
    a.x0 < b.x1 && a.x1 > b.x0 && a.y0 < b.y1 && a.y1 > b.y0
}

/// Cap text length; returns (text, truncated).
pub fn truncate_text(text: &str, max_chars: usize) -> (String, bool) {
    let count = text.chars().count();
    if count <= max_chars {
        return (text.to_string(), false);
    }
    let truncated: String = text.chars().take(max_chars).collect();
    (truncated, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::PdfRect;

    fn g(ch: char, line: u32, word: u32, x: f32) -> Glyph {
        Glyph {
            ch,
            bounds: PdfRect::new(x, 0.0, x + 5.0, 10.0),
            line_bounds: PdfRect::new(0.0, 0.0, 100.0, 10.0),
            line,
            word,
        }
    }

    #[test]
    fn reconstruct_inserts_spaces_and_newlines() {
        let glyphs = vec![
            g('H', 0, 0, 0.0),
            g('i', 0, 0, 5.0),
            g('t', 0, 1, 15.0),
            g('h', 0, 1, 20.0),
            g('e', 0, 1, 25.0),
            g('r', 1, 2, 0.0),
            g('e', 1, 2, 5.0),
        ];
        assert_eq!(reconstruct_text(&glyphs, 0, 6), "Hi the\nre");
    }

    #[test]
    fn truncate_marks_overflow() {
        let (t, truncated) = truncate_text("abcdef", 3);
        assert_eq!(t, "abc");
        assert!(truncated);
        let (t, truncated) = truncate_text("ab", 3);
        assert_eq!(t, "ab");
        assert!(!truncated);
    }

    #[test]
    fn intersecting_glyphs() {
        let glyphs = vec![g('a', 0, 0, 0.0), g('b', 0, 1, 20.0), g('c', 0, 2, 40.0)];
        let hit = glyphs_intersecting_rects(&glyphs, &[PdfRect::new(18.0, 0.0, 30.0, 10.0)]);
        assert_eq!(hit, vec![1]);
    }
}
