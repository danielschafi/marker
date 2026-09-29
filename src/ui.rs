use egui::{
    Align, Align2, Button, Color32, CornerRadius, DragValue, FontFamily, FontId, Key, Layout, Pos2,
    Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextEdit, TopBottomPanel, Vec2,
};

use crate::app::{MarkerApp, SaveState, Tool};
use crate::geom::{zoom_percent, Rgb, HIGHLIGHT_COLORS, INK_COLORS};
use crate::pdf::OutlineNode;

pub(crate) const CHROME: Color32 = Color32::from_rgb(30, 30, 34);
pub(crate) const CHROME_RAISED: Color32 = Color32::from_rgb(42, 42, 48);
pub(crate) const BACKDROP: Color32 = Color32::from_rgb(16, 16, 18);
pub(crate) const ACCENT: Color32 = Color32::from_rgb(110, 156, 230);
const TEXT: Color32 = Color32::from_rgb(232, 232, 236);
const TEXT_DIM: Color32 = Color32::from_rgb(154, 154, 162);
const DIRTY: Color32 = Color32::from_rgb(230, 186, 92);
const HAIRLINE: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 26);

pub(crate) fn chrome(app: &mut MarkerApp, ctx: &egui::Context) {
    tab_bar(app, ctx);
    tool_bar(app, ctx);
    search_bar(app, ctx);
    outline_panel(app, ctx);
}

fn tab_bar(app: &mut MarkerApp, ctx: &egui::Context) {
    TopBottomPanel::top("tabs")
        .exact_height(40.0)
        .frame(
            egui::Frame::new()
                .fill(CHROME)
                .inner_margin(egui::Margin::symmetric(10, 6)),
        )
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
            ui.spacing_mut().button_padding = vec2(8.0, 3.0);
            ui.horizontal_centered(|ui| {
                if chrome_button(ui, "Open", false)
                    .on_hover_text("Open a PDF (Ctrl+O)")
                    .clicked()
                {
                    app.open_dialog();
                }
                let outline_on = app.tab().map(|tab| tab.outline_open).unwrap_or(false);
                if app.tab().is_some()
                    && chrome_button(ui, "Outline", outline_on)
                        .on_hover_text("Document outline")
                        .clicked()
                {
                    if let Some(tab) = app.tab_mut() {
                        tab.outline_open = !tab.outline_open;
                    }
                }
                ui.add_space(6.0);

                let mut close = None;
                let mut select = None;
                for (index, tab) in app.tabs.iter().enumerate() {
                    let name = tab
                        .doc
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("document.pdf");
                    let action = tab_pill(
                        ui,
                        name,
                        index == app.active,
                        tab.doc.session.is_dirty(),
                        index,
                    );
                    if action.select {
                        select = Some(index);
                    }
                    if action.close {
                        close = Some(index);
                    }
                }
                if !app.opening.is_empty() {
                    ui.label(RichText::new("Opening…").weak().size(12.0));
                }
                document_controls(app, ui);
                if let Some(index) = select {
                    app.active = index;
                }
                if let Some(index) = close {
                    app.close_tab(index);
                }
            });
        });
}

struct TabAction {
    select: bool,
    close: bool,
}

