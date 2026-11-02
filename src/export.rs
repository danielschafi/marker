//! Export highlights (and native FT5 markup) to Markdown (ST1) and Anki CSV (ST3).

use std::collections::HashMap;

use crate::annot::{Glyph, PageGlyphs};
use crate::assistant::{glyphs_intersecting_rects, reconstruct_text};
use crate::geom::{PdfRect, Rgb};
use crate::settings::HighlightPreset;

/// One highlight or native markup quad-set to export.
#[derive(Clone, Debug)]
pub struct ExportHighlight {
    /// Zero-based page index.
    pub page: usize,
    pub quads: Vec<PdfRect>,
    pub color: Rgb,
    /// Nearby sticky-note text, if any.
    pub note: Option<String>,
}

/// How far (PDF points) a note's center may sit from a highlight's bounds
/// and still count as "nearby".
pub const NOTE_NEARBY_PTS: f32 = 48.0;

/// Exact RGB → preset name; unknown colors become `"Custom"` (ST2 rule).
pub fn preset_name_for(presets: &[HighlightPreset], color: Rgb) -> &str {
    presets
        .iter()
        .find(|preset| preset.rgb == color)
        .map(|preset| preset.name.as_str())
        .unwrap_or("Custom")
}

/// Whether a note rect is near a highlight's union bounds on the same page.
pub fn note_is_nearby(highlight_bounds: PdfRect, note_rect: PdfRect) -> bool {
    let expanded = highlight_bounds.inflate(NOTE_NEARBY_PTS);
    expanded.contains(note_rect.center())
}

/// Build Markdown for native highlights/markup. Groups by page (ascending).
///
/// Each entry is a blockquote of reconstructed glyph text, then the preset
/// name, 1-based page, a `file.pdf#page=N` link, and optional note text.
pub fn highlights_to_markdown(
    doc_name: &str,
    highlights: &[ExportHighlight],
    glyph_pages: &HashMap<usize, PageGlyphs>,
    presets: &[HighlightPreset],
) -> String {
    if highlights.is_empty() {
        return format!("# {doc_name}\n\n_(no highlights)_\n");
    }

    let mut by_page: HashMap<usize, Vec<&ExportHighlight>> = HashMap::new();
    for hl in highlights {
        by_page.entry(hl.page).or_default().push(hl);
    }
    let mut pages: Vec<usize> = by_page.keys().copied().collect();
    pages.sort_unstable();

    let mut out = String::new();
    out.push_str("# ");
    out.push_str(doc_name);
    out.push_str("\n\n");

    for page in pages {
        let Some(items) = by_page.get(&page) else {
            continue;
        };
        let page_1 = page + 1;
        out.push_str(&format!("## Page {page_1}\n\n"));
        let glyphs: &[Glyph] = match glyph_pages.get(&page) {
            Some(page_glyphs) => page_glyphs,
            None => &[],
        };
        for hl in items {
            let quote = quote_for(hl, glyphs);
            write_blockquote(&mut out, &quote);
            out.push('\n');
            let name = preset_name_for(presets, hl.color);
            let link = format!("{doc_name}#page={page_1}");
            out.push_str(&format!("**{name}** · p.{page_1} · [{link}]({link})\n"));
            if let Some(note) = hl.note.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                out.push('\n');
                out.push_str(note);
                out.push('\n');
            }
            out.push('\n');
        }
    }
    out
}

fn quote_for(hl: &ExportHighlight, glyphs: &[Glyph]) -> String {
    if glyphs.is_empty() || hl.quads.is_empty() {
        return String::new();
    }
    let indices = glyphs_intersecting_rects(glyphs, &hl.quads);
    let (Some(&lo), Some(&hi)) = (indices.first(), indices.last()) else {
        return String::new();
    };
    reconstruct_text(glyphs, lo, hi)
}

fn write_blockquote(out: &mut String, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        out.push_str("> _(no text)_\n");
        return;
    }
    for line in trimmed.lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }
}

/// Default export file name next to the PDF: `stem-notes.md`.
pub fn default_export_name(pdf_path: &std::path::Path) -> String {
    let stem = pdf_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("document");
    format!("{stem}-notes.md")
}

/// Default Anki CSV file name next to the PDF: `stem-cards.csv`.
pub fn default_cards_name(pdf_path: &std::path::Path) -> String {
    let stem = pdf_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("document");
    format!("{stem}-cards.csv")
}

/// Escape one CSV field (RFC 4180): quote when needed; double internal quotes.
pub fn csv_escape(field: &str) -> String {
    if field.contains('"') || field.contains(',') || field.contains('\n') || field.contains('\r') {
        let mut out = String::with_capacity(field.len() + 2);
        out.push('"');
        for ch in field.chars() {
            if ch == '"' {
                out.push('"');
            }
            out.push(ch);
        }
        out.push('"');
        out
    } else {
        field.to_owned()
    }
}

