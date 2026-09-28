use egui::{
    Align, Button, Color32, DragValue, FontFamily, FontId, Key, Layout, RichText, ScrollArea,
    Sense, Stroke, TextEdit, TopBottomPanel, Vec2,
};

use crate::app::{MarkerApp, SaveState, Tool};
use crate::geom::{zoom_percent, HIGHLIGHT_COLORS, INK_COLORS, Rgb};
use crate::pdf::OutlineNode;

pub(crate) fn chrome(app: &mut MarkerApp, ctx: &egui::Context) {
    toolbar(app, ctx);
    tab_bar(app, ctx);
    search_bar(app, ctx);
    status_bar(app, ctx);
    outline_panel(app, ctx);
}

fn toolbar(app: &mut MarkerApp, ctx: &egui::Context) {
    TopBottomPanel::top("toolbar")
        .exact_height(38.0)
        .frame(egui::Frame::new().fill(Color32::from_rgb(18, 18, 20)).inner_margin(egui::Margin::symmetric(8, 4)))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
            ui.horizontal_centered(|ui| {
                if icon_btn(ui, "Open", "Open a PDF (Ctrl+O)").clicked() {
                    app.open_dialog();
                }
                let outline_on = app.tab().map(|tab| tab.outline_open).unwrap_or(false);
                if ui
                    .add(Button::new("Outline").selected(outline_on))
                    .on_hover_text("Document outline")
                    .clicked()
                {
                    if let Some(tab) = app.tab_mut() {
                        tab.outline_open = !tab.outline_open;
                    }
                }
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                tool_chip(ui, &mut app.tool, Tool::Select, "Select", "Move and resize (V)");
                tool_chip(ui, &mut app.tool, Tool::Highlight, "Highlight", "Mark text");
                tool_chip(ui, &mut app.tool, Tool::Text, "Text", "Write on the page (T)");
                tool_chip(ui, &mut app.tool, Tool::Note, "Note", "Sticky note (N)");
                tool_chip(ui, &mut app.tool, Tool::Rect, "Rect", "Rectangle (R)");
                tool_chip(ui, &mut app.tool, Tool::Ellipse, "Ellipse", "Ellipse (E)");
                tool_chip(ui, &mut app.tool, Tool::Line, "Line", "Line");
                tool_chip(ui, &mut app.tool, Tool::Math, "Math", "Equation (M)");
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                app.color_controls(ui);
                app.metric_controls(ui);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(Button::new("Fit").small())
                        .on_hover_text("Fit page width (Ctrl+0)")
                        .clicked()
                    {
                        app.fit_width();
                    }
                    if ui.small_button("+").on_hover_text("Zoom in (Ctrl+=)").clicked() {
                        app.zoom_by(1.1);
                    }
                    let mut percent = app
                        .doc()
                        .map(|doc| zoom_percent(doc.scale) as f32)
                        .unwrap_or(100.0);
                    let zoom = ui.add(
                        DragValue::new(&mut percent)
                            .range(20.0..=800.0)
                            .suffix("%")
                            .speed(1.0)
                            .max_decimals(0),
                    );
                    if zoom.changed() {
                        app.set_zoom_percent(percent);
                    }
                    zoom.on_hover_text("Drag or type a zoom level. Ctrl+scroll also zooms.");
                    if ui.small_button("−").on_hover_text("Zoom out (Ctrl+-)").clicked() {
                        app.zoom_by(1.0 / 1.1);
                    }
                });
            });
        });
}

