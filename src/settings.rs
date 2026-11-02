use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::geom::{Rgb, HIGHLIGHT_COLORS, INK_COLORS};

/// Which static annotation swatch row to show when presets are unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaletteKind {
    Highlights,
    Ink,
}

/// Static palette fallback (ink always; built-in highlights when presets are unavailable).
pub(crate) fn palette_for(kind: PaletteKind) -> &'static [Rgb] {
    match kind {
        PaletteKind::Highlights => &HIGHLIGHT_COLORS,
        PaletteKind::Ink => &INK_COLORS,
    }
}

pub const RECENT_LIMIT: usize = 15;

/// Maximum editable highlight presets (toolbar / style bar / sidebar).
pub const MAX_HIGHLIGHT_PRESETS: usize = 8;

/// Live case flag for the PDF reader thread. `dispatch_search` in `app.rs` cannot
/// take a new argument this wave (FP9 owns that file), so the worker reads here.
static SEARCH_CASE_SENSITIVE: AtomicBool = AtomicBool::new(false);

/// Case sensitivity currently used by in-flight / next search jobs.
pub fn live_search_case_sensitive() -> bool {
    SEARCH_CASE_SENSITIVE.load(Ordering::Relaxed)
}

fn sync_search_case_flag(value: bool) {
    SEARCH_CASE_SENSITIVE.store(value, Ordering::Relaxed);
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighlightPreset {
    pub name: String,
    pub rgb: Rgb,
}

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
    /// Warm sepia fill behind page tiles instead of white paper.
    #[serde(default)]
    pub sepia: bool,
    /// Opt-in LaTeX autosnippets inside text boxes (`//` → `\frac`, `mk`, `dm`).
    #[serde(default)]
    pub math_autosnippets: bool,
    /// When true, Find matches the query's letter case. Default false matches
    /// MuPDF's case-insensitive `TextPage::search`.
    #[serde(default)]
    pub search_case_sensitive: bool,
    /// Named highlight colors used by the toolbar, style bar, and sidebar filter.
    #[serde(default = "default_highlight_presets")]
    pub highlight_presets: Vec<HighlightPreset>,
    #[serde(default)]
    pub recent: Vec<PathBuf>,
}

fn default_true() -> bool {
    true
}

