//! App chrome, shared widgets, and panel module wiring (RF12).

use std::sync::Arc;

use egui::{
    Align, Align2, Button, Color32, CornerRadius, DragValue, FontData, FontDefinitions, FontFamily,
    FontId, Id, Key, Layout, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextEdit,
    TopBottomPanel, Vec2, WidgetInfo, WidgetType,
};

use crate::annot::{AnnotKind, Annotation, ForeignKind, MarkupStyle, ShapeKind};
use crate::app::{AnnotFilterKind, MarkerApp, ReviewGroupMode, ShortcutSection, Tab, SHORTCUTS};
use crate::assistant::{glyphs_intersecting_rects, reconstruct_text};
use crate::geom::{Rgb, INK_COLORS};
use crate::pdf::OutlineNode;
use crate::settings::{HighlightPreset, MAX_HIGHLIGHT_PRESETS};
use crate::theme::{self, ThemeColors};

mod assistant_panel;
mod empty_state;
mod split_drop;
mod style_bar;
mod tab_bar;
mod toolbar;

pub(crate) use empty_state::empty_state;
pub(crate) use split_drop::{split_viewports, tab_drag_overlay};

use assistant_panel::assistant_panel;
use empty_state::outline_node;
use tab_bar::{paint_tab_list, paint_tab_menu, tab_bar};
use toolbar::tool_bar;

const BRAND_FAMILY: &str = "Brand";

pub(crate) const CONTROL_H: f32 = 20.0;

const SEARCH_BAR_H: f32 = 32.0;
const SEARCH_HIT_ROW_H: f32 = 22.0;
const SEARCH_HIT_MAX_ROWS: usize = 6;

pub(crate) fn palette(ctx: &egui::Context) -> ThemeColors {
    theme::palette(ctx)
}

pub(crate) fn chrome(app: &mut MarkerApp, ctx: &egui::Context) {
    let show_chrome = app.chrome_visible(ctx);
    if show_chrome {
        tab_bar(app, ctx);
        tool_bar(app, ctx);
    }
    search_bar(app, ctx);
    if show_chrome {
        outline_panel(app, ctx);
        thumbs_panel(app, ctx);
        annot_sidebar(app, ctx);
        review_panel(app, ctx);
        assistant_panel(app, ctx);
    }
    paint_tab_list(app, ctx, show_chrome);
    // Menu must paint even in zen (tab list still opens there).
    paint_tab_menu(app, ctx);
    cheatsheet_window(app, ctx);
    preset_editor_window(app, ctx);
}

fn search_bar(app: &mut MarkerApp, ctx: &egui::Context) {
    let colors = palette(ctx);
    let mut close = false;
    let mut next = false;
    let mut prev = false;
    let mut jump = None;
    let mut case_flag = app.settings.search_case_sensitive;
    let mut case_changed = false;
    {
        let Some(tab) = app.tab_mut() else {
            return;
        };
        if !tab.search.open {
            return;
        }
        let mut query_changed = false;
        let hit_rows = tab.search.hits.len().min(SEARCH_HIT_MAX_ROWS);
        let list_h = if hit_rows > 0 {
            hit_rows as f32 * SEARCH_HIT_ROW_H + 6.0
        } else {
            0.0
        };
        TopBottomPanel::top("search")
            .exact_height(SEARCH_BAR_H + list_h)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(colors.chrome)
                    .stroke(Stroke::NONE)
                    .inner_margin(egui::Margin::symmetric(10, 4)),
            )
            .show(ctx, |ui| {
                // Fixed-height find row so the hit list below keeps room
                // (horizontal_centered would eat the whole panel height).
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), SEARCH_BAR_H - 8.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.label(RichText::new("Find").strong());
                        let edit = ui.add(
                            TextEdit::singleline(&mut tab.search.query)
                                .desired_width(280.0)
                                .hint_text("Search this PDF"),
                        );
                        if tab.search.focus {
                            edit.request_focus();
                            tab.search.focus = false;
                        }
                        if edit.changed() {
                            query_changed = true;
                        }
                        if ui
                            .checkbox(&mut case_flag, "Case")
                            .on_hover_text("Match letter case")
                            .changed()
                        {
                            case_changed = true;
                        }
                        let count = tab.search.hits.len();
                        let label = if tab.search.query.is_empty() {
                            String::new()
                        } else if count == 0 {
                            if tab.search.pending {
                                "searching…".into()
                            } else {
                                "no matches".into()
                            }
                        } else {
                            format!("{} / {}", tab.search.current + 1, count)
                        };
                        ui.label(RichText::new(label).weak().size(12.0));
                        if chrome_button(ui, "Prev", false).clicked() {
                            prev = true;
                        }
                        if chrome_button(ui, "Next", false).clicked() {
                            next = true;
                        }
                        if chrome_button(ui, "×", false).clicked() {
                            close = true;
                        }
                        if edit.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
                            if ui.input(|input| input.modifiers.shift) {
                                prev = true;
                            } else {
                                next = true;
                            }
                        }
                        if edit.has_focus() && ui.input(|input| input.key_pressed(Key::Escape)) {
                            close = true;
                        }
                    },
                );
                if !tab.search.hits.is_empty() {
                    ui.add_space(2.0);
                    let query = tab.search.query.clone();
                    let current = tab.search.current;
                    let hits: Vec<(usize, Option<f32>)> = tab
                        .search
                        .hits
                        .iter()
                        .map(|(page, quads)| (*page, quads.first().map(|q| q.y0)))
                        .collect();
                    ScrollArea::vertical()
                        .max_height(SEARCH_HIT_MAX_ROWS as f32 * SEARCH_HIT_ROW_H)
                        .show(ui, |ui| {
                            for (index, (page, y)) in hits.iter().enumerate() {
                                let selected = index == current;
                                let label = if query.is_empty() {
                                    format!("page {}", page + 1)
                                } else {
                                    format!("page {} · {query}", page + 1)
                                };
                                let fill = if selected {
                                    theme::accent_fill(ui.ctx(), 36)
                                } else {
                                    Color32::TRANSPARENT
                                };
                                let text = if selected { colors.accent } else { colors.text };
                                let response = ui.add(
                                    Button::new(RichText::new(label).size(12.0).color(text))
                                        .fill(fill)
                                        .stroke(Stroke::NONE)
                                        .corner_radius(4.0)
                                        .min_size(Vec2::new(
                                            ui.available_width(),
                                            SEARCH_HIT_ROW_H,
                                        )),
                                );
                                if response.clicked() {
                                    tab.search.current = index;
                                    jump = Some((*page, *y));
                                }
                            }
                        });
                }
            });
        if case_changed {
            tab.search.current = 0;
            tab.search.hits.clear();
            tab.search.pending = !tab.search.query.trim().is_empty();
            tab.search.last_sent.clear();
        }
        if query_changed {
            tab.search.current = 0;
            tab.search.last_sent.clear();
        }
        if close {
            tab.search.open = false;
            tab.search.hits.clear();
            tab.search.query.clear();
            tab.search.last_sent.clear();
        }
    }
    if case_changed {
        app.settings.set_search_case_sensitive(case_flag);
    }
    if let Some((page, y)) = jump {
        app.queue_jump(page, y);
    }
    if next {
        app.search_step(1);
    } else if prev {
        app.search_step(-1);
    }
}

