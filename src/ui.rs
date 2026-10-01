use std::sync::Arc;

use egui::{
    Align, Align2, Button, Color32, CornerRadius, DragValue, FontData, FontDefinitions, FontFamily,
    FontId, Id, Key, Layout, PointerButton, Pos2, Rect, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, TextEdit, TopBottomPanel, Vec2,
};

use crate::app::{MarkerApp, SaveState, SplitDropZone, SplitState, TabListState, Tool};
use crate::assistant::{AssistantAttachment, AssistantRole, CaptureMode};
use crate::geom::{zoom_percent, Rgb, HIGHLIGHT_COLORS, INK_COLORS};
use crate::pdf::OutlineNode;
use crate::theme::{self, ThemeColors};
use crate::view::viewport_tab;

const BRAND_FAMILY: &str = "Brand";

fn p(ctx: &egui::Context) -> ThemeColors {
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
        assistant_panel(app, ctx);
    }
    paint_tab_list(app, ctx, show_chrome);
}

const TAB_BAR_H: f32 = 32.0;
const TOOL_BAR_H: f32 = 28.0;
const SEARCH_BAR_H: f32 = 32.0;
const TAB_PILL_H: f32 = 20.0;
const TOOL_CHIP: Vec2 = Vec2::new(24.0, 22.0);

fn tab_bar(app: &mut MarkerApp, ctx: &egui::Context) {
    let colors = p(ctx);
    TopBottomPanel::top("tabs")
        .exact_height(TAB_BAR_H)
        .show_separator_line(false)
        .frame(
            egui::Frame::new()
                .fill(colors.chrome)
                .stroke(Stroke::NONE)
                .inner_margin(egui::Margin::symmetric(8, 4)),
        )
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(3.0, 0.0);
            ui.spacing_mut().button_padding = vec2(6.0, 2.0);
            let row_h = ui.available_height();
            let full = ui.available_rect_before_wrap();
            let (_, row_response) =
                ui.allocate_exact_size(vec2(full.width(), row_h), Sense::hover());
            let row = row_response.rect;

            let center_reserve = 160.0;
            let left_rect = Rect::from_min_max(
                row.left_top(),
                Pos2::new((row.center().x - center_reserve).max(row.left() + 8.0), row.bottom()),
            );
            let right_rect = Rect::from_min_max(
                Pos2::new((row.center().x + center_reserve).min(row.right() - 8.0), row.top()),
                row.right_bottom(),
            );

            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(left_rect)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    chrome_nav(app, ui);
                },
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(right_rect)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    document_controls(app, ui);
                },
            );
            paint_current_title(app, ui, row);
        });
    paint_tab_menu(app, ctx);
}

fn chrome_nav(app: &mut MarkerApp, ui: &mut egui::Ui) {
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
    if app.tab().is_some() {
        let tools_on = app.settings.toolbar_visible;
        if chrome_button(ui, "Tools", tools_on)
            .on_hover_text("Show or hide the annotation toolbar (Ctrl+Shift+B)")
            .clicked()
        {
            app.toggle_toolbar();
        }
        if chrome_button(ui, "Zen", app.zen)
            .on_hover_text(
                "Fullscreen reading; chrome hides until the pointer hits the top (F11)",
            )
            .clicked()
        {
            app.toggle_zen(ui.ctx());
        }
        if chrome_button(ui, "Assistant", app.assistant_open)
            .on_hover_text("Cursor learning assistant (Ctrl+Alt+I)")
            .clicked()
        {
            app.toggle_assistant();
        }
    }
}

fn paint_current_title(app: &mut MarkerApp, ui: &mut egui::Ui, row: Rect) {
    let colors = p(ui.ctx());
    let opening = !app.opening.is_empty();
    let Some(tab) = app.tab() else {
        if opening {
            ui.painter().text(
                row.center(),
                Align2::CENTER_CENTER,
                "Opening…",
                FontId::new(12.5, FontFamily::Proportional),
                colors.text_dim,
            );
        }
        return;
    };
    let name = tab_file_name(tab);
    let dirty = tab.doc.session.is_dirty();
    let tab_count = app.tabs.len();
    let list_open = app.tab_list.is_some();

    let title_color = colors.text;
    let galley = ui.painter().layout_no_wrap(
        name.to_owned(),
        FontId::new(13.0, FontFamily::Proportional),
        title_color,
    );
    let mut width = galley.size().x + 20.0;
    if dirty {
        width += 12.0;
    }
    let max_w = (row.width() * 0.38).clamp(120.0, 420.0);
    width = width.min(max_w);

    let title_rect = Rect::from_center_size(row.center(), vec2(width, TAB_PILL_H + 4.0));
    let response = ui.interact(title_rect, Id::new("marker-current-tab"), Sense::click());
    let fill = if list_open {
        colors.chrome_raised
    } else if response.hovered() && tab_count > 1 {
        Color32::from_white_alpha(14)
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(title_rect, CornerRadius::same(6), fill);
    }

    let mut cursor_x = title_rect.center().x - galley.size().x * 0.5;
    if dirty {
        cursor_x -= 6.0;
    }
    let text_pos = Pos2::new(cursor_x, title_rect.center().y - galley.size().y * 0.5);
    let text_right = text_pos.x + galley.size().x;
    ui.painter().galley(text_pos, galley, title_color);
    if dirty {
        ui.painter().circle_filled(
            Pos2::new(text_right + 8.0, title_rect.center().y),
            2.5,
            colors.dirty,
        );
    }

    if tab_count > 1 && response.clicked() {
        app.toggle_tab_list();
    }
    let tip = if tab_count > 1 {
        "Show open tabs · Ctrl+Tab to cycle"
    } else {
        "Current document"
    };
    response.on_hover_text(tip);
}

