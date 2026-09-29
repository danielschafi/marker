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
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        let mut settings: Self = toml::from_str(&text).unwrap_or_default();
        settings.recent.truncate(RECENT_LIMIT);
        settings
    }

    pub fn save(&self) {
        let Some(path) = config_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(text) = toml::to_string_pretty(self) {
            let _ = fs::write(path, text);
        }
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

fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "marker")
        .map(|dirs| dirs.config_dir().join("config.toml"))
}