pub(crate) fn control_cluster(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let colors = palette(ui.ctx());
    let mut prepared = egui::Frame::new()
        .fill(colors.chrome_raised)
        .corner_radius(CornerRadius::same(5))
        .inner_margin(egui::Margin::symmetric(1, 1))
        .begin(ui);
    prepared.content_ui.spacing_mut().item_spacing = Vec2::new(0.0, 0.0);
    prepared.content_ui.spacing_mut().button_padding = Vec2::new(0.0, 0.0);
    prepared.content_ui.horizontal(|ui| {
        ui.set_height(CONTROL_H);
        add_contents(ui);
    });
    prepared.end(ui);
}

pub(crate) fn cluster_sep(ui: &mut egui::Ui) {
    let colors = palette(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, CONTROL_H - 6.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        (rect.top() - 1.0)..=(rect.bottom() + 1.0),
        Stroke::new(1.0_f32, colors.hairline),
    );
}

pub(crate) fn cluster_button(ui: &mut egui::Ui, label: &str, size: Vec2) -> egui::Response {
    let colors = palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if response.is_pointer_button_down_on() {
        theme::chrome_overlay(ui.ctx(), 28)
    } else if response.hovered() {
        theme::chrome_overlay(ui.ctx(), 16)
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(5), fill);
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::new(12.5, FontFamily::Proportional),
        colors.text,
    );
    let access_name = cluster_button_name(label);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), access_name));
    response
}

pub(crate) fn cluster_drag(ui: &mut egui::Ui, drag: DragValue<'_>, width: f32) -> egui::Response {
    ui.scope(|ui| {
        let hover = theme::chrome_overlay(ui.ctx(), 16);
        let active = theme::chrome_overlay(ui.ctx(), 28);
        {
            let visuals = ui.visuals_mut();
            visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
            visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            visuals.widgets.inactive.bg_stroke = Stroke::NONE;
            visuals.widgets.hovered.bg_fill = hover;
            visuals.widgets.hovered.weak_bg_fill = hover;
            visuals.widgets.hovered.bg_stroke = Stroke::NONE;
            visuals.widgets.active.bg_fill = active;
            visuals.widgets.active.weak_bg_fill = active;
            visuals.widgets.active.bg_stroke = Stroke::NONE;
            visuals.widgets.inactive.corner_radius = CornerRadius::same(5);
            visuals.widgets.hovered.corner_radius = CornerRadius::same(5);
            visuals.widgets.active.corner_radius = CornerRadius::same(5);
        }
        ui.spacing_mut().interact_size.y = CONTROL_H;
        ui.add_sized(Vec2::new(width, CONTROL_H), drag)
    })
    .inner
}

fn cheatsheet_window(app: &mut MarkerApp, ctx: &egui::Context) {
    if !app.cheatsheet_open {
        return;
    }
    let colors = palette(ctx);
    let mut open = app.cheatsheet_open;
    egui::Window::new("Keyboard shortcuts")
        .id(Id::new("marker-cheatsheet"))
        .open(&mut open)
        .resizable(true)
        .default_size(Vec2::new(420.0, 520.0))
        .min_width(320.0)
        .min_height(240.0)
        .frame(
            egui::Frame::new()
                .fill(colors.chrome_raised)
                .stroke(Stroke::new(1.0_f32, colors.hairline))
                .corner_radius(8.0)
                .inner_margin(12.0),
        )
        .show(ctx, |ui| {
            ui.label(
                RichText::new("Press ? again or Esc to close")
                    .weak()
                    .size(12.0),
            );
            ui.add_space(8.0);
            ScrollArea::vertical()
                .id_salt("cheatsheet-scroll")
                .show(ui, |ui| {
                    for section in ShortcutSection::ALL {
                        ui.label(RichText::new(section.label()).strong().size(13.0));
                        ui.add_space(4.0);
                        for row in SHORTCUTS.iter().filter(|row| row.section == section) {
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    Vec2::new(168.0, 18.0),
                                    egui::Label::new(
                                        RichText::new(row.keys)
                                            .monospace()
                                            .size(12.0)
                                            .color(colors.accent),
                                    ),
                                );
                                ui.label(RichText::new(row.action).size(12.0));
                            });
                        }
                        ui.add_space(10.0);
                    }
                });
        });
    app.cheatsheet_open = open;
}

