//! Settings window for themes, tool defaults, and toolbar visibility (#86).

use egui::{Button, Color32, DragValue, Id, RichText, ScrollArea, Sense, Stroke, Vec2};

use crate::app::{MarkerApp, Tool};
use crate::geom::Rgb;
use crate::settings::UiTheme;
use crate::theme;

use super::palette;

pub(super) fn settings_window(app: &mut MarkerApp, ctx: &egui::Context) {
    if !app.settings_open {
        return;
    }
    let colors = palette(ctx);
    let mut open = app.settings_open;
    let mut dirty = false;
    let mut open_presets = false;
    let mut theme_changed = false;

    egui::Window::new("Settings")
        .id(Id::new("marker-settings"))
        .open(&mut open)
        .resizable(true)
        .default_size(Vec2::new(440.0, 560.0))
        .min_width(360.0)
        .min_height(320.0)
        .frame(
            egui::Frame::new()
                .fill(colors.chrome_raised)
                .stroke(Stroke::new(1.0_f32, colors.hairline))
                .corner_radius(8.0)
                .inner_margin(12.0),
        )
        .show(ctx, |ui| {
            ui.label(
                RichText::new("Appearance and annotation defaults. PDF pages stay unstyled.")
                    .weak()
                    .size(12.0),
            );
            ui.add_space(8.0);
            ScrollArea::vertical()
                .id_salt("settings-scroll")
                .show(ui, |ui| {
                    ui.label(RichText::new("Theme").strong().size(13.0));
                    ui.label(
                        RichText::new("Chrome only — page tiles keep their own colors.")
                            .weak()
                            .size(11.0),
                    );
                    ui.add_space(4.0);
                    for candidate in UiTheme::ALL {
                        let selected = app.settings.ui_theme == candidate;
                        if ui
                            .selectable_label(selected, candidate.label())
                            .on_hover_text(candidate.hint())
                            .clicked()
                            && !selected
                        {
                            app.settings.ui_theme = candidate;
                            theme_changed = true;
                            dirty = true;
                        }
                    }
                    ui.add_space(6.0);
                    if ui
                        .checkbox(
                            &mut app.settings.translucent_chrome,
                            "Translucent window background",
                        )
                        .on_hover_text(
                            "Shows the desktop through the empty area around pages (and softens chrome). PDF pages stay opaque. Needs compositor alpha support (Wayland/X11).",
                        )
                        .changed()
                    {
                        theme_changed = true;
                        dirty = true;
                    }

                    ui.add_space(14.0);
                    ui.label(RichText::new("Tool colors").strong().size(13.0));
                    ui.label(
                        RichText::new("Defaults used when creating new annotations.")
                            .weak()
                            .size(11.0),
                    );
                    ui.add_space(4.0);
                    dirty |= rgb_row(ui, "Highlight", &mut app.settings.highlight_color);
                    dirty |= rgb_row(ui, "Text / note", &mut app.settings.text_color);
                    dirty |= rgb_row(ui, "Shapes", &mut app.settings.shape_color);
                    ui.add_space(4.0);
                    if ui
                        .button("Edit highlight presets…")
                        .on_hover_text("Named colors for the toolbar and export")
                        .clicked()
                    {
                        open_presets = true;
                    }

                    ui.add_space(14.0);
                    ui.label(RichText::new("Toolbar tools").strong().size(13.0));
                    ui.label(
                        RichText::new("Hidden tools stay available via shortcuts.")
                            .weak()
                            .size(11.0),
                    );
                    ui.add_space(4.0);
                    for tool in Tool::ALL {
                        let mut visible = app.settings.tool_visible(tool.settings_id());
                        let enabled = tool != Tool::Select;
                        let response = ui.add_enabled(
                            enabled,
                            egui::Checkbox::new(
                                &mut visible,
                                format!("{} ({})", tool.label(), tool.shortcut().name()),
                            ),
                        );
                        if enabled && response.changed() {
                            app.settings.set_tool_visible(tool.settings_id(), visible);
                            dirty = true;
                        }
                        if !enabled {
                            response.on_hover_text("Select stays on the toolbar");
                        }
                    }

                    ui.add_space(14.0);
                    ui.label(RichText::new("Defaults").strong().size(13.0));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("Text size");
                        if ui
                            .add(
                                DragValue::new(&mut app.settings.text_size)
                                    .range(6.0..=96.0)
                                    .speed(0.5)
                                    .suffix(" pt"),
                            )
                            .changed()
                        {
                            dirty = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Shape width");
                        if ui
                            .add(
                                DragValue::new(&mut app.settings.shape_width)
                                    .range(0.25..=16.0)
                                    .speed(0.05)
                                    .suffix(" pt"),
                            )
                            .changed()
                        {
                            dirty = true;
                        }
                    });
                    if ui
                        .checkbox(&mut app.settings.toolbar_visible, "Show tools strip")
                        .on_hover_text("Ctrl+Shift+B also toggles")
                        .changed()
                    {
                        dirty = true;
                    }
                    if ui
                        .checkbox(&mut app.settings.sepia, "Sepia page background")
                        .on_hover_text("Warm fill behind page tiles")
                        .changed()
                    {
                        dirty = true;
                    }
                    if ui
                        .checkbox(
                            &mut app.settings.math_autosnippets,
                            "LaTeX autosnippets in text boxes",
                        )
                        .on_hover_text("`//` → fraction helpers, `mk` / `dm`")
                        .changed()
                    {
                        dirty = true;
                    }
                    if ui
                        .checkbox(
                            &mut app.settings.search_case_sensitive,
                            "Case-sensitive Find",
                        )
                        .changed()
                    {
                        dirty = true;
                    }

                    ui.add_space(12.0);
                    if ui
                        .add(
                            Button::new("Reset appearance defaults")
                                .fill(Color32::TRANSPARENT)
                                .stroke(Stroke::new(1.0_f32, colors.hairline)),
                        )
                        .on_hover_text("Theme, translucency, tool visibility, and sizes")
                        .clicked()
                    {
                        app.settings.ui_theme = UiTheme::System;
                        app.settings.translucent_chrome = false;
                        app.settings.hidden_tools.clear();
                        app.settings.text_size = 14.0;
                        app.settings.text_color = Rgb::new(24, 24, 24);
                        app.settings.highlight_color = Rgb::new(255, 214, 0);
                        app.settings.shape_color = Rgb::new(28, 78, 186);
                        app.settings.shape_width = 1.5;
                        app.settings.toolbar_visible = true;
                        app.settings.sepia = false;
                        app.settings.math_autosnippets = false;
                        app.settings.search_case_sensitive = false;
                        theme_changed = true;
                        dirty = true;
                    }
                });
        });

    if open_presets {
        app.preset_editor_open = true;
    }
    if dirty {
        if !app.settings.tool_visible(app.tool.settings_id()) {
            app.tool = Tool::Select;
        }
        app.settings.save();
    }
    if theme_changed {
        theme::apply_user_theme(
            ctx,
            app.settings.ui_theme,
            app.settings.translucent_chrome,
            true,
        );
    }
    app.settings_open = open;
}

fn rgb_row(ui: &mut egui::Ui, label: &str, rgb: &mut Rgb) -> bool {
    let mut dirty = false;
    ui.horizontal(|ui| {
        let swatch = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
        ui.painter()
            .circle_filled(swatch.0.center(), 6.5, rgb.to_color32());
        ui.add_sized(
            Vec2::new(88.0, 18.0),
            egui::Label::new(RichText::new(label).size(12.0)),
        );
        ui.label(RichText::new("R").weak().size(11.0));
        let mut r = rgb.r as i32;
        if ui
            .add(DragValue::new(&mut r).range(0..=255).speed(1.0))
            .changed()
        {
            rgb.r = r as u8;
            dirty = true;
        }
        ui.label(RichText::new("G").weak().size(11.0));
        let mut g = rgb.g as i32;
        if ui
            .add(DragValue::new(&mut g).range(0..=255).speed(1.0))
            .changed()
        {
            rgb.g = g as u8;
            dirty = true;
        }
        ui.label(RichText::new("B").weak().size(11.0));
        let mut b = rgb.b as i32;
        if ui
            .add(DragValue::new(&mut b).range(0..=255).speed(1.0))
            .changed()
        {
            rgb.b = b as u8;
            dirty = true;
        }
    });
    dirty
}
