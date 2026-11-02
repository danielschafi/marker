//! Shared UI ↔ background-worker channel.
//!
//! [`Channel`] owns the job sender and reply receiver used by `PdfWorker` and
//! `MathWorker`. Each worker still supplies its own loop body; this module only
//! covers spawn / send / poll / shutdown-on-drop and the worker-side reply
//! helper that wakes the UI through [`Wake`].

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

/// Ask the UI to schedule another frame after a worker reply (or IPC arrival).
///
/// Production uses [`EguiWake`]; tests and headless callers use [`NoopWake`].
/// Workers never see `egui::Context` — only this seam.
pub trait Wake: Send + Sync {
    fn wake(&self);
}

/// Production adapter: `egui::Context::request_repaint`.
#[derive(Clone)]
pub struct EguiWake(pub egui::Context);

impl EguiWake {
    pub fn arc(ctx: egui::Context) -> Arc<dyn Wake> {
        Arc::new(Self(ctx))
    }
}

impl Wake for EguiWake {
    fn wake(&self) {
        self.0.request_repaint();
    }
}

/// Headless / test adapter: no-op.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopWake;

impl NoopWake {
    pub fn arc() -> Arc<dyn Wake> {
        Arc::new(Self)
    }
}

impl Wake for NoopWake {
    fn wake(&self) {}
}

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
        wake: Arc<dyn Wake>,
        loop_fn: impl FnOnce(Arc<dyn Wake>, Receiver<Job>, Sender<Reply>) + Send + 'static,
    ) -> Self {
        let name = name.into();
        let (job_tx, job_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let thread_name = name.clone();
        thread::Builder::new()
            .name(name)
            .spawn(move || loop_fn(wake, job_rx, reply_tx))
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

/// Worker-side send that also wakes the UI through [`Wake`].
pub fn reply<Reply>(wake: &dyn Wake, replies: &Sender<Reply>, msg: Reply) {
    let _ = replies.send(msg);
    wake.wake();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountWake(AtomicUsize);

    impl Wake for CountWake {
        fn wake(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn reply_sends_and_wakes() {
        let (tx, rx) = mpsc::channel();
        let wake = CountWake(AtomicUsize::new(0));
        reply(&wake, &tx, 42u32);
        assert_eq!(rx.try_recv(), Ok(42));
        assert_eq!(wake.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn noop_wake_is_silent() {
        NoopWake.wake();
    }
}
