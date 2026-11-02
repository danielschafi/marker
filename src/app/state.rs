//! App document/tab state types and constructors.
//!
//! Behavior-preserving extract from the former monolithic `app` module (RF9a).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::Key;

use crate::annot::{AnnotKind, Handle, MarkupStyle, PageGlyphs, Session, ShapeKind};
use crate::assistant::TabAssistant;
use crate::geom::{PdfPoint, Rgb};
use crate::pdf::{DocId, OpenedDoc, OutlineNode, PageInfo};
use crate::undo::UndoStack;

use super::TabId;

pub(crate) struct Tab {
    pub(crate) id: TabId,
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
    /// Study prompt waiting on a one-shot glyph load for `page` (ST5).
    pub(crate) pending_study: Option<(usize, String)>,
    /// Markdown / Anki CSV export waiting on glyph loads for highlight pages (ST1 / ST3).
    pub(crate) pending_export: Option<PendingExport>,
    pub(crate) previews: HashMap<u64, MathPreview>,
    pub(crate) image_textures: HashMap<u64, (usize, egui::TextureHandle)>,
    /// After Tab exits a math island, place the TextEdit caret here (bytes).
    pub(crate) pending_text_caret: Option<usize>,
    /// Caret is strictly inside this math span: `(text_annot_id, span_start_byte)`.
    /// Selects the live render lane. There is no second editor.
    pub(crate) live_math: Option<(u64, usize)>,
    pub(crate) save: SaveState,
    pub(crate) save_epoch: u64,
    pub(crate) save_deletes: Vec<(usize, i32)>,
    pub(crate) force_save: bool,
    pub(crate) save_when_math_ready: bool,
    pub(crate) close_after_save: bool,
    /// After a successful save, spawn this tab in a new window and drop it here.
    pub(crate) detach_after_save: bool,
    pub(crate) outline_open: bool,
    pub(crate) thumbs_open: bool,
    pub(crate) annots_open: bool,
    pub(crate) review_open: bool,
    /// Page → texture for the thumbnails panel; capped by [`THUMB_CACHE_PAGES`].
    pub(crate) thumbs: HashMap<usize, CachedThumb>,
    /// LRU order for `thumbs` (oldest at the front).
    pub(crate) thumb_order: VecDeque<usize>,
    /// In-flight thumbnail renders (page indices).
    pub(crate) thumbs_inflight: HashSet<usize>,
    pub(crate) annot_sidebar: AnnotSidebarState,
    pub(crate) review: ReviewState,
    pub(crate) assistant: TabAssistant,
    pub(crate) search: SearchState,
    pub(crate) last_hl: Option<(Instant, usize, u32, Option<u64>)>,
    pub(crate) focus_edit: bool,
    pub(crate) menu: Option<ContextMenu>,
    /// Compact color/size strip next to an annotation (e.g. just after create).
    pub(crate) style_bar: Option<StyleBar>,
    /// Annotation and page-op undo/redo history.
    pub(super) undo: UndoStack,
    /// Annotation indices per page; rebuilt when `session.epoch` changes.
    pub(crate) annot_by_page: Vec<Vec<usize>>,
    pub(super) annot_index_epoch: u64,
    /// Last `session.epoch` when image annotation textures were refreshed.
    pub(crate) image_textures_epoch: u64,
    /// Cached `session.is_dirty()` for autosave (keyed on `session.epoch`).
    pub(super) autosave_dirty_epoch: u64,
    pub(super) autosave_dirty: bool,
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
    /// Clipboard offered an image when the menu opened (avoids probing every frame).
    pub can_paste: bool,
}

