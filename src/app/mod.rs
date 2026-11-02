use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use egui::ViewportCommand;

use crate::annot::{AnnotKind, Annotation};
use crate::assistant::{
    assistant_configured, AssistantAttachment, AssistantWorker, CaptureMode, GENERATE_CARDS_PROMPT,
    MAX_TEXT_CHARS,
};
use crate::geom::{PdfPoint, PdfRect, Rgb};
use crate::instance::IpcInbox;
use crate::math::MathPipeline;
use crate::pdf::{DocId, PdfReply, PdfWorker};
use crate::platform;
use crate::settings::Settings;
use crate::ui;
use crate::undo::{Direction, PageOpKind, StepOutcome, UndoEntry};
use crate::view::{self, viewport};
use crate::worker::EguiWake;
use egui_commonmark::CommonMarkCache;

/// Identity of one open tab in the strip (stable across reorders / closes).
///
/// Distinct from [`DocId`]: a tab keeps its id for its lifetime, while the
/// document id is allocated when a path is opened. Prefer a separate counter so
/// retargeting a document cannot look like a new tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TabId(pub u64);

impl std::fmt::Display for TabId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

pub(crate) struct MarkerApp {
    pub(crate) settings: Settings,
    pub(crate) tool: Tool,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: TabId,
    pub(crate) view_rect: egui::Rect,
    /// Second pane rect when a split is open.
    pub(crate) split_view_rect: egui::Rect,
    /// Side-by-side or stacked second document pane.
    pub(crate) split: Option<SplitState>,
    /// Right-click menu on a tab pill: (tab id, screen pos).
    pub(crate) tab_menu: Option<(TabId, egui::Pos2)>,
    /// Tab being dragged for split / pane assignment.
    pub(crate) tab_drag: Option<TabId>,
    /// While dragging, open-tabs row under the pointer (if any).
    pub(crate) tab_drag_over: Option<TabId>,
    /// Ephemeral or pinned open-tabs overlay (top-right).
    pub(crate) tab_list: Option<TabListState>,
    pub(crate) opening: HashSet<DocId>,
    pub(crate) error: Option<String>,
    pub(crate) page_focus: bool,
    /// Fullscreen reading; chrome auto-hides until the pointer reaches the top edge.
    pub(crate) zen: bool,
    /// Keep chrome visible briefly after the pointer leaves the top reveal strip.
    zen_chrome_until: Option<Instant>,
    /// Learning assistant panel (closed on launch).
    pub(crate) assistant_open: bool,
    /// Temporary capture mode for assistant attachments.
    pub(crate) capture: CaptureMode,
    /// Shared cache for assistant Markdown rendering (issue #26).
    pub(crate) assistant_md_cache: CommonMarkCache,
    /// `?` shortcut cheat sheet window.
    pub(crate) cheatsheet_open: bool,
    /// Named highlight preset editor (Colors chrome button).
    pub(crate) preset_editor_open: bool,
    vim_count: u32,
    vim_g: bool,
    worker: PdfWorker,
    /// Cache, deadlines, scan stamps, and Typst worker behind one interface.
    pub(crate) math: MathPipeline,
    assistant: AssistantWorker,
    next_doc_id: DocId,
    next_tab_id: TabId,
    dialog_tx: Sender<DialogReply>,
    dialog_rx: Receiver<DialogReply>,
    dialog_busy: bool,
    /// External opens forwarded from secondary Marker processes (single-instance).
    ipc: Option<IpcInbox>,
    /// Cloned into worker/dialog threads so they can wake the UI on completion.
    egui_ctx: egui::Context,
    /// Last window title sent to the viewport (skip redundant updates).
    last_title: String,
    /// When the previous frame finished, so interacting frames stay at least ~8ms apart.
    last_frame: Instant,
    /// Cached `session.is_dirty()` for the active tab's title suffix.
    title_dirty_epoch: u64,
    title_dirty: bool,
    title_tab_doc_id: DocId,
    exit_flush_done: bool,
    /// Tile replies waiting for a capped UI-thread upload batch.
    pending_pdf_tiles: VecDeque<PdfReply>,
    /// Monotonic frame id for tile LRU (incremented each `update`).
    pub(crate) tile_frame: u64,
    /// Keyboard-focused tab; unfocused tabs are trimmed to a smaller tile share.
    tile_focus_tab: TabId,
    /// Latest wanted tile keys per document id (read by the pdf worker).
    tile_wanted: Arc<Mutex<HashMap<DocId, HashSet<TileKey>>>>,
    /// Latest wanted thumbnail pages per document id (read by the pdf worker).
    thumb_wanted: Arc<Mutex<HashMap<DocId, HashSet<usize>>>>,
    /// Generated Anki CSV waiting for the cards save dialog (ST3).
    pending_generated_csv: Option<(DocId, String)>,
}

/// Two-pane document layout. `first` is left/top; `second` is right/bottom.
/// Focus is `MarkerApp::active`, which must be one of the two pane tabs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SplitState {
    pub first: TabId,
    pub second: TabId,
    pub stacked: bool,
    pub ratio: f32,
}

impl SplitState {
    pub(crate) fn contains(self, id: TabId) -> bool {
        self.first == id || self.second == id
    }

    pub(crate) fn other(self, active: TabId) -> TabId {
        if active == self.first {
            self.second
        } else {
            self.first
        }
    }
}

/// Open-tabs overlay: brief flash on switch, or pinned until dismissed.
#[derive(Clone, Copy)]
pub(crate) enum TabListState {
    Ephemeral { until: Instant },
    Pinned,
}

