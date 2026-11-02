//! Inline text/math/note editors and math completion UI (RF11b).

use std::time::Duration;

use egui::text::CCursor;
use egui::text_selection::CCursorRange;
use egui::{
    Color32, Event, FontFamily, FontId, Frame, Id, ImeEvent, Key, Modifiers, Pos2, Rect, Sense,
    Stroke, TextEdit, Vec2,
};

#[path = "../math_input.rs"]
mod math_input;

use super::geometry::pdf_rect_screen;
use crate::annot::AnnotKind;
use crate::app::MarkerApp;
use crate::geom::PdfRect;
use crate::math::rich_text::{
    self, conceal_editor, preview_bubble, BubbleImage, CharSpan, EditorOutput, Render, Style,
    MATH_ERROR_IDLE_SECS,
};
use crate::math::{EntryKind, MathKey};
use crate::math_spans;
use crate::pdf::DocId;

/// Geometry and style of the text annotation currently being edited.
#[derive(Clone, Copy, Debug)]
struct EditTarget {
    id: u64,
    page: usize,
    rect: PdfRect,
    size: f32,
    color: crate::geom::Rgb,
}

fn restore_pending_caret(app: &mut MarkerApp, ctx: &egui::Context, id: u64, text_edit_id: Id) {
    // A previous Tab exit can still hand us a caret. The key itself is handled
    // in the same frame below via `apply_math_input`.
    let Some(tab) = app.tab_mut() else {
        return;
    };
    let Some(byte) = tab.pending_text_caret.take() else {
        return;
    };
    let Some(AnnotKind::Text { content, .. }) = tab.doc.session.get(id).map(|annot| &annot.kind)
    else {
        return;
    };
    let index = byte_to_char_index(content, byte);
    let mut state = TextEdit::load_state(ctx, text_edit_id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(CCursorRange::one(CCursor::new(index))));
    state.store(ctx, text_edit_id);
    tab.live_math = None;
}

