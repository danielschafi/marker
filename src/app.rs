use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use egui::{DragValue, Key, ViewportCommand};

use crate::annot::{AnnotKind, Annotation, Glyph, Handle, Session, ShapeKind};
use crate::geom::{PdfPoint, Rgb, ZOOM_100};
use crate::math::{MathRender, MathWorker, RgbaImage};
use crate::pdf::{OutlineNode, PageInfo, PdfReply, PdfWorker, SaveSnapshot};
use crate::settings::Settings;
use crate::ui::{self, color_dot, palette_for};
use crate::view::{self, viewport};

pub(crate) struct MarkerApp {
    pub(crate) settings: Settings,
    pub(crate) tool: Tool,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: usize,
    pub(crate) view_rect: egui::Rect,
    pub(crate) opening: HashSet<u64>,
    pub(crate) error: Option<String>,
    pub(crate) page_focus: bool,
    vim_count: u32,
    vim_g: bool,
    worker: PdfWorker,
    math: MathWorker,
    math_seq: u64,
    math_deadline: Option<(u64, u64, Instant)>,
    next_gen: u64,
    dialog_tx: Sender<Option<PathBuf>>,
    dialog_rx: Receiver<Option<PathBuf>>,
    dialog_busy: bool,
}

pub(crate) struct Tab {
    pub(crate) doc: DocState,
    pub(crate) selected: Option<u64>,
    pub(crate) editing: Option<u64>,
    pub(crate) drag: Option<Drag>,
    pub(crate) pending_jump: Option<(usize, Option<f32>)>,
    pub(crate) inflight: HashSet<TileKey>,
    pub(crate) glyphs_waiting: HashSet<usize>,
    pub(crate) previews: HashMap<u64, MathPreview>,
    pub(crate) image_textures: HashMap<u64, (usize, egui::TextureHandle)>,
    pub(crate) save: SaveState,
    pub(crate) save_epoch: u64,
    pub(crate) save_deletes: Vec<(usize, i32)>,
    pub(crate) force_save: bool,
    pub(crate) save_when_math_ready: bool,
    pub(crate) close_after_save: bool,
    pub(crate) outline_open: bool,
    pub(crate) search: SearchState,
    pub(crate) last_hl: Option<(Instant, usize, u32, Option<u64>)>,
    pub(crate) focus_edit: bool,
    pub(crate) menu: Option<(egui::Pos2, Option<u64>)>,
    pending_undo: Option<Session>,
    undo_edit: Option<u64>,
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    /// In-flight page insert/delete that participates in undo/redo.
    page_op: Option<PendingPageOp>,
}

pub(crate) struct DocState {
    pub(crate) path: PathBuf,
    pub(crate) gen: u64,
    pub(crate) pages: Vec<PageInfo>,
    pub(crate) tops: Vec<f32>,
    pub(crate) outline: Vec<OutlineNode>,
    pub(crate) session: Session,
    pub(crate) scale: f32,
    pub(crate) scroll_x: f32,
    pub(crate) scroll_y: f32,
    pub(crate) fitted: bool,
    pub(crate) last_zoom: Instant,
    pub(crate) glyphs: HashMap<usize, Vec<Glyph>>,
    pub(crate) tiles: HashMap<TileKey, CachedTile>,
}

pub(crate) struct CachedTile {
    pub(crate) scale: f32,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) texture: egui::TextureHandle,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TileKey {
    pub(crate) page: usize,
    pub(crate) scale_bits: u32,
    pub(crate) col: i32,
    pub(crate) row: i32,
}

pub(crate) struct MathPreview {
    source: String,
    size: f32,
    color: Rgb,
    req: u64,
    pending: bool,
    pub(crate) texture: Option<egui::TextureHandle>,
    pub(crate) width_pt: f32,
    pub(crate) height_pt: f32,
    pdf: Option<Vec<u8>>,
    pub(crate) error: Option<String>,
}

#[derive(Default)]
pub(crate) struct SearchState {
    pub(crate) open: bool,
    pub(crate) query: String,
    pub(crate) last_sent: String,
    pub(crate) seq: u64,
    pub(crate) hits: Vec<(usize, Vec<crate::geom::PdfRect>)>,
    pub(crate) current: usize,
    pub(crate) focus: bool,
    pub(crate) pending: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    Select,
    Highlight,
    Text,
    Rect,
    Ellipse,
    Line,
    Math,
}

impl Tool {
    pub(crate) const ALL: [Tool; 7] = [
        Tool::Select,
        Tool::Highlight,
        Tool::Text,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Line,
        Tool::Math,
    ];

    /// Bare letter that selects this tool. `H` and `L` stay as vim panning.
    pub(crate) fn shortcut(self) -> Key {
        match self {
            Tool::Select => Key::V,
            Tool::Highlight => Key::A,
            Tool::Text => Key::T,
            Tool::Rect => Key::R,
            Tool::Ellipse => Key::E,
            Tool::Line => Key::I,
            Tool::Math => Key::M,
        }
    }

    pub(crate) fn hint(self) -> &'static str {
        match self {
            Tool::Select => "Move and resize",
            Tool::Highlight => "Mark text",
            Tool::Text => "Write on the page",
            Tool::Rect => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Line => "Line",
            Tool::Math => "Equation",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CreateKind {
    Text,
    Math,
}

#[derive(Clone, PartialEq)]
enum UndoEntry {
    Annots(Session),
    /// A blank page was inserted at this index.
    InsertPage { index: usize },
}

#[derive(Clone, Copy)]
enum PendingPageOp {
    /// Insert blank page at `index`. `record_undo` is set for user-driven inserts.
    Insert { index: usize, record_undo: bool },
    /// Undo of InsertPage — waiting for delete.
    Delete { index: usize },
}

#[derive(Clone)]
pub(crate) enum Drag {
    Highlight {
        page: usize,
        anchor: Option<usize>,
        current: Option<usize>,
        origin: PdfPoint,
        current_pt: PdfPoint,
        word_lo: Option<usize>,
        word_hi: Option<usize>,
        replace: Option<u64>,
    },
    Shape {
        page: usize,
        kind: ShapeKind,
        origin: PdfPoint,
        current: PdfPoint,
    },
    Create {
        page: usize,
        origin: PdfPoint,
        current: PdfPoint,
        kind: CreateKind,
    },
    Move {
        id: u64,
        origin: AnnotKind,
        grab: PdfPoint,
        page: usize,
        moved: bool,
    },
    Resize {
        id: u64,
        handle: Handle,
        origin: AnnotKind,
        page: usize,
    },
    Pan {
        scroll_x: f32,
        scroll_y: f32,
        pos: egui::Pos2,
    },
}

pub(crate) enum SaveState {
    Clean,
    Dirty { since: Instant },
    Saving,
    Failed { message: String, at: Instant },
}

impl MarkerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: Vec<PathBuf>) -> Self {
        ui::apply_theme(&cc.egui_ctx);
        let (dialog_tx, dialog_rx) = mpsc::channel();
        let mut app = Self {
            settings: Settings::load(),
            tool: Tool::Select,
            tabs: Vec::new(),
            active: 0,
            view_rect: egui::Rect::NOTHING,
            opening: HashSet::new(),
            error: None,
            page_focus: false,
            vim_count: 0,
            vim_g: false,
            worker: PdfWorker::spawn(),
            math: MathWorker::spawn(),
            math_seq: 1,
            math_deadline: None,
            next_gen: 1,
            dialog_tx,
            dialog_rx,
            dialog_busy: false,
        };
        for path in paths {
            app.open_path(path);
        }
        app
    }

