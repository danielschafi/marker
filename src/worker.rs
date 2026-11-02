//! Shared UI ↔ background-worker channel.
//!
//! [`Channel`] owns the job sender and reply receiver used by `PdfWorker`,
//! `MathWorker`, and `AssistantWorker`. Each worker still supplies its own
//! loop body; this module only covers spawn / send / poll / shutdown-on-drop
//! and the worker-side reply helper that wakes egui.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

/// Job enums that carry an explicit shutdown variant for [`Channel`] drop.
pub trait ShutdownJob {
    fn shutdown() -> Self;
}

/// One job→reply worker channel with shutdown-on-drop.
pub struct Channel<Job, Reply> {
    jobs: Option<Sender<Job>>,
    replies: Receiver<Reply>,
    /// Prebuilt shutdown message so [`Drop`] needs no trait bound (E0367).
    shutdown: Option<Job>,
}

impl<Job, Reply> Channel<Job, Reply> {
    /// Queue a job for the worker thread. Ignores send failures after shutdown.
    pub fn send(&self, job: Job) {
        if let Some(tx) = self.jobs.as_ref() {
            let _ = tx.send(job);
        }
    }

    /// Drain every pending reply into a fresh `Vec`.
    pub fn poll(&self) -> Vec<Reply> {
        let mut out = Vec::new();
        while let Ok(reply) = self.replies.try_recv() {
            out.push(reply);
        }
        out
    }
}

impl<Job, Reply> Channel<Job, Reply>
where
    Job: ShutdownJob + Send + 'static,
    Reply: Send + 'static,
{
    /// Spawn a named worker thread that runs `loop_fn`.
    pub fn spawn(
        name: impl Into<String>,
        ctx: egui::Context,
        loop_fn: impl FnOnce(egui::Context, Receiver<Job>, Sender<Reply>) + Send + 'static,
    ) -> Self {
        let name = name.into();
        let (job_tx, job_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let thread_name = name.clone();
        thread::Builder::new()
            .name(name)
            .spawn(move || loop_fn(ctx, job_rx, reply_tx))
            .unwrap_or_else(|err| panic!("{thread_name} thread: {err}"));
        Self {
            jobs: Some(job_tx),
            replies: reply_rx,
            shutdown: Some(Job::shutdown()),
        }
    }
}

impl<Job, Reply> Drop for Channel<Job, Reply> {
    fn drop(&mut self) {
        if let (Some(tx), Some(msg)) = (self.jobs.take(), self.shutdown.take()) {
            let _ = tx.send(msg);
        }
    }
}

/// Worker-side send that also asks egui to repaint.
pub fn reply<Reply>(ctx: &egui::Context, replies: &Sender<Reply>, msg: Reply) {
    let _ = replies.send(msg);
    ctx.request_repaint();
}
