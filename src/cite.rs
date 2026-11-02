//! Quote-with-citation formatting (ST9).
//!
//! Builds a clipboard string from a selected quote plus a citation line.
//! Citation prefers a DOI or arXiv id found in page-0 text; otherwise the
//! file name and 1-based page. No network.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set while the context menu needs page-0 glyphs for citation (view sets,
/// `wanted_tiles` reads). Avoids an `app.rs` glyph request API this wave.
static NEED_PAGE0: AtomicBool = AtomicBool::new(false);

/// Ask the tile/glyph dispatcher to fetch page 0 when it is not cached.
pub fn need_page0_glyphs() {
    NEED_PAGE0.store(true, Ordering::Relaxed);
}

/// Clear the page-0 request flag (menu closed or glyphs arrived).
pub fn clear_page0_request() {
    NEED_PAGE0.store(false, Ordering::Relaxed);
}

/// Whether `wanted_tiles` should enqueue a glyphs job for page 0.
pub fn page0_glyphs_needed() -> bool {
    NEED_PAGE0.load(Ordering::Relaxed)
}

/// Identifier extracted from first-page text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiteId {
    Doi(String),
    Arxiv(String),
}

/// Find a DOI (`10.xxxx/…`) in `text`, if any.
pub fn find_doi(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == b'1' && bytes[i + 1] == b'0' && bytes[i + 2] == b'.' {
            if let Some(doi) = parse_doi_at(text, i) {
                return Some(doi);
            }
        }
        i += 1;
    }
    None
}

fn parse_doi_at(text: &str, start: usize) -> Option<String> {
    let rest = &text[start..];
    // "10." then 4–9 digits, then '/', then path body.
    let after_prefix = rest.strip_prefix("10.")?;
    let mut digit_end = 0;
    for (i, c) in after_prefix.char_indices() {
        if c.is_ascii_digit() {
            digit_end = i + c.len_utf8();
        } else {
            break;
        }
    }
    if !(4..=9).contains(&digit_end) {
        return None;
    }
    let after_digits = &after_prefix[digit_end..];
    if !after_digits.starts_with('/') {
        return None;
    }
    let path = &after_digits[1..];
    let mut path_end = 0;
    for (i, c) in path.char_indices() {
        if is_doi_path_char(c) {
            path_end = i + c.len_utf8();
        } else {
            break;
        }
    }
    if path_end == 0 {
        return None;
    }
    // Trim trailing punctuation that often follows a DOI in prose.
    let mut body = &path[..path_end];
    while let Some(last) = body.chars().last() {
        if matches!(last, '.' | ',' | ';' | ':' | ')' | ']' | '>') {
            body = &body[..body.len() - last.len_utf8()];
        } else {
            break;
        }
    }
    if body.is_empty() {
        return None;
    }
    Some(format!("10.{}/{}", &after_prefix[..digit_end], body))
}

fn is_doi_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ';' | '(' | ')' | '/' | ':' | '+')
}

/// Find an arXiv id in `text` (`arXiv:2301.12345` or old `hep-th/9901001`).
pub fn find_arxiv(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find("arxiv") {
        let at = search_from + rel;
        let after = &text[at + 5..];
        let after_trim =
            after.trim_start_matches(|c: char| c.is_whitespace() || c == ':' || c == '.');
        // Skip "arxiv.org/abs/" or "arxiv.org/pdf/" prefix if present.
        let after_trim = strip_arxiv_url_prefix(after_trim);
        if let Some(id) = parse_arxiv_id(after_trim) {
            return Some(id);
        }
        search_from = at + 5;
    }
    None
}

fn strip_arxiv_url_prefix(s: &str) -> &str {
    let lower_head: String = s.chars().take(16).collect::<String>().to_ascii_lowercase();
    if lower_head.starts_with("org/abs/") {
        &s["org/abs/".len()..]
    } else if lower_head.starts_with("org/pdf/") {
        &s["org/pdf/".len()..]
    } else {
        s
    }
}

fn parse_arxiv_id(s: &str) -> Option<String> {
    let s = s.trim_start();
    // New-style: YYMM.NNNNN with optional version.
    if let Some(id) = parse_new_arxiv(s) {
        return Some(id);
    }
    // Old-style: archive/YYMMNNN
    parse_old_arxiv(s)
}