/// Where a dragged tab would land relative to the viewport.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SplitDropZone {
    Left,
    Right,
    Top,
    Bottom,
    /// Replace the document in the unfocused pane.
    OtherPane,
    /// Focus / place the dragged tab in the focused pane.
    ActivePane,
    /// Pair with this tab (drop on a row in the open-tabs list).
    Tab(TabId),
}

mod assistant;
mod input;
mod math_jobs;
mod replies;
mod save;
mod state;
mod tabs;
pub(crate) use input::{ShortcutSection, SHORTCUTS};
pub(crate) use math_jobs::upload_png_texture;
pub(crate) use replies::trim_tab_tiles;
pub(crate) use state::*;

/// Pages that still need glyphs before Markdown export can run.
fn export_missing_glyph_pages(tab: &Tab) -> Vec<usize> {
    let mut missing = Vec::new();
    for annot in &tab.doc.session.annotations {
        if !matches!(
            annot.kind,
            AnnotKind::Highlight { .. } | AnnotKind::Markup { .. }
        ) {
            continue;
        }
        if tab.doc.glyphs.contains_key(&annot.page) {
            continue;
        }
        if !missing.contains(&annot.page) {
            missing.push(annot.page);
        }
    }
    missing
}

/// Native highlights and FT5 markup only — skip Foreign.
fn collect_export_highlights(tab: &Tab) -> Vec<crate::export::ExportHighlight> {
    use crate::export::{note_is_nearby, ExportHighlight};

    let notes: Vec<(usize, PdfRect, &str)> = tab
        .doc
        .session
        .annotations
        .iter()
        .filter_map(|annot| match &annot.kind {
            AnnotKind::Note { rect, content, .. } => Some((annot.page, *rect, content.as_str())),
            _ => None,
        })
        .collect();

    let mut out = Vec::new();
    for annot in &tab.doc.session.annotations {
        let (quads, color) = match &annot.kind {
            AnnotKind::Highlight { quads, color } => (quads, *color),
            AnnotKind::Markup { quads, color, .. } => (quads, *color),
            _ => continue,
        };
        let bounds = annot.bounds();
        let mut best: Option<(f32, &str)> = None;
        if let Some(bounds) = bounds {
            for (page, note_rect, content) in &notes {
                if *page != annot.page || content.trim().is_empty() {
                    continue;
                }
                if !note_is_nearby(bounds, *note_rect) {
                    continue;
                }
                let c = bounds.center();
                let n = note_rect.center();
                let dist = (c.x - n.x).hypot(c.y - n.y);
                if best.is_none_or(|(d, _)| dist < d) {
                    best = Some((dist, *content));
                }
            }
        }
        out.push(ExportHighlight {
            page: annot.page,
            quads: quads.clone(),
            color,
            note: best.map(|(_, text)| text.to_owned()),
        });
    }
    out
}

fn pinned_glyph_pages(tab: &Tab) -> Vec<usize> {
    let mut pinned = Vec::new();
    if let Some(sel) = &tab.text_sel {
        pinned.push(sel.page);
    }
    let drag_page = match &tab.drag {
        Some(Drag::Highlight { page, .. })
        | Some(Drag::TextSelect { page, .. })
        | Some(Drag::LearningSelect { page, .. }) => Some(*page),
        _ => None,
    };
    if let Some(page) = drag_page {
        if !pinned.contains(&page) {
            pinned.push(page);
        }
    }
    if tab.pending_export.is_some() {
        for annot in &tab.doc.session.annotations {
            if matches!(
                annot.kind,
                AnnotKind::Highlight { .. } | AnnotKind::Markup { .. }
            ) && !pinned.contains(&annot.page)
            {
                pinned.push(annot.page);
            }
        }
    }
    pinned
}

/// Result of an `rfd` file dialog spawned off the UI thread.
enum DialogReply {
    /// One or more PDFs chosen in the open dialog (`pick_files`); `None` if cancelled.
    Open(Option<Vec<PathBuf>>),
    SaveAs {
        doc_id: DocId,
        path: Option<PathBuf>,
    },
    ExportMarkdown {
        doc_id: DocId,
        path: Option<PathBuf>,
    },
    ExportCards {
        doc_id: DocId,
        path: Option<PathBuf>,
    },
}