fn tab_file_name(tab: &crate::app::Tab) -> &str {
    tab.doc
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document.pdf")
}

struct TabAction {
    select: bool,
    close: bool,
    menu: bool,
    drag: bool,
    pointer: Option<Pos2>,
}

fn tab_list_row(
    ui: &mut egui::Ui,
    name: &str,
    selected: bool,
    dirty: bool,
    index: usize,
    row_width: f32,
) -> TabAction {
    let colors = p(ui.ctx());
    let color = if selected { colors.text } else { colors.text_dim };
    let galley = ui.painter().layout_no_wrap(
        name.to_owned(),
        FontId::new(12.5, FontFamily::Proportional),
        color,
    );
    let (rect, response) =
        ui.allocate_exact_size(vec2(row_width, TAB_LIST_ROW_H), Sense::click_and_drag());
    let fill = if selected {
        accent_fill(ui.ctx(), 36)
    } else if response.hovered() || response.dragged() {
        Color32::from_white_alpha(14)
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
    }

    let text_pos = Pos2::new(rect.left() + 10.0, rect.center().y - galley.size().y * 0.5);
    let text_width = galley.size().x;
    ui.painter().galley(text_pos, galley, color);
    if dirty {
        ui.painter().circle_filled(
            Pos2::new(
                (rect.left() + 14.0 + text_width).min(rect.right() - 28.0),
                rect.center().y,
            ),
            2.5,
            colors.dirty,
        );
    }

    let mut close_clicked = false;
    let show_close = selected || response.hovered();
    if show_close {
        let close_rect = Rect::from_center_size(
            Pos2::new(rect.right() - 14.0, rect.center().y),
            Vec2::splat(16.0),
        );
        let close = ui.interact(
            close_rect,
            ui.id().with(("tab-list-close", index)),
            Sense::click(),
        );
        let close_color = if close.hovered() {
            colors.text
        } else {
            colors.text_dim
        };
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

    if response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    let response = response.on_hover_text("Drag to split · right-click for layout");

    TabAction {
        select: response.clicked() && !close_clicked,
        close: close_clicked || response.middle_clicked(),
        menu: response.secondary_clicked(),
        drag: response.drag_started() && !close_clicked,
        pointer: response.interact_pointer_pos(),
    }
}

const TAB_LIST_ROW_H: f32 = 26.0;

fn paint_tab_list(app: &mut MarkerApp, ctx: &egui::Context, show_chrome: bool) {
    let Some(state) = app.tab_list else {
        return;
    };
    if app.tabs.is_empty() {
        app.tab_list = None;
        return;
    }

    let now = std::time::Instant::now();
    if let TabListState::Ephemeral { until } = state {
        if now >= until {
            app.tab_list = None;
            return;
        }
        ctx.request_repaint_after(until.saturating_duration_since(now));
    }

    let colors = p(ctx);
    let mut close = None;
    let mut select = None;
    let mut tab_menu = None;
    let mut drag_tab = None;
    let names: Vec<(usize, String, bool, bool)> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(index, tab)| {
            (
                index,
                tab_file_name(tab).to_owned(),
                index == app.active,
                tab.doc.session.is_dirty(),
            )
        })
        .collect();

    let mut row_width: f32 = 180.0;
    for (_, name, selected, dirty) in &names {
        let mut w = ui_measure_text(ctx, name, 12.5) + 36.0;
        if *dirty {
            w += 12.0;
        }
        if *selected {
            w += 8.0;
        }
        row_width = row_width.max(w);
    }
    row_width = row_width.clamp(180.0, 320.0);

    let top = if show_chrome {
        TAB_BAR_H
            + if app.settings.toolbar_visible {
                TOOL_BAR_H
            } else {
                0.0
            }
            + 8.0
    } else {
        12.0
    };

    let area = egui::Area::new(Id::new("marker-tab-list"))
        .order(egui::Order::Foreground)
        .anchor(Align2::RIGHT_TOP, vec2(-14.0, top))
        .interactable(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(colors.chrome)
                .stroke(Stroke::new(1.0, colors.hairline))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    ui.set_width(row_width);
                    ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
                    for (index, name, selected, dirty) in &names {
                        let action =
                            tab_list_row(ui, name, *selected, *dirty, *index, row_width);
                        if action.select {
                            select = Some(*index);
                        }
                        if action.close {
                            close = Some(*index);
                        }
                        if action.drag {
                            drag_tab = Some(*index);
                        }
                        if action.menu {
                            tab_menu = action
                                .pointer
                                .map(|pos| (*index, pos))
                                .or_else(|| {
                                    Some((
                                        *index,
                                        ui.ctx()
                                            .pointer_latest_pos()
                                            .unwrap_or(Pos2::ZERO),
                                    ))
                                });
                        }
                    }
                });
        });

    // Hovering the overlay counts as activity and resets the auto-hide timer.
    if area.response.contains_pointer() {
        if let Some(TabListState::Ephemeral { .. }) = app.tab_list {
            app.tab_list = Some(TabListState::Ephemeral {
                until: now + std::time::Duration::from_millis(2500),
            });
        }
    }

    if let Some(index) = select {
        app.activate_tab(index);
    }
    if let Some(index) = close {
        app.close_tab(index);
    }
    if let Some(index) = drag_tab {
        if app.tabs.len() >= 2 {
            app.tab_drag = Some(index);
            app.tab_menu = None;
        }
    }
    if let Some(menu) = tab_menu {
        app.tab_menu = Some(menu);
    }

    let pinned = matches!(app.tab_list, Some(TabListState::Pinned));
    // Title / indicator toggles already ran this frame; ignore those clicks for dismiss.
    let chrome_toggle = ["marker-current-tab", "marker-tabs-indicator"].iter().any(|id| {
        ctx.read_response(Id::new(*id)).is_some_and(|r| r.clicked())
    });
    let outside = pinned
        && ctx.input(|input| input.pointer.button_clicked(PointerButton::Primary))
        && !area.response.contains_pointer()
        && !chrome_toggle;
    if outside {
        app.dismiss_tab_list();
    }
}

