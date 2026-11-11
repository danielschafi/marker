use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::{DocId, Error, TileKey};
use crate::annot::{Annotation, PageGlyphs};
use crate::geom::PdfRect;
use crate::worker::{self, Channel, ShutdownJob, Wake};

use super::engine::{
    DocumentEngine, OutlineNode, PageInfo, SaveSnapshot, SavedXref, ThumbImage,
    TileImage,
};

pub enum PdfJob {
    Open {
        doc_id: DocId,
        path: PathBuf,
    },
    Close {
        doc_id: DocId,
    },
    Tile {
        doc_id: DocId,
        page: usize,
        scale: f32,
        col: i32,
        row: i32,
        prefetch: bool,
        distance: u32,
    },
    /// Small full-page raster for the thumbnails panel (FT10).
    Thumb {
        doc_id: DocId,
        page: usize,
    },
    Save {
        doc_id: DocId,
        snapshot: SaveSnapshot,
    },
    SaveAs {
        doc_id: DocId,
        path: PathBuf,
        snapshot: SaveSnapshot,
    },
    InsertPage {
        doc_id: DocId,
        after: usize,
    },
    InsertPageAt {
        doc_id: DocId,
        at: usize,
    },
    DeletePage {
        doc_id: DocId,
        index: usize,
    },
    Shutdown,
}

impl ShutdownJob for PdfJob {
    fn shutdown() -> Self {
        Self::Shutdown
    }
}

/// Search and glyph jobs for `marker-pdf-read`. Kept off `PdfJob` so the tile
/// thread never ranks them against raster work.
enum ReadJob {
    Open {
        doc_id: DocId,
        path: PathBuf,
    },
    Close {
        doc_id: DocId,
    },
    /// Drop the reader's document and ack so the tile thread can rewrite the file.
    Release {
        doc_id: DocId,
        ack: Sender<()>,
    },
    Reopen {
        doc_id: DocId,
    },
    Search {
        doc_id: DocId,
        seq: u64,
        needle: String,
        from: usize,
        case_sensitive: bool,
    },
    Glyphs {
        doc_id: DocId,
        page: usize,
    },
    Shutdown,
}

/// Worker → UI replies.
///
/// Error payloads stay `String` here (RF6): engine work uses [`Error`], and this
/// module converts via [`Display`](std::fmt::Display) at the send boundary so
/// `app.rs` (owned by RF5) does not need to change yet.
pub enum PdfReply {
    Opened(Box<OpenedDoc>),
    /// `tile.pixels` are premultiplied RGBA8 (see [`TileImage::pixels`]).
    Tile {
        doc_id: DocId,
        tile: TileImage,
    },
    TileMiss {
        doc_id: DocId,
        page: usize,
        scale: f32,
        col: i32,
        row: i32,
    },
    /// Thumbnail panel reply — not a document tile (do not feed into tile LRU).
    Thumb {
        doc_id: DocId,
        thumb: ThumbImage,
    },
    ThumbMiss {
        doc_id: DocId,
        page: usize,
    },
    Glyphs {
        doc_id: DocId,
        page: usize,
        glyphs: PageGlyphs,
    },
    Search {
        doc_id: DocId,
        seq: u64,
        hits: Vec<(usize, Vec<PdfRect>)>,
        done: bool,
    },
    Saved {
        doc_id: DocId,
        /// Stringly error at the reply boundary; see [`PdfReply`] module note.
        result: Result<Vec<SavedXref>, String>,
    },
    /// Successful Save As retargets the open document onto `path`.
    SavedAs {
        doc_id: DocId,
        path: PathBuf,
        /// Stringly error at the reply boundary; see [`PdfReply`] module note.
        result: Result<Vec<SavedXref>, String>,
    },
    PageInserted {
        doc_id: DocId,
        index: usize,
        pages: Vec<PageInfo>,
    },
    PageDeleted {
        doc_id: DocId,
        index: usize,
        pages: Vec<PageInfo>,
    },
    Failed {
        doc_id: Option<DocId>,
        message: String,
    },
}

pub struct OpenedDoc {
    pub doc_id: DocId,
    pub path: PathBuf,
    pub pages: Vec<PageInfo>,
    pub outline: Vec<OutlineNode>,
    pub annotations: Vec<Annotation>,
    /// Present when `path` is a Poppler-repaired sidecar of this original file.
    pub repaired_from: Option<PathBuf>,
}

pub struct PdfWorker {
    channel: Channel<PdfJob, PdfReply>,
    reads: Sender<ReadJob>,
}

impl PdfWorker {
    pub fn spawn(
        wake: Arc<dyn Wake>,
        tile_wanted: Arc<Mutex<HashMap<DocId, HashSet<TileKey>>>>,
        thumb_wanted: Arc<Mutex<HashMap<DocId, HashSet<usize>>>>,
    ) -> Self {
        let (read_tx, read_rx) = mpsc::channel();
        let reads_for_tiles = read_tx.clone();
        let reads_for_reader = read_tx.clone();
        let channel = Channel::spawn("marker-pdf", wake, move |wake, job_rx, reply_tx| {
            let replies_for_reader = reply_tx.clone();
            let wake_for_reader = Arc::clone(&wake);
            thread::Builder::new()
                .name("marker-pdf-read".into())
                .spawn(move || {
                    reader_loop(
                        wake_for_reader,
                        read_rx,
                        reads_for_reader,
                        replies_for_reader,
                    )
                })
                .expect("pdf read thread");
            worker_loop(
                wake,
                job_rx,
                reply_tx,
                reads_for_tiles,
                tile_wanted,
                thumb_wanted,
            );
        });
        Self {
            channel,
            reads: read_tx,
        }
    }

    pub fn open(&self, doc_id: DocId, path: PathBuf) {
        self.channel.send(PdfJob::Open { doc_id, path });
    }

    pub fn close(&self, doc_id: DocId) {
        self.channel.send(PdfJob::Close { doc_id });
    }

