//! Single-instance handoff for OS / file-manager opens.
//!
//! The first Marker process binds a Unix socket under `$XDG_RUNTIME_DIR`. Later
//! launches connect, forward PDF paths, and exit so documents open as tabs in
//! the existing window. `--new-window` skips handoff and starts a separate UI.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Parsed CLI flags and PDF paths (non-PDF args are ignored).
#[derive(Debug, Clone)]
pub struct LaunchArgs {
    pub new_window: bool,
    pub paths: Vec<PathBuf>,
}

impl LaunchArgs {
    pub fn from_env() -> Self {
        Self::from_args(std::env::args().skip(1))
    }

    fn from_args(args: impl IntoIterator<Item = String>) -> Self {
        let mut new_window = false;
        let mut paths = Vec::new();
        for arg in args {
            if arg == "--new-window" || arg == "-n" {
                new_window = true;
                continue;
            }
            if arg.starts_with('-') {
                // Unknown flags: ignore so future options do not break opens.
                continue;
            }
            let path = PathBuf::from(&arg);
            if is_pdf(&path) {
                let resolved = path
                    .canonicalize()
                    .unwrap_or_else(|_| absolute_fallback(&path));
                paths.push(resolved);
            }
        }
        Self { new_window, paths }
    }
}

/// Result of single-instance negotiation before the UI starts.
pub enum Boot {
    /// Paths were delivered to the running instance; this process should exit.
    HandedOff,
    /// This process owns the UI. `inbox` receives later external opens.
    Run {
        paths: Vec<PathBuf>,
        inbox: Option<IpcInbox>,
    },
}

/// Incoming open requests from secondary Marker processes.
pub struct IpcInbox {
    rx: Receiver<Vec<PathBuf>>,
    /// Kept so the socket file is removed when the primary exits.
    _guard: SocketGuard,
    wake: Arc<Mutex<Option<egui::Context>>>,
}

impl IpcInbox {
    pub fn bind_ctx(&self, ctx: egui::Context) {
        if let Ok(mut slot) = self.wake.lock() {
            *slot = Some(ctx);
        }
    }

    pub fn poll(&self) -> Vec<Vec<PathBuf>> {
        let mut batches = Vec::new();
        while let Ok(paths) = self.rx.try_recv() {
            batches.push(paths);
        }
        batches
    }
}

struct SocketGuard {
    path: PathBuf,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Negotiate single-instance behavior for this launch.
pub fn boot(args: LaunchArgs) -> Boot {
    #[cfg(unix)]
    {
        boot_unix(args)
    }
    #[cfg(not(unix))]
    {
        Boot::Run {
            paths: args.paths,
            inbox: None,
        }
    }
}

#[cfg(unix)]
fn boot_unix(args: LaunchArgs) -> Boot {
    if args.new_window {
        return Boot::Run {
            paths: args.paths,
            inbox: None,
        };
    }

    let Some(socket_path) = socket_path() else {
        return Boot::Run {
            paths: args.paths,
            inbox: None,
        };
    };

    if try_handoff(&socket_path, &args.paths) {
        return Boot::HandedOff;
    }

    match bind_primary(&socket_path) {
        Some(inbox) => Boot::Run {
            paths: args.paths,
            inbox: Some(inbox),
        },
        None => {
            // Another process won the race; retry handoff once.
            thread::sleep(Duration::from_millis(50));
            if try_handoff(&socket_path, &args.paths) {
                Boot::HandedOff
            } else {
                Boot::Run {
                    paths: args.paths,
                    inbox: None,
                }
            }
        }
    }
}

#[cfg(unix)]
fn try_handoff(socket_path: &Path, paths: &[PathBuf]) -> bool {
    use std::os::unix::net::UnixStream;

    let Ok(mut stream) = UnixStream::connect(socket_path) else {
        return false;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));

    let mut payload = String::from("open\n");
    for path in paths {
        payload.push_str(&path.to_string_lossy());
        payload.push('\n');
    }
    payload.push('\n');

    if stream.write_all(payload.as_bytes()).is_err() {
        return false;
    }
    let _ = stream.flush();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(_) => line.trim() == "ok",
        Err(_) => false,
    }
}

#[cfg(unix)]
fn bind_primary(socket_path: &Path) -> Option<IpcInbox> {
    use std::os::unix::net::UnixListener;

    if let Some(parent) = socket_path.parent() {
        let _ = fs::create_dir_all(parent);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }

    if !clear_stale_socket(socket_path) {
        return None;
    }

    let listener = UnixListener::bind(socket_path).ok()?;
    let _ = listener.set_nonblocking(false);
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600));
    }

    let (tx, rx) = mpsc::channel();
    let wake = Arc::new(Mutex::new(None));
    let wake_thread = Arc::clone(&wake);
    let listen_path = socket_path.to_path_buf();

    thread::Builder::new()
        .name("marker-ipc".into())
        .spawn(move || accept_loop(listener, tx, wake_thread))
        .ok()?;

    Some(IpcInbox {
        rx,
        _guard: SocketGuard { path: listen_path },
        wake,
    })
}