impl Tab {
    /// Build a tab from a freshly opened document. Defaults match the former `on_pdf` literal.
    pub(crate) fn new(id: TabId, opened: OpenedDoc) -> Self {
        let tops = DocState::rebuild_tops(&opened.pages);
        Self {
            id,
            doc: DocState {
                path: opened.path,
                doc_id: opened.doc_id,
                pages: opened.pages,
                tops,
                outline: opened.outline,
                session: Session::from_imported(opened.annotations),
                scale: 1.0,
                scroll_x: 0.0,
                scroll_y: 0.0,
                scroll_y_prev: 0.0,
                fitted: false,
                last_zoom: Instant::now(),
                last_scroll: Instant::now(),
                last_fit: None,
                glyphs: HashMap::new(),
                glyph_order: VecDeque::new(),
                tiles: HashMap::new(),
            },
            selected: Vec::new(),
            text_sel: None,
            editing: None,
            drag: None,
            pending_jump: None,
            inflight: HashSet::new(),
            glyphs_waiting: HashSet::new(),
            pending_study: None,
            pending_export: None,
            previews: HashMap::new(),
            image_textures: HashMap::new(),
            pending_text_caret: None,
            live_math: None,
            save: SaveState::Clean,
            save_epoch: 0,
            save_deletes: Vec::new(),
            force_save: false,
            save_when_math_ready: false,
            close_after_save: false,
            detach_after_save: false,
            outline_open: false,
            thumbs_open: false,
            annots_open: false,
            review_open: false,
            thumbs: HashMap::new(),
            thumb_order: VecDeque::new(),
            thumbs_inflight: HashSet::new(),
            annot_sidebar: AnnotSidebarState::default(),
            review: ReviewState::default(),
            assistant: TabAssistant::default(),
            search: SearchState::default(),
            last_hl: None,
            focus_edit: false,
            menu: None,
            style_bar: None,
            undo: UndoStack::default(),
            annot_by_page: Vec::new(),
            // Imported sessions start at epoch 0. MAX forces the first index build.
            annot_index_epoch: u64::MAX,
            image_textures_epoch: 0,
            autosave_dirty_epoch: 0,
            autosave_dirty: false,
        }
    }

    pub(crate) fn ensure_annot_index(&mut self) {
        let epoch = self.doc.session.epoch;
        if self.annot_index_epoch == epoch {
            return;
        }
        self.annot_index_epoch = epoch;
        let pages = self.doc.pages.len();
        self.annot_by_page = vec![Vec::new(); pages.max(1)];
        for (index, annot) in self.doc.session.annotations.iter().enumerate() {
            if annot.page < self.annot_by_page.len() {
                self.annot_by_page[annot.page].push(index);
            }
        }
    }

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

    /// Mark `page` as most-recently used and drop older thumbs past the soft cap.
    pub(crate) fn touch_thumb(&mut self, page: usize) {
        self.thumb_order.retain(|&p| p != page);
        self.thumb_order.push_back(page);
        while self.thumb_order.len() > THUMB_CACHE_PAGES {
            if let Some(old) = self.thumb_order.pop_front() {
                self.thumbs.remove(&old);
            }
        }
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
    pub(crate) doc_id: DocId,
    pub(crate) pages: Vec<PageInfo>,
    pub(crate) tops: Vec<f32>,
    pub(crate) outline: Vec<OutlineNode>,
    pub(crate) session: Session,
    pub(crate) scale: f32,
    pub(crate) scroll_x: f32,
    pub(crate) scroll_y: f32,
    /// Previous `scroll_y` for prefetch direction (updated in `wanted_tiles`).
    pub(crate) scroll_y_prev: f32,
    pub(crate) fitted: bool,
    pub(crate) last_zoom: Instant,
    /// Last pan/scroll; keeps the interactive frame budget warm between wheel samples.
    pub(crate) last_scroll: Instant,
    /// Last Fit action; cleared when zoom changes by other means.
    pub(crate) last_fit: Option<FitKind>,
    /// Per-page glyphs; bounded by [`GLYPH_CACHE_PAGES`] via [`glyph_order`].
    pub(crate) glyphs: HashMap<usize, PageGlyphs>,
    /// LRU order for `glyphs` (oldest at the front).
    pub(crate) glyph_order: VecDeque<usize>,
    pub(crate) tiles: HashMap<TileKey, CachedTile>,
}

/// Soft cap on cached glyph pages per tab.
pub(crate) const GLYPH_CACHE_PAGES: usize = 64;

/// Pages to drop from an LRU key list so at most `cap` remain.
///
/// `keys` is oldest-first. Never returns a page in `pinned`. If more than
/// `cap` pages are pinned, the result may leave the cache above `cap`.
pub(crate) fn evict(keys: &[usize], pinned: &[usize], cap: usize) -> Vec<usize> {
    if keys.len() <= cap {
        return Vec::new();
    }
    let pinned: HashSet<usize> = pinned.iter().copied().collect();
    let mut to_evict = Vec::new();
    let mut remaining = keys.len();
    for &page in keys {
        if remaining <= cap {
            break;
        }
        if pinned.contains(&page) {
            continue;
        }
        to_evict.push(page);
        remaining -= 1;
    }
    to_evict
}
impl DocState {
    fn touch_glyph_page(&mut self, page: usize) {
        self.glyph_order.retain(|&p| p != page);
        self.glyph_order.push_back(page);
    }

