use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use egui::{DragValue, Key, ViewportCommand};

use crate::annot::{AnnotKind, Annotation, Glyph, Handle, Session, ShapeKind};
use crate::assistant::{
    AssistantAttachment, AssistantEvent, AssistantRequest, AssistantRole, AssistantTurn,
    AssistantWorker, BundleImageAttach, BundleInput, BundleTextAttach, CaptureMode, PendingCrop,
    TabAssistant, CROP_DPI, MAX_TEXT_CHARS,
};
use crate::geom::{PdfPoint, PdfRect, Rgb};
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
    /// Second pane rect when a split is open.
    pub(crate) split_view_rect: egui::Rect,
    /// Side-by-side or stacked second document pane.
    pub(crate) split: Option<SplitState>,
    /// Right-click menu on a tab pill: (tab index, screen pos).
    pub(crate) tab_menu: Option<(usize, egui::Pos2)>,
    /// Tab being dragged for split / pane assignment.
    pub(crate) tab_drag: Option<usize>,
    /// Ephemeral or pinned open-tabs overlay (top-right).
    pub(crate) tab_list: Option<TabListState>,
    pub(crate) opening: HashSet<u64>,
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
    vim_count: u32,
    vim_g: bool,
    worker: PdfWorker,
    math: MathWorker,
    assistant: AssistantWorker,
    math_seq: u64,
    math_deadline: Option<(u64, u64, Instant)>,
    next_gen: u64,
    dialog_tx: Sender<Option<PathBuf>>,
    dialog_rx: Receiver<Option<PathBuf>>,
    dialog_busy: bool,
    /// Cloned into worker/dialog threads so they can wake the UI on completion.
    egui_ctx: egui::Context,
}

/// Two-pane document layout. `first` is left/top; `second` is right/bottom.
/// Focus is `MarkerApp::active`, which must be one of the two pane tabs.
#[derive(Clone, Copy)]
pub(crate) struct SplitState {
    pub first: usize,
    pub second: usize,
    pub stacked: bool,
    pub ratio: f32,
}

impl SplitState {
    pub(crate) fn contains(self, index: usize) -> bool {
        self.first == index || self.second == index
    }

    pub(crate) fn other(self, active: usize) -> usize {
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
}

pub(crate) struct Tab {
    pub(crate) doc: DocState,
    /// Selected annotation ids (order = primary first for style/edit).
    pub(crate) selected: Vec<u64>,
    /// Ephemeral page-text selection for copy (Select tool).
    pub(crate) text_sel: Option<TextSel>,
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
    pub(crate) assistant: TabAssistant,
    pub(crate) search: SearchState,
    pub(crate) last_hl: Option<(Instant, usize, u32, Option<u64>)>,
    pub(crate) focus_edit: bool,
    pub(crate) menu: Option<ContextMenu>,
    /// Compact color/size strip next to an annotation (right-click or just after create).
    pub(crate) style_bar: Option<StyleBar>,
    pending_undo: Option<Session>,
    undo_edit: Option<u64>,
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    /// In-flight page insert/delete that participates in undo/redo.
    page_op: Option<PendingPageOp>,
}

/// Page-space glyph range selected with the Select tool (for copy, etc.).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextSel {
    pub page: usize,
    pub glyph_lo: usize,
    pub glyph_hi: usize,
}

/// Right-click page menu snapshot.
#[derive(Clone, Debug)]
pub(crate) struct ContextMenu {
    pub pos: egui::Pos2,
    pub page: usize,
    pub point: PdfPoint,
    pub hit: Option<u64>,
}

impl Tab {
    pub(crate) fn select_only(&mut self, id: u64) {
        self.selected.clear();
        self.selected.push(id);
        self.text_sel = None;
    }

    pub(crate) fn select_many(&mut self, ids: Vec<u64>) {
        self.selected = ids;
        self.text_sel = None;
    }

    pub(crate) fn primary_selected(&self) -> Option<u64> {
        self.selected.first().copied()
    }

    pub(crate) fn is_selected(&self, id: u64) -> bool {
        self.selected.contains(&id)
    }
}

/// Floating style controls for one annotation.
#[derive(Clone, Copy)]
pub(crate) struct StyleBar {
    pub(crate) id: u64,
    /// Auto-hide deadline after create; `None` means stay until dismissed.
    pub(crate) until: Option<Instant>,
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
    /// Last pan/scroll; keeps the interactive frame budget warm between wheel samples.
    pub(crate) last_scroll: Instant,
    /// Last Fit action; cleared when zoom changes by other means.
    pub(crate) last_fit: Option<FitKind>,
    pub(crate) glyphs: HashMap<usize, Vec<Glyph>>,
    pub(crate) tiles: HashMap<TileKey, CachedTile>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FitKind {
    Width,
    Height,
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

    /// Bare letter that selects this tool. Vim pans with `J`/`K`/`L` (not `H` — Highlight).
    pub(crate) fn shortcut(self) -> Key {
        match self {
            Tool::Select => Key::S,
            Tool::Highlight => Key::H,
            Tool::Text => Key::T,
            Tool::Rect => Key::R,
            Tool::Ellipse => Key::E,
            Tool::Line => Key::I,
            Tool::Math => Key::M,
        }
    }

    pub(crate) fn hint(self) -> &'static str {
        match self {
            Tool::Select => "Select text or objects, marquee, move, copy",
            Tool::Highlight => "Mark text",
            Tool::Text => "Write on the page",
            Tool::Rect => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Line => "Line",
            Tool::Math => "Equation",
        }
    }
}

