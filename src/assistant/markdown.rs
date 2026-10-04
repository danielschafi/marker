//! Safe Markdown rendering for assistant turns (issue #26).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(test)]
use std::cell::Cell;

use egui::Ui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

/// Finished replies kept so a painted turn is not sanitized again next frame.
const FINISHED_CACHE_CAP: usize = 128;

thread_local! {
    static FINISHED_MD: RefCell<HashMap<Arc<str>, Arc<str>>> = RefCell::new(HashMap::new());
}

#[cfg(test)]
thread_local! {
    static SANITIZE_COUNT: Cell<u64> = const { Cell::new(0) };
}

/// Schemes allowed for clickable links in assistant Markdown.
fn link_scheme_allowed(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:")
}

/// Rewrite Markdown so only http(s)/mailto destinations stay clickable.
///
/// Other URI schemes (and raw HTML) are left as visible plain text — never executed.
/// `egui_commonmark` already paints HTML tags as text when no `render_html_fn` is set;
/// this pass additionally neuters `javascript:`, `file:`, `data:`, etc. in links/images.
pub fn sanitize_markdown(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        // Inline link/image: [text](url) or ![alt](url)
        let is_image = out.ends_with('!');
        if let Some((label_end, url_part)) = parse_md_link(rest) {
            let label = &rest[1..label_end];
            let url = url_part.trim();
            if link_scheme_allowed(url) {
                if is_image {
                    // Images are not loaded (crate features off); keep alt as plain text.
                    if !out.is_empty() && out.ends_with('!') {
                        out.pop();
                    }
                    out.push_str(label);
                } else {
                    out.push('[');
                    out.push_str(label);
                    out.push_str("](");
                    out.push_str(url);
                    out.push(')');
                }
            } else if is_image {
                if !out.is_empty() && out.ends_with('!') {
                    out.pop();
                }
                out.push_str(label);
            } else {
                // Keep label readable; drop the dangerous destination.
                out.push_str(label);
            }
            rest = &rest[label_end + 1 + url_part.len() + 2..]; // skip ](url)
            continue;
        }
        out.push('[');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

/// If `s` starts with `[label](url)`, returns `(index of ']', url slice without parens)`.
fn parse_md_link(s: &str) -> Option<(usize, &str)> {
    if !s.starts_with('[') {
        return None;
    }
    let mut depth = 0usize;
    let mut label_end = None;
    for (i, ch) in s.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    label_end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let label_end = label_end?;
    let after = s.get(label_end + 1..)?;
    if !after.starts_with('(') {
        return None;
    }
    let mut depth = 0usize;
    let mut url_end = None;
    for (i, ch) in after.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    url_end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let url_end = url_end?;
    let url = &after[1..url_end];
    Some((label_end, url))
}

/// Markdown for one paint.
///
/// A finished turn is sanitized once and reused. A streaming turn changes on
/// every delta, so it is sanitized again and not stored.
fn render_markdown(text: &str, incomplete: bool) -> Arc<str> {
    if incomplete {
        bump_sanitize_count();
        return Arc::from(sanitize_markdown(text));
    }
    FINISHED_MD.with(|cell| {
        let mut map = cell.borrow_mut();
        if let Some(hit) = map.get(text) {
            return Arc::clone(hit);
        }
        bump_sanitize_count();
        let sanitized: Arc<str> = sanitize_markdown(text).into();
        if map.len() >= FINISHED_CACHE_CAP {
            map.clear();
        }
        map.insert(Arc::from(text), Arc::clone(&sanitized));
        sanitized
    })
}

fn bump_sanitize_count() {
    #[cfg(test)]
    SANITIZE_COUNT.with(|count| count.set(count.get() + 1));
}

#[cfg(test)]
fn sanitize_count() -> u64 {
    SANITIZE_COUNT.with(|count| count.get())
}

#[cfg(test)]
fn reset_render_cache() {
    SANITIZE_COUNT.with(|count| count.set(0));
    FINISHED_MD.with(|cell| cell.borrow_mut().clear());
}

/// Render assistant Markdown into `ui`. Plain text stays readable as paragraphs.
///
/// Callers pass finished turns only. Streaming text stays a plain label so a
/// half-open fence is not parsed, and so this cache is not filled with partials.
pub fn show(ui: &mut Ui, cache: &mut CommonMarkCache, text: &str) {
    if text.is_empty() {
        return;
    }
    let sanitized = render_markdown(text, false);
    ui.scope(|ui| {
        ui.style_mut().url_in_tooltip = true;
        CommonMarkViewer::new()
            // Do not assume bare paths are file:// (we never load images anyway).
            .explicit_image_uri_scheme(true)
            .show(ui, cache, &sanitized);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_unchanged() {
        let s = "Hello, just a sentence.\n\nAnother paragraph.";
        assert_eq!(sanitize_markdown(s), s);
    }

    #[test]
    fn keeps_https_links() {
        let s = "See [docs](https://example.com/a) please.";
        assert_eq!(sanitize_markdown(s), s);
    }

    #[test]
    fn strips_javascript_links() {
        let s = "Click [here](javascript:alert(1)) now.";
        assert_eq!(sanitize_markdown(s), "Click here now.");
    }

    #[test]
    fn images_become_alt_text() {
        let s = "Pic ![diagram](https://example.com/x.png) end";
        assert_eq!(sanitize_markdown(s), "Pic diagram end");
    }

    #[test]
    fn headings_lists_code_pass_through() {
        let s = "# Title\n\n- a\n- b\n\n`code` and **bold**\n\n```rust\nfn main() {}\n```\n";
        assert_eq!(sanitize_markdown(s), s);
    }

    #[test]
    fn finished_turn_is_sanitized_once() {
        reset_render_cache();
        let raw = "Click [here](javascript:alert(1)) now.";
        let first = render_markdown(raw, false);
        assert_eq!(&*first, "Click here now.");
        assert_eq!(sanitize_count(), 1);
        for _ in 0..4 {
            let again = render_markdown(raw, false);
            assert_eq!(again, first);
        }
        assert_eq!(sanitize_count(), 1);

        // Streaming text changes every delta: sanitize each update, do not reuse.
        let partial = "See [docs](https://example.com) ";
        assert_eq!(&*render_markdown(partial, true), partial);
        assert_eq!(&*render_markdown(partial, true), partial);
        assert_eq!(sanitize_count(), 3);

        // The next finished body is its own entry.
        let other = "See [docs](https://example.com/a) please.";
        assert_eq!(&*render_markdown(other, false), other);
        assert_eq!(sanitize_count(), 4);
        assert_eq!(&*render_markdown(other, false), other);
        assert_eq!(sanitize_count(), 4);
    }
}
