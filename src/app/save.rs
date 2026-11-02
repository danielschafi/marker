//! Autosave and save job assembly (RF9b).

use std::path::PathBuf;
use std::time::Instant;

use crate::annot::AnnotKind;
use crate::math::MathCache;
use crate::math_spans;
use crate::pdf::{DocId, InlineMathSave, SaveSnapshot};
use crate::view;

use super::*;

impl MarkerApp {
    pub(crate) fn autosave(&mut self, ctx: &egui::Context) {
        let doc_ids: Vec<DocId> = self.tabs.iter().map(|tab| tab.doc.doc_id).collect();
        for doc_id in doc_ids {
            let due = {
                let Some(tab) = self.tab_by_doc_id_mut(doc_id) else {
                    continue;
                };
                let dirty = if tab.doc.session.epoch == tab.autosave_dirty_epoch {
                    tab.autosave_dirty
                } else {
                    let dirty = tab.doc.session.is_dirty();
                    tab.autosave_dirty_epoch = tab.doc.session.epoch;
                    tab.autosave_dirty = dirty;
                    dirty
                };
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
                tab.force_save || tab.save.autosave_due()
            };
            if !due {
                continue;
            }
            let ready = self.ensure_math_for_save(doc_id);
            if ready {
                if let Some(tab) = self.tab_by_doc_id_mut(doc_id) {
                    tab.force_save = false;
                }
                self.start_save_doc_id(doc_id, false);
            } else if let Some(tab) = self.tab_by_doc_id_mut(doc_id) {
                tab.save_when_math_ready = true;
            }
        }
    }

    pub(crate) fn flush_on_exit_if_needed(&mut self) {
        if self.exit_flush_done {
            return;
        }
        self.exit_flush_done = true;
        self.flush_on_exit();
    }

    pub(crate) fn flush_on_exit(&mut self) {
        const WAIT: Duration = Duration::from_secs(2);
        self.math_deadlines.clear();
        self.flush_math();
        let ctx = self.egui_ctx.clone();
        let doc_ids: Vec<DocId> = self
            .tabs
            .iter()
            .filter(|tab| {
                tab.doc.session.is_dirty()
                    || tab.save_when_math_ready
                    || matches!(tab.save, SaveState::Saving)
            })
            .map(|tab| tab.doc.doc_id)
            .collect();
        for doc_id in doc_ids {
            let dirty = self
                .tabs
                .iter()
                .find(|tab| tab.doc.doc_id == doc_id)
                .is_some_and(|tab| tab.doc.session.is_dirty());
            if dirty {
                let deadline = Instant::now() + WAIT;
                while Instant::now() < deadline && !self.ensure_math_for_save(doc_id) {
                    self.poll(&ctx);
                    thread::sleep(Duration::from_millis(5));
                }
            }
            let already_saving = self
                .tabs
                .iter()
                .find(|tab| tab.doc.doc_id == doc_id)
                .is_some_and(|tab| matches!(tab.save, SaveState::Saving));
            if !already_saving {
                self.start_save_doc_id(doc_id, true);
            }
            let deadline = Instant::now() + WAIT;
            while Instant::now() < deadline {
                let saving = self
                    .tabs
                    .iter()
                    .find(|tab| tab.doc.doc_id == doc_id)
                    .is_some_and(|tab| matches!(tab.save, SaveState::Saving));
                if !saving {
                    break;
                }
                self.poll(&ctx);
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    pub(crate) fn start_save_doc_id(&mut self, doc_id: DocId, quit: bool) {
        let dirty = self
            .tabs
            .iter()
            .find(|tab| tab.doc.doc_id == doc_id)
            .is_some_and(|tab| tab.doc.session.is_dirty());
        if dirty && !quit && !self.ensure_math_for_save(doc_id) {
            if let Some(tab) = self.tab_by_doc_id_mut(doc_id) {
                tab.save_when_math_ready = true;
            }
            return;
        }
        let (snapshot, close, detach) = {
            let Some(tab) = self.tabs.iter_mut().find(|tab| tab.doc.doc_id == doc_id) else {
                return;
            };
            if !tab.doc.session.is_dirty() {
                tab.save = SaveState::Clean;
                tab.force_save = false;
                tab.save_when_math_ready = false;
                let close = if quit { false } else { tab.close_after_save };
                let detach = if quit { false } else { tab.detach_after_save };
                (None, close, detach)
            } else {
                let epoch = tab.doc.session.epoch;
                let upserts: Vec<Annotation> = tab
                    .doc
                    .session
                    .cloned_annotations()
                    .into_iter()
                    .filter(|annot| annot.dirty)
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
                let (inline_math, rich_text_parents) =
                    collect_inline_math_saves(&upserts, &self.egui_ctx, &self.math_cache);
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
                        inline_math,
                        rich_text_parents,
                    }),
                    false,
                    false,
                )
            }
        };
        if let Some(snapshot) = snapshot {
            self.worker.save(doc_id, snapshot);
        }
        if quit {
            return;
        }
        if let Some(tab) = self.tabs.iter().find(|tab| tab.doc.doc_id == doc_id) {
            let tab_id = tab.id;
            if detach {
                self.finish_detach_tab(tab_id);
            } else if close {
                self.drop_tab(tab_id);
            }
        }
    }

