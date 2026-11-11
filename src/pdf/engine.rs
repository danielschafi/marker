use std::collections::{HashMap, VecDeque};
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};

use mupdf::pdf::{PdfDocument, PdfWriteOptions};
use mupdf::{Colorspace, Device, DisplayList, IRect, Matrix, Pixmap, Rect, Size, TextPageFlags};

use crate::annot::{Annotation, PageGlyphs};
use crate::geom::{PdfRect, Rgb};

use super::annot_read::{import_annotations, read_marker, read_nm_id};
use super::annot_write::{create_inline_math, delete_inline_math_children, delete_xref, upsert};
use super::convert::{
    convert_outline, glyphs_from_structured, quad_bounds, search_structured_exact,
};
use super::raster::{premultiplied_tile_pixels_from_pixmap, tile_device_rect};
use super::repair::{
    is_repaired_sidecar, pdfunite_available, repaired_sidecar_path, write_poppler_repaired_copy,
};

pub const TILE_PX: i32 = 1024;
/// Target width in device pixels for page thumbnails (FT10). Separate from
/// [`TILE_PX`] so thumbs never share the document tile cache.
pub const THUMB_WIDTH_PX: f32 = 120.0;
const LIST_CACHE: usize = 8;

/// Typed PDF engine failures. Converted to [`String`] at the worker reply boundary
/// (`PdfReply::{Failed,Crop,Saved,SavedAs}`) so the UI stays stringly until a later PR.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Mupdf(#[from] mupdf::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("No document is open.")]
    NoDocument,
    #[error("Page is out of range.")]
    PageOutOfRange { page: usize, count: usize },
    #[error("image encode failed")]
    Encode,
    /// MuPDF reported no pages (and Poppler repair was unavailable or also failed).
    #[error("This PDF has no readable pages. It may be damaged.")]
    NoReadablePages,
    #[error("{0}")]
    Message(String),
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Self::Message(message.to_owned())
    }
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PageInfo {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl PageInfo {
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }
}

#[derive(Clone, Debug)]
pub struct OutlineNode {
    pub title: String,
    pub page: Option<usize>,
    pub y: Option<f32>,
    pub children: Vec<OutlineNode>,
}

pub struct TileImage {
    pub page: usize,
    pub scale: f32,
    pub col: i32,
    pub row: i32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA8, row-major (`egui::ColorImage::from_rgba_premultiplied` byte order).
    pub pixels: Vec<u8>,
}

/// Small full-page raster for the thumbnails panel (not a document tile).
pub struct ThumbImage {
    pub page: usize,
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA8, row-major (`egui::ColorImage::from_rgba_premultiplied` byte order).
    pub pixels: Vec<u8>,
}

pub struct SaveSnapshot {
    pub upserts: Vec<Annotation>,
    pub deletes: Vec<(usize, i32)>,
    pub math_pdfs: HashMap<u64, Vec<u8>>,
    /// Inline math stamps exploded from rich Text annotations.
    pub inline_math: Vec<InlineMathSave>,
    /// Text annot ids whose previous InlineMath children should be removed.
    pub rich_text_parents: Vec<(usize, u64)>,
}

#[derive(Clone, Debug)]
pub struct InlineMathSave {
    pub parent_id: u64,
    pub page: usize,
    pub index: u32,
    pub rect: PdfRect,
    pub source: String,
    pub size: f32,
    pub color: Rgb,
    pub pdf: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug)]
pub struct SavedXref {
    pub id: u64,
    pub page: usize,
    pub xref: i32,
}

pub struct LoadedPdf {
    pub engine: DocumentEngine,
    pub annotations: Vec<Annotation>,
    /// When set, `engine` was opened from a Poppler-repaired sidecar of this path.
    pub repaired_from: Option<PathBuf>,
}

pub struct DocumentEngine {
    doc: PdfDocument,
    path: PathBuf,
    pages: Vec<PageInfo>,
    outline: Vec<OutlineNode>,
    lists: HashMap<usize, DisplayList>,
    list_order: VecDeque<usize>,
    /// Byte length at open, reset after a compacting rewrite.
    opened_len: u64,
}

impl DocumentEngine {
    /// Open `path` with MuPDF. When MuPDF reports zero pages, attempt a Poppler
    /// (`pdfunite`) structural rewrite into a sibling `*.marker-repaired.pdf`
    /// and open that copy instead. The original file is never overwritten.
    pub fn open(path: &Path) -> Result<LoadedPdf, Error> {
        match Self::open_mupdf(path) {
            Ok(loaded) => Ok(loaded),
            Err(Error::NoReadablePages) => Self::open_via_poppler_repair(path),
            Err(err) => Err(err),
        }
    }