#[cfg(unix)]
fn accept_loop(
    listener: std::os::unix::net::UnixListener,
    tx: Sender<Vec<PathBuf>>,
    wake: Arc<Mutex<Option<egui::Context>>>,
) {
    loop {
        let Ok((stream, _)) = listener.accept() else {
            thread::sleep(Duration::from_millis(20));
            continue;
        };
        if let Some(paths) = handle_client(stream) {
            let _ = tx.send(paths);
            if let Ok(slot) = wake.lock() {
                if let Some(ctx) = slot.as_ref() {
                    ctx.request_repaint();
                }
            }
        }
    }
}

#[cfg(unix)]
fn handle_client(mut stream: std::os::unix::net::UnixStream) -> Option<Vec<PathBuf>> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    break;
                }
                lines.push(trimmed.to_string());
            }
            Err(_) => return None,
        }
    }

    let mut paths = Vec::new();
    let mut iter = lines.into_iter();
    match iter.next().as_deref() {
        Some("open") | Some("focus") => {
            for entry in iter {
                let path = PathBuf::from(entry);
                if is_pdf(&path) {
                    paths.push(path);
                }
            }
        }
        _ => return None,
    }

    let _ = stream.write_all(b"ok\n");
    let _ = stream.flush();
    Some(paths)
}

fn socket_path() -> Option<PathBuf> {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        if !runtime.is_empty() {
            return Some(PathBuf::from(runtime).join("marker").join("instance.sock"));
        }
    }
    let uid = runtime_uid();
    let dir = std::env::temp_dir().join(format!("marker-{uid}"));
    remove_legacy_temp_socket(uid, &dir);
    Some(dir.join("instance.sock"))
}

#[cfg(unix)]
fn clear_stale_socket(socket_path: &Path) -> bool {
    use std::io::ErrorKind;
    use std::os::unix::net::UnixStream;

    if !socket_path.exists() {
        return true;
    }
    match UnixStream::connect(socket_path) {
        Ok(_) => false,
        Err(err) if err.kind() == ErrorKind::ConnectionRefused => {
            let _ = fs::remove_file(socket_path);
            true
        }
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn clear_stale_socket(_socket_path: &Path) -> bool {
    true
}

fn remove_legacy_temp_socket(uid: u32, new_dir: &Path) {
    let legacy = std::env::temp_dir().join(format!("marker-{uid}.sock"));
    if legacy != new_dir.join("instance.sock") {
        let _ = fs::remove_file(legacy);
    }
}

fn runtime_uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: getuid is always available on Unix.
        return unsafe { libc::getuid() };
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

fn absolute_fallback(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Spawn a separate Marker window for `path` (bypasses single-instance).
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixListener;

    #[test]
    fn launch_args_parses_flags_and_pdf_paths() {
        let args = LaunchArgs::from_args([
            "--new-window".into(),
            "-n".into(),
            "notes.txt".into(),
            "doc.PDF".into(),
            "--unknown".into(),
        ]);
        assert!(args.new_window);
        assert_eq!(args.paths.len(), 1);
        assert_eq!(args.paths[0].file_name().unwrap(), "doc.PDF");
    }

    #[test]
    fn launch_args_ignores_non_pdf_paths() {
        let args = LaunchArgs::from_args(["readme.md".into(), "paper.pdf".into()]);
        assert!(!args.new_window);
        assert_eq!(args.paths.len(), 1);
        assert!(args.paths[0].ends_with("paper.pdf"));
    }

    #[test]
    fn handle_client_open_parses_pdf_paths() {
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        client
            .write_all(b"open\n/a.pdf\n/b.txt\n\n")
            .unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let paths = handle_client(server).expect("valid open request");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from("/a.pdf"));
    }

    #[test]
    fn handle_client_rejects_unknown_command() {
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        client.write_all(b"quit\n\n").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(handle_client(server).is_none());
    }

    #[test]
    fn clear_stale_socket_removes_only_refused() {
        // pdf::engine tests remove `marker-test-{pid}` while this runs.
        let dir = std::env::temp_dir().join(format!("marker-instance-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let socket_path = dir.join("instance.sock");

        assert!(clear_stale_socket(&socket_path));

        let listener = UnixListener::bind(&socket_path).unwrap();
        assert!(!clear_stale_socket(&socket_path));
        // Other tests spawn processes in this process. fork copies the
        // listening fd until exec, so drop() alone can leave connect()
        // succeeding. shutdown is shared by those duplicated fds.
        // SAFETY: `listener` is an open Unix socket. SHUT_RD only stops
        // accepts on that socket and does not close the fd.
        let rc = unsafe { libc::shutdown(listener.as_raw_fd(), libc::SHUT_RD) };
        assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
        drop(listener);
        assert!(socket_path.exists());
        assert!(clear_stale_socket(&socket_path));
        assert!(!socket_path.exists());

        let _ = fs::remove_dir_all(&dir);
    }
}

pub fn spawn_new_window(path: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .arg("--new-window")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
