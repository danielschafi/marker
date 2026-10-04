use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::geom::Rgb;

pub const RECENT_LIMIT: usize = 15;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub text_size: f32,
    pub text_color: Rgb,
    pub highlight_color: Rgb,
    pub shape_color: Rgb,
    pub shape_width: f32,
    /// Annotation tool strip under the tab bar. Tools stay reachable via shortcuts when hidden.
    #[serde(default = "default_true")]
    pub toolbar_visible: bool,
    #[serde(default)]
    pub recent: Vec<PathBuf>,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            text_size: 14.0,
            text_color: Rgb::new(24, 24, 24),
            highlight_color: Rgb::new(255, 214, 0),
            shape_color: Rgb::new(28, 78, 186),
            shape_width: 1.5,
            toolbar_visible: true,
            recent: Vec::new(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        Self::load_from(&path)
    }

    fn load_from(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        match toml::from_str::<Settings>(&text) {
            Ok(mut settings) => {
                settings.recent.truncate(RECENT_LIMIT);
                settings
            }
            Err(err) => {
                eprintln!(
                    "warning: failed to parse settings at {}: {err}",
                    path.display()
                );
                backup_invalid_config(path);
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        let Some(path) = config_path() else {
            return;
        };
        self.save_to(&path);
    }

    fn save_to(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let Ok(text) = toml::to_string_pretty(self) else {
            return;
        };
        let tmp = settings_temp_path(path);
        if fs::write(&tmp, text).is_err() {
            return;
        }
        let _ = fs::rename(&tmp, path);
    }

    /// Move `path` to the front of the recent list and persist.
    pub fn remember_open(&mut self, path: &Path) {
        let path = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf());
        self.recent.retain(|entry| entry != &path);
        self.recent.insert(0, path);
        self.recent.truncate(RECENT_LIMIT);
        self.save();
    }

    pub fn forget_recent(&mut self, path: &Path) {
        let before = self.recent.len();
        self.recent.retain(|entry| entry != path);
        if let Ok(canonical) = path.canonicalize() {
            self.recent.retain(|entry| entry != &canonical);
        }
        if self.recent.len() != before {
            self.save();
        }
    }
}

fn backup_invalid_config(path: &Path) {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    let bak = path.with_file_name(format!("{file_name}.bak"));
    let _ = fs::copy(path, &bak);
}

fn settings_temp_path(path: &Path) -> PathBuf {
    let mut tmp = path.to_path_buf();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    tmp.set_file_name(format!("{name}.marker-tmp"));
    tmp
}

fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "marker")
        .map(|dirs| dirs.config_dir().join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_config_path(label: &str) -> (PathBuf, PathBuf) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("marker-settings-{label}-{nanos}"));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        (dir, path)
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn parse_error_leaves_config_and_creates_backup() {
        let (dir, path) = test_config_path("parse-error");
        let bad = "not valid toml [[[";
        fs::write(&path, bad).expect("write");

        let settings = Settings::load_from(&path);
        assert_eq!(settings.text_size, Settings::default().text_size);
        assert!(settings.recent.is_empty());
        assert_eq!(fs::read_to_string(&path).expect("read"), bad);
        assert!(path.with_file_name("config.toml.bak").is_file());

        cleanup(&dir);
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temp_file() {
        let (dir, path) = test_config_path("atomic-save");
        let settings = Settings {
            text_size: 18.5,
            ..Settings::default()
        };
        settings.save_to(&path);

        let tmp = settings_temp_path(&path);
        assert!(!tmp.exists());
        let loaded = Settings::load_from(&path);
        assert!((loaded.text_size - 18.5).abs() < f32::EPSILON);

        cleanup(&dir);
    }

    #[test]
    fn round_trip_valid_config() {
        let (dir, path) = test_config_path("round-trip");
        let settings = Settings {
            toolbar_visible: false,
            ..Settings::default()
        };
        settings.save_to(&path);
        let loaded = Settings::load_from(&path);
        assert!(!loaded.toolbar_visible);

        cleanup(&dir);
    }
}