    // RF7 packs tile coords into a request struct.
    #[allow(clippy::too_many_arguments)]
    pub fn tile(
        &self,
        doc_id: DocId,
        page: usize,
        scale: f32,
        col: i32,
        row: i32,
        prefetch: bool,
        distance: u32,
    ) {
        self.channel.send(PdfJob::Tile {
            doc_id,
            page,
            scale,
            col,
            row,
            prefetch,
            distance,
        });
    }

    pub fn thumb(&self, doc_id: DocId, page: usize) {
        self.channel.send(PdfJob::Thumb { doc_id, page });
    }

    pub fn glyphs(&self, doc_id: DocId, page: usize) {
        let _ = self.reads.send(ReadJob::Glyphs { doc_id, page });
    }

    pub fn search(&self, doc_id: DocId, seq: u64, needle: String) {
        // Case flag lives in Settings; app.rs cannot pass it this wave (FP9).
        let case_sensitive = crate::settings::live_search_case_sensitive();
        let _ = self.reads.send(ReadJob::Search {
            doc_id,
            seq,
            needle,
            from: 0,
            case_sensitive,
        });
    }

    pub fn save(&self, doc_id: DocId, snapshot: SaveSnapshot) {
        self.channel.send(PdfJob::Save { doc_id, snapshot });
    }

    pub fn save_as(&self, doc_id: DocId, path: PathBuf, snapshot: SaveSnapshot) {
        self.channel.send(PdfJob::SaveAs {
            doc_id,
            path,
            snapshot,
        });
    }

    pub fn insert_page(&self, doc_id: DocId, after: usize) {
        self.channel.send(PdfJob::InsertPage { doc_id, after });
    }

    pub fn insert_page_at(&self, doc_id: DocId, at: usize) {
        self.channel.send(PdfJob::InsertPageAt { doc_id, at });
    }

    pub fn delete_page(&self, doc_id: DocId, index: usize) {
        self.channel.send(PdfJob::DeletePage { doc_id, index });
    }

    pub fn poll(&self) -> Vec<PdfReply> {
        self.channel.poll()
    }
}

impl Drop for PdfWorker {
    fn drop(&mut self) {
        // Channel's Drop sends PdfJob::Shutdown. The reader keeps its own
        // Senders alive inside the tile/reader threads, so it needs an
        // explicit shutdown (dropping `reads` alone would not disconnect).
        let _ = self.reads.send(ReadJob::Shutdown);
    }
}

/// Tile-thread order. Search and glyphs are not ranked here; they run on
/// `marker-pdf-read`. Thumbs sit below tiles so the panel never starves
/// document work (and never shares the reader with search).
fn job_sort_key(job: &PdfJob) -> (u8, u32) {
    match job {
        PdfJob::Shutdown
        | PdfJob::Open { .. }
        | PdfJob::Close { .. }
        | PdfJob::Save { .. }
        | PdfJob::SaveAs { .. }
        | PdfJob::InsertPage { .. }
        | PdfJob::InsertPageAt { .. }
        | PdfJob::DeletePage { .. } => (0, 0),
        PdfJob::Tile {
            prefetch, distance, ..
        } => {
            if *prefetch {
                (2, *distance)
            } else {
                (1, *distance)
            }
        }
        PdfJob::Thumb { page, .. } => (4, *page as u32),
    }
}

impl PdfReply {
    fn failed(doc_id: DocId, err: impl std::fmt::Display) -> Self {
        Self::Failed {
            doc_id: Some(doc_id),
            message: err.to_string(),
        }
    }
}

/// Map engine [`Error`] to the stringly [`PdfReply`] boundary.
fn reply_err(err: Error) -> String {
    err.to_string()
}

fn engine_mut(
    engines: &mut HashMap<DocId, DocumentEngine>,
    doc_id: DocId,
) -> Result<&mut DocumentEngine, PdfReply> {
    engines
        .get_mut(&doc_id)
        .ok_or_else(|| PdfReply::failed(doc_id, Error::NoDocument))
}

struct WorkerCtx<'a> {
    wake: &'a dyn Wake,
    replies: &'a Sender<PdfReply>,
    reads: &'a Sender<ReadJob>,
    engines: &'a mut HashMap<DocId, DocumentEngine>,
}

impl WorkerCtx<'_> {
    fn send(&self, msg: PdfReply) {
        worker::reply(self.wake, self.replies, msg);
    }
}

fn handle_open(ctx: &mut WorkerCtx<'_>, doc_id: DocId, path: PathBuf) {
    match DocumentEngine::open(&path) {
        Ok(loaded) => {
            let open_path = loaded.engine.path().to_path_buf();
            let opened = OpenedDoc {
                doc_id,
                path: open_path.clone(),
                pages: loaded.engine.pages().to_vec(),
                outline: loaded.engine.outline().to_vec(),
                annotations: loaded.annotations,
                repaired_from: loaded.repaired_from,
            };
            ctx.engines.insert(doc_id, loaded.engine);
            let _ = ctx.reads.send(ReadJob::Open {
                doc_id,
                path: open_path,
            });
            ctx.send(PdfReply::Opened(Box::new(opened)));
        }
        Err(err) => ctx.send(PdfReply::failed(doc_id, err)),
    }
}

fn handle_tile(
    ctx: &mut WorkerCtx<'_>,
    doc_id: DocId,
    page: usize,
    scale: f32,
    col: i32,
    row: i32,
    tile_wanted: &Arc<Mutex<HashMap<DocId, HashSet<TileKey>>>>,
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
        .get(&doc_id)
        .is_some_and(|set| set.contains(&key));
    if !still_wanted {
        ctx.send(PdfReply::TileMiss {
            doc_id,
            page,
            scale,
            col,
            row,
        });
        return;
    }
    let Some(engine) = ctx.engines.get_mut(&doc_id) else {
        return;
    };
    match engine.render_tile(page, scale, col, row) {
        Ok(Some(tile)) => ctx.send(PdfReply::Tile { doc_id, tile }),
        Ok(None) => {
            ctx.send(PdfReply::TileMiss {
                doc_id,
                page,
                scale,
                col,
                row,
            });
        }
        Err(err) => ctx.send(PdfReply::failed(doc_id, err)),
    }
}

