//! Structural PDF repair via Poppler (`pdfunite`).
//!
//! Some PDFs with cyclic/malformed xref tables open in Poppler-based viewers but
//! yield zero pages from MuPDF. Rewriting through `pdfunite` rebuilds a clean
//! xref without rasterizing or flattening page content, and keeps annotations
//! that Poppler can read.
//!
//! The original path is never overwritten; callers write a sibling copy.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::engine::Error;

/// Sibling path for a Poppler-repaired copy: `name.pdf` → `name.pdf.marker-repaired.pdf`.
pub fn repaired_sidecar_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document.pdf");
    path.with_file_name(format!("{name}.marker-repaired.pdf"))
}

pub(crate) fn is_repaired_sidecar(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".marker-repaired.pdf"))
}

/// Whether the Poppler `pdfunite` helper is on `PATH`.
pub(crate) fn pdfunite_available() -> bool {
    Command::new("pdfunite")
        .arg("-v")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Rewrite `src` through Poppler into `dst` (structural repair, not a raster flatten).
///
/// # Errors
///
/// Returns [`Error::Message`] when `pdfunite` is missing, fails, or leaves no output.
pub(crate) fn write_poppler_repaired_copy(src: &Path, dst: &Path) -> Result<(), Error> {
    if src == dst {
        return Err("refusing to overwrite the PDF during repair".into());
    }
    let tmp = dst.with_extension("marker-repair-tmp");
    let _ = std::fs::remove_file(&tmp);

    let output = Command::new("pdfunite")
        .arg(src)
        .arg(&tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                Error::from(
                    "This PDF has no readable pages. Install poppler-utils (pdfunite) to attempt repair.",
                )
            } else {
                Error::from(format!("failed to run pdfunite: {err}"))
            }
        })?;

    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.lines().next().unwrap_or("pdfunite failed");
        return Err(format!("Poppler could not repair this PDF ({detail}).").into());
    }

    let meta = std::fs::metadata(&tmp).map_err(|_| {
        Error::from("Poppler repair produced no output.")
    })?;
    if meta.len() == 0 {
        let _ = std::fs::remove_file(&tmp);
        return Err("Poppler repair produced an empty file.".into());
    }

    std::fs::rename(&tmp, dst).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        Error::from(format!("failed to write repaired PDF: {err}"))
    })?;
    Ok(())
}
