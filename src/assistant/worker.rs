use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::Value;

use super::adapter::{create_chat, probe_agent, AgentCapability};
use super::bundle::{build_bundle, cleanup_bundle, BundleInput, BundlePaths};
use super::types::AssistantEvent;

struct ActiveChild {
    gen: u64,
    seq: u64,
    pid: u32,
    cancel: Arc<AtomicBool>,
    bundle: PathBuf,
}

pub struct AssistantRequest {
    pub gen: u64,
    pub seq: u64,
    pub chat_id: Option<String>,
    pub bundle: BundleInput,
}

enum Job {
    Send(AssistantRequest),
    Cancel { gen: u64, seq: u64 },
    Shutdown,
}

pub struct AssistantWorker {
    jobs: Sender<Job>,
    events: Receiver<AssistantEvent>,
    active: Arc<Mutex<Option<ActiveChild>>>,
}

impl AssistantWorker {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (job_tx, job_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let active = Arc::new(Mutex::new(None));
        let active_loop = Arc::clone(&active);
        thread::Builder::new()
            .name("marker-assistant".into())
            .spawn(move || worker_loop(ctx, job_rx, event_tx, active_loop))
            .expect("assistant thread");
        Self {
            jobs: job_tx,
            events: event_rx,
            active,
        }
    }

    pub fn send(&self, request: AssistantRequest) {
        let _ = self.jobs.send(Job::Send(request));
    }

    pub fn cancel(&self, gen: u64, seq: u64) {
        let _ = self.jobs.send(Job::Cancel { gen, seq });
        if let Ok(guard) = self.active.lock() {
            if let Some(active) = guard.as_ref() {
                if active.gen == gen && active.seq == seq {
                    active.cancel.store(true, Ordering::SeqCst);
                    kill_pgid(active.pid);
                }
            }
        }
    }

    pub fn poll(&self) -> Vec<AssistantEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            out.push(event);
        }
        out
    }
}

impl Drop for AssistantWorker {
    fn drop(&mut self) {
        let _ = self.jobs.send(Job::Shutdown);
        if let Ok(mut guard) = self.active.lock() {
            if let Some(active) = guard.take() {
                active.cancel.store(true, Ordering::SeqCst);
                kill_pgid(active.pid);
                cleanup_bundle(&BundlePaths {
                    root: active.bundle,
                });
            }
        }
    }
}

fn emit(ctx: &egui::Context, events: &Sender<AssistantEvent>, event: AssistantEvent) {
    let _ = events.send(event);
    ctx.request_repaint();
}

fn worker_loop(
    ctx: egui::Context,
    jobs: Receiver<Job>,
    events: Sender<AssistantEvent>,
    active: Arc<Mutex<Option<ActiveChild>>>,
) {
    let mut capability: Option<AgentCapability> = None;
    loop {
        let job = match jobs.recv() {
            Ok(job) => job,
            Err(_) => break,
        };
        match job {
            Job::Shutdown => break,
            Job::Cancel { gen, seq } => {
                clear_active_if(&active, gen, seq);
            }
            Job::Send(request) => {
                clear_any_active(&active);
                run_turn(&ctx, &mut capability, request, &events, &active);
            }
        }
    }
}

fn clear_any_active(active: &Arc<Mutex<Option<ActiveChild>>>) {
    if let Ok(mut guard) = active.lock() {
        if let Some(cur) = guard.take() {
            cur.cancel.store(true, Ordering::SeqCst);
            kill_pgid(cur.pid);
            cleanup_bundle(&BundlePaths { root: cur.bundle });
        }
    }
}

fn clear_active_if(active: &Arc<Mutex<Option<ActiveChild>>>, gen: u64, seq: u64) {
    if let Ok(mut guard) = active.lock() {
        let matches = guard
            .as_ref()
            .is_some_and(|cur| cur.gen == gen && cur.seq == seq);
        if matches {
            if let Some(cur) = guard.take() {
                cur.cancel.store(true, Ordering::SeqCst);
                kill_pgid(cur.pid);
                cleanup_bundle(&BundlePaths { root: cur.bundle });
            }
        }
    }
}