fn ui_measure_text(ctx: &egui::Context, text: &str, size: f32) -> f32 {
    ctx.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(
                text.to_owned(),
                FontId::new(size, FontFamily::Proportional),
                Color32::WHITE,
            )
            .size()
            .x
    })
}

fn search_bar(app: &mut MarkerApp, ctx: &egui::Context) {
    let colors = p(ctx);
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
            .exact_height(SEARCH_BAR_H)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(colors.chrome)
                    .stroke(Stroke::NONE)
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
    let colors = p(ui.ctx());
    let page_info = app.tab().map(|tab| {
        let count = tab.doc.pages.len().max(1);
        let page = tab.doc.current_page(app.view_rect.height().max(1.0)) + 1;
        (page, count)
    });
    let want_page_focus = app.page_focus;
    let notice = app.tab().and_then(|tab| match &tab.save {
        SaveState::Clean | SaveState::Dirty { .. } => None,
        SaveState::Saving => Some(("Saving…".to_string(), colors.accent)),
        SaveState::Failed { message, .. } => {
            Some((message.clone(), Color32::from_rgb(230, 120, 110)))
        }
    });
    let error = app.error.clone();
    let mut jump = None;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 0.0);
        // Rightmost: compact open-tabs affordance (list anchors top-right).
        if app.tabs.len() >= 2 {
            tabs_indicator_button(app, ui);
        }
        if app.doc().is_some() {
            control_cluster(ui, |ui| {
                if cluster_button(ui, "Fit", Vec2::new(36.0, CONTROL_H))
                    .on_hover_text("Fit width, then height on next click (Ctrl+0)")
                    .clicked()
                {
                    app.fit_toggle();
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

const CONTROL_H: f32 = 20.0;

fn tabs_indicator_button(app: &mut MarkerApp, ui: &mut egui::Ui) {
    let colors = p(ui.ctx());
    let list_open = app.tab_list.is_some();
    let label = format!("{} ▾", app.tabs.len());
    let text_w = ui_measure_text(ui.ctx(), &label, 11.5);
    let width = (text_w + 14.0).clamp(36.0, 56.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, CONTROL_H), Sense::hover());
    // Stable id so outside-click dismiss can ignore this toggle.
    let response = ui.interact(rect, Id::new("marker-tabs-indicator"), Sense::click());
    let fill = if list_open {
        accent_fill(ui.ctx(), 42)
    } else if response.is_pointer_button_down_on() {
        Color32::from_white_alpha(28)
    } else if response.hovered() {
        Color32::from_white_alpha(16)
    } else {
        colors.chrome_raised
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(5), fill);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::new(11.5, FontFamily::Proportional),
        if list_open {
            colors.accent
        } else {
            colors.text
        },
    );
    if response.clicked() {
        app.toggle_tab_list();
    }
    response.on_hover_text("Open tabs · Ctrl+Tab to cycle");
}

fn control_cluster(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let colors = p(ui.ctx());
    let mut prepared = egui::Frame::new()
        .fill(colors.chrome_raised)
        .corner_radius(CornerRadius::same(5))
        .inner_margin(egui::Margin::symmetric(1, 1))
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
    let colors = p(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, CONTROL_H - 6.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        (rect.top() - 1.0)..=(rect.bottom() + 1.0),
        Stroke::new(1.0, colors.hairline),
    );
}

fn cluster_button(ui: &mut egui::Ui, label: &str, size: Vec2) -> egui::Response {
    let colors = p(ui.ctx());
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
        colors.text,
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
    let colors = p(ctx);
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
                .stroke(Stroke::new(1.0, colors.hairline))
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

fn assistant_panel(app: &mut MarkerApp, ctx: &egui::Context) {
    if !app.assistant_open || app.tab().is_none() {
        return;
    }
    let colors = p(ctx);
    let max_w = (ctx.content_rect().width() * 0.45).min(640.0).max(300.0);
    let mut send = false;
    let mut stop = false;
    let mut new_chat = false;
    let mut close = false;
    let mut remove_attach = None;
    let mut begin_text = false;
    let mut begin_region = false;

    egui::SidePanel::right("assistant")
        .resizable(true)
        .default_width(360.0)
        .width_range(300.0..=max_w)
        .frame(
            egui::Frame::new()
                .fill(colors.chrome)
                .stroke(Stroke::new(1.0, colors.hairline))
                .inner_margin(10.0),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Cursor").strong().size(14.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("×").on_hover_text("Close panel").clicked() {
                        close = true;
                    }
                    if ui.small_button("New").on_hover_text("New chat").clicked() {
                        new_chat = true;
                    }
                });
            });
            let capture = app.capture;
            let status = app
                .tab()
                .and_then(|t| t.assistant.status_line.clone())
                .unwrap_or_else(|| match capture {
                    CaptureMode::LearningText => "Drag to select text for the assistant.".into(),
                    CaptureMode::Region => "Drag a rectangle to capture a screenshot.".into(),
                    CaptureMode::None => "Ask about the open PDF.".into(),
                });
            ui.label(RichText::new(status).weak().size(12.0));
            ui.add_space(4.0);

            let streaming = app.tab().is_some_and(|t| t.assistant.streaming);
            let error = app.tab().and_then(|t| t.assistant.error.clone());
            let turns = app
                .tab()
                .map(|t| t.assistant.turns.clone())
                .unwrap_or_default();

            ScrollArea::vertical()
                .id_salt("assistant-transcript")
                .auto_shrink([false, false])
                .max_height(ui.available_height() - 180.0)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    if turns.is_empty() {
                        ui.label(
                            RichText::new(
                                "Attach selected text or a page region, then ask Cursor a question.",
                            )
                            .weak(),
                        );
                    }
                    for turn in &turns {
                        let who = match turn.role {
                            AssistantRole::User => "You",
                            AssistantRole::Assistant => "Cursor",
                        };
                        ui.label(RichText::new(who).strong().size(12.0));
                        match turn.role {
                            // User prompts stay plain so typed Markdown is not re-interpreted.
                            AssistantRole::User => {
                                ui.label(&turn.text);
                            }
                            AssistantRole::Assistant => {
                                // While streaming, prefer plain text so half-open fences stay readable.
                                if turn.incomplete {
                                    if !turn.text.is_empty() {
                                        ui.label(&turn.text);
                                    }
                                } else {
                                    crate::assistant::show_markdown(
                                        ui,
                                        &mut app.assistant_md_cache,
                                        &turn.text,
                                    );
                                }
                            }
                        }
                        if turn.incomplete {
                            ui.label(RichText::new("…").weak());
                        }
                        ui.add_space(8.0);
                    }
                });

            if let Some(err) = error {
                ui.colored_label(Color32::from_rgb(200, 80, 80), err);
            }

            ui.horizontal_wrapped(|ui| {
                if ui
                    .small_button("Select text")
                    .on_hover_text("Drag to select text and attach it (Ctrl+Shift+A)")
                    .clicked()
                {
                    begin_text = true;
                }
                if ui
                    .small_button("Screenshot")
                    .on_hover_text("Capture a page region (Ctrl+Alt+S)")
                    .clicked()
                {
                    begin_region = true;
                }
                if streaming {
                    if ui.small_button("Stop").on_hover_text("Ctrl+.").clicked() {
                        stop = true;
                    }
                }
            });

            let attachments: Vec<(String, Option<egui::TextureHandle>)> = app
                .tab()
                .map(|t| {
                    t.assistant
                        .attachments
                        .iter()
                        .map(|a| {
                            let tex = match a {
                                AssistantAttachment::Image { texture, .. } => texture.clone(),
                                _ => None,
                            };
                            (a.label(), tex)
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !attachments.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for (i, (label, tex)) in attachments.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if let Some(tex) = tex {
                                ui.image((tex.id(), Vec2::new(28.0, 28.0)));
                            }
                            let chip = ui.button(format!("{label} ×"));
                            if chip.clicked() {
                                remove_attach = Some(i);
                            }
                        });
                    }
                });
            }

            if let Some(tab) = app.tab_mut() {
                if !tab.assistant.disclosed {
                    ui.checkbox(
                        &mut tab.assistant.disclosed,
                        "I understand prompts and attachments go through my Cursor account",
                    );
                }
                ui.checkbox(
                    &mut tab.assistant.agent_mode,
                    "Agent mode (tools / writes)",
                )
                .on_hover_text(
                    "Off (default): Cursor Ask — read-only Q&A.\n\
                     On: full Cursor Agent — may use tools and write files in the session workspace. Opt-in only.",
                );
                if tab.assistant.agent_mode {
                    ui.label(
                        RichText::new("Agent mode can run tools and write files.")
                            .weak()
                            .size(11.0)
                            .color(Color32::from_rgb(180, 120, 40)),
                    );
                }
                let response = ui.add(
                    TextEdit::multiline(&mut tab.assistant.draft)
                        .id_salt("assistant-draft")
                        .desired_width(ui.available_width())
                        .desired_rows(3)
                        .hint_text("Ask Cursor…"),
                );
                let enter = response.has_focus()
                    && ui.input(|input| {
                        input.key_pressed(Key::Enter) && !input.modifiers.shift
                    });
                if enter {
                    // Consume the newline Enter would insert by trimming trailing newline.
                    if tab.assistant.draft.ends_with('\n') {
                        tab.assistant.draft.pop();
                    }
                    send = true;
                }
            }

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!streaming, Button::new("Send"))
                    .clicked()
                {
                    send = true;
                }
            });
        });

    if close {
        app.assistant_open = false;
        app.capture = CaptureMode::None;
    }
    if new_chat {
        app.assistant_new_chat();
    }
    if stop {
        app.assistant_stop();
    }
    if begin_text {
        app.begin_learning_select();
    }
    if begin_region {
        app.begin_region_capture();
    }
    if let Some(i) = remove_attach {
        if let Some(tab) = app.tab_mut() {
            if i < tab.assistant.attachments.len() {
                tab.assistant.attachments.remove(i);
            }
        }
    }
    if send {
        app.assistant_send();
    }
}