    pub(crate) fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub(crate) fn tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    pub(crate) fn doc(&self) -> Option<&DocState> {
        self.tab().map(|tab| &tab.doc)
    }

    pub(crate) fn doc_mut(&mut self) -> Option<&mut DocState> {
        self.tab_mut().map(|tab| &mut tab.doc)
    }

    fn tab_by_gen_mut(&mut self, gen: u64) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.doc.gen == gen)
    }

    pub(crate) fn queue_math(&mut self, id: u64) {
        if let Some(tab) = self.tab() {
            self.math_deadline =
                Some((tab.doc.gen, id, Instant::now() + Duration::from_millis(160)));
        }
    }

    pub(crate) fn queue_jump(&mut self, page: usize, y: Option<f32>) {
        if let Some(tab) = self.tab_mut() {
            tab.pending_jump = Some((page, y));
        }
    }

    pub(crate) fn open_dialog(&mut self) {
        if self.dialog_busy {
            return;
        }
        self.dialog_busy = true;
        let tx = self.dialog_tx.clone();
        thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .add_filter("PDF", &["pdf"])
                .pick_file();
            let _ = tx.send(path);
        });
    }

    pub(crate) fn open_path(&mut self, path: PathBuf) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.doc.path == path) {
            self.active = index;
            self.settings.remember_open(&path);
            return;
        }
        self.next_gen += 1;
        let gen = self.next_gen;
        self.opening.insert(gen);
        self.error = None;
        self.worker.open(gen, path);
    }

    pub(crate) fn close_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        if tab.doc.session.is_dirty() {
            tab.force_save = true;
            tab.close_after_save = true;
            return;
        }
        self.drop_tab(index);
    }

    fn drop_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        self.worker.close(tab.doc.gen);
        if self.tabs.is_empty() {
            self.active = 0;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if self.active > index {
            self.active -= 1;
        }
    }

    pub(crate) fn fit_width(&mut self) {
        let view = self.view_rect;
        if let Some(doc) = self.doc_mut() {
            doc.fit_width(view.width());
            doc.clamp_scroll(view);
        }
    }

    pub(crate) fn zoom_by(&mut self, factor: f32) {
        let view = self.view_rect;
        let cursor = view.center();
        if let Some(doc) = self.doc_mut() {
            doc.zoom_at(factor, cursor, view);
        }
    }

    pub(crate) fn set_zoom_percent(&mut self, percent: f32) {
        let view = self.view_rect;
        let cursor = view.center();
        if let Some(doc) = self.doc_mut() {
            let target = (percent / 100.0) * ZOOM_100;
            if doc.scale > f32::EPSILON {
                doc.zoom_at(target / doc.scale, cursor, view);
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
        if tab.pending_undo.is_none() {
            tab.pending_undo = Some(tab.doc.session.clone());
        }
    }

    pub(crate) fn seal_undo(&mut self) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        let Some(before) = tab.pending_undo.take() else {
            return;
        };
        tab.undo_edit = None;
        if before == tab.doc.session {
            return;
        }
        tab.undo.push(UndoEntry::Annots(before));
        if tab.undo.len() > 80 {
            tab.undo.remove(0);
        }
        tab.redo.clear();
        if !matches!(tab.save, SaveState::Saving) {
            tab.save = SaveState::Dirty {
                since: Instant::now(),
            };
        }
    }

    pub(crate) fn seal_then_arm(&mut self) {
        self.seal_undo();
        self.arm_undo();
    }

    pub(crate) fn begin_edit_undo(&mut self, id: u64) {
        let same = self
            .tab()
            .is_some_and(|tab| tab.undo_edit == Some(id) && tab.pending_undo.is_some());
        if same {
            return;
        }
        self.seal_undo();
        self.arm_undo();
        if let Some(tab) = self.tab_mut() {
            tab.undo_edit = Some(id);
        }
    }

    pub(crate) fn tag_undo_edit(&mut self, id: u64) {
        if let Some(tab) = self.tab_mut() {
            tab.undo_edit = Some(id);
        }
    }

    pub(crate) fn end_edit_undo(&mut self) {
        let editing = self.tab().and_then(|tab| tab.undo_edit);
        if editing.is_some() {
            self.seal_undo();
        }
    }

    fn undo(&mut self) {
        self.seal_undo();
        let Some(tab) = self.tab() else {
            return;
        };
        if tab.page_op.is_some() || matches!(tab.save, SaveState::Saving) {
            return;
        }
        let Some(entry) = self.tab_mut().and_then(|tab| tab.undo.pop()) else {
            return;
        };
        match entry {
            UndoEntry::Annots(prev) => {
                let Some(tab) = self.tab_mut() else {
                    return;
                };
                let current = tab.doc.session.clone();
                tab.redo.push(UndoEntry::Annots(current.clone()));
                tab.doc.session = crate::annot::restore_session(&current, prev);
                tab.editing = None;
                tab.undo_edit = None;
                tab.menu = None;
                if tab
                    .selected
                    .is_some_and(|id| tab.doc.session.get(id).is_none())
                {
                    tab.selected = None;
                }
                if !matches!(tab.save, SaveState::Saving) {
                    tab.save = SaveState::Dirty {
                        since: Instant::now(),
                    };
                }
            }
            UndoEntry::InsertPage { index } => {
                let gen = self.tab().map(|tab| tab.doc.gen);
                let Some(tab) = self.tab_mut() else {
                    return;
                };
                tab.redo.push(UndoEntry::InsertPage { index });
                tab.page_op = Some(PendingPageOp::Delete { index });
                if let Some(gen) = gen {
                    self.worker.delete_page(gen, index);
                }
            }
        }
    }

    fn redo(&mut self) {
        self.seal_undo();
        let Some(tab) = self.tab() else {
            return;
        };
        if tab.page_op.is_some() || matches!(tab.save, SaveState::Saving) {
            return;
        }
        let Some(entry) = self.tab_mut().and_then(|tab| tab.redo.pop()) else {
            return;
        };
        match entry {
            UndoEntry::Annots(next) => {
                let Some(tab) = self.tab_mut() else {
                    return;
                };
                let current = tab.doc.session.clone();
                tab.undo.push(UndoEntry::Annots(current.clone()));
                tab.doc.session = crate::annot::restore_session(&current, next);
                tab.editing = None;
                tab.undo_edit = None;
                tab.menu = None;
                if tab
                    .selected
                    .is_some_and(|id| tab.doc.session.get(id).is_none())
                {
                    tab.selected = None;
                }
                if !matches!(tab.save, SaveState::Saving) {
                    tab.save = SaveState::Dirty {
                        since: Instant::now(),
                    };
                }
            }
            UndoEntry::InsertPage { index } => {
                let gen = self.tab().map(|tab| tab.doc.gen);
                let Some(tab) = self.tab_mut() else {
                    return;
                };
                tab.undo.push(UndoEntry::InsertPage { index });
                tab.page_op = Some(PendingPageOp::Insert {
                    index,
                    record_undo: false,
                });
                if let Some(gen) = gen {
                    self.worker.insert_page_at(gen, index);
                }
            }
        }
    }

    pub(crate) fn delete_selected(&mut self) {
        self.end_edit_undo();
        if let Some(tab) = self.tab_mut() {
            tab.editing = None;
        }
        let Some(id) = self.tab().and_then(|tab| tab.selected) else {
            return;
        };
        self.seal_then_arm();
        if let Some(tab) = self.tab_mut() {
            tab.doc.session.remove(id);
            tab.selected = None;
            tab.menu = None;
            tab.previews.remove(&id);
            tab.image_textures.remove(&id);
            tab.editing = None;
        }
        self.seal_undo();
    }

    /// Paste an image from the system clipboard onto the current page.
    pub(crate) fn paste_clipboard_image(&mut self) -> bool {
        let Ok(mut clipboard) = arboard::Clipboard::new() else {
            return false;
        };
        let Ok(image) = clipboard.get_image() else {
            return false;
        };
        let width = image.width as u32;
        let height = image.height as u32;
        if width == 0 || height == 0 {
            return false;
        }
        let expected = width as usize * height as usize * 4;
        if image.bytes.len() < expected {
            return false;
        }
        let rgba: std::sync::Arc<[u8]> = std::sync::Arc::from(image.bytes[..expected].to_vec());

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
        tab.selected = Some(id);
        tab.editing = None;
        tab.menu = None;
        self.seal_undo();
        true
    }

    fn nudge(&mut self, dx: f32, dy: f32) {
        let view = self.view_rect;
        if let Some(doc) = self.doc_mut() {
            doc.scroll_x += dx;
            doc.scroll_y += dy;
            doc.clamp_scroll(view);
        }
    }

    /// Vim-style navigation. Returns true when the key should not also switch tools.
    fn handle_vim(&mut self, input: &egui::InputState) -> bool {
        if self.doc().is_none() {
            return false;
        }
        if input.modifiers.command && input.key_pressed(Key::D) {
            let steps = self.take_count();
            let dy = self.view_rect.height() * 0.5 * steps;
            self.nudge(0.0, dy);
            return true;
        }
        if input.modifiers.command && input.key_pressed(Key::U) {
            let steps = self.take_count();
            let dy = self.view_rect.height() * 0.5 * steps;
            self.nudge(0.0, -dy);
            return true;
        }
        if !input.modifiers.command && !input.modifiers.alt {
            if let Some(digit) = vim_digit(input) {
                self.vim_count = self.vim_count.saturating_mul(10).saturating_add(digit);
                return true;
            }
        }
        if input.key_pressed(Key::G) && !input.modifiers.command {
            if input.modifiers.shift {
                let count = self.vim_count;
                self.vim_count = 0;
                self.vim_g = false;
                let last = self
                    .doc()
                    .map(|doc| doc.pages.len().saturating_sub(1))
                    .unwrap_or(0);
                let page = if count == 0 {
                    last
                } else {
                    count.saturating_sub(1) as usize
                };
                self.queue_jump(page, Some(0.0));
            } else if self.vim_g {
                let page = self.take_count() as usize;
                let page = page.saturating_sub(1);
                self.vim_g = false;
                self.queue_jump(page, Some(0.0));
            } else {
                self.vim_g = true;
            }
            return true;
        }
        let step = (self.view_rect.height() * 0.045).clamp(28.0, 72.0);
        if input.key_pressed(Key::J) && !input.modifiers.command {
            let n = self.take_count();
            self.vim_g = false;
            self.nudge(0.0, step * n);
            return true;
        }
        if input.key_pressed(Key::K) && !input.modifiers.command {
            let n = self.take_count();
            self.vim_g = false;
            self.nudge(0.0, -step * n);
            return true;
        }
        if input.key_pressed(Key::H) && !input.modifiers.command {
            let n = self.take_count();
            self.vim_g = false;
            self.nudge(-step * n, 0.0);
            return true;
        }
        if input.key_pressed(Key::L) && !input.modifiers.command {
            let n = self.take_count();
            self.vim_g = false;
            self.nudge(step * n, 0.0);
            return true;
        }
        if input
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::Key { pressed: true, .. }))
        {
            self.vim_count = 0;
            self.vim_g = false;
        }
        false
    }

    fn leave_typing(&mut self) {
        let seal = {
            let Some(tab) = self.tab_mut() else {
                return;
            };
            if tab.menu.take().is_some() {
                return;
            }
            if tab.editing.take().is_some() {
                tab.focus_edit = false;
                true
            } else if tab.search.open {
                tab.search.open = false;
                tab.search.hits.clear();
                tab.search.query.clear();
                tab.search.last_sent.clear();
                false
            } else {
                false
            }
        };
        if seal {
            self.end_edit_undo();
        }
    }

    fn take_count(&mut self) -> f32 {
        let count = self.vim_count.max(1);
        self.vim_count = 0;
        count as f32
    }
}

