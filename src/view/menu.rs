//! Context menu for the document view (RF11b).

use egui::{Button, Id, PointerButton, Pos2};

use super::PaneCtx;
use crate::annot::{glyph_at, word_range, AnnotKind, MarkupStyle, ShapeKind};
use crate::app::{ContextMenu, MarkerApp, Tab};
use crate::assistant::{glyphs_intersecting_rects, LearningSelection};
use crate::cite;
use crate::platform::clipboard_has_image;

pub(super) fn open_context_menu(app: &mut MarkerApp, pos: Pos2, pane: PaneCtx) {
    let view = pane.rect;
    let located = app.tab_by_id(pane.tab_id).and_then(|tab| {
        let (page, point) = tab.doc.screen_to_page(pos, view)?;
        let hit = tab.doc.session.hit_test(page, point, 6.0 / tab.doc.scale);
        Some((page, point, hit))
    });
    let Some((page, point, hit)) = located else {
        return;
    };
    let can_paste = clipboard_has_image();
    if let Some(tab) = app.tab_by_id_mut(pane.tab_id) {
        tab.style_bar = None;
        if let Some(id) = hit {
            // Right-click on an annotation: target that object (keep multi if already in it).
            if !tab.is_selected(id) {
                tab.select_only(id);
            }
        }
        // Empty-page / text clicks keep any existing selection for keyboard shortcuts,
        // but the menu itself only offers actions for the click target.
        tab.menu = Some(ContextMenu {
            pos,
            page,
            point,
            hit,
            can_paste,
        });
    }
}

