//! Click handling and glyph selection under a point (RF11b).

use std::time::{Duration, Instant};

use egui::Pos2;

use super::super::commit::place_box;
use super::super::paint::text_mark_kind;
use super::PaneCtx;
use crate::annot::{click_glyph_range, glyph_at, highlight_quads, AnnotKind, MarkupStyle};
use crate::app::{CreateKind, MarkerApp, Tab, TextSel, Tool};
use crate::assistant::{CaptureMode, LearningSelection};
use crate::geom::PdfPoint;

pub(super) fn click(app: &mut MarkerApp, pos: Pos2, pane: PaneCtx, double: bool) {
    let view = pane.rect;
    // True clicks never start a drag (`Sense::click_and_drag` waits for movement), so
    // text / highlight / learning selection must be handled here — including double-click
    // whole-word selection (#34).
    if app.capture == CaptureMode::LearningText {
        let Some((page, point)) = app
            .tab_by_id(pane.tab_id)
            .and_then(|tab| tab.doc.screen_to_page(pos, view))
        else {
            return;
        };
        select_glyphs_at_point(app, pane, page, point, double, GlyphClick::Learning);
        return;
    }
    if app.capture == CaptureMode::Region {
        return;
    }

    let tool = app.tool;
    let located = app
        .tab_by_id(pane.tab_id)
        .and_then(|tab| tab.doc.screen_to_page(pos, view));
    let Some((page, point)) = located else {
        app.end_edit_undo();
        app.clear_page_selection();
        return;
    };
    let slop = app
        .tab_by_id(pane.tab_id)
        .map(|tab| 4.0 / tab.doc.scale)
        .unwrap_or(4.0);
    let hit = app
        .tab_by_id(pane.tab_id)
        .and_then(|tab| tab.doc.session.hit_test(page, point, slop));
    match tool {
        Tool::Select => {
            if let Some(id) = hit {
                let open = double
                    || app.tab_by_id(pane.tab_id).is_some_and(|tab| {
                        matches!(
                            tab.doc.session.get(id).map(|annot| &annot.kind),
                            Some(AnnotKind::Note { .. })
                        )
                    });
                let editable = app
                    .tab_by_id(pane.tab_id)
                    .is_some_and(|tab| is_editable(tab, id));
                if open && editable {
                    app.begin_edit_undo(id);
                } else if app
                    .tab_by_id(pane.tab_id)
                    .is_some_and(|tab| tab.editing.is_some())
                {
                    app.end_edit_undo();
                }
                if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
                    tab.select_only(id);
                    tab.assistant.learning = None;
                    tab.style_bar = None;
                    if open && editable {
                        tab.editing = Some(id);
                        tab.focus_edit = true;
                    } else {
                        tab.editing = None;
                    }
                }
            } else if double
                && select_glyphs_at_point(app, pane, page, point, true, GlyphClick::TextSelect)
            {
                app.end_edit_undo();
            } else {
                app.end_edit_undo();
                app.clear_page_selection();
            }
        }
        Tool::Highlight | Tool::Underline | Tool::StrikeOut | Tool::Squiggly => {
            if select_glyphs_at_point(
                app,
                pane,
                page,
                point,
                double,
                GlyphClick::Mark(tool.markup_style()),
            ) {
                // Mark created (single glyph, or whole word on double-click).
            } else if hit.is_none() {
                app.clear_page_selection();
            }
        }
        Tool::Text => {
            let text = hit.filter(|id| {
                app.tab_by_id(pane.tab_id).is_some_and(|tab| {
                    matches!(
                        tab.doc.session.get(*id).map(|annot| &annot.kind),
                        Some(AnnotKind::Text { .. })
                    )
                })
            });
            if let Some(id) = text {
                app.begin_edit_undo(id);
                if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
                    tab.select_only(id);
                    tab.assistant.learning = None;
                    tab.editing = Some(id);
                    tab.focus_edit = true;
                }
            } else {
                if hit.is_none() {
                    app.clear_page_selection();
                }
                place_box(app, pane, page, point, None, CreateKind::Text);
            }
        }
        Tool::Note => {
            let note = hit.filter(|id| {
                app.tab_by_id(pane.tab_id).is_some_and(|tab| {
                    matches!(
                        tab.doc.session.get(*id).map(|annot| &annot.kind),
                        Some(AnnotKind::Note { .. })
                    )
                })
            });
            if let Some(id) = note {
                app.begin_edit_undo(id);
                if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
                    tab.select_only(id);
                    tab.assistant.learning = None;
                    tab.editing = Some(id);
                    tab.focus_edit = true;
                }
            } else {
                if hit.is_none() {
                    app.clear_page_selection();
                }
                // Click places a default-size note; notes are not drag-to-size.
                place_box(app, pane, page, point, None, CreateKind::Note);
            }
        }
        Tool::Math => {
            let math = hit.filter(|id| {
                app.tab_by_id(pane.tab_id).is_some_and(|tab| {
                    matches!(
                        tab.doc.session.get(*id).map(|annot| &annot.kind),
                        Some(AnnotKind::Math { .. })
                    )
                })
            });
            if let Some(id) = math {
                app.begin_edit_undo(id);
                if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
                    tab.select_only(id);
                    tab.assistant.learning = None;
                    tab.editing = Some(id);
                    tab.focus_edit = true;
                }
            } else {
                if hit.is_none() {
                    app.clear_page_selection();
                }
                place_box(app, pane, page, point, None, CreateKind::Math);
            }
        }
        Tool::Rect | Tool::Ellipse | Tool::Line => {
            if hit.is_none() {
                app.clear_page_selection();
            }
        }
    }
}

