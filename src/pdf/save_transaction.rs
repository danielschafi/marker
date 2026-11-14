//! Staged PDF writes: the target changes only after a complete, synchronized save.
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::engine::Error;

static NEXT_SAVE: AtomicU64 = AtomicU64::new(0);

/// Run a write against an exclusively created sibling, then replace the target.
/// Cleanup errors are returned alongside the original failure.
///
/// # Errors
/// Returns target resolution/access-metadata errors, exclusive creation or lock
/// failures, callback errors, and synchronization or replacement failures.
/// Cleanup failures retain both the original save error and the removal error.
pub(super) fn write<T>(
    target: &Path,
    write_candidate: impl FnOnce(&Path, &mut File) -> Result<T, Error>,
) -> Result<T, Error> {
    let target = resolve_target(target)?;
    let access = ExistingAccess::read(&target)?;
    let mut candidate = Candidate::new(&target)?;
    let result = (|| {
        #[cfg(unix)]
        lock_file(&candidate.file)?;
        #[cfg(test)]
        if let Err(error) = fail_at(Failure::Write) {
            use std::io::Write;
            candidate.file.write_all(b"partial PDF write")?;
            return Err(error.into());
        }
        let value = write_candidate(&candidate.path, &mut candidate.file)?;
        #[cfg(test)]
        pause_save(&candidate.path)?;
        if let Some(access) = access {
            access.apply(&candidate.file)?;
        }
        #[cfg(test)]
        fail_at(Failure::Sync)?;
        #[cfg(test)]
        fail_at(Failure::Cleanup)?;
        candidate.file.sync_all()?;
        #[cfg(test)]
        fail_at(Failure::Replace)?;
        std::fs::rename(&candidate.path, &target)?;
        candidate.pending = false;
        Ok(value)
    })();
    match (result, candidate.cleanup()) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(source)) => Err(Error::TemporarySaveCleanup { source }),
        (Err(save), Err(cleanup)) => Err(Error::SaveAndCleanup {
            save: Box::new(save),
            cleanup,
        }),
    }
}

/// Access metadata belongs to the old target, rather than to the containing
/// directory. Restore ownership and ACL before widening the private file mode.
struct ExistingAccess {
    permissions: std::fs::Permissions,
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
    #[cfg(target_os = "linux")]
    acl: Option<Vec<u8>>,
}

impl ExistingAccess {
    fn read(target: &Path) -> Result<Option<Self>, Error> {
        let original = match File::open(target) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata = original.metadata()?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Some(Self {
            permissions: metadata.permissions(),
            #[cfg(unix)]
            uid: metadata.uid(),
            #[cfg(unix)]
            gid: metadata.gid(),
            #[cfg(target_os = "linux")]
            acl: read_acl(&original)?,
        }))
    }

    fn apply(self, file: &File) -> Result<(), Error> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::MetadataExt;
            let metadata = file.metadata()?;
            if metadata.uid() != self.uid || metadata.gid() != self.gid {
                // SAFETY: this borrowed file owns a valid descriptor. fchown
                // either preserves the target owner/group or fails before replace.
                if unsafe { libc::fchown(file.as_raw_fd(), self.uid, self.gid) } == -1 {
                    return Err(std::io::Error::last_os_error().into());
                }
            }
        }
        #[cfg(target_os = "linux")]
        apply_acl(file, self.acl.as_deref())?;
        file.set_permissions(self.permissions)?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn read_acl(file: &File) -> Result<Option<Vec<u8>>, Error> {
    use std::os::fd::AsRawFd;
    let name = c"system.posix_acl_access";
    // SAFETY: this valid descriptor and NUL-terminated name outlive the call;
    // a null output buffer with size zero queries the required length.
    let size = unsafe { libc::fgetxattr(file.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0) };
    if size == -1 {
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::ENODATA | libc::ENOTSUP)) {
            return Ok(None);
        }
        return Err(error.into());
    }
    let mut acl = vec![0; usize::try_from(size).map_err(|_| "invalid ACL length")?];
    // SAFETY: the ACL buffer is writable for exactly the length passed in.
    let size = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            name.as_ptr(),
            acl.as_mut_ptr().cast(),
            acl.len(),
        )
    };
    if size == -1 {
        return Err(std::io::Error::last_os_error().into());
    }
    acl.truncate(usize::try_from(size).map_err(|_| "invalid ACL length")?);
    Ok(Some(acl))
}

#[cfg(target_os = "linux")]
fn apply_acl(file: &File, acl: Option<&[u8]>) -> Result<(), Error> {
    use std::os::fd::AsRawFd;
    let name = c"system.posix_acl_access";
    let status = if let Some(acl) = acl {
        // SAFETY: the valid descriptor, name and readable ACL buffer outlive this call.
        unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                acl.as_ptr().cast(),
                acl.len(),
                0,
            )
        }
    } else {
        // Remove directory-inherited entries before chmod can enable their mask.
        // SAFETY: this valid descriptor and NUL-terminated name outlive the call.
        unsafe { libc::fremovexattr(file.as_raw_fd(), name.as_ptr()) }
    };
    if status == -1 {
        let error = std::io::Error::last_os_error();
        if acl.is_none() && matches!(error.raw_os_error(), Some(libc::ENODATA | libc::ENOTSUP)) {
            return Ok(());
        }
        return Err(error.into());
    }
    Ok(())
}