fn vim_digit(input: &egui::InputState) -> Option<u32> {
    const KEYS: [(egui::Key, u32); 10] = [
        (egui::Key::Num0, 0),
        (egui::Key::Num1, 1),
        (egui::Key::Num2, 2),
        (egui::Key::Num3, 3),
        (egui::Key::Num4, 4),
        (egui::Key::Num5, 5),
        (egui::Key::Num6, 6),
        (egui::Key::Num7, 7),
        (egui::Key::Num8, 8),
        (egui::Key::Num9, 9),
    ];
    KEYS.into_iter()
        .find(|(key, _)| input.key_pressed(*key))
        .map(|(_, digit)| digit)
}

impl eframe::App for MarkerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_dialog();
        self.poll(ctx);
        self.flush_math();
        self.prepare_visible_math();
        self.dispatch_search();
        self.handle_keys(ctx);
        self.autosave(ctx);
        ui::chrome(self, ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(ui::BACKDROP))
            .show(ctx, |ui| {
                if self.tab().is_some() {
                    viewport(self, ui);
                    self.dispatch_tiles();
                } else {
                    ui::empty_state(self, ui);
                }
            });
        self.set_title(ctx);
        let busy = !self.opening.is_empty()
            || self.dialog_busy
            || self.tabs.iter().any(|tab| {
                matches!(tab.save, SaveState::Saving)
                    || !tab.inflight.is_empty()
                    || tab.search.pending
                    || tab.doc.last_zoom.elapsed().as_millis() < 200
            });
        if busy {
            let focused = ctx.input(|input| input.focused);
            let wait = if focused { 8 } else { 200 };
            ctx.request_repaint_after(Duration::from_millis(wait));
        }
    }
}