    /// Write a full copy to `path` and switch the tab onto it (FT9).
    pub(crate) fn start_save_as(&mut self, doc_id: DocId, mut path: PathBuf) {
        if !self.tabs.iter().any(|tab| tab.doc.doc_id == doc_id) {
            return;
        }
        if path.extension().is_none() {
            path.set_extension("pdf");
        }
        let dirty = self
            .tabs
            .iter()
            .find(|tab| tab.doc.doc_id == doc_id)
            .is_some_and(|tab| tab.doc.session.is_dirty());
        if dirty && !self.ensure_math_for_save(doc_id) {
            // Dialog path is already chosen; wait briefly like quit flush.
            let ctx = self.egui_ctx.clone();
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline && !self.ensure_math_for_save(doc_id) {
                self.poll(&ctx);
                thread::sleep(Duration::from_millis(5));
            }
        }
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.doc.doc_id == doc_id) else {
            return;
        };
        if matches!(tab.save, SaveState::Saving) {
            return;
        }
        let epoch = tab.doc.session.epoch;
        let upserts: Vec<Annotation> = tab
            .doc
            .session
            .cloned_annotations()
            .into_iter()
            .filter(|annot| annot.dirty)
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
        let (inline_math, rich_text_parents) =
            collect_inline_math_saves(&upserts, &self.egui_ctx, &self.math_cache);
        tab.save_epoch = epoch;
        tab.save_deletes = deletes.clone();
        tab.save = SaveState::Saving;
        tab.save_when_math_ready = false;
        tab.force_save = false;
        let snapshot = SaveSnapshot {
            upserts,
            deletes,
            math_pdfs,
            inline_math,
            rich_text_parents,
        };
        self.worker.save_as(doc_id, path, snapshot);
    }
}

pub(crate) fn collect_inline_math_saves(
    upserts: &[Annotation],
    ctx: &egui::Context,
    cache: &MathCache<InlineReady>,
) -> (Vec<InlineMathSave>, Vec<(usize, u64)>) {
    let mut inline_math = Vec::new();
    let mut rich_text_parents = Vec::new();
    for annot in upserts {
        let AnnotKind::Text {
            rect,
            content,
            size,
            color,
        } = &annot.kind
        else {
            continue;
        };
        if !math_spans::has_math(content) {
            continue;
        }
        rich_text_parents.push((annot.page, annot.id));
        let measure = view::EguiMeasure { ctx };
        let runs = crate::math::rich_text::layout_rich_text(
            content,
            *size,
            rect.width(),
            &measure,
            &|inner, display| view::cached_math_metrics(cache, inner, display, *size, *color),
        );
        let mut index = 0u32;
        for run in runs {
            let crate::math::rich_text::LaidRun::Math {
                inner,
                display,
                x,
                y,
                w,
                h,
                ..
            } = run
            else {
                continue;
            };
            let pdf = match cache.get(&MathKey::new(&inner, display, *size, *color)) {
                Some(EntryKind::Ready { value, .. }) => value.pdf.clone(),
                Some(EntryKind::Pending { .. } | EntryKind::Error { .. }) | None => None,
            };
            inline_math.push(InlineMathSave {
                parent_id: annot.id,
                page: annot.page,
                index,
                rect: PdfRect::new(rect.x0 + x, rect.y0 + y, rect.x0 + x + w, rect.y0 + y + h),
                source: math_spans::source_for_render(&inner, display),
                size: *size,
                color: *color,
                pdf,
            });
            index += 1;
        }
    }
    (inline_math, rich_text_parents)
}