fn is_editable(tab: &Tab, id: u64) -> bool {
    matches!(
        tab.doc.session.get(id).map(|annot| &annot.kind),
        Some(AnnotKind::Text { .. } | AnnotKind::Note { .. } | AnnotKind::Math { .. })
    )
}

#[derive(Clone, Copy)]
enum GlyphClick {
    TextSelect,
    /// `None` is a highlight fill. A style is underline, strikeout, or squiggly.
    Mark(Option<MarkupStyle>),
    Learning,
}

/// Select / highlight glyphs under a click. Double-click expands to the whole word.
/// Returns false when no glyph is under the point.
fn select_glyphs_at_point(
    app: &mut MarkerApp,
    pane: PaneCtx,
    page: usize,
    point: PdfPoint,
    whole_word: bool,
    kind: GlyphClick,
) -> bool {
    let (lo, hi, word) = {
        let Some(tab) = app.tab_by_id(pane.tab_id) else {
            return false;
        };
        let Some(glyphs) = tab.doc.glyphs.get(&page) else {
            return false;
        };
        let Some(index) = glyph_at(glyphs, point) else {
            return false;
        };
        let Some((lo, hi)) = click_glyph_range(glyphs, index, whole_word) else {
            return false;
        };
        let word = glyphs.get(index).map(|g| g.word);
        (lo, hi, word)
    };

    match kind {
        GlyphClick::TextSelect => {
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
                return false;
            };
            tab.selected.clear();
            tab.editing = None;
            tab.style_bar = None;
            tab.assistant.learning = None;
            tab.text_sel = Some(TextSel {
                page,
                glyph_lo: lo,
                glyph_hi: hi,
            });
            true
        }
        GlyphClick::Learning => {
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
                return false;
            };
            tab.assistant.learning = Some(LearningSelection {
                page,
                glyph_lo: lo,
                glyph_hi: hi,
            });
            app.capture = CaptureMode::None;
            app.assistant_open = true;
            app.attach_learning_text();
            true
        }
        GlyphClick::Mark(style) => {
            let color = app.settings.highlight_color;
            let replace = {
                let Some(tab) = app.tab_by_id(pane.tab_id) else {
                    return false;
                };
                if !whole_word {
                    None
                } else {
                    tab.last_hl.and_then(|(when, hit_page, hit_word, id)| {
                        (when.elapsed() < Duration::from_millis(420)
                            && hit_page == page
                            && word == Some(hit_word))
                        .then_some(id)
                        .flatten()
                    })
                }
            };
            let quads = {
                let Some(tab) = app.tab_by_id(pane.tab_id) else {
                    return false;
                };
                let Some(glyphs) = tab.doc.glyphs.get(&page) else {
                    return false;
                };
                highlight_quads(glyphs, lo, hi)
            };
            if quads.is_empty() {
                return false;
            }
            app.seal_then_arm();
            let Some(tab) = app.tab_by_id_mut(pane.tab_id) else {
                return false;
            };
            if let Some(old) = replace {
                tab.doc.session.remove(old);
            }
            let id = tab
                .doc
                .session
                .insert(page, text_mark_kind(style, quads, color));
            // Leave the new mark unselected so the user can keep marking.
            tab.selected.clear();
            tab.text_sel = None;
            if let Some(w) = word {
                // Remember single-glyph marks so a quick second click can upgrade to the word.
                let remembered = (!whole_word).then_some(id);
                tab.last_hl = Some((Instant::now(), page, w, remembered));
            }
            tab.save.touch();
            app.seal_undo();
            true
        }
    }
}