fn tool_bar(app: &mut MarkerApp, ctx: &egui::Context) {
    let colors = p(ctx);
    if app.tab().is_none() || !app.settings.toolbar_visible {
        return;
    }
    TopBottomPanel::top("tools")
        .exact_height(TOOL_BAR_H)
        .show_separator_line(false)
        .frame(
            egui::Frame::new()
                .fill(colors.chrome)
                .stroke(Stroke::NONE)
                .inner_margin(egui::Margin::symmetric(6, 2)),
        )
        .show(ctx, |ui| {
            let width_id = ui.id().with("cluster-width");
            let known = ui.data(|data| data.get_temp::<f32>(width_id));
            let lead = known
                .map(|width| ((ui.available_width() - width) * 0.5).max(0.0))
                .unwrap_or(0.0);
            let mut cluster_width = known.unwrap_or(0.0);
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
                if lead > 0.0 {
                    ui.add_space(lead);
                }
                let cluster = ui.scope_builder(
                    egui::UiBuilder::new().layout(Layout::left_to_right(Align::Center)),
                    |ui| {
                        ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
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
    let colors = p(ui.ctx());
    let selected = *current == tool;
    let (rect, response) = ui.allocate_exact_size(TOOL_CHIP, Sense::click());
    let fill = if selected {
        accent_fill(ui.ctx(), 48)
    } else if response.hovered() {
        Color32::from_white_alpha(16)
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(5), fill);
    }
    let icon = Rect::from_center_size(rect.center() + vec2(-2.5, -0.5), Vec2::splat(13.0));
    let color = if selected { colors.accent } else { colors.text };
    paint_tool_icon(ui.painter(), icon, tool, color);
    let key_color = if selected { colors.accent } else { colors.text_dim };
    ui.painter().text(
        rect.right_bottom() + vec2(-1.0, 0.0),
        Align2::RIGHT_BOTTOM,
        tool.shortcut().name(),
        FontId::new(8.0, FontFamily::Proportional),
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

fn paint_tab_menu(app: &mut MarkerApp, ctx: &egui::Context) {
    let Some((index, pos)) = app.tab_menu else {
        return;
    };
    if index >= app.tabs.len() {
        app.tab_menu = None;
        return;
    }
    let can_split = app.tabs.len() >= 2;
    let split_open = app.split.is_some();
    let mut split_side = false;
    let mut split_stack = false;
    let mut unsplit = false;
    let mut close_menu = false;
    let with_label = if index == app.active {
        let other = (app.active + 1) % app.tabs.len();
        app.tabs.get(other).and_then(|tab| {
            tab.doc
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| format!(" with {name}"))
        })
    } else {
        None
    };

    let area = egui::Area::new(Id::new("marker-tab-menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .interactable(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(168.0);
                if can_split {
                    let side = format!(
                        "Split side-by-side{}",
                        with_label.as_deref().unwrap_or("")
                    );
                    let stack =
                        format!("Split stacked{}", with_label.as_deref().unwrap_or(""));
                    if ui.button(side).clicked() {
                        split_side = true;
                    }
                    if ui.button(stack).clicked() {
                        split_stack = true;
                    }
                }
                if split_open && ui.button("Unsplit").clicked() {
                    unsplit = true;
                }
                if !can_split && !split_open {
                    ui.label(RichText::new("Open another tab to split").weak().size(12.0));
                }
            });
        });

    // Primary-only dismiss: the opening right-click is an `any_click` on the
    // same frame the menu first appears, which would flash-close it otherwise.
    let outside = ctx.input(|input| input.pointer.button_clicked(PointerButton::Primary))
        && !area.response.hovered()
        && !area.response.clicked();
    if split_side {
        app.split_from_tab(index, false);
        close_menu = true;
    } else if split_stack {
        app.split_from_tab(index, true);
        close_menu = true;
    } else if unsplit {
        app.unsplit();
        close_menu = true;
    } else if outside {
        close_menu = true;
    }
    if close_menu {
        app.tab_menu = None;
    }
}

/// Edge / pane drop targets while a tab is being dragged to create or adjust a split.
pub(crate) fn tab_drag_overlay(app: &mut MarkerApp, ctx: &egui::Context) {
    let Some(dragged) = app.tab_drag else {
        return;
    };
    if dragged >= app.tabs.len() || app.tabs.len() < 2 {
        app.tab_drag = None;
        return;
    }

    let pointer = ctx.pointer_interact_pos().or_else(|| ctx.pointer_latest_pos());
    let primary_down = ctx.input(|input| input.pointer.primary_down());
    let released = ctx.input(|input| input.pointer.primary_released());

    if !primary_down && !released {
        app.tab_drag = None;
        return;
    }

    let zone = pointer.and_then(|pos| hit_split_drop_zone(app, pos));
    if let Some(pos) = pointer {
        paint_tab_drag_feedback(app, ctx, dragged, pos, zone);
    }

    if released {
        if let Some(zone) = zone {
            app.apply_tab_drop(dragged, zone);
        }
        app.tab_drag = None;
    } else {
        ctx.request_repaint();
    }
}

fn hit_split_drop_zone(app: &MarkerApp, pos: Pos2) -> Option<SplitDropZone> {
    if let Some(split) = app.split {
        let other = app.split_view_rect;
        let active = app.view_rect;
        let full = if active.is_positive() && other.is_positive() {
            active.union(other)
        } else if active.is_positive() {
            active
        } else if other.is_positive() {
            other
        } else {
            return None;
        };
        if full.contains(pos) {
            // Prefer outer edges so drag can still flip side-by-side ↔ stacked.
            if let Some(edge) = edge_drop_zone(full, pos) {
                let flips = match edge {
                    SplitDropZone::Left | SplitDropZone::Right => split.stacked,
                    SplitDropZone::Top | SplitDropZone::Bottom => !split.stacked,
                    SplitDropZone::OtherPane | SplitDropZone::ActivePane => false,
                };
                if flips {
                    return Some(edge);
                }
            }
            if other.is_positive() && other.contains(pos) {
                return Some(SplitDropZone::OtherPane);
            }
            if active.is_positive() && active.contains(pos) {
                return Some(SplitDropZone::ActivePane);
            }
        }
        return None;
    }

    let view = app.view_rect;
    if !view.is_positive() || !view.contains(pos) {
        return None;
    }
    edge_drop_zone(view, pos)
}

fn edge_drop_zone(full: Rect, pos: Pos2) -> Option<SplitDropZone> {
    let w = full.width().max(1.0);
    let h = full.height().max(1.0);
    let rel_x = ((pos.x - full.left()) / w).clamp(0.0, 1.0);
    let rel_y = ((pos.y - full.top()) / h).clamp(0.0, 1.0);
    let band_x = 0.28;
    let band_y = 0.28;
    let candidates = [
        (rel_x, band_x, SplitDropZone::Left),
        (1.0 - rel_x, band_x, SplitDropZone::Right),
        (rel_y, band_y, SplitDropZone::Top),
        (1.0 - rel_y, band_y, SplitDropZone::Bottom),
    ];
    candidates
        .into_iter()
        .filter(|(dist, band, _)| *dist <= *band)
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, _, zone)| zone)
}

