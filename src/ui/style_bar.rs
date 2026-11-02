//! Floating style strip and toolbar color/metric widgets (RF9c).
//!
//! These still take [`MarkerApp`] until RF10 introduces a pane/style context.

use egui::{DragValue, Key};

use crate::annot::{AnnotKind, ShapeKind};
use crate::app::{MarkerApp, Tool};
use crate::geom::Rgb;
use crate::settings::{self, HighlightPreset};
use crate::ui::{color_dot, highlight_color_name, ink_color_name};

/// Which selected annotation kinds a color pick should update.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorTarget {
    Highlights,
    /// Text / note / math fill and shape stroke (images ignored).
    Ink,
}

impl MarkerApp {
    pub(crate) fn color_controls(&mut self, ui: &mut egui::Ui) {
        let has_sel = self.tab().is_some_and(|tab| !tab.selected.is_empty());
        let (show_hl, show_ink, _, _) = self.selection_style_flags();
        // Select tool: palette follows the selection (highlight vs ink); images alone hide it.
        let target = if self.tool == Tool::Select && has_sel {
            if show_hl && !show_ink {
                ColorTarget::Highlights
            } else if show_ink {
                // Mixed or ink-only: toolbar shows ink / text color.
                ColorTarget::Ink
            } else {
                return;
            }
        } else if self.tool.is_text_mark() {
            ColorTarget::Highlights
        } else {
            ColorTarget::Ink
        };
        let current = match target {
            ColorTarget::Highlights => self
                .selected_highlight_color()
                .unwrap_or(self.settings.highlight_color),
            ColorTarget::Ink => self.selected_ink_color().unwrap_or(self.active_color()),
        };
        let mut picked = None;
        match target {
            ColorTarget::Highlights => {
                let presets = self.settings.highlight_presets.clone();
                let mut matched = false;
                for preset in &presets {
                    let on = preset.rgb == current;
                    matched |= on;
                    if color_dot(ui, preset.rgb, on, &preset.name) {
                        picked = Some(preset.rgb);
                    }
                }
                if !matched {
                    let name = self
                        .settings
                        .highlight_name_for(current)
                        .unwrap_or("Custom");
                    let _ = color_dot(ui, current, true, name);
                }
            }
            ColorTarget::Ink => {
                let mut matched = false;
                for color in settings::palette_for(settings::PaletteKind::Ink) {
                    let on = *color == current;
                    matched |= on;
                    if color_dot(ui, *color, on, ink_color_name(*color)) {
                        picked = Some(*color);
                    }
                }
                if !matched {
                    let _ = color_dot(ui, current, true, "Custom");
                }
            }
        }
        if let Some(color) = picked {
            self.seal_then_arm();
            self.apply_color_to(color, target);
            self.seal_undo();
        }
    }

    pub(crate) fn metric_controls(&mut self, ui: &mut egui::Ui) {
        let show_size = matches!(self.tool, Tool::Text | Tool::Math | Tool::Select)
            || self.selected_is_text_like();
        let show_stroke = matches!(self.tool, Tool::Rect | Tool::Ellipse | Tool::Line)
            || self.selected_is_shape();
        if show_size {
            let mut size = self.active_text_size();
            let response = ui.add(
                DragValue::new(&mut size)
                    .range(6.0..=96.0)
                    .speed(0.2)
                    .suffix(" pt"),
            );
            if response.changed() {
                self.arm_undo();
                self.apply_text_size(size);
            }
        }
        if show_stroke {
            let mut width = self.active_stroke();
            let response = ui.add(
                DragValue::new(&mut width)
                    .range(0.5..=12.0)
                    .speed(0.05)
                    .suffix(" px"),
            );
            if response.changed() {
                self.arm_undo();
                self.apply_stroke(width);
            }
        }
    }

