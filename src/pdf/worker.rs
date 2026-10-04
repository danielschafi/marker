use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::app::TileKey;

use crate::annot::{Annotation, Glyph};
use crate::geom::PdfRect;

use super::engine::{
    CropImage, DocumentEngine, OutlineNode, PageInfo, SaveSnapshot, SavedXref, TileImage,
};

pub enum PdfJob {
    Open {
        gen: u64,
        path: PathBuf,
    },
    Close {
        gen: u64,
    },
    Tile {
        gen: u64,
        page: usize,
        scale: f32,
        col: i32,
        row: i32,
        prefetch: bool,
        distance: u32,
    },
    Glyphs {
        gen: u64,
        page: usize,
    },
    Crop {
        gen: u64,
        seq: u64,
        page: usize,
        rect: PdfRect,
        dpi: f32,
    },
    Search {
        gen: u64,
        seq: u64,
        needle: String,
        from: usize,
    },
    Save {
        gen: u64,
        snapshot: SaveSnapshot,
    },
    InsertPage {
        gen: u64,
        after: usize,
    },
    InsertPageAt {
        gen: u64,
        at: usize,
    },
    DeletePage {
        gen: u64,
        index: usize,
    },
    Shutdown,
}

pub enum PdfReply {
    Opened(Result<OpenedDoc, String>),
    /// `tile.pixels` are premultiplied RGBA8 (see [`TileImage::pixels`]).
    Tile {
        gen: u64,
        tile: TileImage,
    },
    TileMiss {
        gen: u64,
        page: usize,
        scale: f32,
        col: i32,
        row: i32,
    },
    Glyphs {
        gen: u64,
        page: usize,
        glyphs: Vec<Glyph>,
    },
    Crop {
        gen: u64,
        seq: u64,
        result: Result<CropImage, String>,
    },
    Search {
        gen: u64,
        seq: u64,
        hits: Vec<(usize, Vec<PdfRect>)>,
        done: bool,
    },
    Saved {
        gen: u64,
        result: Result<Vec<SavedXref>, String>,
    },
    PageInserted {
        gen: u64,
        index: usize,
        pages: Vec<PageInfo>,
    },
    PageDeleted {
        gen: u64,
        index: usize,
        pages: Vec<PageInfo>,
    },
    Failed {
        gen: Option<u64>,
        message: String,
    },
}

pub struct OpenedDoc {
    pub gen: u64,
    pub path: PathBuf,
    pub pages: Vec<PageInfo>,
    pub outline: Vec<OutlineNode>,
    pub annotations: Vec<Annotation>,
}

pub struct PdfWorker {
    jobs: Sender<PdfJob>,
    replies: Receiver<PdfReply>,
}