impl MarkerApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        paths: Vec<PathBuf>,
        ipc: Option<IpcInbox>,
    ) -> Self {
        ui::apply_theme(&cc.egui_ctx);
        let (dialog_tx, dialog_rx) = mpsc::channel();
        let egui_ctx = cc.egui_ctx.clone();
        let wake = EguiWake::arc(egui_ctx.clone());
        if let Some(inbox) = ipc.as_ref() {
            inbox.bind_wake(Arc::clone(&wake));
        }
        let tile_wanted = Arc::new(Mutex::new(HashMap::new()));
        let thumb_wanted = Arc::new(Mutex::new(HashMap::new()));
        let mut app = Self {
            settings: Settings::load(),
            tool: Tool::Select,
            tabs: Vec::new(),
            active: TabId(0),
            view_rect: egui::Rect::NOTHING,
            split_view_rect: egui::Rect::NOTHING,
            split: None,
            tab_menu: None,
            tab_drag: None,
            tab_drag_over: None,
            tab_list: None,
            opening: HashSet::new(),
            error: None,
            page_focus: false,
            zen: false,
            zen_chrome_until: None,
            assistant_open: false,
            capture: CaptureMode::None,
            assistant_md_cache: CommonMarkCache::default(),
            cheatsheet_open: false,
            preset_editor_open: false,
            vim_count: 0,
            vim_g: false,
            worker: PdfWorker::spawn(
                Arc::clone(&wake),
                Arc::clone(&tile_wanted),
                Arc::clone(&thumb_wanted),
            ),
            math: MathPipeline::spawn(Arc::clone(&wake)),
            assistant: AssistantWorker::spawn(wake),
            next_doc_id: DocId(1),
            next_tab_id: TabId(1),
            dialog_tx,
            dialog_rx,
            dialog_busy: false,
            ipc,
            egui_ctx,
            last_title: String::new(),
            last_frame: Instant::now(),
            title_dirty_epoch: 0,
            title_dirty: false,
            title_tab_doc_id: DocId(0),
            exit_flush_done: false,
            pending_pdf_tiles: VecDeque::new(),
            tile_frame: 0,
            tile_focus_tab: TabId(0),
            tile_wanted,
            thumb_wanted,
            pending_generated_csv: None,
        };
        for path in paths {
            app.open_path(path);
        }
        app
    }

    pub(crate) fn tab(&self) -> Option<&Tab> {
        self.tab_by_id(self.active)
    }

    pub(crate) fn tab_mut(&mut self) -> Option<&mut Tab> {
        self.tab_by_id_mut(self.active)
    }

    /// Strip position for a live [`TabId`], if any.
    pub(crate) fn tab_index(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    pub(crate) fn tab_by_id(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub(crate) fn tab_by_id_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    fn alloc_tab_id(&mut self) -> TabId {
        let id = self.next_tab_id;
        self.next_tab_id = TabId(self.next_tab_id.0.saturating_add(1));
        id
    }

    /// Neighbor in strip order (`delta` = ±1), wrapping.
    fn adjacent_tab_id(&self, id: TabId, delta: isize) -> Option<TabId> {
        let index = self.tab_index(id)?;
        let len = self.tabs.len();
        if len == 0 {
            return None;
        }
        let next = (index as isize + delta).rem_euclid(len as isize) as usize;
        Some(self.tabs[next].id)
    }

    /// Cache and the active tab together, so the conceal layouter can read
    /// renders while the text buffer is borrowed.
    pub(crate) fn math_cache_and_tab_mut(
        &mut self,
    ) -> Option<(&crate::math::MathCache<crate::math::InlineReady>, &mut Tab)> {
        let index = self.tab_index(self.active)?;
        Some((&self.math.cache, &mut self.tabs[index]))
    }

    pub(crate) fn doc(&self) -> Option<&DocState> {
        self.tab().map(|tab| &tab.doc)
    }

    pub(crate) fn doc_mut(&mut self) -> Option<&mut DocState> {
        self.tab_mut().map(|tab| &mut tab.doc)
    }

    fn tab_by_doc_id(&self, doc_id: DocId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.doc.doc_id == doc_id)
    }

    fn tab_by_doc_id_mut(&mut self, doc_id: DocId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.doc.doc_id == doc_id)
    }

    pub(crate) fn queue_math(&mut self, id: u64) {
        let Some(doc_id) = self.tab().map(|tab| tab.doc.doc_id) else {
            return;
        };
        self.arm_math_deadline(doc_id, id, None);
    }

    pub(crate) fn queue_inline_math(&mut self, id: u64, span_key: u64) {
        let Some(doc_id) = self.tab().map(|tab| tab.doc.doc_id) else {
            return;
        };
        self.arm_math_deadline(doc_id, id, Some(span_key));
    }

    /// First request for a key starts the timer. Later calls in the same burst
    /// do not push it back, and they do not erase a different key's timer.
    fn arm_math_deadline(&mut self, doc_id: DocId, id: u64, span_key: Option<u64>) {
        self.math.arm_deadline(doc_id, id, span_key);
    }

    pub(crate) fn queue_jump(&mut self, page: usize, y: Option<f32>) {
        if let Some(tab) = self.tab_mut() {
            tab.pending_jump = Some((page, y));
        }
    }

    /// Ask the worker for visible (+ lookahead) thumbnail pages. Updates the
    /// cancel set so scrolled-away pages become `ThumbMiss` instead of work.
    pub(crate) fn request_thumbs(&mut self, pages: &[usize]) {
        let (doc_id, page_count, open) = match self.tab() {
            Some(tab) => (tab.doc.doc_id, tab.doc.pages.len(), tab.thumbs_open),
            None => return,
        };
        if !open {
            self.thumb_wanted
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&doc_id);
            return;
        }
        let mut wanted: HashSet<usize> = HashSet::new();
        for &page in pages {
            if page < page_count {
                wanted.insert(page);
            }
        }
        {
            let mut guard = self
                .thumb_wanted
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            guard.insert(doc_id, wanted.clone());
        }
        let mut to_queue = Vec::new();
        if let Some(tab) = self.tab_mut() {
            for &page in &wanted {
                if tab.thumbs.contains_key(&page) {
                    tab.touch_thumb(page);
                    continue;
                }
                if !tab.thumbs_inflight.contains(&page) {
                    to_queue.push(page);
                }
            }
        }
        // Cap new jobs per frame so a long scroll does not flood the tile thread.
        to_queue.sort_unstable();
        for page in to_queue.into_iter().take(8) {
            if let Some(tab) = self.tab_mut() {
                tab.thumbs_inflight.insert(page);
            }
            self.worker.thumb(doc_id, page);
        }
    }

    pub(crate) fn open_dialog(&mut self) {
        if self.dialog_busy {
            return;
        }
        self.dialog_busy = true;
        let tx = self.dialog_tx.clone();
        let ctx = self.egui_ctx.clone();
        thread::spawn(move || {
            let paths = rfd::FileDialog::new()
                .add_filter("PDF", &["pdf"])
                .pick_files();
            let _ = tx.send(DialogReply::Open(paths));
            ctx.request_repaint();
        });
    }

    /// Save As: pick a path, write a full copy, then switch this tab onto the copy.
    pub(crate) fn save_as_dialog(&mut self) {
        if self.dialog_busy {
            return;
        }
        let Some(tab) = self.tab() else {
            return;
        };
        if matches!(tab.save, SaveState::Saving) {
            return;
        }
        let doc_id = tab.doc.doc_id;
        let suggested = tab
            .doc
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("document.pdf")
            .to_owned();
        self.dialog_busy = true;
        let tx = self.dialog_tx.clone();
        let ctx = self.egui_ctx.clone();
        thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .add_filter("PDF", &["pdf"])
                .set_file_name(&suggested)
                .save_file();
            let _ = tx.send(DialogReply::SaveAs { doc_id, path });
            ctx.request_repaint();
        });
    }

    /// Export native highlights/markup to Markdown (ST1).
    ///
    /// If glyph pages are missing, sets `pending_export` and enqueues jobs;
    /// `check_pending_export` / glyph replies finish without freezing the UI.
    pub(crate) fn export_markdown(&mut self) {
        self.begin_highlight_export(PendingExport::Markdown);
    }

    /// Export native highlights/markup as Anki-importable CSV (ST3).
    pub(crate) fn export_cards(&mut self) {
        self.pending_generated_csv = None;
        self.begin_highlight_export(PendingExport::Cards);
    }

    /// Whether the optional "Generate cards" control should show (CLI on PATH).
    pub(crate) fn assistant_is_configured(&self) -> bool {
        assistant_configured()
    }

    fn begin_highlight_export(&mut self, kind: PendingExport) {
        let (doc_id, missing) = {
            let Some(tab) = self.tab_mut() else {
                return;
            };
            tab.pending_export = None;
            let missing = export_missing_glyph_pages(tab);
            (tab.doc.doc_id, missing)
        };
        if missing.is_empty() {
            self.open_export_dialog_for(doc_id, kind);
            return;
        }
        let mut to_request = Vec::new();
        if let Some(tab) = self.tab_mut() {
            tab.pending_export = Some(kind);
            for page in missing {
                if tab.glyphs_waiting.insert(page) {
                    to_request.push(page);
                }
            }
        }
        for page in to_request {
            self.worker.glyphs(doc_id, page);
        }
        self.egui_ctx.request_repaint();
    }

    fn check_pending_export(&mut self) {
        let pending: Vec<(DocId, PendingExport)> = self
            .tabs
            .iter()
            .filter_map(|tab| tab.pending_export.map(|kind| (tab.doc.doc_id, kind)))
            .collect();
        for (doc_id, _) in pending {
            self.try_finish_pending_export(doc_id);
        }
    }

    fn try_finish_pending_export(&mut self, doc_id: DocId) {
        let kind = {
            let Some(tab) = self.tab_by_doc_id_mut(doc_id) else {
                return;
            };
            let Some(kind) = tab.pending_export else {
                return;
            };
            if !export_missing_glyph_pages(tab).is_empty() {
                return;
            }
            tab.pending_export = None;
            kind
        };
        self.open_export_dialog_for(doc_id, kind);
    }

    fn open_export_dialog_for(&mut self, doc_id: DocId, kind: PendingExport) {
        if self.dialog_busy {
            return;
        }
        let Some(tab) = self.tab_by_doc_id(doc_id) else {
            return;
        };
        let (suggested, filter_name, filter_ext, reply_kind) = match kind {
            PendingExport::Markdown => (
                crate::export::default_export_name(&tab.doc.path),
                "Markdown",
                "md",
                PendingExport::Markdown,
            ),
            PendingExport::Cards => (
                crate::export::default_cards_name(&tab.doc.path),
                "CSV",
                "csv",
                PendingExport::Cards,
            ),
        };
        let start_dir = tab.doc.path.parent().map(|p| p.to_path_buf());
        self.dialog_busy = true;
        let tx = self.dialog_tx.clone();
        let ctx = self.egui_ctx.clone();
        thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new()
                .add_filter(filter_name, &[filter_ext])
                .set_file_name(&suggested);
            if let Some(dir) = start_dir {
                dialog = dialog.set_directory(dir);
            }
            let path = dialog.save_file();
            let reply = match reply_kind {
                PendingExport::Markdown => DialogReply::ExportMarkdown { doc_id, path },
                PendingExport::Cards => DialogReply::ExportCards { doc_id, path },
            };
            let _ = tx.send(reply);
            ctx.request_repaint();
        });
    }

    fn write_export_markdown(&mut self, doc_id: DocId, path: PathBuf) {
        let Some(tab) = self.tab_by_doc_id(doc_id) else {
            return;
        };
        let doc_name = tab
            .doc
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("document.pdf")
            .to_owned();
        let highlights = collect_export_highlights(tab);
        let md = crate::export::highlights_to_markdown(
            &doc_name,
            &highlights,
            &tab.doc.glyphs,
            &self.settings.highlight_presets,
        );
        match std::fs::write(&path, md) {
            Ok(()) => self.error = None,
            Err(err) => {
                self.error = Some(format!("Could not write {}: {err}", path.display()));
            }
        }
    }

    fn write_export_cards(&mut self, doc_id: DocId, path: PathBuf) {
        let csv = if let Some((pending_doc_id, body)) = self.pending_generated_csv.take() {
            if pending_doc_id == doc_id {
                body
            } else {
                self.pending_generated_csv = Some((pending_doc_id, body));
                match self.cards_csv_for_tab(doc_id) {
                    Some(csv) => csv,
                    None => return,
                }
            }
        } else {
            match self.cards_csv_for_tab(doc_id) {
                Some(csv) => csv,
                None => return,
            }
        };
        match std::fs::write(&path, csv) {
            Ok(()) => self.error = None,
            Err(err) => {
                self.error = Some(format!("Could not write {}: {err}", path.display()));
            }
        }
    }

    fn cards_csv_for_tab(&self, doc_id: DocId) -> Option<String> {
        let tab = self.tab_by_doc_id(doc_id)?;
        let doc_name = tab
            .doc
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("document.pdf");
        let highlights = collect_export_highlights(tab);
        Some(crate::export::highlights_to_csv(
            doc_name,
            &highlights,
            &tab.doc.glyphs,
            &self.settings.highlight_presets,
        ))
    }

    /// Ask the assistant for Q/A cards from the current selection (ST3).
    ///
    /// Uses the existing bundle + fixed prompt. Does not force agent mode on.
    /// On completion, parses a fenced TSV; failure leaves the raw reply visible
    /// and skips writing a file.
    pub(crate) fn generate_cards_from_selection(&mut self) {
        if !assistant_configured() {
            return;
        }
        self.set_assistant_open(true);
        if let Some(tab) = self.tab_mut() {
            tab.pending_study = None;
            tab.assistant.pending_cards_parse = None;
        }
        if let Some((page, raw)) = self.study_selection_text() {
            self.apply_generate_cards(page, &raw);
            return;
        }
        if let Some(tab) = self.tab_mut() {
            tab.assistant.error = Some("Select text before generating cards.".into());
        }
    }

    fn apply_generate_cards(&mut self, page: usize, raw: &str) {
        {
            let Some(tab) = self.tab_mut() else {
                return;
            };
            if raw.trim().is_empty() {
                tab.assistant.error = Some("Selection has no text.".into());
                return;
            }
            if tab.assistant.streaming {
                tab.assistant.error = Some("Wait for the current reply to finish.".into());
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
            tab.assistant.draft = GENERATE_CARDS_PROMPT.to_string();
            tab.assistant.error = None;
            tab.assistant.status_line = None;
            // Do not force agent mode on — leave the user's checkbox as-is.
            tab.assistant.pending_cards_parse = Some(page);
        }
        self.assistant_send();
        // Disclosure / empty-send failures leave streaming false — drop the parse flag.
        if let Some(tab) = self.tab_mut() {
            if !tab.assistant.streaming {
                tab.assistant.pending_cards_parse = None;
            }
        }
    }

    fn finish_generated_cards(&mut self, doc_id: DocId, page: usize, reply: &str) {
        match crate::export::parse_cards_tsv_fence(reply) {
            Ok(cards) => {
                let n = cards.len();
                let doc_name = {
                    let Some(tab) = self.tab_by_doc_id(doc_id) else {
                        return;
                    };
                    tab.doc
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("document.pdf")
                        .to_owned()
                };
                let source = format!("{doc_name}#page={}", page + 1);
                let csv = crate::export::generated_cards_to_csv(&cards, "generated", &source);
                self.pending_generated_csv = Some((doc_id, csv));
                if let Some(tab) = self.tab_by_doc_id_mut(doc_id) {
                    tab.assistant.status_line =
                        Some(format!("Parsed {n} card(s) — choose a save location."));
                    tab.assistant.error = None;
                }
                self.open_export_dialog_for(doc_id, PendingExport::Cards);
            }
            Err(err) => {
                if let Some(tab) = self.tab_by_doc_id_mut(doc_id) {
                    tab.assistant.error = Some(format!(
                        "Could not parse card TSV ({err}). Raw reply is above; no file written."
                    ));
                    tab.assistant.status_line = None;
                }
            }
        }
    }

    pub(crate) fn fit_width(&mut self) {
        let view = self.view_rect;
        if let Some(doc) = self.doc_mut() {
            doc.fit_width(view.width());
            doc.last_fit = Some(FitKind::Width);
            doc.clamp_scroll(view);
        }
    }

    pub(crate) fn fit_height(&mut self) {
        let view = self.view_rect;
        if let Some(doc) = self.doc_mut() {
            doc.fit_height(view.height());
            doc.last_fit = Some(FitKind::Height);
            doc.clamp_scroll(view);
        }
    }

    /// Fit width, or height if the previous Fit was width and zoom was not changed since.
    pub(crate) fn fit_toggle(&mut self) {
        let next_height = self
            .doc()
            .is_some_and(|doc| doc.last_fit == Some(FitKind::Width));
        if next_height {
            self.fit_height();
        } else {
            self.fit_width();
        }
    }

    pub(crate) fn zoom_by(&mut self, factor: f32) {
        let view = self.view_rect;
        let cursor = view.center();
        if let Some(doc) = self.doc_mut() {
            doc.zoom_at(factor, cursor, view);
            doc.last_fit = None;
        }
    }

    pub(crate) fn set_zoom_percent(&mut self, percent: f32) {
        let view = self.view_rect;
        let cursor = view.center();
        if let Some(doc) = self.doc_mut() {
            let target = (percent / 100.0 * crate::geom::ZOOM_100)
                .clamp(crate::geom::MIN_SCALE, crate::geom::MAX_SCALE);
            if (target - doc.scale).abs() > f32::EPSILON {
                doc.zoom_at(target / doc.scale, cursor, view);
                doc.last_fit = None;
            }
        }
    }

    pub(crate) fn search_step(&mut self, delta: i32) {
        let jump = {
            let Some(tab) = self.tab_mut() else {
                return;
            };
            if tab.search.hits.is_empty() {
                return;
            }
            let len = tab.search.hits.len() as i32;
            let next = (tab.search.current as i32 + delta).rem_euclid(len) as usize;
            tab.search.current = next;
            let (page, quads) = &tab.search.hits[next];
            let y = quads.first().map(|quad| quad.y0);
            Some((*page, y))
        };
        if let Some((page, y)) = jump {
            self.queue_jump(page, y);
        }
    }

    fn open_search(&mut self) {
        if let Some(tab) = self.tab_mut() {
            tab.search.open = true;
            tab.search.focus = true;
        }
    }

    pub(crate) fn arm_undo(&mut self) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        let session = tab.doc.session.clone();
        tab.undo.arm(&session);
    }

    pub(crate) fn seal_undo(&mut self) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        if tab.undo.seal(&tab.doc.session) {
            tab.save.touch();
        }
    }

    pub(crate) fn seal_then_arm(&mut self) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        let sealed = tab.undo.seal(&tab.doc.session);
        tab.undo.arm(&tab.doc.session);
        if sealed {
            tab.save.touch();
        }
    }

    pub(crate) fn begin_edit_undo(&mut self, id: u64) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        if tab.undo.begin_edit(&tab.doc.session, id) {
            tab.save.touch();
        }
    }

    pub(crate) fn tag_undo_edit(&mut self, id: u64) {
        if let Some(tab) = self.tab_mut() {
            tab.undo.tag_edit(id);
        }
    }

    pub(crate) fn end_edit_undo(&mut self) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        if tab.undo.end_edit(&tab.doc.session) {
            tab.save.touch();
        }
    }

    fn undo(&mut self) {
        self.step_undo(Direction::Undo);
    }

    fn redo(&mut self) {
        self.step_undo(Direction::Redo);
    }

    fn step_undo(&mut self, dir: Direction) {
        self.seal_undo();
        let Some(tab) = self.tab() else {
            return;
        };
        if tab.undo.page_op.is_some() || matches!(tab.save, SaveState::Saving) {
            return;
        }
        let doc_id = tab.doc.doc_id;
        let current = tab.doc.session.clone();
        let Some(tab) = self.tab_mut() else {
            return;
        };
        let Some(outcome) = tab.undo.step(dir, &current) else {
            return;
        };
        match outcome {
            StepOutcome::Annots { session } => {
                tab.doc.session = session;
                tab.editing = None;
                tab.menu = None;
                tab.style_bar = None;
                tab.selected.retain(|id| tab.doc.session.get(*id).is_some());
                tab.save.touch();
            }
            StepOutcome::PageOp { kind, index } => match kind {
                PageOpKind::Insert => self.worker.insert_page_at(doc_id, index),
                PageOpKind::Delete => self.worker.delete_page(doc_id, index),
            },
        }
    }

    /// Text available for Ctrl+C: page text selection, highlight glyphs, or text annot.
    pub(crate) fn copyable_selection_text(&self) -> Option<String> {
        let tab = self.tab()?;
        if let Some(sel) = &tab.text_sel {
            let glyphs = tab.doc.glyphs.get(&sel.page)?;
            let text = crate::assistant::reconstruct_text(glyphs, sel.glyph_lo, sel.glyph_hi);
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
        if tab.selected.len() == 1 {
            let id = tab.selected[0];
            let annot = tab.doc.session.get(id)?;
            match &annot.kind {
                AnnotKind::Highlight { quads, .. } | AnnotKind::Markup { quads, .. } => {
                    let glyphs = tab.doc.glyphs.get(&annot.page)?;
                    let indices = crate::assistant::glyphs_intersecting_rects(glyphs, quads);
                    let (&lo, &hi) = (indices.first()?, indices.last()?);
                    let text = crate::assistant::reconstruct_text(glyphs, lo, hi);
                    if !text.trim().is_empty() {
                        return Some(text);
                    }
                }
                AnnotKind::Text { content, .. } | AnnotKind::Note { content, .. }
                    if !content.is_empty() =>
                {
                    return Some(content.clone());
                }
                AnnotKind::Text { .. }
                | AnnotKind::Note { .. }
                | AnnotKind::Math { .. }
                | AnnotKind::Shape { .. }
                | AnnotKind::Image { .. }
                | AnnotKind::Foreign { .. } => {}
            }
        }
        None
    }

    /// Word under a page point (for right-click Copy when nothing is selected).
    pub(crate) fn word_at_point(&self, page: usize, point: PdfPoint) -> Option<String> {
        let tab = self.tab()?;
        let glyphs = tab.doc.glyphs.get(&page)?;
        let index = crate::annot::glyph_at(glyphs, point)?;
        let (lo, hi) = crate::annot::word_range(glyphs, index)?;
        let text = crate::assistant::reconstruct_text(glyphs, lo, hi);
        (!text.trim().is_empty()).then_some(text)
    }

    pub(crate) fn delete_selected(&mut self) {
        self.end_edit_undo();
        if let Some(tab) = self.tab_mut() {
            tab.editing = None;
        }
        let ids = self
            .tab()
            .map(|tab| tab.selected.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            return;
        }
        self.seal_then_arm();
        if let Some(tab) = self.tab_mut() {
            for id in &ids {
                if tab.doc.session.get(*id).is_some_and(|annot| {
                    matches!(annot.kind, crate::annot::AnnotKind::Foreign { .. })
                }) {
                    continue;
                }
                tab.doc.session.remove(*id);
                tab.previews.remove(id);
                tab.image_textures.remove(id);
            }
            tab.selected.clear();
            tab.menu = None;
            tab.style_bar = None;
            tab.editing = None;
        }
        if let Some(doc_id) = self.tab().map(|tab| tab.doc.doc_id) {
            for id in &ids {
                self.math.forget_annot(doc_id, *id);
            }
        }
        self.seal_undo();
    }

    /// Paste an image from the system clipboard onto the current page.
    pub(crate) fn paste_clipboard_image(&mut self) -> bool {
        let Some((width, height, rgba)) = platform::read_clipboard_rgba() else {
            return false;
        };
        if width == 0 || height == 0 {
            return false;
        }

        let view_h = self.view_rect.height().max(1.0);
        let Some(tab) = self.tab() else {
            return false;
        };
        if tab.doc.pages.is_empty() {
            return false;
        }
        let page = tab.doc.current_page(view_h);
        let info = tab.doc.pages[page];
        let aspect = height as f32 / width as f32;
        let mut disp_w = info.width() * 0.5;
        let mut disp_h = disp_w * aspect;
        if disp_h > info.height() * 0.9 {
            disp_h = info.height() * 0.9;
            disp_w = disp_h / aspect;
        }
        let cx = (info.x0 + info.x1) * 0.5;
        let cy = (info.y0 + info.y1) * 0.5;
        let rect = crate::geom::PdfRect::new(
            cx - disp_w * 0.5,
            cy - disp_h * 0.5,
            cx + disp_w * 0.5,
            cy + disp_h * 0.5,
        );

        self.seal_then_arm();
        let Some(tab) = self.tab_mut() else {
            return false;
        };
        let id = tab.doc.session.insert(
            page,
            AnnotKind::Image {
                rect,
                rgba,
                width,
                height,
            },
        );
        tab.select_only(id);
        tab.editing = None;
        tab.menu = None;
        tab.style_bar = None;
        self.seal_undo();
        true
    }

    pub(crate) fn toggle_preset_editor(&mut self) {
        self.preset_editor_open = !self.preset_editor_open;
    }

    /// Re-selecting a markup or shape tool restores that family's factory color (#85).
    /// Text / note / math ink stays sticky on purpose.
    pub(crate) fn restore_default_tool_color(&mut self, tool: Tool) {
        let changed = if tool.is_text_mark() {
            self.settings.restore_default_highlight_color()
        } else if tool.is_shape() {
            self.settings.restore_default_shape_color()
        } else {
            false
        };
        if changed {
            self.settings.save();
        }
    }

    /// Clear annotation selection, style bar, and assistant learning selection.
    pub(crate) fn clear_page_selection(&mut self) {
        if let Some(tab) = self.tab_mut() {
            tab.selected.clear();
            tab.text_sel = None;
            tab.editing = None;
            tab.style_bar = None;
            tab.assistant.learning = None;
        }
        if self.capture == CaptureMode::LearningText {
            // Keep capture mode so the user can re-drag; only clear the committed selection.
        }
    }
}