fn handle_thumb(
    ctx: &mut WorkerCtx<'_>,
    doc_id: DocId,
    page: usize,
    thumb_wanted: &Arc<Mutex<HashMap<DocId, HashSet<usize>>>>,
) {
    let still_wanted = thumb_wanted
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&doc_id)
        .is_some_and(|set| set.contains(&page));
    if !still_wanted {
        ctx.send(PdfReply::ThumbMiss { doc_id, page });
        return;
    }
    let Some(engine) = ctx.engines.get_mut(&doc_id) else {
        return;
    };
    match engine.render_thumb(page) {
        Ok(Some(thumb)) => ctx.send(PdfReply::Thumb { doc_id, thumb }),
        Ok(None) => ctx.send(PdfReply::ThumbMiss { doc_id, page }),
        Err(err) => ctx.send(PdfReply::failed(doc_id, err)),
    }
}

fn handle_save(ctx: &mut WorkerCtx<'_>, doc_id: DocId, snapshot: SaveSnapshot) {
    release_reader(ctx.reads, doc_id);
    let reply = match engine_mut(ctx.engines, doc_id) {
        Ok(engine) => PdfReply::Saved {
            doc_id,
            result: engine.save(&snapshot).map_err(reply_err),
        },
        Err(_) => PdfReply::Saved {
            doc_id,
            result: Err(reply_err(Error::NoDocument)),
        },
    };
    reopen_reader(ctx.reads, doc_id);
    ctx.send(reply);
}

fn handle_save_as(ctx: &mut WorkerCtx<'_>, doc_id: DocId, path: PathBuf, snapshot: SaveSnapshot) {
    release_reader(ctx.reads, doc_id);
    let reply = match engine_mut(ctx.engines, doc_id) {
        Ok(engine) => {
            let result = engine.save_as(&snapshot, &path).map_err(reply_err);
            // Reader still holds the old path; retarget while suspended so reopen
            // loads the copy we just wrote.
            if result.is_ok() {
                let _ = ctx.reads.send(ReadJob::Open {
                    doc_id,
                    path: path.clone(),
                });
            }
            PdfReply::SavedAs {
                doc_id,
                path,
                result,
            }
        }
        Err(_) => PdfReply::SavedAs {
            doc_id,
            path,
            result: Err(reply_err(Error::NoDocument)),
        },
    };
    reopen_reader(ctx.reads, doc_id);
    ctx.send(reply);
}

fn handle_insert_page(ctx: &mut WorkerCtx<'_>, doc_id: DocId, after: usize) {
    release_reader(ctx.reads, doc_id);
    let reply = match engine_mut(ctx.engines, doc_id) {
        Ok(engine) => match engine.insert_blank_page(after) {
            Ok((index, pages)) => PdfReply::PageInserted {
                doc_id,
                index,
                pages,
            },
            Err(message) => PdfReply::failed(doc_id, message),
        },
        Err(reply) => reply,
    };
    reopen_reader(ctx.reads, doc_id);
    ctx.send(reply);
}

fn handle_insert_page_at(ctx: &mut WorkerCtx<'_>, doc_id: DocId, at: usize) {
    release_reader(ctx.reads, doc_id);
    let reply = match engine_mut(ctx.engines, doc_id) {
        Ok(engine) => match engine.insert_blank_page_at(at) {
            Ok((index, pages)) => PdfReply::PageInserted {
                doc_id,
                index,
                pages,
            },
            Err(message) => PdfReply::failed(doc_id, message),
        },
        Err(reply) => reply,
    };
    reopen_reader(ctx.reads, doc_id);
    ctx.send(reply);
}

fn handle_delete_page(ctx: &mut WorkerCtx<'_>, doc_id: DocId, index: usize) {
    release_reader(ctx.reads, doc_id);
    let reply = match engine_mut(ctx.engines, doc_id) {
        Ok(engine) => match engine.delete_page_at(index) {
            Ok(pages) => PdfReply::PageDeleted {
                doc_id,
                index,
                pages,
            },
            Err(message) => PdfReply::failed(doc_id, message),
        },
        Err(reply) => reply,
    };
    reopen_reader(ctx.reads, doc_id);
    ctx.send(reply);
}