fn tab_pill(ui: &mut egui::Ui, name: &str, selected: bool, dirty: bool, index: usize) -> TabAction {
    let color = if selected { TEXT } else { TEXT_DIM };
    let galley = ui.painter().layout_no_wrap(
        name.to_owned(),
        FontId::new(12.5, FontFamily::Proportional),
        color,
    );
    let mut width = galley.size().x + 16.0;
    if dirty {
        width += 12.0;
    }
    if selected {
        width += 16.0;
    }
    let (rect, response) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click());
    let fill = if selected {
        CHROME_RAISED
    } else if response.hovered() {
        Color32::from_white_alpha(14)
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    }
    let text_width = galley.size().x;
    let text_pos = Pos2::new(rect.left() + 8.0, rect.center().y - galley.size().y * 0.5);
    ui.painter().galley(text_pos, galley, color);
    if dirty {
        ui.painter().circle_filled(
            Pos2::new(rect.left() + 12.0 + text_width, rect.center().y),
            2.5,
            DIRTY,
        );
    }

    let mut close_clicked = false;
    if selected {
        let close_rect = Rect::from_center_size(
            Pos2::new(rect.right() - 11.0, rect.center().y),
            Vec2::splat(14.0),
        );
        let close = ui.interact(
            close_rect,
            ui.id().with(("tab-close", index)),
            Sense::click(),
        );
        let close_color = if close.hovered() { TEXT } else { TEXT_DIM };
        ui.painter().text(
            close_rect.center(),
            Align2::CENTER_CENTER,
            "×",
            FontId::new(13.0, FontFamily::Proportional),
            close_color,
        );
        let close = close.on_hover_text("Close tab (Ctrl+W)");
        close_clicked = close.clicked() || close.middle_clicked();
    }

    TabAction {
        select: response.clicked() && !close_clicked,
        close: close_clicked || response.middle_clicked(),
    }
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
            .exact_height(40.0)
            .frame(
                egui::Frame::new()
                    .fill(CHROME)
                    .inner_margin(egui::Margin::symmetric(12, 6)),
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

fn document_controls(app: &mut MarkerApp, ui: &mut egui::Ui) {
    let page_info = app.tab().map(|tab| {
        let count = tab.doc.pages.len().max(1);
        let page = tab.doc.current_page(app.view_rect.height().max(1.0)) + 1;
        (page, count)
    });
    let want_page_focus = app.page_focus;
    let notice = app.tab().and_then(|tab| match &tab.save {
        SaveState::Clean | SaveState::Dirty { .. } => None,
        SaveState::Saving => Some(("Saving…".to_string(), ACCENT)),
        SaveState::Failed { message, .. } => {
            Some((message.clone(), Color32::from_rgb(230, 120, 110)))
        }
    });
    let error = app.error.clone();
    let mut jump = None;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 0.0);
        if app.doc().is_some() {
            control_cluster(ui, |ui| {
                if cluster_button(ui, "Fit", Vec2::new(36.0, CONTROL_H))
                    .on_hover_text("Fit page width (Ctrl+0)")
                    .clicked()
                {
                    app.fit_width();
                }
                cluster_sep(ui);
                if cluster_button(ui, "+", Vec2::splat(CONTROL_H))
                    .on_hover_text("Zoom in (Ctrl+=)")
                    .clicked()
                {
                    app.zoom_by(1.1);
                }
                let mut percent = app
                    .doc()
                    .map(|doc| zoom_percent(doc.scale) as f32)
                    .unwrap_or(100.0);
                let zoom = cluster_drag(
                    ui,
                    DragValue::new(&mut percent)
                        .range(20.0..=800.0)
                        .suffix("%")
                        .speed(1.0)
                        .max_decimals(0),
                    56.0,
                );
                if zoom.changed() {
                    app.set_zoom_percent(percent);
                }
                zoom.on_hover_text("Drag or type a zoom level. Pinch or Ctrl+scroll also zooms.");
                if cluster_button(ui, "−", Vec2::splat(CONTROL_H))
                    .on_hover_text("Zoom out (Ctrl+-)")
                    .clicked()
                {
                    app.zoom_by(1.0 / 1.1);
                }
            });
        }
        if let Some((label, color)) = notice {
            ui.label(RichText::new(label).color(color).size(12.0));
        }
        if let Some(error) = error {
            ui.label(
                RichText::new(error)
                    .color(Color32::from_rgb(230, 120, 110))
                    .size(12.0),
            );
        }
        if let Some((page, count)) = page_info {
            control_cluster(ui, |ui| {
                if cluster_button(ui, "+Page", Vec2::new(52.0, CONTROL_H))
                    .on_hover_text("Insert blank page after current (Ctrl+Shift+Enter)")
                    .clicked()
                {
                    app.insert_page_after_current();
                }
                cluster_sep(ui);
                let mut page_1 = page as u32;
                let response = cluster_drag(
                    ui,
                    DragValue::new(&mut page_1)
                        .range(1..=count as u32)
                        .suffix(format!(" / {count}"))
                        .speed(0.2),
                    88.0,
                )
                .on_hover_text("Page. Ctrl+G jumps here.");
                if want_page_focus {
                    response.request_focus();
                }
                if response.changed() {
                    jump = Some(page_1 as usize - 1);
                }
            });
        }
    });
    if app.page_focus {
        app.page_focus = false;
    }
    if let Some(page) = jump {
        app.queue_jump(page, None);
    }
}