fn push_csv_row(out: &mut String, front: &str, back: &str, tag: &str, source: &str) {
    out.push_str(&csv_escape(front));
    out.push(',');
    out.push_str(&csv_escape(back));
    out.push(',');
    out.push_str(&csv_escape(tag));
    out.push(',');
    out.push_str(&csv_escape(source));
    out.push('\n');
}

/// Wrap a phrase as an Anki cloze deletion (`{{c1::…}}`).
pub fn cloze_front(phrase: &str) -> String {
    format!("{{{{c1::{}}}}}", phrase.trim())
}

/// Build Anki-importable CSV for native highlights/markup.
///
/// Columns: `front`, `back`, `tag` (preset name), `source` (`file.pdf#page=N`).
/// Highlight alone → cloze of the quote; highlight + note → front=quote, back=note.
pub fn highlights_to_csv(
    doc_name: &str,
    highlights: &[ExportHighlight],
    glyph_pages: &HashMap<usize, PageGlyphs>,
    presets: &[HighlightPreset],
) -> String {
    let mut out = String::from("front,back,tag,source\n");
    if highlights.is_empty() {
        return out;
    }

    let mut by_page: HashMap<usize, Vec<&ExportHighlight>> = HashMap::new();
    for hl in highlights {
        by_page.entry(hl.page).or_default().push(hl);
    }
    let mut pages: Vec<usize> = by_page.keys().copied().collect();
    pages.sort_unstable();

    for page in pages {
        let Some(items) = by_page.get(&page) else {
            continue;
        };
        let page_1 = page + 1;
        let glyphs: &[Glyph] = match glyph_pages.get(&page) {
            Some(page_glyphs) => page_glyphs,
            None => &[],
        };
        for hl in items {
            let quote = quote_for(hl, glyphs);
            let tag = preset_name_for(presets, hl.color);
            let source = format!("{doc_name}#page={page_1}");
            let note = hl.note.as_deref().map(str::trim).filter(|s| !s.is_empty());
            match note {
                Some(note) => push_csv_row(&mut out, quote.trim(), note, tag, &source),
                None => push_csv_row(&mut out, &cloze_front(&quote), "", tag, &source),
            }
        }
    }
    out
}

/// Build CSV from assistant-generated front/back pairs (ST3).
pub fn generated_cards_to_csv(cards: &[(String, String)], tag: &str, source: &str) -> String {
    let mut out = String::from("front,back,tag,source\n");
    for (front, back) in cards {
        push_csv_row(&mut out, front, back, tag, source);
    }
    out
}

/// Extract the body of the first fenced code block in `reply`.
///
/// Prefers a fence whose info string starts with `tsv` (case-insensitive).
/// Falls back to the first fence of any language. Returns `None` when no
/// complete opening/closing fence pair is found.
fn first_fenced_body(reply: &str) -> Option<&str> {
    let bytes = reply.as_bytes();
    let mut i = 0;
    let mut first_any: Option<&str> = None;
    while i + 3 <= bytes.len() {
        if &bytes[i..i + 3] != b"```" {
            i += 1;
            continue;
        }
        let after_ticks = i + 3;
        let line_end = reply[after_ticks..]
            .find('\n')
            .map(|n| after_ticks + n)
            .unwrap_or(reply.len());
        let info = reply[after_ticks..line_end].trim();
        let body_start = if line_end < reply.len() {
            line_end + 1
        } else {
            line_end
        };
        let rest = &reply[body_start..];
        let Some(close) = rest.find("```") else {
            break;
        };
        let body = rest[..close].trim_end_matches('\r').trim_end_matches('\n');
        let is_tsv = info
            .split_whitespace()
            .next()
            .is_some_and(|lang| lang.eq_ignore_ascii_case("tsv"));
        if is_tsv {
            return Some(body);
        }
        if first_any.is_none() {
            first_any = Some(body);
        }
        i = body_start + close + 3;
    }
    first_any
}

/// Parse a fenced TSV flashcard block from an assistant reply.
///
/// Each non-empty line is `front<TAB>back`. No header row. On failure returns
/// `Err` so the caller can show the raw reply and skip writing a file.
pub fn parse_cards_tsv_fence(reply: &str) -> Result<Vec<(String, String)>, String> {
    let Some(body) = first_fenced_body(reply) else {
        return Err("no fenced TSV block in reply".into());
    };
    let mut cards = Vec::new();
    for line in body.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let Some((front, back)) = line.split_once('\t') else {
            return Err(format!(
                "TSV line missing tab separator: {}",
                truncate_for_err(line)
            ));
        };
        let front = front.trim().to_owned();
        let back = back.trim().to_owned();
        if front.is_empty() {
            return Err("TSV card has empty front".into());
        }
        cards.push((front, back));
    }
    if cards.is_empty() {
        return Err("fenced TSV block has no cards".into());
    }
    Ok(cards)
}

