//! Tab open/close/activate, detach, and split (RF9b).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::ViewportCommand;

use crate::instance;
use crate::pdf::DocId;

use super::*;

impl MarkerApp {
    pub(crate) fn open_path(&mut self, path: PathBuf) {
        let repaired = crate::pdf::repaired_sidecar_for(&path);
        if let Some(tab) = self
            .tabs
            .iter()
            .find(|tab| tab.doc.path == path || tab.doc.path == repaired)
        {
            let id = tab.id;
            if self.active != id {
                self.activate_tab(id);
            }
            self.settings.remember_open(&path);
            return;
        }
        self.next_doc_id = DocId(self.next_doc_id.0 + 1);
        let doc_id = self.next_doc_id;
        self.opening.insert(doc_id);
        self.error = None;
        self.worker.open(doc_id, path);
    }

    /// Move a tab into its own Marker process/window, then close it here.
    pub(crate) fn move_tab_to_new_window(&mut self, id: TabId) {
        let Some(tab) = self.tab_by_id_mut(id) else {
            return;
        };
        if tab.doc.session.is_dirty() {
            tab.force_save = true;
            tab.detach_after_save = true;
            return;
        }
        let path = tab.doc.path.clone();
        if let Err(message) = instance::spawn_new_window(&path) {
            self.error = Some(format!("Could not open new window: {message}"));
            return;
        }
        self.drop_tab(id);
    }

    pub(crate) fn finish_detach_tab(&mut self, id: TabId) {
        let Some(tab) = self.tab_by_id(id) else {
            return;
        };
        let path = tab.doc.path.clone();
        if let Err(message) = instance::spawn_new_window(&path) {
            if let Some(tab) = self.tab_by_id_mut(id) {
                tab.detach_after_save = false;
            }
            self.error = Some(format!("Could not open new window: {message}"));
            return;
        }
        self.drop_tab(id);
    }

