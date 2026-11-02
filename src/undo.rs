//! Undo / redo stack for annotation edits and page insert/delete.
//!
//! Extracted from `app` so the history can be unit-tested without booting egui.

use std::collections::VecDeque;

use crate::annot::{restore_session, Session};

/// Max retained undo entries (oldest dropped from the front).
pub(crate) const UNDO_CAP: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Undo,
    Redo,
}

#[derive(Clone, PartialEq)]
pub(crate) enum UndoEntry {
    Annots(Session),
    /// A blank page was inserted at this index.
    InsertPage {
        index: usize,
    },
    /// A page was deleted at this index (undo re-inserts a blank page).
    DeletePage {
        index: usize,
    },
}

/// How to restore undo/redo stacks when a page op fails after they were pre-mutated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageOpFailRevert {
    /// Entry was moved undo→redo; on failure pop redo and push undo.
    FromRedo,
    /// Entry was moved redo→undo; on failure pop undo and push redo.
    FromUndo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingPageOp {
    /// Insert blank page at `index`. `record_undo` is set for user-driven inserts.
    Insert {
        index: usize,
        record_undo: bool,
        revert: Option<PageOpFailRevert>,
    },
    /// Delete page at `index`. `record_undo` is set for user-driven deletes.
    Delete {
        index: usize,
        record_undo: bool,
        revert: Option<PageOpFailRevert>,
    },
}

/// Worker action the app must send after a page-op step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageOpKind {
    Insert,
    Delete,
}

/// Result of [`UndoStack::step`].
#[derive(Clone, Debug)]
pub(crate) enum StepOutcome {
    /// Restored annotation session; caller replaces the live session and clears edit UI.
    Annots { session: Session },
    /// Page op staged on the stack; caller sends the matching worker job.
    PageOp { kind: PageOpKind, index: usize },
}

#[derive(Clone, Default)]
pub(crate) struct UndoStack {
    pending: Option<Session>,
    edit_id: Option<u64>,
    undo: VecDeque<UndoEntry>,
    redo: VecDeque<UndoEntry>,
    pub(crate) page_op: Option<PendingPageOp>,
}

impl UndoStack {
    #[cfg(test)]
    pub(crate) fn undo_len(&self) -> usize {
        self.undo.len()
    }

    #[cfg(test)]
    pub(crate) fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Snapshot the live session if no pending edit is already armed.
    pub(crate) fn arm(&mut self, session: &Session) {
        if self.pending.is_none() {
            self.pending = Some(session.clone());
        }
    }

    /// Commit the pending snapshot if the live session changed.
    ///
    /// Returns `true` when an undo entry was pushed (caller should mark dirty).
    pub(crate) fn seal(&mut self, session: &Session) -> bool {
        let Some(before) = self.pending.take() else {
            return false;
        };
        self.edit_id = None;
        if before.eq_for_undo(session) {
            return false;
        }
        self.push(UndoEntry::Annots(before));
        self.redo.clear();
        true
    }

    /// Begin (or continue) a multi-keystroke edit of one annotation.
    ///
    /// Returns `true` when sealing the previous pending edit pushed an undo entry.
    pub(crate) fn begin_edit(&mut self, session: &Session, id: u64) -> bool {
        if self.edit_id == Some(id) && self.pending.is_some() {
            return false;
        }
        let sealed = self.seal(session);
        self.arm(session);
        self.edit_id = Some(id);
        sealed
    }

    pub(crate) fn tag_edit(&mut self, id: u64) {
        self.edit_id = Some(id);
    }

    /// Seal if an annotated edit session is open.
    pub(crate) fn end_edit(&mut self, session: &Session) -> bool {
        if self.edit_id.is_some() {
            self.seal(session)
        } else {
            false
        }
    }

    /// Push a page-op undo entry after a successful user-driven insert/delete.
    pub(crate) fn record_page_op(&mut self, entry: UndoEntry) {
        self.push(entry);
        self.redo.clear();
    }