fn preset_editor_window(app: &mut MarkerApp, ctx: &egui::Context) {
    if !app.preset_editor_open {
        return;
    }
    let colors = palette(ctx);
    let mut open = app.preset_editor_open;
    let mut dirty = false;
    let mut reset = false;
    let mut add = false;
    let mut remove: Option<usize> = None;
    egui::Window::new("Highlight presets")
        .id(Id::new("marker-highlight-presets"))
        .open(&mut open)
        .resizable(true)
        .default_size(Vec2::new(420.0, 320.0))
        .min_width(340.0)
        .min_height(200.0)
        .frame(
            egui::Frame::new()
                .fill(colors.chrome_raised)
                .stroke(Stroke::new(1.0_f32, colors.hairline))
                .corner_radius(8.0)
                .inner_margin(12.0),
        )
        .show(ctx, |ui| {
            ui.label(
                RichText::new("Exact RGB match names highlights. Unknown colors stay Custom.")
                    .weak()
                    .size(12.0),
            );
            ui.add_space(8.0);
            let can_add = app.settings.highlight_presets.len() < MAX_HIGHLIGHT_PRESETS;
            let can_remove = app.settings.highlight_presets.len() > 1;
            ScrollArea::vertical()
                .id_salt("highlight-preset-scroll")
                .show(ui, |ui| {
                    for i in 0..app.settings.highlight_presets.len() {
                        ui.horizontal(|ui| {
                            let preset = &mut app.settings.highlight_presets[i];
                            let swatch = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                            ui.painter().circle_filled(
                                swatch.0.center(),
                                7.0,
                                preset.rgb.to_color32(),
                            );
                            let name_edit = ui.add(
                                TextEdit::singleline(&mut preset.name)
                                    .desired_width(140.0)
                                    .hint_text("Name"),
                            );
                            if name_edit.changed() {
                                dirty = true;
                            }
                            ui.label(RichText::new("R").weak().size(11.0));
                            let mut r = preset.rgb.r as i32;
                            if ui
                                .add(DragValue::new(&mut r).range(0..=255).speed(1.0))
                                .changed()
                            {
                                preset.rgb.r = r as u8;
                                dirty = true;
                            }
                            ui.label(RichText::new("G").weak().size(11.0));
                            let mut g = preset.rgb.g as i32;
                            if ui
                                .add(DragValue::new(&mut g).range(0..=255).speed(1.0))
                                .changed()
                            {
                                preset.rgb.g = g as u8;
                                dirty = true;
                            }
                            ui.label(RichText::new("B").weak().size(11.0));
                            let mut b = preset.rgb.b as i32;
                            if ui
                                .add(DragValue::new(&mut b).range(0..=255).speed(1.0))
                                .changed()
                            {
                                preset.rgb.b = b as u8;
                                dirty = true;
                            }
                            if can_remove
                                && ui
                                    .add(
                                        Button::new(RichText::new("×").size(12.0))
                                            .fill(Color32::TRANSPARENT)
                                            .stroke(Stroke::NONE),
                                    )
                                    .on_hover_text("Remove preset")
                                    .clicked()
                            {
                                remove = Some(i);
                            }
                        });
                        ui.add_space(4.0);
                    }
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(can_add, Button::new("Add"))
                    .on_hover_text(format!("Up to {MAX_HIGHLIGHT_PRESETS} presets"))
                    .clicked()
                {
                    add = true;
                }
                if ui
                    .button("Reset to defaults")
                    .on_hover_text(
                        "Restore the five built-in named colors and the default highlight swatch",
                    )
                    .clicked()
                {
                    reset = true;
                }
            });
        });

    if reset {
        app.settings.reset_highlight_presets();
        dirty = true;
    }
    if add && app.settings.highlight_presets.len() < MAX_HIGHLIGHT_PRESETS {
        app.settings.highlight_presets.push(HighlightPreset {
            name: "Custom".into(),
            rgb: app.settings.highlight_color,
        });
        dirty = true;
    }
    if let Some(i) = remove {
        if app.settings.highlight_presets.len() > 1 && i < app.settings.highlight_presets.len() {
            app.settings.highlight_presets.remove(i);
            dirty = true;
        }
    }
    if dirty {
        if let Some(first) = app.settings.highlight_presets.first() {
            if !app
                .settings
                .highlight_presets
                .iter()
                .any(|p| p.rgb == app.settings.highlight_color)
            {
                app.settings.highlight_color = first.rgb;
            }
        }
        app.settings.save();
    }
    app.preset_editor_open = open;
}

fn outline_panel(app: &mut MarkerApp, ctx: &egui::Context) {
    let colors = palette(ctx);
    let Some(tab) = app.tab() else {
        return;
    };
    if !tab.outline_open {
        return;
    }
    let outline = tab.doc.outline.clone();
    let mut jump = None;
    egui::SidePanel::left("outline")
        .resizable(true)
        .default_width(240.0)
        .width_range(180.0..=420.0)
        .frame(
            egui::Frame::new()
                .fill(colors.chrome)
                .stroke(Stroke::new(1.0_f32, colors.hairline))
                .inner_margin(10.0),
        )
        .show(ctx, |ui| {
            ui.label(RichText::new("Outline").strong().size(14.0));
            ui.add_space(6.0);
            ScrollArea::vertical().show(ui, |ui| {
                if outline.is_empty() {
                    ui.label(RichText::new("No outline in this PDF.").weak());
                }
                for node in &outline {
                    outline_node(ui, node, 0, &mut jump);
                }
            });
        });
    if let Some((page, y)) = jump {
        app.queue_jump(page, y);
    }
}

const THUMB_PANEL_IMG_W: f32 = 120.0;
const THUMB_LOOKAHEAD: usize = 2;