fn parse_new_arxiv(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    if bytes.len() < 9 {
        return None;
    }
    // 4 digits, '.', 4 or 5 digits, optional vN
    for (i, b) in bytes.iter().take(4).enumerate() {
        if !b.is_ascii_digit() {
            return None;
        }
        let _ = i;
    }
    if bytes[4] != b'.' {
        return None;
    }
    let mut n = 0;
    while 5 + n < bytes.len() && bytes[5 + n].is_ascii_digit() {
        n += 1;
    }
    if n != 4 && n != 5 {
        return None;
    }
    let mut end = 5 + n;
    if end < bytes.len() && bytes[end] == b'v' {
        let mut v = 0;
        while end + 1 + v < bytes.len() && bytes[end + 1 + v].is_ascii_digit() {
            v += 1;
        }
        if v == 0 {
            return None;
        }
        end = end + 1 + v;
    }
    // Reject if the next char is still alphanumeric (id would continue).
    if end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
        return None;
    }
    Some(s[..end].to_string())
}

fn parse_old_arxiv(s: &str) -> Option<String> {
    // archive-name/7digits, e.g. hep-th/9901001
    let slash = s.find('/')?;
    let archive = &s[..slash];
    if archive.is_empty() || !archive.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
        return None;
    }
    let rest = &s[slash + 1..];
    let mut n = 0;
    let bytes = rest.as_bytes();
    while n < bytes.len() && bytes[n].is_ascii_digit() {
        n += 1;
    }
    if n != 7 {
        return None;
    }
    let mut end = n;
    if end < bytes.len() && bytes[end] == b'v' {
        let mut v = 0;
        while end + 1 + v < bytes.len() && bytes[end + 1 + v].is_ascii_digit() {
            v += 1;
        }
        if v == 0 {
            return None;
        }
        end = end + 1 + v;
    }
    if end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
        return None;
    }
    Some(format!("{archive}/{}", &rest[..end]))
}

/// Prefer DOI, then arXiv, from page-0 (or metadata) text.
pub fn find_cite_id(text: &str) -> Option<CiteId> {
    if let Some(doi) = find_doi(text) {
        return Some(CiteId::Doi(doi));
    }
    if let Some(id) = find_arxiv(text) {
        return Some(CiteId::Arxiv(id));
    }
    None
}

/// Wrap quote text in ASCII double quotes; collapse outer whitespace.
pub fn wrap_quote(quote: &str) -> String {
    let trimmed = quote.trim();
    format!("\"{trimmed}\"")
}

/// Build the citation line from file name, 1-based page, and page-0 text.
pub fn citation_line(file_name: &str, page_one_based: usize, page0_text: &str) -> String {
    match find_cite_id(page0_text) {
        Some(CiteId::Doi(doi)) => format!("doi:{doi}"),
        Some(CiteId::Arxiv(id)) => format!("arXiv:{id}"),
        None => format!("{file_name}, p. {page_one_based}"),
    }
}

/// Full clipboard payload: quoted selection plus citation line.
pub fn format_quoted_citation(quote: &str, citation: &str) -> String {
    format!("{}\n\n{citation}", wrap_quote(quote))
}

/// Reconstruct full-page text for DOI/arXiv scanning.
pub fn page_text(glyphs: &[crate::annot::Glyph]) -> String {
    if glyphs.is_empty() {
        return String::new();
    }
    crate::page_text::reconstruct_text(glyphs, 0, glyphs.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_doi_in_prose() {
        let text = "See https://doi.org/10.1145/1234567.7654321 for details.";
        assert_eq!(find_doi(text).as_deref(), Some("10.1145/1234567.7654321"));
        assert_eq!(
            find_cite_id(text),
            Some(CiteId::Doi("10.1145/1234567.7654321".into()))
        );
    }

    #[test]
    fn finds_arxiv_new_style() {
        let text = "Available at arXiv:2301.12345v2 (cs.LG).";
        assert_eq!(find_arxiv(text).as_deref(), Some("2301.12345v2"));
        assert_eq!(
            find_cite_id(text),
            Some(CiteId::Arxiv("2301.12345v2".into()))
        );
    }

    #[test]
    fn neither_doi_nor_arxiv() {
        let text = "Hello Marker world — no identifiers here.";
        assert_eq!(find_doi(text), None);
        assert_eq!(find_arxiv(text), None);
        assert_eq!(find_cite_id(text), None);
        assert_eq!(citation_line("notes.pdf", 3, text), "notes.pdf, p. 3");
    }

    #[test]
    fn quote_wrapping_and_payload() {
        let wrapped = wrap_quote("  partial quote  ");
        assert_eq!(wrapped, "\"partial quote\"");
        let payload = format_quoted_citation(
            "partial quote",
            &citation_line("paper.pdf", 1, "doi: 10.1000/xyz123."),
        );
        assert_eq!(payload, "\"partial quote\"\n\ndoi:10.1000/xyz123");
    }

    #[test]
    fn doi_preferred_over_arxiv() {
        let text = "arXiv:2301.00001 and doi:10.1000/abc";
        assert_eq!(find_cite_id(text), Some(CiteId::Doi("10.1000/abc".into())));
    }
}