    /// Roll stacks back when a page op that already moved an entry fails.
    pub(crate) fn revert_failed_page_op(&mut self, revert: PageOpFailRevert) {
        match revert {
            PageOpFailRevert::FromRedo => {
                if let Some(entry) = self.redo.pop_back() {
                    self.undo.push_back(entry);
                }
            }
            PageOpFailRevert::FromUndo => {
                if let Some(entry) = self.undo.pop_back() {
                    self.redo.push_back(entry);
                }
            }
        }
    }

    /// Pop one entry from the `dir` side and apply it.
    ///
    /// Annotation steps return a restored [`Session`]. Page steps stage
    /// [`Self::page_op`] with the inverse (undo) or same (redo) worker action.
    pub(crate) fn step(&mut self, dir: Direction, current: &Session) -> Option<StepOutcome> {
        let entry = match dir {
            Direction::Undo => self.undo.pop_back()?,
            Direction::Redo => self.redo.pop_back()?,
        };
        match entry {
            UndoEntry::Annots(prev) => {
                let opposite = UndoEntry::Annots(current.clone());
                match dir {
                    Direction::Undo => self.redo.push_back(opposite),
                    Direction::Redo => self.undo.push_back(opposite),
                }
                self.edit_id = None;
                let session = restore_session(current, prev);
                Some(StepOutcome::Annots { session })
            }
            UndoEntry::InsertPage { index } => {
                match dir {
                    Direction::Undo => self.redo.push_back(UndoEntry::InsertPage { index }),
                    Direction::Redo => self.undo.push_back(UndoEntry::InsertPage { index }),
                }
                let (kind, op) = match dir {
                    Direction::Undo => (
                        PageOpKind::Delete,
                        PendingPageOp::Delete {
                            index,
                            record_undo: false,
                            revert: Some(PageOpFailRevert::FromRedo),
                        },
                    ),
                    Direction::Redo => (
                        PageOpKind::Insert,
                        PendingPageOp::Insert {
                            index,
                            record_undo: false,
                            revert: Some(PageOpFailRevert::FromUndo),
                        },
                    ),
                };
                self.page_op = Some(op);
                Some(StepOutcome::PageOp { kind, index })
            }
            UndoEntry::DeletePage { index } => {
                match dir {
                    Direction::Undo => self.redo.push_back(UndoEntry::DeletePage { index }),
                    Direction::Redo => self.undo.push_back(UndoEntry::DeletePage { index }),
                }
                let (kind, op) = match dir {
                    Direction::Undo => (
                        PageOpKind::Insert,
                        PendingPageOp::Insert {
                            index,
                            record_undo: false,
                            revert: Some(PageOpFailRevert::FromRedo),
                        },
                    ),
                    Direction::Redo => (
                        PageOpKind::Delete,
                        PendingPageOp::Delete {
                            index,
                            record_undo: false,
                            revert: Some(PageOpFailRevert::FromUndo),
                        },
                    ),
                };
                self.page_op = Some(op);
                Some(StepOutcome::PageOp { kind, index })
            }
        }
    }