    pub(crate) fn focus_main_window(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(
            egui::UserAttentionType::Informational,
        ));
    }

    pub(crate) fn poll_ipc(&mut self, ctx: &egui::Context) {
        let Some(inbox) = self.ipc.as_ref() else {
            return;
        };
        let batches = inbox.poll();
        if batches.is_empty() {
            return;
        }
        self.focus_main_window(ctx);
        for paths in batches {
            for path in paths {
                self.open_path(path);
            }
        }
    }

    /// Briefly show the open-tabs list (e.g. after Ctrl+Tab). No-op while pinned.
    pub(crate) fn pulse_tab_list(&mut self) {
        if self.tabs.len() < 2 {
            return;
        }
        if matches!(self.tab_list, Some(TabListState::Pinned)) {
            return;
        }
        self.tab_list = Some(TabListState::Ephemeral {
            until: Instant::now() + Duration::from_millis(2500),
        });
    }

    /// Toggle the pinned open-tabs list (title / indicator click).
    /// An ephemeral flash (Ctrl+Tab) becomes pinned so the list stays open.
    pub(crate) fn toggle_tab_list(&mut self) {
        if self.tabs.is_empty() {
            self.tab_list = None;
            return;
        }
        match self.tab_list {
            Some(TabListState::Pinned) => self.tab_list = None,
            Some(TabListState::Ephemeral { .. }) => {
                self.tab_list = Some(TabListState::Pinned);
            }
            None => self.tab_list = Some(TabListState::Pinned),
        }
    }

    pub(crate) fn dismiss_tab_list(&mut self) {
        self.tab_list = None;
    }

    /// Activate a tab from the switcher / list / Ctrl+Tab, respecting split panes.
    ///
    /// If `id` is already a pane, focus it. Otherwise replace the focused pane so
    /// keyboard and list navigation share one rule (never leave `active` outside
    /// the split — that used to flash then clamp back to `split.first`).
    pub(crate) fn activate_tab(&mut self, id: TabId) {
        if self.tab_index(id).is_none() {
            return;
        }
        if let Some(split) = self.split {
            if split.contains(id) {
                self.active = id;
            } else {
                let focus_first = self.active == split.first;
                if let Some(s) = self.split.as_mut() {
                    if focus_first {
                        s.first = id;
                    } else {
                        s.second = id;
                    }
                }
                self.active = id;
            }
        } else {
            self.active = id;
        }
        self.pulse_tab_list();
    }

    pub(crate) fn close_tab(&mut self, id: TabId) {
        let Some(tab) = self.tab_by_id_mut(id) else {
            return;
        };
        if tab.doc.session.is_dirty() {
            tab.force_save = true;
            tab.close_after_save = true;
            return;
        }
        self.drop_tab(id);
    }

    pub(crate) fn drop_tab(&mut self, id: TabId) {
        let Some(index) = self.tab_index(id) else {
            return;
        };
        let tab = self.tabs.remove(index);
        debug_assert_eq!(tab.id, id);
        let doc_id = tab.doc.doc_id;
        self.math.forget_doc(doc_id);
        self.worker.close(tab.doc.doc_id);
        self.tile_wanted
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&tab.doc.doc_id);
        self.thumb_wanted
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&tab.doc.doc_id);

        let remaining: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
        reconcile_tab_ids_after_close(
            TabIdChrome {
                active: &mut self.active,
                split: &mut self.split,
                tab_menu: &mut self.tab_menu,
                tab_drag: &mut self.tab_drag,
                tab_drag_over: &mut self.tab_drag_over,
                tab_list: &mut self.tab_list,
            },
            id,
            &remaining,
            index,
        );
        if self.tile_focus_tab == id {
            self.tile_focus_tab = self.active;
        }
    }

    pub(crate) fn split_with(&mut self, other: TabId, stacked: bool) {
        if self.tab_index(other).is_none() || other == self.active || self.tabs.len() < 2 {
            return;
        }
        let ratio = self.split.map(|s| s.ratio).unwrap_or(0.5);
        self.split = Some(SplitState {
            first: self.active,
            second: other,
            stacked,
            ratio,
        });
    }

    /// Split using `id` as the other pane, or the next tab when `id` is active.
    pub(crate) fn split_from_tab(&mut self, id: TabId, stacked: bool) {
        if self.tabs.len() < 2 || self.tab_index(id).is_none() {
            return;
        }
        let other = if id == self.active {
            match self.adjacent_tab_id(self.active, 1) {
                Some(next) => next,
                None => return,
            }
        } else {
            id
        };
        self.split_with(other, stacked);
    }

    /// Toggle side-by-side / stacked split with the next tab; same layout again unsplits.
    pub(crate) fn toggle_split(&mut self, stacked: bool) {
        if self.tabs.len() < 2 {
            return;
        }
        if let Some(split) = self.split {
            if split.stacked == stacked {
                self.unsplit();
            } else if let Some(s) = self.split.as_mut() {
                s.stacked = stacked;
            }
        } else {
            let Some(other) = self.adjacent_tab_id(self.active, 1) else {
                return;
            };
            self.split_with(other, stacked);
        }
    }

    pub(crate) fn set_split_panes(&mut self, first: TabId, second: TabId, stacked: bool) {
        if first == second || self.tab_index(first).is_none() || self.tab_index(second).is_none() {
            return;
        }
        let ratio = self.split.map(|s| s.ratio).unwrap_or(0.5);
        self.split = Some(SplitState {
            first,
            second,
            stacked,
            ratio,
        });
        if self.active != first && self.active != second {
            self.active = first;
        }
    }

    /// Place `dragged` into a new or existing split according to an edge / pane drop.
    pub(crate) fn apply_tab_drop(&mut self, dragged: TabId, zone: SplitDropZone) {
        if self.tabs.len() < 2 || self.tab_index(dragged).is_none() {
            return;
        }
        if let Some(target) = zone.tab_target() {
            if self.tab_index(target).is_none() {
                return;
            }
        }
        let adjacent = self.adjacent_tab_id(self.active, 1);
        let Some(plan) = plan_tab_drop(dragged, zone, self.active, self.split, adjacent) else {
            return;
        };
        match plan.split {
            Some(split) => {
                self.set_split_panes(split.first, split.second, split.stacked);
                if let Some(s) = self.split.as_mut() {
                    s.ratio = split.ratio;
                }
            }
            None => self.split = None,
        }
        self.active = plan.active;
    }

    pub(crate) fn unsplit(&mut self) {
        self.split = None;
    }

    pub(crate) fn focus_split_other(&mut self) {
        let Some(split) = self.split else {
            return;
        };
        self.active = split.other(self.active);
        self.pulse_tab_list();
    }
}