fn tab_bar(app: &mut MarkerApp, ctx: &egui::Context) {
    if app.tabs.is_empty() && app.opening.is_empty() {
        return;
    }
    TopBottomPanel::top("tabs")
        .exact_height(30.0)
        .frame(egui::Frame::new().fill(Color32::from_rgb(14, 14, 16)).inner_margin(egui::Margin::symmetric(6, 0)))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
            ui.horizontal_centered(|ui| {
                let mut close = None;
                let mut select = None;
                for (index, tab) in app.tabs.iter().enumerate() {
                    let name = tab
                        .doc
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("document.pdf");
                    let dirty = if tab.doc.session.is_dirty() { " ●" } else { "" };
                    let selected = index == app.active;
                    let fill = if selected {
                        Color32::from_rgb(36, 36, 42)
                    } else {
                        Color32::TRANSPARENT
                    };
                    let response = ui.add(
                        Button::new(format!("{name}{dirty}"))
                            .fill(fill)
                            .stroke(if selected {
                                Stroke::new(1.0, Color32::from_rgb(70, 110, 190))
                            } else {
                                Stroke::NONE
                            }),
                    );
                    if response.clicked() {
                        select = Some(index);
                    }
                    if response.middle_clicked() {
                        close = Some(index);
                    }
                    if ui
                        .add(Button::new("×").small().fill(Color32::TRANSPARENT))
                        .on_hover_text("Close tab (Ctrl+W)")
                        .clicked()
                    {
                        close = Some(index);
                    }
                    ui.add_space(4.0);
                }
                if !app.opening.is_empty() {
                    ui.label(RichText::new("Opening…").weak().size(12.0));
                }
                if ui.small_button("+").on_hover_text("Open another PDF").clicked() {
                    app.open_dialog();
                }
                if let Some(index) = select {
                    app.active = index;
                }
                if let Some(index) = close {
                    app.close_tab(index);
                }
            });
        });
}

fn search_bar(app: &mut MarkerApp, ctx: &egui::Context) {
    let mut close = false;
    let mut next = false;
    let mut prev = false;
    {
        let Some(tab) = app.tab_mut() else {
            return;
        };
        if !tab.search.open {
            return;
        }
        let mut query_changed = false;
        TopBottomPanel::top("search")
            .exact_height(34.0)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(24, 28, 36))
                    .inner_margin(egui::Margin::symmetric(10, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
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
                    if ui.button("Prev").clicked() {
                        prev = true;
                    }
                    if ui.button("Next").clicked() {
                        next = true;
                    }
                    if ui.small_button("×").clicked() {
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
                });
            });
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
    if next {
        app.search_step(1);
    } else if prev {
        app.search_step(-1);
    }
}

fn status_bar(app: &mut MarkerApp, ctx: &egui::Context) {
    TopBottomPanel::bottom("status")
        .exact_height(28.0)
        .frame(egui::Frame::new().fill(Color32::from_rgb(18, 18, 20)).inner_margin(egui::Margin::symmetric(8, 4)))
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                let page_info = app.tab().map(|tab| {
                    let count = tab.doc.pages.len().max(1);
                    let page = tab.doc.current_page(app.view_rect.height().max(1.0)) + 1;
                    (page, count)
                });
                let want_page_focus = app.page_focus;
                let mut jump = None;
                if let Some((page, count)) = page_info {
                    let mut page_1 = page as u32;
                    let response = ui.add(
                        DragValue::new(&mut page_1)
                            .range(1..=count as u32)
                            .prefix("Page ")
                            .suffix(format!(" / {count}"))
                            .speed(0.2),
                    );
                    if want_page_focus {
                        response.request_focus();
                    }
                    if response.changed() {
                        jump = Some(page_1 as usize - 1);
                    }
                } else if !app.opening.is_empty() {
                    ui.label("Opening…");
                } else {
                    ui.label(RichText::new("No document").weak());
                }
                if let Some(error) = &app.error {
                    ui.label(RichText::new(error).color(Color32::from_rgb(230, 120, 110)));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let save = app.tab().map(|tab| match &tab.save {
                        SaveState::Clean => ("Saved".to_string(), Color32::from_rgb(140, 140, 144)),
                        SaveState::Dirty { .. } => ("Unsaved".into(), Color32::from_rgb(230, 190, 90)),
                        SaveState::Saving => ("Saving…".into(), Color32::from_rgb(140, 180, 230)),
                        SaveState::Failed { message, .. } => {
                            (message.clone(), Color32::from_rgb(230, 120, 110))
                        }
                    });
                    if let Some((label, color)) = save {
                        ui.label(RichText::new(label).color(color).size(12.0));
                    }
                });
                if app.page_focus {
                    app.page_focus = false;
                }
                if let Some(page) = jump {
                    app.queue_jump(page, None);
                }
            });
        });
}

