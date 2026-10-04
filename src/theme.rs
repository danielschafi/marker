//! Chrome colors driven by egui's resolved theme, with a Linux OS probe.
//!
//! On Windows/macOS, `ThemePreference::System` tracks the desktop. On Linux,
//! winit often reports no system theme, so we read GNOME `gsettings` when present.

use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use egui::{Color32, Context, Theme, ThemePreference};

#[derive(Clone, Copy)]
pub(crate) struct ThemeColors {
    pub chrome: Color32,
    pub chrome_raised: Color32,
    pub backdrop: Color32,
    pub accent: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub dirty: Color32,
    pub hairline: Color32,
    pub widget_inactive: Color32,
    pub widget_hovered: Color32,
    pub widget_active: Color32,
    pub faint_bg: Color32,
}

impl ThemeColors {
    pub const DARK: Self = Self {
        chrome: Color32::from_rgb(30, 30, 34),
        chrome_raised: Color32::from_rgb(42, 42, 48),
        backdrop: Color32::from_rgb(16, 16, 18),
        accent: Color32::from_rgb(110, 156, 230),
        text: Color32::from_rgb(232, 232, 236),
        text_dim: Color32::from_rgb(154, 154, 162),
        dirty: Color32::from_rgb(230, 186, 92),
        hairline: Color32::from_rgba_unmultiplied_const(255, 255, 255, 26),
        widget_inactive: Color32::from_rgb(48, 48, 56),
        widget_hovered: Color32::from_rgb(60, 60, 68),
        widget_active: Color32::from_rgb(70, 70, 80),
        faint_bg: Color32::from_rgb(48, 48, 54),
    };

    pub const LIGHT: Self = Self {
        chrome: Color32::from_rgb(245, 245, 247),
        chrome_raised: Color32::from_rgb(255, 255, 255),
        backdrop: Color32::from_rgb(228, 228, 232),
        accent: Color32::from_rgb(56, 112, 196),
        text: Color32::from_rgb(28, 28, 32),
        text_dim: Color32::from_rgb(110, 110, 118),
        dirty: Color32::from_rgb(180, 120, 40),
        hairline: Color32::from_rgba_unmultiplied_const(0, 0, 0, 22),
        widget_inactive: Color32::from_rgb(235, 235, 240),
        widget_hovered: Color32::from_rgb(220, 220, 228),
        widget_active: Color32::from_rgb(200, 200, 212),
        faint_bg: Color32::from_rgb(238, 238, 242),
    };
}

static LINUX_CACHE: Mutex<Option<(Instant, Option<Theme>, Option<Color32>)>> = Mutex::new(None);

/// Resolved Marker chrome for the current egui theme, with optional OS accent.
pub(crate) fn palette(ctx: &Context) -> ThemeColors {
    let mut colors = match ctx.theme() {
        Theme::Dark => ThemeColors::DARK,
        Theme::Light => ThemeColors::LIGHT,
    };
    if let Some(accent) = cached_linux_accent() {
        colors.accent = accent;
    }
    colors
}

pub(crate) fn accent_fill(ctx: &Context, alpha: u8) -> Color32 {
    let a = palette(ctx).accent;
    Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), alpha)
}

/// Semi-transparent wash for custom chrome (tab pills, tool chips, clusters).
pub(crate) fn chrome_overlay(ctx: &Context, alpha: u8) -> Color32 {
    match ctx.theme() {
        Theme::Dark => Color32::from_white_alpha(alpha),
        Theme::Light => Color32::from_black_alpha(alpha),
    }
}

/// Ring around the active color swatch in the style bar.
pub(crate) fn color_dot_selection_ring(ctx: &Context) -> Color32 {
    match ctx.theme() {
        Theme::Dark => Color32::WHITE,
        Theme::Light => Color32::from_rgba_unmultiplied(28, 28, 32, 230),
    }
}

/// Install dual dark/light styles and follow the OS where possible.
pub(crate) fn apply_theme(ctx: &Context) {
    ctx.set_theme(ThemePreference::System);
    ctx.options_mut(|opt| {
        opt.fallback_theme = Theme::Dark;
    });
    install_visuals(ctx, Theme::Dark, &ThemeColors::DARK);
    install_visuals(ctx, Theme::Light, &ThemeColors::LIGHT);
    sync_os_theme(ctx);
}

