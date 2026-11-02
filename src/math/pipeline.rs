//! Session-scoped math pipeline: cache, deadlines, scan stamps, and worker.
//!
//! **Interface:** given annotations / visible pages / zoom, enqueue renders,
//! apply replies, and answer cache lookups. Typst/MiTeX and [`MathWorker`] are
//! internal adapters. Callers (MarkerApp) bind tabs and egui textures at the
//! seam; they do not own deadline/scan/cache policy.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::annot::{AnnotKind, Annotation};
use crate::geom::Rgb;
use crate::math_spans;
use crate::pdf::DocId;

use super::{
    map_typst_error, needs_sharper_raster, pdf_plan, raster_scale_for, wants_display_math,
    EntryKind, MathKey, MathLane, MathRender, MathWorker, PdfPlan, RgbaImage,
};

/// Trailing debounce before a queued keystroke fires a render request.
const DEADLINE_DEBOUNCE: Duration = Duration::from_millis(40);

/// Point size used for math inside finished assistant turns (ST7).
pub const ASSISTANT_MATH_SIZE: f32 = 14.0;

/// Ready payload for [`super::MathCache`]. The SVG stays so a later zoom can re-raster.
pub struct InlineReady {
    /// SVG stays so a later zoom can re-raster without asking Typst again.
    pub svg: String,
    pub texture: Option<egui::TextureHandle>,
    /// Alpha-identical white copy used by the fixed-contrast live preview.
    pub white_texture: Option<egui::TextureHandle>,
    /// CSS-pixel multiplier used for `texture` (√2 buckets from the base 3×).
    pub raster_scale: f32,
    pub w_pt: f32,
    pub h_pt: f32,
    pub baseline_pt: Option<f32>,
    pub pdf: Option<Vec<u8>>,
    pub pdf_pending: bool,
    pub(crate) pdf_req: Option<u64>,
    /// In-flight sharper raster (`MathRender::req`), if any.
    pub(crate) reraster_req: Option<u64>,
}

enum CacheAction {
    Skip,
    Preview,
    Pdf,
}

/// What paint and the popup need from one cached equation.
#[derive(Clone)]
pub struct InlinePreview {
    pub pending: bool,
    pub texture: Option<egui::TextureHandle>,
    pub white_texture: Option<egui::TextureHandle>,
    pub width_pt: f32,
    pub height_pt: f32,
    pub error: Option<String>,
}

/// Block-math preview state stored per annotation on a tab.
pub struct MathPreview {
    pub source: String,
    pub size: f32,
    pub color: Rgb,
    pub req: u64,
    pub pending: bool,
    pub texture: Option<egui::TextureHandle>,
    pub width_pt: f32,
    pub height_pt: f32,
    pub pdf: Option<Vec<u8>>,
    pub pdf_pending: bool,
    pub pdf_req: Option<u64>,
    pub error: Option<String>,
}

impl MathPreview {
    /// True when the block preview has a final PDF or a terminal error.
    pub fn is_settled(&self) -> bool {
        !self.pending && (self.pdf.is_some() || self.error.is_some())
    }
}

/// One visible document pane for [`MathPipeline::prepare_visible`].
pub struct VisibleMathPane<'a> {
    pub doc_id: DocId,
    pub epoch: u64,
    pub scale: f32,
    pub first_page: usize,
    pub last_page: usize,
    pub annotations: &'a [Arc<Annotation>],
    pub previews: &'a HashMap<u64, MathPreview>,
}

/// Side effects for the app after a block-math reply is applied.
pub enum BlockReplyEffect {
    /// Reply ignored (stale req, missing tab data, etc.).
    Ignored,
    /// PDF bytes landed on the preview; app should try finishing a math-gated save.
    PdfUpdated,
    /// Preview removed because the annot no longer exists.
    Removed,
    /// Raster (or error) applied. `auto_size` is the new PDF size when the annot wants it.
    Applied { auto_size: Option<(f32, f32)> },
}