pub(super) fn paint_menu(app: &mut MarkerApp, ctx: &egui::Context) {
    let Some(menu) = app.tab().and_then(|tab| tab.menu.clone()) else {
        return;
    };

    let on_annot = menu.hit.is_some();
    let selected_len = app.tab().map(|tab| tab.selected.len()).unwrap_or(0);
    let multi = menu
        .hit
        .is_some_and(|id| selected_len > 1 && app.tab().is_some_and(|tab| tab.is_selected(id)));
    let hit_kind = menu.hit.and_then(|id| {
        app.tab()
            .and_then(|tab| tab.doc.session.get(id))
            .map(|annot| annot.kind.clone())
    });

    // Object actions follow the click target only — leftover selection on empty
    // page must not surface Delete/Style for a prior multi-select.
    let can_style = if multi {
        app.tab().is_some_and(|tab| {
            tab.selected.iter().any(|id| {
                tab.doc
                    .session
                    .get(*id)
                    .is_some_and(|annot| annot.kind.is_styleable())
            })
        })
    } else {
        hit_kind.as_ref().is_some_and(|kind| kind.is_styleable())
    };
    let can_edit = !multi
        && matches!(
            hit_kind,
            Some(AnnotKind::Text { .. } | AnnotKind::Note { .. } | AnnotKind::Math { .. })
        );
    let can_delete = on_annot;
    let delete_count = if multi { selected_len } else { 1 };

    let has_learning = app
        .tab()
        .is_some_and(|tab| tab.assistant.learning.is_some());
    let has_text_sel = app.tab().is_some_and(|tab| tab.text_sel.is_some());
    let copy_text = if on_annot {
        // Annotation under the pointer (now selected) — highlight / text / note body.
        app.copyable_selection_text()
    } else {
        app.copyable_selection_text()
            .or_else(|| app.word_at_point(menu.page, menu.point))
    };
    // Assistant only when there is page/highlight text to work with — not for
    // every annotation that happens to have a copyable body (text/note).
    let on_highlight = matches!(
        hit_kind,
        Some(AnnotKind::Highlight { .. } | AnnotKind::Markup { .. })
    );
    let show_assistant_actions =
        has_learning || has_text_sel || on_highlight || (!on_annot && copy_text.is_some());
    let show_assistant_invite = !on_annot && !show_assistant_actions;
    let show_assistant = show_assistant_actions || show_assistant_invite;
    let can_attach_shot =
        has_learning || has_text_sel || on_highlight || (!on_annot && copy_text.is_some());

    let header = menu_context_header(
        hit_kind.as_ref(),
        multi,
        selected_len,
        !on_annot && copy_text.is_some() && !has_text_sel && !has_learning,
        copy_text.as_deref(),
    );

    let mut action = MenuAction::None;
    let area = egui::Area::new(Id::new("marker-context"))
        .order(egui::Order::Tooltip)
        .fixed_pos(menu.pos)
        .interactable(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(180.0);

                if let Some(header) = &header {
                    ui.label(egui::RichText::new(header).small().weak());
                    ui.add_space(2.0);
                }

                let mut wrote = false;
                if can_edit {
                    if menu_item(ui, edit_menu_label(&hit_kind), None) {
                        action = MenuAction::Edit;
                    }
                    wrote = true;
                }
                if can_style {
                    if wrote {
                        ui.separator();
                    }
                    ui.label(egui::RichText::new("Format").small().weak());
                    ui.horizontal(|ui| {
                        let _ = app.style_bar_contents(ui, false);
                    });
                    wrote = true;
                }
                if copy_text.is_some() {
                    if menu_item(ui, "Copy text", Some("Ctrl+C")) {
                        action = MenuAction::Copy;
                    }
                    let cite_ready = app.tab().is_some_and(|tab| tab.doc.glyphs.contains_key(&0));
                    if !cite_ready {
                        cite::need_page0_glyphs();
                    } else {
                        cite::clear_page0_request();
                    }
                    if menu_item_enabled(ui, "Copy quote with citation", None, cite_ready) {
                        action = MenuAction::CopyCite;
                    }
                    wrote = true;
                }
                if can_delete {
                    let label = if delete_count > 1 {
                        format!("Delete {delete_count} annotations")
                    } else {
                        "Delete".into()
                    };
                    if menu_item(ui, &label, Some("Del")) {
                        action = MenuAction::Delete;
                    }
                    wrote = true;
                }

                if menu.can_paste {
                    if wrote {
                        ui.separator();
                    }
                    if menu_item(ui, "Paste image", Some("Ctrl+Shift+V")) {
                        action = MenuAction::Paste;
                    }
                    wrote = true;
                }

                if show_assistant {
                    if wrote {
                        ui.separator();
                    }
                    ui.label(egui::RichText::new("Assistant").small().weak());
                    if show_assistant_actions {
                        if menu_item(ui, "Attach text", Some("Ctrl+Shift+A")) {
                            action = MenuAction::AttachText;
                        }
                        if can_attach_shot && menu_item(ui, "Attach screenshot", None) {
                            action = MenuAction::AttachShot;
                        }
                        if menu_item(ui, "Explain with Cursor", None) {
                            action = MenuAction::Explain;
                        }
                        if menu_item(ui, "Look up in browser", None) {
                            action = MenuAction::Lookup;
                        }
                    } else if menu_item(ui, "Select text for Assistant", Some("Ctrl+Shift+A")) {
                        action = MenuAction::AttachText;
                    }
                    wrote = true;
                }

                if wrote {
                    ui.separator();
                }
                if menu_item(ui, "Delete page", Some("Ctrl+Shift+Backspace")) {
                    action = MenuAction::DeletePage;
                }
            });
        });

    // Dismiss only on primary click outside. The opening right-click is a
    // secondary `any_click` on the same frame the Area first appears (hovered
    // is still false), which would otherwise flash the menu for one frame.
    let outside = ctx.input(|input| input.pointer.button_clicked(PointerButton::Primary))
        && !area.response.hovered()
        && !area.response.clicked();
    match action {
        MenuAction::None => {
            if outside {
                if let Some(tab) = app.tab_mut() {
                    tab.menu = None;
                }
                cite::clear_page0_request();
            }
        }
        MenuAction::Paste => {
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
            let _ = app.paste_clipboard_image();
        }
        MenuAction::Copy => {
            if let Some(text) = copy_text {
                ctx.copy_text(text);
            }
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
        }
        MenuAction::CopyCite => {
            if let Some(quote) = copy_text.as_deref() {
                match quoted_citation_clipboard(app, &menu, quote) {
                    Some(payload) => ctx.copy_text(payload),
                    None => {
                        // Page-0 glyphs raced out; fall back to bare quote so the
                        // click still does something visible while glyphs reload.
                        ctx.copy_text(cite::wrap_quote(quote));
                        cite::need_page0_glyphs();
                    }
                }
            }
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
        }
        MenuAction::Edit => {
            if let Some(id) = menu.hit {
                if let Some(tab) = app.tab_mut() {
                    tab.menu = None;
                }
                cite::clear_page0_request();
                app.begin_edit_undo(id);
                if let Some(tab) = app.tab_mut() {
                    tab.select_only(id);
                    tab.editing = Some(id);
                    tab.focus_edit = true;
                }
            }
        }
        MenuAction::Delete => {
            if let Some(tab) = app.tab_mut() {
                if let Some(id) = menu.hit {
                    if !tab.is_selected(id) {
                        tab.select_only(id);
                    }
                }
                tab.menu = None;
            }
            cite::clear_page0_request();
            app.delete_selected();
        }
        MenuAction::AttachText => {
            ensure_learning_from_menu(app, &menu);
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
            if app
                .tab()
                .is_some_and(|tab| tab.assistant.learning.is_some())
            {
                app.attach_learning_text();
            } else {
                app.begin_learning_select();
            }
        }
        MenuAction::AttachShot => {
            ensure_learning_from_menu(app, &menu);
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
            app.attach_learning_screenshot();
        }
        MenuAction::Explain => {
            ensure_learning_from_menu(app, &menu);
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
            app.explain_selection();
        }
        MenuAction::Lookup => {
            ensure_learning_from_menu(app, &menu);
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
            app.lookup_selection_in_browser();
        }
        MenuAction::DeletePage => {
            let page = menu.page;
            if let Some(tab) = app.tab_mut() {
                tab.menu = None;
            }
            cite::clear_page0_request();
            app.delete_page_at(page);
        }
    }
}

