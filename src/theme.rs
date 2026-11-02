//! Chrome colors driven by egui's resolved theme, with a Linux OS probe.
//!
//! On Windows/macOS, `ThemePreference::System` tracks the desktop. On Linux,
//! winit often reports no system theme, so we read GNOME `gsettings` when present.
//! Named schemes (Tokyo Night, Everforest) restyle chrome only — PDF tiles stay as-is.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use egui::{Color32, Context, Theme, ThemePreference};

use crate::platform::gsettings;
use crate::settings::UiTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

    /// Tokyo Night Storm–inspired chrome (UI only).
    pub const TOKYO_NIGHT: Self = Self {
        chrome: Color32::from_rgb(26, 27, 38),
        chrome_raised: Color32::from_rgb(36, 40, 59),
        backdrop: Color32::from_rgb(22, 22, 30),
        accent: Color32::from_rgb(122, 162, 247),
        text: Color32::from_rgb(192, 202, 245),
        text_dim: Color32::from_rgb(86, 95, 137),
        dirty: Color32::from_rgb(224, 175, 104),
        hairline: Color32::from_rgba_unmultiplied_const(192, 202, 245, 28),
        widget_inactive: Color32::from_rgb(41, 46, 66),
        widget_hovered: Color32::from_rgb(51, 57, 82),
        widget_active: Color32::from_rgb(61, 68, 99),
        faint_bg: Color32::from_rgb(36, 40, 59),
    };

    /// Everforest Dark Medium–inspired chrome (UI only).
    pub const EVERFOREST: Self = Self {
        chrome: Color32::from_rgb(45, 53, 59),
        chrome_raised: Color32::from_rgb(61, 72, 77),
        backdrop: Color32::from_rgb(35, 42, 46),
        accent: Color32::from_rgb(167, 192, 128),
        text: Color32::from_rgb(211, 198, 170),
        text_dim: Color32::from_rgb(133, 146, 137),
        dirty: Color32::from_rgb(230, 152, 117),
        hairline: Color32::from_rgba_unmultiplied_const(211, 198, 170, 28),
        widget_inactive: Color32::from_rgb(64, 75, 80),
        widget_hovered: Color32::from_rgb(75, 88, 93),
        widget_active: Color32::from_rgb(86, 99, 104),
        faint_bg: Color32::from_rgb(55, 64, 69),
    };

    fn with_translucent(self, on: bool) -> Self {
        if !on {
            return self;
        }
        Self {
            chrome: with_alpha(self.chrome, 210),
            chrome_raised: with_alpha(self.chrome_raised, 220),
            backdrop: with_alpha(self.backdrop, 200),
            accent: self.accent,
            text: self.text,
            text_dim: self.text_dim,
            dirty: self.dirty,
            hairline: with_alpha(self.hairline, 36),
            widget_inactive: with_alpha(self.widget_inactive, 200),
            widget_hovered: with_alpha(self.widget_hovered, 210),
            widget_active: with_alpha(self.widget_active, 220),
            faint_bg: with_alpha(self.faint_bg, 200),
        }
    }

    fn is_dark(self) -> bool {
        let lum = u16::from(self.chrome.r()) + u16::from(self.chrome.g()) + u16::from(self.chrome.b());
        lum < 480
    }
}