/// Result of resolving a tab drag-drop without mutating app state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TabDropPlan {
    pub split: Option<SplitState>,
    pub active: TabId,
}

/// Pure planner for [`MarkerApp::apply_tab_drop`].
///
/// `adjacent_of_active` is the strip neighbor of `active` (used only when creating
/// a split by dragging the focused tab and no explicit partner is known).
pub(crate) fn plan_tab_drop(
    dragged: TabId,
    zone: SplitDropZone,
    active: TabId,
    split: Option<SplitState>,
    adjacent_of_active: Option<TabId>,
) -> Option<TabDropPlan> {
    match zone {
        SplitDropZone::Left
        | SplitDropZone::Right
        | SplitDropZone::Top
        | SplitDropZone::Bottom
        | SplitDropZone::Center => {
            plan_edge_or_center(dragged, zone, active, split, adjacent_of_active)
        }
        SplitDropZone::OtherPane => plan_other_pane(dragged, active, split?),
        SplitDropZone::ActivePane => plan_active_pane(dragged, active, split?),
        SplitDropZone::Tab(target) => {
            if target == dragged {
                return None;
            }
            plan_tab_row(dragged, target, active, split)
        }
    }
}

fn plan_edge_or_center(
    dragged: TabId,
    zone: SplitDropZone,
    active: TabId,
    split: Option<SplitState>,
    adjacent_of_active: Option<TabId>,
) -> Option<TabDropPlan> {
    let stacked = match zone {
        SplitDropZone::Top | SplitDropZone::Bottom => true,
        SplitDropZone::Left | SplitDropZone::Right => false,
        // Center: keep current orientation, or side-by-side when creating.
        SplitDropZone::Center => split.map(|s| s.stacked).unwrap_or(false),
        SplitDropZone::OtherPane | SplitDropZone::ActivePane | SplitDropZone::Tab(_) => {
            return None;
        }
    };
    let as_first = match zone {
        SplitDropZone::Left | SplitDropZone::Top => true,
        SplitDropZone::Right | SplitDropZone::Bottom | SplitDropZone::Center => false,
        SplitDropZone::OtherPane | SplitDropZone::ActivePane | SplitDropZone::Tab(_) => {
            return None;
        }
    };

    if let Some(split) = split {
        let ratio = split.ratio;
        let (first, second) = if split.contains(dragged) {
            // Keep both pane partners; only reorder / flip orientation.
            let other = split.other(dragged);
            if as_first {
                (dragged, other)
            } else {
                (other, dragged)
            }
        } else {
            // Third tab: assign to the edge's slot; keep the opposite pane.
            let keep = if as_first { split.second } else { split.first };
            if as_first {
                (dragged, keep)
            } else {
                (keep, dragged)
            }
        };
        Some(TabDropPlan {
            split: Some(SplitState {
                first,
                second,
                stacked,
                ratio,
            }),
            active: dragged,
        })
    } else {
        let other = if dragged == active {
            let partner = adjacent_of_active?;
            if partner == dragged {
                return None;
            }
            partner
        } else {
            active
        };
        if other == dragged {
            return None;
        }
        let (first, second) = if as_first {
            (dragged, other)
        } else {
            (other, dragged)
        };
        Some(TabDropPlan {
            split: Some(SplitState {
                first,
                second,
                stacked,
                ratio: 0.5,
            }),
            active: dragged,
        })
    }
}