const CONTROL_H: f32 = 24.0;

fn control_cluster(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let mut prepared = egui::Frame::new()
        .fill(CHROME_RAISED)
        .corner_radius(CornerRadius::same(7))
        .inner_margin(egui::Margin::symmetric(2, 2))
        .begin(ui);
    prepared.content_ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    prepared.content_ui.spacing_mut().button_padding = vec2(0.0, 0.0);
    prepared
        .content_ui
        .horizontal(|ui| {
            ui.set_height(CONTROL_H);
            add_contents(ui);
        });
    prepared.end(ui);
}

fn cluster_sep(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, CONTROL_H - 6.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        (rect.top() - 1.0)..=(rect.bottom() + 1.0),
        Stroke::new(1.0, HAIRLINE),
    );
}

fn cluster_button(ui: &mut egui::Ui, label: &str, size: Vec2) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if response.is_pointer_button_down_on() {
        Color32::from_white_alpha(28)
    } else if response.hovered() {
        Color32::from_white_alpha(16)
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(5), fill);
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::new(12.5, FontFamily::Proportional),
        TEXT,
    );
    response
}

fn cluster_drag(ui: &mut egui::Ui, drag: DragValue<'_>, width: f32) -> egui::Response {
    ui.scope(|ui| {
        {
            let visuals = ui.visuals_mut();
            visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
            visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            visuals.widgets.inactive.bg_stroke = Stroke::NONE;
            visuals.widgets.hovered.bg_fill = Color32::from_white_alpha(16);
            visuals.widgets.hovered.weak_bg_fill = Color32::from_white_alpha(16);
            visuals.widgets.hovered.bg_stroke = Stroke::NONE;
            visuals.widgets.active.bg_fill = Color32::from_white_alpha(28);
            visuals.widgets.active.weak_bg_fill = Color32::from_white_alpha(28);
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
        .frame(
            egui::Frame::new()
                .fill(CHROME)
                .stroke(Stroke::new(1.0, HAIRLINE))
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

fn tool_bar(app: &mut MarkerApp, ctx: &egui::Context) {
    if app.tab().is_none() {
        return;
    }
    TopBottomPanel::top("tools")
        .exact_height(40.0)
        .frame(
            egui::Frame::new()
                .fill(CHROME)
                .inner_margin(egui::Margin::symmetric(8, 5)),
        )
        .show(ctx, |ui| {
            let width_id = ui.id().with("cluster-width");
            let known = ui.data(|data| data.get_temp::<f32>(width_id));
            let lead = known
                .map(|width| ((ui.available_width() - width) * 0.5).max(0.0))
                .unwrap_or(0.0);
            let mut cluster_width = known.unwrap_or(0.0);
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing = vec2(3.0, 0.0);
                if lead > 0.0 {
                    ui.add_space(lead);
                }
                let cluster = ui.scope_builder(
                    egui::UiBuilder::new().layout(Layout::left_to_right(Align::Center)),
                    |ui| {
                        ui.spacing_mut().item_spacing = vec2(3.0, 0.0);
                        for tool in Tool::ALL {
                            tool_chip(ui, &mut app.tool, tool);
                        }
                        vbar(ui);
                        app.color_controls(ui);
                        app.metric_controls(ui);
                    },
                );
                cluster_width = cluster.response.rect.width();
            });
            ui.data_mut(|data| data.insert_temp(width_id, cluster_width));
        });
}

fn tool_chip(ui: &mut egui::Ui, current: &mut Tool, tool: Tool) {
    let selected = *current == tool;
    let (rect, response) = ui.allocate_exact_size(vec2(30.0, 28.0), Sense::click());
    let fill = if selected {
        accent_fill(48)
    } else if response.hovered() {
        Color32::from_white_alpha(16)
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    }
    let icon = Rect::from_center_size(rect.center() + vec2(-3.0, -1.0), Vec2::splat(15.0));
    let color = if selected { ACCENT } else { TEXT };
    paint_tool_icon(ui.painter(), icon, tool, color);
    let key_color = if selected { ACCENT } else { TEXT_DIM };
    ui.painter().text(
        rect.right_bottom() + vec2(-2.0, -1.0),
        Align2::RIGHT_BOTTOM,
        tool.shortcut().name(),
        FontId::new(9.0, FontFamily::Proportional),
        key_color,
    );
    if response.clicked() {
        *current = tool;
    }
    response.on_hover_text(format!("{} ({})", tool.hint(), tool.shortcut().name()));
}

fn paint_tool_icon(painter: &egui::Painter, rect: Rect, tool: Tool, color: Color32) {
    let stroke = Stroke::new(1.35, color);
    let origin = rect.left_top();
    match tool {
        Tool::Select => {
            let points = vec![
                origin + vec2(1.5, 1.0),
                origin + vec2(1.5, 13.5),
                origin + vec2(4.8, 10.0),
                origin + vec2(7.6, 14.2),
                origin + vec2(9.6, 13.2),
                origin + vec2(6.8, 9.0),
                origin + vec2(11.2, 8.6),
            ];
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        Tool::Highlight => {
            let bar = Rect::from_min_max(origin + vec2(0.5, 11.0), origin + vec2(14.5, 14.2));
            painter.rect_filled(bar, 1.5, color.gamma_multiply(0.4));
            let points = vec![
                origin + vec2(1.5, 7.5),
                origin + vec2(7.5, 1.0),
                origin + vec2(13.5, 5.5),
                origin + vec2(7.5, 12.0),
            ];
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        Tool::Text => {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "T",
                FontId::new(14.0, FontFamily::Proportional),
                color,
            );
        }
        Tool::Rect => {
            painter.rect_stroke(rect.shrink(1.5), 2.0, stroke, StrokeKind::Inside);
        }
        Tool::Ellipse => {
            painter.circle_stroke(rect.center(), rect.width() * 0.38, stroke);
        }
        Tool::Line => {
            painter.line_segment(
                [
                    rect.left_bottom() + vec2(1.5, -1.5),
                    rect.right_top() + vec2(-1.5, 1.5),
                ],
                stroke,
            );
        }
        Tool::Math => {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "∑",
                FontId::new(15.0, FontFamily::Proportional),
                color,
            );
        }
    }
}

fn vbar(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, 18.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.top()..=rect.bottom(),
        Stroke::new(1.0, HAIRLINE),
    );
}

pub(crate) fn empty_state(app: &mut MarkerApp, ui: &mut egui::Ui) {
    let rect = ui.available_rect_before_wrap();
    ui.painter().rect_filled(rect, 0.0, BACKDROP);
    let recent: Vec<_> = app.settings.recent.clone();
    let mut open_path = None;
    let mut forget = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ScrollArea::vertical()
            .id_salt("empty-recent")
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.16).clamp(24.0, 120.0));
                    ui.label(RichText::new("Marker").size(36.0).color(TEXT));
                    ui.add_space(8.0);
                    ui.label(RichText::new("Drop a PDF here, or open one.").weak());
                    ui.add_space(18.0);
                    if ui
                        .add(
                            Button::new(
                                RichText::new("Open PDF").color(Color32::from_rgb(14, 18, 28)),
                            )
                            .fill(ACCENT)
                            .stroke(Stroke::NONE)
                            .corner_radius(8.0)
                            .min_size(Vec2::new(128.0, 32.0)),
                        )
                        .clicked()
                    {
                        app.open_dialog();
                    }
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(
                            "Ctrl+O  ·  Ctrl+F search  ·  / vim search  ·  pinch or Ctrl+scroll zoom",
                        )
                        .weak()
                        .size(12.0),
                    );
                    if !app.opening.is_empty() {
                        ui.add_space(12.0);
                        ui.label("Opening…");
                    }

                    if !recent.is_empty() {
                        ui.add_space(28.0);
                        ui.label(RichText::new("Recent").size(13.0).color(TEXT_DIM));
                        ui.add_space(8.0);
                        let list_width = ui.available_width().min(440.0).max(280.0);
                        for path in &recent {
                            let name = path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("document.pdf");
                            let exists = path.is_file();
                            let parent = path
                                .parent()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default();
                            let (rect, response) = ui.allocate_exact_size(
                                Vec2::new(list_width, 44.0),
                                Sense::click(),
                            );
                            let fill = if response.hovered() {
                                Color32::from_white_alpha(18)
                            } else {
                                Color32::from_white_alpha(10)
                            };
                            ui.painter()
                                .rect_filled(rect, CornerRadius::same(8), fill);
                            let name_color = if exists { TEXT } else { TEXT_DIM };
                            ui.painter().text(
                                Pos2::new(rect.left() + 12.0, rect.top() + 7.0),
                                Align2::LEFT_TOP,
                                name,
                                FontId::new(13.0, FontFamily::Proportional),
                                name_color,
                            );
                            if !parent.is_empty() {
                                ui.painter().text(
                                    Pos2::new(rect.left() + 12.0, rect.top() + 24.0),
                                    Align2::LEFT_TOP,
                                    if exists {
                                        parent.as_str()
                                    } else {
                                        "(missing)"
                                    },
                                    FontId::new(11.0, FontFamily::Proportional),
                                    TEXT_DIM,
                                );
                            }
                            let response = if parent.is_empty() {
                                response
                            } else {
                                response.on_hover_text(&parent)
                            };
                            if response.clicked() {
                                if exists {
                                    open_path = Some(path.clone());
                                } else {
                                    forget = Some(path.clone());
                                    app.error =
                                        Some(format!("File not found: {}", path.display()));
                                }
                            }
                            if response.secondary_clicked() {
                                forget = Some(path.clone());
                            }
                        }
                    }
                    ui.add_space(40.0);
                });
            });
    });
    if let Some(path) = open_path {
        app.open_path(path);
    }
    if let Some(path) = forget {
        app.settings.forget_recent(&path);
    }
}