    /// Compact icon row for annotation color/size. Returns true if Delete was chosen.
    pub(crate) fn style_bar_contents(&mut self, ui: &mut egui::Ui, show_delete: bool) -> bool {
        let (show_hl, show_ink, show_size, show_stroke) = self.selection_style_flags();
        let show_fill = self.selected_has_fillable_shape();

        ui.spacing_mut().item_spacing = egui::vec2(3.0, 0.0);
        // Shared multi-select options: highlight color and/or text/ink color (#28).
        // Arrow keys cycle only when a single palette is showing.
        let cycle_ok = show_hl ^ show_ink;
        if show_hl {
            let current = self
                .selected_highlight_color()
                .unwrap_or(self.settings.highlight_color);
            let presets = self.settings.highlight_presets.clone();
            self.style_highlight_palette_row(ui, &presets, current, cycle_ok);
        }
        if show_hl && show_ink {
            ui.separator();
        }
        if show_ink {
            self.style_ink_palette_row(
                ui,
                self.selected_ink_color()
                    .unwrap_or(self.settings.text_color),
                cycle_ok,
            );
        }
        if show_size {
            let mut size = self.active_text_size();
            if style_step(ui, "A−", "Smaller text").clicked() {
                size = (size - 1.0).max(6.0);
                self.seal_then_arm();
                self.apply_text_size(size);
                self.seal_undo();
            }
            ui.label(
                egui::RichText::new(format!("{size:.0}"))
                    .size(11.0)
                    .color(egui::Color32::from_rgb(232, 232, 236)),
            );
            if style_step(ui, "A+", "Larger text").clicked() {
                size = (size + 1.0).min(96.0);
                self.seal_then_arm();
                self.apply_text_size(size);
                self.seal_undo();
            }
        }
        if show_stroke {
            let mut width = self.active_stroke();
            if style_step(ui, "−", "Thinner stroke").clicked() {
                width = (width - 0.25).max(0.5);
                self.seal_then_arm();
                self.apply_stroke(width);
                self.seal_undo();
            }
            ui.label(
                egui::RichText::new(format!("{width:.1}"))
                    .size(11.0)
                    .color(egui::Color32::from_rgb(232, 232, 236)),
            );
            if style_step(ui, "+", "Thicker stroke").clicked() {
                width = (width + 0.25).min(12.0);
                self.seal_then_arm();
                self.apply_stroke(width);
                self.seal_undo();
            }
        }
        if show_fill {
            let filled = self.selected_shape_filled();
            let tip = if filled {
                "Remove shape fill"
            } else {
                "Fill shape"
            };
            let label = if filled { "▣" } else { "□" };
            if style_step(ui, label, tip).clicked() {
                self.seal_then_arm();
                self.apply_shape_fill(!filled);
                self.seal_undo();
            }
        }
        if show_delete {
            ui.add_space(4.0);
            style_step(ui, "×", "Delete annotation").clicked()
        } else {
            false
        }
    }

    fn style_highlight_palette_row(
        &mut self,
        ui: &mut egui::Ui,
        presets: &[HighlightPreset],
        current: Rgb,
        allow_cycle: bool,
    ) {
        let colors: Vec<Rgb> = presets.iter().map(|p| p.rgb).collect();
        self.cycle_palette_color(ui, &colors, ColorTarget::Highlights, current, allow_cycle);
        let current = self
            .selected_highlight_color()
            .unwrap_or(self.settings.highlight_color);
        let mut picked = None;
        let mut matched = false;
        for preset in presets {
            let on = preset.rgb == current;
            matched |= on;
            if color_dot(ui, preset.rgb, on, &preset.name) {
                picked = Some(preset.rgb);
            }
        }
        if !matched {
            let _ = color_dot(ui, current, true, highlight_color_name(presets, current));
        }
        if let Some(color) = picked {
            self.seal_then_arm();
            self.apply_color_to(color, ColorTarget::Highlights);
            self.seal_undo();
        }
    }