fn swap_split(split: SplitState) -> SplitState {
    SplitState {
        first: split.second,
        second: split.first,
        stacked: split.stacked,
        ratio: 1.0 - split.ratio,
    }
}

fn plan_other_pane(dragged: TabId, active: TabId, split: SplitState) -> Option<TabDropPlan> {
    let unfocused = split.other(active);
    if dragged == active || dragged == unfocused {
        // Dragging either pane onto the other swaps.
        Some(TabDropPlan {
            split: Some(swap_split(split)),
            active: dragged,
        })
    } else {
        let mut next = split;
        if next.first == unfocused {
            next.first = dragged;
        } else {
            next.second = dragged;
        }
        Some(TabDropPlan {
            split: Some(next),
            active: dragged,
        })
    }
}

fn plan_active_pane(dragged: TabId, active: TabId, split: SplitState) -> Option<TabDropPlan> {
    let unfocused = split.other(active);
    if dragged == active {
        return None;
    }
    if dragged == unfocused {
        Some(TabDropPlan {
            split: Some(swap_split(split)),
            active: dragged,
        })
    } else {
        let mut next = split;
        if next.first == active {
            next.first = dragged;
        } else {
            next.second = dragged;
        }
        Some(TabDropPlan {
            split: Some(next),
            active: dragged,
        })
    }
}

fn plan_tab_row(
    dragged: TabId,
    target: TabId,
    active: TabId,
    split: Option<SplitState>,
) -> Option<TabDropPlan> {
    let stacked = split.map(|s| s.stacked).unwrap_or(false);
    let ratio = split.map(|s| s.ratio).unwrap_or(0.5);

    if let Some(split) = split {
        if split.contains(target) {
            // Drop onto a pane's list row: put dragged in that pane's slot.
            if dragged == target {
                return None;
            }
            if split.contains(dragged) {
                return Some(TabDropPlan {
                    split: Some(swap_split(split)),
                    active: dragged,
                });
            }
            let mut next = split;
            if next.first == target {
                next.first = dragged;
            } else {
                next.second = dragged;
            }
            return Some(TabDropPlan {
                split: Some(next),
                active: dragged,
            });
        }
        if split.contains(dragged) {
            // Dragging a visible pane onto a hidden tab: show that tab in the other pane.
            let mut next = split;
            if next.first == dragged {
                next.second = target;
            } else {
                next.first = target;
            }
            return Some(TabDropPlan {
                split: Some(next),
                active: dragged,
            });
        }
    }

    // Neither is constrained, or no split: pair them. Prefer dragged as first when
    // it was focused; otherwise put the drop target first so the gesture reads as
    // "open dragged beside target".
    let (first, second) = if active == dragged || active == target {
        if active == dragged {
            (dragged, target)
        } else {
            (target, dragged)
        }
    } else {
        (dragged, target)
    };
    Some(TabDropPlan {
        split: Some(SplitState {
            first,
            second,
            stacked,
            ratio,
        }),
        active: dragged,
    })
}

/// Identity-keyed tab chrome passed through [`reconcile_tab_ids_after_close`].
pub(crate) struct TabIdChrome<'a> {
    active: &'a mut TabId,
    split: &'a mut Option<SplitState>,
    tab_menu: &'a mut Option<(TabId, egui::Pos2)>,
    tab_drag: &'a mut Option<TabId>,
    tab_drag_over: &'a mut Option<TabId>,
    tab_list: &'a mut Option<TabListState>,
}