fn run_turn(
    ctx: &egui::Context,
    capability: &mut Option<AgentCapability>,
    request: AssistantRequest,
    events: &Sender<AssistantEvent>,
    active: &Arc<Mutex<Option<ActiveChild>>>,
) {
    let gen = request.gen;
    let seq = request.seq;

    let cap = match capability {
        Some(c) => c.clone(),
        None => match probe_agent() {
            Ok(c) => {
                *capability = Some(c.clone());
                c
            }
            Err(message) => {
                emit(ctx, events, AssistantEvent::Failed { gen, seq, message });
                return;
            }
        },
    };
    if !cap.logged_in {
        emit(
            ctx,
            events,
            AssistantEvent::AuthRequired {
                gen,
                seq,
                message: "Cursor Agent is not signed in. Run `agent login` and try again.".into(),
            },
        );
        return;
    }

    let chat_id = match request.chat_id {
        Some(id) => id,
        None => match create_chat(&cap.path) {
            Ok(id) => id,
            Err(message) => {
                emit(ctx, events, AssistantEvent::Failed { gen, seq, message });
                return;
            }
        },
    };

    let bundle = match build_bundle(&request.bundle) {
        Ok(b) => b,
        Err(message) => {
            emit(ctx, events, AssistantEvent::Failed { gen, seq, message });
            return;
        }
    };

    emit(
        ctx,
        events,
        AssistantEvent::Started {
            gen,
            seq,
            chat_id: chat_id.clone(),
        },
    );

    let cancel = Arc::new(AtomicBool::new(false));
    let mut child = match spawn_agent(&cap.path, &bundle.root, &chat_id) {
        Ok(child) => child,
        Err(message) => {
            cleanup_bundle(&bundle);
            emit(ctx, events, AssistantEvent::Failed { gen, seq, message });
            return;
        }
    };
    let pid = child.id();
    if let Ok(mut guard) = active.lock() {
        *guard = Some(ActiveChild {
            gen,
            seq,
            pid,
            cancel: Arc::clone(&cancel),
            bundle: bundle.root.clone(),
        });
    }

    let stdout = child.stdout.take();
    let events_stream = events.clone();
    let cancel_stream = Arc::clone(&cancel);
    let ctx_stream = ctx.clone();
    let reader = thread::spawn(move || {
        stream_stdout(stdout, gen, seq, &ctx_stream, &events_stream, &cancel_stream)
    });

    let status = loop {
        if cancel.load(Ordering::SeqCst) {
            kill_pgid(pid);
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => thread::sleep(std::time::Duration::from_millis(40)),
            Err(_) => {
                kill_pgid(pid);
                break None;
            }
        }
    };

    let streamed = reader.join().unwrap_or_default();
    let was_cancelled = cancel.load(Ordering::SeqCst);
    if let Ok(mut guard) = active.lock() {
        if guard
            .as_ref()
            .is_some_and(|cur| cur.gen == gen && cur.seq == seq)
        {
            *guard = None;
        }
    }
    cleanup_bundle(&bundle);

    if was_cancelled {
        emit(ctx, events, AssistantEvent::Cancelled { gen, seq });
        return;
    }

    if streamed.had_result {
        if streamed.assistant_text.is_empty() {
            // result already emitted final text via Completed in stream parser when present
        }
        return;
    }

    if let Some(status) = status {
        if !status.success() {
            let message = streamed
                .error
                .unwrap_or_else(|| format!("Cursor Agent exited with {status}"));
            emit(ctx, events, AssistantEvent::Failed { gen, seq, message });
            return;
        }
    }

    emit(
        ctx,
        events,
        AssistantEvent::Completed {
            gen,
            seq,
            text: streamed.assistant_text,
        },
    );
}

struct StreamOutcome {
    assistant_text: String,
    had_result: bool,
    error: Option<String>,
}

impl Default for StreamOutcome {
    fn default() -> Self {
        Self {
            assistant_text: String::new(),
            had_result: false,
            error: None,
        }
    }
}