fn menu_item(ui: &mut egui::Ui, label: &str, shortcut: Option<&str>) -> bool {
    menu_item_enabled(ui, label, shortcut, true)
}

fn menu_item_enabled(
    ui: &mut egui::Ui,
    label: &str,
    shortcut: Option<&str>,
    enabled: bool,
) -> bool {
    let mut button = Button::new(label);
    if let Some(shortcut) = shortcut {
        button = button.shortcut_text(shortcut);
    }
    ui.add_enabled(enabled, button).clicked()
}

/// Build `quote` + citation from page-0 glyphs and the file name (no info dict yet).
fn quoted_citation_clipboard(app: &MarkerApp, menu: &ContextMenu, quote: &str) -> Option<String> {
    let tab = app.tab()?;
    let glyphs = tab.doc.glyphs.get(&0)?;
    let page0 = cite::page_text(glyphs);
    let file_name = tab
        .doc
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document.pdf");
    let page = citation_page(tab, menu);
    let citation = cite::citation_line(file_name, page + 1, &page0);
    Some(cite::format_quoted_citation(quote, &citation))
}

fn citation_page(tab: &Tab, menu: &ContextMenu) -> usize {
    if let Some(sel) = &tab.text_sel {
        return sel.page;
    }
    if tab.selected.len() == 1 {
        if let Some(annot) = tab.doc.session.get(tab.selected[0]) {
            return annot.page;
        }
    }
    menu.page
}

fn menu_context_header(
    kind: Option<&AnnotKind>,
    multi: bool,
    selected_len: usize,
    word_under_cursor: bool,
    copy_text: Option<&str>,
) -> Option<String> {
    if multi {
        return Some(format!("{selected_len} annotations"));
    }
    if let Some(kind) = kind {
        return Some(annot_menu_label(kind).into());
    }
    if word_under_cursor {
        if let Some(text) = copy_text {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                let preview: String = trimmed.chars().take(28).collect();
                let ellipsis = if trimmed.chars().count() > 28 {
                    "…"
                } else {
                    ""
                };
                return Some(format!("“{preview}{ellipsis}”"));
            }
        }
        return Some("Page text".into());
    }
    None
}