    fn style_ink_palette_row(&mut self, ui: &mut egui::Ui, current: Rgb, allow_cycle: bool) {
        let colors = settings::palette_for(settings::PaletteKind::Ink);
        self.cycle_palette_color(ui, colors, ColorTarget::Ink, current, allow_cycle);
        let current = self
            .selected_ink_color()
            .unwrap_or(self.settings.text_color);
        let mut picked = None;
        let mut matched = false;
        for color in colors {
            let on = *color == current;
            matched |= on;
            if color_dot(ui, *color, on, ink_color_name(*color)) {
                picked = Some(*color);
            }
        }
        if !matched {
            let _ = color_dot(ui, current, true, "Custom");
        }
        if let Some(color) = picked {
            self.seal_then_arm();
            self.apply_color_to(color, ColorTarget::Ink);
            self.seal_undo();
        }
    }

    fn cycle_palette_color(
        &mut self,
        ui: &mut egui::Ui,
        colors: &[Rgb],
        target: ColorTarget,
        current: Rgb,
        allow_cycle: bool,
    ) {
        // Left / Right cycles the focused palette while the style strip is open.
        let cycle = allow_cycle
            .then(|| {
                ui.ctx().input(|input| {
                    if input.key_pressed(Key::ArrowRight) {
                        Some(1isize)
                    } else if input.key_pressed(Key::ArrowLeft) {
                        Some(-1isize)
                    } else {
                        None
                    }
                })
            })
            .flatten();
        if let Some(delta) = cycle {
            if let Some(idx) = colors.iter().position(|c| *c == current) {
                let next = (idx as isize + delta).rem_euclid(colors.len() as isize) as usize;
                let color = colors[next];
                self.seal_then_arm();
                self.apply_color_to(color, target);
                self.seal_undo();
            } else if let Some(color) = colors.first() {
                self.seal_then_arm();
                self.apply_color_to(*color, target);
                self.seal_undo();
            }
        }
    }

    fn active_color(&self) -> Rgb {
        if let Some(color) = self.selected_color() {
            return color;
        }
        match self.tool {
            Tool::Highlight | Tool::Underline | Tool::StrikeOut | Tool::Squiggly => {
                self.settings.highlight_color
            }
            Tool::Rect | Tool::Ellipse | Tool::Line => self.settings.shape_color,
            Tool::Select | Tool::Text | Tool::Note | Tool::Math => self.settings.text_color,
        }
    }

    fn selected_color(&self) -> Option<Rgb> {
        self.selected_ink_color()
            .or_else(|| self.selected_highlight_color())
    }

    fn selected_highlight_color(&self) -> Option<Rgb> {
        let tab = self.tab()?;
        for id in &tab.selected {
            if let Some(AnnotKind::Highlight { color, .. } | AnnotKind::Markup { color, .. }) =
                tab.doc.session.get(*id).map(|a| &a.kind)
            {
                return Some(*color);
            }
        }
        None
    }

    fn selected_ink_color(&self) -> Option<Rgb> {
        let tab = self.tab()?;
        for id in &tab.selected {
            match tab.doc.session.get(*id).map(|a| &a.kind) {
                Some(
                    AnnotKind::Text { color, .. }
                    | AnnotKind::Note { color, .. }
                    | AnnotKind::Math { color, .. },
                ) => return Some(*color),
                Some(AnnotKind::Shape { stroke, .. }) => return Some(*stroke),
                _ => {}
            }
        }
        None
    }

    fn active_text_size(&self) -> f32 {
        if let Some(tab) = self.tab() {
            for id in &tab.selected {
                if let Some(annot) = tab.doc.session.get(*id) {
                    match &annot.kind {
                        AnnotKind::Text { size, .. } | AnnotKind::Math { size, .. } => {
                            return *size
                        }
                        _ => {}
                    }
                }
            }
        }
        self.settings.text_size
    }

    fn active_stroke(&self) -> f32 {
        if let Some(tab) = self.tab() {
            for id in &tab.selected {
                if let Some(AnnotKind::Shape { width, .. }) =
                    tab.doc.session.get(*id).map(|a| &a.kind)
                {
                    return *width;
                }
            }
        }
        self.settings.shape_width
    }