/// After removing `closed` from the strip, retarget identity-keyed chrome.
///
/// Surviving [`TabId`] values are left alone — no index arithmetic — so a missed
/// field cannot drift. Cleared only when it named the closed tab; otherwise it
/// must still refer to a live id in `remaining`.
pub(crate) fn reconcile_tab_ids_after_close(
    chrome: TabIdChrome<'_>,
    closed: TabId,
    remaining: &[TabId],
    closed_index: usize,
) {
    let TabIdChrome {
        active,
        split,
        tab_menu,
        tab_drag,
        tab_drag_over,
        tab_list,
    } = chrome;

    if *tab_drag == Some(closed) {
        *tab_drag = None;
    }
    if tab_menu.as_ref().is_some_and(|(id, _)| *id == closed) {
        *tab_menu = None;
    }
    if *tab_drag_over == Some(closed) {
        *tab_drag_over = None;
    }

    if remaining.is_empty() {
        *active = TabId(0);
        *split = None;
        *tab_drag = None;
        *tab_menu = None;
        *tab_drag_over = None;
        *tab_list = None;
        return;
    }

    let live = |id: TabId| remaining.contains(&id);

    if let Some(s) = *split {
        if s.first == closed || s.second == closed {
            let survivor = if s.first == closed { s.second } else { s.first };
            *split = None;
            if live(survivor) {
                *active = survivor;
            } else if *active == closed || !live(*active) {
                let fallback = closed_index.min(remaining.len() - 1);
                *active = remaining[fallback];
            }
        }
        // Else: both pane ids still live; leave the split untouched.
    }

    if *active == closed || !live(*active) {
        let fallback = closed_index.min(remaining.len() - 1);
        *active = remaining[fallback];
    }

    debug_assert!(live(*active));
    if let Some(id) = *tab_drag {
        debug_assert!(live(id));
    }
    if let Some((id, _)) = *tab_menu {
        debug_assert!(live(id));
    }
    if let Some(id) = *tab_drag_over {
        debug_assert!(live(id));
    }
}

#[cfg(test)]
mod tab_id_tests {
    use super::{
        plan_tab_drop, reconcile_tab_ids_after_close, SplitDropZone, SplitState, TabDropPlan,
        TabId, TabIdChrome, TabListState,
    };
    use egui::Pos2;

    fn side(first: TabId, second: TabId) -> SplitState {
        SplitState {
            first,
            second,
            stacked: false,
            ratio: 0.5,
        }
    }

    fn stacked(first: TabId, second: TabId) -> SplitState {
        SplitState {
            first,
            second,
            stacked: true,
            ratio: 0.5,
        }
    }

    #[test]
    pub(crate) fn drop_middle_tab_keeps_surviving_ids() {
        let a = TabId(1);
        let b = TabId(2);
        let c = TabId(3);
        let mut active = a;
        let mut split = Some(SplitState {
            first: a,
            second: c,
            stacked: false,
            ratio: 0.5,
        });
        let mut tab_menu = Some((a, Pos2::ZERO));
        let mut tab_drag = Some(a);
        let mut tab_drag_over = Some(c);
        let mut tab_list = Some(TabListState::Pinned);

        // Close the middle strip tab; panes and drag-over name A/C, not B.
        let remaining = [a, c];
        reconcile_tab_ids_after_close(
            TabIdChrome {
                active: &mut active,
                split: &mut split,
                tab_menu: &mut tab_menu,
                tab_drag: &mut tab_drag,
                tab_drag_over: &mut tab_drag_over,
                tab_list: &mut tab_list,
            },
            b,
            &remaining,
            1,
        );

        assert_eq!(active, a);
        assert_eq!(
            split,
            Some(SplitState {
                first: a,
                second: c,
                stacked: false,
                ratio: 0.5,
            })
        );
        assert_eq!(tab_menu.map(|(id, _)| id), Some(a));
        assert_eq!(tab_drag, Some(a));
        assert_eq!(
            tab_drag_over,
            Some(c),
            "tab_drag_over must keep naming a live tab (not be index-shifted)"
        );
        assert!(matches!(tab_list, Some(TabListState::Pinned)));
    }

    #[test]
    pub(crate) fn drop_drag_over_target_clears_it() {
        let a = TabId(10);
        let b = TabId(20);
        let c = TabId(30);
        let mut active = a;
        let mut split = Some(SplitState {
            first: a,
            second: c,
            stacked: true,
            ratio: 0.4,
        });
        let mut tab_menu = None;
        let mut tab_drag = Some(a);
        let mut tab_drag_over = Some(b);
        let mut tab_list = None;

        let remaining = [a, c];
        reconcile_tab_ids_after_close(
            TabIdChrome {
                active: &mut active,
                split: &mut split,
                tab_menu: &mut tab_menu,
                tab_drag: &mut tab_drag,
                tab_drag_over: &mut tab_drag_over,
                tab_list: &mut tab_list,
            },
            b,
            &remaining,
            1,
        );

        assert_eq!(active, a);
        assert!(split.is_some());
        assert_eq!(tab_drag, Some(a));
        assert_eq!(tab_drag_over, None);
    }