impl PdfWorker {
    pub fn spawn(
        ctx: egui::Context,
        tile_wanted: Arc<Mutex<HashMap<u64, HashSet<TileKey>>>>,
    ) -> Self {
        let (job_tx, job_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let loop_tx = job_tx.clone();
        thread::Builder::new()
            .name("marker-pdf".into())
            .spawn(move || worker_loop(ctx, job_rx, loop_tx, reply_tx, tile_wanted))
            .expect("pdf thread");
        Self {
            jobs: job_tx,
            replies: reply_rx,
        }
    }

    pub fn open(&self, gen: u64, path: PathBuf) {
        let _ = self.jobs.send(PdfJob::Open { gen, path });
    }

    pub fn close(&self, gen: u64) {
        let _ = self.jobs.send(PdfJob::Close { gen });
    }

    pub fn tile(
        &self,
        gen: u64,
        page: usize,
        scale: f32,
        col: i32,
        row: i32,
        prefetch: bool,
        distance: u32,
    ) {
        let _ = self.jobs.send(PdfJob::Tile {
            gen,
            page,
            scale,
            col,
            row,
            prefetch,
            distance,
        });
    }

    pub fn glyphs(&self, gen: u64, page: usize) {
        let _ = self.jobs.send(PdfJob::Glyphs { gen, page });
    }

    pub fn crop(&self, gen: u64, seq: u64, page: usize, rect: PdfRect, dpi: f32) {
        let _ = self.jobs.send(PdfJob::Crop {
            gen,
            seq,
            page,
            rect,
            dpi,
        });
    }

    pub fn search(&self, gen: u64, seq: u64, needle: String) {
        let _ = self.jobs.send(PdfJob::Search {
            gen,
            seq,
            needle,
            from: 0,
        });
    }

    pub fn save(&self, gen: u64, snapshot: SaveSnapshot) {
        let _ = self.jobs.send(PdfJob::Save { gen, snapshot });
    }

    pub fn insert_page(&self, gen: u64, after: usize) {
        let _ = self.jobs.send(PdfJob::InsertPage { gen, after });
    }

    pub fn insert_page_at(&self, gen: u64, at: usize) {
        let _ = self.jobs.send(PdfJob::InsertPageAt { gen, at });
    }

    pub fn delete_page(&self, gen: u64, index: usize) {
        let _ = self.jobs.send(PdfJob::DeletePage { gen, index });
    }

    pub fn poll(&self) -> Vec<PdfReply> {
        let mut replies = Vec::new();
        while let Ok(reply) = self.replies.try_recv() {
            replies.push(reply);
        }
        replies
    }
}

impl Drop for PdfWorker {
    fn drop(&mut self) {
        let _ = self.jobs.send(PdfJob::Shutdown);
    }
}

fn job_sort_key(job: &PdfJob) -> (u8, u32) {
    match job {
        PdfJob::Shutdown
        | PdfJob::Open { .. }
        | PdfJob::Close { .. }
        | PdfJob::Save { .. }
        | PdfJob::InsertPage { .. }
        | PdfJob::InsertPageAt { .. }
        | PdfJob::DeletePage { .. } => (0, 0),
        PdfJob::Tile {
            prefetch,
            distance,
            ..
        } => {
            if *prefetch {
                (2, *distance)
            } else {
                (1, *distance)
            }
        }
        PdfJob::Crop { .. } | PdfJob::Search { .. } => (3, 0),
        PdfJob::Glyphs { .. } => (4, 0),
    }
}

fn reply(ctx: &egui::Context, replies: &Sender<PdfReply>, msg: PdfReply) {
    let _ = replies.send(msg);
    ctx.request_repaint();
}

impl PdfReply {
    fn failed(gen: u64, message: impl Into<String>) -> Self {
        Self::Failed {
            gen: Some(gen),
            message: message.into(),
        }
    }
}

fn engine_mut(
    engines: &mut HashMap<u64, DocumentEngine>,
    gen: u64,
) -> Result<&mut DocumentEngine, PdfReply> {
    engines
        .get_mut(&gen)
        .ok_or_else(|| PdfReply::failed(gen, "No document is open."))
}

struct WorkerCtx<'a> {
    ctx: &'a egui::Context,
    replies: &'a Sender<PdfReply>,
    jobs_tx: &'a Sender<PdfJob>,
    engines: &'a mut HashMap<u64, DocumentEngine>,
    latest_search: &'a mut HashMap<u64, u64>,
}

impl WorkerCtx<'_> {
    fn send(&self, msg: PdfReply) {
        reply(self.ctx, self.replies, msg);
    }
}

fn handle_open(ctx: &mut WorkerCtx<'_>, gen: u64, path: PathBuf) {
    match DocumentEngine::open(&path) {
        Ok(loaded) => {
            let opened = OpenedDoc {
                gen,
                path,
                pages: loaded.engine.pages().to_vec(),
                outline: loaded.engine.outline().to_vec(),
                annotations: loaded.annotations,
            };
            ctx.engines.insert(gen, loaded.engine);
            ctx.send(PdfReply::Opened(Ok(opened)));
        }
        Err(err) => ctx.send(PdfReply::failed(gen, err)),
    }
}

