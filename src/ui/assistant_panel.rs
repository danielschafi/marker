//! Cursor learning assistant side panel (RF12).
//!
//! UI still collects local bools then applies them; RF9b [`crate::app::input::Action`]
//! covers only a subset of panel buttons, so no second action enum was introduced.

use egui::{Align, Button, Color32, Key, Layout, RichText, ScrollArea, Stroke, TextEdit, Vec2};

use crate::app::{CreateKind, MarkerApp};
use crate::assistant::{assistant_session_hint, AssistantAttachment, AssistantRole, CaptureMode};
use crate::geom::Rgb;
use crate::math_spans;

use super::palette;

pub(super) fn assistant_panel(app: &mut MarkerApp, ctx: &egui::Context) {
    if !app.assistant_open || app.tab().is_none() {
        return;
    }
    let colors = palette(ctx);
    let max_w = (ctx.content_rect().width() * 0.45).clamp(300.0, 640.0);
    let mut send = false;
    let mut stop = false;
    let mut new_chat = false;
    let mut close = false;
    let mut remove_attach = None;
    let mut begin_text = false;
    let mut begin_region = false;
    let mut study: Option<&'static str> = None;
    let mut generate_cards = false;
    let assistant_ready = app.assistant_is_configured();

    egui::SidePanel::right("assistant")
        .resizable(true)
        .default_width(360.0)
        .width_range(300.0..=max_w)
        .frame(
            egui::Frame::new()
                .fill(colors.chrome)
                .stroke(Stroke::new(1.0_f32, colors.hairline))
                .inner_margin(10.0),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Cursor").strong().size(14.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .small_button("×")
                        .on_hover_text("Hide panel (keeps session and any running reply)")
                        .clicked()
                    {
                        close = true;
                    }
                    if ui
                        .small_button("New")
                        .on_hover_text(
                            "New chat — clears local transcript; next send creates a fresh Cursor chat",
                        )
                        .clicked()
                    {
                        new_chat = true;
                    }
                });
            });
            let capture = app.capture;
            let status = app
                .tab()
                .and_then(|t| {
                    t.assistant
                        .status_line
                        .clone()
                        .or_else(|| assistant_session_hint(&t.assistant))
                })
                .unwrap_or_else(|| match capture {
                    CaptureMode::LearningText => "Drag to select text for the assistant.".into(),
                    CaptureMode::Region => "Drag a rectangle to capture a screenshot.".into(),
                    CaptureMode::None => "Ask about the open PDF.".into(),
                });
            ui.label(RichText::new(status).weak().size(12.0));
            ui.add_space(4.0);

            let streaming = app.tab().is_some_and(|t| t.assistant.streaming);
            let error = app.tab().and_then(|t| t.assistant.error.clone());
            paint_assistant_turns(ui, app);

            if let Some(err) = error {
                ui.colored_label(Color32::from_rgb(200, 80, 80), err);
            }

            ui.horizontal_wrapped(|ui| {
                if ui
                    .small_button("Select text")
                    .on_hover_text("Drag to select text and attach it (Ctrl+Shift+A)")
                    .clicked()
                {
                    begin_text = true;
                }
                if ui
                    .small_button("Screenshot")
                    .on_hover_text("Capture a page region (Ctrl+Alt+S)")
                    .clicked()
                {
                    begin_region = true;
                }
                if streaming
                    && ui.small_button("Stop").on_hover_text("Ctrl+.").clicked() {
                        stop = true;
                    }
            });

            let attachments: Vec<(String, Option<egui::TextureHandle>)> = app
                .tab()
                .map(|t| {
                    t.assistant
                        .attachments
                        .iter()
                        .map(|a| {
                            let tex = match a {
                                AssistantAttachment::Image { texture, .. } => texture.clone(),
                                _ => None,
                            };
                            (a.label(), tex)
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !attachments.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for (i, (label, tex)) in attachments.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if let Some(tex) = tex {
                                ui.image((tex.id(), Vec2::new(28.0, 28.0)));
                            }
                            let chip = ui.button(format!("{label} ×"));
                            if chip.clicked() {
                                remove_attach = Some(i);
                            }
                        });
                    }
                });
            }

            if let Some(tab) = app.tab_mut() {
                if !tab.assistant.disclosed {
                    ui.checkbox(
                        &mut tab.assistant.disclosed,
                        "I understand prompts and attachments go through my Cursor account",
                    );
                }
                ui.checkbox(
                    &mut tab.assistant.agent_mode,
                    "Agent mode (tools / writes)",
                )
                .on_hover_text(
                    "Off (default): Cursor Ask — read-only Q&A.\n\
                     On: full Cursor Agent — may use tools and write files in the session workspace. Opt-in only.",
                );
                if tab.assistant.agent_mode {
                    ui.label(
                        RichText::new("Agent mode can run tools and write files.")
                            .weak()
                            .size(11.0)
                            .color(Color32::from_rgb(180, 120, 40)),
                    );
                }
            }

            // Study presets sit above the composer; agent mode stays on its own row.
            ui.horizontal_wrapped(|ui| {
                for &(label, prompt) in MarkerApp::STUDY_PROMPTS {
                    if ui
                        .small_button(label)
                        .on_hover_text(prompt)
                        .clicked()
                    {
                        study = Some(prompt);
                    }
                }
                if assistant_ready
                    && ui
                        .small_button("Generate cards from selection")
                        .on_hover_text(
                            "Ask Cursor for front/back cards as a fenced TSV block. \
                             Does not force agent mode. Anki can import the resulting CSV.",
                        )
                        .clicked()
                {
                    generate_cards = true;
                }
            });

            if let Some(tab) = app.tab_mut() {
                let response = ui.add(
                    TextEdit::multiline(&mut tab.assistant.draft)
                        .id_salt("assistant-draft")
                        .desired_width(ui.available_width())
                        .desired_rows(3)
                        .hint_text("Ask Cursor…"),
                );
                let enter = response.has_focus()
                    && ui.input(|input| {
                        input.key_pressed(Key::Enter) && !input.modifiers.shift
                    });
                if enter {
                    // Consume the newline Enter would insert by trimming trailing newline.
                    if tab.assistant.draft.ends_with('\n') {
                        tab.assistant.draft.pop();
                    }
                    send = true;
                }
            }

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!streaming, Button::new("Send"))
                    .clicked()
                {
                    send = true;
                }
            });
        });

    if close {
        // Hide only — do not call new_chat(); session + streaming stay alive (#27).
        app.set_assistant_open(false);
    }
    if new_chat {
        app.assistant_new_chat();
    }
    if stop {
        app.assistant_stop();
    }
    if begin_text {
        app.begin_learning_select();
    }
    if begin_region {
        app.begin_region_capture();
    }
    if let Some(prompt) = study {
        app.study_prompt(prompt);
    }
    if generate_cards {
        app.generate_cards_from_selection();
    }
    if let Some(i) = remove_attach {
        if let Some(tab) = app.tab_mut() {
            if i < tab.assistant.attachments.len() {
                tab.assistant.attachments.remove(i);
            }
        }
    }
    if send {
        app.assistant_send();
    }
}