fn paint_tab_drag_feedback(
    app: &MarkerApp,
    ctx: &egui::Context,
    dragged: usize,
    pos: Pos2,
    zone: Option<SplitDropZone>,
) {
    let colors = p(ctx);
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        Id::new("marker-tab-drag"),
    ));

    if let Some(zone) = zone {
        let highlight = drop_zone_rect(app, zone);
        if highlight.is_positive() {
            painter.rect_filled(
                highlight,
                CornerRadius::same(4),
                colors.accent.gamma_multiply(0.22),
            );
            painter.rect_stroke(
                highlight,
                CornerRadius::same(4),
                Stroke::new(1.5, colors.accent.gamma_multiply(0.85)),
                StrokeKind::Inside,
            );
        }
    }

    let name = app
        .tabs
        .get(dragged)
        .and_then(|tab| tab.doc.path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("document.pdf");
    let galley = painter.layout_no_wrap(
        name.to_owned(),
        FontId::new(12.5, FontFamily::Proportional),
        colors.text,
    );
    let pill = Rect::from_center_size(
        pos + vec2(12.0, 14.0),
        Vec2::new(galley.size().x + 16.0, TAB_PILL_H),
    );
    painter.rect_filled(pill, CornerRadius::same(6), colors.chrome_raised);
    painter.rect_stroke(
        pill,
        CornerRadius::same(6),
        Stroke::new(1.0, colors.hairline),
        StrokeKind::Inside,
    );
    painter.galley(
        Pos2::new(pill.left() + 8.0, pill.center().y - galley.size().y * 0.5),
        galley,
        colors.text,
    );
}

