//! Tab open/close/activate, detach, and split (RF9b).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::ViewportCommand;

use crate::instance;
use crate::pdf::DocId;

use super::*;

impl MarkerApp {
    pub(crate) fn open_path(&mut self, path: PathBuf) {
        if let Some(tab) = self.tabs.iter().find(|tab| tab.doc.path == path) {
            let id = tab.id;
            if self.active != id {
                self.active = id;
                self.pulse_tab_list();
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

    /// Activate a tab from the switcher / list, respecting split pane assignment.
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
        self.math_scan.remove(&doc_id);
        self.math_deadlines.retain(|(g, _, _), _| *g != doc_id);
        self.math_last_good.retain(|(g, _, _), _| *g != doc_id);
        if tab.assistant.streaming {
            self.assistant
                .cancel(tab.doc.doc_id, tab.assistant.request_seq);
        }
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
        match zone {
            SplitDropZone::Left | SplitDropZone::Top => {
                let stacked = matches!(zone, SplitDropZone::Top);
                if dragged == self.active {
                    let Some(other) = self.adjacent_tab_id(self.active, 1) else {
                        return;
                    };
                    self.set_split_panes(dragged, other, stacked);
                } else {
                    let prev = self.active;
                    self.active = dragged;
                    self.set_split_panes(dragged, prev, stacked);
                }
            }
            SplitDropZone::Right | SplitDropZone::Bottom => {
                let stacked = matches!(zone, SplitDropZone::Bottom);
                if dragged == self.active {
                    let Some(next) = self.adjacent_tab_id(self.active, 1) else {
                        return;
                    };
                    self.set_split_panes(next, dragged, stacked);
                    self.active = next;
                } else {
                    self.set_split_panes(self.active, dragged, stacked);
                }
            }
            SplitDropZone::OtherPane => {
                let Some(split) = self.split else {
                    return;
                };
                let unfocused = split.other(self.active);
                if dragged == self.active {
                    // Swap pane contents so the focused doc moves to the other side.
                    self.split = Some(SplitState {
                        first: split.second,
                        second: split.first,
                        stacked: split.stacked,
                        ratio: 1.0 - split.ratio,
                    });
                } else if dragged != unfocused {
                    if let Some(s) = self.split.as_mut() {
                        if s.first == unfocused {
                            s.first = dragged;
                        } else {
                            s.second = dragged;
                        }
                    }
                }
            }
            SplitDropZone::ActivePane => {
                let Some(split) = self.split else {
                    return;
                };
                if dragged == self.active {
                    return;
                }
                if dragged == split.other(self.active) {
                    self.active = dragged;
                } else if let Some(s) = self.split.as_mut() {
                    if s.first == self.active {
                        s.first = dragged;
                    } else {
                        s.second = dragged;
                    }
                    self.active = dragged;
                }
            }
            SplitDropZone::Tab(target) => {
                if target == dragged || self.tab_index(target).is_none() {
                    return;
                }
                let stacked = self.split.map(|s| s.stacked).unwrap_or(false);
                if self.active == dragged {
                    self.set_split_panes(dragged, target, stacked);
                } else if self.active == target {
                    self.set_split_panes(target, dragged, stacked);
                } else {
                    self.active = dragged;
                    self.set_split_panes(dragged, target, stacked);
                }
            }
        }
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
    use super::{reconcile_tab_ids_after_close, SplitState, TabId, TabIdChrome, TabListState};
    use egui::Pos2;

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
}