fn edit_menu_label(kind: &Option<AnnotKind>) -> &'static str {
    match kind {
        Some(AnnotKind::Text { .. }) => "Edit text",
        Some(AnnotKind::Note { .. }) => "Edit note",
        Some(AnnotKind::Math { .. }) => "Edit equation",
        _ => "Edit",
    }
}

/// Promote click-target text into a learning selection so Assistant actions
/// work from highlights, Select-tool ranges, or a word under the cursor.
fn ensure_learning_from_menu(app: &mut MarkerApp, menu: &ContextMenu) {
    let Some(tab) = app.tab_mut() else {
        return;
    };
    if tab.assistant.learning.is_some() {
        return;
    }
    if let Some(sel) = tab.text_sel.clone() {
        tab.assistant.learning = Some(LearningSelection {
            page: sel.page,
            glyph_lo: sel.glyph_lo,
            glyph_hi: sel.glyph_hi,
        });
        return;
    }
    if let Some(id) = menu.hit {
        let page = tab.doc.session.get(id).map(|annot| annot.page);
        let quads = tab.doc.session.get(id).and_then(|annot| match &annot.kind {
            AnnotKind::Highlight { quads, .. } | AnnotKind::Markup { quads, .. } => {
                Some(quads.clone())
            }
            _ => None,
        });
        if let (Some(page), Some(quads)) = (page, quads) {
            if let Some(glyphs) = tab.doc.glyphs.get(&page) {
                let indices = glyphs_intersecting_rects(glyphs, &quads);
                if let (Some(&lo), Some(&hi)) = (indices.first(), indices.last()) {
                    tab.assistant.learning = Some(LearningSelection {
                        page,
                        glyph_lo: lo,
                        glyph_hi: hi,
                    });
                    return;
                }
            }
        }
    }
    if menu.hit.is_none() {
        if let Some(glyphs) = tab.doc.glyphs.get(&menu.page) {
            if let Some(index) = glyph_at(glyphs, menu.point) {
                if let Some((lo, hi)) = word_range(glyphs, index) {
                    tab.assistant.learning = Some(LearningSelection {
                        page: menu.page,
                        glyph_lo: lo,
                        glyph_hi: hi,
                    });
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    None,
    Paste,
    Copy,
    CopyCite,
    Edit,
    Delete,
    DeletePage,
    AttachText,
    AttachShot,
    Explain,
    Lookup,
}

fn annot_menu_label(kind: &AnnotKind) -> &'static str {
    match kind {
        AnnotKind::Highlight { .. } => "Highlight",
        AnnotKind::Markup { style, .. } => match style {
            MarkupStyle::Underline => "Underline",
            MarkupStyle::StrikeOut => "Strikeout",
            MarkupStyle::Squiggly => "Squiggly",
        },
        AnnotKind::Text { .. } => "Text",
        AnnotKind::Note { .. } => "Note",
        AnnotKind::Math { .. } => "Equation",
        AnnotKind::Shape {
            kind: ShapeKind::Rect,
            ..
        } => "Rectangle",
        AnnotKind::Shape {
            kind: ShapeKind::Ellipse,
            ..
        } => "Ellipse",
        AnnotKind::Shape {
            kind: ShapeKind::Line,
            ..
        } => "Line",
        AnnotKind::Ink { .. } => "Drawing",
        AnnotKind::Image { .. } => "Image",
        AnnotKind::Foreign { kind, .. } => match kind {
            crate::annot::ForeignKind::Underline => "Underline",
            crate::annot::ForeignKind::StrikeOut => "Strikeout",
            crate::annot::ForeignKind::Squiggly => "Squiggly",
            crate::annot::ForeignKind::Ink => "Ink",
            crate::annot::ForeignKind::Polygon => "Polygon",
            crate::annot::ForeignKind::PolyLine => "Polyline",
            crate::annot::ForeignKind::Caret => "Caret",
            crate::annot::ForeignKind::FileAttachment => "Attachment",
        },
    }
}