fn edit_text_annot(
    app: &mut MarkerApp,
    ctx: &egui::Context,
    view: Rect,
    doc_id: DocId,
    scale: f32,
    focus: bool,
    target: EditTarget,
) {
    let EditTarget {
        id,
        page,
        rect,
        size,
        color,
    } = target;
    let screen = {
        let Some(tab) = app.tab() else {
            return;
        };
        pdf_rect_screen(&tab.doc, page, rect, view)
    };
    let area_id = Id::new(("marker-text-area", doc_id, id));
    let text_edit_id = Id::new(("marker-text", doc_id, id));
    let mut changed = false;
    let mut height = screen.height();

    restore_pending_caret(app, ctx, id, text_edit_id);

    egui::Area::new(area_id)
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .constrain(false)
        .show(ctx, |ui| {
            ui.set_max_width(screen.width().max(24.0));
            ui.visuals_mut().text_cursor.stroke = Stroke::new(2.0_f32, Color32::BLACK);
            let font_px = (size * scale).max(8.0);
            let style = math_edit_style(color.to_color32(), font_px);
            let autosnippets = app.settings.math_autosnippets;
            let bubble = {
                let Some((cache, tab)) = app.math_cache_and_tab_mut() else {
                    return;
                };
                let (plan, live_start) = {
                    let Some(AnnotKind::Text { content, .. }) =
                        tab.doc.session.get_mut(id).map(|annot| &mut annot.kind)
                    else {
                        return;
                    };
                    if let Some(text_changed) =
                        apply_math_input(ctx, ui, text_edit_id, content, autosnippets, id)
                    {
                        changed |= text_changed;
                    }
                    let render = |inner: &str, display: bool| {
                        cache_render(cache, inner, display, size, color, scale)
                    };
                    let output = conceal_editor(
                        ui,
                        text_edit_id,
                        content,
                        screen.width().max(24.0),
                        &style,
                        &render,
                    );
                    if focus {
                        output.response.request_focus();
                    }
                    changed |= output.response.changed();
                    height = output.response.rect.height().max(size * scale);
                    paint_concealed_math(ui, cache, &output, size, color);
                    paint_math_errors(ui, &output, style.error);
                    paint_math_completion(ctx, ui, text_edit_id, id, content, &output);
                    let plan = bubble_plan(ctx, content, &output, cache, size, color, changed);
                    let live_start = output.caret.and_then(|(index, _)| {
                        let byte = char_index_to_byte(content, index);
                        math_spans::math_span_at(content, byte).and_then(|span| {
                            (span.closed && byte > span.start && byte < span.end)
                                .then_some(span.start)
                        })
                    });
                    (plan, live_start)
                };
                tab.live_math = live_start.map(|start| (id, start));
                plan
            };
            if let Some(plan) = bubble {
                paint_math_bubble(app, ctx, view, doc_id, id, &plan, scale);
            }
        });

    let mut queue_keys: Vec<u64> = Vec::new();
    if changed {
        if let Some(tab) = app.tab() {
            if let Some(AnnotKind::Text { content, .. }) =
                tab.doc.session.get(id).map(|annot| &annot.kind)
            {
                for span in math_spans::closed_math_spans(content) {
                    let inner = &content[span.inner_start..span.inner_end];
                    if !inner.trim().is_empty() {
                        queue_keys.push(math_spans::span_key(inner, span.display));
                    }
                }
            }
        }
    } else if let Some((edit_id, start)) = app.tab().and_then(|tab| tab.live_math) {
        if edit_id == id {
            if let Some(tab) = app.tab() {
                if let Some(AnnotKind::Text { content, .. }) =
                    tab.doc.session.get(id).map(|annot| &annot.kind)
                {
                    if let Some(span) = math_spans::math_span_at(content, start) {
                        let inner = &content[span.inner_start..span.inner_end];
                        if span.closed && !inner.trim().is_empty() {
                            let key = math_spans::span_key(inner, span.display);
                            let needs = !app.inline_preview(id, key).is_some_and(|preview| {
                                !preview.pending
                                    && (preview.texture.is_some() || preview.error.is_some())
                            });
                            if needs {
                                queue_keys.push(key);
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(tab) = app.tab_mut() {
        tab.focus_edit = false;
        if let Some(AnnotKind::Text { rect, size, .. }) =
            tab.doc.session.get_mut(id).map(|annot| &mut annot.kind)
        {
            let min_h = (height / scale).max(*size * 1.35);
            if rect.height() + 0.5 < min_h {
                rect.y1 = rect.y0 + min_h;
                changed = true;
            }
        }
        if changed {
            tab.doc.session.mark_dirty(id);
            tab.save.touch();
        }
    }
    queue_keys.sort_unstable();
    queue_keys.dedup();
    for key in queue_keys {
        app.queue_inline_math(id, key);
    }
}

fn byte_to_char_index(text: &str, byte: usize) -> usize {
    text.char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .take_while(|&i| i <= byte.min(text.len()))
        .count()
        .saturating_sub(1)
}

fn char_index_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

struct BubblePlan {
    anchor: Rect,
    state: Render,
    current: Option<(egui::TextureHandle, f32, f32)>,
    ordinal: usize,
    idle_secs: f64,
}

fn math_edit_style(ink: Color32, font_px: f32) -> Style {
    Style {
        font: FontId::new(font_px, FontFamily::Proportional),
        color: ink,
        source_color: Color32::from_rgb(ink.r() / 2 + 36, ink.g() / 2 + 64, ink.b() / 2 + 110),
        source_bg: Color32::from_rgba_unmultiplied(80, 120, 220, 36),
        error: Color32::from_rgb(210, 70, 60),
    }
}

fn cache_render(
    cache: &crate::math::MathCache<crate::app::InlineReady>,
    inner: &str,
    display: bool,
    size_pt: f32,
    color: crate::geom::Rgb,
    scale: f32,
) -> Render {
    let key = MathKey::new(inner, display, size_pt, color);
    match cache.get(&key) {
        Some(EntryKind::Ready { value, .. }) if value.w_pt > 0.5 && value.h_pt > 0.5 => {
            Render::Ready(Vec2::new(value.w_pt * scale, value.h_pt * scale))
        }
        Some(EntryKind::Error { message }) => Render::Error(message.clone()),
        Some(EntryKind::Pending { .. } | EntryKind::Ready { .. }) | None => Render::Pending,
    }
}

fn ready_texture(
    cache: &crate::math::MathCache<crate::app::InlineReady>,
    inner: &str,
    display: bool,
    size_pt: f32,
    color: crate::geom::Rgb,
) -> Option<(egui::TextureHandle, f32, f32)> {
    let key = MathKey::new(inner, display, size_pt, color);
    match cache.get(&key)? {
        EntryKind::Ready { value, .. } => value
            .texture
            .clone()
            .map(|texture| (texture, value.w_pt, value.h_pt)),
        EntryKind::Pending { .. } | EntryKind::Error { .. } => None,
    }
}

fn ready_white_texture(
    cache: &crate::math::MathCache<crate::app::InlineReady>,
    inner: &str,
    display: bool,
    size_pt: f32,
    color: crate::geom::Rgb,
) -> Option<(egui::TextureHandle, f32, f32)> {
    let key = MathKey::new(inner, display, size_pt, color);
    match cache.get(&key)? {
        EntryKind::Ready { value, .. } => value
            .white_texture
            .clone()
            .map(|texture| (texture, value.w_pt, value.h_pt)),
        EntryKind::Pending { .. } | EntryKind::Error { .. } => None,
    }
}

/// Autopair / tab-out / completion pre-filter (R7–R9). Runs before
/// `conceal_editor` so `TextEdit` never sees a handled event. Skips while IME
/// is composing. When the completion popup is open, Tab/Enter accept and
/// arrows move the highlight; Esc is handled via `reclaim_edit_after_completion_escape`.
fn apply_math_input(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    id: Id,
    content: &mut String,
    autosnippets: bool,
    annot_id: u64,
) -> Option<bool> {
    if ime_composing(ui) {
        return None;
    }
    let mut state = TextEdit::load_state(ctx, id)?;
    let range = state.cursor.char_range()?;
    let caret = range.primary.index;

    if let Some(result) = apply_completion_keys(ctx, ui, id, content, range, annot_id) {
        let text_changed = result.0 != *content;
        *content = result.0;
        state.cursor.set_char_range(Some(result.1));
        state.store(ctx, id);
        return Some(text_changed);
    }

    // While a completion list is visible, Tab/Enter are reserved for accept
    // (handled above). Skip the tab-out path so they don't fall through.
    let popup_open = completion_popup_open(ctx, id, content, caret);

    let mut hit: Option<(usize, (String, CCursorRange))> = None;
    ui.input(|input| {
        for (index, event) in input.events.iter().enumerate() {
            if popup_open
                && matches!(
                    event,
                    Event::Key {
                        key: Key::Tab | Key::Enter | Key::ArrowUp | Key::ArrowDown | Key::Escape,
                        pressed: true,
                        ..
                    }
                )
            {
                continue;
            }
            if let Some(result) = math_input::apply(content, range, event, autosnippets) {
                hit = Some((index, result));
                break;
            }
        }
    });
    let (index, (next, new_range)) = hit?;
    ui.input_mut(|input| {
        if index < input.events.len() {
            input.events.remove(index);
        }
    });
    let text_changed = next != *content;
    *content = next;
    state.cursor.set_char_range(Some(new_range));
    state.store(ctx, id);
    Some(text_changed)
}

fn completion_dismissed_id(text_edit_id: Id) -> Id {
    text_edit_id.with("math-completion-dismissed")
}

fn completion_selected_id(text_edit_id: Id) -> Id {
    text_edit_id.with("math-completion-selected")
}

#[derive(Clone)]
struct CompletionSelection {
    prefix: String,
    index: usize,
}

fn completion_session_id() -> Id {
    Id::new("marker-math-completion-session")
}

#[derive(Clone)]
struct CompletionSession {
    annot_id: u64,
    text_edit_id: Id,
    query: String,
}

fn completion_hits(
    content: &str,
    caret: usize,
) -> Option<(
    math_input::CompletionQuery,
    Vec<&'static math_input::LatexCommand>,
)> {
    let query = math_input::completion_query(content, caret)?;
    let hits = math_input::rank_completions(&query.prefix);
    if hits.is_empty() {
        None
    } else {
        Some((query, hits))
    }
}

fn completion_popup_open(
    ctx: &egui::Context,
    text_edit_id: Id,
    content: &str,
    caret: usize,
) -> bool {
    let Some((query, _)) = completion_hits(content, caret) else {
        return false;
    };
    let dismissed = ctx.data(|data| data.get_temp::<String>(completion_dismissed_id(text_edit_id)));
    dismissed.as_ref() != Some(&query.prefix)
}

fn apply_completion_keys(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    text_edit_id: Id,
    content: &str,
    range: CCursorRange,
    annot_id: u64,
) -> Option<(String, CCursorRange)> {
    if !range.is_empty() {
        return None;
    }
    let caret = range.primary.index;
    let (query, hits) = completion_hits(content, caret)?;
    let dismissed = ctx.data(|data| data.get_temp::<String>(completion_dismissed_id(text_edit_id)));
    if dismissed.as_ref() == Some(&query.prefix) {
        return None;
    }

    // Clear a stale dismiss when the query changes.
    if dismissed.is_some() {
        ctx.data_mut(|data| {
            data.remove::<String>(completion_dismissed_id(text_edit_id));
        });
    }

    let mut selected = ctx
        .data(|data| data.get_temp::<CompletionSelection>(completion_selected_id(text_edit_id)))
        .map(|sel| {
            if sel.prefix == query.prefix {
                sel.index
            } else {
                0
            }
        })
        .unwrap_or(0);
    if selected >= hits.len() {
        selected = 0;
    }

    let mut accept = false;
    let mut handled_index: Option<usize> = None;
    let mut navigated = false;
    ui.input(|input| {
        for (index, event) in input.events.iter().enumerate() {
            let Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            if !modifiers.is_none() {
                continue;
            }
            match *key {
                Key::ArrowDown => {
                    selected = (selected + 1) % hits.len();
                    handled_index = Some(index);
                    navigated = true;
                    break;
                }
                Key::ArrowUp => {
                    selected = (selected + hits.len() - 1) % hits.len();
                    handled_index = Some(index);
                    navigated = true;
                    break;
                }
                Key::Tab | Key::Enter => {
                    accept = true;
                    handled_index = Some(index);
                    break;
                }
                _ => {}
            }
        }
    });

    if let Some(index) = handled_index {
        ui.input_mut(|input| {
            if index < input.events.len() {
                input.events.remove(index);
            }
            if accept {
                input.consume_key(Modifiers::NONE, Key::Tab);
                input.consume_key(Modifiers::NONE, Key::Enter);
            } else if navigated {
                input.consume_key(Modifiers::NONE, Key::ArrowUp);
                input.consume_key(Modifiers::NONE, Key::ArrowDown);
            }
        });
    }

    ctx.data_mut(|data| {
        data.insert_temp(
            completion_selected_id(text_edit_id),
            CompletionSelection {
                prefix: query.prefix.clone(),
                index: selected,
            },
        );
        data.insert_temp(
            completion_session_id(),
            CompletionSession {
                annot_id,
                text_edit_id,
                query: query.prefix.clone(),
            },
        );
    });

    if accept {
        let cmd = hits[selected];
        ctx.data_mut(|data| {
            data.remove::<CompletionSelection>(completion_selected_id(text_edit_id));
            data.remove::<CompletionSession>(completion_session_id());
            data.remove::<String>(completion_dismissed_id(text_edit_id));
        });
        return Some(math_input::accept_completion(content, &query, cmd));
    }

    None
}

/// `handle_keys` runs before the editor and clears `editing` on Escape. When a
/// completion popup was open, put the annotation back into edit mode and treat
/// Escape as "dismiss the list" only.
pub(super) fn reclaim_edit_after_completion_escape(app: &mut MarkerApp, ctx: &egui::Context) {
    if !ctx.input(|input| input.key_pressed(Key::Escape)) {
        return;
    }
    let Some(session) =
        ctx.data(|data| data.get_temp::<CompletionSession>(completion_session_id()))
    else {
        return;
    };
    let Some(tab) = app.tab_mut() else {
        return;
    };
    if tab.editing.is_some() {
        return;
    }
    tab.editing = Some(session.annot_id);
    tab.focus_edit = true;
    let text_edit_id = session.text_edit_id;
    let query = session.query.clone();
    let annot_id = session.annot_id;
    ctx.data_mut(|data| {
        data.insert_temp(completion_dismissed_id(text_edit_id), query);
        data.remove::<CompletionSession>(completion_session_id());
        data.remove::<CompletionSelection>(completion_selected_id(text_edit_id));
    });
    ctx.input_mut(|input| {
        input.consume_key(Modifiers::NONE, Key::Escape);
    });
    app.begin_edit_undo(annot_id);
}

fn paint_math_completion(
    ctx: &egui::Context,
    ui: &egui::Ui,
    text_edit_id: Id,
    annot_id: u64,
    content: &str,
    output: &EditorOutput,
) {
    let Some((index, _)) = output.caret else {
        ctx.data_mut(|data| data.remove::<CompletionSession>(completion_session_id()));
        return;
    };
    let Some((query, hits)) = completion_hits(content, index) else {
        ctx.data_mut(|data| {
            data.remove::<CompletionSession>(completion_session_id());
            data.remove::<CompletionSelection>(completion_selected_id(text_edit_id));
        });
        return;
    };
    let dismissed = ctx.data(|data| data.get_temp::<String>(completion_dismissed_id(text_edit_id)));
    if dismissed.as_ref() == Some(&query.prefix) {
        ctx.data_mut(|data| data.remove::<CompletionSession>(completion_session_id()));
        return;
    }
    if dismissed.is_some() {
        ctx.data_mut(|data| {
            data.remove::<String>(completion_dismissed_id(text_edit_id));
        });
    }

    let selected = ctx
        .data(|data| data.get_temp::<CompletionSelection>(completion_selected_id(text_edit_id)))
        .map(|sel| {
            if sel.prefix == query.prefix {
                sel.index.min(hits.len().saturating_sub(1))
            } else {
                0
            }
        })
        .unwrap_or(0);

    ctx.data_mut(|data| {
        data.insert_temp(
            completion_session_id(),
            CompletionSession {
                annot_id,
                text_edit_id,
                query: query.prefix.clone(),
            },
        );
    });

    let caret_rect = output.galley.pos_from_cursor(CCursor {
        index,
        prefer_next_row: true,
    });
    let anchor = Pos2::new(
        output.galley_pos.x + caret_rect.min.x,
        output.galley_pos.y + caret_rect.max.y + 2.0,
    );

    let visible = hits.len().min(8);
    egui::Area::new(text_edit_id.with("math-completion-popup"))
        .order(egui::Order::Tooltip)
        .fixed_pos(anchor)
        .interactable(false)
        .show(ctx, |ui| {
            Frame::popup(ui.style())
                .inner_margin(egui::Margin::symmetric(6, 4))
                .show(ui, |ui| {
                    ui.set_min_width(120.0);
                    ui.spacing_mut().item_spacing.y = 1.0;
                    for (i, cmd) in hits.iter().take(visible).enumerate() {
                        let row = format!("{}  \\{}", cmd.preview, cmd.name);
                        let rich = if i == selected {
                            egui::RichText::new(row).strong()
                        } else {
                            egui::RichText::new(row)
                        };
                        ui.label(rich);
                    }
                    if hits.len() > visible {
                        ui.label(
                            egui::RichText::new(format!("+{} more", hits.len() - visible)).weak(),
                        );
                    }
                });
        });
    // Keep the TextEdit focused; the popup must not steal keyboard focus.
    let _ = ui;
}

fn ime_composing(ui: &egui::Ui) -> bool {
    ui.input(|input| {
        input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Ime(ImeEvent::Preedit(_)) | egui::Event::Ime(ImeEvent::Enabled)
            )
        })
    })
}

fn paint_concealed_math(
    ui: &egui::Ui,
    cache: &crate::math::MathCache<crate::app::InlineReady>,
    output: &EditorOutput,
    size_pt: f32,
    color: crate::geom::Rgb,
) {
    let painter = ui.painter();
    for (rect, inner, display, span) in &output.math {
        let Some((texture, _, _)) = ready_texture(cache, inner, *display, size_pt, color) else {
            continue;
        };
        painter.image(
            texture.id(),
            *rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        if let Some((a, b)) = output.caret {
            if a != b {
                let (lo, hi) = (a.min(b), a.max(b));
                if lo <= span.start && hi >= span.end {
                    painter.rect_filled(
                        *rect,
                        0.0,
                        ui.visuals().selection.bg_fill.gamma_multiply(0.45),
                    );
                }
            }
        }
    }
}

fn paint_math_errors(ui: &egui::Ui, output: &EditorOutput, color: Color32) {
    let pointer = ui.input(|input| input.pointer.hover_pos());
    let caret = output.caret.map(|(index, _)| index);
    for (span, message) in &output.errors {
        paint_dotted_span(ui.painter(), &output.galley, output.galley_pos, span, color);
        let inside = caret.is_some_and(|index| index > span.start && index < span.end);
        if inside {
            continue;
        }
        let rect = span_screen_rect(&output.galley, output.galley_pos, span);
        if pointer.is_some_and(|pos| rect.contains(pos)) {
            egui::Tooltip::always_open(
                ui.ctx().clone(),
                ui.layer_id(),
                Id::new(("marker-math-tip", span.start, span.end)),
                rect,
            )
            .gap(6.0)
            .show(|ui| {
                ui.label(rich_text::latex_error_message(message));
            });
        }
    }
}

fn paint_dotted_span(
    painter: &egui::Painter,
    galley: &egui::Galley,
    origin: Pos2,
    span: &CharSpan,
    color: Color32,
) {
    let stroke = Stroke::new(1.0_f32, color);
    for index in span.start..span.end {
        let glyph = galley
            .pos_from_cursor(CCursor {
                index,
                prefer_next_row: true,
            })
            .translate(origin.to_vec2());
        if (index - span.start) % 2 != 0 {
            continue;
        }
        let y = glyph.max.y - 1.0;
        let x1 = (glyph.min.x + 2.0).min(glyph.max.x);
        if x1 > glyph.min.x {
            painter.line_segment([Pos2::new(glyph.min.x, y), Pos2::new(x1, y)], stroke);
        }
    }
}

fn span_screen_rect(galley: &egui::Galley, origin: Pos2, span: &CharSpan) -> Rect {
    let left = galley.pos_from_cursor(CCursor {
        index: span.start,
        prefer_next_row: true,
    });
    let right = galley.pos_from_cursor(CCursor {
        index: span.end.saturating_sub(1),
        prefer_next_row: true,
    });
    Rect::from_min_max(
        origin + left.min.to_vec2(),
        origin + Vec2::new(right.max.x, right.max.y.max(left.max.y)),
    )
}

fn bubble_plan(
    ctx: &egui::Context,
    content: &str,
    output: &EditorOutput,
    cache: &crate::math::MathCache<crate::app::InlineReady>,
    size_pt: f32,
    color: crate::geom::Rgb,
    typed: bool,
) -> Option<BubblePlan> {
    let (index, _) = output.caret?;
    let byte = char_index_to_byte(content, index);
    let span = math_spans::math_span_at(content, byte)?;
    if byte <= span.start || byte >= span.end {
        return None;
    }
    let inner = &content[span.inner_start..span.inner_end];
    let state = if inner.trim().is_empty() {
        Render::Pending
    } else {
        cache_render(cache, inner, span.display, size_pt, color, 1.0)
    };
    let now = ctx.input(|input| input.time);
    let idle_id = Id::new(("marker-math-idle", output.response.id));
    if typed || output.response.changed() {
        ctx.data_mut(|data| data.insert_temp(idle_id, now));
    }
    let idle_secs = match ctx.data(|data| data.get_temp::<f64>(idle_id)) {
        Some(start) => now - start,
        None => MATH_ERROR_IDLE_SECS,
    };
    if matches!(state, Render::Error(_)) && idle_secs < MATH_ERROR_IDLE_SECS {
        ctx.request_repaint_after(Duration::from_secs_f64(MATH_ERROR_IDLE_SECS - idle_secs));
    }
    Some(BubblePlan {
        anchor: span_screen_rect(
            &output.galley,
            output.galley_pos,
            &char_span_of(&span, content),
        ),
        state,
        current: ready_white_texture(cache, inner, span.display, size_pt, color),
        ordinal: span.index,
        idle_secs,
    })
}

fn char_span_of(span: &math_spans::MathSpanRef, content: &str) -> CharSpan {
    let (start, end) = math_spans::byte_range_char_indices(content, span.start, span.end);
    let (inner_start, inner_end) =
        math_spans::byte_range_char_indices(content, span.inner_start, span.inner_end);
    CharSpan {
        start,
        end,
        inner_start,
        inner_end,
        display: span.display,
        closed: span.closed,
    }
}

fn paint_math_bubble(
    app: &MarkerApp,
    ctx: &egui::Context,
    view: Rect,
    doc_id: DocId,
    id: u64,
    plan: &BubblePlan,
    scale: f32,
) {
    let last = app.last_good_inline(id, plan.ordinal);
    let has_last = last
        .as_ref()
        .is_some_and(|preview| preview.white_texture.is_some());
    let bubble = preview_bubble(&plan.state, has_last, plan.idle_secs);
    if bubble.image == BubbleImage::None && bubble.error.is_none() {
        return;
    }
    let shown = match bubble.image {
        BubbleImage::Current => plan
            .current
            .clone()
            .map(|(texture, w, h)| (texture, w, h, Color32::WHITE)),
        BubbleImage::LastGoodDimmed => last.and_then(|preview| {
            preview.white_texture.map(|texture| {
                (
                    texture,
                    preview.width_pt,
                    preview.height_pt,
                    Color32::from_white_alpha(150),
                )
            })
        }),
        BubbleImage::None => None,
    };
    let img_h = shown
        .as_ref()
        .map(|(_, w, h, _)| {
            let nat = Vec2::new(*w * scale, *h * scale);
            let fit = (240.0 / nat.x.max(1.0)).min(72.0 / nat.y.max(1.0)).min(1.0);
            nat.y * fit
        })
        .unwrap_or(0.0);
    let extra = if bubble.error.is_some() { 22.0 } else { 0.0 };
    let est = img_h + 16.0 + extra;
    let above = plan.anchor.min.y - est > view.min.y + 4.0;
    let pos = if above {
        Pos2::new(plan.anchor.min.x, plan.anchor.min.y - est)
    } else {
        Pos2::new(plan.anchor.min.x, plan.anchor.max.y + 4.0)
    };
    egui::Area::new(Id::new(("marker-math-bubble", doc_id, id)))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos)
        .constrain(false)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(Color32::BLACK)
                .show(ui, |ui| {
                    if let Some((texture, w, h, tint)) = shown {
                        let nat = Vec2::new(w * scale, h * scale);
                        let fit = (240.0 / nat.x.max(1.0)).min(72.0 / nat.y.max(1.0)).min(1.0);
                        let (rect, _) = ui.allocate_exact_size(nat * fit, Sense::hover());
                        ui.painter().image(
                            texture.id(),
                            rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            tint,
                        );
                    }
                    if let Some(error) = &bubble.error {
                        ui.label(
                            egui::RichText::new(error)
                                .color(Color32::from_rgb(210, 70, 60))
                                .size(12.0),
                        );
                    }
                });
        });
}

pub(super) fn inline_editors(app: &mut MarkerApp, ctx: &egui::Context, view: Rect) {
    let snapshot = {
        let Some(tab) = app.tab() else {
            return;
        };
        let Some(id) = tab.editing else {
            return;
        };
        let Some(annot) = tab.doc.session.get(id) else {
            return;
        };
        (
            id,
            annot.page,
            tab.doc.doc_id,
            tab.doc.scale,
            tab.focus_edit,
            tab.previews
                .get(&id)
                .and_then(|preview| preview.error.clone()),
            annot.kind.clone(),
        )
    };
    let (id, page, doc_id, scale, focus, error, kind) = snapshot;
    match kind {
        AnnotKind::Text {
            rect, size, color, ..
        } => {
            edit_text_annot(
                app,
                ctx,
                view,
                doc_id,
                scale,
                focus,
                EditTarget {
                    id,
                    page,
                    rect,
                    size,
                    color,
                },
            );
        }
        AnnotKind::Math { rect, .. } => {
            let screen = {
                let Some(tab) = app.tab() else {
                    return;
                };
                pdf_rect_screen(&tab.doc, page, rect, view)
            };
            let mut changed = false;
            egui::Area::new(Id::new(("marker-math", doc_id, id)))
                .order(egui::Order::Foreground)
                .fixed_pos(Pos2::new(screen.min.x, screen.max.y + 4.0))
                .constrain(false)
                .show(ctx, |ui| {
                    ui.set_max_width(screen.width().max(180.0).max(280.0));
                    let Some(tab) = app.tab_mut() else {
                        return;
                    };
                    let Some(AnnotKind::Math { source, .. }) =
                        tab.doc.session.get_mut(id).map(|annot| &mut annot.kind)
                    else {
                        return;
                    };
                    ui.label(egui::RichText::new("LaTeX").weak().size(11.0));
                    let response = ui.add(
                        TextEdit::multiline(source)
                            .font(FontId::new(13.0, FontFamily::Monospace))
                            .desired_width(screen.width().max(280.0))
                            .desired_rows(3)
                            .hint_text(r"\frac{1}{2}"),
                    );
                    if focus {
                        response.request_focus();
                    }
                    changed = response.changed();
                    if let Some(error) = &error {
                        ui.label(
                            egui::RichText::new(error)
                                .color(Color32::from_rgb(220, 110, 100))
                                .size(11.0),
                        );
                    }
                });
            if let Some(tab) = app.tab_mut() {
                tab.focus_edit = false;
                tab.live_math = None;
                if changed {
                    tab.doc.session.mark_dirty(id);
                    tab.save.touch();
                }
            }
            if changed {
                app.queue_math(id);
            }
        }
        AnnotKind::Note { rect, .. } => {
            let screen = {
                let Some(tab) = app.tab() else {
                    return;
                };
                pdf_rect_screen(&tab.doc, page, rect, view)
            };
            let mut changed = false;
            egui::Area::new(Id::new(("marker-note", doc_id, id)))
                .order(egui::Order::Foreground)
                .fixed_pos(Pos2::new(screen.max.x + 8.0, screen.min.y))
                .constrain(false)
                .show(ctx, |ui| {
                    ui.set_min_width(200.0);
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label(egui::RichText::new("Note").weak().size(11.0));
                        let Some(tab) = app.tab_mut() else {
                            return;
                        };
                        let Some(AnnotKind::Note { content, .. }) =
                            tab.doc.session.get_mut(id).map(|annot| &mut annot.kind)
                        else {
                            return;
                        };
                        let response = ui.add(
                            TextEdit::multiline(content)
                                .desired_width(220.0)
                                .desired_rows(4),
                        );
                        if focus {
                            response.request_focus();
                        }
                        changed = response.changed();
                    });
                });
            if let Some(tab) = app.tab_mut() {
                tab.focus_edit = false;
                tab.live_math = None;
                if changed {
                    tab.doc.session.mark_dirty(id);
                    tab.save.touch();
                }
            }
        }
        _ => {
            if let Some(tab) = app.tab_mut() {
                tab.editing = None;
                tab.live_math = None;
            }
        }
    }
}