/// Owns cache, deadlines, scan stamps, and the worker behind one small interface.
pub struct MathPipeline {
    worker: MathWorker,
    pub cache: super::MathCache<InlineReady>,
    seq: u64,
    /// Per-key debounce. `(doc_id, annot id, span key)`; `None` is a standalone math annot.
    /// A key already waiting is not postponed, so one frame cannot drop the others.
    deadlines: HashMap<(DocId, u64, Option<u64>), Instant>,
    /// Last successful key for `(doc_id, annot id, span ordinal)`, so a later error
    /// can still find the previous good render.
    last_good: HashMap<(DocId, u64, usize), MathKey>,
    /// Last prepare_visible scan: doc_id → (epoch, cache rev, first, last, zoom×ppp bits).
    scan: HashMap<DocId, (u64, u64, usize, usize, u32)>,
}

impl MathPipeline {
    pub fn spawn(ctx: egui::Context) -> Self {
        Self {
            worker: MathWorker::spawn(ctx),
            cache: super::MathCache::new(),
            seq: 1,
            deadlines: HashMap::new(),
            last_good: HashMap::new(),
            scan: HashMap::new(),
        }
    }

    pub fn poll(&self) -> Vec<MathRender> {
        self.worker.poll()
    }

    /// First request for a key starts the timer. Later calls in the same burst
    /// do not push it back, and they do not erase a different key's timer.
    pub fn arm_deadline(&mut self, doc_id: DocId, id: u64, span_key: Option<u64>) {
        self.deadlines
            .entry((doc_id, id, span_key))
            .or_insert_with(|| Instant::now() + DEADLINE_DEBOUNCE);
    }

    pub fn clear_deadlines(&mut self) {
        self.deadlines.clear();
    }

    pub fn has_deadlines(&self) -> bool {
        !self.deadlines.is_empty()
    }

    pub fn soonest_deadline(&self) -> Option<Instant> {
        self.deadlines.values().copied().min()
    }

    pub fn forget_doc(&mut self, doc_id: DocId) {
        self.scan.remove(&doc_id);
        self.deadlines.retain(|(g, _, _), _| *g != doc_id);
        self.last_good.retain(|(g, _, _), _| *g != doc_id);
    }

    pub fn forget_annot(&mut self, doc_id: DocId, id: u64) {
        self.last_good
            .retain(|&(g, annot, _), _| g != doc_id || annot != id);
        self.deadlines
            .retain(|&(g, annot, _), _| g != doc_id || annot != id);
    }

    /// Fire due deadlines. `span_is_live` decides Live vs Background for inline spans.
    pub fn flush_due(
        &mut self,
        mut span_is_live: impl FnMut(DocId, u64, u64) -> bool,
        mut block: impl FnMut(&mut Self, DocId, u64),
        mut inline: impl FnMut(&mut Self, DocId, u64, u64, bool),
    ) {
        let now = Instant::now();
        let due: Vec<(DocId, u64, Option<u64>)> = self
            .deadlines
            .iter()
            .filter(|(_, when)| **when <= now)
            .map(|(key, _)| *key)
            .collect();
        for (doc_id, id, span_key) in due {
            self.deadlines.remove(&(doc_id, id, span_key));
            if let Some(span_key) = span_key {
                let live = span_is_live(doc_id, id, span_key);
                inline(self, doc_id, id, span_key, live);
            } else {
                block(self, doc_id, id);
            }
        }
    }