/// Which selected annotation kinds a color pick should update.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorTarget {
    Highlights,
    /// Text / note / math fill and shape stroke (images ignored).
    Ink,
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
    Region {
        page: usize,
        origin: PdfPoint,
        current: PdfPoint,
    },
    LearningSelect {
        page: usize,
        anchor: Option<usize>,
        current: Option<usize>,
        origin: PdfPoint,
        current_pt: PdfPoint,
    },
    /// Select-tool text drag (copyable page text; not an annotation).
    TextSelect {
        page: usize,
        anchor: Option<usize>,
        current: Option<usize>,
        origin: PdfPoint,
        current_pt: PdfPoint,
    },
    /// Select-tool area drag — bulk-select by annotation center.
    Marquee {
        page: usize,
        origin: PdfPoint,
        current: PdfPoint,
    },
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
        /// Ids being translated (primary first).
        ids: Vec<u64>,
        origins: Vec<(u64, AnnotKind)>,
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
        let egui_ctx = cc.egui_ctx.clone();
        let mut app = Self {
            settings: Settings::load(),
            tool: Tool::Select,
            tabs: Vec::new(),
            active: 0,
            view_rect: egui::Rect::NOTHING,
            split_view_rect: egui::Rect::NOTHING,
            split: None,
            tab_menu: None,
            tab_drag: None,
            tab_list: None,
            opening: HashSet::new(),
            error: None,
            page_focus: false,
            zen: false,
            zen_chrome_until: None,
            assistant_open: false,
            capture: CaptureMode::None,
            vim_count: 0,
            vim_g: false,
            worker: PdfWorker::spawn(egui_ctx.clone()),
            math: MathWorker::spawn(egui_ctx.clone()),
            assistant: AssistantWorker::spawn(egui_ctx.clone()),
            math_seq: 1,
            math_deadline: None,
            next_gen: 1,
            dialog_tx,
            dialog_rx,
            dialog_busy: false,
            egui_ctx,
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
        let ctx = self.egui_ctx.clone();
        thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .add_filter("PDF", &["pdf"])
                .pick_file();
            let _ = tx.send(path);
            ctx.request_repaint();
        });
    }

    pub(crate) fn open_path(&mut self, path: PathBuf) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.doc.path == path) {
            if self.active != index {
                self.active = index;
                self.pulse_tab_list();
            }
            self.settings.remember_open(&path);
            return;
        }
        self.next_gen += 1;
        let gen = self.next_gen;
        self.opening.insert(gen);
        self.error = None;
        self.worker.open(gen, path);
    }

    /// Briefly show the open-tabs list (e.g. after Ctrl+Tab). No-op while pinned.
    pub(crate) fn pulse_tab_list(&mut self) {
        if self.tabs.len() < 2 {
            return;
        }
        if matches!(self.tab_list, Some(TabListState::Pinned)) {
            return;
        }
        self.tab_list = Some(TabListState::Ephemeral {
            until: Instant::now() + Duration::from_millis(2500),
        });
    }

    /// Toggle the pinned open-tabs list (title / indicator click).
    pub(crate) fn toggle_tab_list(&mut self) {
        if self.tabs.is_empty() {
            self.tab_list = None;
            return;
        }
        if self.tab_list.is_some() {
            self.tab_list = None;
        } else {
            self.tab_list = Some(TabListState::Pinned);
        }
    }

    pub(crate) fn dismiss_tab_list(&mut self) {
        self.tab_list = None;
    }

    /// Activate a tab from the switcher / list, respecting split pane assignment.
    pub(crate) fn activate_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if let Some(split) = self.split {
            if split.contains(index) {
                self.active = index;
            } else {
                let focus_first = self.active == split.first;
                if let Some(s) = self.split.as_mut() {
                    if focus_first {
                        s.first = index;
                    } else {
                        s.second = index;
                    }
                }
                self.active = index;
            }
        } else {
            self.active = index;
        }
        self.pulse_tab_list();
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
        if tab.assistant.streaming {
            self.assistant
                .cancel(tab.doc.gen, tab.assistant.request_seq);
        }
        self.worker.close(tab.doc.gen);
        if let Some(drag) = self.tab_drag {
            if drag == index {
                self.tab_drag = None;
            } else if drag > index {
                self.tab_drag = Some(drag - 1);
            }
        }
        if let Some((menu_index, pos)) = self.tab_menu {
            if menu_index == index {
                self.tab_menu = None;
            } else if menu_index > index {
                self.tab_menu = Some((menu_index - 1, pos));
            }
        }
        if self.tabs.is_empty() {
            self.active = 0;
            self.split = None;
            self.tab_drag = None;
            self.tab_menu = None;
            self.tab_list = None;
            return;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if self.active > index {
            self.active -= 1;
        }
        if let Some(split) = self.split {
            if split.first == index || split.second == index {
                let survivor = if split.first == index {
                    split.second
                } else {
                    split.first
                };
                let survivor = if survivor > index {
                    survivor - 1
                } else {
                    survivor
                };
                self.split = None;
                if survivor < self.tabs.len() {
                    self.active = survivor;
                }
            } else {
                let mut split = split;
                if split.first > index {
                    split.first -= 1;
                }
                if split.second > index {
                    split.second -= 1;
                }
                self.split = Some(split);
            }
        }
    }

    pub(crate) fn split_with(&mut self, other: usize, stacked: bool) {
        if other >= self.tabs.len() || other == self.active || self.tabs.len() < 2 {
            return;
        }
        let ratio = self.split.map(|s| s.ratio).unwrap_or(0.5);
        self.split = Some(SplitState {
            first: self.active,
            second: other,
            stacked,
            ratio,
        });
    }

    /// Split using `index` as the other pane, or the next tab when `index` is active.
    pub(crate) fn split_from_tab(&mut self, index: usize, stacked: bool) {
        if self.tabs.len() < 2 || index >= self.tabs.len() {
            return;
        }
        let other = if index == self.active {
            (self.active + 1) % self.tabs.len()
        } else {
            index
        };
        self.split_with(other, stacked);
    }

    /// Toggle side-by-side / stacked split with the next tab; same layout again unsplits.
    pub(crate) fn toggle_split(&mut self, stacked: bool) {
        if self.tabs.len() < 2 {
            return;
        }
        if let Some(split) = self.split {
            if split.stacked == stacked {
                self.unsplit();
            } else if let Some(s) = self.split.as_mut() {
                s.stacked = stacked;
            }
        } else {
            let other = (self.active + 1) % self.tabs.len();
            self.split_with(other, stacked);
        }
    }

    fn set_split_panes(&mut self, first: usize, second: usize, stacked: bool) {
        if first == second || first >= self.tabs.len() || second >= self.tabs.len() {
            return;
        }
        let ratio = self.split.map(|s| s.ratio).unwrap_or(0.5);
        self.split = Some(SplitState {
            first,
            second,
            stacked,
            ratio,
        });
        if self.active != first && self.active != second {
            self.active = first;
        }
    }

    /// Place `dragged` into a new or existing split according to an edge / pane drop.
    pub(crate) fn apply_tab_drop(&mut self, dragged: usize, zone: SplitDropZone) {
        if self.tabs.len() < 2 || dragged >= self.tabs.len() {
            return;
        }
        match zone {
            SplitDropZone::Left | SplitDropZone::Top => {
                let stacked = matches!(zone, SplitDropZone::Top);
                if dragged == self.active {
                    let other = (self.active + 1) % self.tabs.len();
                    self.set_split_panes(dragged, other, stacked);
                } else {
                    let prev = self.active;
                    self.active = dragged;
                    self.set_split_panes(dragged, prev, stacked);
                }
            }
            SplitDropZone::Right | SplitDropZone::Bottom => {
                let stacked = matches!(zone, SplitDropZone::Bottom);
                if dragged == self.active {
                    let next = (self.active + 1) % self.tabs.len();
                    self.set_split_panes(next, dragged, stacked);
                    self.active = next;
                } else {
                    self.set_split_panes(self.active, dragged, stacked);
                }
            }
            SplitDropZone::OtherPane => {
                let Some(split) = self.split else {
                    return;
                };
                let unfocused = split.other(self.active);
                if dragged == self.active {
                    // Swap pane contents so the focused doc moves to the other side.
                    self.split = Some(SplitState {
                        first: split.second,
                        second: split.first,
                        stacked: split.stacked,
                        ratio: 1.0 - split.ratio,
                    });
                } else if dragged != unfocused {
                    if let Some(s) = self.split.as_mut() {
                        if s.first == unfocused {
                            s.first = dragged;
                        } else {
                            s.second = dragged;
                        }
                    }
                }
            }
            SplitDropZone::ActivePane => {
                let Some(split) = self.split else {
                    return;
                };
                if dragged == self.active {
                    return;
                }
                if dragged == split.other(self.active) {
                    self.active = dragged;
                } else if let Some(s) = self.split.as_mut() {
                    if s.first == self.active {
                        s.first = dragged;
                    } else {
                        s.second = dragged;
                    }
                    self.active = dragged;
                }
            }
        }
    }

    pub(crate) fn unsplit(&mut self) {
        self.split = None;
    }

    pub(crate) fn focus_split_other(&mut self) {
        let Some(split) = self.split else {
            return;
        };
        self.active = split.other(self.active);
        self.pulse_tab_list();
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
            let target = (percent / 100.0 * crate::geom::ZOOM_100).clamp(
                crate::geom::MIN_SCALE,
                crate::geom::MAX_SCALE,
            );
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
                tab.style_bar = None;
                tab.selected
                    .retain(|id| tab.doc.session.get(*id).is_some());
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
                tab.style_bar = None;
                tab.selected
                    .retain(|id| tab.doc.session.get(*id).is_some());
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
                AnnotKind::Highlight { quads, .. } => {
                    let glyphs = tab.doc.glyphs.get(&annot.page)?;
                    let indices = crate::assistant::glyphs_intersecting_rects(glyphs, quads);
                    let (&lo, &hi) = (indices.first()?, indices.last()?);
                    let text = crate::assistant::reconstruct_text(glyphs, lo, hi);
                    if !text.trim().is_empty() {
                        return Some(text);
                    }
                }
                AnnotKind::Text { content, .. } | AnnotKind::Note { content, .. } => {
                    if !content.is_empty() {
                        return Some(content.clone());
                    }
                }
                _ => {}
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
                tab.doc.session.remove(*id);
                tab.previews.remove(id);
                tab.image_textures.remove(id);
            }
            tab.selected.clear();
            tab.menu = None;
            tab.style_bar = None;
            tab.editing = None;
        }
        self.seal_undo();
    }

    /// Paste an image from the system clipboard onto the current page.
    pub(crate) fn paste_clipboard_image(&mut self) -> bool {
        let Some((width, height, rgba)) = read_clipboard_rgba() else {
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
        // `H` is Highlight; vim horizontal pan uses `L` for right only (left: Shift+L via count).
        if input.key_pressed(Key::L) && !input.modifiers.command {
            let n = self.take_count();
            self.vim_g = false;
            if input.modifiers.shift {
                self.nudge(-step * n, 0.0);
            } else {
                self.nudge(step * n, 0.0);
            }
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
        let mut exit_zen = false;
        let mut seal = false;
        if self.capture != CaptureMode::None {
            self.cancel_capture();
            return;
        }
        if self.tab_list.take().is_some() {
            return;
        }
        if self.tab_menu.take().is_some() {
            return;
        }
        if self.tab_drag.take().is_some() {
            return;
        }
        if let Some(tab) = self.tab_mut() {
            if tab.menu.take().is_some() {
                return;
            }
            if tab.style_bar.take().is_some() {
                // Dismiss the style strip first; a second Escape clears selection.
                return;
            }
            if tab.editing.take().is_some() {
                tab.focus_edit = false;
                seal = true;
            } else if tab.assistant.learning.take().is_some()
                || !tab.selected.is_empty()
                || tab.text_sel.take().is_some()
            {
                tab.selected.clear();
                tab.editing = None;
                // Cleared learning selection and/or annotation/text selection.
            } else if tab.search.open {
                tab.search.open = false;
                tab.search.hits.clear();
                tab.search.query.clear();
                tab.search.last_sent.clear();
            } else {
                exit_zen = self.zen;
            }
        } else {
            exit_zen = self.zen;
        }
        if seal {
            self.end_edit_undo();
        }
        if exit_zen {
            self.zen = false;
            self.zen_chrome_until = None;
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

    /// Apply pending fullscreen exit from Escape (needs a ctx for the viewport command).
    fn sync_zen_viewport(&mut self, ctx: &egui::Context) {
        let fullscreen = ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
        if self.zen != fullscreen {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(self.zen));
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

        // Soft frame budget while interacting (~60fps). Background work wakes the
        // UI via egui::Context::request_repaint from worker threads; a slow safety
        // net covers a missed wake without burning CPU.
        //
        // With vsync off (Hyprland hidden-workspace workaround), Wayland/OpenGL
        // may deliver uncapped RedrawRequested. Pace idle frames so that path
        // cannot spin the CPU when nothing is changing.
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
            || self.math_deadline.is_some()
            || self.tabs.iter().any(|tab| {
                matches!(tab.save, SaveState::Saving)
                    || !tab.inflight.is_empty()
                    || tab.search.pending
                    || tab.assistant.streaming
                    || tab.assistant.pending_crop.is_some()
            });
        if interacting {
            let focused = ctx.input(|input| input.focused);
            let wait = if focused { 16 } else { 33 };
            ctx.request_repaint_after(Duration::from_millis(wait));
        } else if let Some((_, _, when)) = self.math_deadline {
            ctx.request_repaint_after(when.saturating_duration_since(Instant::now()));
        } else if pending {
            ctx.request_repaint_after(Duration::from_millis(500));
        } else {
            thread::sleep(Duration::from_millis(100));
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
        for event in self.assistant.poll() {
            self.on_assistant(ctx, event);
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
                            if self.active != index {
                                self.active = index;
                                self.pulse_tab_list();
                            }
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
                            last_scroll: Instant::now(),
                            last_fit: None,
                            glyphs: HashMap::new(),
                            tiles: HashMap::new(),
                        },
                        selected: Vec::new(),
                        text_sel: None,
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
                        assistant: TabAssistant::default(),
                        search: SearchState::default(),
                        last_hl: None,
                        focus_edit: false,
                        menu: None,
                        style_bar: None,
                        pending_undo: None,
                        undo_edit: None,
                        undo: Vec::new(),
                        redo: Vec::new(),
                        page_op: None,
                    });
                    self.active = self.tabs.len() - 1;
                    if self.tabs.len() > 1 {
                        self.pulse_tab_list();
                    }
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
                    // Linear at ~1:1 (HiDPI) keeps glyph AA smooth; nearest looked crunchy.
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
            PdfReply::Crop { gen, seq, result } => self.on_crop(ctx, gen, seq, result),
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
        tab.selected
            .retain(|id| tab.doc.session.get(*id).is_some());
        if tab.selected.is_empty() {
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

    pub(crate) fn dispatch_tiles(&mut self, pixels_per_point: f32) {
        self.dispatch_tiles_at(self.active, self.view_rect, pixels_per_point);
        if let Some(split) = self.split {
            self.dispatch_tiles_at(
                split.other(self.active),
                self.split_view_rect,
                pixels_per_point,
            );
        }
    }

    fn dispatch_tiles_at(&mut self, index: usize, view: egui::Rect, pixels_per_point: f32) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let wanted =
            view::wanted_tiles(&tab.doc, &tab.inflight, self.tool, view, pixels_per_point);
        let gen = tab.doc.gen;
        let mut glyph_pages = Vec::new();
        let mut tiles = Vec::new();
        for page in wanted.words {
            if let Some(tab) = self.tabs.get_mut(index) {
                if tab.glyphs_waiting.insert(page) {
                    glyph_pages.push(page);
                }
            }
        }
        for (key, scale) in wanted.tiles {
            if let Some(tab) = self.tabs.get_mut(index) {
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
        if ctx.input(|input| {
            input.key_pressed(Key::B) && input.modifiers.command && input.modifiers.shift
        }) {
            self.toggle_toolbar();
        }
        if ctx.input(|input| input.key_pressed(Key::F11)) {
            self.toggle_zen(ctx);
        }
        // Assistant: Ctrl/Cmd+Alt+I toggle, Shift+A attach text, Alt+S region, Alt+E explain, . stop
        if ctx.input(|input| {
            input.key_pressed(Key::I) && input.modifiers.command && input.modifiers.alt
        }) {
            self.toggle_assistant();
        }
        if ctx.input(|input| {
            input.key_pressed(Key::A) && input.modifiers.command && input.modifiers.shift
        }) {
            if self.tab().is_some_and(|t| t.assistant.learning.is_some()) {
                self.attach_learning_text();
            } else {
                self.begin_learning_select();
            }
        }
        if ctx.input(|input| {
            input.key_pressed(Key::S) && input.modifiers.command && input.modifiers.alt
        }) {
            self.begin_region_capture();
        }
        if ctx.input(|input| {
            input.key_pressed(Key::E) && input.modifiers.command && input.modifiers.alt
        }) {
            self.explain_selection();
        }
        if ctx.input(|input| {
            input.key_pressed(Key::Period) && input.modifiers.command
        }) {
            self.assistant_stop();
        }
        if ctx.input(|input| input.key_pressed(Key::Tab) && input.modifiers.command) {
            if self.tabs.len() >= 2 {
                if input_shift(ctx) {
                    self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
                } else {
                    self.active = (self.active + 1) % self.tabs.len();
                }
                self.pulse_tab_list();
            }
        }
        if ctx.input(|input| {
            input.key_pressed(Key::Backslash) && input.modifiers.command && !input.modifiers.alt
        }) {
            self.toggle_split(input_shift(ctx));
        }
        if ctx.input(|input| {
            input.key_pressed(Key::Backslash)
                && input.modifiers.command
                && input.modifiers.alt
        }) {
            self.focus_split_other();
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
                if self.capture != CaptureMode::None {
                    self.cancel_capture();
                } else {
                    self.leave_typing();
                    self.sync_zen_viewport(ctx);
                }
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
        let want_copy = ctx.input(|input| {
            input.events.iter().any(|event| matches!(event, egui::Event::Copy))
                || (input.modifiers.command
                    && !input.modifiers.shift
                    && input.key_pressed(Key::C))
        });
        if want_copy && !editing {
            if let Some(text) = self.copyable_selection_text() {
                ctx.copy_text(text);
            }
        }
        let mut fit = false;
        let mut zoom = None;
        let mut search_delta = None;
        let mut delete_selected = false;
        let vim = ctx.input(|input| self.handle_vim(input));
        let mut escaped = false;
        ctx.input(|input| {
            if input.key_pressed(Key::Escape) {
                escaped = true;
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
                    .is_some_and(|tab| tab.editing.is_none() && !tab.selected.is_empty())
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
        if escaped {
            if self.capture != CaptureMode::None {
                self.cancel_capture();
            } else {
                self.leave_typing();
                self.sync_zen_viewport(ctx);
            }
        }
        if delete_selected {
            self.delete_selected();
        }
        if search_delta.is_some() {
            if let Some(delta) = search_delta {
                self.search_step(delta);
            }
        }
        if fit {
            self.fit_toggle();
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
        let has_sel = self.tab().is_some_and(|tab| !tab.selected.is_empty());
        let (show_hl, show_ink, _, _) = self.selection_style_flags();
        // Select tool: palette follows the selection (highlight vs ink); images alone hide it.
        let (colors, target) = if self.tool == Tool::Select && has_sel {
            if show_hl && !show_ink {
                (
                    &crate::geom::HIGHLIGHT_COLORS[..],
                    ColorTarget::Highlights,
                )
            } else if show_ink {
                // Mixed or ink-only: toolbar shows ink / text color.
                (&crate::geom::INK_COLORS[..], ColorTarget::Ink)
            } else {
                return;
            }
        } else if self.tool == Tool::Highlight {
            (
                palette_for(self.tool),
                ColorTarget::Highlights,
            )
        } else {
            (palette_for(self.tool), ColorTarget::Ink)
        };
        let current = match target {
            ColorTarget::Highlights => self
                .selected_highlight_color()
                .unwrap_or(self.settings.highlight_color),
            ColorTarget::Ink => self
                .selected_ink_color()
                .unwrap_or(self.active_color()),
        };
        let mut picked = None;
        for color in colors {
            if color_dot(ui, *color, *color == current) {
                picked = Some(*color);
            }
        }
        if let Some(color) = picked {
            self.seal_then_arm();
            self.apply_color_to(color, target);
            self.seal_undo();
        }
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

    pub(crate) fn toggle_assistant(&mut self) {
        self.assistant_open = !self.assistant_open;
        if !self.assistant_open {
            self.capture = CaptureMode::None;
        }
    }

    pub(crate) fn assistant_new_chat(&mut self) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        if tab.assistant.streaming {
            let gen = tab.doc.gen;
            let seq = tab.assistant.request_seq;
            self.assistant.cancel(gen, seq);
        }
        if let Some(tab) = self.tab_mut() {
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
        let gen = tab.doc.gen;
        let seq = tab.assistant.request_seq;
        self.assistant.cancel(gen, seq);
    }

    pub(crate) fn begin_learning_select(&mut self) {
        self.assistant_open = true;
        self.capture = CaptureMode::LearningText;
        if let Some(tab) = self.tab_mut() {
            tab.drag = None;
        }
    }

    pub(crate) fn begin_region_capture(&mut self) {
        self.assistant_open = true;
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
        self.assistant_open = true;
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
        self.assistant_open = true;
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
        self.assistant_open = true;
        self.attach_learning_text();
        if let Some(tab) = self.tab_mut() {
            if tab.assistant.draft.trim().is_empty() {
                tab.assistant.draft = "Explain this selection clearly for a learner.".into();
            }
        }
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
        let gen = tab.doc.gen;
        tab.assistant.pending_crop = Some(PendingCrop {
            gen,
            seq,
            page,
            rect,
        });
        tab.assistant.status_line = Some("Rendering screenshot…".into());
        self.worker.crop(gen, seq, page, rect, CROP_DPI);
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
        let gen = tab.doc.gen;
        let seq = tab.assistant.bump_seq();
        let chat_id = tab.assistant.chat_id.clone();
        tab.assistant.streaming = true;
        tab.assistant.error = None;
        tab.assistant.status_line = Some("Talking to Cursor…".into());
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
            gen,
            seq,
            chat_id,
            bundle: BundleInput {
                question,
                filename,
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

    fn on_crop(
        &mut self,
        ctx: &egui::Context,
        gen: u64,
        seq: u64,
        result: Result<crate::pdf::CropImage, String>,
    ) {
        let Some(tab) = self.tab_by_gen_mut(gen) else {
            return;
        };
        let pending = tab.assistant.pending_crop.take();
        if pending.as_ref().is_none_or(|p| p.seq != seq) {
            return;
        }
        tab.assistant.status_line = None;
        match result {
            Ok(crop) => {
                let texture = upload_png_texture(ctx, gen, seq, &crop.png, crop.width, crop.height);
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

    fn on_assistant(&mut self, _ctx: &egui::Context, event: AssistantEvent) {
        let (gen, seq) = match &event {
            AssistantEvent::Started { gen, seq, .. }
            | AssistantEvent::Delta { gen, seq, .. }
            | AssistantEvent::Completed { gen, seq, .. }
            | AssistantEvent::Cancelled { gen, seq }
            | AssistantEvent::Failed { gen, seq, .. }
            | AssistantEvent::AuthRequired { gen, seq, .. } => (*gen, *seq),
        };
        let Some(tab) = self.tab_by_gen_mut(gen) else {
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
                if let Some(turn) = tab
                    .assistant
                    .turns
                    .iter_mut()
                    .rev()
                    .find(|t| t.role == AssistantRole::Assistant)
                {
                    if turn.text.is_empty() {
                        turn.text = text;
                    } else if !text.is_empty() && text.len() >= turn.text.len() {
                        turn.text = text;
                    }
                    turn.incomplete = false;
                }
                tab.assistant.streaming = false;
                tab.assistant.status_line = None;
            }
            AssistantEvent::Cancelled { .. } => {
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

    /// Whether tab/tool chrome should draw this frame (zen auto-hides until top-edge hover).
    pub(crate) fn chrome_visible(&mut self, ctx: &egui::Context) -> bool {
        if !self.zen {
            return true;
        }
        let top_hover = ctx.input(|input| {
            input.pointer.hover_pos().is_some_and(|pos| pos.y <= 10.0)
        });
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

    /// Compact icon row for the floating annotation style strip. Returns true if Delete was chosen.
    pub(crate) fn style_bar_contents(&mut self, ui: &mut egui::Ui) -> bool {
        let (show_hl, show_ink, show_size, show_stroke) = self.selection_style_flags();

        ui.spacing_mut().item_spacing = egui::vec2(3.0, 0.0);
        // Shared multi-select options: highlight color and/or text/ink color (#28).
        // Arrow keys cycle only when a single palette is showing.
        let cycle_ok = show_hl ^ show_ink;
        if show_hl {
            self.style_palette_row(
                ui,
                &crate::geom::HIGHLIGHT_COLORS,
                ColorTarget::Highlights,
                self.selected_highlight_color()
                    .unwrap_or(self.settings.highlight_color),
                cycle_ok,
            );
        }
        if show_hl && show_ink {
            ui.separator();
        }
        if show_ink {
            self.style_palette_row(
                ui,
                &crate::geom::INK_COLORS,
                ColorTarget::Ink,
                self.selected_ink_color()
                    .unwrap_or(self.settings.text_color),
                cycle_ok,
            );
        }
        if show_size {
            let mut size = self.active_text_size();
            if style_step(ui, "A−", "Smaller text").clicked() {
                size = (size - 1.0).max(6.0);
                self.seal_then_arm();
                self.apply_text_size(size);
                self.seal_undo();
            }
            ui.label(
                egui::RichText::new(format!("{size:.0}"))
                    .size(11.0)
                    .color(egui::Color32::from_rgb(232, 232, 236)),
            );
            if style_step(ui, "A+", "Larger text").clicked() {
                size = (size + 1.0).min(96.0);
                self.seal_then_arm();
                self.apply_text_size(size);
                self.seal_undo();
            }
        }
        if show_stroke {
            let mut width = self.active_stroke();
            if style_step(ui, "−", "Thinner stroke").clicked() {
                width = (width - 0.25).max(0.5);
                self.seal_then_arm();
                self.apply_stroke(width);
                self.seal_undo();
            }
            ui.label(
                egui::RichText::new(format!("{width:.1}"))
                    .size(11.0)
                    .color(egui::Color32::from_rgb(232, 232, 236)),
            );
            if style_step(ui, "+", "Thicker stroke").clicked() {
                width = (width + 0.25).min(12.0);
                self.seal_then_arm();
                self.apply_stroke(width);
                self.seal_undo();
            }
        }
        ui.add_space(4.0);
        style_step(ui, "×", "Delete annotation").clicked()
    }

    fn style_palette_row(
        &mut self,
        ui: &mut egui::Ui,
        colors: &[Rgb],
        target: ColorTarget,
        current: Rgb,
        allow_cycle: bool,
    ) {
        // Left / Right cycles the focused palette while the style strip is open.
        let cycle = allow_cycle.then(|| {
            ui.ctx().input(|input| {
                if input.key_pressed(Key::ArrowRight) {
                    Some(1isize)
                } else if input.key_pressed(Key::ArrowLeft) {
                    Some(-1isize)
                } else {
                    None
                }
            })
        })
        .flatten();
        if let Some(delta) = cycle {
            if let Some(idx) = colors.iter().position(|c| *c == current) {
                let next = (idx as isize + delta).rem_euclid(colors.len() as isize) as usize;
                let color = colors[next];
                self.seal_then_arm();
                self.apply_color_to(color, target);
                self.seal_undo();
            } else if let Some(color) = colors.first() {
                self.seal_then_arm();
                self.apply_color_to(*color, target);
                self.seal_undo();
            }
        }
        let current = match target {
            ColorTarget::Highlights => self
                .selected_highlight_color()
                .unwrap_or(self.settings.highlight_color),
            ColorTarget::Ink => self
                .selected_ink_color()
                .unwrap_or(self.settings.text_color),
        };
        let mut picked = None;
        for color in colors {
            if color_dot(ui, *color, *color == current) {
                picked = Some(*color);
            }
        }
        if let Some(color) = picked {
            self.seal_then_arm();
            self.apply_color_to(color, target);
            self.seal_undo();
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
        self.selected_ink_color()
            .or_else(|| self.selected_highlight_color())
    }

    fn selected_highlight_color(&self) -> Option<Rgb> {
        let tab = self.tab()?;
        for id in &tab.selected {
            if let Some(AnnotKind::Highlight { color, .. }) =
                tab.doc.session.get(*id).map(|a| &a.kind)
            {
                return Some(*color);
            }
        }
        None
    }

    fn selected_ink_color(&self) -> Option<Rgb> {
        let tab = self.tab()?;
        for id in &tab.selected {
            match tab.doc.session.get(*id).map(|a| &a.kind) {
                Some(
                    AnnotKind::Text { color, .. }
                    | AnnotKind::Note { color, .. }
                    | AnnotKind::Math { color, .. },
                ) => return Some(*color),
                Some(AnnotKind::Shape { stroke, .. }) => return Some(*stroke),
                _ => {}
            }
        }
        None
    }

    fn active_text_size(&self) -> f32 {
        if let Some(tab) = self.tab() {
            for id in &tab.selected {
                if let Some(annot) = tab.doc.session.get(*id) {
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
            for id in &tab.selected {
                if let Some(AnnotKind::Shape { width, .. }) =
                    tab.doc.session.get(*id).map(|a| &a.kind)
                {
                    return *width;
                }
            }
        }
        self.settings.shape_width
    }

    fn selected_is_text_like(&self) -> bool {
        self.tab().is_some_and(|tab| {
            tab.selected.iter().any(|id| {
                matches!(
                    tab.doc.session.get(*id).map(|a| &a.kind),
                    Some(AnnotKind::Text { .. } | AnnotKind::Math { .. })
                )
            })
        })
    }

    fn selected_is_shape(&self) -> bool {
        self.tab().is_some_and(|tab| {
            tab.selected.iter().any(|id| {
                matches!(
                    tab.doc.session.get(*id).map(|a| &a.kind),
                    Some(AnnotKind::Shape { .. })
                )
            })
        })
    }

    /// (show_highlight_color, show_ink/text_color, show_size, show_stroke) for the selection.
    fn selection_style_flags(&self) -> (bool, bool, bool, bool) {
        let Some(tab) = self.tab() else {
            return (false, false, false, false);
        };
        let mut highlight = false;
        let mut ink = false;
        let mut size = false;
        let mut stroke = false;
        for id in &tab.selected {
            match tab.doc.session.get(*id).map(|a| &a.kind) {
                Some(AnnotKind::Highlight { .. }) => {
                    highlight = true;
                }
                Some(AnnotKind::Text { .. } | AnnotKind::Math { .. } | AnnotKind::Note { .. }) => {
                    ink = true;
                    size = true;
                }
                Some(AnnotKind::Shape { .. }) => {
                    ink = true;
                    stroke = true;
                }
                Some(AnnotKind::Image { .. } | AnnotKind::Future(_)) | None => {}
            }
        }
        (highlight, ink, size, stroke)
    }

    fn apply_color_to(&mut self, color: Rgb, target: ColorTarget) {
        let mut kind_bucket = None;
        let mut math_ids = Vec::new();
        let ids = self
            .tab()
            .map(|tab| tab.selected.clone())
            .unwrap_or_default();
        if let Some(tab) = self.tab_mut() {
            for id in ids {
                let mut touched = None;
                if let Some(annot) = tab.doc.session.get_mut(id) {
                    match (&mut annot.kind, target) {
                        (AnnotKind::Highlight { color: slot, .. }, ColorTarget::Highlights) => {
                            *slot = color;
                            touched = Some(0);
                        }
                        (
                            AnnotKind::Text { color: slot, .. }
                            | AnnotKind::Note { color: slot, .. }
                            | AnnotKind::Math { color: slot, .. },
                            ColorTarget::Ink,
                        ) => {
                            *slot = color;
                            touched = Some(1);
                            if matches!(annot.kind, AnnotKind::Math { .. }) {
                                math_ids.push(id);
                            }
                        }
                        (AnnotKind::Shape { stroke, .. }, ColorTarget::Ink) => {
                            *stroke = color;
                            touched = Some(2);
                        }
                        _ => {}
                    }
                }
                if let Some(bucket) = touched {
                    kind_bucket = Some(bucket);
                    tab.doc.session.mark_dirty(id);
                    if !matches!(tab.save, SaveState::Saving) {
                        tab.save = SaveState::Dirty {
                            since: Instant::now(),
                        };
                    }
                }
            }
        }
        for id in math_ids {
            self.queue_math(id);
        }
        match kind_bucket.unwrap_or(match target {
            ColorTarget::Highlights => 0,
            ColorTarget::Ink => match self.tool {
                Tool::Rect | Tool::Ellipse | Tool::Line => 2,
                _ => 1,
            },
        }) {
            0 => self.settings.highlight_color = color,
            2 => self.settings.shape_color = color,
            _ => self.settings.text_color = color,
        }
        self.settings.save();
    }

    fn apply_text_size(&mut self, size: f32) {
        let size = size.clamp(6.0, 96.0);
        let mut math_ids = Vec::new();
        let ids = self
            .tab()
            .map(|tab| tab.selected.clone())
            .unwrap_or_default();
        if let Some(tab) = self.tab_mut() {
            for id in ids {
                let mut changed = false;
                if let Some(annot) = tab.doc.session.get_mut(id) {
                    match &mut annot.kind {
                        AnnotKind::Text { size: slot, .. } => {
                            *slot = size;
                            changed = true;
                        }
                        AnnotKind::Math { size: slot, .. } => {
                            *slot = size;
                            changed = true;
                            math_ids.push(id);
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
                }
            }
        }
        for id in math_ids {
            self.queue_math(id);
        }
        self.settings.text_size = size;
        self.settings.save();
    }

    fn apply_stroke(&mut self, width: f32) {
        let width = width.clamp(0.25, 16.0);
        let ids = self
            .tab()
            .map(|tab| tab.selected.clone())
            .unwrap_or_default();
        if let Some(tab) = self.tab_mut() {
            for id in ids {
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

fn upload_png_texture(
    ctx: &egui::Context,
    gen: u64,
    seq: u64,
    png: &[u8],
    width: u32,
    height: u32,
) -> Option<egui::TextureHandle> {
    let decoded = image::load_from_memory(png).ok()?.to_rgba8();
    let pixels = egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        decoded.as_raw(),
    );
    Some(ctx.load_texture(
        format!("assistant-crop-{gen}-{seq}"),
        pixels,
        egui::TextureOptions::LINEAR,
    ))
}

fn union_rects(rects: &[PdfRect]) -> Option<PdfRect> {
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

fn urlencoding_minimal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// RGBA pixels from the system clipboard (arboard, then `wl-paste` on Wayland).
fn read_clipboard_rgba() -> Option<(u32, u32, std::sync::Arc<[u8]>)> {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        if let Ok(image) = clipboard.get_image() {
            let width = image.width as u32;
            let height = image.height as u32;
            let expected = width as usize * height as usize * 4;
            if width > 0 && height > 0 && image.bytes.len() >= expected {
                let rgba = std::sync::Arc::from(image.bytes[..expected].to_vec());
                return Some((width, height, rgba));
            }
        }
    }
    read_clipboard_rgba_wl_paste()
}

fn read_clipboard_rgba_wl_paste() -> Option<(u32, u32, std::sync::Arc<[u8]>)> {
    let output = Command::new("wl-paste")
        .args(["-t", "image/png"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    let image = image::load_from_memory(&output.stdout).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height, std::sync::Arc::from(image.into_raw())))
}

fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Command::new("xdg-open")
            .arg(url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn input_shift(ctx: &egui::Context) -> bool {
    ctx.input(|input| input.modifiers.shift)
}

fn style_step(ui: &mut egui::Ui, label: &str, tip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(20.0, 18.0), egui::Sense::click());
    let fill = if response.is_pointer_button_down_on() {
        egui::Color32::from_white_alpha(28)
    } else if response.hovered() {
        egui::Color32::from_white_alpha(16)
    } else {
        egui::Color32::TRANSPARENT
    };
    if fill != egui::Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(4), fill);
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::new(12.0, egui::FontFamily::Proportional),
        egui::Color32::from_rgb(232, 232, 236),
    );
    response.on_hover_text(tip)
}
