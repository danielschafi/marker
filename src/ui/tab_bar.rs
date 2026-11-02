//! Tab bar, open-tabs list, document chrome controls, and tab context menu (RF12).

use egui::{
    Align, Align2, Color32, CornerRadius, DragValue, FontFamily, FontId, Id, Layout, PointerButton,
    Pos2, Rect, RichText, Sense, Stroke, TopBottomPanel, Vec2, WidgetInfo, WidgetType,
};

use crate::app::{MarkerApp, SaveState, TabId, TabListState};
use crate::geom::zoom_percent;
use crate::theme;

use super::toolbar::TOOL_BAR_H;
use super::{
    chrome_button, cluster_button, cluster_drag, cluster_sep, control_cluster, palette, CONTROL_H,
};

pub(super) const TAB_BAR_H: f32 = 32.0;
pub(super) const TAB_PILL_H: f32 = 20.0;

pub(super) fn tab_bar(app: &mut MarkerApp, ctx: &egui::Context) {
    let colors = palette(ctx);
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
            ui.spacing_mut().item_spacing = Vec2::new(3.0, 0.0);
            ui.spacing_mut().button_padding = Vec2::new(6.0, 2.0);
            let row_h = ui.available_height();
            let full = ui.available_rect_before_wrap();
            let (_, row_response) =
                ui.allocate_exact_size(Vec2::new(full.width(), row_h), Sense::hover());
            let row = row_response.rect;

            let center_reserve = 160.0;
            let left_rect = Rect::from_min_max(
                row.left_top(),
                Pos2::new(
                    (row.center().x - center_reserve).max(row.left() + 8.0),
                    row.bottom(),
                ),
            );
            let right_rect = Rect::from_min_max(
                Pos2::new(
                    (row.center().x + center_reserve).min(row.right() - 8.0),
                    row.top(),
                ),
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
}

fn chrome_nav(app: &mut MarkerApp, ui: &mut egui::Ui) {
    if chrome_button(ui, "Open", false)
        .on_hover_text("Open a PDF (Ctrl+O)")
        .clicked()
    {
        app.open_dialog();
    }
    if app.tab().is_some()
        && chrome_button(ui, "Save As", false)
            .on_hover_text("Save a copy and switch to it (Ctrl+Shift+S)")
            .clicked()
    {
        app.save_as_dialog();
    }
    if app.tab().is_some()
        && chrome_button(ui, "Export", false)
            .on_hover_text("Export highlights to Markdown")
            .clicked()
    {
        app.export_markdown();
    }
    if app.tab().is_some()
        && chrome_button(ui, "Cards", false)
            .on_hover_text("Export highlights as CSV for Anki import (cloze or front/back)")
            .clicked()
    {
        app.export_cards();
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
    let thumbs_on = app.tab().map(|tab| tab.thumbs_open).unwrap_or(false);
    if app.tab().is_some()
        && chrome_button(ui, "Thumbs", thumbs_on)
            .on_hover_text("Page thumbnails")
            .clicked()
    {
        if let Some(tab) = app.tab_mut() {
            tab.thumbs_open = !tab.thumbs_open;
        }
    }
    let annots_on = app.tab().map(|tab| tab.annots_open).unwrap_or(false);
    if app.tab().is_some()
        && chrome_button(ui, "Annots", annots_on)
            .on_hover_text("Annotation list — filter, search, and jump")
            .clicked()
    {
        if let Some(tab) = app.tab_mut() {
            tab.annots_open = !tab.annots_open;
        }
    }
    let review_on = app.tab().map(|tab| tab.review_open).unwrap_or(false);
    if app.tab().is_some()
        && chrome_button(ui, "Review", review_on)
            .on_hover_text("Review highlights and notes by preset or outline")
            .clicked()
    {
        if let Some(tab) = app.tab_mut() {
            tab.review_open = !tab.review_open;
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
        if chrome_button(ui, "Colors", app.preset_editor_open)
            .on_hover_text("Edit named highlight color presets")
            .clicked()
        {
            app.toggle_preset_editor();
        }
        if chrome_button(ui, "Zen", app.zen)
            .on_hover_text("Fullscreen reading; chrome hides until the pointer hits the top (F11)")
            .clicked()
        {
            app.toggle_zen(ui.ctx());
        }
        if chrome_button(ui, "Assistant", app.assistant_open)
            .on_hover_text(
                "Cursor learning assistant (Ctrl+Alt+I). Hiding the panel keeps the session alive.",
            )
            .clicked()
        {
            app.toggle_assistant();
        }
    }
}

fn paint_current_title(app: &mut MarkerApp, ui: &mut egui::Ui, row: Rect) {
    let colors = palette(ui.ctx());
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
    let name = tab_file_name(tab).to_owned();
    let dirty = tab.doc.session.is_dirty();
    let tab_count = app.tabs.len();
    let list_open = app.tab_list.is_some();

    let title_color = colors.text;
    let galley = ui.painter().layout_no_wrap(
        name.clone(),
        FontId::new(13.0, FontFamily::Proportional),
        title_color,
    );
    let mut width = galley.size().x + 20.0;
    if dirty {
        width += 12.0;
    }
    let max_w = (row.width() * 0.38).clamp(120.0, 420.0);
    width = width.min(max_w);

    let title_rect = Rect::from_center_size(row.center(), Vec2::new(width, TAB_PILL_H + 4.0));
    let sense = if tab_count > 1 {
        Sense::click_and_drag()
    } else {
        Sense::click()
    };
    let response = ui.interact(title_rect, Id::new("marker-current-tab"), sense);
    let fill = if list_open || response.dragged() {
        colors.chrome_raised
    } else if response.hovered() && tab_count > 1 {
        theme::chrome_overlay(ui.ctx(), 14)
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

    if response.secondary_clicked() {
        let pos = response
            .interact_pointer_pos()
            .or_else(|| ui.ctx().pointer_latest_pos())
            .unwrap_or(title_rect.center());
        app.tab_menu = Some((app.active, pos));
    } else if response.drag_started() && tab_count > 1 {
        app.tab_drag = Some(app.active);
        app.tab_menu = None;
        if app.tab_list.is_none() {
            app.tab_list = Some(TabListState::Pinned);
        }
    } else if response.clicked() {
        app.toggle_tab_list();
    }
    if response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    let tip = if tab_count > 1 {
        "Open tabs · drag to split · right-click to split · Ctrl+Tab · Ctrl+\\"
    } else {
        "Current document"
    };
    let tab_label = if tab_count > 1 {
        format!("{name} tab")
    } else {
        name.clone()
    };
    response.widget_info(|| {
        WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), tab_label.clone())
    });
    response.on_hover_text(tip);
}

pub(super) fn tab_file_name(tab: &crate::app::Tab) -> &str {
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
    hovered: bool,
    pointer: Option<Pos2>,
}

fn tab_list_row(
    ui: &mut egui::Ui,
    name: &str,
    selected: bool,
    dirty: bool,
    index: usize,
    row_width: f32,
    drop_target: bool,
) -> TabAction {
    let colors = palette(ui.ctx());
    let color = if selected {
        colors.text
    } else {
        colors.text_dim
    };
    let galley = ui.painter().layout_no_wrap(
        name.to_owned(),
        FontId::new(12.5, FontFamily::Proportional),
        color,
    );
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(row_width, TAB_LIST_ROW_H),
        Sense::click_and_drag(),
    );
    let fill = if drop_target {
        colors.accent.gamma_multiply(0.28)
    } else if selected {
        theme::accent_fill(ui.ctx(), 36)
    } else if response.hovered() || response.dragged() {
        theme::chrome_overlay(ui.ctx(), 14)
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
        close.widget_info(|| {
            WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), format!("Close {name}"))
        });
        let close = close.on_hover_text("Close tab (Ctrl+W)");
        close_clicked = close.clicked() || close.middle_clicked();
    }

    if response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    response
        .widget_info(|| WidgetInfo::selected(WidgetType::Button, ui.is_enabled(), selected, name));
    let response = response.on_hover_text("Drag to split · right-click for layout");

    TabAction {
        select: response.clicked() && !close_clicked,
        close: close_clicked || response.middle_clicked(),
        menu: response.secondary_clicked(),
        drag: response.drag_started() && !close_clicked,
        hovered: response.hovered() || response.dragged(),
        pointer: response.interact_pointer_pos(),
    }
}