    /// Re-scan only when the document, the cache, or the visible page range changes.
    ///
    /// Inline/reraster work is enqueued here. Block-math ids are returned so the
    /// app adapter can pass each tab's mutable preview map into [`Self::request_block`].
    pub fn prepare_visible(
        &mut self,
        panes: &[VisibleMathPane<'_>],
        pixels_per_point: f32,
    ) -> Vec<(DocId, u64)> {
        let revision = self.cache.revision();
        let mut queued: Vec<(DocId, u64, u64, String, f32, Rgb, bool)> = Vec::new();
        let mut reraster: Vec<(DocId, u64, u64, MathKey, f32)> = Vec::new();
        let mut block: Vec<(DocId, u64)> = Vec::new();
        let mut scanned: Vec<(DocId, u64, usize, usize, u32)> = Vec::new();
        for pane in panes {
            let needed = pane.scale * pixels_per_point.max(0.5);
            let needed_bits = needed.to_bits();
            let stamp = (
                pane.epoch,
                revision,
                pane.first_page,
                pane.last_page,
                needed_bits,
            );
            if self.scan.get(&pane.doc_id) == Some(&stamp) {
                continue;
            }
            let doc_id = pane.doc_id;
            for annot in pane.annotations {
                if annot.page < pane.first_page || annot.page > pane.last_page {
                    continue;
                }
                match &annot.kind {
                    AnnotKind::Math { source, .. } if !source.trim().is_empty() => {
                        if needs_block_math(pane.previews, annot.id, &annot.kind) {
                            block.push((doc_id, annot.id));
                        }
                    }
                    AnnotKind::Text {
                        content,
                        size,
                        color,
                        ..
                    } => {
                        for span in math_spans::closed_math_spans(content) {
                            let inner = &content[span.inner_start..span.inner_end];
                            if inner.trim().is_empty() {
                                continue;
                            }
                            let cache_key = MathKey::new(inner, span.display, *size, *color);
                            match self.cache.get(&cache_key) {
                                Some(EntryKind::Pending { .. } | EntryKind::Error { .. }) => {}
                                Some(EntryKind::Ready { value, .. }) => {
                                    if value.reraster_req.is_none()
                                        && !value.svg.is_empty()
                                        && needs_sharper_raster(value.raster_scale, needed)
                                    {
                                        let scale = raster_scale_for(needed);
                                        if scale > value.raster_scale + f32::EPSILON {
                                            reraster.push((
                                                doc_id,
                                                annot.id,
                                                math_spans::span_key(inner, span.display),
                                                cache_key,
                                                scale,
                                            ));
                                        }
                                    }
                                }
                                None => queued.push((
                                    doc_id,
                                    annot.id,
                                    math_spans::span_key(inner, span.display),
                                    math_spans::source_for_render(inner, span.display),
                                    *size,
                                    *color,
                                    span.display,
                                )),
                            }
                        }
                    }
                    AnnotKind::Math { .. }
                    | AnnotKind::Highlight { .. }
                    | AnnotKind::Markup { .. }
                    | AnnotKind::Note { .. }
                    | AnnotKind::Shape { .. }
                    | AnnotKind::Image { .. }
                    | AnnotKind::Foreign { .. } => {}
                }
            }
            scanned.push((
                doc_id,
                pane.epoch,
                pane.first_page,
                pane.last_page,
                needed_bits,
            ));
        }
        for (doc_id, id, span_key, source, size, color, display) in queued {
            self.request_inline_with(
                doc_id,
                id,
                span_key,
                source,
                size,
                color,
                display,
                MathLane::Background,
                false,
            );
        }
        for (doc_id, id, span_key, key, scale) in reraster {
            self.request_inline_reraster(doc_id, id, span_key, key, scale);
        }
        let revision = self.cache.revision();
        for (doc_id, epoch, first, last, needed_bits) in scanned {
            self.scan
                .insert(doc_id, (epoch, revision, first, last, needed_bits));
        }
        block
    }

    pub fn request_block(
        &mut self,
        doc_id: DocId,
        id: u64,
        source: String,
        size: f32,
        color: Rgb,
        previews: &mut HashMap<u64, MathPreview>,
    ) {
        if source.trim().is_empty() {
            return;
        }
        if let Some(preview) = previews.get(&id) {
            if preview.pending
                && preview.source == source
                && (preview.size - size).abs() < 0.05
                && preview.color == color
            {
                return;
            }
            if !preview.pending
                && preview.source == source
                && (preview.size - size).abs() < 0.05
                && preview.color == color
                && (preview.texture.is_some() || preview.error.is_some())
            {
                return;
            }
        }
        let texture = previews
            .get(&id)
            .and_then(|preview| preview.texture.clone());
        let width_pt = previews.get(&id).map(|p| p.width_pt).unwrap_or(0.0);
        let height_pt = previews.get(&id).map(|p| p.height_pt).unwrap_or(0.0);
        self.seq += 1;
        let req = self.seq;
        previews.insert(
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
                pdf_pending: false,
                pdf_req: None,
                error: None,
            },
        );
        self.worker.request(doc_id, id, req, source, size, color);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn request_inline(
        &mut self,
        doc_id: DocId,
        id: u64,
        span_key: u64,
        live: bool,
        content: &str,
        size: f32,
        color: Rgb,
    ) {
        let Some(span) = math_spans::closed_math_spans(content)
            .into_iter()
            .find(|span| {
                let inner = &content[span.inner_start..span.inner_end];
                math_spans::span_key(inner, span.display) == span_key
            })
        else {
            return;
        };
        let inner = &content[span.inner_start..span.inner_end];
        if inner.trim().is_empty() {
            return;
        }
        let source = math_spans::source_for_render(inner, span.display);
        let display = span.display;
        let lane = if live {
            MathLane::Live
        } else {
            MathLane::Background
        };
        self.request_inline_with(
            doc_id, id, span_key, source, size, color, display, lane, false,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn request_inline_with(
        &mut self,
        doc_id: DocId,
        id: u64,
        span_key: u64,
        source: String,
        size: f32,
        color: Rgb,
        display: bool,
        lane: MathLane,
        want_pdf: bool,
    ) {
        let key = MathKey::new(&source, display, size, color);
        let action = match self.cache.get(&key) {
            Some(EntryKind::Pending { .. }) => CacheAction::Skip,
            Some(EntryKind::Error { .. }) => CacheAction::Skip,
            Some(EntryKind::Ready { value, .. }) => {
                if want_pdf && value.pdf.is_none() && !value.pdf_pending {
                    CacheAction::Pdf
                } else {
                    CacheAction::Skip
                }
            }
            None => {
                if want_pdf {
                    CacheAction::Skip
                } else {
                    CacheAction::Preview
                }
            }
        };
        match action {
            CacheAction::Skip => {}
            CacheAction::Preview => {
                self.seq += 1;
                let req = self.seq;
                self.cache.insert_pending(key, req);
                self.worker.request_span(
                    doc_id,
                    id,
                    Some(span_key),
                    req,
                    source,
                    size,
                    color,
                    display,
                    lane,
                    false,
                );
            }
            CacheAction::Pdf => {
                self.seq += 1;
                let req = self.seq;
                if let Some(ready) = self.cache.ready_mut(&key) {
                    ready.pdf_pending = true;
                    ready.pdf_req = Some(req);
                }
                self.worker.request_span(
                    doc_id,
                    id,
                    Some(span_key),
                    req,
                    source,
                    size,
                    color,
                    display,
                    MathLane::Background,
                    true,
                );
            }
        }
    }

    /// Enqueue a background preview for math in an assistant answer.
    ///
    /// Keys the LT0 cache with no annotation id (`id = 0`) and never asks for
    /// PDF bytes — these textures are UI-only and must not land in the document.
    pub fn request_answer_math(
        &mut self,
        doc_id: DocId,
        source: String,
        display: bool,
        color: Rgb,
    ) {
        if source.trim().is_empty() {
            return;
        }
        let size = ASSISTANT_MATH_SIZE;
        let key = MathKey::new(&source, display, size, color);
        if self.cache.get(&key).is_some() {
            return;
        }
        self.seq += 1;
        let req = self.seq;
        let span_key = math_spans::span_key(&key.source, display);
        self.cache.insert_pending(key, req);
        self.worker.request_span(
            doc_id,
            0,
            Some(span_key),
            req,
            source,
            size,
            color,
            display,
            MathLane::Background,
            false,
        );
    }

    pub fn request_inline_reraster(
        &mut self,
        doc_id: DocId,
        id: u64,
        span_key: u64,
        key: MathKey,
        raster_scale: f32,
    ) {
        let Some(ready) = self.cache.ready_mut(&key) else {
            return;
        };
        if ready.reraster_req.is_some() || ready.svg.is_empty() {
            return;
        }
        if raster_scale <= ready.raster_scale + f32::EPSILON {
            return;
        }
        self.seq += 1;
        let req = self.seq;
        let svg = ready.svg.clone();
        let width_pt = ready.w_pt;
        let height_pt = ready.h_pt;
        let baseline_pt = ready.baseline_pt;
        ready.reraster_req = Some(req);
        self.worker.request_reraster(
            doc_id,
            id,
            Some(span_key),
            req,
            key,
            svg,
            raster_scale,
            width_pt,
            height_pt,
            baseline_pt,
        );
    }

    pub fn request_block_pdf(
        &mut self,
        doc_id: DocId,
        id: u64,
        source: String,
        size: f32,
        color: Rgb,
        previews: &mut HashMap<u64, MathPreview>,
    ) {
        let Some(preview) = previews.get(&id) else {
            return;
        };
        if preview.pending || preview.pdf_pending || preview.is_settled() {
            return;
        }
        let display = wants_display_math(&source);
        self.seq += 1;
        let req = self.seq;
        if let Some(preview) = previews.get_mut(&id) {
            preview.pdf_pending = true;
            preview.pdf_req = Some(req);
        }
        self.worker.request_span(
            doc_id,
            id,
            None,
            req,
            source,
            size,
            color,
            display,
            MathLane::Background,
            true,
        );
    }

    /// Request any preview or PDF a dirty annotation still needs, and report
    /// whether the save snapshot can be built now.
    pub fn ensure_for_save(
        &mut self,
        doc_id: DocId,
        annotations: &[Arc<Annotation>],
        previews: &mut HashMap<u64, MathPreview>,
    ) -> bool {
        enum Job {
            BlockPreview(u64),
            BlockPdf(u64),
            Inline {
                id: u64,
                span_key: u64,
                source: String,
                size: f32,
                color: Rgb,
                display: bool,
                want_pdf: bool,
            },
        }
        let mut jobs = Vec::new();
        let mut waiting = false;
        for annot in annotations {
            if !annot.dirty {
                continue;
            }
            match &annot.kind {
                AnnotKind::Math { source, .. } => {
                    if crate::geom::normalize_latex(source).is_empty() {
                        continue;
                    }
                    match previews.get(&annot.id) {
                        Some(preview) if preview.is_settled() => {}
                        Some(preview) if preview.pending || preview.pdf_pending => waiting = true,
                        Some(preview)
                            if !preview.pending
                                && (preview.texture.is_some() || preview.width_pt > 0.0) =>
                        {
                            jobs.push(Job::BlockPdf(annot.id));
                        }
                        _ => jobs.push(Job::BlockPreview(annot.id)),
                    }
                }
                AnnotKind::Text {
                    content,
                    size,
                    color,
                    ..
                } if math_spans::has_math(content) => {
                    for span in math_spans::closed_math_spans(content) {
                        let inner = &content[span.inner_start..span.inner_end];
                        if inner.trim().is_empty() {
                            continue;
                        }
                        let key = MathKey::new(inner, span.display, *size, *color);
                        match self.cache.get(&key) {
                            Some(EntryKind::Error { .. }) => {}
                            Some(EntryKind::Pending { .. }) => waiting = true,
                            Some(EntryKind::Ready { value, .. }) => {
                                match pdf_plan(value.pdf_pending, true, value.pdf.is_some(), false)
                                {
                                    PdfPlan::Have => {}
                                    PdfPlan::Wait => waiting = true,
                                    PdfPlan::Request => jobs.push(Job::Inline {
                                        id: annot.id,
                                        span_key: math_spans::span_key(inner, span.display),
                                        source: math_spans::source_for_render(inner, span.display),
                                        size: *size,
                                        color: *color,
                                        display: span.display,
                                        want_pdf: true,
                                    }),
                                }
                            }
                            None => jobs.push(Job::Inline {
                                id: annot.id,
                                span_key: math_spans::span_key(inner, span.display),
                                source: math_spans::source_for_render(inner, span.display),
                                size: *size,
                                color: *color,
                                display: span.display,
                                want_pdf: false,
                            }),
                        }
                    }
                }
                AnnotKind::Text { .. }
                | AnnotKind::Highlight { .. }
                | AnnotKind::Markup { .. }
                | AnnotKind::Note { .. }
                | AnnotKind::Shape { .. }
                | AnnotKind::Image { .. }
                | AnnotKind::Foreign { .. } => {}
            }
        }
        if jobs.is_empty() && !waiting {
            return true;
        }
        for job in jobs {
            match job {
                Job::BlockPreview(id) => {
                    let Some(annot) = annotations.iter().find(|a| a.id == id) else {
                        continue;
                    };
                    let AnnotKind::Math {
                        source,
                        size,
                        color,
                        ..
                    } = &annot.kind
                    else {
                        continue;
                    };
                    self.request_block(doc_id, id, source.clone(), *size, *color, previews);
                }
                Job::BlockPdf(id) => {
                    let Some(annot) = annotations.iter().find(|a| a.id == id) else {
                        continue;
                    };
                    let AnnotKind::Math {
                        source,
                        size,
                        color,
                        ..
                    } = &annot.kind
                    else {
                        continue;
                    };
                    self.request_block_pdf(doc_id, id, source.clone(), *size, *color, previews);
                }
                Job::Inline {
                    id,
                    span_key,
                    source,
                    size,
                    color,
                    display,
                    want_pdf,
                } => self.request_inline_with(
                    doc_id,
                    id,
                    span_key,
                    source,
                    size,
                    color,
                    display,
                    MathLane::Background,
                    want_pdf,
                ),
            }
        }
        false
    }

    pub fn apply_block_reply(
        &mut self,
        ctx: &egui::Context,
        render: MathRender,
        source: Option<(String, f32, Rgb, bool, crate::geom::PdfRect)>,
        previews: &mut HashMap<u64, MathPreview>,
    ) -> BlockReplyEffect {
        if render.want_pdf {
            if let Some(preview) = previews.get_mut(&render.id) {
                if preview.pdf_req == Some(render.req) {
                    preview.pdf = render.pdf;
                    preview.pdf_pending = false;
                }
            }
            return BlockReplyEffect::PdfUpdated;
        }
        let Some((source, size, color, auto_size, rect)) = source else {
            previews.remove(&render.id);
            return BlockReplyEffect::Removed;
        };
        if previews
            .get(&render.id)
            .is_some_and(|preview| preview.req != render.req)
        {
            return BlockReplyEffect::Ignored;
        }
        let texture = render
            .preview
            .as_ref()
            .map(|image| upload_preview(ctx, render.doc_id, render.id, render.req, image));
        let previous = previews
            .get(&render.id)
            .and_then(|preview| preview.texture.clone());
        previews.insert(
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
                pdf_pending: false,
                pdf_req: None,
                error: render.error,
            },
        );
        let auto_size = if auto_size && render.width_pt > 1.0 && render.height_pt > 1.0 {
            let changed = (rect.width() - render.width_pt).abs() > 0.5
                || (rect.height() - render.height_pt).abs() > 0.5;
            if changed {
                Some((render.width_pt, render.height_pt))
            } else {
                None
            }
        } else {
            None
        };
        BlockReplyEffect::Applied { auto_size }
    }

    /// Apply an inline/cache reply. When `content` is present, a successful render
    /// updates [`Self::last_good`]. Returns whether the app should try finishing a
    /// math-gated save.
    pub fn apply_inline_reply(
        &mut self,
        ctx: &egui::Context,
        render: MathRender,
        content: Option<&str>,
    ) -> bool {
        if render.want_pdf {
            if let Some(ready) = self.cache.ready_mut(&render.key) {
                if ready.pdf_req == Some(render.req) {
                    ready.pdf = render.pdf;
                    ready.pdf_pending = false;
                }
            }
            return true;
        }
        let reraster_hit = self
            .cache
            .ready_mut(&render.key)
            .is_some_and(|ready| ready.reraster_req == Some(render.req));
        if reraster_hit {
            let bytes = render
                .preview
                .as_ref()
                .map(|image| image.width as usize * image.height as usize * 8);
            if let Some(ready) = self.cache.ready_mut(&render.key) {
                if let Some(image) = render.preview.as_ref() {
                    let pixels = egui::ColorImage::from_rgba_premultiplied(
                        [image.width as usize, image.height as usize],
                        &image.pixels,
                    );
                    if let Some(texture) = ready.texture.as_mut() {
                        texture.set(pixels, egui::TextureOptions::LINEAR);
                    } else {
                        let stamp = render.span_key.unwrap_or(0);
                        ready.texture = Some(upload_preview(
                            ctx,
                            render.doc_id,
                            render.id ^ stamp,
                            render.req,
                            image,
                        ));
                    }
                    let white_pixels = white_preview_image(image);
                    if let Some(texture) = ready.white_texture.as_mut() {
                        texture.set(white_pixels, egui::TextureOptions::LINEAR);
                    } else {
                        let stamp = render.span_key.unwrap_or(0);
                        ready.white_texture = Some(upload_white_preview(
                            ctx,
                            render.doc_id,
                            render.id ^ stamp,
                            render.req,
                            image,
                        ));
                    }
                    ready.raster_scale = render.raster_scale;
                }
                ready.reraster_req = None;
            }
            if let Some(bytes) = bytes {
                self.cache.set_ready_bytes(&render.key, bytes);
            }
            return false;
        }
        let accept = match self.cache.get(&render.key) {
            Some(EntryKind::Pending { req }) => *req == render.req,
            Some(EntryKind::Ready { .. } | EntryKind::Error { .. }) => false,
            None => true,
        };
        if !accept {
            return false;
        }
        if let Some(message) = render.error.clone() {
            if render.preview.is_none() {
                self.cache
                    .insert_error(render.key.clone(), map_typst_error(&message));
                return true;
            }
        }
        let bytes = render
            .preview
            .as_ref()
            .map(|image| image.width as usize * image.height as usize * 8)
            .unwrap_or(0);
        let texture = render.preview.as_ref().map(|image| {
            let stamp = render.span_key.unwrap_or(0);
            upload_preview(ctx, render.doc_id, render.id ^ stamp, render.req, image)
        });
        let white_texture = render.preview.as_ref().map(|image| {
            let stamp = render.span_key.unwrap_or(0);
            upload_white_preview(ctx, render.doc_id, render.id ^ stamp, render.req, image)
        });
        if let Some(content) = content {
            self.remember_good_from_content(&render, content);
        }
        self.cache.insert_ready(
            render.key.clone(),
            InlineReady {
                svg: render.svg.unwrap_or_default(),
                texture,
                white_texture,
                raster_scale: render.raster_scale,
                w_pt: render.width_pt,
                h_pt: render.height_pt,
                baseline_pt: render.baseline_pt,
                pdf: render.pdf,
                pdf_pending: false,
                pdf_req: None,
                reraster_req: None,
            },
            bytes,
        );
        true
    }

    pub fn remember_good_from_content(&mut self, render: &MathRender, content: &str) {
        if render.error.is_some() {
            return;
        }
        let Some(span_key) = render.span_key else {
            return;
        };
        let ordinal = math_spans::closed_math_spans(content)
            .into_iter()
            .find(|span| {
                let inner = &content[span.inner_start..span.inner_end];
                math_spans::span_key(inner, span.display) == span_key
            })
            .map(|span| span.index);
        if let Some(ordinal) = ordinal {
            self.last_good
                .insert((render.doc_id, render.id, ordinal), render.key.clone());
        }
    }

    pub fn last_good_inline(
        &self,
        doc_id: DocId,
        annot_id: u64,
        ordinal: usize,
    ) -> Option<InlinePreview> {
        let key = self.last_good.get(&(doc_id, annot_id, ordinal))?;
        match self.cache.get(key)? {
            EntryKind::Ready { value, .. } => Some(InlinePreview {
                pending: false,
                texture: value.texture.clone(),
                white_texture: value.white_texture.clone(),
                width_pt: value.w_pt,
                height_pt: value.h_pt,
                error: None,
            }),
            EntryKind::Pending { .. } | EntryKind::Error { .. } => None,
        }
    }

    pub fn preview_at(
        &self,
        inner: &str,
        display: bool,
        size: f32,
        color: Rgb,
    ) -> Option<InlinePreview> {
        let key = MathKey::new(inner, display, size, color);
        match self.cache.get(&key)? {
            EntryKind::Pending { .. } => Some(InlinePreview {
                pending: true,
                texture: None,
                white_texture: None,
                width_pt: 0.0,
                height_pt: 0.0,
                error: None,
            }),
            EntryKind::Error { message } => Some(InlinePreview {
                pending: false,
                texture: None,
                white_texture: None,
                width_pt: 0.0,
                height_pt: 0.0,
                error: Some(message.clone()),
            }),
            EntryKind::Ready { value, .. } => Some(InlinePreview {
                pending: false,
                texture: value.texture.clone(),
                white_texture: value.white_texture.clone(),
                width_pt: value.w_pt,
                height_pt: value.h_pt,
                error: None,
            }),
        }
    }

    pub fn inline_preview_for(
        &self,
        content: &str,
        size: f32,
        color: Rgb,
        span_key: u64,
    ) -> Option<InlinePreview> {
        let span = math_spans::closed_math_spans(content)
            .into_iter()
            .find(|span| {
                let inner = &content[span.inner_start..span.inner_end];
                math_spans::span_key(inner, span.display) == span_key
            })?;
        let inner = &content[span.inner_start..span.inner_end];
        self.preview_at(inner, span.display, size, color)
    }
}

fn needs_block_math(previews: &HashMap<u64, MathPreview>, id: u64, kind: &AnnotKind) -> bool {
    let AnnotKind::Math {
        source,
        size,
        color,
        ..
    } = kind
    else {
        return false;
    };
    if source.trim().is_empty() {
        return false;
    }
    match previews.get(&id) {
        Some(preview) => {
            preview.pending
                || preview.source != *source
                || (preview.size - *size).abs() > 0.05
                || preview.color != *color
        }
        None => true,
    }
}

pub fn upload_preview(
    ctx: &egui::Context,
    doc_id: DocId,
    id: u64,
    req: u64,
    image: &RgbaImage,
) -> egui::TextureHandle {
    let pixels = egui::ColorImage::from_rgba_premultiplied(
        [image.width as usize, image.height as usize],
        &image.pixels,
    );
    ctx.load_texture(
        format!("math-{doc_id}-{id}-{req}"),
        pixels,
        egui::TextureOptions::LINEAR,
    )
}

pub fn white_preview_image(image: &RgbaImage) -> egui::ColorImage {
    let mut pixels = image.pixels.clone();
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3];
        pixel[0] = alpha;
        pixel[1] = alpha;
        pixel[2] = alpha;
    }
    egui::ColorImage::from_rgba_premultiplied(
        [image.width as usize, image.height as usize],
        &pixels,
    )
}

pub fn upload_white_preview(
    ctx: &egui::Context,
    doc_id: DocId,
    id: u64,
    req: u64,
    image: &RgbaImage,
) -> egui::TextureHandle {
    ctx.load_texture(
        format!("math-white-{doc_id}-{id}-{req}"),
        white_preview_image(image),
        egui::TextureOptions::LINEAR,
    )
}