    fn selected_is_text_like(&self) -> bool {
        self.tab().is_some_and(|tab| {
            tab.selected.iter().any(|id| {
                matches!(
                    tab.doc.session.get(*id).map(|a| &a.kind),
                    Some(AnnotKind::Text { .. } | AnnotKind::Math { .. })
                )
            })
        })
    }

    fn selected_is_shape(&self) -> bool {
        self.tab().is_some_and(|tab| {
            tab.selected.iter().any(|id| {
                matches!(
                    tab.doc.session.get(*id).map(|a| &a.kind),
                    Some(AnnotKind::Shape { .. })
                )
            })
        })
    }

    /// (show_highlight_color, show_ink/text_color, show_size, show_stroke) for the selection.
    fn selection_style_flags(&self) -> (bool, bool, bool, bool) {
        let Some(tab) = self.tab() else {
            return (false, false, false, false);
        };
        let mut highlight = false;
        let mut ink = false;
        let mut size = false;
        let mut stroke = false;
        for id in &tab.selected {
            match tab.doc.session.get(*id).map(|a| &a.kind) {
                Some(AnnotKind::Highlight { .. } | AnnotKind::Markup { .. }) => {
                    highlight = true;
                }
                Some(AnnotKind::Text { .. } | AnnotKind::Math { .. }) => {
                    ink = true;
                    size = true;
                }
                Some(AnnotKind::Note { .. }) => {
                    ink = true;
                }
                Some(AnnotKind::Shape { .. }) => {
                    ink = true;
                    stroke = true;
                }
                Some(AnnotKind::Image { .. } | AnnotKind::Foreign { .. }) | None => {}
            }
        }
        (highlight, ink, size, stroke)
    }

    fn apply_color_to(&mut self, color: Rgb, target: ColorTarget) {
        let mut kind_bucket = None;
        let mut math_ids = Vec::new();
        let ids = self
            .tab()
            .map(|tab| tab.selected.clone())
            .unwrap_or_default();
        if let Some(tab) = self.tab_mut() {
            for id in ids {
                let mut touched = None;
                if let Some(annot) = tab.doc.session.get_mut(id) {
                    match (&mut annot.kind, target) {
                        (
                            AnnotKind::Highlight { color: slot, .. }
                            | AnnotKind::Markup { color: slot, .. },
                            ColorTarget::Highlights,
                        ) => {
                            *slot = color;
                            touched = Some(0);
                        }
                        (
                            AnnotKind::Text { color: slot, .. }
                            | AnnotKind::Note { color: slot, .. }
                            | AnnotKind::Math { color: slot, .. },
                            ColorTarget::Ink,
                        ) => {
                            *slot = color;
                            touched = Some(1);
                            if matches!(annot.kind, AnnotKind::Math { .. }) {
                                math_ids.push(id);
                            }
                        }
                        (AnnotKind::Shape { stroke, fill, .. }, ColorTarget::Ink) => {
                            *stroke = color;
                            if fill.is_some() {
                                *fill = Some(color);
                            }
                            touched = Some(2);
                        }
                        _ => {}
                    }
                }
                if let Some(bucket) = touched {
                    kind_bucket = Some(bucket);
                    tab.doc.session.mark_dirty(id);
                    tab.save.touch();
                }
            }
        }
        for id in math_ids {
            self.queue_math(id);
        }
        match kind_bucket.unwrap_or(match target {
            ColorTarget::Highlights => 0,
            ColorTarget::Ink => match self.tool {
                Tool::Rect | Tool::Ellipse | Tool::Line => 2,
                _ => 1,
            },
        }) {
            0 => self.settings.highlight_color = color,
            2 => self.settings.shape_color = color,
            _ => self.settings.text_color = color,
        }
        self.settings.save();
    }