fn install_visuals(ctx: &Context, theme: Theme, c: &ThemeColors) {
    let mut visuals = theme.default_visuals();
    visuals.window_fill = c.chrome_raised;
    visuals.panel_fill = c.chrome;
    visuals.extreme_bg_color = c.backdrop;
    visuals.faint_bg_color = c.faint_bg;
    visuals.widgets.noninteractive.bg_fill = c.chrome;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, c.hairline);
    visuals.widgets.inactive.bg_fill = c.widget_inactive;
    visuals.widgets.hovered.bg_fill = c.widget_hovered;
    visuals.widgets.active.bg_fill = c.widget_active;
    visuals.selection.bg_fill = c.accent;
    visuals.override_text_color = Some(c.text);
    visuals.window_corner_radius = egui::CornerRadius::same(12);
    visuals.menu_corner_radius = egui::CornerRadius::same(8);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(8);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(8);
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(8);
    ctx.set_visuals_of(theme, visuals);
}

/// Refresh Linux color-scheme / accent when winit has no system theme.
pub(crate) fn sync_os_theme(ctx: &Context) {
    let prev_theme = cached_linux_theme();
    let prev_accent = cached_linux_accent();
    refresh_linux_cache();
    let theme = cached_linux_theme();
    let accent = cached_linux_accent();

    if ctx.system_theme().is_some() {
        // Native path (Win/macOS/web) already feeds RawInput.system_theme.
        if accent != prev_accent {
            if let Some(accent) = accent {
                let current = ctx.theme();
                let mut colors = match current {
                    Theme::Dark => ThemeColors::DARK,
                    Theme::Light => ThemeColors::LIGHT,
                };
                colors.accent = accent;
                install_visuals(ctx, current, &colors);
            }
        }
        return;
    }
    if theme == prev_theme && accent == prev_accent {
        return;
    }
    if let Some(theme) = theme {
        if ctx.theme() != theme {
            ctx.set_theme(theme);
        }
        let mut colors = match theme {
            Theme::Dark => ThemeColors::DARK,
            Theme::Light => ThemeColors::LIGHT,
        };
        if let Some(accent) = accent {
            colors.accent = accent;
        }
        install_visuals(ctx, theme, &colors);
    }
}

fn cached_linux_theme() -> Option<Theme> {
    LINUX_CACHE
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|(_, t, _)| *t))
}

fn cached_linux_accent() -> Option<Color32> {
    LINUX_CACHE
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|(_, _, a)| *a))
}

fn refresh_linux_cache() {
    let Ok(mut guard) = LINUX_CACHE.lock() else {
        return;
    };
    if let Some((at, _, _)) = *guard {
        if at.elapsed() < Duration::from_secs(2) {
            return;
        }
    }
    let theme = probe_linux_theme();
    let accent = probe_linux_accent();
    *guard = Some((Instant::now(), theme, accent));
}

fn probe_linux_theme() -> Option<Theme> {
    let stdout = gsettings(&["get", "org.gnome.desktop.interface", "color-scheme"])?;
    if stdout.contains("prefer-dark") {
        Some(Theme::Dark)
    } else if stdout.contains("prefer-light") {
        Some(Theme::Light)
    } else {
        // 'default' — fall back to gtk-theme name heuristics.
        let gtk = gsettings(&["get", "org.gnome.desktop.interface", "gtk-theme"])?;
        if gtk.to_ascii_lowercase().contains("dark") {
            Some(Theme::Dark)
        } else {
            Some(Theme::Light)
        }
    }
}

fn probe_linux_accent() -> Option<Color32> {
    let stdout = gsettings(&["get", "org.gnome.desktop.interface", "accent-color"])?;
    let name = stdout.trim().trim_matches('\'').trim_matches('"');
    Some(match name {
        "blue" => Color32::from_rgb(110, 156, 230),
        "teal" => Color32::from_rgb(46, 168, 168),
        "green" => Color32::from_rgb(72, 168, 96),
        "yellow" => Color32::from_rgb(210, 170, 50),
        "orange" => Color32::from_rgb(230, 140, 60),
        "red" => Color32::from_rgb(220, 90, 90),
        "pink" => Color32::from_rgb(220, 120, 170),
        "purple" => Color32::from_rgb(150, 120, 210),
        "slate" => Color32::from_rgb(120, 130, 148),
        _ => return None,
    })
}

fn gsettings(args: &[&str]) -> Option<String> {
    let output = Command::new("gsettings").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