    fn push(&mut self, entry: UndoEntry) {
        self.undo.push_back(entry);
        while self.undo.len() > UNDO_CAP {
            self.undo.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annot::{AnnotKind, Annotation};
    use crate::geom::{PdfRect, Rgb};
    use std::sync::Arc;

    fn text_annot(id: u64, content: &str) -> Annotation {
        Annotation {
            id,
            page: 0,
            xref: None,
            dirty: true,
            revision: 1,
            kind: AnnotKind::Text {
                rect: PdfRect::new(0.0, 0.0, 40.0, 20.0),
                content: content.into(),
                size: 12.0,
                color: Rgb::new(0, 0, 0),
            },
        }
    }

    fn session_with(annots: Vec<Annotation>, next_id: u64) -> Session {
        Session {
            annotations: annots.into_iter().map(Arc::new).collect(),
            next_id,
            epoch: 1,
            pending_deletes: Vec::new(),
        }
    }

    #[test]
    fn push_undo_redo_annots() {
        let mut stack = UndoStack::default();
        let mut live = session_with(vec![text_annot(1, "a")], 2);

        stack.arm(&live);
        // Edit content via COW.
        if let Some(annot) = live.get_mut(1) {
            if let AnnotKind::Text { content, .. } = &mut annot.kind {
                *content = "b".into();
            }
            annot.revision = 2;
        }
        live.epoch = 2;
        assert!(stack.seal(&live));
        assert_eq!(stack.undo_len(), 1);
        assert_eq!(stack.redo_len(), 0);

        let StepOutcome::Annots { session } =
            stack.step(Direction::Undo, &live).expect("undo annots")
        else {
            panic!("expected annots outcome");
        };
        assert_eq!(session.get(1).unwrap().kind, text_annot(1, "a").kind);
        assert_eq!(stack.undo_len(), 0);
        assert_eq!(stack.redo_len(), 1);
        live = session;

        let StepOutcome::Annots { session } =
            stack.step(Direction::Redo, &live).expect("redo annots")
        else {
            panic!("expected annots outcome");
        };
        match &session.get(1).unwrap().kind {
            AnnotKind::Text { content, .. } => assert_eq!(content, "b"),
            other => panic!("expected text, got {other:?}"),
        }
        assert_eq!(stack.undo_len(), 1);
        assert_eq!(stack.redo_len(), 0);
    }

    #[test]
    fn seal_skips_unchanged_and_shares_arcs() {
        let mut stack = UndoStack::default();
        let live = session_with(vec![text_annot(1, "a"), text_annot(2, "keep")], 3);
        stack.arm(&live);
        assert!(!stack.seal(&live));
        assert_eq!(stack.undo_len(), 0);

        let mut stack = UndoStack::default();
        let mut live = session_with(vec![text_annot(1, "a"), text_annot(2, "keep")], 3);
        let shared_before = Arc::as_ptr(&live.annotations[1]);
        stack.arm(&live);
        // Touch only id 1.
        if let Some(annot) = live.get_mut(1) {
            if let AnnotKind::Text { content, .. } = &mut annot.kind {
                *content = "edited".into();
            }
        }
        live.epoch += 1;
        assert!(stack.seal(&live));
        let UndoEntry::Annots(before) = stack.undo.back().unwrap() else {
            panic!("expected annots entry");
        };
        // Untouched annotation still shares the Arc with the live session.
        assert!(std::ptr::eq(
            Arc::as_ptr(&before.annotations[1]),
            Arc::as_ptr(&live.annotations[1])
        ));
        assert_eq!(Arc::as_ptr(&live.annotations[1]), shared_before);
        // Touched one was replaced (no longer the same pointer as the snapshot).
        assert!(!std::ptr::eq(
            Arc::as_ptr(&before.annotations[0]),
            Arc::as_ptr(&live.annotations[0])
        ));
    }

    #[test]
    fn cap_drops_oldest() {
        let mut stack = UndoStack::default();
        let mut live = session_with(vec![text_annot(1, "start")], 2);
        for i in 0..UNDO_CAP + 5 {
            stack.arm(&live);
            if let Some(annot) = live.get_mut(1) {
                if let AnnotKind::Text { content, .. } = &mut annot.kind {
                    *content = format!("v{i}");
                }
            }
            live.epoch += 1;
            assert!(stack.seal(&live));
        }
        assert_eq!(stack.undo_len(), UNDO_CAP);
        // First five entries dropped; oldest kept is the pre-image of edit i = 5 ("v4").
        let UndoEntry::Annots(oldest) = stack.undo.front().unwrap() else {
            panic!("expected annots");
        };
        match &oldest.get(1).unwrap().kind {
            AnnotKind::Text { content, .. } => assert_eq!(content, "v4"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn page_op_rollback_on_failure() {
        let mut stack = UndoStack::default();
        stack.push(UndoEntry::InsertPage { index: 1 });
        assert_eq!(stack.undo_len(), 1);

        let live = Session::new();
        let outcome = stack.step(Direction::Undo, &live).expect("undo insert");
        assert!(matches!(
            outcome,
            StepOutcome::PageOp {
                kind: PageOpKind::Delete,
                index: 1
            }
        ));
        assert_eq!(stack.redo_len(), 1);
        assert_eq!(stack.undo_len(), 0);
        assert!(matches!(
            stack.page_op,
            Some(PendingPageOp::Delete {
                index: 1,
                record_undo: false,
                revert: Some(PageOpFailRevert::FromRedo),
            })
        ));

        let revert = match stack.page_op.take() {
            Some(PendingPageOp::Delete { revert, .. }) => revert,
            other => panic!("expected delete pending, got {other:?}"),
        };
        stack.revert_failed_page_op(revert.expect("revert flag"));
        assert_eq!(stack.undo_len(), 1);
        assert_eq!(stack.redo_len(), 0);
        assert!(matches!(
            stack.undo.back(),
            Some(UndoEntry::InsertPage { index: 1 })
        ));
    }

    #[test]
    fn interleaved_page_op_and_annot_edits() {
        let mut stack = UndoStack::default();
        let mut live = session_with(vec![text_annot(1, "a")], 2);

        // Annot edit.
        stack.arm(&live);
        if let Some(annot) = live.get_mut(1) {
            if let AnnotKind::Text { content, .. } = &mut annot.kind {
                *content = "b".into();
            }
        }
        live.epoch += 1;
        assert!(stack.seal(&live));

        // Page insert recorded.
        stack.record_page_op(UndoEntry::InsertPage { index: 1 });
        assert_eq!(stack.undo_len(), 2);

        // Another annot edit.
        stack.arm(&live);
        if let Some(annot) = live.get_mut(1) {
            if let AnnotKind::Text { content, .. } = &mut annot.kind {
                *content = "c".into();
            }
        }
        live.epoch += 1;
        assert!(stack.seal(&live));
        assert_eq!(stack.undo_len(), 3);

        // Undo annot → page → annot.
        let StepOutcome::Annots { session } = stack.step(Direction::Undo, &live).unwrap() else {
            panic!("expected annots");
        };
        live = session;
        match &live.get(1).unwrap().kind {
            AnnotKind::Text { content, .. } => assert_eq!(content, "b"),
            other => panic!("expected text, got {other:?}"),
        }

        let outcome = stack.step(Direction::Undo, &live).unwrap();
        assert!(matches!(
            outcome,
            StepOutcome::PageOp {
                kind: PageOpKind::Delete,
                index: 1
            }
        ));
        // Simulate successful page-op completion (clear pending; no session change needed here).
        stack.page_op = None;

        let StepOutcome::Annots { session } = stack.step(Direction::Undo, &live).unwrap() else {
            panic!("expected annots");
        };
        live = session;
        match &live.get(1).unwrap().kind {
            AnnotKind::Text { content, .. } => assert_eq!(content, "a"),
            other => panic!("expected text, got {other:?}"),
        }
        assert_eq!(stack.undo_len(), 0);
        assert_eq!(stack.redo_len(), 3);

        // Redo page op from the middle of the redo stack path: redo annot first.
        let StepOutcome::Annots { session } = stack.step(Direction::Redo, &live).unwrap() else {
            panic!("expected annots");
        };
        live = session;
        let outcome = stack.step(Direction::Redo, &live).unwrap();
        assert!(matches!(
            outcome,
            StepOutcome::PageOp {
                kind: PageOpKind::Insert,
                index: 1
            }
        ));
        stack.page_op = None;
        let StepOutcome::Annots { session } = stack.step(Direction::Redo, &live).unwrap() else {
            panic!("expected annots");
        };
        match &session.get(1).unwrap().kind {
            AnnotKind::Text { content, .. } => assert_eq!(content, "c"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn redo_page_op_failure_rolls_back_to_redo() {
        let mut stack = UndoStack::default();
        stack.redo.push_back(UndoEntry::DeletePage { index: 2 });
        let live = Session::new();
        let outcome = stack.step(Direction::Redo, &live).unwrap();
        assert!(matches!(
            outcome,
            StepOutcome::PageOp {
                kind: PageOpKind::Delete,
                index: 2
            }
        ));
        let revert = match stack.page_op.take() {
            Some(PendingPageOp::Delete { revert, .. }) => revert,
            other => panic!("expected delete, got {other:?}"),
        };
        stack.revert_failed_page_op(revert.unwrap());
        assert_eq!(stack.redo_len(), 1);
        assert_eq!(stack.undo_len(), 0);
    }
}