impl MarkerApp {
    fn poll_dialog(&mut self) {
        match self.dialog_rx.try_recv() {
            Ok(Some(path)) => {
                self.dialog_busy = false;
                self.open_path(path);
            }
            Ok(None) => self.dialog_busy = false,
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.dialog_busy = false,
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        for reply in self.worker.poll() {
            self.on_pdf(ctx, reply);
        }
        for reply in self.math.poll() {
            self.on_math(ctx, reply);
        }
    }

    fn on_pdf(&mut self, ctx: &egui::Context, reply: PdfReply) {
        match reply {
            PdfReply::Opened(result) => match result {
                Ok(opened) => {
                    self.opening.remove(&opened.gen);
                    if self.tabs.iter().any(|tab| tab.doc.path == opened.path) {
                        self.worker.close(opened.gen);
                        if let Some(index) =
                            self.tabs.iter().position(|tab| tab.doc.path == opened.path)
                        {
                            self.active = index;
                        }
                        self.settings.remember_open(&opened.path);
                        return;
                    }
                    let tops = DocState::rebuild_tops(&opened.pages);
                    let has_outline = !opened.outline.is_empty();
                    self.settings.remember_open(&opened.path);
                    self.tabs.push(Tab {
                        doc: DocState {
                            path: opened.path,
                            gen: opened.gen,
                            pages: opened.pages,
                            tops,
                            outline: opened.outline,
                            session: Session::from_imported(opened.annotations),
                            scale: 1.0,
                            scroll_x: 0.0,
                            scroll_y: 0.0,
                            fitted: false,
                            last_zoom: Instant::now(),
                            glyphs: HashMap::new(),
                            tiles: HashMap::new(),
                        },
                        selected: None,
                        editing: None,
                        drag: None,
                        pending_jump: None,
                        inflight: HashSet::new(),
                        glyphs_waiting: HashSet::new(),
                        previews: HashMap::new(),
                        image_textures: HashMap::new(),
                        save: SaveState::Clean,
                        save_epoch: 0,
                        save_deletes: Vec::new(),
                        force_save: false,
                        save_when_math_ready: false,
                        close_after_save: false,
                        outline_open: has_outline,
                        search: SearchState::default(),
                        last_hl: None,
                        focus_edit: false,
                        menu: None,
                        pending_undo: None,
                        undo_edit: None,
                        undo: Vec::new(),
                        redo: Vec::new(),
                        page_op: None,
                    });
                    self.active = self.tabs.len() - 1;
                }
                Err(message) => {
                    self.opening.clear();
                    self.error = Some(message);
                }
            },
            PdfReply::Tile { gen, tile } => {
                let key = tile_key(&tile);
                let Some(tab) = self.tab_by_gen_mut(gen) else {
                    return;
                };
                tab.inflight.remove(&key);
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [tile.width as usize, tile.height as usize],
                    &tile.pixels,
                );
                let texture = ctx.load_texture(
                    format!(
                        "tile-{}-{}-{}-{}-{}",
                        gen, key.page, key.scale_bits, key.col, key.row
                    ),
                    image,
                    egui::TextureOptions::LINEAR,
                );
                tab.doc.tiles.insert(
                    key,
                    CachedTile {
                        scale: tile.scale,
                        x: tile.x,
                        y: tile.y,
                        w: tile.width,
                        h: tile.height,
                        texture,
                    },
                );
                trim_tiles(&mut tab.doc);
            }
            PdfReply::TileMiss {
                gen,
                page,
                scale,
                col,
                row,
            } => {
                if let Some(tab) = self.tab_by_gen_mut(gen) {
                    tab.inflight.remove(&TileKey {
                        page,
                        scale_bits: scale.to_bits(),
                        col,
                        row,
                    });
                }
            }
            PdfReply::Glyphs { gen, page, glyphs } => {
                if let Some(tab) = self.tab_by_gen_mut(gen) {
                    tab.glyphs_waiting.remove(&page);
                    tab.doc.glyphs.insert(page, glyphs);
                }
            }
            PdfReply::Search {
                gen,
                seq,
                hits,
                done,
            } => {
                if let Some(tab) = self.tab_by_gen_mut(gen) {
                    if tab.search.seq == seq {
                        let first = tab.search.hits.is_empty() && !hits.is_empty();
                        tab.search.hits.extend(hits);
                        tab.search.pending = !done;
                        if first {
                            if let Some((page, quads)) = tab.search.hits.first() {
                                let y = quads.first().map(|quad| quad.y0);
                                tab.pending_jump = Some((*page, y));
                            }
                        }
                    }
                }
            }
            PdfReply::Saved { gen, result } => self.on_saved(gen, result),
            PdfReply::PageInserted { gen, index, pages } => {
                self.on_page_inserted(gen, index, pages);
            }
            PdfReply::PageDeleted { gen, index, pages } => {
                self.on_page_deleted(gen, index, pages);
            }
            PdfReply::Failed { gen, message } => {
                if let Some(gen) = gen {
                    self.opening.remove(&gen);
                    if let Some(tab) = self.tab_by_gen_mut(gen) {
                        tab.inflight.clear();
                        tab.search.pending = false;
                        if let Some(op) = tab.page_op.take() {
                            match op {
                                PendingPageOp::Delete { index } => {
                                    tab.redo.pop();
                                    tab.undo.push(UndoEntry::InsertPage { index });
                                }
                                PendingPageOp::Insert {
                                    index,
                                    record_undo,
                                } => {
                                    if !record_undo {
                                        tab.undo.pop();
                                        tab.redo.push(UndoEntry::InsertPage { index });
                                    }
                                }
                            }
                        }
                    }
                }
                self.error = Some(message);
            }
        }
    }

    fn on_page_inserted(&mut self, gen: u64, index: usize, pages: Vec<crate::pdf::PageInfo>) {
        let record_undo = {
            let Some(tab) = self.tab_by_gen_mut(gen) else {
                return;
            };
            let op = tab.page_op.take();
            match op {
                Some(PendingPageOp::Insert { record_undo, .. }) => record_undo,
                None => true,
                Some(PendingPageOp::Delete { .. }) => {
                    tab.page_op = op;
                    return;
                }
            }
        };
        let Some(tab) = self.tab_by_gen_mut(gen) else {
            return;
        };
        tab.doc.session.shift_pages_from(index);
        tab.doc.pages = pages;
        tab.doc.tops = DocState::rebuild_tops(&tab.doc.pages);
        tab.doc.tiles.clear();
        tab.inflight.clear();
        tab.glyphs_waiting.clear();
        let glyphs = std::mem::take(&mut tab.doc.glyphs);
        tab.doc.glyphs.clear();
        for (page, list) in glyphs {
            let page = if page >= index { page + 1 } else { page };
            tab.doc.glyphs.insert(page, list);
        }
        for (page, _) in &mut tab.search.hits {
            if *page >= index {
                *page += 1;
            }
        }
        tab.pending_jump = Some((index, Some(0.0)));
        if record_undo {
            tab.undo.push(UndoEntry::InsertPage { index });
            if tab.undo.len() > 80 {
                tab.undo.remove(0);
            }
            tab.redo.clear();
        }
        self.error = None;
    }

