//! Math worker replies, visible-math prep, and requests (RF9b).

use crate::annot::AnnotKind;
use crate::math::{
    map_typst_error, needs_sharper_raster, pdf_plan, raster_scale_for, EntryKind, MathKey,
    MathLane, MathRender, PdfPlan, RgbaImage,
};
use crate::math_spans;
use crate::pdf::DocId;

use super::*;

impl MarkerApp {
    pub(crate) fn on_math(&mut self, ctx: &egui::Context, render: MathRender) {
        if render.span_key.is_some() {
            self.on_inline_math(ctx, render);
            return;
        }
        if render.want_pdf {
            if let Some(tab) = self.tab_by_doc_id_mut(render.doc_id) {
                if let Some(preview) = tab.previews.get_mut(&render.id) {
                    if preview.pdf_req == Some(render.req) {
                        preview.pdf = render.pdf;
                        preview.pdf_pending = false;
                    }
                }
            }
            self.finish_math_save(render.doc_id);
            return;
        }
        let snapshot = self
            .tabs
            .iter()
            .find(|tab| tab.doc.doc_id == render.doc_id)
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
            if let Some(tab) = self.tab_by_doc_id_mut(render.doc_id) {
                tab.previews.remove(&render.id);
            }
            return;
        };
        {
            let Some(tab) = self.tab_by_doc_id_mut(render.doc_id) else {
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
                .map(|image| upload_preview(ctx, render.doc_id, render.id, render.req, image));
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
                    pdf_pending: false,
                    pdf_req: None,
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
                    tab.save.touch();
                }
            }
        }
        self.finish_math_save(render.doc_id);
    }

    pub(crate) fn on_inline_math(&mut self, ctx: &egui::Context, render: MathRender) {
        if render.want_pdf {
            if let Some(ready) = self.math_cache.ready_mut(&render.key) {
                if ready.pdf_req == Some(render.req) {
                    ready.pdf = render.pdf;
                    ready.pdf_pending = false;
                }
            }
            self.finish_math_save(render.doc_id);
            return;
        }
        // Sharper zoom raster: reuse the texture id and keep drawing until this lands.
        let reraster_hit = self
            .math_cache
            .ready_mut(&render.key)
            .is_some_and(|ready| ready.reraster_req == Some(render.req));
        if reraster_hit {
            let bytes = render
                .preview
                .as_ref()
                .map(|image| image.width as usize * image.height as usize * 8);
            if let Some(ready) = self.math_cache.ready_mut(&render.key) {
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
                self.math_cache.set_ready_bytes(&render.key, bytes);
            }
            return;
        }
        let accept = match self.math_cache.get(&render.key) {
            Some(EntryKind::Pending { req }) => *req == render.req,
            Some(EntryKind::Ready { .. } | EntryKind::Error { .. }) => false,
            None => true,
        };
        if !accept {
            return;
        }
        if let Some(message) = render.error.clone() {
            if render.preview.is_none() {
                self.math_cache
                    .insert_error(render.key, map_typst_error(&message));
                self.finish_math_save(render.doc_id);
                return;
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
        self.remember_good(&render);
        self.math_cache.insert_ready(
            render.key,
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
        self.finish_math_save(render.doc_id);
    }

    pub(crate) fn remember_good(&mut self, render: &MathRender) {
        if render.error.is_some() {
            return;
        }
        let Some(span_key) = render.span_key else {
            return;
        };
        let ordinal = self
            .tabs
            .iter()
            .find(|tab| tab.doc.doc_id == render.doc_id)
            .and_then(|tab| {
                let annot = tab.doc.session.get(render.id)?;
                let AnnotKind::Text { content, .. } = &annot.kind else {
                    return None;
                };
                math_spans::closed_math_spans(content)
                    .into_iter()
                    .find(|span| {
                        let inner = &content[span.inner_start..span.inner_end];
                        math_spans::span_key(inner, span.display) == span_key
                    })
                    .map(|span| span.index)
            });
        if let Some(ordinal) = ordinal {
            self.math_last_good
                .insert((render.doc_id, render.id, ordinal), render.key.clone());
        }
    }

    pub(crate) fn finish_math_save(&mut self, doc_id: DocId) {
        let waiting = self
            .tabs
            .iter()
            .find(|tab| tab.doc.doc_id == doc_id)
            .is_some_and(|tab| tab.save_when_math_ready);
        if waiting {
            self.start_save_doc_id(doc_id, false);
        }
    }

    pub(crate) fn flush_math(&mut self) {
        let now = Instant::now();
        let due: Vec<(DocId, u64, Option<u64>)> = self
            .math_deadlines
            .iter()
            .filter(|(_, when)| **when <= now)
            .map(|(key, _)| *key)
            .collect();
        for (doc_id, id, span_key) in due {
            self.math_deadlines.remove(&(doc_id, id, span_key));
            if let Some(span_key) = span_key {
                let live = self.span_is_live(doc_id, id, span_key);
                self.request_inline_math(doc_id, id, span_key, live);
            } else {
                self.request_math(doc_id, id);
            }
        }
    }

    pub(crate) fn span_is_live(&self, doc_id: DocId, id: u64, span_key: u64) -> bool {
        let Some(tab) = self.tabs.iter().find(|tab| tab.doc.doc_id == doc_id) else {
            return false;
        };
        let Some((edit_id, start)) = tab.live_math else {
            return false;
        };
        if edit_id != id {
            return false;
        }
        let Some(annot) = tab.doc.session.get(id) else {
            return false;
        };
        let AnnotKind::Text { content, .. } = &annot.kind else {
            return false;
        };
        math_spans::math_span_at(content, start).is_some_and(|span| {
            let inner = &content[span.inner_start..span.inner_end];
            math_spans::span_key(inner, span.display) == span_key
        })
    }

    /// Re-scan only when the document, the cache, or the visible page range changes.
    pub(crate) fn prepare_visible_math(&mut self, pixels_per_point: f32) {
        let mut panes: Vec<(usize, egui::Rect)> = Vec::new();
        if let Some(index) = self.tab_index(self.active) {
            panes.push((index, self.view_rect));
        }
        if let Some(split) = self.split {
            let other = split.other(self.active);
            if other != self.active {
                if let Some(index) = self.tab_index(other) {
                    panes.push((index, self.split_view_rect));
                }
            }
        }
        let revision = self.math_cache.revision();
        let mut queued: Vec<(DocId, u64, u64, String, f32, Rgb, bool)> = Vec::new();
        let mut reraster: Vec<(DocId, u64, u64, MathKey, f32)> = Vec::new();
        let mut block: Vec<(DocId, u64)> = Vec::new();
        let mut scanned: Vec<(DocId, u64, usize, usize, u32)> = Vec::new();
        for (index, view) in panes {
            let tab = &self.tabs[index];
            let (first, last) = view::visible_pages(&tab.doc, view);
            let needed = tab.doc.scale * pixels_per_point.max(0.5);
            let needed_bits = needed.to_bits();
            let stamp = (tab.doc.session.epoch, revision, first, last, needed_bits);
            if self.math_scan.get(&tab.doc.doc_id) == Some(&stamp) {
                continue;
            }
            let doc_id = tab.doc.doc_id;
            for annot in &tab.doc.session.annotations {
                if annot.page < first || annot.page > last {
                    continue;
                }
                match &annot.kind {
                    AnnotKind::Math { source, .. } if !source.trim().is_empty() => {
                        if needs_math(tab, annot.id) {
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
                            match self.math_cache.get(&cache_key) {
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
            scanned.push((doc_id, tab.doc.session.epoch, first, last, needed_bits));
        }
        for (doc_id, id) in block {
            self.request_math(doc_id, id);
        }
        for (doc_id, id, span_key, source, size, color, display) in queued {
            self.request_inline_math_with(
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
        let revision = self.math_cache.revision();
        for (doc_id, epoch, first, last, needed_bits) in scanned {
            self.math_scan
                .insert(doc_id, (epoch, revision, first, last, needed_bits));
        }
    }

    pub(crate) fn request_math(&mut self, doc_id: DocId, id: u64) {
        let payload = {
            let Some(tab) = self.tab_by_doc_id_mut(doc_id) else {
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
        if let Some(tab) = self.tab_by_doc_id_mut(doc_id) {
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
                    pdf_pending: false,
                    pdf_req: None,
                    error: None,
                },
            );
        }
        self.math.request(doc_id, id, req, source, size, color);
    }

    pub(crate) fn request_inline_math(
        &mut self,
        doc_id: DocId,
        id: u64,
        span_key: u64,
        live: bool,
    ) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.doc.doc_id == doc_id) else {
            return;
        };
        let Some(annot) = tab.doc.session.get(id) else {
            return;
        };
        let AnnotKind::Text {
            content,
            size,
            color,
            ..
        } = &annot.kind
        else {
            return;
        };
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
        let size = *size;
        let color = *color;
        let lane = if live {
            MathLane::Live
        } else {
            MathLane::Background
        };
        self.request_inline_math_with(
            doc_id, id, span_key, source, size, color, display, lane, false,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn request_inline_math_with(
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
        let action = match self.math_cache.get(&key) {
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
                self.math_seq += 1;
                let req = self.math_seq;
                self.math_cache.insert_pending(key, req);
                self.math.request_span(
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
                self.math_seq += 1;
                let req = self.math_seq;
                if let Some(ready) = self.math_cache.ready_mut(&key) {
                    ready.pdf_pending = true;
                    ready.pdf_req = Some(req);
                }
                self.math.request_span(
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
    pub(crate) fn request_answer_math(&mut self, source: String, display: bool, color: Rgb) {
        let Some(doc_id) = self.tab().map(|tab| tab.doc.doc_id) else {
            return;
        };
        if source.trim().is_empty() {
            return;
        }
        let size = Self::ASSISTANT_MATH_SIZE;
        let key = MathKey::new(&source, display, size, color);
        if self.math_cache.get(&key).is_some() {
            return;
        }
        self.math_seq += 1;
        let req = self.math_seq;
        let span_key = math_spans::span_key(&key.source, display);
        self.math_cache.insert_pending(key, req);
        self.math.request_span(
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

    pub(crate) fn request_inline_reraster(
        &mut self,
        doc_id: DocId,
        id: u64,
        span_key: u64,
        key: MathKey,
        raster_scale: f32,
    ) {
        let Some(ready) = self.math_cache.ready_mut(&key) else {
            return;
        };
        if ready.reraster_req.is_some() || ready.svg.is_empty() {
            return;
        }
        if raster_scale <= ready.raster_scale + f32::EPSILON {
            return;
        }
        self.math_seq += 1;
        let req = self.math_seq;
        let svg = ready.svg.clone();
        let width_pt = ready.w_pt;
        let height_pt = ready.h_pt;
        let baseline_pt = ready.baseline_pt;
        ready.reraster_req = Some(req);
        self.math.request_reraster(
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

    pub(crate) fn request_block_pdf(&mut self, doc_id: DocId, id: u64) {
        let payload = {
            let Some(tab) = self.tab_by_doc_id_mut(doc_id) else {
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
            let Some(preview) = tab.previews.get(&id) else {
                return;
            };
            if preview.pending || preview.pdf_pending || preview.is_settled() {
                return;
            }
            (
                source.clone(),
                *size,
                *color,
                crate::math::wants_display_math(source),
            )
        };
        let (source, size, color, display) = payload;
        self.math_seq += 1;
        let req = self.math_seq;
        if let Some(tab) = self.tab_by_doc_id_mut(doc_id) {
            if let Some(preview) = tab.previews.get_mut(&id) {
                preview.pdf_pending = true;
                preview.pdf_req = Some(req);
            }
        }
        self.math.request_span(
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
    pub(crate) fn ensure_math_for_save(&mut self, doc_id: DocId) -> bool {
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
        let Some(tab) = self.tabs.iter().find(|tab| tab.doc.doc_id == doc_id) else {
            return true;
        };
        for annot in &tab.doc.session.annotations {
            if !annot.dirty {
                continue;
            }
            match &annot.kind {
                AnnotKind::Math { source, .. } => {
                    if crate::geom::normalize_latex(source).is_empty() {
                        continue;
                    }
                    match tab.previews.get(&annot.id) {
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
                        match self.math_cache.get(&key) {
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
                Job::BlockPreview(id) => self.request_math(doc_id, id),
                Job::BlockPdf(id) => self.request_block_pdf(doc_id, id),
                Job::Inline {
                    id,
                    span_key,
                    source,
                    size,
                    color,
                    display,
                    want_pdf,
                } => self.request_inline_math_with(
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

    pub(crate) fn inline_preview(&self, annot_id: u64, span_key: u64) -> Option<InlinePreview> {
        let tab = self.tab()?;
        let annot = tab.doc.session.get(annot_id)?;
        let AnnotKind::Text {
            content,
            size,
            color,
            ..
        } = &annot.kind
        else {
            return None;
        };
        let span = math_spans::closed_math_spans(content)
            .into_iter()
            .find(|span| {
                let inner = &content[span.inner_start..span.inner_end];
                math_spans::span_key(inner, span.display) == span_key
            })?;
        let inner = &content[span.inner_start..span.inner_end];
        self.preview_at(inner, span.display, *size, *color)
    }

    /// Last successful render of this span, kept while the current source is invalid.
    pub(crate) fn last_good_inline(&self, annot_id: u64, ordinal: usize) -> Option<InlinePreview> {
        let doc_id = self.tab()?.doc.doc_id;
        let key = self.math_last_good.get(&(doc_id, annot_id, ordinal))?;
        match self.math_cache.get(key)? {
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

    pub(crate) fn preview_at(
        &self,
        inner: &str,
        display: bool,
        size: f32,
        color: Rgb,
    ) -> Option<InlinePreview> {
        let key = MathKey::new(inner, display, size, color);
        match self.math_cache.get(&key)? {
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
}

pub(crate) fn needs_math(tab: &Tab, id: u64) -> bool {
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

pub(crate) fn upload_preview(
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

pub(crate) fn white_preview_image(image: &RgbaImage) -> egui::ColorImage {
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

pub(crate) fn upload_white_preview(
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

pub(crate) fn upload_png_texture(
    ctx: &egui::Context,
    doc_id: DocId,
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
        format!("assistant-crop-{doc_id}-{seq}"),
        pixels,
        egui::TextureOptions::LINEAR,
    ))
}