fn thumbs_panel(app: &mut MarkerApp, ctx: &egui::Context) {
    let colors = palette(ctx);
    let view_h = app.view_rect.height().max(1.0);
    let Some((page_count, current, aspects, textures)) = (|| {
        let tab = app.tab()?;
        if !tab.thumbs_open {
            return None;
        }
        let page_count = tab.doc.pages.len();
        let current = tab.doc.current_page(view_h);
        let aspects: Vec<f32> = tab
            .doc
            .pages
            .iter()
            .map(|info| {
                let w = info.width().max(1.0);
                info.height().max(1.0) / w
            })
            .collect();
        // At most THUMB_CACHE_PAGES handles; cheap Arc clones.
        let textures: std::collections::HashMap<usize, egui::TextureHandle> = tab
            .thumbs
            .iter()
            .map(|(&page, thumb)| (page, thumb.texture.clone()))
            .collect();
        Some((page_count, current, aspects, textures))
    })() else {
        return;
    };
    let mut jump: Option<usize> = None;
    let mut close = false;
    let mut wanted = Vec::new();

    egui::SidePanel::left("thumbs")
        .resizable(true)
        .default_width(148.0)
        .width_range(132.0..=280.0)
        .frame(
            egui::Frame::new()
                .fill(colors.chrome)
                .stroke(Stroke::new(1.0_f32, colors.hairline))
                .inner_margin(8.0),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Pages").strong().size(14.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .small_button("×")
                        .on_hover_text("Hide page thumbnails")
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
            ui.add_space(4.0);
            ScrollArea::vertical()
                .id_salt("thumbs-list")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if page_count == 0 {
                        ui.label(RichText::new("No pages").weak());
                        return;
                    }
                    let clip = ui.clip_rect();
                    for page in 0..page_count {
                        let aspect = aspects.get(page).copied().unwrap_or(1.414);
                        let img_h = THUMB_PANEL_IMG_W * aspect;
                        let row_h = img_h + 22.0;
                        let (row_rect, response) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), row_h),
                            Sense::click(),
                        );
                        if clip.intersects(row_rect) {
                            wanted.push(page);
                        }
                        let selected = page == current;
                        let fill = if selected {
                            theme::accent_fill(ui.ctx(), 28)
                        } else {
                            Color32::TRANSPARENT
                        };
                        ui.painter().rect_filled(row_rect, 4.0, fill);
                        let img_rect = Rect::from_min_size(
                            Pos2::new(
                                row_rect.center().x - THUMB_PANEL_IMG_W * 0.5,
                                row_rect.top() + 2.0,
                            ),
                            Vec2::new(THUMB_PANEL_IMG_W, img_h),
                        );
                        ui.painter().rect_filled(img_rect, 2.0, Color32::WHITE);
                        if let Some(texture) = textures.get(&page) {
                            ui.painter().image(
                                texture.id(),
                                img_rect,
                                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        } else {
                            ui.painter().rect_stroke(
                                img_rect,
                                2.0,
                                Stroke::new(1.0_f32, colors.hairline),
                                StrokeKind::Inside,
                            );
                        }
                        let label = format!("{}", page + 1);
                        let label_pos = Pos2::new(row_rect.center().x, img_rect.bottom() + 2.0);
                        ui.painter().text(
                            label_pos,
                            Align2::CENTER_TOP,
                            label,
                            FontId::proportional(11.0),
                            if selected { colors.accent } else { colors.text },
                        );
                        if response.clicked() {
                            jump = Some(page);
                        }
                        response.on_hover_text(format!("Go to page {}", page + 1));
                    }
                });
        });

    if close {
        if let Some(tab) = app.tab_mut() {
            tab.thumbs_open = false;
        }
        app.request_thumbs(&[]);
        return;
    }

    // Lookahead around the visible rows so scrolling stays smooth.
    let mut request = wanted;
    if let (Some(first), Some(last)) = (request.first().copied(), request.last().copied()) {
        let lo = first.saturating_sub(THUMB_LOOKAHEAD);
        let hi = (last + THUMB_LOOKAHEAD).min(page_count.saturating_sub(1));
        request = (lo..=hi).collect();
    }
    app.request_thumbs(&request);
    if let Some(page) = jump {
        app.queue_jump(page, Some(0.0));
    }
}

fn annot_sidebar(app: &mut MarkerApp, ctx: &egui::Context) {
    let colors = palette(ctx);
    let presets = app.settings.highlight_presets.clone();
    let Some(tab) = app.tab() else {
        return;
    };
    if !tab.annots_open {
        return;
    }

    let rows = collect_annot_sidebar_rows(tab);
    let total = tab.doc.session.annotations.len();
    let selected = tab.selected.clone();
    let mut filter = tab.annot_sidebar.query.clone();
    let mut kind_filters = tab.annot_sidebar.kinds.clone();
    let mut color_filters = tab.annot_sidebar.colors.clone();
    let mut activate: Option<u64> = None;
    let mut close = false;

    egui::SidePanel::left("annot-sidebar")
        .resizable(true)
        .default_width(280.0)
        .width_range(220.0..=480.0)
        .frame(
            egui::Frame::new()
                .fill(colors.chrome)
                .stroke(Stroke::new(1.0_f32, colors.hairline))
                .inner_margin(10.0),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Annotations").strong().size(14.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .small_button("×")
                        .on_hover_text("Hide annotation list")
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
            ui.add_space(4.0);

            let search = ui.add(
                TextEdit::singleline(&mut filter)
                    .hint_text("Search notes…")
                    .desired_width(f32::INFINITY),
            );
            search.on_hover_text("Match text inside notes, equations, and highlights");
            ui.add_space(4.0);

            ui.label(RichText::new("Kind").weak().size(11.0));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
                for kind in AnnotFilterKind::ALL {
                    let on = kind_filters.contains(&kind);
                    if filter_chip(ui, kind.label(), on).clicked() {
                        if on {
                            kind_filters.remove(&kind);
                        } else {
                            kind_filters.insert(kind);
                        }
                    }
                }
            });
            ui.add_space(4.0);

            ui.label(RichText::new("Color").weak().size(11.0));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
                for preset in &presets {
                    let on = color_filters.contains(&preset.rgb);
                    if color_dot(ui, preset.rgb, on, &preset.name) {
                        if on {
                            color_filters.retain(|c| *c != preset.rgb);
                        } else {
                            color_filters.push(preset.rgb);
                        }
                    }
                }
            });
            ui.add_space(6.0);

            let query = filter.trim().to_ascii_lowercase();
            let shown: Vec<&AnnotSidebarRow> = rows
                .iter()
                .filter(|row| annot_row_matches(row, &kind_filters, &color_filters, &query))
                .collect();

            ScrollArea::vertical()
                .id_salt("annot-sidebar-list")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if total == 0 {
                        ui.label(RichText::new("No annotations").weak());
                        return;
                    }
                    if shown.is_empty() {
                        ui.label(RichText::new("No matching annotations").weak());
                        return;
                    }
                    for row in shown {
                        let is_sel = selected.contains(&row.id);
                        if annot_sidebar_row(ui, row, is_sel).clicked() {
                            activate = Some(row.id);
                        }
                    }
                });
        });

    if let Some(tab) = app.tab_mut() {
        tab.annot_sidebar.query = filter;
        tab.annot_sidebar.kinds = kind_filters;
        tab.annot_sidebar.colors = color_filters;
        if close {
            tab.annots_open = false;
        }
    }
    if let Some(id) = activate {
        if let Some(tab) = app.tab_mut() {
            let (page, y) = tab
                .doc
                .session
                .get(id)
                .map(|annot| (annot.page, annot.bounds().map(|b| b.y0)))
                .unwrap_or((0, None));
            tab.select_only(id);
            tab.pending_jump = Some((page, y));
        }
    }
}