fn handle_tile(
    ctx: &mut WorkerCtx<'_>,
    gen: u64,
    page: usize,
    scale: f32,
    col: i32,
    row: i32,
    tile_wanted: &Arc<Mutex<HashMap<u64, HashSet<TileKey>>>>,
) {
    let key = TileKey {
        page,
        scale_bits: scale.to_bits(),
        col,
        row,
    };
    let still_wanted = tile_wanted
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&gen)
        .is_some_and(|set| set.contains(&key));
    if !still_wanted {
        ctx.send(PdfReply::TileMiss {
            gen,
            page,
            scale,
            col,
            row,
        });
        return;
    }
    let Some(engine) = ctx.engines.get_mut(&gen) else {
        return;
    };
    match engine.render_tile(page, scale, col, row) {
        Ok(Some(tile)) => ctx.send(PdfReply::Tile { gen, tile }),
        Ok(None) => {
            ctx.send(PdfReply::TileMiss {
                gen,
                page,
                scale,
                col,
                row,
            });
        }
        Err(err) => ctx.send(PdfReply::failed(gen, err)),
    }
}

fn handle_glyphs(ctx: &mut WorkerCtx<'_>, gen: u64, page: usize) {
    let Some(engine) = ctx.engines.get(&gen) else {
        return;
    };
    match engine.glyphs(page) {
        Ok(glyphs) => ctx.send(PdfReply::Glyphs { gen, page, glyphs }),
        Err(err) => ctx.send(PdfReply::failed(gen, err)),
    }
}

fn handle_crop(
    ctx: &mut WorkerCtx<'_>,
    gen: u64,
    seq: u64,
    page: usize,
    rect: PdfRect,
    dpi: f32,
) {
    let result = match engine_mut(ctx.engines, gen) {
        Ok(engine) => engine.render_crop_png(page, rect, dpi),
        Err(_) => Err("No document is open.".into()),
    };
    ctx.send(PdfReply::Crop { gen, seq, result });
}

fn handle_search(
    ctx: &mut WorkerCtx<'_>,
    gen: u64,
    seq: u64,
    needle: String,
    from: usize,
) {
    if from == 0 {
        ctx.latest_search.insert(gen, seq);
    }
    if ctx.latest_search.get(&gen) != Some(&seq) {
        return;
    }
    let Some(engine) = ctx.engines.get(&gen) else {
        return;
    };
    let page_count = engine.pages().len();
    let end = (from + 2).min(page_count);
    let mut hits = Vec::new();
    let mut failed = None;
    for page in from..end {
        match engine.search_page(page, &needle) {
            Ok(quads) if !quads.is_empty() => hits.push((page, quads)),
            Ok(_) => {}
            Err(err) => {
                failed = Some(err);
                break;
            }
        }
    }
    if let Some(message) = failed {
        ctx.send(PdfReply::failed(gen, message));
        return;
    }
    let done = end >= page_count;
    ctx.send(PdfReply::Search {
        gen,
        seq,
        hits,
        done,
    });
    if !done {
        let _ = ctx.jobs_tx.send(PdfJob::Search {
            gen,
            seq,
            needle,
            from: end,
        });
    }
}

fn handle_save(ctx: &mut WorkerCtx<'_>, gen: u64, snapshot: SaveSnapshot) {
    let reply = match engine_mut(ctx.engines, gen) {
        Ok(engine) => PdfReply::Saved {
            gen,
            result: engine.save(&snapshot),
        },
        Err(_) => PdfReply::Saved {
            gen,
            result: Err("No document is open.".into()),
        },
    };
    ctx.send(reply);
}

fn handle_insert_page(ctx: &mut WorkerCtx<'_>, gen: u64, after: usize) {
    let reply = match engine_mut(ctx.engines, gen) {
        Ok(engine) => match engine.insert_blank_page(after) {
            Ok((index, pages)) => PdfReply::PageInserted { gen, index, pages },
            Err(message) => PdfReply::failed(gen, message),
        },
        Err(reply) => reply,
    };
    ctx.send(reply);
}

