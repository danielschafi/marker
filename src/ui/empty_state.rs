//! Empty-document landing screen and outline tree rows (RF12).

use std::path::{Path, PathBuf};

use egui::{
    Align2, Button, Color32, CornerRadius, FontFamily, FontId, Id, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, Vec2, WidgetInfo, WidgetType,
};

use crate::app::MarkerApp;
use crate::pdf::OutlineNode;
use crate::theme;

use super::{brand_wordmark, palette};

/// `is_file` results for the empty-state recent list.
///
/// Refreshed when the path list changes (settings load, open, forget) and when
/// the window gains focus. A missing path stays in the list.
#[derive(Clone, Default)]
struct RecentExistsCache {
    paths: Vec<PathBuf>,
    exists: Vec<bool>,
    focused: bool,
    seen_focus: bool,
}

fn sync_recent_exists(
    cache: &mut RecentExistsCache,
    recent: &[PathBuf],
    focus_gain: bool,
    mut is_file: impl FnMut(&Path) -> bool,
) {
    if cache.paths.as_slice() == recent {
        if !focus_gain {
            return;
        }
        for (path, slot) in cache.paths.iter().zip(cache.exists.iter_mut()) {
            *slot = is_file(path);
        }
        return;
    }
    cache.exists.clear();
    cache.exists.reserve(recent.len());
    for path in recent {
        cache.exists.push(is_file(path));
    }
    cache.paths.clear();
    cache.paths.extend(recent.iter().cloned());
}

fn recent_file_flags(ctx: &egui::Context, recent: &[PathBuf]) -> Vec<bool> {
    let id = Id::new("marker-recent-file-exists");
    let (focused, event_gain) = ctx.input(|input| {
        let focused = input.viewport().focused.unwrap_or(false);
        let event_gain = input
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::WindowFocused(true)));
        (focused, event_gain)
    });
    let mut cache = ctx
        .data(|data| data.get_temp::<RecentExistsCache>(id))
        .unwrap_or_default();
    let focus_gain = event_gain || (cache.seen_focus && focused && !cache.focused);
    sync_recent_exists(&mut cache, recent, focus_gain, Path::is_file);
    cache.focused = focused;
    cache.seen_focus = true;
    let exists = cache.exists.clone();
    ctx.data_mut(|data| data.insert_temp(id, cache));
    exists
}

pub(crate) fn empty_state(app: &mut MarkerApp, ui: &mut egui::Ui) {
    let colors = palette(ui.ctx());
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
                            "Ctrl+O  ·  Ctrl+F search  ·  Ctrl+Shift+V paste image  ·  ? shortcuts",
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
                        let list_width = ui.available_width().clamp(280.0, 440.0);
                        let exists_flags = recent_file_flags(ui.ctx(), &recent);
                        for (path, exists) in recent.iter().zip(exists_flags) {
                            let name = path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("document.pdf");
                            let parent = path
                                .parent()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default();
                            let (rect, response) =
                                ui.allocate_exact_size(Vec2::new(list_width, 44.0), Sense::click());
                            let fill = if response.hovered() {
                                theme::chrome_overlay(ui.ctx(), 18)
                            } else {
                                theme::chrome_overlay(ui.ctx(), 10)
                            };
                            ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
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
                                    if exists { parent.as_str() } else { "(missing)" },
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
                                let close_color = if close.hovered() {
                                    colors.text
                                } else {
                                    colors.text_dim
                                };
                                ui.painter().text(
                                    close_rect.center(),
                                    Align2::CENTER_CENTER,
                                    "×",
                                    FontId::new(14.0, FontFamily::Proportional),
                                    close_color,
                                );
                                if close.on_hover_text("Remove from recent").clicked() {
                                    remove_clicked = true;
                                    forget = Some(path.clone());
                                }
                            }
                            let tip = if parent.is_empty() {
                                format!("Open {name}")
                            } else {
                                format!("{parent}\nRight-click or × to remove")
                            };
                            response.widget_info(|| {
                                WidgetInfo::labeled(
                                    WidgetType::Button,
                                    ui.is_enabled(),
                                    format!("Open recent file {name}"),
                                )
                            });
                            let response = response.on_hover_text(tip);
                            if response.clicked() && !remove_clicked {
                                if exists {
                                    open_path = Some(path.clone());
                                } else {
                                    forget = Some(path.clone());
                                    app.error = Some(format!("File not found: {}", path.display()));
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

pub(super) fn outline_node(
    ui: &mut egui::Ui,
    node: &OutlineNode,
    depth: usize,
    jump: &mut Option<(usize, Option<f32>)>,
) {
    if node.children.is_empty() {
        let available = ui.available_width();
        if ui
            .add(
                Button::new(&node.title)
                    .frame(false)
                    .wrap()
                    .min_size(Vec2::new(available, 0.0)),
            )
            .clicked()
        {
            if let Some(page) = node.page {
                *jump = Some((page, node.y));
            }
        }
        return;
    }

    let id = ui.make_persistent_id((
        "outline-node",
        depth,
        node.page,
        node.y.map(f32::to_bits),
        &node.title,
    ));
    let mut state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, depth < 1);
    let header = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let toggle = state.show_toggle_button(ui, egui::collapsing_header::paint_default_icon);
        let title = ui.add(
            Button::new(&node.title)
                .frame(false)
                .wrap()
                .min_size(Vec2::new(ui.available_width(), 0.0)),
        );
        (toggle, title)
    });
    if header.inner.1.clicked() {
        state.toggle(ui);
        if let Some(page) = node.page {
            *jump = Some((page, node.y));
        }
    }
    state.show_body_indented(&header.response, ui, |ui| {
        for child in &node.children {
            outline_node(ui, child, depth + 1, jump);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_exists_stats_on_load_and_focus_not_every_frame() {
        let recent = vec![
            PathBuf::from("/tmp/marker-a.pdf"),
            PathBuf::from("/tmp/marker-b.pdf"),
        ];
        let mut cache = RecentExistsCache::default();
        let mut stats = 0;
        sync_recent_exists(&mut cache, &recent, false, |_| {
            stats += 1;
            true
        });
        assert_eq!(stats, 2);
        assert_eq!(cache.exists, vec![true, true]);

        sync_recent_exists(&mut cache, &recent, false, |_| {
            stats += 1;
            false
        });
        assert_eq!(stats, 2);
        assert_eq!(cache.exists, vec![true, true]);

        sync_recent_exists(&mut cache, &recent, true, |_| {
            stats += 1;
            false
        });
        assert_eq!(stats, 4);
        assert_eq!(cache.exists, vec![false, false]);

        let changed = vec![recent[0].clone()];
        sync_recent_exists(&mut cache, &changed, false, |_| {
            stats += 1;
            true
        });
        assert_eq!(stats, 5);
        assert_eq!(cache.exists, vec![true]);
        // Missing entries stay in the cached list; nothing is deleted here.
        assert_eq!(cache.paths, changed);
    }
}