fn outline_node(
    ui: &mut egui::Ui,
    node: &OutlineNode,
    depth: usize,
    jump: &mut Option<(usize, Option<f32>)>,
) {
    if node.children.is_empty() {
        if ui
            .add(Button::new(&node.title).frame(false).wrap())
            .clicked()
        {
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

fn chrome_button(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let fill = if selected {
        accent_fill(42)
    } else {
        Color32::TRANSPARENT
    };
    let color = if selected { ACCENT } else { TEXT };
    ui.add(
        Button::new(RichText::new(label).size(12.5).color(color))
            .fill(fill)
            .stroke(Stroke::NONE)
            .corner_radius(8.0),
    )
}

pub(crate) fn color_dot(ui: &mut egui::Ui, color: Rgb, selected: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
    ui.painter()
        .circle_filled(rect.center(), 6.0, color.to_color32());
    if selected {
        ui.painter()
            .circle_stroke(rect.center(), 7.5, Stroke::new(1.5, Color32::WHITE));
    }
    response.clicked()
}

pub(crate) fn palette_for(tool: Tool) -> &'static [Rgb] {
    match tool {
        Tool::Highlight => &HIGHLIGHT_COLORS,
        _ => &INK_COLORS,
    }
}

pub(crate) fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = CHROME_RAISED;
    visuals.panel_fill = CHROME;
    visuals.extreme_bg_color = BACKDROP;
    visuals.faint_bg_color = Color32::from_rgb(48, 48, 54);
    visuals.widgets.noninteractive.bg_fill = CHROME;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(48, 48, 56);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(60, 60, 68);
    visuals.widgets.active.bg_fill = Color32::from_rgb(70, 70, 80);
    visuals.selection.bg_fill = ACCENT;
    visuals.override_text_color = Some(TEXT);
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
    visuals.widgets.active.corner_radius = CornerRadius::same(8);
    ctx.set_visuals(visuals);
    let mut style = (*ctx.style()).clone();
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.text_styles.insert(
        egui::TextStyle::Body,
        FontId::new(13.5, FontFamily::Proportional),
    );
    ctx.set_style(style);
}

fn accent_fill(alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(110, 156, 230, alpha)
}

fn vec2(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}