    fn on_page_deleted(&mut self, gen: u64, index: usize, pages: Vec<crate::pdf::PageInfo>) {
        let Some(tab) = self.tab_by_gen_mut(gen) else {
            return;
        };
        match tab.page_op.take() {
            Some(PendingPageOp::Delete { index: expected }) if expected == index => {}
            other => {
                tab.page_op = other;
                return;
            }
        }
        tab.doc.session.unshift_pages_from(index);
        tab.doc.pages = pages;
        tab.doc.tops = DocState::rebuild_tops(&tab.doc.pages);
        tab.doc.tiles.clear();
        tab.inflight.clear();
        tab.glyphs_waiting.clear();
        let glyphs = std::mem::take(&mut tab.doc.glyphs);
        tab.doc.glyphs.clear();
        for (page, list) in glyphs {
            if page == index {
                continue;
            }
            let page = if page > index { page - 1 } else { page };
            tab.doc.glyphs.insert(page, list);
        }
        tab.search.hits.retain(|(page, _)| *page != index);
        for (page, _) in &mut tab.search.hits {
            if *page > index {
                *page -= 1;
            }
        }
        if tab
            .selected
            .is_some_and(|id| tab.doc.session.get(id).is_none())
        {
            tab.selected = None;
            tab.editing = None;
        }
        let jump = index.min(tab.doc.pages.len().saturating_sub(1));
        tab.pending_jump = Some((jump, Some(0.0)));
        self.error = None;
    }

    pub(crate) fn insert_page_after_current(&mut self) {
        let Some(tab) = self.tab() else {
            return;
        };
        if matches!(tab.save, SaveState::Saving) || tab.page_op.is_some() {
            return;
        }
        let gen = tab.doc.gen;
        let after = tab.doc.current_page(self.view_rect.height().max(1.0));
        let at = if tab.doc.pages.is_empty() {
            0
        } else {
            (after + 1).min(tab.doc.pages.len())
        };
        self.seal_undo();
        if let Some(tab) = self.tab_mut() {
            tab.page_op = Some(PendingPageOp::Insert {
                index: at,
                record_undo: true,
            });
        }
        self.worker.insert_page(gen, after);
    }