fn drop_zone_rect(app: &MarkerApp, zone: SplitDropZone) -> Rect {
    match zone {
        SplitDropZone::OtherPane => app.split_view_rect,
        SplitDropZone::ActivePane => app.view_rect,
        SplitDropZone::Left
        | SplitDropZone::Right
        | SplitDropZone::Top
        | SplitDropZone::Bottom => {
            let full = if app.split.is_some() {
                let a = app.view_rect;
                let b = app.split_view_rect;
                if a.is_positive() && b.is_positive() {
                    a.union(b)
                } else if a.is_positive() {
                    a
                } else {
                    b
                }
            } else {
                app.view_rect
            };
            if !full.is_positive() {
                return Rect::NOTHING;
            }
            let mid_x = full.center().x;
            let mid_y = full.center().y;
            match zone {
                SplitDropZone::Left => {
                    Rect::from_min_max(full.min, Pos2::new(mid_x, full.bottom()))
                }
                SplitDropZone::Right => {
                    Rect::from_min_max(Pos2::new(mid_x, full.top()), full.max)
                }
                SplitDropZone::Top => {
                    Rect::from_min_max(full.min, Pos2::new(full.right(), mid_y))
                }
                SplitDropZone::Bottom => {
                    Rect::from_min_max(Pos2::new(full.left(), mid_y), full.max)
                }
                SplitDropZone::OtherPane | SplitDropZone::ActivePane => unreachable!(),
            }
        }
    }
}

