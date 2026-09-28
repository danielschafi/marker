use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::geom::Rgb;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub text_size: f32,
    pub text_color: Rgb,
    pub highlight_color: Rgb,
    pub shape_color: Rgb,
    pub shape_width: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            text_size: 14.0,
            text_color: Rgb::new(24, 24, 24),
            highlight_color: Rgb::new(255, 214, 0),
            shape_color: Rgb::new(28, 78, 186),
            shape_width: 1.5,
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
        toml::from_str(&text).unwrap_or_default()
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
}

fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "marker")
        .map(|dirs| dirs.config_dir().join("config.toml"))
}