    pub(super) fn insert_glyphs(&mut self, page: usize, glyphs: PageGlyphs) {
        self.glyphs.insert(page, glyphs);
        self.touch_glyph_page(page);
    }

    pub(super) fn trim_glyph_cache(&mut self, pinned: &[usize]) {
        let keys: Vec<usize> = self.glyph_order.iter().copied().collect();
        let drop = evict(&keys, pinned, GLYPH_CACHE_PAGES);
        for page in drop {
            self.glyphs.remove(&page);
            self.glyph_order.retain(|&p| p != page);
        }
    }
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
    pub(crate) last_used_frame: u64,
    pub(crate) texture: egui::TextureHandle,
}

/// Soft cap on cached page thumbnails per tab (FT10). Separate from the tile budget.
pub(crate) const THUMB_CACHE_PAGES: usize = 40;

pub(crate) struct CachedThumb {
    pub(crate) texture: egui::TextureHandle,
}

pub(crate) use crate::pdf::TileKey;

pub(crate) struct MathPreview {
    pub(super) source: String,
    pub(super) size: f32,
    pub(super) color: Rgb,
    pub(super) req: u64,
    pub(crate) pending: bool,
    pub(crate) texture: Option<egui::TextureHandle>,
    pub(crate) width_pt: f32,
    pub(crate) height_pt: f32,
    pub(super) pdf: Option<Vec<u8>>,
    pub(super) pdf_pending: bool,
    pub(super) pdf_req: Option<u64>,
    pub(crate) error: Option<String>,
}

impl MathPreview {
    /// True when the block preview has a final PDF or a terminal error.
    pub(crate) fn is_settled(&self) -> bool {
        !self.pending && (self.pdf.is_some() || self.error.is_some())
    }
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

/// Coarse kind buckets for the annotation sidebar filters (FT8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AnnotFilterKind {
    Highlight,
    Underline,
    StrikeOut,
    Squiggly,
    Text,
    Note,
    Math,
    Shape,
    Image,
    Foreign,
}

impl AnnotFilterKind {
    pub(crate) const ALL: [Self; 10] = [
        Self::Highlight,
        Self::Underline,
        Self::StrikeOut,
        Self::Squiggly,
        Self::Text,
        Self::Note,
        Self::Math,
        Self::Shape,
        Self::Image,
        Self::Foreign,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Highlight => "Highlight",
            Self::Underline => "Underline",
            Self::StrikeOut => "Strikeout",
            Self::Squiggly => "Squiggly",
            Self::Text => "Text",
            Self::Note => "Note",
            Self::Math => "Equation",
            Self::Shape => "Shape",
            Self::Image => "Image",
            Self::Foreign => "Imported",
        }
    }

    pub(crate) fn from_annot(kind: &AnnotKind) -> Option<Self> {
        match kind {
            AnnotKind::Highlight { .. } => Some(Self::Highlight),
            AnnotKind::Markup {
                style: MarkupStyle::Underline,
                ..
            } => Some(Self::Underline),
            AnnotKind::Markup {
                style: MarkupStyle::StrikeOut,
                ..
            } => Some(Self::StrikeOut),
            AnnotKind::Markup {
                style: MarkupStyle::Squiggly,
                ..
            } => Some(Self::Squiggly),
            AnnotKind::Text { .. } => Some(Self::Text),
            AnnotKind::Note { .. } => Some(Self::Note),
            AnnotKind::Math { .. } => Some(Self::Math),
            AnnotKind::Shape { .. } => Some(Self::Shape),
            AnnotKind::Image { .. } => Some(Self::Image),
            AnnotKind::Foreign { .. } => Some(Self::Foreign),
        }
    }
}

#[derive(Default)]
pub(crate) struct AnnotSidebarState {
    pub(crate) query: String,
    /// Empty means every kind. Non-empty is an OR of selected kinds.
    pub(crate) kinds: HashSet<AnnotFilterKind>,
    /// Empty means every color. Non-empty is an OR of selected palette colors.
    pub(crate) colors: Vec<Rgb>,
}

/// How the review panel buckets highlights and notes (ST4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum ReviewGroupMode {
    #[default]
    Preset,
    Outline,
}

