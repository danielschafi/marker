use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct AgentCapability {
    pub path: PathBuf,
    pub version: String,
    pub logged_in: bool,
    #[allow(dead_code)]
    pub account_hint: Option<String>,
}

/// Probe the standalone `agent` executable. Does not fall back to `cursor agent`
/// (Phase 0: that probe hangs headlessly).
pub fn probe_agent() -> Result<AgentCapability, String> {
    let path = which_agent().ok_or_else(|| {
        String::from(
            "Cursor Agent CLI (`agent`) not found on PATH. Install Cursor Agent and try again.",
        )
    })?;
    let version = run_capture(&path, &["--version"], Duration::from_secs(3))?
        .trim()
        .to_string();
    if version.is_empty() {
        return Err("Cursor Agent CLI returned an empty version.".into());
    }
    let status = run_capture(&path, &["status"], Duration::from_secs(8)).unwrap_or_default();
    let logged_in = status.to_ascii_lowercase().contains("logged in");
    let account_hint = status
        .lines()
        .find(|line| line.to_ascii_lowercase().contains("logged in"))
        .map(|line| {
            line.split(" as ")
                .nth(1)
                .unwrap_or(line)
                .trim()
                .to_string()
        });
    Ok(AgentCapability {
        path,
        version,
        logged_in,
        account_hint,
    })
}

pub fn create_chat(agent: &Path) -> Result<String, String> {
    let out = run_capture(agent, &["create-chat"], Duration::from_secs(15))?;
    let id = out
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "create-chat returned no chat id".to_string())?
        .to_string();
    if id.len() < 8 {
        return Err(format!("create-chat returned a suspicious id: {id}"));
    }
    Ok(id)
}

fn which_agent() -> Option<PathBuf> {
    if let Ok(override_path) = std::env::var("MARKER_AGENT") {
        let path = PathBuf::from(override_path);
        if path.is_file() {
            return Some(path);
        }
    }
    let output = Command::new("which")
        .arg("agent")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn run_capture(bin: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    // Prefer a short hard timeout via the `timeout` utility when available.
    let mut cmd = if Command::new("timeout")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        let secs = timeout.as_secs().max(1).to_string();
        let mut c = Command::new("timeout");
        c.arg(&secs).arg(bin).args(args);
        c
    } else {
        let mut c = Command::new(bin);
        c.args(args);
        c
    };
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run {}: {e}", bin.display()))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let err = err.trim();
        if err.is_empty() {
            return Err(format!(
                "{} {:?} exited {}",
                bin.display(),
                args,
                output.status
            ));
        }
        return Err(err.to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn create_chat_parses_fake_cli() {
        let dir = std::env::temp_dir().join(format!("marker-fake-agent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("agent");
        fs::write(
            &script,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'test-agent-1'; exit 0; fi\nif [ \"$1\" = \"status\" ]; then echo '✓ Logged in as test@example.com'; exit 0; fi\nif [ \"$1\" = \"create-chat\" ]; then echo 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee'; exit 0; fi\necho unexpected >&2; exit 1\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("MARKER_AGENT", &script);
        let cap = probe_agent().expect("probe");
        assert!(cap.logged_in);
        assert_eq!(cap.version, "test-agent-1");
        let id = create_chat(&cap.path).unwrap();
        assert_eq!(id, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
        std::env::remove_var("MARKER_AGENT");
        let _ = fs::remove_dir_all(&dir);
    }
}