impl eframe::App for MarkerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|input| input.viewport().close_requested()) {
            self.flush_on_exit_if_needed();
        }
        let pixels_per_point = ctx.pixels_per_point();
        if self.active != self.tile_focus_tab {
            if let Some(prev) = self.tab_index(self.tile_focus_tab) {
                trim_tab_tiles(self, prev, UNFOCUSED_TAB_TILE_BYTES, pixels_per_point);
            }
            self.tile_focus_tab = self.active;
        }
        self.tile_frame += 1;
        self.poll_ipc(ctx);
        self.poll_dialog();
        self.poll(ctx);
        self.check_pending_export();
        self.flush_math();
        self.prepare_visible_math(pixels_per_point);
        self.dispatch_search();
        self.handle_keys(ctx);
        self.autosave(ctx);
        crate::theme::sync_os_theme(ctx);
        ui::chrome(self, ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(crate::theme::palette(ctx).backdrop))
            .show(ctx, |ui| {
                if self.tabs.is_empty() {
                    ui::empty_state(self, ui);
                    return;
                }
                if let Some(split) = self.split {
                    ui::split_viewports(self, ui, split);
                } else {
                    viewport(self, ui, true);
                    self.dispatch_tiles(ctx.pixels_per_point());
                }
            });
        ui::tab_drag_overlay(self, ctx);
        self.set_title(ctx);

        // Title changes are sent only when the text changes, so idle must not
        // schedule another frame. The old UI-thread sleep was only there to stop
        // a set_title-every-frame loop from spinning the CPU.
        //
        // vsync stays off (Hyprland hidden workspaces never get frame callbacks).
        // While interacting, keep a ~60fps budget and do not schedule the next
        // frame sooner than 8ms after this one.
        let interacting = ctx.input(|input| {
            let scroll = input.smooth_scroll_delta.length_sq() > 0.01
                || input.raw_scroll_delta.length_sq() > 0.01;
            let zoom = (input.zoom_delta() - 1.0).abs() > 0.001;
            input.pointer.any_down() || scroll || zoom
        }) || self.tabs.iter().any(|tab| {
            tab.doc.last_zoom.elapsed().as_millis() < 150
                || tab.doc.last_scroll.elapsed().as_millis() < 150
        });
        let pending = !self.opening.is_empty()
            || self.dialog_busy
            || self.math.has_deadlines()
            || self.tabs.iter().any(|tab| {
                matches!(tab.save, SaveState::Saving)
                    || !tab.inflight.is_empty()
                    || !tab.thumbs_inflight.is_empty()
                    || tab.search.pending
                    || tab.pending_export.is_some()
                    || tab.assistant.streaming
                    || tab.assistant.pending_crop.is_some()
            });
        if interacting {
            let focused = ctx.input(|input| input.focused);
            let base = if focused { 16 } else { 33 };
            let elapsed = self.last_frame.elapsed().as_millis() as u64;
            let wait = if elapsed < 8 {
                base.max(8 - elapsed)
            } else {
                base
            };
            ctx.request_repaint_after(Duration::from_millis(wait));
        } else if let Some(when) = self.math.soonest_deadline() {
            ctx.request_repaint_after(when.saturating_duration_since(Instant::now()));
        } else if pending {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        self.last_frame = Instant::now();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.flush_on_exit_if_needed();
    }
}