    fn apply_text_size(&mut self, size: f32) {
        let size = size.clamp(6.0, 96.0);
        let mut math_ids = Vec::new();
        let ids = self
            .tab()
            .map(|tab| tab.selected.clone())
            .unwrap_or_default();
        if let Some(tab) = self.tab_mut() {
            for id in ids {
                let mut changed = false;
                if let Some(annot) = tab.doc.session.get_mut(id) {
                    match &mut annot.kind {
                        AnnotKind::Text { size: slot, .. } => {
                            *slot = size;
                            changed = true;
                        }
                        AnnotKind::Math { size: slot, .. } => {
                            *slot = size;
                            changed = true;
                            math_ids.push(id);
                        }
                        _ => {}
                    }
                }
                if changed {
                    tab.doc.session.mark_dirty(id);
                    tab.save.touch();
                }
            }
        }
        for id in math_ids {
            self.queue_math(id);
        }
        self.settings.text_size = size;
        self.settings.save();
    }

    fn apply_stroke(&mut self, width: f32) {
        let width = width.clamp(0.25, 16.0);
        let ids = self
            .tab()
            .map(|tab| tab.selected.clone())
            .unwrap_or_default();
        if let Some(tab) = self.tab_mut() {
            for id in ids {
                let mut changed = false;
                if let Some(annot) = tab.doc.session.get_mut(id) {
                    if let AnnotKind::Shape { width: slot, .. } = &mut annot.kind {
                        *slot = width;
                        changed = true;
                    }
                }
                if changed {
                    tab.doc.session.mark_dirty(id);
                    tab.save.touch();
                }
            }
        }
        self.settings.shape_width = width;
        self.settings.save();
    }

    fn selected_has_fillable_shape(&self) -> bool {
        self.tab().is_some_and(|tab| {
            tab.selected.iter().any(|id| {
                matches!(
                    tab.doc.session.get(*id).map(|a| &a.kind),
                    Some(AnnotKind::Shape {
                        kind: ShapeKind::Rect | ShapeKind::Ellipse,
                        ..
                    })
                )
            })
        })
    }

    /// True when every selected fillable shape already has a fill.
    fn selected_shape_filled(&self) -> bool {
        let Some(tab) = self.tab() else {
            return false;
        };
        let mut any = false;
        for id in &tab.selected {
            if let Some(AnnotKind::Shape {
                kind: ShapeKind::Rect | ShapeKind::Ellipse,
                fill,
                ..
            }) = tab.doc.session.get(*id).map(|a| &a.kind)
            {
                any = true;
                if fill.is_none() {
                    return false;
                }
            }
        }
        any
    }

    fn apply_shape_fill(&mut self, filled: bool) {
        let ids = self
            .tab()
            .map(|tab| tab.selected.clone())
            .unwrap_or_default();
        if let Some(tab) = self.tab_mut() {
            for id in ids {
                let mut changed = false;
                if let Some(annot) = tab.doc.session.get_mut(id) {
                    if let AnnotKind::Shape {
                        kind: ShapeKind::Rect | ShapeKind::Ellipse,
                        stroke,
                        fill,
                        ..
                    } = &mut annot.kind
                    {
                        let next = filled.then_some(*stroke);
                        if *fill != next {
                            *fill = next;
                            changed = true;
                        }
                    }
                }
                if changed {
                    tab.doc.session.mark_dirty(id);
                    tab.save.touch();
                }
            }
        }
    }
}

fn style_step(ui: &mut egui::Ui, label: &str, tip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(20.0, 18.0), egui::Sense::click());
    let fill = if response.is_pointer_button_down_on() {
        egui::Color32::from_white_alpha(28)
    } else if response.hovered() {
        egui::Color32::from_white_alpha(16)
    } else {
        egui::Color32::TRANSPARENT
    };
    if fill != egui::Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(4), fill);
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::new(12.0, egui::FontFamily::Proportional),
        egui::Color32::from_rgb(232, 232, 236),
    );
    response.on_hover_text(tip)
}