fn review_panel(app: &mut MarkerApp, ctx: &egui::Context) {
    let colors = palette(ctx);
    let presets = app.settings.highlight_presets.clone();
    let Some(tab) = app.tab() else {
        return;
    };
    if !tab.review_open {
        return;
    }

    let rows = collect_review_rows(tab, &presets);
    let outline = tab.doc.outline.clone();
    let mut group_by = tab.review.group_by;
    let mut hide_text = tab.review.hide_text;
    let revealed = tab.review.revealed.clone();
    let mut activate: Option<u64> = None;
    let mut reveal: Option<u64> = None;
    let mut close = false;
    let mut clear_revealed = false;

    // Empty outline falls back to preset grouping (ST4 pitfall).
    let outline_starts = flatten_outline_starts(&outline);
    let use_outline = group_by == ReviewGroupMode::Outline && !outline_starts.is_empty();
    let groups = group_review_rows(&rows, use_outline, &outline_starts);

    egui::SidePanel::left("review-panel")
        .resizable(true)
        .default_width(300.0)
        .width_range(220.0..=480.0)
        .frame(
            egui::Frame::new()
                .fill(colors.chrome)
                .stroke(Stroke::new(1.0_f32, colors.hairline))
                .inner_margin(10.0),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Review").strong().size(14.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("×").on_hover_text("Hide review").clicked() {
                        close = true;
                    }
                });
            });
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if filter_chip(ui, "Preset", group_by == ReviewGroupMode::Preset).clicked() {
                    group_by = ReviewGroupMode::Preset;
                }
                if filter_chip(ui, "Outline", group_by == ReviewGroupMode::Outline).clicked() {
                    group_by = ReviewGroupMode::Outline;
                }
            });
            if group_by == ReviewGroupMode::Outline && outline_starts.is_empty() {
                ui.label(
                    RichText::new("No outline — grouping by preset.")
                        .size(11.0)
                        .weak(),
                );
            }
            ui.add_space(2.0);
            let hide_was = hide_text;
            if ui
                .checkbox(&mut hide_text, "Hide text")
                .on_hover_text("Show only page and preset until a row is clicked")
                .changed()
                && hide_text
                && !hide_was
            {
                clear_revealed = true;
            }
            ui.add_space(6.0);

            ScrollArea::vertical()
                .id_salt("review-panel-list")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if rows.is_empty() {
                        ui.label(RichText::new("No highlights or notes").weak());
                        return;
                    }
                    for (heading, items) in &groups {
                        ui.add_space(4.0);
                        ui.label(RichText::new(heading).strong().size(12.0));
                        ui.add_space(2.0);
                        for row in items {
                            let show_quote = !hide_text || revealed.contains(&row.id);
                            let response = review_row(ui, row, show_quote);
                            if response.clicked() {
                                if hide_text && !show_quote {
                                    reveal = Some(row.id);
                                } else {
                                    activate = Some(row.id);
                                }
                            }
                        }
                    }
                });
        });

    if let Some(tab) = app.tab_mut() {
        tab.review.group_by = group_by;
        tab.review.hide_text = hide_text;
        if clear_revealed {
            tab.review.revealed.clear();
        }
        if let Some(id) = reveal {
            tab.review.revealed.insert(id);
        }
        if close {
            tab.review_open = false;
        }
    }
    if let Some(id) = activate {
        if let Some(tab) = app.tab_mut() {
            let (page, y) = tab
                .doc
                .session
                .get(id)
                .map(|annot| (annot.page, annot.bounds().map(|b| b.y0)))
                .unwrap_or((0, None));
            tab.select_only(id);
            tab.pending_jump = Some((page, y));
        }
    }
}

struct ReviewRow {
    id: u64,
    page: usize,
    preset: String,
    kind_label: &'static str,
    color: Option<Rgb>,
    quote: String,
}

fn collect_review_rows(tab: &Tab, presets: &[HighlightPreset]) -> Vec<ReviewRow> {
    tab.doc
        .session
        .annotations
        .iter()
        .filter_map(|annot| review_row_data(tab, annot, presets))
        .collect()
}

fn review_row_data(
    tab: &Tab,
    annot: &Annotation,
    presets: &[HighlightPreset],
) -> Option<ReviewRow> {
    let (kind_label, color, quote) = match &annot.kind {
        AnnotKind::Highlight { quads, color } | AnnotKind::Markup { quads, color, .. } => {
            let quote = if let Some(glyphs) = tab.doc.glyphs.get(&annot.page) {
                let indices = glyphs_intersecting_rects(glyphs, quads);
                if let (Some(&lo), Some(&hi)) = (indices.first(), indices.last()) {
                    reconstruct_text(glyphs, lo, hi)
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            (annot_kind_label(&annot.kind), Some(*color), quote)
        }
        AnnotKind::Note { content, color, .. } => ("Note", Some(*color), content.clone()),
        AnnotKind::Text { .. }
        | AnnotKind::Shape { .. }
        | AnnotKind::Math { .. }
        | AnnotKind::Image { .. }
        | AnnotKind::Foreign { .. } => return None,
    };
    let preset = color
        .map(|c| highlight_color_name(presets, c).to_owned())
        .unwrap_or_else(|| "Custom".into());
    Some(ReviewRow {
        id: annot.id,
        page: annot.page,
        preset,
        kind_label,
        color,
        quote: one_line_snippet(&quote, 120),
    })
}

/// DFS order of outline nodes that have a destination page.
fn flatten_outline_starts(nodes: &[OutlineNode]) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    flatten_outline_starts_into(nodes, &mut out);
    out
}

fn flatten_outline_starts_into(nodes: &[OutlineNode], out: &mut Vec<(String, usize)>) {
    for node in nodes {
        if let Some(page) = node.page {
            out.push((node.title.clone(), page));
        }
        flatten_outline_starts_into(&node.children, out);
    }
}

/// Last outline entry whose start page is ≤ `page`, else "Front matter".
fn outline_bucket_for(starts: &[(String, usize)], page: usize) -> String {
    let mut best: Option<&str> = None;
    let mut best_page = 0usize;
    for (title, start) in starts {
        if *start <= page && (best.is_none() || *start >= best_page) {
            best = Some(title.as_str());
            best_page = *start;
        }
    }
    best.unwrap_or("Front matter").to_owned()
}

fn group_review_rows<'a>(
    rows: &'a [ReviewRow],
    by_outline: bool,
    outline_starts: &[(String, usize)],
) -> Vec<(String, Vec<&'a ReviewRow>)> {
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::HashMap<String, Vec<&ReviewRow>> =
        std::collections::HashMap::new();
    for row in rows {
        let key = if by_outline {
            outline_bucket_for(outline_starts, row.page)
        } else {
            row.preset.clone()
        };
        if !map.contains_key(&key) {
            order.push(key.clone());
        }
        map.entry(key).or_default().push(row);
    }
    order
        .into_iter()
        .filter_map(|key| {
            let items = map.remove(&key)?;
            Some((key, items))
        })
        .collect()
}