/// Paint the transcript from a borrow of the live turns.
fn paint_assistant_turns(ui: &mut egui::Ui, app: &mut MarkerApp) {
    let active = app.tab_index(app.active);
    let streaming = active
        .and_then(|i| app.tabs.get(i))
        .is_some_and(|tab| tab.assistant.streaming);
    let math_color = {
        let c = palette(ui.ctx()).text;
        Rgb::new(c.r(), c.g(), c.b())
    };
    // Finished turns only — streaming keeps source until the turn completes (ST7).
    prepare_assistant_answer_math(app, math_color);

    let mut save: Option<(usize, CreateKind)> = None;
    let mut copy: Option<usize> = None;
    {
        // Disjoint fields so Markdown cache, math cache, and turns can be borrowed together.
        let math_cache = &app.math.cache;
        let md_cache = &mut app.assistant_md_cache;
        let turns = active
            .and_then(|i| app.tabs.get(i))
            .map(|tab| tab.assistant.turns.as_slice())
            .unwrap_or(&[]);
        ScrollArea::vertical()
            .id_salt("assistant-transcript")
            .auto_shrink([false, false])
            .max_height(ui.available_height() - 180.0)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                if turns.is_empty() {
                    ui.label(
                        RichText::new(
                            "Attach selected text or a page region, then ask Cursor a question.",
                        )
                        .weak(),
                    );
                }
                for (index, turn) in turns.iter().enumerate() {
                    let who = match turn.role {
                        AssistantRole::User => "You",
                        AssistantRole::Assistant => "Cursor",
                    };
                    ui.label(RichText::new(who).strong().size(12.0));
                    match turn.role {
                        // User prompts stay plain so typed Markdown is not re-interpreted.
                        AssistantRole::User => {
                            ui.label(&turn.text);
                        }
                        AssistantRole::Assistant => {
                            // While streaming, prefer plain text so half-open fences stay readable
                            // and so we do not enqueue math for every delta.
                            if turn.incomplete {
                                if !turn.text.is_empty() {
                                    ui.label(&turn.text);
                                }
                            } else if math_spans::has_math(&turn.text) {
                                paint_assistant_answer_math(ui, math_cache, &turn.text, math_color);
                            } else {
                                crate::assistant_md::show_markdown(ui, md_cache, &turn.text);
                            }
                        }
                    }
                    if turn.incomplete {
                        ui.label(RichText::new("…").weak());
                    } else if matches!(turn.role, AssistantRole::Assistant) && !turn.text.is_empty()
                    {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(!streaming, Button::new("Copy"))
                                .on_hover_text("Copy the raw answer text")
                                .clicked()
                            {
                                copy = Some(index);
                            }
                            if ui
                                .add_enabled(!streaming, Button::new("Save to page"))
                                .on_hover_text(
                                    "Insert this answer as a text annotation on the page",
                                )
                                .clicked()
                            {
                                save = Some((index, CreateKind::Text));
                            }
                            if ui
                                .add_enabled(!streaming, Button::new("Save as note"))
                                .on_hover_text("Insert this answer as a sticky note on the page")
                                .clicked()
                            {
                                save = Some((index, CreateKind::Note));
                            }
                        });
                    }
                    ui.add_space(8.0);
                }
            });
    }
    if let Some(index) = copy {
        let text = active
            .and_then(|i| app.tabs.get(i))
            .and_then(|tab| tab.assistant.turns.get(index))
            .map(|turn| turn.text.clone());
        if let Some(text) = text {
            ui.ctx().copy_text(text);
        }
    }
    if let Some((index, kind)) = save {
        let text = active
            .and_then(|i| app.tabs.get(i))
            .and_then(|tab| tab.assistant.turns.get(index))
            .map(|turn| turn.text.clone());
        if let Some(text) = text {
            app.save_assistant_answer(&text, kind);
        }
    }
}