pub(crate) fn split_viewports(app: &mut MarkerApp, ui: &mut egui::Ui, split: SplitState) {
    let colors = p(ui.ctx());
    let full = ui.available_rect_before_wrap();
    let gap = 4.0;
    let handle = 5.0;
    let ratio = split.ratio.clamp(0.2, 0.8);

    let (first_rect, second_rect, handle_rect) = if split.stacked {
        let h = full.height();
        let top_h = ((h - handle) * ratio).clamp(80.0, (h - handle - 80.0).max(80.0));
        let a = Rect::from_min_max(full.min, Pos2::new(full.right(), full.top() + top_h));
        let hr = Rect::from_min_max(
            Pos2::new(full.left(), a.bottom()),
            Pos2::new(full.right(), a.bottom() + handle),
        );
        let b = Rect::from_min_max(Pos2::new(full.left(), hr.bottom() + gap * 0.0), full.max);
        (a, b, hr)
    } else {
        let w = full.width();
        let left_w = ((w - handle) * ratio).clamp(120.0, (w - handle - 120.0).max(120.0));
        let a = Rect::from_min_max(full.min, Pos2::new(full.left() + left_w, full.bottom()));
        let hr = Rect::from_min_max(
            Pos2::new(a.right(), full.top()),
            Pos2::new(a.right() + handle, full.bottom()),
        );
        let b = Rect::from_min_max(Pos2::new(hr.right(), full.top()), full.max);
        (a, b, hr)
    };

    let first = split.first;
    let second = split.second;
    if !split.contains(app.active) {
        app.active = first;
    }
    let focus_first = app.active == first;
    ui.scope_builder(egui::UiBuilder::new().max_rect(first_rect), |ui| {
        viewport_tab(app, ui, first, focus_first);
    });

    let response = ui.interact(handle_rect, Id::new("marker-split-handle"), Sense::drag());
    ui.painter()
        .rect_filled(handle_rect, 0.0, colors.chrome);
    if response.hovered() || response.dragged() {
        ui.painter().rect_filled(
            handle_rect.shrink(1.0),
            CornerRadius::same(2),
            colors.accent.gamma_multiply(0.55),
        );
        ui.ctx().set_cursor_icon(if split.stacked {
            egui::CursorIcon::ResizeVertical
        } else {
            egui::CursorIcon::ResizeHorizontal
        });
    }
    if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some(split) = app.split.as_mut() {
                if split.stacked {
                    split.ratio = ((pos.y - full.top()) / full.height()).clamp(0.2, 0.8);
                } else {
                    split.ratio = ((pos.x - full.left()) / full.width()).clamp(0.2, 0.8);
                }
            }
        }
    }

    ui.scope_builder(egui::UiBuilder::new().max_rect(second_rect), |ui| {
        viewport_tab(app, ui, second, !focus_first);
    });
    app.dispatch_tiles(ui.ctx().pixels_per_point());
}