fn review_row(ui: &mut egui::Ui, row: &ReviewRow, show_quote: bool) -> egui::Response {
    let colors = palette(ui.ctx());
    let available = ui.available_width();
    let prompt = format!("p.{} · {}", row.page + 1, row.preset);
    let response = egui::Frame::new()
        .fill(Color32::TRANSPARENT)
        .corner_radius(5.0)
        .inner_margin(egui::Margin::symmetric(6, 5))
        .show(ui, |ui| {
            ui.set_width(available - 4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (swatch, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                if let Some(color) = row.color {
                    ui.painter()
                        .circle_filled(swatch.center(), 5.0, color.to_color32());
                } else {
                    ui.painter().circle_stroke(
                        swatch.center(),
                        5.0,
                        Stroke::new(1.0_f32, colors.hairline),
                    );
                }
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&prompt).size(12.5).strong());
                        ui.label(RichText::new(row.kind_label).size(11.0).weak());
                    });
                    if show_quote {
                        let quote = if row.quote.is_empty() {
                            "—"
                        } else {
                            row.quote.as_str()
                        };
                        ui.label(
                            RichText::new(quote)
                                .size(11.5)
                                .color(colors.text.gamma_multiply(0.85)),
                        );
                    } else {
                        ui.label(RichText::new("Click to reveal").size(11.0).italics().weak());
                    }
                });
            });
        })
        .response
        .interact(Sense::click());
    if show_quote && !row.quote.is_empty() {
        response.clone().on_hover_text(&row.quote);
    } else if !show_quote {
        response.clone().on_hover_text("Click to reveal quote");
    }
    response
}

struct AnnotSidebarRow {
    id: u64,
    page: usize,
    kind: AnnotFilterKind,
    kind_label: &'static str,
    color: Option<Rgb>,
    snippet: String,
    read_only: bool,
}

fn collect_annot_sidebar_rows(tab: &Tab) -> Vec<AnnotSidebarRow> {
    tab.doc
        .session
        .annotations
        .iter()
        .filter_map(|annot| annot_sidebar_row_data(tab, annot))
        .collect()
}

fn annot_sidebar_row_data(tab: &Tab, annot: &Annotation) -> Option<AnnotSidebarRow> {
    let kind = AnnotFilterKind::from_annot(&annot.kind)?;
    Some(AnnotSidebarRow {
        id: annot.id,
        page: annot.page,
        kind,
        kind_label: annot_kind_label(&annot.kind),
        color: annot_display_color(&annot.kind),
        snippet: annot_snippet(tab, annot),
        read_only: matches!(annot.kind, AnnotKind::Foreign { .. }),
    })
}

fn annot_row_matches(
    row: &AnnotSidebarRow,
    kinds: &std::collections::HashSet<AnnotFilterKind>,
    colors: &[Rgb],
    query: &str,
) -> bool {
    if !kinds.is_empty() && !kinds.contains(&row.kind) {
        return false;
    }
    if !colors.is_empty() {
        let Some(color) = row.color else {
            return false;
        };
        if !colors.contains(&color) {
            return false;
        }
    }
    if query.is_empty() {
        return true;
    }
    let hay = format!(
        "{} {} {}",
        row.kind_label,
        row.snippet,
        if row.read_only {
            "read-only imported"
        } else {
            ""
        }
    )
    .to_ascii_lowercase();
    hay.contains(query)
}

fn annot_sidebar_row(ui: &mut egui::Ui, row: &AnnotSidebarRow, selected: bool) -> egui::Response {
    let colors = palette(ui.ctx());
    let fill = if selected {
        theme::accent_fill(ui.ctx(), 36)
    } else {
        Color32::TRANSPARENT
    };
    let available = ui.available_width();
    let response = egui::Frame::new()
        .fill(fill)
        .corner_radius(5.0)
        .inner_margin(egui::Margin::symmetric(6, 5))
        .show(ui, |ui| {
            ui.set_width(available - 4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (swatch, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                if let Some(color) = row.color {
                    ui.painter()
                        .circle_filled(swatch.center(), 5.0, color.to_color32());
                } else {
                    ui.painter().circle_stroke(
                        swatch.center(),
                        5.0,
                        Stroke::new(1.0_f32, colors.hairline),
                    );
                }
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(row.kind_label).size(12.5).strong());
                        ui.label(
                            RichText::new(format!("p.{}", row.page + 1))
                                .size(11.0)
                                .weak(),
                        );
                        if row.read_only {
                            ui.label(RichText::new("read-only").size(10.5).weak());
                        }
                    });
                    let snippet = if row.snippet.is_empty() {
                        "—"
                    } else {
                        row.snippet.as_str()
                    };
                    ui.label(
                        RichText::new(snippet)
                            .size(11.5)
                            .color(colors.text.gamma_multiply(0.85)),
                    );
                });
            });
        })
        .response
        .interact(Sense::click());
    if !row.snippet.is_empty() {
        response.clone().on_hover_text(&row.snippet);
    }
    response
}

fn filter_chip(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let colors = palette(ui.ctx());
    let fill = if selected {
        theme::accent_fill(ui.ctx(), 42)
    } else {
        Color32::TRANSPARENT
    };
    let color = if selected { colors.accent } else { colors.text };
    ui.add(
        Button::new(RichText::new(label).size(11.0).color(color))
            .fill(fill)
            .stroke(Stroke::new(
                1.0_f32,
                if selected {
                    colors.accent.gamma_multiply(0.55)
                } else {
                    colors.hairline
                },
            ))
            .corner_radius(5.0),
    )
}

fn annot_display_color(kind: &AnnotKind) -> Option<Rgb> {
    match kind {
        AnnotKind::Highlight { color, .. }
        | AnnotKind::Markup { color, .. }
        | AnnotKind::Text { color, .. }
        | AnnotKind::Note { color, .. }
        | AnnotKind::Math { color, .. }
        | AnnotKind::Foreign { color, .. } => Some(*color),
        AnnotKind::Shape { stroke, .. } => Some(*stroke),
        AnnotKind::Image { .. } => None,
    }
}