/// Local UI state for the review panel. Never written to the PDF.
#[derive(Default)]
pub(crate) struct ReviewState {
    pub(crate) group_by: ReviewGroupMode,
    /// When true, rows show only the prompt (page + preset) until clicked.
    pub(crate) hide_text: bool,
    /// Annot ids whose quote is revealed while `hide_text` is on.
    pub(crate) revealed: HashSet<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tool {
    Select,
    Highlight,
    Underline,
    StrikeOut,
    Squiggly,
    Text,
    Note,
    Rect,
    Ellipse,
    Line,
    Math,
}

impl Tool {
    pub(crate) const ALL: [Tool; 11] = [
        Tool::Select,
        Tool::Highlight,
        Tool::Underline,
        Tool::StrikeOut,
        Tool::Squiggly,
        Tool::Text,
        Tool::Note,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Line,
        Tool::Math,
    ];

    /// Bare letter that selects this tool. Vim pans with `J`/`K`/`L` (not `H` — Highlight).
    /// Underline is `U`, strikeout is `X`, squiggly is `W`. Note is `N`.
    pub(crate) fn shortcut(self) -> Key {
        match self {
            Tool::Select => Key::S,
            Tool::Highlight => Key::H,
            Tool::Underline => Key::U,
            Tool::StrikeOut => Key::X,
            Tool::Squiggly => Key::W,
            Tool::Text => Key::T,
            Tool::Note => Key::N,
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
            Tool::Underline => "Underline text",
            Tool::StrikeOut => "Strike through text",
            Tool::Squiggly => "Squiggly underline",
            Tool::Text => "Write on the page",
            Tool::Note => "Sticky note",
            Tool::Rect => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Line => "Line",
            Tool::Math => "Equation",
        }
    }

    /// `None` is the highlight fill. The three stroke tools share its gesture.
    pub(crate) fn markup_style(self) -> Option<MarkupStyle> {
        match self {
            Tool::Underline => Some(MarkupStyle::Underline),
            Tool::StrikeOut => Some(MarkupStyle::StrikeOut),
            Tool::Squiggly => Some(MarkupStyle::Squiggly),
            Tool::Select
            | Tool::Highlight
            | Tool::Text
            | Tool::Note
            | Tool::Rect
            | Tool::Ellipse
            | Tool::Line
            | Tool::Math => None,
        }
    }

    pub(crate) fn is_text_mark(self) -> bool {
        matches!(
            self,
            Tool::Highlight | Tool::Underline | Tool::StrikeOut | Tool::Squiggly
        )
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CreateKind {
    Text,
    Note,
    Math,
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
        /// `None` paints a highlight fill. A style paints underline, strike, or squiggle.
        style: Option<MarkupStyle>,
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

impl SaveState {
    /// Mark dirty unless a save is already in flight.
    pub(crate) fn touch(&mut self) {
        if !matches!(self, Self::Saving) {
            *self = SaveState::Dirty {
                since: Instant::now(),
            };
        }
    }

    /// After a finished save, re-arm dirty with a backdated stamp so autosave retries soon.
    pub(crate) fn redirty_soon(&mut self) {
        *self = Self::Dirty {
            since: Instant::now() - Duration::from_secs(2),
        };
    }

    /// Whether the debounce has elapsed (or a prior failure should retry).
    pub(crate) fn autosave_due(&self) -> bool {
        match self {
            Self::Dirty { since } => since.elapsed() >= Duration::from_millis(1500),
            Self::Failed { .. } => true,
            Self::Clean | Self::Saving => false,
        }
    }
}

/// Which highlight export is waiting on glyph loads (ST1 / ST3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingExport {
    Markdown,
    Cards,
}
#[cfg(test)]
mod glyph_cache_tests {
    use super::{evict, GLYPH_CACHE_PAGES};

    #[test]
    fn evict_drops_oldest_unpinned_to_cap() {
        let keys: Vec<usize> = (0..10).collect();
        assert_eq!(evict(&keys, &[], 4), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(evict(&keys, &[0, 1], 4), vec![2, 3, 4, 5, 6, 7]);
        assert!(evict(&keys, &[], 10).is_empty());
        assert!(evict(&keys, &[], 64).is_empty());
        // Pinned oldest pages are skipped; newer unpinned pages go instead.
        assert_eq!(evict(&keys, &[0, 1, 2, 3], 4), vec![4, 5, 6, 7, 8, 9]);
        // More pinned than cap: never evict pinned, even if over capacity.
        assert!(evict(&[1, 2, 3], &[1, 2, 3], 1).is_empty());
        assert_eq!(GLYPH_CACHE_PAGES, 64);
    }
}
