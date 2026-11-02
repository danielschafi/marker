//! Learning assistant, capture, explain, and crop (RF9b).

use crate::assistant::{
    absolute_filepath, AssistantAttachment, AssistantEvent, AssistantRequest, AssistantRole,
    AssistantTurn, BundleImageAttach, BundleInput, BundleTextAttach, CaptureMode, PendingCrop,
    CROP_DPI, MAX_TEXT_CHARS,
};
use crate::geom::PdfRect;
use crate::pdf::DocId;
use crate::view;

use super::*;

impl MarkerApp {
    pub(crate) fn toggle_assistant(&mut self) {
        self.set_assistant_open(!self.assistant_open);
    }

    /// Show or hide the assistant panel without ending the Cursor session (#27).
    ///
    /// Hiding clears ephemeral capture UI only. `chat_id`, transcript, and any
    /// in-flight turn keep running; reopening reconnects to the same state.
    pub(crate) fn set_assistant_open(&mut self, open: bool) {
        self.assistant_open = open;
        if !open {
            self.capture = CaptureMode::None;
        }
    }

    pub(crate) fn assistant_new_chat(&mut self) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        if tab.assistant.streaming {
            let doc_id = tab.doc.doc_id;
            let seq = tab.assistant.request_seq;
            self.assistant.cancel(doc_id, seq);
        }
        if let Some(tab) = self.tab_mut() {
            tab.pending_study = None;
            tab.assistant.new_chat();
        }
    }

    pub(crate) fn assistant_stop(&mut self) {
        let Some(tab) = self.tab() else {
            return;
        };
        if !tab.assistant.streaming {
            return;
        }
        let doc_id = tab.doc.doc_id;
        let seq = tab.assistant.request_seq;
        self.assistant.cancel(doc_id, seq);
    }

    /// Insert a finished assistant turn as a Text or Note annotation (ST6).
    ///
    /// No-op while streaming, or when `text` is empty. Does not modify the turn.
    /// Inline `$…$` math stays as source text; the normal dirty/save path applies.
    pub(crate) fn save_assistant_answer(&mut self, text: &str, kind: CreateKind) {
        if text.is_empty() || !matches!(kind, CreateKind::Text | CreateKind::Note) {
            return;
        }
        if self.tab().is_some_and(|tab| tab.assistant.streaming) {
            return;
        }
        let view = self.view_rect;
        let settings = self.settings.clone();
        self.seal_then_arm();
        let id = {
            let Some(tab) = self.tab_mut() else {
                return;
            };
            view::place_assistant_answer(tab, view, kind, &settings, text.to_string())
        };
        if let Some(id) = id {
            self.tag_undo_edit(id);
            self.open_style_bar(id, true);
        }
    }

    pub(crate) fn begin_learning_select(&mut self) {
        self.set_assistant_open(true);
        self.capture = CaptureMode::LearningText;
        if let Some(tab) = self.tab_mut() {
            tab.drag = None;
        }
    }

    pub(crate) fn begin_region_capture(&mut self) {
        self.set_assistant_open(true);
        self.capture = CaptureMode::Region;
        if let Some(tab) = self.tab_mut() {
            tab.drag = None;
        }
    }

    pub(crate) fn cancel_capture(&mut self) {
        self.capture = CaptureMode::None;
        if let Some(tab) = self.tab_mut() {
            if matches!(
                tab.drag,
                Some(Drag::Region { .. } | Drag::LearningSelect { .. })
            ) {
                tab.drag = None;
            }
        }
    }

    pub(crate) fn attach_learning_text(&mut self) {
        self.set_assistant_open(true);
        let Some(tab) = self.tab_mut() else {
            return;
        };
        let Some(sel) = tab.assistant.learning.clone() else {
            tab.assistant.error = Some("Select text for the assistant first.".into());
            self.capture = CaptureMode::LearningText;
            return;
        };
        let Some(glyphs) = tab.doc.glyphs.get(&sel.page).cloned() else {
            tab.assistant.error = Some("Text is still loading for that page.".into());
            return;
        };
        let raw = crate::assistant::reconstruct_text(&glyphs, sel.glyph_lo, sel.glyph_hi);
        if raw.trim().is_empty() {
            tab.assistant.error =
                Some("No text in that selection. Try a screenshot attachment instead.".into());
            return;
        }
        let (text, truncated) = crate::assistant::truncate_text(&raw, MAX_TEXT_CHARS);
        tab.assistant
            .attachments
            .retain(|a| !matches!(a, AssistantAttachment::Text { .. }));
        tab.assistant.attachments.push(AssistantAttachment::Text {
            page: sel.page,
            text,
            truncated,
        });
        tab.assistant.error = None;
    }

    pub(crate) fn attach_learning_screenshot(&mut self) {
        self.set_assistant_open(true);
        let Some(tab) = self.tab() else {
            return;
        };
        let Some(sel) = tab.assistant.learning.clone() else {
            if let Some(tab) = self.tab_mut() {
                tab.assistant.error = Some("Select text for the assistant first.".into());
            }
            self.capture = CaptureMode::LearningText;
            return;
        };
        let Some(glyphs) = tab.doc.glyphs.get(&sel.page) else {
            if let Some(tab) = self.tab_mut() {
                tab.assistant.error = Some("Text is still loading for that page.".into());
            }
            return;
        };
        let quads = crate::annot::highlight_quads(glyphs, sel.glyph_lo, sel.glyph_hi);
        let Some(bounds) = union_rects(&quads) else {
            return;
        };
        let rect = bounds.inflate(4.0);
        self.request_crop(sel.page, rect);
    }

    pub(crate) fn explain_selection(&mut self) {
        self.set_assistant_open(true);
        self.attach_learning_text();
        if let Some(tab) = self.tab_mut() {
            if tab.assistant.draft.trim().is_empty() {
                tab.assistant.draft = "Explain this selection clearly for a learner.".into();
            }
        }
    }

    /// Pre-fill a study prompt and attach the selection, or the current page text.
    ///
    /// Does not enable agent mode and does not auto-send. If page glyphs are not
    /// loaded yet, requests them once and shows "Reading this page…" until they
    /// arrive. An empty text layer updates the status line only.
    pub(crate) fn study_prompt(&mut self, prompt: &str) {
        self.set_assistant_open(true);
        if let Some(tab) = self.tab_mut() {
            tab.pending_study = None;
        }
        if let Some((page, raw)) = self.study_selection_text() {
            self.apply_study_prompt(prompt, page, &raw);
            return;
        }
        let view_h = self.view_rect.height().max(1.0);
        let (doc_id, page, loaded) = {
            let Some(tab) = self.tab() else {
                return;
            };
            let page = tab.doc.current_page(view_h);
            let loaded = tab.doc.glyphs.get(&page).map(|glyphs| {
                if glyphs.is_empty() {
                    String::new()
                } else {
                    crate::assistant::reconstruct_text(glyphs, 0, glyphs.len() - 1)
                }
            });
            (tab.doc.doc_id, page, loaded)
        };
        if let Some(raw) = loaded {
            self.apply_study_prompt(prompt, page, &raw);
            return;
        }
        let Some(tab) = self.tab_mut() else {
            return;
        };
        let need_request = tab.glyphs_waiting.insert(page);
        tab.pending_study = Some((page, prompt.to_string()));
        tab.assistant.status_line = Some("Reading this page…".into());
        tab.assistant.error = None;
        if need_request {
            self.worker.glyphs(doc_id, page);
        }
    }

    /// Selection text for study prompts: learning attach first, then Select-tool range.
    pub(crate) fn study_selection_text(&self) -> Option<(usize, String)> {
        let tab = self.tab()?;
        if let Some(sel) = &tab.assistant.learning {
            let glyphs = tab.doc.glyphs.get(&sel.page)?;
            let text = crate::assistant::reconstruct_text(glyphs, sel.glyph_lo, sel.glyph_hi);
            if !text.trim().is_empty() {
                return Some((sel.page, text));
            }
        }
        if let Some(sel) = &tab.text_sel {
            let glyphs = tab.doc.glyphs.get(&sel.page)?;
            let text = crate::assistant::reconstruct_text(glyphs, sel.glyph_lo, sel.glyph_hi);
            if !text.trim().is_empty() {
                return Some((sel.page, text));
            }
        }
        None
    }

    pub(crate) fn apply_study_prompt(&mut self, prompt: &str, page: usize, raw: &str) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        tab.pending_study = None;
        if raw.trim().is_empty() {
            tab.assistant.status_line = Some("This page has no text layer.".into());
            tab.assistant.error = None;
            return;
        }
        let (text, truncated) = crate::assistant::truncate_text(raw, MAX_TEXT_CHARS);
        tab.assistant
            .attachments
            .retain(|a| !matches!(a, AssistantAttachment::Text { .. }));
        tab.assistant.attachments.push(AssistantAttachment::Text {
            page,
            text,
            truncated,
        });
        tab.assistant.draft = prompt.to_string();
        tab.assistant.error = None;
        tab.assistant.status_line = None;
    }

    pub(crate) fn finish_pending_study(&mut self, doc_id: DocId, page: usize) {
        let Some(tab) = self.tab_by_doc_id_mut(doc_id) else {
            return;
        };
        let Some((pending_page, prompt)) = tab.pending_study.take() else {
            return;
        };
        if pending_page != page {
            tab.pending_study = Some((pending_page, prompt));
            return;
        }
        let raw = match tab.doc.glyphs.get(&page) {
            Some(glyphs) if !glyphs.is_empty() => {
                crate::assistant::reconstruct_text(glyphs, 0, glyphs.len() - 1)
            }
            _ => String::new(),
        };
        if raw.trim().is_empty() {
            tab.assistant.status_line = Some("This page has no text layer.".into());
            tab.assistant.error = None;
            return;
        }
        let (text, truncated) = crate::assistant::truncate_text(&raw, MAX_TEXT_CHARS);
        tab.assistant
            .attachments
            .retain(|a| !matches!(a, AssistantAttachment::Text { .. }));
        tab.assistant.attachments.push(AssistantAttachment::Text {
            page,
            text,
            truncated,
        });
        tab.assistant.draft = prompt;
        tab.assistant.error = None;
        tab.assistant.status_line = None;
    }

    pub(crate) fn request_crop(&mut self, page: usize, rect: PdfRect) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        let Some(info) = tab.doc.pages.get(page).copied() else {
            return;
        };
        let rect = crate::assistant::clamp_crop_rect(rect, info);
        if rect.is_empty() {
            tab.assistant.error = Some("Crop region is empty.".into());
            return;
        }
        let seq = tab.assistant.bump_crop_seq();
        let doc_id = tab.doc.doc_id;
        tab.assistant.pending_crop = Some(PendingCrop { seq });
        tab.assistant.status_line = Some("Rendering screenshot…".into());
        self.worker.crop(doc_id, seq, page, rect, CROP_DPI);
    }

    pub(crate) fn assistant_send(&mut self) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        if tab.assistant.streaming {
            return;
        }
        let question = tab.assistant.draft.trim().to_string();
        if question.is_empty() && tab.assistant.attachments.is_empty() {
            tab.assistant.error = Some("Enter a question or attach context.".into());
            return;
        }
        if !tab.assistant.disclosed {
            tab.assistant.error =
                Some("Confirm the Cursor disclosure below before the first send.".into());
            return;
        }
        let question = if question.is_empty() {
            "Please explain the attached material.".into()
        } else {
            question
        };
        let filename = tab
            .doc
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string);
        let filepath = Some(absolute_filepath(&tab.doc.path));
        let agent_mode = tab.assistant.agent_mode;
        let mut text = None;
        let mut images = Vec::new();
        for attach in &tab.assistant.attachments {
            match attach {
                AssistantAttachment::Text {
                    page,
                    text: body,
                    truncated,
                } => {
                    text = Some(BundleTextAttach {
                        page: *page,
                        text: body.clone(),
                        truncated: *truncated,
                    });
                }
                AssistantAttachment::Image {
                    page,
                    rect,
                    png,
                    width,
                    height,
                    ..
                } => {
                    images.push(BundleImageAttach {
                        page: *page,
                        rect: *rect,
                        png: png.clone(),
                        width: *width,
                        height: *height,
                        filename: format!("crop-{page}.png"),
                    });
                }
            }
        }
        let doc_id = tab.doc.doc_id;
        let seq = tab.assistant.bump_seq();
        // Reuse create-chat id across panel hide / tab background / between turns.
        let chat_id = tab.assistant.resume_chat_id();
        tab.assistant.streaming = true;
        tab.assistant.error = None;
        tab.assistant.status_line = Some(if agent_mode {
            "Talking to Cursor (agent mode)…".into()
        } else {
            "Talking to Cursor…".into()
        });
        tab.assistant.turns.push(AssistantTurn {
            role: AssistantRole::User,
            text: question.clone(),
            incomplete: false,
        });
        tab.assistant.turns.push(AssistantTurn {
            role: AssistantRole::Assistant,
            text: String::new(),
            incomplete: true,
        });
        tab.assistant.draft.clear();
        tab.assistant.attachments.clear();

        self.assistant.send(AssistantRequest {
            doc_id,
            seq,
            chat_id,
            agent_mode,
            bundle: BundleInput {
                question,
                filename,
                filepath,
                text,
                images,
            },
        });
    }

    pub(crate) fn lookup_selection_in_browser(&mut self) {
        let Some(tab) = self.tab() else {
            return;
        };
        let query = if let Some(sel) = &tab.assistant.learning {
            tab.doc
                .glyphs
                .get(&sel.page)
                .map(|glyphs| {
                    crate::assistant::reconstruct_text(glyphs, sel.glyph_lo, sel.glyph_hi)
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        let query = query.trim().to_string();
        if query.is_empty() {
            if let Some(tab) = self.tab_mut() {
                tab.assistant.error =
                    Some("Select text before looking it up in the browser.".into());
            }
            return;
        }
        let encoded = urlencoding_minimal(&query);
        let url = format!("https://duckduckgo.com/?q={encoded}");
        let _ = open_browser(&url);
    }

    pub(crate) fn on_crop(
        &mut self,
        ctx: &egui::Context,
        doc_id: DocId,
        seq: u64,
        result: Result<crate::pdf::CropImage, String>,
    ) {
        let Some(tab) = self.tab_by_doc_id_mut(doc_id) else {
            return;
        };
        let pending = tab.assistant.pending_crop.take();
        if pending.as_ref().is_none_or(|p| p.seq != seq) {
            return;
        }
        tab.assistant.status_line = None;
        match result {
            Ok(crop) => {
                let texture =
                    upload_png_texture(ctx, doc_id, seq, &crop.png, crop.width, crop.height);
                tab.assistant.attachments.push(AssistantAttachment::Image {
                    page: crop.page,
                    rect: crop.rect,
                    png: crop.png,
                    width: crop.width,
                    height: crop.height,
                    texture,
                });
                tab.assistant.error = None;
            }
            Err(message) => {
                tab.assistant.error = Some(message);
            }
        }
    }

    pub(crate) fn on_assistant(&mut self, _ctx: &egui::Context, event: AssistantEvent) {
        let (doc_id, seq) = match &event {
            AssistantEvent::Started { doc_id, seq, .. }
            | AssistantEvent::Delta { doc_id, seq, .. }
            | AssistantEvent::Completed { doc_id, seq, .. }
            | AssistantEvent::Cancelled { doc_id, seq }
            | AssistantEvent::Failed { doc_id, seq, .. }
            | AssistantEvent::AuthRequired { doc_id, seq, .. } => (*doc_id, *seq),
        };
        let mut parse_cards_reply: Option<(usize, String)> = None;
        {
            let Some(tab) = self.tab_by_doc_id_mut(doc_id) else {
                return;
            };
            if tab.assistant.request_seq != seq {
                return;
            }
            match event {
                AssistantEvent::Started { chat_id, .. } => {
                    tab.assistant.chat_id = Some(chat_id);
                    tab.assistant.status_line = Some("Streaming…".into());
                }
                AssistantEvent::Delta { text, .. } => {
                    if let Some(turn) = tab
                        .assistant
                        .turns
                        .iter_mut()
                        .rev()
                        .find(|t| t.role == AssistantRole::Assistant)
                    {
                        turn.text.push_str(&text);
                        turn.incomplete = true;
                    }
                }
                AssistantEvent::Completed { text, .. } => {
                    let parse_page = tab.assistant.pending_cards_parse.take();
                    if let Some(turn) = tab
                        .assistant
                        .turns
                        .iter_mut()
                        .rev()
                        .find(|t| t.role == AssistantRole::Assistant)
                    {
                        if turn.text.is_empty()
                            || (!text.is_empty() && text.len() >= turn.text.len())
                        {
                            turn.text = text.clone();
                        }
                        turn.incomplete = false;
                    }
                    tab.assistant.streaming = false;
                    // Clear busy status; panel hint shows session-alive / resume (#27).
                    tab.assistant.status_line = None;
                    if let Some(page) = parse_page {
                        let final_text = tab
                            .assistant
                            .turns
                            .iter()
                            .rev()
                            .find(|t| t.role == AssistantRole::Assistant)
                            .map(|t| t.text.clone())
                            .unwrap_or(text);
                        parse_cards_reply = Some((page, final_text));
                    }
                }
                AssistantEvent::Cancelled { .. } => {
                    tab.assistant.pending_cards_parse = None;
                    if let Some(turn) = tab
                        .assistant
                        .turns
                        .iter_mut()
                        .rev()
                        .find(|t| t.role == AssistantRole::Assistant)
                    {
                        turn.incomplete = true;
                        if turn.text.is_empty() {
                            turn.text = "(stopped)".into();
                        }
                    }
                    tab.assistant.streaming = false;
                    tab.assistant.status_line = Some("Stopped.".into());
                }
                AssistantEvent::Failed { message, .. }
                | AssistantEvent::AuthRequired { message, .. } => {
                    tab.assistant.pending_cards_parse = None;
                    tab.assistant.streaming = false;
                    tab.assistant.status_line = None;
                    tab.assistant.error = Some(message);
                    if let Some(turn) = tab
                        .assistant
                        .turns
                        .iter_mut()
                        .rev()
                        .find(|t| t.role == AssistantRole::Assistant)
                    {
                        if turn.text.is_empty() {
                            tab.assistant.turns.pop();
                        } else {
                            turn.incomplete = true;
                        }
                    }
                }
            }
        }
        if let Some((page, reply)) = parse_cards_reply {
            self.finish_generated_cards(doc_id, page, &reply);
        }
    }

    /// Fixed study prompts shown above the assistant composer (ST5).
    pub(crate) const STUDY_PROMPTS: &[(&str, &str)] = &[
        (
            "Summarize this page",
            "Summarize this page clearly for a learner.",
        ),
        ("Explain simply", "Explain this simply for a learner."),
        (
            "Define terms",
            "Define the key terms in this text for a learner.",
        ),
        ("Quiz me", "Quiz me on this material."),
    ];
}

pub(crate) fn union_rects(rects: &[PdfRect]) -> Option<PdfRect> {
    let first = *rects.first()?;
    Some(rects.iter().skip(1).fold(first, |acc, r| {
        PdfRect::new(
            acc.x0.min(r.x0),
            acc.y0.min(r.y0),
            acc.x1.max(r.x1),
            acc.y1.max(r.y1),
        )
    }))
}