/// Resolve an existing symbolic link to its target; a dangling link is an error.
/// New destinations still resolve their parent so candidates share its filesystem.
///
/// # Errors
/// Returns filesystem metadata/canonicalization errors or an invalid-path error.
fn resolve_target(path: &Path) -> Result<PathBuf, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(path.canonicalize()?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let name = path.file_name().ok_or("the PDF path has no file name")?;
            Ok(parent.canonicalize()?.join(name))
        }
        Err(error) => Err(error.into()),
    }
}

/// Remove only versioned candidates whose owning process has exited and whose
/// exclusive lock can be acquired. PID liveness also protects the interval
/// between exclusive creation and locking; reused PIDs conservatively retain files.
///
/// # Errors
/// Returns target resolution or directory/entry metadata errors. Candidate open,
/// lock, and removal failures retain the abandoned candidate path and I/O source.
#[cfg(unix)]
pub(super) fn cleanup_abandoned(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let target = resolve_target(path)?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("the PDF path is not valid UTF-8")?;
    let prefix = candidate_prefix(name);
    let legacy_prefix = format!("{name}.marker-save-v1-");
    let parent = target
        .parent()
        .ok_or("the PDF path has no parent directory")?;
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let filename = entry.file_name();
        let Some(owner) = filename.to_str().and_then(|name| {
            candidate_owner(name, &prefix).or_else(|| candidate_owner(name, &legacy_prefix))
        }) else {
            continue;
        };
        if !owner_is_dead(owner) || !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(cleanup_error(&path, error)),
        };
        match lock_file(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(cleanup_error(&path, error)),
        }
        let current = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(cleanup_error(&path, error)),
        };
        let locked = file.metadata()?;
        if current.dev() != locked.dev() || current.ino() != locked.ino() {
            continue;
        }
        if let Err(error) = std::fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(cleanup_error(&path, error));
            }
        }
    }
    Ok(())
}

// Without a supported cross-process ownership check, retaining a candidate is
// safer than guessing it is abandoned. Marker currently ships Linux binaries.
/// Retain candidates when cross-process ownership checks are unavailable.
///
/// # Errors
/// This fallback always succeeds without removing files.
#[cfg(not(unix))]
pub(super) fn cleanup_abandoned(_path: &Path) -> Result<(), Error> {
    Ok(())
}

#[cfg(unix)]
fn cleanup_error(path: &Path, error: std::io::Error) -> Error {
    Error::AbandonedSaveCleanup {
        path: path.to_path_buf(),
        source: error,
    }
}

/// FNV-1a 128 over the full basename keeps the target identifier stable across
/// processes and directory renames. Even maximum-width owner/nonce fields keep
/// the candidate basename below 160 bytes, independent of the PDF name length.
fn candidate_prefix(name: &str) -> String {
    let mut identifier = 0x6c62272e07bb014262b821756295c58d_u128;
    for byte in name.bytes() {
        identifier ^= u128::from(byte);
        identifier = identifier.wrapping_mul(0x0000000001000000000000000000013b);
    }
    format!(".marker-save-v2-{identifier:032x}-")
}

#[cfg(unix)]
fn candidate_owner(name: &str, prefix: &str) -> Option<u32> {
    let mut parts = name
        .strip_prefix(prefix)?
        .strip_suffix(".marker-tmp")?
        .split('-');
    let pid = parts.next()?.parse::<u32>().ok()?;
    parts.next()?.parse::<u128>().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    if pid == 0 || parts.next().is_some() {
        return None;
    }
    Some(pid)
}

#[cfg(unix)]
fn owner_is_dead(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: a positive process ID and signal 0 only query process existence.
    let status = unsafe { libc::kill(pid, 0) };
    status == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(unix)]
fn lock_file(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: the borrowed File owns this valid descriptor throughout the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

struct Candidate {
    path: PathBuf,
    file: File,
    pending: bool,
}

impl Candidate {
    fn new(target: &Path) -> Result<Self, Error> {
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("the PDF path is not valid UTF-8")?;
        loop {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let sequence = NEXT_SAVE.fetch_add(1, Ordering::Relaxed);
            let path = target.with_file_name(format!(
                "{}{}-{nonce}-{sequence}.marker-tmp",
                candidate_prefix(name),
                std::process::id()
            ));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file,
                        pending: true,
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn cleanup(&mut self) -> std::io::Result<()> {
        if self.pending {
            #[cfg(test)]
            fail_at(Failure::Cleanup)?;
            std::fs::remove_file(&self.path)?;
            self.pending = false;
        }
        Ok(())
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        // Explicit cleanup reports errors. Drop only covers unwinding.
        let _ = self.cleanup();
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Failure {
    Write,
    Sync,
    Replace,
    Cleanup,
}

#[cfg(test)]
thread_local! {
    static FAILURE: std::cell::Cell<Option<Failure>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn with_failure<T>(failure: Failure, run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            FAILURE.set(None);
        }
    }
    FAILURE.set(Some(failure));
    let _reset = Reset;
    run()
}

#[cfg(test)]
fn fail_at(stage: Failure) -> std::io::Result<()> {
    if FAILURE.get() == Some(stage) {
        Err(std::io::Error::other("injected save failure"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn pause_save(candidate: &Path) -> std::io::Result<()> {
    if let Some(ready) = std::env::var_os("MARKER_TEST_SAVE_READY") {
        std::fs::write(ready, candidate.as_os_str().as_encoded_bytes())?;
        let resume = std::env::var_os("MARKER_TEST_SAVE_RESUME")
            .ok_or_else(|| std::io::Error::other("missing subprocess resume path"))?;
        while !Path::new(&resume).exists() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    Ok(())
}
