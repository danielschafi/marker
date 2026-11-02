//! Thin MarkerApp adapter over [`crate::math::MathPipeline`].
//!
//! Tab binding and egui save triggers stay here; cache /
//! deadline / scan / request / reply policy lives in the pipeline module.

use crate::annot::AnnotKind;
use crate::math::{BlockReplyEffect, VisibleMathPane};
use crate::math_spans;
use crate::pdf::DocId;

use super::*;

impl MarkerApp {
    pub(crate) fn on_math(&mut self, ctx: &egui::Context, render: crate::math::MathRender) {
        if render.span_key.is_some() {
            self.on_inline_math(ctx, render);
            return;
        }
        let doc_id = render.doc_id;
        let id = render.id;
        let source = self
            .tabs
            .iter()
            .find(|tab| tab.doc.doc_id == doc_id)
            .and_then(|tab| {
                let annot = tab.doc.session.get(id)?;
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
        let Some(index) = self.tabs.iter().position(|tab| tab.doc.doc_id == doc_id) else {
            return;
        };
        let effect =
            self.math
                .apply_block_reply(ctx, render, source, &mut self.tabs[index].previews);
        match effect {
            BlockReplyEffect::Ignored => {}
            BlockReplyEffect::PdfUpdated | BlockReplyEffect::Removed => {
                self.finish_math_save(doc_id);
            }
            BlockReplyEffect::Applied { auto_size } => {
                if let Some((width_pt, height_pt)) = auto_size {
                    if let Some(annot) = self.tabs[index].doc.session.get_mut(id) {
                        if let AnnotKind::Math { rect, .. } = &mut annot.kind {
                            rect.x1 = rect.x0 + width_pt;
                            rect.y1 = rect.y0 + height_pt;
                        }
                    }
                    self.tabs[index].doc.session.mark_dirty(id);
                    self.tabs[index].save.touch();
                }
                self.finish_math_save(doc_id);
            }
        }
    }

    pub(crate) fn on_inline_math(&mut self, ctx: &egui::Context, render: crate::math::MathRender) {
        let doc_id = render.doc_id;
        let id = render.id;
        let content = self
            .tabs
            .iter()
            .find(|tab| tab.doc.doc_id == doc_id)
            .and_then(|tab| {
                let annot = tab.doc.session.get(id)?;
                let AnnotKind::Text { content, .. } = &annot.kind else {
                    return None;
                };
                Some(content.clone())
            });
        let finish = self
            .math
            .apply_inline_reply(ctx, render, content.as_deref());
        if finish {
            self.finish_math_save(doc_id);
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
        let mut pending_block: Vec<(DocId, u64)> = Vec::new();
        let mut pending_inline: Vec<(DocId, u64, u64, bool)> = Vec::new();
        {
            let tabs = &self.tabs;
            self.math.flush_due(
                |doc_id, id, span_key| span_is_live(tabs, doc_id, id, span_key),
                |_pipe, doc_id, id| pending_block.push((doc_id, id)),
                |_pipe, doc_id, id, span_key, live| {
                    pending_inline.push((doc_id, id, span_key, live))
                },
            );
        }
        for (doc_id, id) in pending_block {
            self.request_math(doc_id, id);
        }
        for (doc_id, id, span_key, live) in pending_inline {
            self.request_inline_math(doc_id, id, span_key, live);
        }
    }

    /// Re-scan only when the document, the cache, or the visible page range changes.
    pub(crate) fn prepare_visible_math(&mut self, pixels_per_point: f32) {
        let mut panes_meta: Vec<(usize, egui::Rect)> = Vec::new();
        if let Some(index) = self.tab_index(self.active) {
            panes_meta.push((index, self.view_rect));
        }
        if let Some(split) = self.split {
            let other = split.other(self.active);
            if other != self.active {
                if let Some(index) = self.tab_index(other) {
                    panes_meta.push((index, self.split_view_rect));
                }
            }
        }
        let page_ranges: Vec<(usize, usize, usize)> = panes_meta
            .iter()
            .map(|(index, view)| {
                let tab = &self.tabs[*index];
                let (first, last) = view::visible_pages(&tab.doc, *view);
                (*index, first, last)
            })
            .collect();
        let panes: Vec<VisibleMathPane<'_>> = page_ranges
            .iter()
            .map(|(index, first, last)| {
                let tab = &self.tabs[*index];
                VisibleMathPane {
                    doc_id: tab.doc.doc_id,
                    epoch: tab.doc.session.epoch,
                    scale: tab.doc.scale,
                    first_page: *first,
                    last_page: *last,
                    annotations: &tab.doc.session.annotations,
                    previews: &tab.previews,
                }
            })
            .collect();
        let blocks = self.math.prepare_visible(&panes, pixels_per_point);
        for (doc_id, id) in blocks {
            self.request_math(doc_id, id);
        }
    }

    pub(crate) fn request_math(&mut self, doc_id: DocId, id: u64) {
        let Some(index) = self.tabs.iter().position(|tab| tab.doc.doc_id == doc_id) else {
            return;
        };
        let payload = {
            let tab = &self.tabs[index];
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
            (source.clone(), *size, *color)
        };
        let (source, size, color) = payload;
        self.math.request_block(
            doc_id,
            id,
            source,
            size,
            color,
            &mut self.tabs[index].previews,
        );
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
        self.math
            .request_inline(doc_id, id, span_key, live, content, *size, *color);
    }

    /// Request any preview or PDF a dirty annotation still needs, and report
    /// whether the save snapshot can be built now.
    pub(crate) fn ensure_math_for_save(&mut self, doc_id: DocId) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| tab.doc.doc_id == doc_id) else {
            return true;
        };
        let annotations = self.tabs[index].doc.session.annotations.clone();
        self.math.ensure_for_save(
            doc_id,
            annotations.as_slice(),
            &mut self.tabs[index].previews,
        )
    }

    pub(crate) fn inline_preview(
        &self,
        annot_id: u64,
        span_key: u64,
    ) -> Option<crate::math::InlinePreview> {
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
        self.math
            .inline_preview_for(content, *size, *color, span_key)
    }

    /// Last successful render of this span, kept while the current source is invalid.
    pub(crate) fn last_good_inline(
        &self,
        annot_id: u64,
        ordinal: usize,
    ) -> Option<crate::math::InlinePreview> {
        let doc_id = self.tab()?.doc.doc_id;
        self.math.last_good_inline(doc_id, annot_id, ordinal)
    }
}

fn span_is_live(tabs: &[Tab], doc_id: DocId, id: u64, span_key: u64) -> bool {
    let Some(tab) = tabs.iter().find(|tab| tab.doc.doc_id == doc_id) else {
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