fn truncate_for_err(s: &str) -> String {
    let mut out: String = s.chars().take(48).collect();
    if s.chars().count() > 48 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::PdfRect;
    use crate::settings::default_highlight_presets;

    fn g(ch: char, line: u32, word: u32, x: f32) -> Glyph {
        Glyph {
            ch,
            bounds: PdfRect::new(x, 0.0, x + 5.0, 10.0),
            line,
            word,
        }
    }

    fn page_glyphs(glyphs: Vec<Glyph>) -> PageGlyphs {
        let mut line_bounds = Vec::new();
        for glyph in &glyphs {
            let i = glyph.line as usize;
            if line_bounds.len() <= i {
                line_bounds.resize(i + 1, PdfRect::new(0.0, 0.0, 0.0, 0.0));
            }
            let slot = &mut line_bounds[i];
            if slot.width() < 0.5 && slot.height() < 0.5 {
                *slot = glyph.bounds;
            } else {
                *slot = PdfRect::new(
                    slot.x0.min(glyph.bounds.x0),
                    slot.y0.min(glyph.bounds.y0),
                    slot.x1.max(glyph.bounds.x1),
                    slot.y1.max(glyph.bounds.y1),
                );
            }
        }
        PageGlyphs::new(glyphs, line_bounds)
    }

    #[test]
    fn markdown_two_colors_one_unknown() {
        let presets = default_highlight_presets();
        let known = presets[0].rgb;
        let unknown = Rgb::new(1, 2, 3);

        let glyphs = page_glyphs(vec![
            g('H', 0, 0, 0.0),
            g('e', 0, 0, 5.0),
            g('l', 0, 0, 10.0),
            g('l', 0, 0, 15.0),
            g('o', 0, 0, 20.0),
            g('W', 0, 1, 30.0),
            g('o', 0, 1, 35.0),
            g('r', 0, 1, 40.0),
            g('l', 0, 1, 45.0),
            g('d', 0, 1, 50.0),
        ]);
        let mut pages = HashMap::new();
        pages.insert(0, glyphs);

        let highlights = vec![
            ExportHighlight {
                page: 0,
                quads: vec![PdfRect::new(0.0, 0.0, 25.0, 10.0)],
                color: known,
                note: Some("Why this matters".into()),
            },
            ExportHighlight {
                page: 0,
                quads: vec![PdfRect::new(30.0, 0.0, 55.0, 10.0)],
                color: unknown,
                note: None,
            },
        ];

        let md = highlights_to_markdown("paper.pdf", &highlights, &pages, &presets);
        assert!(md.contains("# paper.pdf"));
        assert!(md.contains("## Page 1"));
        assert!(md.contains("> Hello"));
        assert!(md.contains("> World"));
        assert!(md.contains("**Key idea**"));
        assert!(md.contains("**Custom**"));
        assert!(md.contains("[paper.pdf#page=1](paper.pdf#page=1)"));
        assert!(md.contains("Why this matters"));
        assert_eq!(preset_name_for(&presets, known), "Key idea");
        assert_eq!(preset_name_for(&presets, unknown), "Custom");
    }

    #[test]
    fn groups_by_page_ascending() {
        let presets = default_highlight_presets();
        let color = presets[1].rgb;
        let g0 = page_glyphs(vec![g('A', 0, 0, 0.0)]);
        let g2 = page_glyphs(vec![g('B', 0, 0, 0.0)]);
        let mut pages = HashMap::new();
        pages.insert(0, g0);
        pages.insert(2, g2);
        let highlights = vec![
            ExportHighlight {
                page: 2,
                quads: vec![PdfRect::new(0.0, 0.0, 5.0, 10.0)],
                color,
                note: None,
            },
            ExportHighlight {
                page: 0,
                quads: vec![PdfRect::new(0.0, 0.0, 5.0, 10.0)],
                color,
                note: None,
            },
        ];
        let md = highlights_to_markdown("doc.pdf", &highlights, &pages, &presets);
        let p1 = md.find("## Page 1").expect("page 1");
        let p3 = md.find("## Page 3").expect("page 3");
        assert!(p1 < p3);
        assert!(md.contains("**Definition**"));
        assert!(md.contains("doc.pdf#page=3"));
    }

    #[test]
    fn nearby_note_uses_inflated_bounds() {
        let hl = PdfRect::new(100.0, 100.0, 200.0, 120.0);
        let near = PdfRect::new(210.0, 105.0, 234.0, 129.0);
        let far = PdfRect::new(400.0, 400.0, 424.0, 424.0);
        assert!(note_is_nearby(hl, near));
        assert!(!note_is_nearby(hl, far));
    }

    #[test]
    fn default_name_uses_stem() {
        assert_eq!(
            default_export_name(std::path::Path::new("/tmp/lecture.pdf")),
            "lecture-notes.md"
        );
        assert_eq!(
            default_cards_name(std::path::Path::new("/tmp/lecture.pdf")),
            "lecture-cards.csv"
        );
    }

    #[test]
    fn csv_cloze_when_no_note() {
        let presets = default_highlight_presets();
        let color = presets[0].rgb;
        let glyphs = page_glyphs(vec![
            g('H', 0, 0, 0.0),
            g('e', 0, 0, 5.0),
            g('l', 0, 0, 10.0),
            g('l', 0, 0, 15.0),
            g('o', 0, 0, 20.0),
        ]);
        let mut pages = HashMap::new();
        pages.insert(0, glyphs);
        let highlights = vec![ExportHighlight {
            page: 0,
            quads: vec![PdfRect::new(0.0, 0.0, 25.0, 10.0)],
            color,
            note: None,
        }];
        let csv = highlights_to_csv("paper.pdf", &highlights, &pages, &presets);
        assert!(csv.starts_with("front,back,tag,source\n"));
        assert!(csv.contains("{{c1::Hello}}"));
        assert!(csv.contains("Key idea"));
        assert!(csv.contains("paper.pdf#page=1"));
        // cloze row: empty back between commas
        assert!(csv.contains("{{c1::Hello}},,Key idea,paper.pdf#page=1\n"));
    }

    #[test]
    fn csv_front_back_when_note_present() {
        let presets = default_highlight_presets();
        let color = presets[1].rgb;
        let glyphs = page_glyphs(vec![
            g('W', 0, 0, 0.0),
            g('o', 0, 0, 5.0),
            g('r', 0, 0, 10.0),
            g('l', 0, 0, 15.0),
            g('d', 0, 0, 20.0),
        ]);
        let mut pages = HashMap::new();
        pages.insert(0, glyphs);
        let highlights = vec![ExportHighlight {
            page: 0,
            quads: vec![PdfRect::new(0.0, 0.0, 25.0, 10.0)],
            color,
            note: Some("What is this?\nA greeting.".into()),
        }];
        let csv = highlights_to_csv("doc.pdf", &highlights, &pages, &presets);
        assert!(csv.contains("World"));
        assert!(!csv.contains("{{c1::"));
        // Newline in note → quoted field
        assert!(csv.contains("\"What is this?\nA greeting.\""));
        assert!(csv.contains("Definition"));
        assert!(csv.contains("doc.pdf#page=1"));
    }

    #[test]
    fn csv_escape_quotes_and_commas() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_escape("line\nbreak"), "\"line\nbreak\"");
    }

    #[test]
    fn parse_tsv_fence_ok() {
        let reply = "Here you go:\n\n```tsv\nWhat is 2+2?\t4\nCapital of France?\tParis\n```\n";
        let cards = parse_cards_tsv_fence(reply).expect("parse");
        assert_eq!(
            cards,
            vec![
                ("What is 2+2?".into(), "4".into()),
                ("Capital of France?".into(), "Paris".into()),
            ]
        );
    }

    #[test]
    fn parse_tsv_broken_fence_errors() {
        let no_fence = "front\tback\nwithout a fence";
        assert!(parse_cards_tsv_fence(no_fence).is_err());

        let empty_fence = "```tsv\n```\n";
        assert!(parse_cards_tsv_fence(empty_fence).is_err());

        let unclosed = "```tsv\nQ\tA\n";
        assert!(parse_cards_tsv_fence(unclosed).is_err());

        let missing_tab = "```tsv\nno tab here\n```\n";
        assert!(parse_cards_tsv_fence(missing_tab).is_err());
    }

    #[test]
    fn generated_cards_csv_quotes() {
        let cards = vec![("Q, with comma".into(), "A \"quoted\"".into())];
        let csv = generated_cards_to_csv(&cards, "generated", "paper.pdf#page=2");
        assert_eq!(
            csv,
            "front,back,tag,source\n\"Q, with comma\",\"A \"\"quoted\"\"\",generated,paper.pdf#page=2\n"
        );
    }
}