/// Enqueue background LT0 cache jobs for closed math in finished assistant turns.
fn prepare_assistant_answer_math(app: &mut MarkerApp, color: Rgb) {
    let jobs: Vec<(String, bool)> = {
        let Some(tab) = app.tab() else {
            return;
        };
        tab.assistant
            .turns
            .iter()
            .filter(|turn| {
                matches!(turn.role, AssistantRole::Assistant)
                    && !turn.incomplete
                    && !turn.text.is_empty()
            })
            .flat_map(|turn| {
                math_spans::closed_math_spans(&turn.text)
                    .into_iter()
                    .filter_map(|span| {
                        let inner = &turn.text[span.inner_start..span.inner_end];
                        if inner.trim().is_empty() {
                            return None;
                        }
                        Some((
                            math_spans::source_for_render(inner, span.display),
                            span.display,
                        ))
                    })
            })
            .collect()
    };
    for (source, display) in jobs {
        app.request_answer_math(source, display, color);
    }
}

/// Draw a finished assistant turn with `$…$` / `$$…$$` / `\(...\)` rendered via the math cache.
///
/// Pending or unclosed spans stay as source text. Prose is plain (not Markdown) so math can sit
/// inline; turns without math still go through `show_markdown`.
fn paint_assistant_answer_math(
    ui: &mut egui::Ui,
    math_cache: &crate::math::MathCache<crate::math::InlineReady>,
    text: &str,
    color: Rgb,
) {
    let size = MarkerApp::ASSISTANT_MATH_SIZE;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
        for span in math_spans::parse_spans(text) {
            match span {
                math_spans::Span::Prose { start, end } => {
                    let prose = math_spans::display_prose(&text[start..end]);
                    for (line_i, line) in prose.split('\n').enumerate() {
                        if line_i > 0 {
                            // Force a wrap to the next row (egui has no end_row here).
                            let rem = ui.available_size_before_wrap().x;
                            if rem > 0.0 {
                                ui.add_space(rem);
                            }
                        }
                        if !line.is_empty() {
                            ui.label(line);
                        }
                    }
                }
                math_spans::Span::Math {
                    start,
                    end,
                    inner_start,
                    inner_end,
                    display,
                    closed,
                } => {
                    let source = &text[start..end];
                    if !closed {
                        ui.label(source);
                        continue;
                    }
                    let inner = &text[inner_start..inner_end];
                    if display {
                        let rem = ui.available_size_before_wrap().x;
                        if rem > 0.0 {
                            ui.add_space(rem);
                        }
                    }
                    let key = crate::math::MathKey::new(inner, display, size, color);
                    let ready = match math_cache.get(&key) {
                        Some(crate::math::EntryKind::Ready { value, .. }) => value
                            .texture
                            .as_ref()
                            .map(|texture| (texture.id(), value.w_pt, value.h_pt)),
                        Some(
                            crate::math::EntryKind::Pending { .. }
                            | crate::math::EntryKind::Error { .. },
                        )
                        | None => None,
                    };
                    if let Some((texture_id, w_pt, h_pt)) = ready {
                        let w = w_pt.max(1.0);
                        let h = h_pt.max(1.0);
                        ui.image((texture_id, Vec2::new(w, h)));
                    } else {
                        // Pending / error: keep the raw delimiters visible.
                        ui.label(source);
                    }
                    if display {
                        let rem = ui.available_size_before_wrap().x;
                        if rem > 0.0 {
                            ui.add_space(rem);
                        }
                    }
                }
            }
        }
    });
}