const TAB_LIST_ROW_H: f32 = 26.0;

pub(super) fn paint_tab_list(app: &mut MarkerApp, ctx: &egui::Context, show_chrome: bool) {
    let Some(state) = app.tab_list else {
        app.tab_drag_over = None;
        return;
    };
    if app.tabs.is_empty() {
        app.tab_list = None;
        app.tab_drag_over = None;
        return;
    }

    let dragging = app.tab_drag.is_some();
    let drop_over = app.tab_drag_over;
    let now = std::time::Instant::now();
    if let TabListState::Ephemeral { until } = state {
        if !dragging && now >= until {
            app.tab_list = None;
            app.tab_drag_over = None;
            return;
        }
        if !dragging {
            ctx.request_repaint_after(until.saturating_duration_since(now));
        }
    }

    let colors = palette(ctx);
    let mut close = None;
    let mut select = None;
    let mut tab_menu = None;
    let mut drag_tab = None;
    let mut keep_alive = false;
    let names: Vec<(TabId, String, bool, bool)> = app
        .tabs
        .iter()
        .map(|tab| {
            (
                tab.id,
                tab_file_name(tab).to_owned(),
                tab.id == app.active,
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

    let pointer = ctx
        .pointer_interact_pos()
        .or_else(|| ctx.pointer_latest_pos());
    let drag_src = app.tab_drag;

    let area = egui::Area::new(Id::new("marker-tab-list"))
        .order(egui::Order::Foreground)
        .anchor(Align2::RIGHT_TOP, Vec2::new(-14.0, top))
        .interactable(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(colors.chrome)
                .stroke(Stroke::new(1.0_f32, colors.hairline))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    ui.set_width(row_width);
                    ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
                    for (id, name, selected, dirty) in &names {
                        let is_drop =
                            drop_over == Some(*id) && drag_src.is_some_and(|src| src != *id);
                        let row_index = app.tab_index(*id).unwrap_or(0);
                        let action = tab_list_row(
                            ui, name, *selected, *dirty, row_index, row_width, is_drop,
                        );
                        if action.hovered {
                            keep_alive = true;
                        }
                        if action.select && !dragging {
                            select = Some(*id);
                        }
                        if action.close && !dragging {
                            close = Some(*id);
                        }
                        if action.drag {
                            drag_tab = Some(*id);
                        }
                        if action.menu && !dragging {
                            tab_menu = action.pointer.map(|pos| (*id, pos)).or_else(|| {
                                Some((*id, ui.ctx().pointer_latest_pos().unwrap_or(Pos2::ZERO)))
                            });
                        }
                    }
                });
        });

    // Geometric hit-test so list drops work while the drag owns the pointer.
    app.tab_drag_over = None;
    if let (Some(pos), Some(src)) = (pointer, drag_src) {
        if area.response.rect.contains(pos) {
            let inner = area.response.rect.shrink(6.0);
            let mut y = inner.top();
            for (id, _, _, _) in &names {
                let row = Rect::from_min_max(
                    Pos2::new(inner.left(), y),
                    Pos2::new(inner.right(), y + TAB_LIST_ROW_H),
                );
                if row.contains(pos) && *id != src {
                    app.tab_drag_over = Some(*id);
                    break;
                }
                y += TAB_LIST_ROW_H + 2.0;
            }
        }
    }

    if keep_alive || dragging {
        if let Some(TabListState::Ephemeral { .. }) = app.tab_list {
            app.tab_list = Some(TabListState::Ephemeral {
                until: now + std::time::Duration::from_millis(2500),
            });
        }
    }

    if let Some(id) = select {
        app.activate_tab(id);
    }
    if let Some(id) = close {
        app.close_tab(id);
    }
    if let Some(id) = drag_tab {
        if app.tabs.len() >= 2 {
            app.tab_drag = Some(id);
            app.tab_menu = None;
        }
    }
    if let Some(menu) = tab_menu {
        app.tab_menu = Some(menu);
    }

    let pinned = matches!(app.tab_list, Some(TabListState::Pinned));
    // Title / indicator toggles already ran this frame; ignore those clicks for dismiss.
    let chrome_toggle = ["marker-current-tab", "marker-tabs-indicator"]
        .iter()
        .any(|id| ctx.read_response(Id::new(*id)).is_some_and(|r| r.clicked()));
    let outside = pinned
        && !dragging
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

fn document_controls(app: &mut MarkerApp, ui: &mut egui::Ui) {
    let colors = palette(ui.ctx());
    let page_info = app.tab().map(|tab| {
        let count = tab.doc.pages.len().max(1);
        let page = tab.doc.current_page(app.view_rect.height().max(1.0)) + 1;
        (page, count)
    });
    let want_page_focus = app.page_focus;
    let dirty = app.tab().is_some_and(|tab| tab.doc.session.is_dirty());
    let notice = app.tab().and_then(|tab| match &tab.save {
        SaveState::Saving => Some((
            "Saving…".to_string(),
            colors.accent,
            "Writing changes to disk…".to_string(),
        )),
        SaveState::Failed { message, .. } => {
            let fail = Color32::from_rgb(230, 120, 110);
            Some((message.clone(), fail, message.clone()))
        }
        _ => None,
    });
    let error = app.error.clone();
    let mut jump = None;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 0.0);
        // Rightmost: compact open-tabs affordance (list anchors top-right).
        if app.tabs.len() >= 2 {
            tabs_indicator_button(app, ui);
        }
        if app.doc().is_some() {
            if ui
                .checkbox(&mut app.settings.sepia, "Sepia")
                .on_hover_text("Warm page background for reading")
                .changed()
            {
                app.settings.save();
            }
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
        if let Some((label, color, hover)) = notice {
            ui.label(RichText::new(label).color(color).size(12.0))
                .on_hover_text(hover);
        } else if dirty {
            ui.label(RichText::new("Modified").color(colors.dirty).size(12.0))
                .on_hover_text("Unsaved changes — will be written automatically");
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

fn tabs_indicator_button(app: &mut MarkerApp, ui: &mut egui::Ui) {
    let colors = palette(ui.ctx());
    let list_open = app.tab_list.is_some();
    let label = format!("{} ▾", app.tabs.len());
    let text_w = ui_measure_text(ui.ctx(), &label, 11.5);
    let width = (text_w + 14.0).clamp(36.0, 56.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, CONTROL_H), Sense::hover());
    // Stable id so outside-click dismiss can ignore this toggle.
    let response = ui.interact(rect, Id::new("marker-tabs-indicator"), Sense::click());
    let fill = if list_open {
        theme::accent_fill(ui.ctx(), 42)
    } else if response.is_pointer_button_down_on() {
        theme::chrome_overlay(ui.ctx(), 28)
    } else if response.hovered() {
        theme::chrome_overlay(ui.ctx(), 16)
    } else {
        colors.chrome_raised
    };
    ui.painter().rect_filled(rect, CornerRadius::same(5), fill);
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
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), "Open tabs"));
    response.on_hover_text("Open tabs · Ctrl+Tab to cycle");
}