fn vbar(ui: &mut egui::Ui) {
    let colors = p(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(7.0, 14.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.top()..=rect.bottom(),
        Stroke::new(1.0, colors.hairline),
    );
}

pub(crate) fn empty_state(app: &mut MarkerApp, ui: &mut egui::Ui) {
    let colors = p(ui.ctx());
    let rect = ui.available_rect_before_wrap();
    ui.painter().rect_filled(rect, 0.0, colors.backdrop);
    let recent: Vec<_> = app.settings.recent.clone();
    let mut open_path = None;
    let mut forget = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ScrollArea::vertical()
            .id_salt("empty-recent")
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.16).clamp(24.0, 120.0));
                    brand_wordmark(ui);
                    ui.add_space(10.0);
                    ui.label(RichText::new("Drop a PDF here, or open one.").weak());
                    ui.add_space(18.0);
                    if ui
                        .add(
                            Button::new(
                                RichText::new("Open PDF").color(Color32::from_rgb(14, 18, 28)),
                            )
                            .fill(colors.accent)
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
                            "Ctrl+O  ·  Ctrl+F search  ·  Ctrl+Shift+V paste image  ·  pinch zoom",
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
                        ui.label(RichText::new("Recent").size(13.0).color(colors.text_dim));
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
                            let name_color = if exists { colors.text } else { colors.text_dim };
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
                                    colors.text_dim,
                                );
                            }
                            let mut remove_clicked = false;
                            if response.hovered() {
                                let close_rect = Rect::from_center_size(
                                    Pos2::new(rect.right() - 16.0, rect.center().y),
                                    Vec2::splat(18.0),
                                );
                                let close = ui.interact(
                                    close_rect,
                                    ui.id().with(("recent-forget", path.as_os_str())),
                                    Sense::click(),
                                );
                                let close_color = if close.hovered() { colors.text } else { colors.text_dim };
                                ui.painter().text(
                                    close_rect.center(),
                                    Align2::CENTER_CENTER,
                                    "×",
                                    FontId::new(14.0, FontFamily::Proportional),
                                    close_color,
                                );
                                if close
                                    .on_hover_text("Remove from recent")
                                    .clicked()
                                {
                                    remove_clicked = true;
                                    forget = Some(path.clone());
                                }
                            }
                            let response = if parent.is_empty() {
                                response
                            } else {
                                response.on_hover_text(format!(
                                    "{parent}\nRight-click or × to remove"
                                ))
                            };
                            if response.clicked() && !remove_clicked {
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
    let colors = p(ui.ctx());
    let fill = if selected {
        accent_fill(ui.ctx(), 42)
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

pub(crate) fn color_dot(ui: &mut egui::Ui, color: Rgb, selected: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::click());
    ui.painter()
        .circle_filled(rect.center(), 5.0, color.to_color32());
    if selected {
        ui.painter()
            .circle_stroke(rect.center(), 6.5, Stroke::new(1.35, Color32::WHITE));
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
            "../assets/fonts/InstrumentSerif-Italic.ttf"
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

fn brand_wordmark(ui: &mut egui::Ui) {
    let colors = p(ui.ctx());
    let font = FontId::new(72.0, brand_family());
    let galley = ui.fonts_mut(|fonts| {
        fonts.layout_no_wrap("Marker".to_owned(), font, colors.text)
    });
    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
    let text_pos = Pos2::new(
        rect.center().x - galley.size().x * 0.5,
        rect.top(),
    );
    ui.painter().galley(text_pos, galley, colors.text);
}

fn accent_fill(ctx: &egui::Context, alpha: u8) -> Color32 {
    theme::accent_fill(ctx, alpha)
}

fn vec2(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}
