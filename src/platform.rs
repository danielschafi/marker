//! OS helpers: clipboard, browser, URL encoding, and desktop settings probes.

use std::process::{Command, Stdio};

/// Percent-encode for query strings (spaces as `+`).
pub(crate) fn urlencoding_minimal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// True when the system clipboard currently offers an image (for context menus).
pub(crate) fn clipboard_has_image() -> bool {
    // Prefer a cheap MIME listing on Wayland before pulling pixel data.
    if let Ok(output) = Command::new("wl-paste")
        .arg("-l")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        if output.status.success() {
            let list = String::from_utf8_lossy(&output.stdout);
            if list.lines().any(|line| {
                let line = line.trim().to_ascii_lowercase();
                line.starts_with("image/")
                    || line.contains("image/png")
                    || line.contains("image/jpeg")
            }) {
                return true;
            }
        }
    }
    arboard::Clipboard::new()
        .ok()
        .and_then(|mut clipboard| clipboard.get_image().ok())
        .is_some_and(|image| image.width > 0 && image.height > 0)
}

/// RGBA pixels from the system clipboard (arboard, then `wl-paste` on Wayland).
pub(crate) fn read_clipboard_rgba() -> Option<(u32, u32, std::sync::Arc<[u8]>)> {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        if let Ok(image) = clipboard.get_image() {
            let width = image.width as u32;
            let height = image.height as u32;
            let expected = width as usize * height as usize * 4;
            if width > 0 && height > 0 && image.bytes.len() >= expected {
                let rgba = std::sync::Arc::from(image.bytes[..expected].to_vec());
                return Some((width, height, rgba));
            }
        }
    }
    read_clipboard_rgba_wl_paste()
}

fn read_clipboard_rgba_wl_paste() -> Option<(u32, u32, std::sync::Arc<[u8]>)> {
    let output = Command::new("wl-paste")
        .args(["-t", "image/png"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    let image = image::load_from_memory(&output.stdout).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height, std::sync::Arc::from(image.into_raw())))
}

pub(crate) fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Command::new("xdg-open")
            .arg(url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Run `gsettings` and return trimmed stdout on success.
pub(crate) fn gsettings(args: &[&str]) -> Option<String> {
    let output = Command::new("gsettings").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