impl MarkerApp {
    /// Point size used for math inside finished assistant turns (ST7).
    pub(crate) const ASSISTANT_MATH_SIZE: f32 = crate::math::ASSISTANT_MATH_SIZE;

    pub(crate) fn dispatch_tiles(&mut self, pixels_per_point: f32) {
        if let Some(index) = self.tab_index(self.active) {
            self.dispatch_tiles_at(index, self.view_rect, pixels_per_point);
        }
        if let Some(split) = self.split {
            if let Some(index) = self.tab_index(split.other(self.active)) {
                self.dispatch_tiles_at(index, self.split_view_rect, pixels_per_point);
            }
        }
    }

    fn dispatch_tiles_at(&mut self, index: usize, view: egui::Rect, pixels_per_point: f32) {
        if index >= self.tabs.len() {
            return;
        }
        let (doc_id, wanted) = {
            let tab = &mut self.tabs[index];
            let wanted = view::wanted_tiles(
                &mut tab.doc,
                &tab.inflight,
                self.tool,
                view,
                pixels_per_point,
            );
            (tab.doc.doc_id, wanted)
        };
        {
            let mut guard = self
                .tile_wanted
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            guard.insert(doc_id, wanted.tiles.iter().map(|req| req.key).collect());
        }
        let mut glyph_pages = Vec::new();
        let mut tiles = Vec::new();
        for page in wanted.words {
            if let Some(tab) = self.tabs.get_mut(index) {
                if tab.glyphs_waiting.insert(page) {
                    glyph_pages.push(page);
                }
            }
        }
        for req in wanted.tiles {
            if let Some(tab) = self.tabs.get_mut(index) {
                tab.inflight.insert(req.key);
            }
            tiles.push(req);
        }
        for req in tiles {
            self.worker.tile(
                doc_id,
                req.key.page,
                req.scale,
                req.key.col,
                req.key.row,
                req.prefetch,
                req.distance,
            );
        }
        for page in glyph_pages {
            self.worker.glyphs(doc_id, page);
        }
    }

