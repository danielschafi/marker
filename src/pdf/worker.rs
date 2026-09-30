use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

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
    pub fn spawn(ctx: egui::Context) -> Self {
        let (job_tx, job_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let loop_tx = job_tx.clone();
        thread::Builder::new()
            .name("marker-pdf".into())
            .spawn(move || worker_loop(ctx, job_rx, loop_tx, reply_tx))
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

    pub fn tile(&self, gen: u64, page: usize, scale: f32, col: i32, row: i32) {
        let _ = self.jobs.send(PdfJob::Tile {
            gen,
            page,
            scale,
            col,
            row,
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

fn job_rank(job: &PdfJob) -> u8 {
    match job {
        PdfJob::Shutdown
        | PdfJob::Open { .. }
        | PdfJob::Close { .. }
        | PdfJob::Save { .. }
        | PdfJob::InsertPage { .. }
        | PdfJob::InsertPageAt { .. }
        | PdfJob::DeletePage { .. } => 0,
        PdfJob::Tile { .. } => 1,
        PdfJob::Crop { .. } | PdfJob::Search { .. } => 2,
        PdfJob::Glyphs { .. } => 3,
    }
}

fn reply(ctx: &egui::Context, replies: &Sender<PdfReply>, msg: PdfReply) {
    let _ = replies.send(msg);
    ctx.request_repaint();
}

fn worker_loop(
    ctx: egui::Context,
    jobs: Receiver<PdfJob>,
    jobs_tx: Sender<PdfJob>,
    replies: Sender<PdfReply>,
) {
    let mut engines: HashMap<u64, DocumentEngine> = HashMap::new();
    let mut latest_search: HashMap<u64, u64> = HashMap::new();
    let mut queued = VecDeque::new();
    loop {
        if queued.is_empty() {
            match jobs.recv() {
                Ok(job) => queued.push_back(job),
                Err(_) => break,
            }
        }
        while let Ok(job) = jobs.try_recv() {
            queued.push_back(job);
        }
        let idx = queued
            .iter()
            .enumerate()
            .min_by_key(|(_, job)| job_rank(job))
            .map(|(index, _)| index)
            .unwrap_or(0);
        let Some(job) = queued.remove(idx) else {
            continue;
        };
        match job {
            PdfJob::Shutdown => break,
            PdfJob::Open { gen, path } => match DocumentEngine::open(&path) {
                Ok(loaded) => {
                    let opened = OpenedDoc {
                        gen,
                        path,
                        pages: loaded.engine.pages().to_vec(),
                        outline: loaded.engine.outline().to_vec(),
                        annotations: loaded.annotations,
                    };
                    engines.insert(gen, loaded.engine);
                    reply(&ctx, &replies, PdfReply::Opened(Ok(opened)));
                }
                Err(err) => {
                    reply(
                        &ctx,
                        &replies,
                        PdfReply::Failed {
                            gen: Some(gen),
                            message: err,
                        },
                    );
                }
            },
            PdfJob::Close { gen } => {
                engines.remove(&gen);
            }
            PdfJob::Tile {
                gen,
                page,
                scale,
                col,
                row,
            } => {
                let Some(engine) = engines.get_mut(&gen) else {
                    continue;
                };
                match engine.render_tile(page, scale, col, row) {
                    Ok(Some(tile)) => {
                        reply(&ctx, &replies, PdfReply::Tile { gen, tile });
                    }
                    Ok(None) => {
                        reply(
                            &ctx,
                            &replies,
                            PdfReply::TileMiss {
                                gen,
                                page,
                                scale,
                                col,
                                row,
                            },
                        );
                    }
                    Err(err) => {
                        reply(
                            &ctx,
                            &replies,
                            PdfReply::Failed {
                                gen: Some(gen),
                                message: err,
                            },
                        );
                    }
                }
            }
            PdfJob::Glyphs { gen, page } => {
                let Some(engine) = engines.get(&gen) else {
                    continue;
                };
                match engine.glyphs(page) {
                    Ok(glyphs) => {
                        reply(&ctx, &replies, PdfReply::Glyphs { gen, page, glyphs });
                    }
                    Err(err) => {
                        reply(
                            &ctx,
                            &replies,
                            PdfReply::Failed {
                                gen: Some(gen),
                                message: err,
                            },
                        );
                    }
                }
            }
            PdfJob::Crop {
                gen,
                seq,
                page,
                rect,
                dpi,
            } => {
                let Some(engine) = engines.get_mut(&gen) else {
                    reply(
                        &ctx,
                        &replies,
                        PdfReply::Crop {
                            gen,
                            seq,
                            result: Err("No document is open.".into()),
                        },
                    );
                    continue;
                };
                let result = engine.render_crop_png(page, rect, dpi);
                reply(&ctx, &replies, PdfReply::Crop { gen, seq, result });
            }
            PdfJob::Search {
                gen,
                seq,
                needle,
                from,
            } => {
                if from == 0 {
                    latest_search.insert(gen, seq);
                }
                if latest_search.get(&gen) != Some(&seq) {
                    continue;
                }
                let Some(engine) = engines.get(&gen) else {
                    continue;
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
                    reply(
                        &ctx,
                        &replies,
                        PdfReply::Failed {
                            gen: Some(gen),
                            message,
                        },
                    );
                    continue;
                }
                let done = end >= page_count;
                reply(
                    &ctx,
                    &replies,
                    PdfReply::Search {
                        gen,
                        seq,
                        hits,
                        done,
                    },
                );
                if !done {
                    let _ = jobs_tx.send(PdfJob::Search {
                        gen,
                        seq,
                        needle,
                        from: end,
                    });
                }
            }
            PdfJob::Save { gen, snapshot } => {
                let Some(engine) = engines.get_mut(&gen) else {
                    reply(
                        &ctx,
                        &replies,
                        PdfReply::Saved {
                            gen,
                            result: Err("No document is open.".into()),
                        },
                    );
                    continue;
                };
                let result = engine.save(&snapshot);
                reply(&ctx, &replies, PdfReply::Saved { gen, result });
            }
            PdfJob::InsertPage { gen, after } => {
                let Some(engine) = engines.get_mut(&gen) else {
                    reply(
                        &ctx,
                        &replies,
                        PdfReply::Failed {
                            gen: Some(gen),
                            message: "No document is open.".into(),
                        },
                    );
                    continue;
                };
                match engine.insert_blank_page(after) {
                    Ok((index, pages)) => {
                        reply(&ctx, &replies, PdfReply::PageInserted { gen, index, pages });
                    }
                    Err(message) => {
                        reply(
                            &ctx,
                            &replies,
                            PdfReply::Failed {
                                gen: Some(gen),
                                message,
                            },
                        );
                    }
                }
            }
            PdfJob::InsertPageAt { gen, at } => {
                let Some(engine) = engines.get_mut(&gen) else {
                    reply(
                        &ctx,
                        &replies,
                        PdfReply::Failed {
                            gen: Some(gen),
                            message: "No document is open.".into(),
                        },
                    );
                    continue;
                };
                match engine.insert_blank_page_at(at) {
                    Ok((index, pages)) => {
                        reply(&ctx, &replies, PdfReply::PageInserted { gen, index, pages });
                    }
                    Err(message) => {
                        reply(
                            &ctx,
                            &replies,
                            PdfReply::Failed {
                                gen: Some(gen),
                                message,
                            },
                        );
                    }
                }
            }
            PdfJob::DeletePage { gen, index } => {
                let Some(engine) = engines.get_mut(&gen) else {
                    reply(
                        &ctx,
                        &replies,
                        PdfReply::Failed {
                            gen: Some(gen),
                            message: "No document is open.".into(),
                        },
                    );
                    continue;
                };
                match engine.delete_page_at(index) {
                    Ok(pages) => {
                        reply(&ctx, &replies, PdfReply::PageDeleted { gen, index, pages });
                    }
                    Err(message) => {
                        reply(
                            &ctx,
                            &replies,
                            PdfReply::Failed {
                                gen: Some(gen),
                                message,
                            },
                        );
                    }
                }
            }
        }
    }
}