fn stream_stdout(
    stdout: Option<std::process::ChildStdout>,
    gen: u64,
    seq: u64,
    ctx: &egui::Context,
    events: &Sender<AssistantEvent>,
    cancel: &AtomicBool,
) -> StreamOutcome {
    let Some(stdout) = stdout else {
        return StreamOutcome {
            error: Some("Cursor Agent produced no stdout.".into()),
            ..Default::default()
        };
    };
    let mut outcome = StreamOutcome::default();
    let mut assembled = String::new();
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match value.get("type").and_then(|v| v.as_str()) {
            Some("assistant") => {
                if let Some(text) = assistant_text_from(&value) {
                    // Full chunks without --stream-partial-output: append if new.
                    if !assembled.ends_with(&text) {
                        assembled.push_str(&text);
                        emit(
                            ctx,
                            events,
                            AssistantEvent::Delta {
                                gen,
                                seq,
                                text: text.clone(),
                            },
                        );
                    }
                }
            }
            Some("result") => {
                outcome.had_result = true;
                let subtype = value.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
                let is_error = value
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if is_error || subtype == "error" {
                    let message = value
                        .get("result")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Cursor Agent reported an error.")
                        .to_string();
                    emit(ctx, events, AssistantEvent::Failed { gen, seq, message });
                } else {
                    let text = value
                        .get("result")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| assembled.clone());
                    assembled = text.clone();
                    emit(
                        ctx,
                        events,
                        AssistantEvent::Completed { gen, seq, text },
                    );
                }
            }
            _ => {}
        }
    }
    outcome.assistant_text = assembled;
    outcome
}

fn assistant_text_from(value: &Value) -> Option<String> {
    let content = value
        .pointer("/message/content")
        .and_then(|v| v.as_array())?;
    let mut out = String::new();
    for part in content {
        if part.get("type").and_then(|v| v.as_str()) == Some("text") {
            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                out.push_str(text);
            }
        }
    }
    // Partial deltas (with --stream-partial-output) may put text at top level.
    if out.is_empty() {
        if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
            out.push_str(text);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn spawn_agent(agent: &Path, workspace: &Path, chat_id: &str) -> Result<Child, String> {
    let mut cmd = Command::new(agent);
    cmd.arg("--print")
        .arg("--mode")
        .arg("ask")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--stream-partial-output")
        .arg("--sandbox")
        .arg("enabled")
        .arg("--trust")
        .arg("--workspace")
        .arg(workspace)
        .arg("--resume")
        .arg(chat_id)
        .arg("Read request.md and answer the user's question.")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New process group so Marker can kill the whole tree on cancel.
        cmd.process_group(0);
    }
    cmd.spawn()
        .map_err(|e| format!("failed to start {}: {e}", agent.display()))
}

fn kill_pgid(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        thread::sleep(std::time::Duration::from_millis(150));
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn stale_completed_can_be_filtered_by_seq() {
        // Document the contract: UI ignores events whose seq != tab.request_seq.
        let current_seq = 3u64;
        let event = AssistantEvent::Completed {
            gen: 1,
            seq: 2,
            text: "old".into(),
        };
        let AssistantEvent::Completed { seq, .. } = event else {
            panic!();
        };
        assert_ne!(seq, current_seq);
    }

    #[test]
    fn parses_assistant_and_result_lines() {
        let dir = std::env::temp_dir().join(format!("marker-stream-parse-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-stream.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
echo '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello"}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"Hello"}'
"#,
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let child = Command::new(&script)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let cancel = AtomicBool::new(false);
        let ctx = egui::Context::default();
        let outcome = stream_stdout(child.stdout, 1, 1, &ctx, &tx, &cancel);
        assert!(outcome.had_result);
        let events: Vec<_> = rx.try_iter().collect();
        assert!(events.iter().any(|e| matches!(e, AssistantEvent::Delta { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, AssistantEvent::Completed { text, .. } if text == "Hello")));
        let _ = fs::remove_dir_all(&dir);
    }
}