    fn open_via_poppler_repair(path: &Path) -> Result<LoadedPdf, Error> {
        if is_repaired_sidecar(path) || !pdfunite_available() {
            return Err(Error::NoReadablePages);
        }
        let sidecar = repaired_sidecar_path(path);
        write_poppler_repaired_copy(path, &sidecar)?;
        let mut loaded = Self::open_mupdf(&sidecar)?;
        loaded.repaired_from = Some(path.to_path_buf());
        Ok(loaded)
    }

    fn open_mupdf(path: &Path) -> Result<LoadedPdf, Error> {
        super::save_transaction::cleanup_abandoned(path)?;
        let doc = PdfDocument::open(path)?;
        if doc.needs_password().unwrap_or(false) {
            return Err("This PDF is password protected.".into());
        }
        let count = doc.page_count()?;
        if count <= 0 {
            return Err(Error::NoReadablePages);
        }
        let mut pages = Vec::with_capacity(count as usize);
        for index in 0..count {
            let page = doc.load_pdf_page(index)?;
            let bounds = page.bounds()?;
            pages.push(PageInfo {
                x0: bounds.x0,
                y0: bounds.y0,
                x1: bounds.x1,
                y1: bounds.y1,
            });
        }
        let outline = doc.outlines().map(convert_outline).unwrap_or_default();
        let annotations = import_annotations(&doc)?;
        let opened_len = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        Ok(LoadedPdf {
            engine: Self {
                doc,
                path: path.to_path_buf(),
                pages,
                outline,
                lists: HashMap::new(),
                list_order: VecDeque::new(),
                opened_len,
            },
            annotations,
            repaired_from: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn pages(&self) -> &[PageInfo] {
        &self.pages
    }

    pub fn outline(&self) -> &[OutlineNode] {
        &self.outline
    }

    pub fn glyphs(&self, page: usize) -> Result<PageGlyphs, Error> {
        // Prefer a cached display list so we do not load the page again.
        if let Some(list) = self.lists.get(&page) {
            let text = list.to_text_page(TextPageFlags::ACCURATE_BBOXES)?;
            return Ok(glyphs_from_structured(&text.structured()));
        }
        let pdf_page = self.doc.load_pdf_page(page as i32)?;
        let text = pdf_page.to_text_page(TextPageFlags::ACCURATE_BBOXES)?;
        Ok(glyphs_from_structured(&text.structured()))
    }

    pub fn search_page(
        &self,
        page: usize,
        needle: &str,
        case_sensitive: bool,
    ) -> Result<Vec<PdfRect>, Error> {
        if needle.is_empty() || page >= self.pages.len() {
            return Ok(Vec::new());
        }
        let pdf_page = self.doc.load_pdf_page(page as i32)?;
        let text = pdf_page.to_text_page(TextPageFlags::empty())?;
        if case_sensitive {
            return Ok(search_structured_exact(&text.structured(), needle));
        }
        // MuPDF's TextPage::search folds case (FZ_SEARCH_IGNORE_CASE).
        Ok(text.search(needle)?.into_iter().map(quad_bounds).collect())
    }

    /// Rasterize one contents-only tile. Annotations stay in the overlay.
    pub fn render_tile(
        &mut self,
        page: usize,
        scale: f32,
        col: i32,
        row: i32,
    ) -> Result<Option<TileImage>, Error> {
        let info = *self.pages.get(page).ok_or(Error::PageOutOfRange {
            page,
            count: self.pages.len(),
        })?;
        let scale = scale.max(0.05);
        let (tx0, ty0, tx1, ty1) = tile_device_rect(info, scale, col, row);
        if tx1 <= tx0 || ty1 <= ty0 {
            return Ok(None);
        }

        let ctm = Matrix::new_scale(scale, scale);
        let irect = IRect::new(tx0, ty0, tx1, ty1);
        let mut pixmap = Pixmap::new_with_rect(&Colorspace::device_rgb(), irect, false)?;
        pixmap.clear_with(255)?;
        let device = Device::from_pixmap(&pixmap)?;
        let scissor = Rect::new(tx0 as f32, ty0 as f32, tx1 as f32, ty1 as f32);
        self.display_list(page)?.run(&device, &ctm, scissor)?;
        drop(device);

        let width = pixmap.width();
        let height = pixmap.height();
        let pixels = premultiplied_tile_pixels_from_pixmap(&pixmap);
        Ok(Some(TileImage {
            page,
            scale,
            col,
            row,
            x: tx0,
            y: ty0,
            width,
            height,
            pixels,
        }))
    }

    /// Rasterize a whole page at ~[`THUMB_WIDTH_PX`] wide for the side panel.
    /// Uses the same contents-only path as tiles; not stored in the tile cache.
    pub fn render_thumb(&mut self, page: usize) -> Result<Option<ThumbImage>, Error> {
        let info = *self.pages.get(page).ok_or(Error::PageOutOfRange {
            page,
            count: self.pages.len(),
        })?;
        let page_w = info.width().max(1.0);
        let scale = (THUMB_WIDTH_PX / page_w).max(0.05);
        let tx0 = (info.x0 * scale).floor() as i32;
        let ty0 = (info.y0 * scale).floor() as i32;
        let tx1 = (info.x1 * scale).ceil() as i32;
        let ty1 = (info.y1 * scale).ceil() as i32;
        if tx1 <= tx0 || ty1 <= ty0 {
            return Ok(None);
        }

        let ctm = Matrix::new_scale(scale, scale);
        let irect = IRect::new(tx0, ty0, tx1, ty1);
        let mut pixmap = Pixmap::new_with_rect(&Colorspace::device_rgb(), irect, false)?;
        pixmap.clear_with(255)?;
        let device = Device::from_pixmap(&pixmap)?;
        let scissor = Rect::new(tx0 as f32, ty0 as f32, tx1 as f32, ty1 as f32);
        self.display_list(page)?.run(&device, &ctm, scissor)?;
        drop(device);

        let width = pixmap.width();
        let height = pixmap.height();
        let pixels = premultiplied_tile_pixels_from_pixmap(&pixmap);
        Ok(Some(ThumbImage {
            page,
            width,
            height,
            pixels,
        }))
    }

    pub fn save(&mut self, snapshot: &SaveSnapshot) -> Result<Vec<SavedXref>, Error> {
        let saved = (|| {
            let mut xrefs = self.apply(snapshot)?;
            if self.persist()? {
                self.remap_xrefs(&mut xrefs);
            }
            Ok(xrefs)
        })();
        if saved.is_err() {
            let path = self.path.clone();
            if let Ok(reloaded) = Self::open(&path) {
                *self = reloaded.engine;
            }
        }
        saved
    }

    /// Full write to `new_path`, then switch this engine onto that file.
    ///
    /// Never writes the open path unless `new_path` is the same file. On failure the
    /// engine is reloaded from the previous path so a half-applied Save As does not
    /// leave the tab pointing at a new file that was not written.
    pub fn save_as(
        &mut self,
        snapshot: &SaveSnapshot,
        new_path: &Path,
    ) -> Result<Vec<SavedXref>, Error> {
        let old_path = self.path.clone();
        let same = paths_equal(&old_path, new_path);
        let saved = (|| {
            let xrefs = self.apply(snapshot)?;
            self.rewrite_all(new_path)?;
            if !same {
                self.path = new_path.to_path_buf();
            }
            if let Ok(meta) = std::fs::metadata(new_path) {
                self.opened_len = meta.len();
            }
            Ok(xrefs)
        })();
        if saved.is_err() {
            if let Ok(reloaded) = Self::open(&old_path) {
                *self = reloaded.engine;
            }
        }
        saved
    }

    /// Insert a blank page after `after` (0-based). Returns the new page index.
    /// Size matches `after` when present, otherwise the first page / A4.
    pub fn insert_blank_page(&mut self, after: usize) -> Result<(usize, Vec<PageInfo>), Error> {
        let at = if self.pages.is_empty() {
            0
        } else {
            (after + 1).min(self.pages.len())
        };
        self.insert_blank_page_at(at)
    }

    /// Insert a blank page at `at` (0-based). Returns the new page index.
    pub fn insert_blank_page_at(&mut self, at: usize) -> Result<(usize, Vec<PageInfo>), Error> {
        let template = self
            .pages
            .get(at.saturating_sub(1).min(self.pages.len().saturating_sub(1)))
            .or_else(|| self.pages.first())
            .copied()
            .unwrap_or(PageInfo {
                x0: 0.0,
                y0: 0.0,
                x1: Size::A4.width,
                y1: Size::A4.height,
            });
        let at = at.min(self.pages.len());
        let size = Size::new(template.width().max(1.0), template.height().max(1.0));
        self.doc.new_page_at(at as i32, size)?;
        self.lists.clear();
        self.list_order.clear();
        self.refresh_pages()?;
        self.persist()?;
        Ok((at, self.pages.clone()))
    }

    /// Delete the page at `index` (0-based).
    pub fn delete_page_at(&mut self, index: usize) -> Result<Vec<PageInfo>, Error> {
        if index >= self.pages.len() {
            return Err("Page index out of range.".into());
        }
        if self.pages.len() <= 1 {
            return Err("Cannot delete the only page.".into());
        }
        self.doc.delete_page(index as i32)?;
        self.lists.clear();
        self.list_order.clear();
        self.refresh_pages()?;
        self.persist()?;
        Ok(self.pages.clone())
    }

    fn refresh_pages(&mut self) -> Result<(), Error> {
        let count = self.doc.page_count()?;
        let mut pages = Vec::with_capacity(count as usize);
        for index in 0..count {
            let page = self.doc.load_pdf_page(index)?;
            let bounds = page.bounds()?;
            pages.push(PageInfo {
                x0: bounds.x0,
                y0: bounds.y0,
                x1: bounds.x1,
                y1: bounds.y1,
            });
        }
        self.pages = pages;
        Ok(())
    }

    fn display_list(&mut self, page: usize) -> Result<&DisplayList, Error> {
        if !self.lists.contains_key(&page) {
            let pdf_page = self.doc.load_pdf_page(page as i32)?;
            let list = pdf_page.to_display_list(false)?;
            self.lists.insert(page, list);
            self.list_order.push_back(page);
            while self.list_order.len() > LIST_CACHE {
                if let Some(old) = self.list_order.pop_front() {
                    if old != page {
                        self.lists.remove(&old);
                    }
                }
            }
        }
        self.lists
            .get(&page)
            .ok_or_else(|| "Missing page cache.".into())
    }

    fn apply(&mut self, snapshot: &SaveSnapshot) -> Result<Vec<SavedXref>, Error> {
        for &(page, xref) in &snapshot.deletes {
            let mut pdf_page = self.doc.load_pdf_page(page as i32)?;
            delete_xref(&mut pdf_page, xref)?;
        }
        for &(page, parent_id) in &snapshot.rich_text_parents {
            let mut pdf_page = self.doc.load_pdf_page(page as i32)?;
            delete_inline_math_children(&mut pdf_page, parent_id)?;
        }
        let mut saved = Vec::new();
        for annot in &snapshot.upserts {
            if !annot.dirty {
                continue;
            }
            let math = snapshot.math_pdfs.get(&annot.id).map(Vec::as_slice);
            let mut pdf_page = self.doc.load_pdf_page(annot.page as i32)?;
            let xref = upsert(&mut self.doc, &mut pdf_page, annot, math)?;
            saved.push(SavedXref {
                id: annot.id,
                page: annot.page,
                xref,
            });
        }
        for inline in &snapshot.inline_math {
            let mut pdf_page = self.doc.load_pdf_page(inline.page as i32)?;
            create_inline_math(&mut self.doc, &mut pdf_page, inline)?;
        }
        Ok(saved)
    }

    /// Stage incremental output and any compaction before one atomic replacement.
    /// `Ok(true)` means xrefs may have moved during a full rewrite.
    fn persist(&mut self) -> Result<bool, Error> {
        let path = self.path.clone();
        let incremental = self.doc.can_be_saved_incrementally();
        let (doc, rewritten, len) = super::save_transaction::write(&path, |candidate, file| {
            let mut options = PdfWriteOptions::default();
            options.set_appearance(false);
            if incremental {
                options.set_incremental(true);
                let name = candidate
                    .to_str()
                    .ok_or("The PDF path is not valid UTF-8.")?;
                self.doc.save_with_options(name, options)?;
            } else {
                self.doc.write_to_with_options(file, options)?;
            }
            let compact = incremental
                && self.opened_len != 0
                && file.metadata()?.len() > self.opened_len.saturating_mul(5) / 4;
            if compact {
                file.set_len(0)?;
                file.seek(SeekFrom::Start(0))?;
                let mut options = PdfWriteOptions::default();
                options.set_appearance(false);
                options.set_garbage(true);
                self.doc.write_to_with_options(file, options)?;
            }
            Ok((
                PdfDocument::open(candidate)?,
                !incremental || compact,
                file.metadata()?.len(),
            ))
        })?;
        self.doc = doc;
        self.lists.clear();
        self.list_order.clear();
        if rewritten {
            self.opened_len = len;
        }
        Ok(rewritten)
    }

    fn remap_xrefs(&self, saved: &mut [SavedXref]) {
        for entry in saved {
            if let Some(xref) = self.xref_for_id(entry.page, entry.id) {
                entry.xref = xref;
            }
        }
    }

    fn xref_for_id(&self, page: usize, id: u64) -> Option<i32> {
        let pdf_page = self.doc.load_pdf_page(page as i32).ok()?;
        for annot in pdf_page.annotations() {
            let marker = read_marker(&annot.object());
            let same_id = marker.id == Some(id) || read_nm_id(&annot.object()) == Some(id);
            if same_id {
                return annot.xref().ok();
            }
        }
        None
    }

    fn rewrite_all(&mut self, path: &Path) -> Result<(), Error> {
        let doc = super::save_transaction::write(path, |candidate, file| {
            self.doc.write_to(file)?;
            Ok(PdfDocument::open(candidate)?)
        })?;
        self.doc = doc;
        self.lists.clear();
        self.list_order.clear();
        Ok(())
    }
}

/// `name.pdf.bak` — append `.bak` to the file name, not a replaced extension.
#[cfg(test)]
fn bak_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document.pdf");
    path.with_file_name(format!("{name}.bak"))
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