fn handle_job(
    ctx: &mut WorkerCtx<'_>,
    job: PdfJob,
    tile_wanted: &Arc<Mutex<HashMap<DocId, HashSet<TileKey>>>>,
    thumb_wanted: &Arc<Mutex<HashMap<DocId, HashSet<usize>>>>,
) -> bool {
    match job {
        PdfJob::Shutdown => true,
        PdfJob::Open { doc_id, path } => {
            handle_open(ctx, doc_id, path);
            false
        }
        PdfJob::Close { doc_id } => {
            ctx.engines.remove(&doc_id);
            let _ = ctx.reads.send(ReadJob::Close { doc_id });
            false
        }
        PdfJob::Tile {
            doc_id,
            page,
            scale,
            col,
            row,
            ..
        } => {
            handle_tile(ctx, doc_id, page, scale, col, row, tile_wanted);
            false
        }
        PdfJob::Thumb { doc_id, page } => {
            handle_thumb(ctx, doc_id, page, thumb_wanted);
            false
        }
        PdfJob::Save { doc_id, snapshot } => {
            handle_save(ctx, doc_id, snapshot);
            false
        }
        PdfJob::SaveAs {
            doc_id,
            path,
            snapshot,
        } => {
            handle_save_as(ctx, doc_id, path, snapshot);
            false
        }
        PdfJob::InsertPage { doc_id, after } => {
            handle_insert_page(ctx, doc_id, after);
            false
        }
        PdfJob::InsertPageAt { doc_id, at } => {
            handle_insert_page_at(ctx, doc_id, at);
            false
        }
        PdfJob::DeletePage { doc_id, index } => {
            handle_delete_page(ctx, doc_id, index);
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
    wake: Arc<dyn Wake>,
    jobs: Receiver<PdfJob>,
    replies: Sender<PdfReply>,
    reads: Sender<ReadJob>,
    tile_wanted: Arc<Mutex<HashMap<DocId, HashSet<TileKey>>>>,
    thumb_wanted: Arc<Mutex<HashMap<DocId, HashSet<usize>>>>,
) {
    let mut engines: HashMap<DocId, DocumentEngine> = HashMap::new();
    let mut queued = VecDeque::new();
    while let Some(job) = dequeue_job(&jobs, &mut queued) {
        let mut worker = WorkerCtx {
            wake: wake.as_ref(),
            replies: &replies,
            reads: &reads,
            engines: &mut engines,
        };
        if handle_job(&mut worker, job, &tile_wanted, &thumb_wanted) {
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
fn release_reader(reads: &Sender<ReadJob>, doc_id: DocId) {
    let (ack_tx, ack_rx) = mpsc::channel();
    if reads
        .send(ReadJob::Release {
            doc_id,
            ack: ack_tx,
        })
        .is_err()
    {
        return;
    }
    let _ = ack_rx.recv_timeout(READER_RELEASE_WAIT);
}

fn reopen_reader(reads: &Sender<ReadJob>, doc_id: DocId) {
    let _ = reads.send(ReadJob::Reopen { doc_id });
}

struct ReaderState {
    engines: HashMap<DocId, DocumentEngine>,
    paths: HashMap<DocId, PathBuf>,
    latest_search: HashMap<DocId, u64>,
    /// Documents dropped so the tile thread can write the file.
    suspended: HashSet<DocId>,
    /// Search and glyph jobs waiting for a reopen.
    deferred: VecDeque<ReadJob>,
}

fn reader_loop(
    wake: Arc<dyn Wake>,
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
        wake: wake.as_ref(),
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

fn is_suspended_work(job: &ReadJob, suspended: &HashSet<DocId>) -> bool {
    match job {
        ReadJob::Search { doc_id, .. } | ReadJob::Glyphs { doc_id, .. } => {
            suspended.contains(doc_id)
        }
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

fn discard_work(queue: &mut VecDeque<ReadJob>, doc_id: DocId) {
    queue.retain(|job| !is_doc_id_work(job, doc_id));
}

fn is_doc_id_work(job: &ReadJob, doc_id: DocId) -> bool {
    match job {
        ReadJob::Search {
            doc_id: job_doc_id, ..
        }
        | ReadJob::Glyphs {
            doc_id: job_doc_id, ..
        } => *job_doc_id == doc_id,
        ReadJob::Open { .. }
        | ReadJob::Close { .. }
        | ReadJob::Release { .. }
        | ReadJob::Reopen { .. }
        | ReadJob::Shutdown => false,
    }
}

fn flush_deferred(state: &mut ReaderState, queued: &mut VecDeque<ReadJob>, doc_id: DocId) {
    let pending = std::mem::take(&mut state.deferred);
    for job in pending {
        if is_doc_id_work(&job, doc_id) {
            queued.push_back(job);
        } else {
            state.deferred.push_back(job);
        }
    }
}

fn open_engine(path: &Path) -> Result<DocumentEngine, Error> {
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
    doc_id: DocId,
    path: &Path,
) -> bool {
    match open_engine(path) {
        Ok(engine) => {
            state.engines.insert(doc_id, engine);
            state.suspended.remove(&doc_id);
            flush_deferred(state, queued, doc_id);
            true
        }
        Err(_) => {
            state.engines.remove(&doc_id);
            false
        }
    }
}

struct ReadIo<'a> {
    wake: &'a dyn Wake,
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
        ReadJob::Open { doc_id, path } => {
            state.paths.insert(doc_id, path.clone());
            if !state.suspended.contains(&doc_id) {
                let _ = install_engine(state, queued, doc_id, &path);
            }
            false
        }
        ReadJob::Close { doc_id } => {
            state.engines.remove(&doc_id);
            state.paths.remove(&doc_id);
            state.latest_search.remove(&doc_id);
            state.suspended.remove(&doc_id);
            discard_work(&mut state.deferred, doc_id);
            discard_work(queued, doc_id);
            false
        }
        ReadJob::Release { doc_id, ack } => {
            state.engines.remove(&doc_id);
            state.suspended.insert(doc_id);
            let _ = ack.send(());
            false
        }
        ReadJob::Reopen { doc_id } => {
            let Some(path) = state.paths.get(&doc_id).cloned() else {
                state.suspended.remove(&doc_id);
                flush_deferred(state, queued, doc_id);
                return false;
            };
            if !install_engine(state, queued, doc_id, &path) {
                state.suspended.insert(doc_id);
            }
            false
        }
        ReadJob::Glyphs { doc_id, page } => {
            handle_glyphs(io, state, doc_id, page);
            false
        }
        ReadJob::Search {
            doc_id,
            seq,
            needle,
            from,
            case_sensitive,
        } => {
            handle_search(io, state, doc_id, seq, needle, from, case_sensitive);
            false
        }
    }
}

fn handle_glyphs(io: &ReadIo<'_>, state: &ReaderState, doc_id: DocId, page: usize) {
    let Some(engine) = state.engines.get(&doc_id) else {
        return;
    };
    match engine.glyphs(page) {
        Ok(glyphs) => worker::reply(
            io.wake,
            io.replies,
            PdfReply::Glyphs {
                doc_id,
                page,
                glyphs,
            },
        ),
        Err(err) => worker::reply(io.wake, io.replies, PdfReply::failed(doc_id, err)),
    }
}

fn handle_search(
    io: &ReadIo<'_>,
    state: &mut ReaderState,
    doc_id: DocId,
    seq: u64,
    needle: String,
    from: usize,
    case_sensitive: bool,
) {
    if from == 0 {
        state.latest_search.insert(doc_id, seq);
    }
    if state.latest_search.get(&doc_id) != Some(&seq) {
        return;
    }
    let Some(engine) = state.engines.get(&doc_id) else {
        return;
    };
    let page_count = engine.pages().len();
    let end = (from + 2).min(page_count);
    let mut hits = Vec::new();
    let mut failed = None;
    for page in from..end {
        match engine.search_page(page, &needle, case_sensitive) {
            Ok(quads) if !quads.is_empty() => hits.push((page, quads)),
            Ok(_) => {}
            Err(err) => {
                failed = Some(err);
                break;
            }
        }
    }
    if let Some(message) = failed {
        worker::reply(io.wake, io.replies, PdfReply::failed(doc_id, message));
        return;
    }
    let done = end >= page_count;
    worker::reply(
        io.wake,
        io.replies,
        PdfReply::Search {
            doc_id,
            seq,
            hits,
            done,
        },
    );
    if !done {
        let _ = io.jobs_tx.send(ReadJob::Search {
            doc_id,
            seq,
            needle,
            from: end,
            case_sensitive,
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

    use super::{DocId, PdfReply, PdfWorker, TileKey};
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

    struct SaveFixture {
        dir: PathBuf,
        worker: PdfWorker,
        id: DocId,
    }

    impl SaveFixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "marker-save-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&dir).unwrap();
            write_sample(&dir.join("sample.pdf"), 1);
            let worker = PdfWorker::spawn(
                crate::worker::NoopWake::arc(),
                Arc::new(Mutex::new(HashMap::new())),
                Arc::new(Mutex::new(HashMap::new())),
            );
            Self {
                dir,
                worker,
                id: DocId(139),
            }
        }

        fn open(&self, path: PathBuf) -> Box<super::OpenedDoc> {
            self.worker.open(self.id, path);
            wait_until(&self.worker, |replies| {
                replies
                    .iter()
                    .any(|reply| matches!(reply, PdfReply::Opened(_) | PdfReply::Failed { .. }))
            })
            .into_iter()
            .find_map(|reply| match reply {
                PdfReply::Opened(doc) => Some(doc),
                PdfReply::Failed { message, .. } => panic!("open failed: {message}"),
                _ => None,
            })
            .expect("open reply")
        }

        fn save(
            &self,
            snapshot: SaveSnapshot,
        ) -> Result<Vec<crate::pdf::engine::SavedXref>, String> {
            self.worker.save(self.id, snapshot);
            wait_until(&self.worker, |replies| {
                replies
                    .iter()
                    .any(|reply| matches!(reply, PdfReply::Saved { .. }))
            })
            .into_iter()
            .find_map(|reply| match reply {
                PdfReply::Saved { result, .. } => Some(result),
                _ => None,
            })
            .expect("save reply")
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<_> = std::fs::read_dir(&self.dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for SaveFixture {
        fn drop(&mut self) {
            self.worker.close(self.id);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn saving_and_reopening_leaves_only_the_pdf_and_preserves_existing_backup() {
        let fixture = SaveFixture::new();
        let path = fixture.dir.join("sample.pdf");
        fixture.open(path.clone());
        assert_eq!(fixture.names(), ["sample.pdf"]);
        fixture.save(empty_save()).unwrap();
        assert_eq!(fixture.names(), ["sample.pdf"]);
        std::fs::write(fixture.dir.join("sample.pdf.bak"), b"existing backup").unwrap();
        fixture.save(empty_save()).unwrap();
        fixture.worker.close(fixture.id);
        fixture.open(path);
        assert_eq!(fixture.names(), ["sample.pdf", "sample.pdf.bak"]);
        assert_eq!(
            std::fs::read(fixture.dir.join("sample.pdf.bak")).unwrap(),
            b"existing backup"
        );
    }

    #[test]
    fn failed_save_as_reports_error_preserves_source_and_cleans_candidate() {
        let fixture = SaveFixture::new();
        let path = fixture.dir.join("sample.pdf");
        let original = std::fs::read(&path).unwrap();
        fixture.open(path.clone());
        let destination = fixture.dir.join("destination.pdf");
        std::fs::create_dir(&destination).unwrap();
        fixture
            .worker
            .save_as(fixture.id, destination, empty_save());
        let replies = wait_until(&fixture.worker, |replies| {
            replies
                .iter()
                .any(|reply| matches!(reply, PdfReply::SavedAs { .. }))
        });
        assert!(replies
            .iter()
            .any(|reply| matches!(reply, PdfReply::SavedAs { result: Err(_), .. })));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(fixture.names(), ["destination.pdf", "sample.pdf"]);
        fixture.save(empty_save()).unwrap();
        fixture.worker.close(fixture.id);
        fixture.open(path);
    }

    #[test]
    #[cfg(unix)]
    fn save_as_preserves_symbolic_link_target_and_access_mode() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let fixture = SaveFixture::new();
        let source = fixture.dir.join("sample.pdf");
        let original = std::fs::read(&source).unwrap();
        let destination = fixture.dir.join("destination.pdf");
        write_sample(&destination, 2);
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o640)).unwrap();
        let link = fixture.dir.join("link.pdf");
        symlink("destination.pdf", &link).unwrap();
        fixture.open(source.clone());
        fixture
            .worker
            .save_as(fixture.id, link.clone(), empty_save());
        let replies = wait_until(&fixture.worker, |replies| {
            replies
                .iter()
                .any(|reply| matches!(reply, PdfReply::SavedAs { .. }))
        });
        assert!(replies
            .iter()
            .any(|reply| matches!(reply, PdfReply::SavedAs { result: Ok(_), .. })));
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(std::fs::read(&source).unwrap(), original);
        fixture.worker.close(fixture.id);
        assert_eq!(fixture.open(link).pages.len(), 1);
        assert_eq!(
            fixture.names(),
            ["destination.pdf", "link.pdf", "sample.pdf"]
        );
    }

    #[cfg(unix)]
    struct PausedSave {
        child: std::process::Child,
        ready: PathBuf,
        resume: PathBuf,
    }

    #[cfg(unix)]
    impl PausedSave {
        fn start(fixture: &SaveFixture) -> (Self, PathBuf) {
            let ready = fixture.dir.with_extension("ready");
            let resume = fixture.dir.with_extension("resume");
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "pdf::worker::tests::save_in_subprocess",
                    "--ignored",
                    "--nocapture",
                ])
                .env("MARKER_TEST_SAVE_PDF", fixture.dir.join("sample.pdf"))
                .env("MARKER_TEST_SAVE_READY", &ready)
                .env("MARKER_TEST_SAVE_RESUME", &resume)
                .spawn()
                .unwrap();
            let mut process = Self {
                child,
                ready,
                resume,
            };
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Ok(candidate) = std::fs::read_to_string(&process.ready) {
                    return (process, PathBuf::from(candidate));
                }
                assert!(
                    process.child.try_wait().unwrap().is_none(),
                    "save process exited before pausing"
                );
                assert!(Instant::now() < deadline, "save process did not pause");
                thread::sleep(Duration::from_millis(5));
            }
        }

        fn resume(&mut self) {
            std::fs::write(&self.resume, b"resume").unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    assert!(status.success());
                    return;
                }
                assert!(Instant::now() < deadline, "save process did not complete");
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    #[cfg(unix)]
    impl Drop for PausedSave {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_file(&self.ready);
            let _ = std::fs::remove_file(&self.resume);
        }
    }

    #[test]
    #[ignore = "subprocess helper, run by the interrupted-save test"]
    #[cfg(unix)]
    fn save_in_subprocess() {
        use crate::annot::{AnnotKind, Annotation};
        use crate::geom::{PdfRect, Rgb};
        let path = PathBuf::from(std::env::var_os("MARKER_TEST_SAVE_PDF").expect("subprocess PDF"));
        let worker = PdfWorker::spawn(
            crate::worker::NoopWake::arc(),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
        );
        let id = DocId(139);
        worker.open(id, path);
        let replies = wait_until(&worker, |replies| {
            replies
                .iter()
                .any(|reply| matches!(reply, PdfReply::Opened(_) | PdfReply::Failed { .. }))
        });
        assert_no_fail(&replies);
        let xref = replies.iter().find_map(|reply| match reply {
            PdfReply::Opened(doc) => doc
                .annotations
                .iter()
                .find(|annotation| annotation.id == 139)
                .and_then(|annotation| annotation.xref),
            _ => None,
        });
        let mut snapshot = empty_save();
        snapshot.upserts.push(Annotation {
            id: 139,
            page: 0,
            xref,
            dirty: true,
            revision: 0,
            kind: AnnotKind::Note {
                rect: PdfRect::new(72.0, 120.0, 96.0, 144.0),
                content: "completed child save".into(),
                color: Rgb::new(255, 196, 0),
            },
        });
        worker.save(id, snapshot);
        let replies = wait_until(&worker, |replies| {
            replies
                .iter()
                .any(|reply| matches!(reply, PdfReply::Saved { .. }))
        });
        assert!(replies
            .iter()
            .any(|reply| matches!(reply, PdfReply::Saved { result: Ok(_), .. })));
    }

    #[test]
    #[cfg(unix)]
    fn opening_protects_another_process_save_then_cleans_its_abandoned_candidate() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = SaveFixture::new();
        let path = fixture.dir.join("sample.pdf");
        let original = std::fs::read(&path).unwrap();
        let (mut process, candidate) = PausedSave::start(&fixture);
        assert_eq!(
            std::fs::metadata(&candidate).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(fixture.open(path.clone()).annotations.is_empty());
        assert!(
            candidate.exists(),
            "another process's active candidate must survive open"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "target must stay intact before replacement"
        );
        process.resume();
        drop(process);
        fixture.worker.close(fixture.id);
        let opened = fixture.open(path.clone());
        assert_eq!(opened.annotations.len(), 1);
        assert_eq!(opened.annotations[0].id, 139);
        assert_eq!(fixture.names(), ["sample.pdf"]);

        for (name, contents) in [
            ("sample.pdf.bak", "original backup"),
            ("sample.pdf.marker-tmp", "ambiguous legacy save"),
            (
                "sample.pdf.marker-save-v1-unknown.marker-tmp",
                "unrecognized owner",
            ),
            (
                "other.pdf.marker-save-v1-999999-123-0.marker-tmp",
                "unrelated file",
            ),
        ] {
            std::fs::write(fixture.dir.join(name), contents).unwrap();
        }
        let saved = std::fs::read(&path).unwrap();
        let (mut process, abandoned) = PausedSave::start(&fixture);
        process.child.kill().unwrap();
        process.child.wait().unwrap();
        drop(process);
        assert!(abandoned.exists());
        std::fs::set_permissions(&abandoned, std::fs::Permissions::from_mode(0o400)).unwrap();
        fixture.worker.close(fixture.id);
        let opened = fixture.open(path);
        assert_eq!(opened.annotations.len(), 1);
        assert!(
            !abandoned.exists(),
            "reopening must remove the dead process's candidate"
        );
        assert_eq!(
            std::fs::read(fixture.dir.join("sample.pdf")).unwrap(),
            saved
        );
        assert_eq!(
            fixture.names(),
            [
                "other.pdf.marker-save-v1-999999-123-0.marker-tmp",
                "sample.pdf",
                "sample.pdf.bak",
                "sample.pdf.marker-save-v1-unknown.marker-tmp",
                "sample.pdf.marker-tmp",
            ]
        );
        assert_eq!(
            std::fs::read(fixture.dir.join("sample.pdf.bak")).unwrap(),
            b"original backup"
        );
    }

    #[test]
    fn repeated_saves_compaction_and_save_as_preserve_annotations_and_refresh_reader() {
        use crate::annot::{AnnotKind, Annotation};
        use crate::geom::{PdfRect, Rgb};
        let fixture = SaveFixture::new();
        let source = fixture.dir.join("sample.pdf");
        fixture.open(source.clone());
        let first_content = "first saved note ".repeat(2048);
        let second_content = "second saved note ".repeat(2048);
        let mut note = Annotation {
            id: 139,
            page: 0,
            xref: None,
            dirty: true,
            revision: 0,
            kind: AnnotKind::Note {
                rect: PdfRect::new(72.0, 120.0, 96.0, 144.0),
                content: first_content,
                color: Rgb::new(255, 196, 0),
            },
        };
        let mut snapshot = empty_save();
        snapshot.upserts.push(note.clone());
        note.xref = Some(fixture.save(snapshot).unwrap()[0].xref);
        let first_len = std::fs::metadata(&source).unwrap().len();
        if let AnnotKind::Note { content, .. } = &mut note.kind {
            *content = second_content.clone();
        }
        let mut snapshot = empty_save();
        snapshot.upserts.push(note.clone());
        note.xref = Some(fixture.save(snapshot).unwrap()[0].xref);
        assert!(
            std::fs::metadata(&source).unwrap().len() < first_len * 3 / 2,
            "compaction should not retain every large annotation revision"
        );

        fixture.worker.glyphs(fixture.id, 0);
        fixture.worker.search(fixture.id, 139, "Hello".into());
        let replies = wait_until(&fixture.worker, |replies| {
            replies.iter().any(|reply| matches!(reply, PdfReply::Glyphs { glyphs, .. } if !glyphs.is_empty()))
                && replies.iter().any(|reply| matches!(reply, PdfReply::Search { seq: 139, done: true, hits, .. } if !hits.is_empty()))
        });
        assert_no_fail(&replies);
        fixture.worker.close(fixture.id);
        let opened = fixture.open(source.clone());
        assert_eq!(opened.annotations.len(), 1);
        assert!(
            matches!(&opened.annotations[0].kind, AnnotKind::Note { content, .. } if content == &second_content)
        );
        note.xref = opened.annotations[0].xref;
        let source_bytes = std::fs::read(&source).unwrap();
        let destination = fixture.dir.join("copy.pdf");
        let mut snapshot = empty_save();
        snapshot.upserts.push(note.clone());
        fixture
            .worker
            .save_as(fixture.id, destination.clone(), snapshot);
        let replies = wait_until(&fixture.worker, |replies| {
            replies
                .iter()
                .any(|reply| matches!(reply, PdfReply::SavedAs { .. }))
        });
        note.xref = replies.into_iter().find_map(|reply| match reply {
            PdfReply::SavedAs { result, .. } => Some(result.unwrap()[0].xref),
            _ => None,
        });
        if let AnnotKind::Note { content, .. } = &mut note.kind {
            *content = "saved after retarget".into();
        }
        let mut snapshot = empty_save();
        snapshot.upserts.push(note);
        fixture.save(snapshot).unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), source_bytes);
        fixture.worker.close(fixture.id);
        let opened = fixture.open(destination);
        assert_eq!(opened.annotations.len(), 1);
        assert!(
            matches!(&opened.annotations[0].kind, AnnotKind::Note { content, .. } if content == "saved after retarget")
        );
        fixture.worker.close(fixture.id);
        fixture.open(source);
        assert_eq!(fixture.names(), ["copy.pdf", "sample.pdf"]);
    }

    #[test]
    #[cfg(unix)]
    fn abandoned_cleanup_permission_error_is_reported_and_open_can_be_retried() {
        use std::os::unix::fs::PermissionsExt;
        // Root bypasses directory write permissions, so this failure is unavailable.
        // SAFETY: geteuid takes no arguments and only reports the effective user ID.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        struct RestoreDirectory(PathBuf);
        impl Drop for RestoreDirectory {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        let fixture = SaveFixture::new();
        let path = fixture.dir.join("sample.pdf");
        let original = std::fs::read(&path).unwrap();
        let (mut process, abandoned) = PausedSave::start(&fixture);
        process.child.kill().unwrap();
        process.child.wait().unwrap();
        drop(process);
        let restore = RestoreDirectory(fixture.dir.clone());
        std::fs::set_permissions(&fixture.dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        fixture.worker.open(fixture.id, path.clone());
        let replies = wait_including_failure(&fixture.worker, |replies| {
            replies
                .iter()
                .any(|reply| matches!(reply, PdfReply::Opened(_) | PdfReply::Failed { .. }))
        });
        assert!(replies.iter().any(|reply| matches!(reply, PdfReply::Failed { message, .. } if message.contains("could not remove abandoned save"))));
        assert!(abandoned.exists());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        drop(restore);
        fixture.open(path);
        assert!(!abandoned.exists());
        assert_eq!(fixture.names(), ["sample.pdf"]);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn replacement_preserves_owner_group_and_posix_access_acl() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        let fixture = SaveFixture::new();
        let path = fixture.dir.join("sample.pdf");
        let file = std::fs::File::open(&path).unwrap();
        let mut groups = [0_u32; 64];
        // SAFETY: the output array has capacity for the group count passed in.
        let count = unsafe { libc::getgroups(64, groups.as_mut_ptr()) };
        assert!(count >= 0);
        let metadata = file.metadata().unwrap();
        if let Some(group) = groups[..usize::try_from(count).unwrap()]
            .iter()
            .find(|group| **group != metadata.gid())
        {
            // SAFETY: this valid descriptor remains open, -1 keeps its owner.
            // Restricted user namespaces can hide supplementary group mappings.
            let _ = unsafe { libc::fchown(file.as_raw_fd(), u32::MAX, *group) };
        }
        // Linux POSIX ACL xattr v2: owner rw, named current user r, group r,
        // mask r, others none. Tags/layout follow linux/posix_acl_xattr.h.
        let mut acl = 2_u32.to_le_bytes().to_vec();
        for (tag, permission, id) in [
            (1_u16, 6_u16, u32::MAX),
            (2, 4, metadata.uid()),
            (4, 4, u32::MAX),
            (16, 4, u32::MAX),
            (32, 0, u32::MAX),
        ] {
            acl.extend_from_slice(&tag.to_le_bytes());
            acl.extend_from_slice(&permission.to_le_bytes());
            acl.extend_from_slice(&id.to_le_bytes());
        }
        let name = c"system.posix_acl_access";
        // SAFETY: the valid descriptor, C string and ACL byte buffer live through this call.
        assert_eq!(
            unsafe {
                libc::fsetxattr(
                    file.as_raw_fd(),
                    name.as_ptr(),
                    acl.as_ptr().cast(),
                    acl.len(),
                    0,
                )
            },
            0,
            "fixture ACL: {}",
            std::io::Error::last_os_error()
        );
        let before = file.metadata().unwrap();
        fixture.open(path.clone());
        fixture.save(empty_save()).unwrap();
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(
            (after.uid(), after.gid(), after.mode()),
            (before.uid(), before.gid(), before.mode())
        );
        let file = std::fs::File::open(path).unwrap();
        let mut saved_acl = vec![0; acl.len()];
        // SAFETY: the descriptor, C string and writable output buffer are valid for this call.
        let len = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                saved_acl.as_mut_ptr().cast(),
                saved_acl.len(),
            )
        };
        assert_eq!(
            len,
            isize::try_from(acl.len()).unwrap(),
            "saved ACL: {}",
            std::io::Error::last_os_error()
        );
        assert_eq!(saved_acl, acl);

        // A target without an ACL must not acquire the directory's default ACL.
        let directory = std::fs::File::open(&fixture.dir).unwrap();
        // SAFETY: the valid directory descriptor, name and ACL bytes outlive the call.
        assert_eq!(
            unsafe {
                libc::fsetxattr(
                    directory.as_raw_fd(),
                    c"system.posix_acl_default".as_ptr(),
                    acl.as_ptr().cast(),
                    acl.len(),
                    0,
                )
            },
            0
        );
        // SAFETY: this valid descriptor and ACL name outlive the call.
        assert_eq!(
            unsafe { libc::fremovexattr(file.as_raw_fd(), name.as_ptr()) },
            0
        );
        fixture.save(empty_save()).unwrap();
        let file = std::fs::File::open(fixture.dir.join("sample.pdf")).unwrap();
        // SAFETY: a null buffer and zero size query the ACL length on a valid descriptor.
        assert_eq!(
            unsafe { libc::fgetxattr(file.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ENODATA)
        );
        assert_eq!(fixture.names(), ["sample.pdf"]);
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
    #[cfg(target_os = "linux")]
    fn drop_shuts_down_marker_pdf_threads() {
        let wake = crate::worker::NoopWake::arc();
        let tile_wanted = Arc::new(Mutex::new(HashMap::new()));
        let thumb_wanted = Arc::new(Mutex::new(HashMap::new()));
        let worker = PdfWorker::spawn(wake, tile_wanted, thumb_wanted);
        // Reader is spawned from inside the tile thread; wait until both exist.
        let appear = Instant::now() + Duration::from_secs(2);
        loop {
            let names = thread_names();
            let tile = names.iter().any(|name| name == "marker-pdf");
            let reader = names.iter().any(|name| name == "marker-pdf-read");
            if tile && reader {
                break;
            }
            assert!(
                Instant::now() < appear,
                "pdf threads did not start: {names:?}"
            );
            thread::sleep(Duration::from_millis(5));
        }
        drop(worker);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let names = thread_names();
            let leftover: Vec<_> = names
                .iter()
                .filter(|name| name.as_str() == "marker-pdf" || name.as_str() == "marker-pdf-read")
                .cloned()
                .collect();
            if leftover.is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "marker-pdf threads still alive after drop: {leftover:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
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

        let wake = crate::worker::NoopWake::arc();
        let tile_wanted = Arc::new(Mutex::new(HashMap::new()));
        let thumb_wanted = Arc::new(Mutex::new(HashMap::new()));
        let worker = PdfWorker::spawn(wake, Arc::clone(&tile_wanted), thumb_wanted);
        let doc_id = DocId(3);
        worker.open(doc_id, path);
        let opened = wait_until(&worker, |replies| {
            replies
                .iter()
                .any(|reply| matches!(reply, PdfReply::Opened(_)))
        });
        assert!(
            opened
                .iter()
                .any(|reply| matches!(reply, PdfReply::Opened(doc) if doc.pages.len() == 4)),
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
                .entry(doc_id)
                .or_default()
                .insert(wanted_for(0, scale, 0, 0));
        }
        worker.glyphs(doc_id, 0);
        worker.search(doc_id, 1, "Hello".into());
        worker.tile(doc_id, 0, scale, 0, 0, false, 0);
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

        worker.search(doc_id, 2, "Hello".into());
        worker.save(doc_id, empty_save());
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

        worker.insert_page(doc_id, 0);
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
        worker.delete_page(doc_id, 4);
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
        worker.search(doc_id, 3, "Hello".into());
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
        let wake = crate::worker::NoopWake::arc();
        let tile_wanted = Arc::new(Mutex::new(HashMap::new()));
        let thumb_wanted = Arc::new(Mutex::new(HashMap::new()));
        let worker = PdfWorker::spawn(wake, Arc::clone(&tile_wanted), thumb_wanted);
        worker.open(DocId(1), path);
        let opened = wait_until(&worker, |replies| {
            replies
                .iter()
                .any(|reply| matches!(reply, PdfReply::Opened(_)))
        });
        let pages = opened.iter().find_map(|reply| match reply {
            PdfReply::Opened(doc) => Some(doc.pages.len()),
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
            let set = guard.entry(DocId(1)).or_default();
            for &(page, col, row) in &tiles {
                set.insert(wanted_for(page, scale, col, row));
            }
        }
        let t0 = Instant::now();
        worker.search(DocId(1), 1, "commit".into());
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
                            worker.tile(DocId(1), tiles[0].0, scale, 0, 0, false, 0);
                            for &(page, col, row) in tiles.iter().skip(1) {
                                worker.tile(DocId(1), page, scale, col, row, false, page as u32);
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
        let replies = wait_including_failure(worker, pred);
        assert_no_fail(&replies);
        replies
    }

    fn wait_including_failure(
        worker: &PdfWorker,
        pred: impl Fn(&[PdfReply]) -> bool,
    ) -> Vec<PdfReply> {
        let start = Instant::now();
        let mut all = Vec::new();
        while start.elapsed() < Duration::from_secs(20) {
            all.extend(worker.poll());
            if pred(&all) {
                return all;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for replies ({})", all.len());
    }
}