pub(super) fn paint_tab_menu(app: &mut MarkerApp, ctx: &egui::Context) {
    let Some((menu_id, pos)) = app.tab_menu else {
        return;
    };
    if app.tab_index(menu_id).is_none() {
        app.tab_menu = None;
        return;
    }
    let can_split = app.tabs.len() >= 2;
    let split_open = app.split.is_some();
    let active = app.active;
    let mut split_side = false;
    let mut split_stack = false;
    let mut split_with: Option<(TabId, bool)> = None;
    let mut unsplit = false;
    let mut move_window = false;
    let mut close_menu = false;
    let others: Vec<(TabId, String)> = app
        .tabs
        .iter()
        .filter(|tab| tab.id != menu_id)
        .map(|tab| (tab.id, tab_file_name(tab).to_owned()))
        .collect();
    let next_id = app
        .tab_index(active)
        .map(|i| app.tabs[(i + 1) % app.tabs.len().max(1)].id);
    let next_label = others
        .iter()
        .find(|(id, _)| Some(*id) == next_id)
        .or_else(|| others.first())
        .map(|(_, name)| format!(" with {name}"));

    let area = egui::Area::new(Id::new("marker-tab-menu"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .interactable(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(188.0);
                if can_split {
                    if menu_id == active && others.len() > 1 {
                        ui.menu_button("Split side-by-side with…", |ui| {
                            for (other, name) in &others {
                                if ui.button(name).clicked() {
                                    split_with = Some((*other, false));
                                    ui.close();
                                }
                            }
                        });
                        ui.menu_button("Split stacked with…", |ui| {
                            for (other, name) in &others {
                                if ui.button(name).clicked() {
                                    split_with = Some((*other, true));
                                    ui.close();
                                }
                            }
                        });
                    } else {
                        let suffix = if menu_id == active {
                            next_label.as_deref().unwrap_or("")
                        } else {
                            ""
                        };
                        if ui.button(format!("Split side-by-side{suffix}")).clicked() {
                            split_side = true;
                        }
                        if ui.button(format!("Split stacked{suffix}")).clicked() {
                            split_stack = true;
                        }
                    }
                }
                if split_open && ui.button("Unsplit").clicked() {
                    unsplit = true;
                }
                if ui.button("Move to new window").clicked() {
                    move_window = true;
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
    if let Some((other, stacked)) = split_with {
        if menu_id == app.active {
            app.split_with(other, stacked);
        } else {
            app.active = menu_id;
            app.split_with(other, stacked);
        }
        close_menu = true;
    } else if split_side {
        app.split_from_tab(menu_id, false);
        close_menu = true;
    } else if split_stack {
        app.split_from_tab(menu_id, true);
        close_menu = true;
    } else if unsplit {
        app.unsplit();
        close_menu = true;
    } else if move_window {
        app.move_tab_to_new_window(menu_id);
        close_menu = true;
    } else if outside {
        close_menu = true;
    }
    if close_menu {
        app.tab_menu = None;
    }
}