pub fn default_highlight_presets() -> Vec<HighlightPreset> {
    let colors = palette_for(PaletteKind::Highlights);
    vec![
        HighlightPreset {
            name: "Key idea".into(),
            rgb: colors[0],
        },
        HighlightPreset {
            name: "Definition".into(),
            rgb: colors[1],
        },
        HighlightPreset {
            name: "Example".into(),
            rgb: colors[2],
        },
        HighlightPreset {
            name: "Question".into(),
            rgb: colors[3],
        },
        HighlightPreset {
            name: "Orange".into(),
            rgb: colors[4],
        },
    ]
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
            sepia: false,
            math_autosnippets: false,
            search_case_sensitive: false,
            highlight_presets: default_highlight_presets(),
            recent: Vec::new(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            let settings = Self::default();
            sync_search_case_flag(settings.search_case_sensitive);
            return settings;
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
                settings.clamp_highlight_presets();
                sync_search_case_flag(settings.search_case_sensitive);
                settings
            }
            Err(err) => {
                eprintln!(
                    "warning: failed to parse settings at {}: {err}",
                    path.display()
                );
                backup_invalid_config(path);
                let settings = Self::default();
                sync_search_case_flag(settings.search_case_sensitive);
                settings
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
        sync_search_case_flag(self.search_case_sensitive);
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

    /// Persist and publish the Find case-sensitivity flag for the reader thread.
    pub fn set_search_case_sensitive(&mut self, value: bool) {
        self.search_case_sensitive = value;
        sync_search_case_flag(value);
        self.save();
    }

    /// Exact RGB match → preset name. Unknown colors are custom (no rewrite of annot bytes).
    pub fn highlight_name_for(&self, color: Rgb) -> Option<&str> {
        self.highlight_presets
            .iter()
            .find(|preset| preset.rgb == color)
            .map(|preset| preset.name.as_str())
    }

    pub fn reset_highlight_presets(&mut self) {
        self.highlight_presets = default_highlight_presets();
    }

    fn clamp_highlight_presets(&mut self) {
        if self.highlight_presets.is_empty() {
            self.highlight_presets = default_highlight_presets();
            return;
        }
        self.highlight_presets.truncate(MAX_HIGHLIGHT_PRESETS);
    }

    /// Move `path` to the front of the recent list and persist.
    pub fn remember_open(&mut self, path: &Path) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
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
            math_autosnippets: true,
            ..Settings::default()
        };
        settings.save_to(&path);
        let loaded = Settings::load_from(&path);
        assert!(!loaded.toolbar_visible);
        assert!(loaded.math_autosnippets);

        cleanup(&dir);
    }

    #[test]
    fn math_autosnippets_defaults_false() {
        assert!(!Settings::default().math_autosnippets);
        let (dir, path) = test_config_path("autosnippets-default");
        Settings::default().save_to(&path);
        let text = fs::read_to_string(&path).expect("read");
        let without: String = text
            .lines()
            .filter(|line| !line.contains("math_autosnippets"))
            .flat_map(|line| [line, "\n"])
            .collect();
        fs::write(&path, without).expect("write");
        let loaded = Settings::load_from(&path);
        assert!(!loaded.math_autosnippets);
        cleanup(&dir);
    }

    #[test]
    fn search_case_sensitive_defaults_false_and_round_trips() {
        assert!(!Settings::default().search_case_sensitive);
        let (dir, path) = test_config_path("search-case");
        Settings::default().save_to(&path);
        let text = fs::read_to_string(&path).expect("read");
        let without: String = text
            .lines()
            .filter(|line| !line.contains("search_case_sensitive"))
            .flat_map(|line| [line, "\n"])
            .collect();
        fs::write(&path, without).expect("write");
        let loaded = Settings::load_from(&path);
        assert!(!loaded.search_case_sensitive);

        let on = Settings {
            search_case_sensitive: true,
            ..Settings::default()
        };
        on.save_to(&path);
        let again = Settings::load_from(&path);
        assert!(again.search_case_sensitive);
        assert!(live_search_case_sensitive());
        cleanup(&dir);
    }

    #[test]
    fn palette_for_selects_static_swatches() {
        assert_eq!(palette_for(PaletteKind::Highlights), &HIGHLIGHT_COLORS);
        assert_eq!(palette_for(PaletteKind::Ink), &INK_COLORS);
    }

    #[test]
    fn missing_highlight_presets_loads_defaults() {
        let defaults = default_highlight_presets();
        assert_eq!(defaults.len(), 5);
        assert_eq!(defaults[0].name, "Key idea");
        assert_eq!(defaults[0].rgb, HIGHLIGHT_COLORS[0]);
        assert_eq!(defaults[1].name, "Definition");
        assert_eq!(defaults[2].name, "Example");
        assert_eq!(defaults[3].name, "Question");
        assert_eq!(defaults[4].name, "Orange");
        assert_eq!(defaults[4].rgb, HIGHLIGHT_COLORS[4]);

        let (dir, path) = test_config_path("presets-missing");
        Settings::default().save_to(&path);
        let text = fs::read_to_string(&path).expect("read");
        let mut value: toml::Value = toml::from_str(&text).expect("parse toml");
        value
            .as_table_mut()
            .expect("table")
            .remove("highlight_presets");
        fs::write(&path, toml::to_string_pretty(&value).expect("serialize")).expect("write");

        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.highlight_presets, defaults);
        assert_eq!(
            loaded.highlight_name_for(HIGHLIGHT_COLORS[0]),
            Some("Key idea")
        );
        assert_eq!(loaded.highlight_name_for(Rgb::new(1, 2, 3)), None);
        cleanup(&dir);
    }

    #[test]
    fn highlight_presets_round_trip_and_cap() {
        let (dir, path) = test_config_path("presets-round-trip");
        let mut presets = default_highlight_presets();
        presets[0].name = "Thesis".into();
        presets.push(HighlightPreset {
            name: "Extra".into(),
            rgb: Rgb::new(10, 20, 30),
        });
        let settings = Settings {
            highlight_presets: presets.clone(),
            ..Settings::default()
        };
        settings.save_to(&path);
        let loaded = Settings::load_from(&path);
        assert_eq!(loaded.highlight_presets, presets);
        assert_eq!(
            loaded.highlight_name_for(Rgb::new(10, 20, 30)),
            Some("Extra")
        );

        let oversized = Settings {
            highlight_presets: (0..12)
                .map(|i| HighlightPreset {
                    name: format!("P{i}"),
                    rgb: Rgb::new(i as u8, 0, 0),
                })
                .collect(),
            ..Settings::default()
        };
        oversized.save_to(&path);
        let capped = Settings::load_from(&path);
        assert_eq!(capped.highlight_presets.len(), MAX_HIGHLIGHT_PRESETS);
        cleanup(&dir);
    }
}