    fn dispatch_search(&mut self) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        if !tab.search.open {
            return;
        }
        if tab.search.query == tab.search.last_sent {
            return;
        }
        if tab.search.query.trim().is_empty() {
            tab.search.hits.clear();
            tab.search.last_sent.clear();
            tab.search.pending = false;
            return;
        }
        tab.search.seq += 1;
        tab.search.last_sent = tab.search.query.clone();
        tab.search.pending = true;
        tab.search.hits.clear();
        tab.search.current = 0;
        let doc_id = tab.doc.doc_id;
        let seq = tab.search.seq;
        let query = tab.search.query.clone();
        self.worker.search(doc_id, seq, query);
    }

    pub(crate) fn toggle_toolbar(&mut self) {
        self.settings.toolbar_visible = !self.settings.toolbar_visible;
        self.settings.save();
    }

    pub(crate) fn toggle_zen(&mut self, ctx: &egui::Context) {
        self.zen = !self.zen;
        self.zen_chrome_until = None;
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(self.zen));
        if self.zen {
            // Peek chrome briefly so the user sees how to get it back.
            self.zen_chrome_until = Some(Instant::now() + Duration::from_secs(2));
        }
    }

    /// Whether tab/tool chrome should draw this frame (zen auto-hides until top-edge hover).
    pub(crate) fn chrome_visible(&mut self, ctx: &egui::Context) -> bool {
        if !self.zen {
            return true;
        }
        let top_hover =
            ctx.input(|input| input.pointer.hover_pos().is_some_and(|pos| pos.y <= 10.0));
        if top_hover {
            self.zen_chrome_until = Some(Instant::now() + Duration::from_millis(900));
            ctx.request_repaint_after(Duration::from_millis(950));
            return true;
        }
        if let Some(until) = self.zen_chrome_until {
            if Instant::now() < until {
                ctx.request_repaint_after(until.saturating_duration_since(Instant::now()));
                return true;
            }
            self.zen_chrome_until = None;
        }
        false
    }

    /// Show the floating style strip for `id`. Brief = auto-hide a few seconds after create.
    pub(crate) fn open_style_bar(&mut self, id: u64, brief: bool) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        if tab.doc.session.get(id).is_none() {
            return;
        }
        if !tab.is_selected(id) {
            tab.select_only(id);
        } else {
            // Keep multi-select; ensure `id` is primary for the strip anchor.
            tab.selected.retain(|x| *x != id);
            tab.selected.insert(0, id);
        }
        tab.menu = None;
        tab.style_bar = Some(StyleBar {
            id,
            until: brief.then(|| Instant::now() + Duration::from_secs(4)),
        });
    }

    fn set_title(&mut self, ctx: &egui::Context) {
        if std::env::var_os("PERF_NO_TITLE").is_some_and(|v| v == "1") {
            return;
        }
        let title = if let Some(index) = self.tab_index(self.active) {
            let tab = &self.tabs[index];
            let name = tab
                .doc
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("document.pdf");
            let epoch = tab.doc.session.epoch;
            let doc_id = tab.doc.doc_id;
            let dirty_suffix = if doc_id == self.title_tab_doc_id && epoch == self.title_dirty_epoch
            {
                if self.title_dirty {
                    " •"
                } else {
                    ""
                }
            } else {
                let dirty = tab.doc.session.is_dirty();
                self.title_tab_doc_id = doc_id;
                self.title_dirty_epoch = epoch;
                self.title_dirty = dirty;
                if dirty {
                    " •"
                } else {
                    ""
                }
            };
            format!("Marker — {name}{dirty_suffix}")
        } else {
            "Marker".into()
        };
        if title == self.last_title {
            return;
        }
        self.last_title = title.clone();
        ctx.send_viewport_cmd(ViewportCommand::Title(title));
    }
}

const PROCESS_TILE_BUDGET_BYTES: usize = 256 * 1024 * 1024;
const UNFOCUSED_TAB_TILE_BYTES: usize = 32 * 1024 * 1024;