fn outline_panel(app: &mut MarkerApp, ctx: &egui::Context) {
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
        .frame(egui::Frame::new().fill(Color32::from_rgb(16, 16, 18)).inner_margin(8.0))
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

pub(crate) fn empty_state(app: &mut MarkerApp, ui: &mut egui::Ui) {
    let rect = ui.available_rect_before_wrap();
    ui.painter().rect_filled(rect, 0.0, Color32::from_rgb(22, 22, 24));
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Marker").size(32.0).color(Color32::from_rgb(236, 236, 240)));
                ui.add_space(6.0);
                ui.label(RichText::new("Drop a PDF here, or open one from the toolbar.").weak());
                ui.add_space(14.0);
                if ui.add(Button::new("Open PDF").min_size(Vec2::new(120.0, 28.0))).clicked() {
                    app.open_dialog();
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Ctrl+O  ·  Ctrl+F search  ·  / vim search  ·  Ctrl+scroll zoom")
                        .weak()
                        .size(12.0),
                );
                if !app.opening.is_empty() {
                    ui.add_space(10.0);
                    ui.label("Opening…");
                }
            });
        });
    });
}

fn outline_node(ui: &mut egui::Ui, node: &OutlineNode, depth: usize, jump: &mut Option<(usize, Option<f32>)>) {
    if node.children.is_empty() {
        if ui.add(Button::new(&node.title).frame(false).wrap()).clicked() {
            if let Some(page) = node.page {
                *jump = Some((page, node.y));
            }
        }
        return;
    }
    let header = egui::CollapsingHeader::new(&node.title)
        .default_open(depth < 1)
        .show(ui, |ui| {
            for child in &node.children {
                outline_node(ui, child, depth + 1, jump);
            }
        });
    if header.header_response.clicked() {
        if let Some(page) = node.page {
            *jump = Some((page, node.y));
        }
    }
}

fn tool_chip(ui: &mut egui::Ui, tool: &mut Tool, value: Tool, label: &str, tip: &str) {
    let selected = *tool == value;
    let response = ui.add(Button::new(label).selected(selected));
    if response.clicked() {
        *tool = value;
    }
    response.on_hover_text(tip);
}

fn icon_btn(ui: &mut egui::Ui, label: &str, tip: &str) -> egui::Response {
    ui.button(label).on_hover_text(tip)
}

pub(crate) fn color_dot(ui: &mut egui::Ui, color: Rgb, selected: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
    ui.painter().circle_filled(rect.center(), 6.0, color.to_color32());
    if selected {
        ui.painter()
            .circle_stroke(rect.center(), 7.5, Stroke::new(1.5, Color32::WHITE));
    }
    response.clicked()
}

pub(crate) fn palette_for(tool: Tool) -> &'static [Rgb] {
    match tool {
        Tool::Highlight | Tool::Note => &HIGHLIGHT_COLORS,
        _ => &INK_COLORS,
    }
}

pub(crate) fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = Color32::from_rgb(28, 28, 32);
    visuals.panel_fill = Color32::from_rgb(18, 18, 20);
    visuals.extreme_bg_color = Color32::from_rgb(12, 12, 14);
    visuals.faint_bg_color = Color32::from_rgb(32, 32, 36);
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(18, 18, 20);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(38, 38, 44);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(52, 52, 60);
    visuals.widgets.active.bg_fill = Color32::from_rgb(62, 62, 72);
    visuals.selection.bg_fill = Color32::from_rgb(62, 104, 176);
    visuals.override_text_color = Some(Color32::from_rgb(232, 232, 236));
    visuals.widgets.inactive.corner_radius = 4.0.into();
    visuals.widgets.hovered.corner_radius = 4.0.into();
    visuals.widgets.active.corner_radius = 4.0.into();
    ctx.set_visuals(visuals);
    let mut style = (*ctx.style()).clone();
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.text_styles.insert(
        egui::TextStyle::Body,
        FontId::new(13.5, FontFamily::Proportional),
    );
    ctx.set_style(style);
}

fn vec2(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}