fn annot_kind_label(kind: &AnnotKind) -> &'static str {
    match kind {
        AnnotKind::Highlight { .. } => "Highlight",
        AnnotKind::Markup { style, .. } => match style {
            MarkupStyle::Underline => "Underline",
            MarkupStyle::StrikeOut => "Strikeout",
            MarkupStyle::Squiggly => "Squiggly",
        },
        AnnotKind::Text { .. } => "Text",
        AnnotKind::Note { .. } => "Note",
        AnnotKind::Math { .. } => "Equation",
        AnnotKind::Shape {
            kind: ShapeKind::Rect,
            ..
        } => "Rectangle",
        AnnotKind::Shape {
            kind: ShapeKind::Ellipse,
            ..
        } => "Ellipse",
        AnnotKind::Shape {
            kind: ShapeKind::Line,
            ..
        } => "Line",
        AnnotKind::Image { .. } => "Image",
        AnnotKind::Foreign { kind, .. } => match kind {
            ForeignKind::Underline => "Underline",
            ForeignKind::StrikeOut => "Strikeout",
            ForeignKind::Squiggly => "Squiggly",
            ForeignKind::Ink => "Ink",
            ForeignKind::Polygon => "Polygon",
            ForeignKind::PolyLine => "Polyline",
            ForeignKind::Caret => "Caret",
            ForeignKind::FileAttachment => "Attachment",
        },
    }
}

fn annot_snippet(tab: &Tab, annot: &Annotation) -> String {
    let raw = match &annot.kind {
        AnnotKind::Text { content, .. } | AnnotKind::Note { content, .. } => content.clone(),
        AnnotKind::Math { source, .. } => source.clone(),
        AnnotKind::Foreign { contents, .. } => contents.clone(),
        AnnotKind::Highlight { quads, .. } | AnnotKind::Markup { quads, .. } => {
            if let Some(glyphs) = tab.doc.glyphs.get(&annot.page) {
                let indices = glyphs_intersecting_rects(glyphs, quads);
                if let (Some(&lo), Some(&hi)) = (indices.first(), indices.last()) {
                    reconstruct_text(glyphs, lo, hi)
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        }
        AnnotKind::Shape { .. } | AnnotKind::Image { .. } => String::new(),
    };
    one_line_snippet(&raw, 72)
}

fn one_line_snippet(text: &str, max_chars: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let mut out: String = collapsed
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect();
    out.push('…');
    out
}

pub(crate) fn chrome_button(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let colors = palette(ui.ctx());
    let fill = if selected {
        theme::accent_fill(ui.ctx(), 42)
    } else {
        Color32::TRANSPARENT
    };
    let color = if selected { colors.accent } else { colors.text };
    ui.add(
        Button::new(RichText::new(label).size(12.0).color(color))
            .fill(fill)
            .stroke(Stroke::NONE)
            .corner_radius(5.0),
    )
}

pub(crate) fn color_dot(ui: &mut egui::Ui, color: Rgb, selected: bool, name: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::click());
    ui.painter()
        .circle_filled(rect.center(), 5.0, color.to_color32());
    if selected {
        ui.painter().circle_stroke(
            rect.center(),
            6.5,
            Stroke::new(1.35_f32, theme::color_dot_selection_ring(ui.ctx())),
        );
    }
    response.widget_info(|| {
        WidgetInfo::selected(WidgetType::ColorButton, ui.is_enabled(), selected, name)
    });
    response.on_hover_text(name).clicked()
}

pub(crate) fn ink_color_name(color: Rgb) -> &'static str {
    match color {
        c if c == INK_COLORS[0] => "Black ink",
        c if c == INK_COLORS[1] => "Red ink",
        c if c == INK_COLORS[2] => "Blue ink",
        c if c == INK_COLORS[3] => "Green ink",
        _ => "Color",
    }
}

/// Exact RGB → preset name; unknown RGB is shown as Custom.
pub(crate) fn highlight_color_name(presets: &[HighlightPreset], color: Rgb) -> &str {
    // Same exact-match rule as `Settings::highlight_name_for`.
    presets
        .iter()
        .find(|preset| preset.rgb == color)
        .map(|preset| preset.name.as_str())
        .unwrap_or("Custom")
}

fn cluster_button_name(label: &str) -> &str {
    match label {
        "Fit" => "Fit width",
        "+" => "Zoom in",
        "−" => "Zoom out",
        "+Page" => "Insert blank page",
        _ => label,
    }
}

pub(crate) fn apply_theme(ctx: &egui::Context) {
    install_fonts(ctx);
    theme::apply_theme(ctx);
    ctx.all_styles_mut(|style| {
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.text_styles.insert(
            egui::TextStyle::Body,
            FontId::new(13.5, FontFamily::Proportional),
        );
    });
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "InstrumentSerif-Italic".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/fonts/InstrumentSerif-Italic.ttf"
        ))),
    );
    let mut brand = vec!["InstrumentSerif-Italic".to_owned()];
    brand.extend(fonts.families[&FontFamily::Proportional].clone());
    fonts
        .families
        .insert(FontFamily::Name(BRAND_FAMILY.into()), brand);
    ctx.set_fonts(fonts);
}

fn brand_family() -> FontFamily {
    FontFamily::Name(BRAND_FAMILY.into())
}

