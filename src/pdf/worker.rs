use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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
    Crop {
        gen: u64,
        seq: u64,
        page: usize,
        rect: PdfRect,
        dpi: f32,
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

/// Search and glyph jobs for `marker-pdf-read`. Kept off `PdfJob` so the tile
/// thread never ranks them against raster work.
enum ReadJob {
    Open {
        gen: u64,
        path: PathBuf,
    },
    Close {
        gen: u64,
    },
    /// Drop the reader's document and ack so the tile thread can rewrite the file.
    Release {
        gen: u64,
        ack: Sender<()>,
    },
    Reopen {
        gen: u64,
    },
    Search {
        gen: u64,
        seq: u64,
        needle: String,
        from: usize,
    },
    Glyphs {
        gen: u64,
        page: usize,
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
    reads: Sender<ReadJob>,
    replies: Receiver<PdfReply>,
}

impl PdfWorker {
    pub fn spawn(
        ctx: egui::Context,
        tile_wanted: Arc<Mutex<HashMap<u64, HashSet<TileKey>>>>,
    ) -> Self {
        let (job_tx, job_rx) = mpsc::channel();
        let (read_tx, read_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let reads_for_tiles = read_tx.clone();
        let reads_for_reader = read_tx.clone();
        let replies_for_reader = reply_tx.clone();
        let ctx_for_reader = ctx.clone();
        thread::Builder::new()
            .name("marker-pdf".into())
            .spawn(move || worker_loop(ctx, job_rx, reply_tx, reads_for_tiles, tile_wanted))
            .expect("pdf thread");
        thread::Builder::new()
            .name("marker-pdf-read".into())
            .spawn(move || {
                reader_loop(
                    ctx_for_reader,
                    read_rx,
                    reads_for_reader,
                    replies_for_reader,
                )
            })
            .expect("pdf read thread");
        Self {
            jobs: job_tx,
            reads: read_tx,
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
        let _ = self.reads.send(ReadJob::Glyphs { gen, page });
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
        let _ = self.reads.send(ReadJob::Search {
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
        let _ = self.reads.send(ReadJob::Shutdown);
    }
}

/// Tile-thread order. Search and glyphs are not ranked here; they run on
/// `marker-pdf-read`.
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
        PdfJob::Crop { .. } => (3, 0),
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
    reads: &'a Sender<ReadJob>,
    engines: &'a mut HashMap<u64, DocumentEngine>,
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
                path: path.clone(),
                pages: loaded.engine.pages().to_vec(),
                outline: loaded.engine.outline().to_vec(),
                annotations: loaded.annotations,
            };
            ctx.engines.insert(gen, loaded.engine);
            let _ = ctx.reads.send(ReadJob::Open { gen, path });
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

fn handle_save(ctx: &mut WorkerCtx<'_>, gen: u64, snapshot: SaveSnapshot) {
    release_reader(ctx.reads, gen);
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
    reopen_reader(ctx.reads, gen);
    ctx.send(reply);
}

fn handle_insert_page(ctx: &mut WorkerCtx<'_>, gen: u64, after: usize) {
    release_reader(ctx.reads, gen);
    let reply = match engine_mut(ctx.engines, gen) {
        Ok(engine) => match engine.insert_blank_page(after) {
            Ok((index, pages)) => PdfReply::PageInserted { gen, index, pages },
            Err(message) => PdfReply::failed(gen, message),
        },
        Err(reply) => reply,
    };
    reopen_reader(ctx.reads, gen);
    ctx.send(reply);
}

fn handle_insert_page_at(ctx: &mut WorkerCtx<'_>, gen: u64, at: usize) {
    release_reader(ctx.reads, gen);
    let reply = match engine_mut(ctx.engines, gen) {
        Ok(engine) => match engine.insert_blank_page_at(at) {
            Ok((index, pages)) => PdfReply::PageInserted { gen, index, pages },
            Err(message) => PdfReply::failed(gen, message),
        },
        Err(reply) => reply,
    };
    reopen_reader(ctx.reads, gen);
    ctx.send(reply);
}

fn handle_delete_page(ctx: &mut WorkerCtx<'_>, gen: u64, index: usize) {
    release_reader(ctx.reads, gen);
    let reply = match engine_mut(ctx.engines, gen) {
        Ok(engine) => match engine.delete_page_at(index) {
            Ok(pages) => PdfReply::PageDeleted { gen, index, pages },
            Err(message) => PdfReply::failed(gen, message),
        },
        Err(reply) => reply,
    };
    reopen_reader(ctx.reads, gen);
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
            let _ = ctx.reads.send(ReadJob::Close { gen });
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
    replies: Sender<PdfReply>,
    reads: Sender<ReadJob>,
    tile_wanted: Arc<Mutex<HashMap<u64, HashSet<TileKey>>>>,
) {
    let mut engines: HashMap<u64, DocumentEngine> = HashMap::new();
    let mut queued = VecDeque::new();
    while let Some(job) = dequeue_job(&jobs, &mut queued) {
        let mut worker = WorkerCtx {
            ctx: &ctx,
            replies: &replies,
            reads: &reads,
            engines: &mut engines,
        };
        if handle_job(&mut worker, job, &tile_wanted) {
            break;
        }
    }
}

const READER_RELEASE_WAIT: Duration = Duration::from_secs(30);
const READER_OPEN_RETRY: Duration = Duration::from_millis(10);

/// Unmap the reader's document before the tile thread writes the file.
///
/// A second `PdfDocument` on the same file can make `rewrite_all` fail. The
/// reader acks only after that document is dropped. If the ack never comes,
/// the write proceeds anyway rather than wedging the tile thread.
fn release_reader(reads: &Sender<ReadJob>, gen: u64) {
    let (ack_tx, ack_rx) = mpsc::channel();
    if reads.send(ReadJob::Release { gen, ack: ack_tx }).is_err() {
        return;
    }
    let _ = ack_rx.recv_timeout(READER_RELEASE_WAIT);
}

fn reopen_reader(reads: &Sender<ReadJob>, gen: u64) {
    let _ = reads.send(ReadJob::Reopen { gen });
}

struct ReaderState {
    engines: HashMap<u64, DocumentEngine>,
    paths: HashMap<u64, PathBuf>,
    latest_search: HashMap<u64, u64>,
    /// Documents dropped so the tile thread can write the file.
    suspended: HashSet<u64>,
    /// Search and glyph jobs waiting for a reopen.
    deferred: VecDeque<ReadJob>,
}

fn reader_loop(
    ctx: egui::Context,
    jobs: Receiver<ReadJob>,
    jobs_tx: Sender<ReadJob>,
    replies: Sender<PdfReply>,
) {
    let mut state = ReaderState {
        engines: HashMap::new(),
        paths: HashMap::new(),
        latest_search: HashMap::new(),
        suspended: HashSet::new(),
        deferred: VecDeque::new(),
    };
    let mut queued = VecDeque::new();
    let io = ReadIo {
        ctx: &ctx,
        replies: &replies,
        jobs_tx: &jobs_tx,
    };
    while let Some(job) = dequeue_read(&jobs, &mut queued, &mut state) {
        if handle_read(&io, &mut state, &mut queued, job) {
            break;
        }
    }
}

fn read_sort_key(job: &ReadJob) -> u8 {
    match job {
        ReadJob::Shutdown
        | ReadJob::Open { .. }
        | ReadJob::Close { .. }
        | ReadJob::Release { .. }
        | ReadJob::Reopen { .. } => 0,
        ReadJob::Search { .. } => 1,
        ReadJob::Glyphs { .. } => 2,
    }
}

fn is_suspended_work(job: &ReadJob, suspended: &HashSet<u64>) -> bool {
    match job {
        ReadJob::Search { gen, .. } | ReadJob::Glyphs { gen, .. } => suspended.contains(gen),
        ReadJob::Open { .. }
        | ReadJob::Close { .. }
        | ReadJob::Release { .. }
        | ReadJob::Reopen { .. }
        | ReadJob::Shutdown => false,
    }
}

fn dequeue_read(
    jobs: &Receiver<ReadJob>,
    queued: &mut VecDeque<ReadJob>,
    state: &mut ReaderState,
) -> Option<ReadJob> {
    loop {
        if queued.is_empty() {
            match jobs.recv() {
                Ok(job) => queued.push_back(job),
                Err(_) => return None,
            }
        }
        while let Ok(job) = jobs.try_recv() {
            queued.push_back(job);
        }
        let ready = queued
            .iter()
            .enumerate()
            .filter(|(_, job)| !is_suspended_work(job, &state.suspended))
            .min_by_key(|(_, job)| read_sort_key(job))
            .map(|(index, _)| index);
        if let Some(index) = ready {
            return queued.remove(index);
        }
        state.deferred.append(queued);
        match jobs.recv() {
            Ok(job) => queued.push_back(job),
            Err(_) => return None,
        }
    }
}

fn discard_work(queue: &mut VecDeque<ReadJob>, gen: u64) {
    queue.retain(|job| !is_gen_work(job, gen));
}

fn is_gen_work(job: &ReadJob, gen: u64) -> bool {
    match job {
        ReadJob::Search { gen: job_gen, .. } | ReadJob::Glyphs { gen: job_gen, .. } => {
            *job_gen == gen
        }
        ReadJob::Open { .. }
        | ReadJob::Close { .. }
        | ReadJob::Release { .. }
        | ReadJob::Reopen { .. }
        | ReadJob::Shutdown => false,
    }
}

fn flush_deferred(state: &mut ReaderState, queued: &mut VecDeque<ReadJob>, gen: u64) {
    let pending = std::mem::take(&mut state.deferred);
    for job in pending {
        if is_gen_work(&job, gen) {
            queued.push_back(job);
        } else {
            state.deferred.push_back(job);
        }
    }
}

fn open_engine(path: &Path) -> Result<DocumentEngine, String> {
    match DocumentEngine::open(path) {
        Ok(loaded) => Ok(loaded.engine),
        Err(_) => {
            thread::sleep(READER_OPEN_RETRY);
            DocumentEngine::open(path).map(|loaded| loaded.engine)
        }
    }
}

fn install_engine(
    state: &mut ReaderState,
    queued: &mut VecDeque<ReadJob>,
    gen: u64,
    path: &Path,
) -> bool {
    match open_engine(path) {
        Ok(engine) => {
            state.engines.insert(gen, engine);
            state.suspended.remove(&gen);
            flush_deferred(state, queued, gen);
            true
        }
        Err(_) => {
            state.engines.remove(&gen);
            false
        }
    }
}

struct ReadIo<'a> {
    ctx: &'a egui::Context,
    replies: &'a Sender<PdfReply>,
    jobs_tx: &'a Sender<ReadJob>,
}

fn handle_read(
    io: &ReadIo<'_>,
    state: &mut ReaderState,
    queued: &mut VecDeque<ReadJob>,
    job: ReadJob,
) -> bool {
    match job {
        ReadJob::Shutdown => true,
        ReadJob::Open { gen, path } => {
            state.paths.insert(gen, path.clone());
            if !state.suspended.contains(&gen) {
                let _ = install_engine(state, queued, gen, &path);
            }
            false
        }
        ReadJob::Close { gen } => {
            state.engines.remove(&gen);
            state.paths.remove(&gen);
            state.latest_search.remove(&gen);
            state.suspended.remove(&gen);
            discard_work(&mut state.deferred, gen);
            discard_work(queued, gen);
            false
        }
        ReadJob::Release { gen, ack } => {
            state.engines.remove(&gen);
            state.suspended.insert(gen);
            let _ = ack.send(());
            false
        }
        ReadJob::Reopen { gen } => {
            let Some(path) = state.paths.get(&gen).cloned() else {
                state.suspended.remove(&gen);
                flush_deferred(state, queued, gen);
                return false;
            };
            if !install_engine(state, queued, gen, &path) {
                state.suspended.insert(gen);
            }
            false
        }
        ReadJob::Glyphs { gen, page } => {
            handle_glyphs(io, state, gen, page);
            false
        }
        ReadJob::Search {
            gen,
            seq,
            needle,
            from,
        } => {
            handle_search(io, state, gen, seq, needle, from);
            false
        }
    }
}

fn handle_glyphs(io: &ReadIo<'_>, state: &ReaderState, gen: u64, page: usize) {
    let Some(engine) = state.engines.get(&gen) else {
        return;
    };
    match engine.glyphs(page) {
        Ok(glyphs) => reply(io.ctx, io.replies, PdfReply::Glyphs { gen, page, glyphs }),
        Err(err) => reply(io.ctx, io.replies, PdfReply::failed(gen, err)),
    }
}

fn handle_search(
    io: &ReadIo<'_>,
    state: &mut ReaderState,
    gen: u64,
    seq: u64,
    needle: String,
    from: usize,
) {
    if from == 0 {
        state.latest_search.insert(gen, seq);
    }
    if state.latest_search.get(&gen) != Some(&seq) {
        return;
    }
    let Some(engine) = state.engines.get(&gen) else {
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
        reply(io.ctx, io.replies, PdfReply::failed(gen, message));
        return;
    }
    let done = end >= page_count;
    reply(
        io.ctx,
        io.replies,
        PdfReply::Search {
            gen,
            seq,
            hits,
            done,
        },
    );
    if !done {
        let _ = io.jobs_tx.send(ReadJob::Search {
            gen,
            seq,
            needle,
            from: end,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use crate::app::TileKey;

    use super::{PdfReply, PdfWorker};
    use crate::pdf::engine::SaveSnapshot;

    fn wanted_for(page: usize, scale: f32, col: i32, row: i32) -> TileKey {
        TileKey {
            page,
            scale_bits: scale.to_bits(),
            col,
            row,
        }
    }

    fn write_sample(path: &std::path::Path, pages: usize) {
        use mupdf::pdf::PdfDocument;
        use mupdf::shape::{Shape, TextOptions};
        use mupdf::{Point, Size};

        let mut doc = PdfDocument::new();
        for index in 0..pages {
            let mut page = doc.new_page(Size::A4).unwrap();
            let mut shape = Shape::new(&mut page).unwrap();
            shape
                .insert_text(
                    Point::new(72.0, 96.0),
                    &format!("Hello Marker page {index}"),
                    &TextOptions {
                        fontsize: 18.0,
                        ..TextOptions::default()
                    },
                )
                .unwrap()
                .commit(&mut doc, true)
                .unwrap();
        }
        doc.save(path.to_str().unwrap()).unwrap();
    }

    fn empty_save() -> SaveSnapshot {
        SaveSnapshot {
            upserts: Vec::new(),
            deletes: Vec::new(),
            math_pdfs: HashMap::new(),
            inline_math: Vec::new(),
            rich_text_parents: Vec::new(),
        }
    }

    #[cfg(target_os = "linux")]
    fn thread_names() -> Vec<String> {
        let mut names = Vec::new();
        let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
            return names;
        };
        for task in tasks.flatten() {
            let Ok(name) = std::fs::read_to_string(task.path().join("comm")) else {
                continue;
            };
            names.push(name.trim().to_string());
        }
        names
    }

    #[test]
    fn search_and_glyphs_run_off_the_tile_thread() {
        let dir = std::env::temp_dir().join(format!(
            "marker-read-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.pdf");
        write_sample(&path, 4);

        let ctx = egui::Context::default();
        let tile_wanted = Arc::new(Mutex::new(HashMap::new()));
        let worker = PdfWorker::spawn(ctx, Arc::clone(&tile_wanted));
        let gen = 3u64;
        worker.open(gen, path);
        let opened = wait_until(&worker, |replies| {
            replies
                .iter()
                .any(|reply| matches!(reply, PdfReply::Opened(Ok(_))))
        });
        assert!(
            opened
                .iter()
                .any(|reply| matches!(reply, PdfReply::Opened(Ok(doc)) if doc.pages.len() == 4)),
            "document did not open"
        );
        #[cfg(target_os = "linux")]
        {
            let names = thread_names();
            assert!(
                names.iter().any(|name| name == "marker-pdf"),
                "tile thread missing: {names:?}"
            );
            assert!(
                names.iter().any(|name| name == "marker-pdf-read"),
                "reader thread missing: {names:?}"
            );
        }

        let scale = 1.0f32;
        {
            let mut guard = tile_wanted.lock().unwrap();
            guard
                .entry(gen)
                .or_default()
                .insert(wanted_for(0, scale, 0, 0));
        }
        worker.glyphs(gen, 0);
        worker.search(gen, 1, "Hello".into());
        worker.tile(gen, 0, scale, 0, 0, false, 0);
        let first = wait_until(&worker, |replies| {
            let glyphs = replies.iter().any(
                |reply| matches!(reply, PdfReply::Glyphs { glyphs, .. } if !glyphs.is_empty()),
            );
            let search = replies.iter().any(|reply| {
                matches!(reply, PdfReply::Search { seq: 1, done: true, hits, .. } if !hits.is_empty())
            });
            let tile = replies.iter().any(|reply| {
                matches!(
                    reply,
                    PdfReply::Tile { tile, .. } if tile.page == 0
                ) || matches!(reply, PdfReply::TileMiss { page: 0, .. })
            });
            glyphs && search && tile
        });
        assert_no_fail(&first);

        worker.search(gen, 2, "Hello".into());
        worker.save(gen, empty_save());
        let saved = wait_until(&worker, |replies| {
            let save_ok = replies
                .iter()
                .any(|reply| matches!(reply, PdfReply::Saved { result: Ok(_), .. }));
            let search = replies.iter().any(|reply| {
                matches!(reply, PdfReply::Search { seq: 2, done: true, hits, .. } if !hits.is_empty())
            });
            save_ok && search
        });
        assert_no_fail(&saved);

        worker.insert_page(gen, 0);
        let inserted = wait_until(&worker, |replies| {
            replies.iter().any(
                |reply| matches!(reply, PdfReply::PageInserted { pages, .. } if pages.len() == 5),
            )
        });
        assert!(
            inserted
                .iter()
                .any(|reply| matches!(reply, PdfReply::PageInserted { .. })),
            "insert did not finish"
        );
        worker.delete_page(gen, 4);
        let deleted = wait_until(&worker, |replies| {
            replies.iter().any(
                |reply| matches!(reply, PdfReply::PageDeleted { pages, .. } if pages.len() == 4),
            )
        });
        assert!(
            deleted
                .iter()
                .any(|reply| matches!(reply, PdfReply::PageDeleted { .. })),
            "delete did not finish"
        );
        worker.search(gen, 3, "Hello".into());
        let again = wait_until(&worker, |replies| {
            replies.iter().any(|reply| {
                matches!(reply, PdfReply::Search { seq: 3, done: true, hits, .. } if !hits.is_empty())
            })
        });
        assert_no_fail(&again);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Search for `commit` in Pro Git, then request cold tiles once the first
    /// search chunk has been observed. Prints overlap timings.
    #[test]
    #[ignore]
    fn bench_search_overlaps_tiles() {
        let path =
            PathBuf::from(std::env::var("BENCH_PDF").unwrap_or("/tmp/pdfs/progit.pdf".into()));
        let ctx = egui::Context::default();
        let tile_wanted = Arc::new(Mutex::new(HashMap::new()));
        let worker = PdfWorker::spawn(ctx, Arc::clone(&tile_wanted));
        worker.open(1, path);
        let opened = wait_until(&worker, |replies| {
            replies
                .iter()
                .any(|reply| matches!(reply, PdfReply::Opened(Ok(_))))
        });
        let pages = opened.iter().find_map(|reply| match reply {
            PdfReply::Opened(Ok(doc)) => Some(doc.pages.len()),
            _ => None,
        });
        let page_count = pages.expect("opened");
        let scale = 2.0f32;
        let tiles: Vec<(usize, i32, i32)> = (0..page_count)
            .step_by(30)
            .take(16)
            .map(|page| (page, 0, 0))
            .collect();
        {
            let mut guard = tile_wanted.lock().unwrap();
            let set = guard.entry(1).or_default();
            for &(page, col, row) in &tiles {
                set.insert(wanted_for(page, scale, col, row));
            }
        }
        let t0 = Instant::now();
        worker.search(1, 1, "commit".into());
        let mut saw_progress = false;
        let mut tile_sent = None;
        let mut first_tile = None;
        let mut tiles_done = 0usize;
        let mut all_tiles = None;
        let mut search_done = None;
        let mut tile_before_done = 0usize;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            for reply in worker.poll() {
                match reply {
                    PdfReply::Search { done, .. } => {
                        if !done && !saw_progress {
                            saw_progress = true;
                            worker.tile(1, tiles[0].0, scale, 0, 0, false, 0);
                            for &(page, col, row) in tiles.iter().skip(1) {
                                worker.tile(1, page, scale, col, row, false, page as u32);
                            }
                            tile_sent = Some(t0.elapsed());
                        }
                        if done && search_done.is_none() {
                            search_done = Some(t0.elapsed());
                        }
                    }
                    PdfReply::Tile { .. } | PdfReply::TileMiss { .. } => {
                        if first_tile.is_none() {
                            first_tile = Some(t0.elapsed());
                        }
                        if search_done.is_none() {
                            tile_before_done += 1;
                        }
                        tiles_done += 1;
                        if tiles_done == tiles.len() && all_tiles.is_none() {
                            all_tiles = Some(t0.elapsed());
                        }
                    }
                    _ => {}
                }
            }
            if search_done.is_some() && all_tiles.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        let both = t0.elapsed();
        println!(
            "OVERLAP pages={page_count} tiles={} progress_then_tiles_at={:.1}ms first_tile={:.1}ms all_tiles={:.1}ms search_done={:.1}ms both={:.1}ms tiles_before_search_done={tile_before_done}",
            tiles.len(),
            tile_sent.map(|d| d.as_secs_f64() * 1000.0).unwrap_or(-1.0),
            first_tile.map(|d| d.as_secs_f64() * 1000.0).unwrap_or(-1.0),
            all_tiles.map(|d| d.as_secs_f64() * 1000.0).unwrap_or(-1.0),
            search_done.map(|d| d.as_secs_f64() * 1000.0).unwrap_or(-1.0),
            both.as_secs_f64() * 1000.0,
        );
        assert!(search_done.is_some(), "search did not finish");
        assert!(all_tiles.is_some(), "tiles did not finish");
    }

    fn assert_no_fail(replies: &[PdfReply]) {
        for reply in replies {
            if let PdfReply::Failed { message, .. } = reply {
                panic!("pdf worker failed: {message}");
            }
        }
    }

    fn wait_until(worker: &PdfWorker, pred: impl Fn(&[PdfReply]) -> bool) -> Vec<PdfReply> {
        let start = Instant::now();
        let mut all = Vec::new();
        while start.elapsed() < Duration::from_secs(20) {
            all.extend(worker.poll());
            assert_no_fail(&all);
            if pred(&all) {
                return all;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for replies ({})", all.len());
    }
}
