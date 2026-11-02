//! Math completion popup and key handling for the text editor (RF11b).

use egui::text::CCursor;
use egui::text_selection::CCursorRange;
use egui::{Event, Frame, Id, Key, Modifiers, Pos2};

use super::math_input;
use crate::app::MarkerApp;
use crate::math::rich_text::EditorOutput;

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

pub(super) fn completion_popup_open(
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

pub(super) fn apply_completion_keys(
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

pub(super) fn paint_math_completion(
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
