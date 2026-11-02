//! PDF worker replies, page insert/delete, and tile upload (RF9b).

use crate::pdf::{DocId, PdfReply};
use crate::undo::PendingPageOp;

use super::*;

impl MarkerApp {
    pub(crate) fn poll_dialog(&mut self) {
        match self.dialog_rx.try_recv() {
            Ok(DialogReply::Open(Some(paths))) => {
                self.dialog_busy = false;
                for path in paths {
                    self.open_path(path);
                }
            }
            Ok(DialogReply::Open(None)) => self.dialog_busy = false,
            Ok(DialogReply::SaveAs {
                doc_id,
                path: Some(path),
            }) => {
                self.dialog_busy = false;
                self.start_save_as(doc_id, path);
            }
            Ok(DialogReply::SaveAs { path: None, .. }) => self.dialog_busy = false,
            Ok(DialogReply::ExportMarkdown {
                doc_id,
                path: Some(path),
            }) => {
                self.dialog_busy = false;
                self.write_export_markdown(doc_id, path);
            }
            Ok(DialogReply::ExportMarkdown { path: None, .. }) => {
                self.dialog_busy = false;
            }
            Ok(DialogReply::ExportCards {
                doc_id,
                path: Some(path),
            }) => {
                self.dialog_busy = false;
                self.write_export_cards(doc_id, path);
            }
            Ok(DialogReply::ExportCards { path: None, .. }) => {
                self.dialog_busy = false;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.dialog_busy = false,
        }
    }

    pub(crate) fn poll(&mut self, ctx: &egui::Context) {
        for reply in self.worker.poll() {
            match reply {
                PdfReply::Tile { .. } => self.pending_pdf_tiles.push_back(reply),
                other => self.on_pdf(ctx, other),
            }
        }
        self.upload_pending_tiles(ctx);
        for reply in self.math.poll() {
            self.on_math(ctx, reply);
        }
    }

    pub(crate) fn upload_pending_tiles(&mut self, ctx: &egui::Context) {
        const MAX_TILES: usize = 4;
        const MAX_BYTES: usize = 16 * 1024 * 1024;
        let mut count = 0usize;
        let mut bytes = 0usize;
        while count < MAX_TILES {
            let nbytes = match self.pending_pdf_tiles.front() {
                Some(PdfReply::Tile { tile, .. }) => tile.pixels.len(),
                _ => break,
            };
            if count > 0 && bytes + nbytes > MAX_BYTES {
                break;
            }
            bytes += nbytes;
            count += 1;
            let reply = self.pending_pdf_tiles.pop_front().expect("front checked");
            self.on_pdf(ctx, reply);
        }
        if self
            .pending_pdf_tiles
            .iter()
            .any(|reply| matches!(reply, PdfReply::Tile { .. }))
        {
            ctx.request_repaint();
        }
    }

    pub(crate) fn on_pdf(&mut self, ctx: &egui::Context, reply: PdfReply) {
        match reply {
            PdfReply::Opened(opened) => {
                self.opening.remove(&opened.doc_id);
                if self.tabs.iter().any(|tab| tab.doc.path == opened.path) {
                    self.worker.close(opened.doc_id);
                    if let Some(tab) = self.tabs.iter().find(|tab| tab.doc.path == opened.path) {
                        let id = tab.id;
                        if self.active != id {
                            self.active = id;
                            self.pulse_tab_list();
                        }
                    }
                    self.settings.remember_open(&opened.path);
                    return;
                }
                self.settings.remember_open(&opened.path);
                let tab_id = self.alloc_tab_id();
                self.tabs.push(Tab::new(tab_id, opened));
                self.active = tab_id;
                if self.tabs.len() > 1 {
                    self.pulse_tab_list();
                }
            }
            PdfReply::Tile { doc_id, tile } => {
                let key = tile_key(&tile);
                let frame = self.tile_frame;
                let pixels_per_point = ctx.pixels_per_point();
                let Some(tab) = self.tab_by_doc_id_mut(doc_id) else {
                    return;
                };
                tab.inflight.remove(&key);
                let image = egui::ColorImage::from_rgba_premultiplied(
                    [tile.width as usize, tile.height as usize],
                    &tile.pixels,
                );
                let texture = ctx.load_texture(
                    format!(
                        "tile-{}-{}-{}-{}-{}",
                        doc_id, key.page, key.scale_bits, key.col, key.row
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
                        last_used_frame: frame,
                        texture,
                    },
                );
                trim_process_tiles(self, pixels_per_point);
            }
            PdfReply::TileMiss {
                doc_id,
                page,
                scale,
                col,
                row,
            } => {
                if let Some(tab) = self.tab_by_doc_id_mut(doc_id) {
                    tab.inflight.remove(&TileKey {
                        page,
                        scale_bits: scale.to_bits(),
                        col,
                        row,
                    });
                }
            }
            PdfReply::Thumb { doc_id, thumb } => {
                let Some(tab) = self.tab_by_doc_id_mut(doc_id) else {
                    return;
                };
                tab.thumbs_inflight.remove(&thumb.page);
                if !tab.thumbs_open {
                    return;
                }
                let image = egui::ColorImage::from_rgba_premultiplied(
                    [thumb.width as usize, thumb.height as usize],
                    &thumb.pixels,
                );
                let texture = ctx.load_texture(
                    format!("thumb-{}-{}", doc_id, thumb.page),
                    image,
                    egui::TextureOptions::LINEAR,
                );
                tab.thumbs.insert(thumb.page, CachedThumb { texture });
                tab.touch_thumb(thumb.page);
            }
            PdfReply::ThumbMiss { doc_id, page } => {
                if let Some(tab) = self.tab_by_doc_id_mut(doc_id) {
                    tab.thumbs_inflight.remove(&page);
                }
            }
            PdfReply::Glyphs {
                doc_id,
                page,
                glyphs,
            } => {
                if let Some(tab) = self.tab_by_doc_id_mut(doc_id) {
                    tab.glyphs_waiting.remove(&page);
                    tab.doc.insert_glyphs(page, glyphs);
                    let pinned = pinned_glyph_pages(tab);
                    tab.doc.trim_glyph_cache(&pinned);
                }
                self.try_finish_pending_export(doc_id);
            }
            PdfReply::Search {
                doc_id,
                seq,
                hits,
                done,
            } => {
                if let Some(tab) = self.tab_by_doc_id_mut(doc_id) {
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
            PdfReply::Saved { doc_id, result } => self.on_saved(doc_id, result),
            PdfReply::SavedAs {
                doc_id,
                path,
                result,
            } => self.on_saved_as(doc_id, path, result),
            PdfReply::PageInserted {
                doc_id,
                index,
                pages,
            } => {
                self.on_page_inserted(doc_id, index, pages);
            }
            PdfReply::PageDeleted {
                doc_id,
                index,
                pages,
            } => {
                self.on_page_deleted(doc_id, index, pages);
            }
            PdfReply::Failed { doc_id, message } => {
                if let Some(doc_id) = doc_id {
                    self.opening.remove(&doc_id);
                    if let Some(tab) = self.tab_by_doc_id_mut(doc_id) {
                        tab.inflight.clear();
                        tab.thumbs_inflight.clear();
                        tab.search.pending = false;
                        if let Some(op) = tab.undo.page_op.take() {
                            let revert = match op {
                                PendingPageOp::Delete { revert, .. }
                                | PendingPageOp::Insert { revert, .. } => revert,
                            };
                            if let Some(revert) = revert {
                                tab.undo.revert_failed_page_op(revert);
                            }
                        }
                    }
                }
                self.error = Some(message);
            }
        }
    }

    pub(crate) fn on_page_inserted(
        &mut self,
        doc_id: DocId,
        index: usize,
        pages: Vec<crate::pdf::PageInfo>,
    ) {
        let record_undo = {
            let Some(tab) = self.tab_by_doc_id_mut(doc_id) else {
                return;
            };
            let op = tab.undo.page_op.take();
            match op {
                Some(PendingPageOp::Insert {
                    index: expected,
                    record_undo,
                    ..
                }) if expected == index => record_undo,
                None => true,
                other => {
                    tab.undo.page_op = other;
                    return;
                }
            }
        };
        let Some(tab) = self.tab_by_doc_id_mut(doc_id) else {
            return;
        };
        tab.doc.session.shift_pages_from(index);
        tab.doc.pages = pages;
        tab.doc.tops = DocState::rebuild_tops(&tab.doc.pages);
        tab.doc.tiles.clear();
        tab.inflight.clear();
        tab.thumbs.clear();
        tab.thumb_order.clear();
        tab.thumbs_inflight.clear();
        tab.glyphs_waiting.clear();
        tab.pending_export = None;
        let glyphs = std::mem::take(&mut tab.doc.glyphs);
        let order = std::mem::take(&mut tab.doc.glyph_order);
        tab.doc.glyphs.clear();
        tab.doc.glyph_order.clear();
        for (page, list) in glyphs {
            let page = if page >= index { page + 1 } else { page };
            tab.doc.glyphs.insert(page, list);
        }
        for page in order {
            let page = if page >= index { page + 1 } else { page };
            if tab.doc.glyphs.contains_key(&page) {
                tab.doc.glyph_order.push_back(page);
            }
        }
        for (page, _) in &mut tab.search.hits {
            if *page >= index {
                *page += 1;
            }
        }
        tab.pending_jump = Some((index, Some(0.0)));
        if record_undo {
            tab.undo.record_page_op(UndoEntry::InsertPage { index });
        }
        self.error = None;
    }

    pub(crate) fn on_page_deleted(
        &mut self,
        doc_id: DocId,
        index: usize,
        pages: Vec<crate::pdf::PageInfo>,
    ) {
        let record_undo = {
            let Some(tab) = self.tab_by_doc_id_mut(doc_id) else {
                return;
            };
            match tab.undo.page_op.take() {
                Some(PendingPageOp::Delete {
                    index: expected,
                    record_undo,
                    ..
                }) if expected == index => record_undo,
                other => {
                    tab.undo.page_op = other;
                    return;
                }
            }
        };
        let Some(tab) = self.tab_by_doc_id_mut(doc_id) else {
            return;
        };
        tab.doc.session.unshift_pages_from(index);
        tab.doc.pages = pages;
        tab.doc.tops = DocState::rebuild_tops(&tab.doc.pages);
        tab.doc.tiles.clear();
        tab.inflight.clear();
        tab.thumbs.clear();
        tab.thumb_order.clear();
        tab.thumbs_inflight.clear();
        tab.glyphs_waiting.clear();
        tab.pending_export = None;
        let glyphs = std::mem::take(&mut tab.doc.glyphs);
        let order = std::mem::take(&mut tab.doc.glyph_order);
        tab.doc.glyphs.clear();
        tab.doc.glyph_order.clear();
        for (page, list) in glyphs {
            if page == index {
                continue;
            }
            let page = if page > index { page - 1 } else { page };
            tab.doc.glyphs.insert(page, list);
        }
        for page in order {
            if page == index {
                continue;
            }
            let page = if page > index { page - 1 } else { page };
            if tab.doc.glyphs.contains_key(&page) {
                tab.doc.glyph_order.push_back(page);
            }
        }
        tab.search.hits.retain(|(page, _)| *page != index);
        for (page, _) in &mut tab.search.hits {
            if *page > index {
                *page -= 1;
            }
        }
        tab.selected.retain(|id| tab.doc.session.get(*id).is_some());
        if tab.selected.is_empty() {
            tab.editing = None;
        }
        let jump = index.min(tab.doc.pages.len().saturating_sub(1));
        tab.pending_jump = Some((jump, Some(0.0)));
        if record_undo {
            tab.undo.record_page_op(UndoEntry::DeletePage { index });
        }
        self.error = None;
    }

    pub(crate) fn insert_page_after_current(&mut self) {
        let Some(tab) = self.tab() else {
            return;
        };
        if matches!(tab.save, SaveState::Saving) || tab.undo.page_op.is_some() {
            return;
        }
        let doc_id = tab.doc.doc_id;
        let after = tab.doc.current_page(self.view_rect.height().max(1.0));
        let at = if tab.doc.pages.is_empty() {
            0
        } else {
            (after + 1).min(tab.doc.pages.len())
        };
        self.seal_undo();
        if let Some(tab) = self.tab_mut() {
            tab.undo.page_op = Some(PendingPageOp::Insert {
                index: at,
                record_undo: true,
                revert: None,
            });
        }
        self.worker.insert_page(doc_id, after);
    }

    /// Delete the page under the viewport focus.
    pub(crate) fn delete_current_page(&mut self) {
        let Some(tab) = self.tab() else {
            return;
        };
        let index = tab.doc.current_page(self.view_rect.height().max(1.0));
        self.delete_page_at(index);
    }

    /// Delete page `index`. Refuses the only page and while a text edit is open.
    pub(crate) fn delete_page_at(&mut self, index: usize) {
        let Some(tab) = self.tab() else {
            return;
        };
        if tab.editing.is_some() {
            self.error = Some("Finish or cancel the text edit before deleting a page.".into());
            return;
        }
        if matches!(tab.save, SaveState::Saving) || tab.undo.page_op.is_some() {
            return;
        }
        if tab.doc.pages.len() <= 1 {
            self.error = Some("Cannot delete the only page.".into());
            return;
        }
        if index >= tab.doc.pages.len() {
            self.error = Some("Page index out of range.".into());
            return;
        }
        let doc_id = tab.doc.doc_id;
        self.seal_undo();
        if let Some(tab) = self.tab_mut() {
            tab.menu = None;
            tab.undo.page_op = Some(PendingPageOp::Delete {
                index,
                record_undo: true,
                revert: None,
            });
        }
        self.worker.delete_page(doc_id, index);
    }

    pub(crate) fn on_saved(
        &mut self,
        doc_id: DocId,
        result: Result<Vec<crate::pdf::SavedXref>, String>,
    ) {
        let Some(index) = self.tabs.iter().position(|tab| tab.doc.doc_id == doc_id) else {
            return;
        };
        match result {
            Ok(saved) => {
                let epoch = self.tabs[index].save_epoch;
                let sent = std::mem::take(&mut self.tabs[index].save_deletes);
                let (close, detach) = {
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
                        tab.save.redirty_soon();
                    } else {
                        tab.save = SaveState::Clean;
                    }
                    let clean = !tab.doc.session.is_dirty();
                    (
                        tab.close_after_save && clean,
                        tab.detach_after_save && clean,
                    )
                };
                let tab_id = self.tabs[index].id;
                if detach {
                    self.finish_detach_tab(tab_id);
                } else if close {
                    self.drop_tab(tab_id);
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

    pub(crate) fn on_saved_as(
        &mut self,
        doc_id: DocId,
        path: PathBuf,
        result: Result<Vec<crate::pdf::SavedXref>, String>,
    ) {
        let Some(index) = self.tabs.iter().position(|tab| tab.doc.doc_id == doc_id) else {
            return;
        };
        match result {
            Ok(saved) => {
                let epoch = self.tabs[index].save_epoch;
                let sent = std::mem::take(&mut self.tabs[index].save_deletes);
                {
                    let tab = &mut self.tabs[index];
                    tab.doc.path = path.clone();
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
                        tab.save.redirty_soon();
                    } else {
                        tab.save = SaveState::Clean;
                    }
                }
                self.settings.remember_open(&path);
            }
            Err(message) => {
                self.tabs[index].save = SaveState::Failed {
                    message,
                    at: Instant::now(),
                };
            }
        }
    }
}

pub(crate) fn tile_key(tile: &crate::pdf::TileImage) -> TileKey {
    TileKey {
        page: tile.page,
        scale_bits: tile.scale.to_bits(),
        col: tile.col,
        row: tile.row,
    }
}

pub(crate) fn tile_byte_size(tile: &CachedTile) -> usize {
    tile.w as usize * tile.h as usize * 4
}

pub(crate) fn tab_tile_bytes(tab: &Tab) -> usize {
    tab.doc.tiles.values().map(tile_byte_size).sum()
}

pub(crate) fn process_tile_bytes(app: &MarkerApp) -> usize {
    app.tabs.iter().map(tab_tile_bytes).sum()
}

/// Lower tier and older `last_used_frame` are evicted first.
pub(crate) fn tile_eviction_tier(
    app: &MarkerApp,
    tab_idx: usize,
    key: &TileKey,
    pixels_per_point: f32,
) -> u8 {
    let tab = &app.tabs[tab_idx];
    let tab_id = tab.id;
    let on_screen = tab_id == app.active
        || app
            .split
            .is_some_and(|split| split.other(app.active) == tab_id);
    if !on_screen {
        return 0;
    }
    let view = if tab_id == app.active {
        app.view_rect
    } else {
        app.split_view_rect
    };
    let (first, last) = view::visible_pages(&tab.doc, view);
    let current_bits = tab.doc.render_scale(pixels_per_point).to_bits();
    if key.page >= first && key.page <= last && key.scale_bits == current_bits {
        return 3;
    }
    if key.page >= first && key.page <= last {
        return 2;
    }
    1
}

pub(crate) fn tile_evict_rank(
    tier: u8,
    last_used: u64,
    tab_idx: usize,
    key: &TileKey,
) -> (u8, u64, usize, usize, u32, i32, i32) {
    (
        tier,
        last_used,
        tab_idx,
        key.page,
        key.scale_bits,
        key.col,
        key.row,
    )
}

pub(crate) fn pick_tile_to_evict(
    app: &MarkerApp,
    pixels_per_point: f32,
) -> Option<(usize, TileKey)> {
    let mut best: Option<(u8, u64, usize, TileKey)> = None;
    for (tab_idx, tab) in app.tabs.iter().enumerate() {
        for (key, tile) in &tab.doc.tiles {
            let tier = tile_eviction_tier(app, tab_idx, key, pixels_per_point);
            let candidate = (tier, tile.last_used_frame, tab_idx, *key);
            let better = match best {
                None => true,
                Some(prev) => {
                    tile_evict_rank(candidate.0, candidate.1, candidate.2, &candidate.3)
                        < tile_evict_rank(prev.0, prev.1, prev.2, &prev.3)
                }
            };
            if better {
                best = Some(candidate);
            }
        }
    }
    best.map(|(_, _, tab_idx, key)| (tab_idx, key))
}

pub(crate) fn trim_process_tiles(app: &mut MarkerApp, pixels_per_point: f32) {
    while process_tile_bytes(app) > PROCESS_TILE_BUDGET_BYTES {
        let Some((tab_idx, key)) = pick_tile_to_evict(app, pixels_per_point) else {
            break;
        };
        app.tabs[tab_idx].doc.tiles.remove(&key);
    }
}

pub(crate) fn trim_tab_tiles(
    app: &mut MarkerApp,
    tab_idx: usize,
    max_bytes: usize,
    pixels_per_point: f32,
) {
    while tab_idx < app.tabs.len() && tab_tile_bytes(&app.tabs[tab_idx]) > max_bytes {
        let mut best: Option<(u8, u64, TileKey)> = None;
        let tab = &app.tabs[tab_idx];
        for (key, tile) in &tab.doc.tiles {
            let tier = tile_eviction_tier(app, tab_idx, key, pixels_per_point);
            let candidate = (tier, tile.last_used_frame, *key);
            let better = match best {
                None => true,
                Some(prev) => {
                    tile_evict_rank(candidate.0, candidate.1, tab_idx, &candidate.2)
                        < tile_evict_rank(prev.0, prev.1, tab_idx, &prev.2)
                }
            };
            if better {
                best = Some(candidate);
            }
        }
        let Some((_, _, key)) = best else {
            break;
        };
        app.tabs[tab_idx].doc.tiles.remove(&key);
    }
}
