//! Assistant Markdown painting (egui half of `assistant::markdown`).
//!
//! Sanitize / finished-turn cache stay in the library; this binary adapter
//! paints through `egui_commonmark`.

use egui::Ui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

use crate::assistant::finished_markdown;

/// Render finished assistant Markdown into `ui`.
///
/// Callers pass finished turns only. Streaming text stays a plain label so a
/// half-open fence is not parsed, and so the lib cache is not filled with partials.
pub fn show_markdown(ui: &mut Ui, cache: &mut CommonMarkCache, text: &str) {
    if text.is_empty() {
        return;
    }
    let sanitized = finished_markdown(text);
    ui.scope(|ui| {
        ui.style_mut().url_in_tooltip = true;
        CommonMarkViewer::new()
            // Do not assume bare paths are file:// (we never load images anyway).
            .explicit_image_uri_scheme(true)
            .show(ui, cache, &sanitized);
    });
}