fn handle_insert_page_at(ctx: &mut WorkerCtx<'_>, gen: u64, at: usize) {
    let reply = match engine_mut(ctx.engines, gen) {
        Ok(engine) => match engine.insert_blank_page_at(at) {
            Ok((index, pages)) => PdfReply::PageInserted { gen, index, pages },
            Err(message) => PdfReply::failed(gen, message),
        },
        Err(reply) => reply,
    };
    ctx.send(reply);
}

fn handle_delete_page(ctx: &mut WorkerCtx<'_>, gen: u64, index: usize) {
    let reply = match engine_mut(ctx.engines, gen) {
        Ok(engine) => match engine.delete_page_at(index) {
            Ok(pages) => PdfReply::PageDeleted { gen, index, pages },
            Err(message) => PdfReply::failed(gen, message),
        },
        Err(reply) => reply,
    };
    ctx.send(reply);
}

fn handle_job(
    ctx: &mut WorkerCtx<'_>,
    job: PdfJob,
    tile_wanted: &Arc<Mutex<HashMap<u64, HashSet<TileKey>>>>,
) -> bool {
    match job {
        PdfJob::Shutdown => true,
        PdfJob::Open { gen, path } => {
            handle_open(ctx, gen, path);
            false
        }
        PdfJob::Close { gen } => {
            ctx.engines.remove(&gen);
            false
        }
        PdfJob::Tile {
            gen,
            page,
            scale,
            col,
            row,
            ..
        } => {
            handle_tile(ctx, gen, page, scale, col, row, tile_wanted);
            false
        }
        PdfJob::Glyphs { gen, page } => {
            handle_glyphs(ctx, gen, page);
            false
        }
        PdfJob::Crop {
            gen,
            seq,
            page,
            rect,
            dpi,
        } => {
            handle_crop(ctx, gen, seq, page, rect, dpi);
            false
        }
        PdfJob::Search {
            gen,
            seq,
            needle,
            from,
        } => {
            handle_search(ctx, gen, seq, needle, from);
            false
        }
        PdfJob::Save { gen, snapshot } => {
            handle_save(ctx, gen, snapshot);
            false
        }
        PdfJob::InsertPage { gen, after } => {
            handle_insert_page(ctx, gen, after);
            false
        }
        PdfJob::InsertPageAt { gen, at } => {
            handle_insert_page_at(ctx, gen, at);
            false
        }
        PdfJob::DeletePage { gen, index } => {
            handle_delete_page(ctx, gen, index);
            false
        }
    }
}

fn dequeue_job(jobs: &Receiver<PdfJob>, queued: &mut VecDeque<PdfJob>) -> Option<PdfJob> {
    if queued.is_empty() {
        match jobs.recv() {
            Ok(job) => queued.push_back(job),
            Err(_) => return None,
        }
    }
    while let Ok(job) = jobs.try_recv() {
        queued.push_back(job);
    }
    let idx = queued
        .iter()
        .enumerate()
        .min_by_key(|(_, job)| job_sort_key(job))
        .map(|(index, _)| index)
        .unwrap_or(0);
    queued.remove(idx)
}

fn worker_loop(
    ctx: egui::Context,
    jobs: Receiver<PdfJob>,
    jobs_tx: Sender<PdfJob>,
    replies: Sender<PdfReply>,
    tile_wanted: Arc<Mutex<HashMap<u64, HashSet<TileKey>>>>,
) {
    let mut engines: HashMap<u64, DocumentEngine> = HashMap::new();
    let mut latest_search: HashMap<u64, u64> = HashMap::new();
    let mut queued = VecDeque::new();
    while let Some(job) = dequeue_job(&jobs, &mut queued) {
        let mut worker = WorkerCtx {
            ctx: &ctx,
            replies: &replies,
            jobs_tx: &jobs_tx,
            engines: &mut engines,
            latest_search: &mut latest_search,
        };
        if handle_job(&mut worker, job, &tile_wanted) {
            break;
        }
    }
}