/// Vector brand mark (page + highlighter), matching `packaging/marker.svg`.
pub(crate) fn brand_mark(ui: &mut egui::Ui, size: f32) {
    let colors = palette(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let painter = ui.painter();
    let radius = (size * 0.22).clamp(6.0, 18.0);
    let chassis = match ui.ctx().theme() {
        egui::Theme::Dark => Color32::from_rgb(26, 26, 30),
        egui::Theme::Light => Color32::from_rgb(32, 32, 38),
    };
    painter.rect_filled(rect, CornerRadius::same(radius as u8), chassis);

    let page = Rect::from_min_max(
        Pos2::new(rect.left() + size * 0.27, rect.top() + size * 0.17),
        Pos2::new(rect.left() + size * 0.73, rect.top() + size * 0.83),
    );
    painter.rect_filled(
        page,
        CornerRadius::same((size * 0.05).clamp(2.0, 6.0) as u8),
        Color32::from_rgb(251, 250, 246),
    );

    // Accent dog-ear / tab.
    let accent = [
        Pos2::new(page.left() - size * 0.04, page.top() + size * 0.04),
        Pos2::new(page.left() + size * 0.14, page.top() + size * 0.04),
        Pos2::new(page.left() - size * 0.04, page.top() + size * 0.22),
    ];
    painter.add(egui::Shape::convex_polygon(
        accent.to_vec(),
        colors.accent,
        Stroke::NONE,
    ));

    // Highlighter stroke across the page.
    let mark = [
        Pos2::new(rect.left() + size * 0.20, rect.top() + size * 0.70),
        Pos2::new(rect.left() + size * 0.72, rect.top() + size * 0.36),
        Pos2::new(rect.left() + size * 0.80, rect.top() + size * 0.44),
        Pos2::new(rect.left() + size * 0.28, rect.top() + size * 0.78),
    ];
    painter.add(egui::Shape::convex_polygon(
        mark.to_vec(),
        Color32::from_rgb(240, 191, 58),
        Stroke::NONE,
    ));
    let tip = [
        Pos2::new(rect.left() + size * 0.70, rect.top() + size * 0.33),
        Pos2::new(rect.left() + size * 0.82, rect.top() + size * 0.42),
        Pos2::new(rect.left() + size * 0.77, rect.top() + size * 0.47),
        Pos2::new(rect.left() + size * 0.65, rect.top() + size * 0.38),
    ];
    painter.add(egui::Shape::convex_polygon(
        tip.to_vec(),
        Color32::from_rgb(255, 246, 210),
        Stroke::NONE,
    ));
}

pub(crate) fn brand_wordmark(ui: &mut egui::Ui) {
    let colors = palette(ui.ctx());
    brand_mark(ui, 64.0);
    ui.add_space(10.0);
    let font = FontId::new(72.0, brand_family());
    let galley = ui.fonts_mut(|fonts| fonts.layout_no_wrap("Marker".to_owned(), font, colors.text));
    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
    let text_pos = Pos2::new(rect.center().x - galley.size().x * 0.5, rect.top());
    ui.painter().galley(text_pos, galley, colors.text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use crate::geom::HIGHLIGHT_COLORS;

    #[test]
    fn highlight_color_name_is_exact_rgb_or_custom() {
        let presets = crate::settings::default_highlight_presets();
        assert_eq!(
            highlight_color_name(&presets, HIGHLIGHT_COLORS[0]),
            "Key idea"
        );
        assert_eq!(highlight_color_name(&presets, Rgb::new(1, 2, 3)), "Custom");
    }

    #[test]
    fn outline_bucket_picks_last_start_at_or_before_page() {
        let starts = vec![
            ("Intro".into(), 0usize),
            ("Middle".into(), 3usize),
            ("End".into(), 8usize),
        ];
        assert_eq!(outline_bucket_for(&starts, 0), "Intro");
        assert_eq!(outline_bucket_for(&starts, 2), "Intro");
        assert_eq!(outline_bucket_for(&starts, 3), "Middle");
        assert_eq!(outline_bucket_for(&starts, 9), "End");
        assert_eq!(outline_bucket_for(&[], 1), "Front matter");
    }

    #[test]
    fn flatten_outline_starts_is_depth_first() {
        let outline = vec![OutlineNode {
            title: "A".into(),
            page: Some(0),
            y: None,
            children: vec![OutlineNode {
                title: "A.1".into(),
                page: Some(2),
                y: None,
                children: Vec::new(),
            }],
        }];
        let flat = flatten_outline_starts(&outline);
        assert_eq!(flat, vec![("A".into(), 0), ("A.1".into(), 2)]);
    }

    #[test]
    fn group_review_rows_by_preset_and_outline() {
        let rows = vec![
            ReviewRow {
                id: 1,
                page: 0,
                preset: "Key idea".into(),
                kind_label: "Highlight",
                color: None,
                quote: "alpha".into(),
            },
            ReviewRow {
                id: 2,
                page: 4,
                preset: "Key idea".into(),
                kind_label: "Note",
                color: None,
                quote: "beta".into(),
            },
            ReviewRow {
                id: 3,
                page: 5,
                preset: "Question".into(),
                kind_label: "Highlight",
                color: None,
                quote: "gamma".into(),
            },
        ];
        let by_preset = group_review_rows(&rows, false, &[]);
        assert_eq!(by_preset.len(), 2);
        assert_eq!(by_preset[0].0, "Key idea");
        assert_eq!(by_preset[0].1.len(), 2);
        assert_eq!(by_preset[1].0, "Question");

        let starts = vec![("Ch1".into(), 0usize), ("Ch2".into(), 5usize)];
        let by_outline = group_review_rows(&rows, true, &starts);
        assert_eq!(by_outline.len(), 2);
        assert_eq!(by_outline[0].0, "Ch1");
        assert_eq!(by_outline[0].1.len(), 2);
        assert_eq!(by_outline[1].0, "Ch2");
        assert_eq!(by_outline[1].1.len(), 1);
    }

    #[test]
    fn one_line_snippet_collapses_and_truncates() {
        assert_eq!(one_line_snippet("hello\n  world", 72), "hello world");
        let long = "word ".repeat(40);
        let out = one_line_snippet(&long, 20);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 20);
    }

    #[test]
    fn annot_row_filter_matches_kind_color_and_query() {
        let row = AnnotSidebarRow {
            id: 1,
            page: 0,
            kind: AnnotFilterKind::Note,
            kind_label: "Note",
            color: Some(HIGHLIGHT_COLORS[0]),
            snippet: "key idea about commits".into(),
            read_only: false,
        };
        let mut kinds = HashSet::new();
        kinds.insert(AnnotFilterKind::Highlight);
        assert!(!annot_row_matches(&row, &kinds, &[], ""));
        kinds.insert(AnnotFilterKind::Note);
        assert!(annot_row_matches(&row, &kinds, &[], ""));
        assert!(!annot_row_matches(
            &row,
            &HashSet::new(),
            &[HIGHLIGHT_COLORS[1]],
            ""
        ));
        assert!(annot_row_matches(
            &row,
            &HashSet::new(),
            &[HIGHLIGHT_COLORS[0]],
            "commits"
        ));
        assert!(!annot_row_matches(
            &row,
            &HashSet::new(),
            &[HIGHLIGHT_COLORS[0]],
            "missing"
        ));
    }
}