fn with_alpha(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

type LinuxThemeCache = Option<(Instant, Option<Theme>, Option<Color32>)>;
static LINUX_CACHE: Mutex<LinuxThemeCache> = Mutex::new(None);

#[derive(Clone, Copy, PartialEq, Eq)]
struct AppliedTheme {
    preference: UiTheme,
    translucent: bool,
    resolved: Theme,
    os_accent: Option<(u8, u8, u8)>,
}

static APPLIED: Mutex<Option<AppliedTheme>> = Mutex::new(None);
static ACTIVE_COLORS: Mutex<ThemeColors> = Mutex::new(ThemeColors::DARK);

/// Resolved Marker chrome for the current egui theme, with optional OS accent.
pub(crate) fn palette(ctx: &Context) -> ThemeColors {
    if let Ok(guard) = ACTIVE_COLORS.lock() {
        if APPLIED.lock().ok().and_then(|g| *g).is_some() {
            return *guard;
        }
    }
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
    if palette(ctx).is_dark() {
        Color32::from_white_alpha(alpha)
    } else {
        Color32::from_black_alpha(alpha)
    }
}

/// Ring around the active color swatch in the style bar.
pub(crate) fn color_dot_selection_ring(ctx: &Context) -> Color32 {
    if palette(ctx).is_dark() {
        Color32::WHITE
    } else {
        Color32::from_rgba_unmultiplied(28, 28, 32, 230)
    }
}

/// Install dual dark/light styles and follow the OS where possible (startup default).
pub(crate) fn apply_theme(ctx: &Context) {
    apply_user_theme(ctx, UiTheme::System, false, true);
}

/// Apply the user's chrome theme preference. PDF tiles are unaffected.
pub(crate) fn apply_user_theme(
    ctx: &Context,
    preference: UiTheme,
    translucent: bool,
    force: bool,
) {
    refresh_linux_cache();
    let os_theme = ctx.system_theme().or_else(cached_linux_theme);
    let os_accent = cached_linux_accent();
    let accent_key = os_accent.map(|c| (c.r(), c.g(), c.b()));

    let resolved = match preference {
        UiTheme::System => os_theme.unwrap_or(Theme::Dark),
        UiTheme::Dark | UiTheme::TokyoNight | UiTheme::Everforest => Theme::Dark,
        UiTheme::Light => Theme::Light,
    };

    let already = APPLIED.lock().ok().and_then(|g| *g);
    if !force {
        if let Some(prev) = already {
            if prev.preference == preference
                && prev.translucent == translucent
                && prev.resolved == resolved
                && prev.os_accent == accent_key
            {
                return;
            }
        }
    }

    // Keep both egui bases populated so System flips stay instant.
    install_visuals(ctx, Theme::Dark, &ThemeColors::DARK.with_translucent(translucent));
    install_visuals(ctx, Theme::Light, &ThemeColors::LIGHT.with_translucent(translucent));

    let mut colors = match preference {
        UiTheme::System | UiTheme::Dark => match resolved {
            Theme::Dark => ThemeColors::DARK,
            Theme::Light => ThemeColors::LIGHT,
        },
        UiTheme::Light => ThemeColors::LIGHT,
        UiTheme::TokyoNight => ThemeColors::TOKYO_NIGHT,
        UiTheme::Everforest => ThemeColors::EVERFOREST,
    };
    if matches!(preference, UiTheme::System | UiTheme::Dark | UiTheme::Light) {
        if let Some(accent) = os_accent {
            colors.accent = accent;
        }
    }
    colors = colors.with_translucent(translucent);
    install_visuals(ctx, resolved, &colors);

    match preference {
        UiTheme::System => {
            ctx.set_theme(ThemePreference::System);
            ctx.options_mut(|opt| {
                opt.fallback_theme = Theme::Dark;
            });
            // Linux often has no winit system theme — pin the probed value.
            if ctx.system_theme().is_none() {
                if let Some(theme) = os_theme {
                    ctx.set_theme(theme);
                }
            }
        }
        UiTheme::Dark | UiTheme::TokyoNight | UiTheme::Everforest => {
            ctx.set_theme(ThemePreference::Dark);
        }
        UiTheme::Light => {
            ctx.set_theme(ThemePreference::Light);
        }
    }

    if let Ok(mut guard) = ACTIVE_COLORS.lock() {
        *guard = colors;
    }
    if let Ok(mut guard) = APPLIED.lock() {
        *guard = Some(AppliedTheme {
            preference,
            translucent,
            resolved,
            os_accent: accent_key,
        });
    }
}

/// Refresh Linux color-scheme / accent when following System, otherwise no-op
/// unless the active preference needs an OS accent refresh.
pub(crate) fn sync_os_theme(ctx: &Context, preference: UiTheme, translucent: bool) {
    apply_user_theme(ctx, preference, translucent, false);
}

fn install_visuals(ctx: &Context, theme: Theme, c: &ThemeColors) {
    let mut visuals = theme.default_visuals();
    visuals.window_fill = c.chrome_raised;
    visuals.panel_fill = c.chrome;
    visuals.extreme_bg_color = c.backdrop;
    visuals.faint_bg_color = c.faint_bg;
    visuals.widgets.noninteractive.bg_fill = c.chrome;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, c.hairline);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_themes_differ_from_defaults() {
        assert_ne!(ThemeColors::TOKYO_NIGHT.chrome, ThemeColors::DARK.chrome);
        assert_ne!(ThemeColors::EVERFOREST.accent, ThemeColors::DARK.accent);
        assert!(ThemeColors::TOKYO_NIGHT.is_dark());
        assert!(ThemeColors::EVERFOREST.is_dark());
        assert!(!ThemeColors::LIGHT.is_dark());
    }

    #[test]
    fn translucent_lowers_chrome_alpha() {
        let solid = ThemeColors::DARK;
        let soft = solid.with_translucent(true);
        assert_eq!(soft.chrome.a(), 210);
        assert_eq!(solid.with_translucent(false).chrome.a(), solid.chrome.a());
    }
}