    fn on_saved(&mut self, gen: u64, result: Result<Vec<crate::pdf::SavedXref>, String>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.doc.gen == gen) else {
            return;
        };
        match result {
            Ok(saved) => {
                let epoch = self.tabs[index].save_epoch;
                let sent = std::mem::take(&mut self.tabs[index].save_deletes);
                let close = {
                    let tab = &mut self.tabs[index];
                    tab.doc
                        .session
                        .pending_deletes
                        .retain(|item| !sent.contains(item));
                    for item in saved {
                        if let Some(annot) = tab.doc.session.get_mut(item.id) {
                            annot.xref = Some(item.xref);
                            if annot.revision <= epoch {
                                annot.dirty = false;
                            }
                        } else {
                            tab.doc.session.pending_deletes.push((item.page, item.xref));
                        }
                    }
                    if tab.doc.session.is_dirty() || tab.force_save {
                        tab.save = SaveState::Dirty {
                            since: Instant::now() - Duration::from_secs(2),
                        };
                    } else {
                        tab.save = SaveState::Clean;
                    }
                    tab.close_after_save && !tab.doc.session.is_dirty()
                };
                if close {
                    self.drop_tab(index);
                }
            }
            Err(message) => {
                self.tabs[index].save = SaveState::Failed {
                    message,
                    at: Instant::now(),
                };
            }
        }
    }

    fn on_math(&mut self, ctx: &egui::Context, render: MathRender) {
        let snapshot = self
            .tabs
            .iter()
            .find(|tab| tab.doc.gen == render.gen)
            .and_then(|tab| {
                let annot = tab.doc.session.get(render.id)?;
                let AnnotKind::Math {
                    source,
                    size,
                    color,
                    auto_size,
                    rect,
                } = &annot.kind
                else {
                    return None;
                };
                Some((source.clone(), *size, *color, *auto_size, *rect))
            });
        let Some((source, size, color, auto_size, rect)) = snapshot else {
            if let Some(tab) = self.tab_by_gen_mut(render.gen) {
                tab.previews.remove(&render.id);
            }
            return;
        };
        {
            let Some(tab) = self.tab_by_gen_mut(render.gen) else {
                return;
            };
            if tab
                .previews
                .get(&render.id)
                .is_some_and(|preview| preview.req != render.req)
            {
                return;
            }
            let texture = render
                .preview
                .as_ref()
                .map(|image| upload_preview(ctx, render.gen, render.id, render.req, image));
            let previous = tab
                .previews
                .get(&render.id)
                .and_then(|preview| preview.texture.clone());
            tab.previews.insert(
                render.id,
                MathPreview {
                    source,
                    size,
                    color,
                    req: render.req,
                    pending: false,
                    texture: texture.or(previous),
                    width_pt: render.width_pt,
                    height_pt: render.height_pt,
                    pdf: render.pdf,
                    error: render.error,
                },
            );
            if auto_size && render.width_pt > 1.0 && render.height_pt > 1.0 {
                let changed = (rect.width() - render.width_pt).abs() > 0.5
                    || (rect.height() - render.height_pt).abs() > 0.5;
                if changed {
                    if let Some(annot) = tab.doc.session.get_mut(render.id) {
                        if let AnnotKind::Math { rect, .. } = &mut annot.kind {
                            rect.x1 = rect.x0 + render.width_pt;
                            rect.y1 = rect.y0 + render.height_pt;
                        }
                    }
                    tab.doc.session.mark_dirty(render.id);
                    if !matches!(tab.save, SaveState::Saving) {
                        tab.save = SaveState::Dirty {
                            since: Instant::now(),
                        };
                    }
                }
            }
        }
        let should_save = self
            .tabs
            .iter()
            .find(|tab| tab.doc.gen == render.gen)
            .is_some_and(|tab| tab.save_when_math_ready && math_ready(tab));
        if should_save {
            self.start_save_gen(render.gen);
        }
    }

    fn flush_math(&mut self) {
        let Some((gen, id, when)) = self.math_deadline else {
            return;
        };
        if when > Instant::now() {
            return;
        }
        self.math_deadline = None;
        self.request_math(gen, id);
    }

    fn prepare_visible_math(&mut self) {
        let (gen, ids) = {
            let Some(tab) = self.tab() else {
                return;
            };
            let gen = tab.doc.gen;
            let ids: Vec<u64> = tab
                .doc
                .session
                .annotations
                .iter()
                .filter_map(|annot| match &annot.kind {
                    AnnotKind::Math { source, .. } if !source.trim().is_empty() => Some(annot.id),
                    _ => None,
                })
                .filter(|id| needs_math(tab, *id))
                .collect();
            (gen, ids)
        };
        for id in ids {
            self.request_math(gen, id);
        }
    }

    fn request_math(&mut self, gen: u64, id: u64) {
        let payload = {
            let Some(tab) = self.tab_by_gen_mut(gen) else {
                return;
            };
            let Some(annot) = tab.doc.session.get(id) else {
                return;
            };
            let AnnotKind::Math {
                source,
                size,
                color,
                ..
            } = &annot.kind
            else {
                return;
            };
            if source.trim().is_empty() {
                return;
            }
            if let Some(preview) = tab.previews.get(&id) {
                if preview.pending
                    && preview.source == *source
                    && (preview.size - *size).abs() < 0.05
                    && preview.color == *color
                {
                    return;
                }
                if !preview.pending
                    && preview.source == *source
                    && (preview.size - *size).abs() < 0.05
                    && preview.color == *color
                    && (preview.texture.is_some() || preview.error.is_some())
                {
                    return;
                }
            }
            let texture = tab
                .previews
                .get(&id)
                .and_then(|preview| preview.texture.clone());
            let width_pt = tab.previews.get(&id).map(|p| p.width_pt).unwrap_or(0.0);
            let height_pt = tab.previews.get(&id).map(|p| p.height_pt).unwrap_or(0.0);
            (source.clone(), *size, *color, texture, width_pt, height_pt)
        };
        let (source, size, color, texture, width_pt, height_pt) = payload;
        self.math_seq += 1;
        let req = self.math_seq;
        if let Some(tab) = self.tab_by_gen_mut(gen) {
            tab.previews.insert(
                id,
                MathPreview {
                    source: source.clone(),
                    size,
                    color,
                    req,
                    pending: true,
                    texture,
                    width_pt,
                    height_pt,
                    pdf: None,
                    error: None,
                },
            );
        }
        self.math.request(gen, id, req, source, size, color);
    }

    fn dispatch_tiles(&mut self) {
        let Some(tab) = self.tab() else {
            return;
        };
        let wanted = view::wanted_tiles(&tab.doc, &tab.inflight, self.tool, self.view_rect);
        let gen = tab.doc.gen;
        let mut glyph_pages = Vec::new();
        let mut tiles = Vec::new();
        for page in wanted.words {
            if let Some(tab) = self.tab_mut() {
                if tab.glyphs_waiting.insert(page) {
                    glyph_pages.push(page);
                }
            }
        }
        for (key, scale) in wanted.tiles {
            if let Some(tab) = self.tab_mut() {
                tab.inflight.insert(key);
            }
            tiles.push((key, scale));
        }
        for (key, scale) in tiles {
            self.worker.tile(gen, key.page, scale, key.col, key.row);
        }
        for page in glyph_pages {
            self.worker.glyphs(gen, page);
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
        let gen = tab.doc.gen;
        let seq = tab.search.seq;
        let query = tab.search.query.clone();
        self.worker.search(gen, seq, query);
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.input(|input| input.key_pressed(Key::S) && input.modifiers.command) {
            if let Some(tab) = self.tab_mut() {
                tab.force_save = true;
            }
        }
        if ctx.input(|input| input.key_pressed(Key::O) && input.modifiers.command) {
            self.open_dialog();
        }
        if ctx.input(|input| input.key_pressed(Key::F) && input.modifiers.command) {
            self.open_search();
        }
        if ctx.input(|input| input.key_pressed(Key::G) && input.modifiers.command) {
            self.page_focus = true;
        }
        if ctx.input(|input| {
            input.key_pressed(Key::Enter)
                && input.modifiers.command
                && input.modifiers.shift
        }) {
            self.insert_page_after_current();
        }
        if ctx.input(|input| input.key_pressed(Key::W) && input.modifiers.command) {
            let active = self.active;
            self.close_tab(active);
        }
        if ctx.input(|input| input.key_pressed(Key::Tab) && input.modifiers.command) {
            if !self.tabs.is_empty() {
                if input_shift(ctx) {
                    self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
                } else {
                    self.active = (self.active + 1) % self.tabs.len();
                }
            }
        }
        for file in ctx.input(|input| input.raw.dropped_files.clone()) {
            if let Some(path) = file.path {
                if path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
                {
                    self.open_path(path);
                }
            }
        }
        if ctx.wants_keyboard_input() {
            if ctx.input(|input| input.key_pressed(Key::Escape)) {
                self.leave_typing();
            }
            return;
        }
        let editing = self.tab().is_some_and(|tab| tab.editing.is_some());
        let paste_image = ctx.input(|input| {
            input.events.iter().any(|event| matches!(event, egui::Event::Paste(_)))
                || (input.modifiers.command
                    && input.modifiers.shift
                    && input.key_pressed(Key::V))
        });
        if paste_image && !editing {
            let _ = self.paste_clipboard_image();
        }
        let mut fit = false;
        let mut zoom = None;
        let mut search_delta = None;
        let mut delete_selected = false;
        let vim = ctx.input(|input| self.handle_vim(input));
        ctx.input(|input| {
            if input.key_pressed(Key::Escape) {
                self.leave_typing();
            }
            if !vim {
                if input.modifiers.command && input.key_pressed(Key::Z) && !input.modifiers.shift {
                    self.undo();
                }
                if input.modifiers.command && input.key_pressed(Key::Y)
                    || (input.modifiers.command
                        && input.modifiers.shift
                        && input.key_pressed(Key::Z))
                {
                    self.redo();
                }
            }
            if input.key_pressed(Key::Slash) {
                self.open_search();
            }
            if !vim && !input.modifiers.command {
                for tool in Tool::ALL {
                    if input.key_pressed(tool.shortcut()) {
                        self.tool = tool;
                    }
                }
            }
            // N / Shift+N step search hits when find is open.
            if input.key_pressed(Key::N) && !input.modifiers.command {
                if self
                    .tab()
                    .is_some_and(|tab| tab.search.open && !tab.search.hits.is_empty())
                {
                    search_delta = Some(if input.modifiers.shift { -1 } else { 1 });
                }
            }
            if input.key_pressed(Key::Delete) || input.key_pressed(Key::Backspace) {
                if self
                    .tab()
                    .is_some_and(|tab| tab.editing.is_none() && tab.selected.is_some())
                {
                    delete_selected = true;
                }
            }
            if input.modifiers.command
                && (input.key_pressed(Key::Equals) || input.key_pressed(Key::Plus))
            {
                zoom = Some(1.1);
            }
            if input.modifiers.command && input.key_pressed(Key::Minus) {
                zoom = Some(1.0 / 1.1);
            }
            if input.modifiers.command && input.key_pressed(Key::Num0) {
                fit = true;
            }
            if input.key_pressed(Key::PageDown) {
                let height = self.view_rect.height();
                let view = self.view_rect;
                if let Some(doc) = self.doc_mut() {
                    doc.scroll_y += height * 0.9;
                    doc.clamp_scroll(view);
                }
            }
            if input.key_pressed(Key::PageUp) {
                let height = self.view_rect.height();
                let view = self.view_rect;
                if let Some(doc) = self.doc_mut() {
                    doc.scroll_y -= height * 0.9;
                    doc.clamp_scroll(view);
                }
            }
        });
        if delete_selected {
            self.delete_selected();
        }
        if search_delta.is_some() {
            if let Some(delta) = search_delta {
                self.search_step(delta);
            }
        }
        if fit {
            self.fit_width();
        }
        if let Some(factor) = zoom {
            self.zoom_by(factor);
        }
    }

    fn autosave(&mut self, ctx: &egui::Context) {
        let gens: Vec<u64> = self.tabs.iter().map(|tab| tab.doc.gen).collect();
        for gen in gens {
            let due = {
                let Some(tab) = self.tab_by_gen_mut(gen) else {
                    continue;
                };
                let dirty = tab.doc.session.is_dirty();
                if !dirty && !tab.force_save {
                    continue;
                }
                ctx.request_repaint_after(Duration::from_millis(200));
                if matches!(tab.save, SaveState::Saving) {
                    continue;
                }
                if let SaveState::Failed { at, .. } = &tab.save {
                    if !tab.force_save && at.elapsed() < Duration::from_secs(3) {
                        continue;
                    }
                }
                tab.force_save
                    || match tab.save {
                        SaveState::Dirty { since } => {
                            since.elapsed() >= Duration::from_millis(1500)
                        }
                        SaveState::Failed { .. } => true,
                        _ => false,
                    }
            };
            if !due {
                continue;
            }
            let ready = self
                .tabs
                .iter()
                .find(|tab| tab.doc.gen == gen)
                .is_some_and(math_ready);
            if ready {
                if let Some(tab) = self.tab_by_gen_mut(gen) {
                    tab.force_save = false;
                }
                self.start_save_gen(gen);
            } else if let Some(tab) = self.tab_by_gen_mut(gen) {
                tab.save_when_math_ready = true;
            }
        }
    }

    fn start_save_gen(&mut self, gen: u64) {
        let (snapshot, close) = {
            let Some(tab) = self.tabs.iter_mut().find(|tab| tab.doc.gen == gen) else {
                return;
            };
            if !tab.doc.session.is_dirty() {
                tab.save = SaveState::Clean;
                tab.force_save = false;
                tab.save_when_math_ready = false;
                (None, tab.close_after_save)
            } else if !math_ready(tab) {
                tab.save_when_math_ready = true;
                (None, false)
            } else {
                let epoch = tab.doc.session.epoch;
                let upserts: Vec<Annotation> = tab
                    .doc
                    .session
                    .annotations
                    .iter()
                    .filter(|annot| annot.dirty)
                    .cloned()
                    .collect();
                let deletes = tab.doc.session.pending_deletes.clone();
                let mut math_pdfs = HashMap::new();
                for annot in &upserts {
                    if let AnnotKind::Math { .. } = annot.kind {
                        if let Some(pdf) = tab
                            .previews
                            .get(&annot.id)
                            .and_then(|preview| preview.pdf.clone())
                        {
                            math_pdfs.insert(annot.id, pdf);
                        }
                    }
                }
                tab.save_epoch = epoch;
                tab.save_deletes = deletes.clone();
                tab.save = SaveState::Saving;
                tab.save_when_math_ready = false;
                tab.force_save = false;
                (
                    Some(SaveSnapshot {
                        upserts,
                        deletes,
                        math_pdfs,
                    }),
                    false,
                )
            }
        };
        if let Some(snapshot) = snapshot {
            self.worker.save(gen, snapshot);
        }
        if close {
            if let Some(index) = self.tabs.iter().position(|tab| tab.doc.gen == gen) {
                self.drop_tab(index);
            }
        }
    }

    pub(crate) fn color_controls(&mut self, ui: &mut egui::Ui) {
        let colors = palette_for(self.tool);
        let current = self.active_color();
        let mut picked = None;
        for color in colors {
            if color_dot(ui, *color, *color == current) {
                picked = Some(*color);
            }
        }
        if let Some(color) = picked {
            self.seal_then_arm();
            self.apply_color(color);
            self.seal_undo();
        }
    }

    pub(crate) fn metric_controls(&mut self, ui: &mut egui::Ui) {
        let show_size = matches!(self.tool, Tool::Text | Tool::Math | Tool::Select)
            || self.selected_is_text_like();
        let show_stroke = matches!(self.tool, Tool::Rect | Tool::Ellipse | Tool::Line)
            || self.selected_is_shape();
        if show_size {
            let mut size = self.active_text_size();
            let response = ui.add(
                DragValue::new(&mut size)
                    .range(6.0..=96.0)
                    .speed(0.2)
                    .suffix(" pt"),
            );
            if response.changed() {
                self.arm_undo();
                self.apply_text_size(size);
            }
        }
        if show_stroke {
            let mut width = self.active_stroke();
            let response = ui.add(
                DragValue::new(&mut width)
                    .range(0.5..=12.0)
                    .speed(0.05)
                    .suffix(" px"),
            );
            if response.changed() {
                self.arm_undo();
                self.apply_stroke(width);
            }
        }
    }

    fn active_color(&self) -> Rgb {
        if let Some(color) = self.selected_color() {
            return color;
        }
        match self.tool {
            Tool::Highlight => self.settings.highlight_color,
            Tool::Rect | Tool::Ellipse | Tool::Line => self.settings.shape_color,
            _ => self.settings.text_color,
        }
    }

    fn selected_color(&self) -> Option<Rgb> {
        let tab = self.tab()?;
        let annot = tab.doc.session.get(tab.selected?)?;
        Some(match &annot.kind {
            AnnotKind::Highlight { color, .. }
            | AnnotKind::Text { color, .. }
            | AnnotKind::Note { color, .. }
            | AnnotKind::Math { color, .. } => *color,
            AnnotKind::Shape { stroke, .. } => *stroke,
            AnnotKind::Image { .. } | AnnotKind::Future(_) => return None,
        })
    }

    fn active_text_size(&self) -> f32 {
        if let Some(tab) = self.tab() {
            if let Some(id) = tab.selected {
                if let Some(annot) = tab.doc.session.get(id) {
                    match &annot.kind {
                        AnnotKind::Text { size, .. } | AnnotKind::Math { size, .. } => {
                            return *size
                        }
                        _ => {}
                    }
                }
            }
        }
        self.settings.text_size
    }

    fn active_stroke(&self) -> f32 {
        if let Some(tab) = self.tab() {
            if let Some(id) = tab.selected {
                if let Some(AnnotKind::Shape { width, .. }) =
                    tab.doc.session.get(id).map(|a| &a.kind)
                {
                    return *width;
                }
            }
        }
        self.settings.shape_width
    }

    fn selected_is_text_like(&self) -> bool {
        matches!(
            self.selected_kind(),
            Some(AnnotKind::Text { .. } | AnnotKind::Math { .. })
        )
    }

    fn selected_is_shape(&self) -> bool {
        matches!(self.selected_kind(), Some(AnnotKind::Shape { .. }))
    }

    fn selected_kind(&self) -> Option<&AnnotKind> {
        let tab = self.tab()?;
        Some(&tab.doc.session.get(tab.selected?)?.kind)
    }

    fn apply_color(&mut self, color: Rgb) {
        let mut kind_bucket = None;
        let mut math_id = None;
        if let Some(tab) = self.tab_mut() {
            if let Some(id) = tab.selected {
                if let Some(annot) = tab.doc.session.get_mut(id) {
                    match &mut annot.kind {
                        AnnotKind::Highlight { color: slot, .. } => {
                            *slot = color;
                            kind_bucket = Some(0);
                        }
                        AnnotKind::Text { color: slot, .. }
                        | AnnotKind::Note { color: slot, .. }
                        | AnnotKind::Math { color: slot, .. } => {
                            *slot = color;
                            kind_bucket = Some(1);
                        }
                        AnnotKind::Shape { stroke, .. } => {
                            *stroke = color;
                            kind_bucket = Some(2);
                        }
                        AnnotKind::Image { .. } | AnnotKind::Future(_) => {}
                    }
                }
                if kind_bucket.is_some() {
                    tab.doc.session.mark_dirty(id);
                    if !matches!(tab.save, SaveState::Saving) {
                        tab.save = SaveState::Dirty {
                            since: Instant::now(),
                        };
                    }
                    math_id = Some(id);
                }
            }
        }
        if let Some(id) = math_id {
            self.queue_math(id);
        }
        match kind_bucket.unwrap_or(match self.tool {
            Tool::Highlight => 0,
            Tool::Rect | Tool::Ellipse | Tool::Line => 2,
            _ => 1,
        }) {
            0 => self.settings.highlight_color = color,
            2 => self.settings.shape_color = color,
            _ => self.settings.text_color = color,
        }
        self.settings.save();
    }

    fn apply_text_size(&mut self, size: f32) {
        let size = size.clamp(6.0, 96.0);
        let mut math_id = None;
        if let Some(tab) = self.tab_mut() {
            if let Some(id) = tab.selected {
                let mut changed = false;
                if let Some(annot) = tab.doc.session.get_mut(id) {
                    match &mut annot.kind {
                        AnnotKind::Text { size: slot, .. } | AnnotKind::Math { size: slot, .. } => {
                            *slot = size;
                            changed = true;
                        }
                        _ => {}
                    }
                }
                if changed {
                    tab.doc.session.mark_dirty(id);
                    if !matches!(tab.save, SaveState::Saving) {
                        tab.save = SaveState::Dirty {
                            since: Instant::now(),
                        };
                    }
                    math_id = Some(id);
                }
            }
        }
        if let Some(id) = math_id {
            self.queue_math(id);
        }
        self.settings.text_size = size;
        self.settings.save();
    }

    fn apply_stroke(&mut self, width: f32) {
        let width = width.clamp(0.25, 16.0);
        if let Some(tab) = self.tab_mut() {
            if let Some(id) = tab.selected {
                let mut changed = false;
                if let Some(annot) = tab.doc.session.get_mut(id) {
                    if let AnnotKind::Shape { width: slot, .. } = &mut annot.kind {
                        *slot = width;
                        changed = true;
                    }
                }
                if changed {
                    tab.doc.session.mark_dirty(id);
                    if !matches!(tab.save, SaveState::Saving) {
                        tab.save = SaveState::Dirty {
                            since: Instant::now(),
                        };
                    }
                }
            }
        }
        self.settings.shape_width = width;
        self.settings.save();
    }

    fn set_title(&self, ctx: &egui::Context) {
        let title = match self.tab() {
            Some(tab) => {
                let name = tab
                    .doc
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("document.pdf");
                let dirty = if tab.doc.session.is_dirty() {
                    " •"
                } else {
                    ""
                };
                format!("Marker — {name}{dirty}")
            }
            None => "Marker".into(),
        };
        ctx.send_viewport_cmd(ViewportCommand::Title(title));
    }
}