    #[test]
    fn edge_drop_while_split_keeps_pane_partners() {
        let a = TabId(1);
        let b = TabId(2);
        let c = TabId(3);
        // Split A|C; B is the strip neighbor of A and must NOT be pulled in.
        let plan =
            plan_tab_drop(a, SplitDropZone::Top, a, Some(side(a, c)), Some(b)).expect("plan");
        assert_eq!(
            plan,
            TabDropPlan {
                split: Some(stacked(a, c)),
                active: a,
            },
            "perpendicular edge flips orientation without ejecting C"
        );
    }

    #[test]
    fn edge_drop_assigns_third_tab_to_slot() {
        let a = TabId(1);
        let b = TabId(2);
        let c = TabId(3);
        let plan =
            plan_tab_drop(c, SplitDropZone::Left, a, Some(side(a, b)), Some(b)).expect("plan");
        assert_eq!(
            plan,
            TabDropPlan {
                split: Some(side(c, b)),
                active: c,
            }
        );
    }

    #[test]
    fn create_split_keeps_focus_on_dragged() {
        let a = TabId(1);
        let b = TabId(2);
        let plan = plan_tab_drop(a, SplitDropZone::Right, a, None, Some(b)).expect("plan");
        assert_eq!(
            plan,
            TabDropPlan {
                split: Some(side(b, a)),
                active: a,
            },
            "right drop places dragged on the right but keeps it focused"
        );
    }

    #[test]
    fn center_drop_opens_beside() {
        let a = TabId(1);
        let b = TabId(2);
        let plan = plan_tab_drop(b, SplitDropZone::Center, a, None, Some(b)).expect("plan");
        assert_eq!(
            plan,
            TabDropPlan {
                split: Some(side(a, b)),
                active: b,
            }
        );
    }

    #[test]
    fn other_pane_swaps_when_dragging_either_pane() {
        let a = TabId(1);
        let b = TabId(2);
        let split = side(a, b);
        let from_active = plan_tab_drop(a, SplitDropZone::OtherPane, a, Some(split), Some(b))
            .expect("swap from active");
        assert_eq!(from_active.split, Some(swap_like(split)));
        assert_eq!(from_active.active, a);

        let from_other = plan_tab_drop(b, SplitDropZone::ActivePane, a, Some(split), Some(b))
            .expect("swap from other");
        assert_eq!(from_other.split, Some(swap_like(split)));
        assert_eq!(from_other.active, b);
    }

    fn swap_like(split: SplitState) -> SplitState {
        SplitState {
            first: split.second,
            second: split.first,
            stacked: split.stacked,
            ratio: 1.0 - split.ratio,
        }
    }

    #[test]
    fn tab_row_onto_pane_replaces_that_slot() {
        let a = TabId(1);
        let b = TabId(2);
        let c = TabId(3);
        let plan =
            plan_tab_drop(c, SplitDropZone::Tab(b), a, Some(side(a, b)), Some(b)).expect("plan");
        assert_eq!(
            plan,
            TabDropPlan {
                split: Some(side(a, c)),
                active: c,
            }
        );
    }

    #[test]
    fn tab_row_pane_onto_hidden_fills_other_pane() {
        let a = TabId(1);
        let b = TabId(2);
        let c = TabId(3);
        let plan =
            plan_tab_drop(a, SplitDropZone::Tab(c), a, Some(side(a, b)), Some(b)).expect("plan");
        assert_eq!(
            plan,
            TabDropPlan {
                split: Some(side(a, c)),
                active: a,
            }
        );
    }
}