fn math_ready(tab: &Tab) -> bool {
    tab.doc.session.annotations.iter().all(|annot| {
        if !annot.dirty {
            return true;
        }
        let AnnotKind::Math { source, .. } = &annot.kind else {
            return true;
        };
        if crate::geom::normalize_latex(source).is_empty() {
            return true;
        }
        match tab.previews.get(&annot.id) {
            Some(preview)
                if !preview.pending && (preview.pdf.is_some() || preview.error.is_some()) =>
            {
                true
            }
            _ => false,
        }
    })
}

fn needs_math(tab: &Tab, id: u64) -> bool {
    let Some(annot) = tab.doc.session.get(id) else {
        return false;
    };
    let AnnotKind::Math {
        source,
        size,
        color,
        ..
    } = &annot.kind
    else {
        return false;
    };
    if source.trim().is_empty() {
        return false;
    }
    match tab.previews.get(&id) {
        Some(preview) => {
            preview.pending
                || preview.source != *source
                || (preview.size - *size).abs() > 0.05
                || preview.color != *color
        }
        None => true,
    }
}

fn tile_key(tile: &crate::pdf::TileImage) -> TileKey {
    TileKey {
        page: tile.page,
        scale_bits: tile.scale.to_bits(),
        col: tile.col,
        row: tile.row,
    }
}

fn trim_tiles(doc: &mut DocState) {
    while doc.tiles.len() > 180 {
        let Some(key) = doc.tiles.keys().next().copied() else {
            break;
        };
        doc.tiles.remove(&key);
    }
}

fn upload_preview(
    ctx: &egui::Context,
    gen: u64,
    id: u64,
    req: u64,
    image: &RgbaImage,
) -> egui::TextureHandle {
    let pixels = egui::ColorImage::from_rgba_premultiplied(
        [image.width as usize, image.height as usize],
        &image.pixels,
    );
    ctx.load_texture(
        format!("math-{gen}-{id}-{req}"),
        pixels,
        egui::TextureOptions::LINEAR,
    )
}

fn input_shift(ctx: &egui::Context) -> bool {
    ctx.input(|input| input.modifiers.shift)
}
