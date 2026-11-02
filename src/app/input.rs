//! Keyboard input, vim navigation, and the declarative keymap (RF9b).

use egui::{Key, ViewportCommand};

use crate::assistant::CaptureMode;
use crate::instance;

use super::*;

/// Cheat-sheet section. Order matches the `?` window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShortcutSection {
    Tools,
    Navigate,
    Edit,
    View,
    Assistant,
}

impl ShortcutSection {
    pub(crate) const ALL: [ShortcutSection; 5] = [
        ShortcutSection::Tools,
        ShortcutSection::Navigate,
        ShortcutSection::Edit,
        ShortcutSection::View,
        ShortcutSection::Assistant,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Tools => "Tools",
            Self::Navigate => "Navigate",
            Self::Edit => "Edit",
            Self::View => "View",
            Self::Assistant => "Assistant",
        }
    }
}

/// One row in the `?` cheat sheet (derived from [`KEYMAP`] labels).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShortcutEntry {
    pub section: ShortcutSection,
    pub keys: &'static str,
    pub action: &'static str,
}

/// Modifier chord. `shift: None` means "ignore the Shift bit" (matches older if-chain).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct KeyChord {
    pub key: Key,
    pub command: bool,
    pub alt: bool,
    pub shift: Option<bool>,
}

impl KeyChord {
    const fn key(key: Key) -> Self {
        Self {
            key,
            command: false,
            alt: false,
            shift: Some(false),
        }
    }

    const fn shift_key(key: Key) -> Self {
        Self {
            key,
            command: false,
            alt: false,
            shift: Some(true),
        }
    }

    const fn cmd(key: Key) -> Self {
        Self {
            key,
            command: true,
            alt: false,
            shift: None,
        }
    }

    const fn cmd_exact(key: Key, shift: bool) -> Self {
        Self {
            key,
            command: true,
            alt: false,
            shift: Some(shift),
        }
    }

    const fn cmd_alt(key: Key) -> Self {
        Self {
            key,
            command: true,
            alt: true,
            shift: Some(false),
        }
    }

    pub(crate) fn matches(self, input: &egui::InputState) -> bool {
        if !input.key_pressed(self.key) {
            return false;
        }
        if input.modifiers.command != self.command {
            return false;
        }
        if input.modifiers.alt != self.alt {
            return false;
        }
        if let Some(shift) = self.shift {
            if input.modifiers.shift != shift {
                return false;
            }
        }
        true
    }

    /// Two chords collide when some modifier combination can satisfy both.
    #[cfg(test)]
    fn conflicts_with(self, other: Self) -> bool {
        if self.key != other.key || self.command != other.command || self.alt != other.alt {
            return false;
        }
        match (self.shift, other.shift) {
            (None, _) | (_, None) => true,
            (Some(a), Some(b)) => a == b,
        }
    }
}

/// Dispatchable action for a keymap row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Save,
    SaveAs,
    Open,
    Search,
    JumpPageFocus,
    InsertPage,
    DeletePage,
    CloseTab,
    ToggleToolbar,
    ToggleZen,
    ToggleAssistant,
    AttachOrSelectText,
    BeginRegionCapture,
    ExplainSelection,
    AssistantStop,
    NextTab,
    PrevTab,
    ToggleSplitSide,
    ToggleSplitStacked,
    FocusOtherPane,
    Undo,
    Redo,
    ToggleCheatsheet,
    OpenSearchSlash,
    SetTool(Tool),
    NextSearchHit,
    PrevSearchHit,
    DeleteSelected,
    ZoomIn,
    ZoomOut,
    FitToggle,
    PageDown,
    PageUp,
    JumpStart,
    JumpEnd,
    ArrowDown,
    ArrowUp,
    ArrowRight,
    ArrowLeft,
    Escape,
}

/// Extra runtime gate beyond chord match + editing guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyGate {
    Always,
    /// N / Shift+N only while find is open with hits.
    SearchHits,
    /// Delete/Backspace only with a non-empty selection and no edit focus.
    HasSelection,
    /// Left/Right: style bar owns them when a selection is active.
    HorizontalArrow,
}

/// One keymap / cheat-sheet row.
#[derive(Clone, Copy, Debug)]
pub(crate) struct KeymapRow {
    /// `None` = documentation-only (vim composites that are not simple chords).
    pub chord: Option<KeyChord>,
    pub action: Option<Action>,
    /// Fire even when a text field wants keyboard input.
    pub global: bool,
    /// Skip when a FreeText/math annot is being edited (post-`wants_keyboard_input` path).
    pub when_not_editing: bool,
    /// Include in the `?` cheat sheet (composites preferred over split chords).
    pub cheatsheet: bool,
    gate: KeyGate,
    pub section: ShortcutSection,
    pub keys: &'static str,
    pub label: &'static str,
}

/// Declarative keymap: source of truth for dispatch and the `?` cheat sheet.
pub(crate) const KEYMAP: &[KeymapRow] = &[
    // Tools (cheat sheet + bare-letter dispatch when not in vim)
    KeymapRow {
        chord: Some(KeyChord::key(Key::S)),
        action: Some(Action::SetTool(Tool::Select)),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Tools,
        keys: "S",
        label: "Select",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::H)),
        action: Some(Action::SetTool(Tool::Highlight)),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Tools,
        keys: "H",
        label: "Highlight",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::U)),
        action: Some(Action::SetTool(Tool::Underline)),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Tools,
        keys: "U",
        label: "Underline",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::X)),
        action: Some(Action::SetTool(Tool::StrikeOut)),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Tools,
        keys: "X",
        label: "Strike out",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::W)),
        action: Some(Action::SetTool(Tool::Squiggly)),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Tools,
        keys: "W",
        label: "Squiggly underline",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::T)),
        action: Some(Action::SetTool(Tool::Text)),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Tools,
        keys: "T",
        label: "Text",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::N)),
        action: Some(Action::SetTool(Tool::Note)),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Tools,
        keys: "N",
        label: "Note",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::R)),
        action: Some(Action::SetTool(Tool::Rect)),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Tools,
        keys: "R",
        label: "Rectangle",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::E)),
        action: Some(Action::SetTool(Tool::Ellipse)),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Tools,
        keys: "E",
        label: "Ellipse",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::I)),
        action: Some(Action::SetTool(Tool::Line)),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Tools,
        keys: "I",
        label: "Line",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::D)),
        action: Some(Action::SetTool(Tool::Draw)),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Tools,
        keys: "D",
        label: "Draw",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::M)),
        action: Some(Action::SetTool(Tool::Math)),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Tools,
        keys: "M",
        label: "Math",
    },
    // Navigate — vim docs (handled in handle_vim, not as simple chords)
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "J / K",
        label: "Pan down / up",
    },
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "L / Shift+L",
        label: "Pan right / left",
    },
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "← → ↑ ↓",
        label: "Scroll, or nudge selection",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::ArrowDown)),
        action: Some(Action::ArrowDown),
        global: false,
        when_not_editing: true,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "↓",
        label: "Scroll / nudge down",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::ArrowUp)),
        action: Some(Action::ArrowUp),
        global: false,
        when_not_editing: true,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "↑",
        label: "Scroll / nudge up",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::ArrowRight)),
        action: Some(Action::ArrowRight),
        global: false,
        when_not_editing: true,
        cheatsheet: false,
        gate: KeyGate::HorizontalArrow,
        section: ShortcutSection::Navigate,
        keys: "→",
        label: "Scroll / nudge right",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::ArrowLeft)),
        action: Some(Action::ArrowLeft),
        global: false,
        when_not_editing: true,
        cheatsheet: false,
        gate: KeyGate::HorizontalArrow,
        section: ShortcutSection::Navigate,
        keys: "←",
        label: "Scroll / nudge left",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::Home)),
        action: Some(Action::JumpStart),
        global: false,
        when_not_editing: true,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "Home",
        label: "First page",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::End)),
        action: Some(Action::JumpEnd),
        global: false,
        when_not_editing: true,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "End",
        label: "Last page",
    },
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "Home / End",
        label: "First / last page",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::Space)),
        action: Some(Action::PageDown),
        global: false,
        when_not_editing: true,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "Space",
        label: "Page down",
    },
    KeymapRow {
        chord: Some(KeyChord::shift_key(Key::Space)),
        action: Some(Action::PageUp),
        global: false,
        when_not_editing: true,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "Shift+Space",
        label: "Page up",
    },
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "Space / Shift+Space",
        label: "Page down / up",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::PageDown)),
        action: Some(Action::PageDown),
        global: false,
        when_not_editing: false,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "PageDown",
        label: "Page down",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::PageUp)),
        action: Some(Action::PageUp),
        global: false,
        when_not_editing: false,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "PageUp",
        label: "Page up",
    },
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "PageDown / PageUp",
        label: "Page down / up",
    },
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "Ctrl+D / Ctrl+U",
        label: "Half-page down / up",
    },
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "gg / G",
        label: "First / last page",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd(Key::G)),
        action: Some(Action::JumpPageFocus),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "Ctrl+G",
        label: "Jump to page",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd_exact(Key::Tab, false)),
        action: Some(Action::NextTab),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "Ctrl+Tab",
        label: "Next tab",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd_exact(Key::Tab, true)),
        action: Some(Action::PrevTab),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "Ctrl+Shift+Tab",
        label: "Previous tab",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd_exact(Key::Backslash, false)),
        action: Some(Action::ToggleSplitSide),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "Ctrl+\\",
        label: "Split side-by-side",
    },
    KeymapRow {
        chord: Some(KeyChord {
            key: Key::Backslash,
            command: true,
            alt: false,
            shift: Some(true),
        }),
        action: Some(Action::ToggleSplitStacked),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "Ctrl+Shift+\\",
        label: "Split stacked",
    },
    KeymapRow {
        chord: Some(KeyChord {
            key: Key::Backslash,
            command: true,
            alt: true,
            shift: None,
        }),
        action: Some(Action::FocusOtherPane),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Navigate,
        keys: "Ctrl+Alt+\\",
        label: "Focus other pane",
    },
    // Edit
    KeymapRow {
        chord: Some(KeyChord::cmd_exact(Key::Z, false)),
        action: Some(Action::Undo),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Edit,
        keys: "Ctrl+Z",
        label: "Undo",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd_exact(Key::Y, false)),
        action: Some(Action::Redo),
        global: false,
        when_not_editing: false,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::Edit,
        keys: "Ctrl+Y",
        label: "Redo",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd_exact(Key::Z, true)),
        action: Some(Action::Redo),
        global: false,
        when_not_editing: false,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::Edit,
        keys: "Ctrl+Shift+Z",
        label: "Redo",
    },
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Edit,
        keys: "Ctrl+Y / Ctrl+Shift+Z",
        label: "Redo",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::Delete)),
        action: Some(Action::DeleteSelected),
        global: false,
        when_not_editing: false,
        cheatsheet: false,
        gate: KeyGate::HasSelection,
        section: ShortcutSection::Edit,
        keys: "Delete",
        label: "Delete selection",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::Backspace)),
        action: Some(Action::DeleteSelected),
        global: false,
        when_not_editing: false,
        cheatsheet: false,
        gate: KeyGate::HasSelection,
        section: ShortcutSection::Edit,
        keys: "Backspace",
        label: "Delete selection",
    },
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Edit,
        keys: "Delete / Backspace",
        label: "Delete selection",
    },
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Edit,
        keys: "Ctrl+C",
        label: "Copy selection",
    },
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Edit,
        keys: "Ctrl+Shift+V",
        label: "Paste image",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd_exact(Key::S, false)),
        action: Some(Action::Save),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Edit,
        keys: "Ctrl+S",
        label: "Save",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd_exact(Key::S, true)),
        action: Some(Action::SaveAs),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Edit,
        keys: "Ctrl+Shift+S",
        label: "Save As",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd_exact(Key::Enter, true)),
        action: Some(Action::InsertPage),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Edit,
        keys: "Ctrl+Shift+Enter",
        label: "Insert blank page",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd_exact(Key::Backspace, true)),
        action: Some(Action::DeletePage),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Edit,
        keys: "Ctrl+Shift+Backspace",
        label: "Delete current page",
    },
    // View
    KeymapRow {
        chord: Some(KeyChord::cmd(Key::O)),
        action: Some(Action::Open),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "Ctrl+O",
        label: "Open PDF",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd(Key::W)),
        action: Some(Action::CloseTab),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "Ctrl+W",
        label: "Close tab",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd(Key::F)),
        action: Some(Action::Search),
        global: true,
        when_not_editing: false,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "Ctrl+F",
        label: "Search",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::Slash)),
        action: Some(Action::OpenSearchSlash),
        global: false,
        when_not_editing: false,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "/",
        label: "Search",
    },
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "Ctrl+F / /",
        label: "Search",
    },
    // Search-hit stepping: same physical N as Note tool; gated so they do not collide.
    KeymapRow {
        chord: Some(KeyChord::key(Key::N)),
        action: Some(Action::NextSearchHit),
        global: false,
        when_not_editing: false,
        cheatsheet: false,
        gate: KeyGate::SearchHits,
        section: ShortcutSection::View,
        keys: "N",
        label: "Next search hit",
    },
    KeymapRow {
        chord: Some(KeyChord::shift_key(Key::N)),
        action: Some(Action::PrevSearchHit),
        global: false,
        when_not_editing: false,
        cheatsheet: false,
        gate: KeyGate::SearchHits,
        section: ShortcutSection::View,
        keys: "Shift+N",
        label: "Previous search hit",
    },
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "N / Shift+N",
        label: "Next / previous search hit",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd(Key::Equals)),
        action: Some(Action::ZoomIn),
        global: false,
        when_not_editing: false,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "Ctrl+=",
        label: "Zoom in",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd(Key::Plus)),
        action: Some(Action::ZoomIn),
        global: false,
        when_not_editing: false,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "Ctrl++",
        label: "Zoom in",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd(Key::Minus)),
        action: Some(Action::ZoomOut),
        global: false,
        when_not_editing: false,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "Ctrl+-",
        label: "Zoom out",
    },
    KeymapRow {
        chord: None,
        action: None,
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "Ctrl+= / Ctrl+-",
        label: "Zoom in / out",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd(Key::Num0)),
        action: Some(Action::FitToggle),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "Ctrl+0",
        label: "Fit width, then height",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd_exact(Key::B, true)),
        action: Some(Action::ToggleToolbar),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "Ctrl+Shift+B",
        label: "Toggle tools strip",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::F11)),
        action: Some(Action::ToggleZen),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "F11",
        label: "Zen mode",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::Questionmark)),
        action: Some(Action::ToggleCheatsheet),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "?",
        label: "Shortcut cheat sheet",
    },
    KeymapRow {
        // Shift+/ produces ? on many layouts; keep both chords.
        chord: Some(KeyChord::shift_key(Key::Slash)),
        action: Some(Action::ToggleCheatsheet),
        global: false,
        when_not_editing: false,
        cheatsheet: false,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "Shift+/",
        label: "Shortcut cheat sheet",
    },
    KeymapRow {
        chord: Some(KeyChord::key(Key::Escape)),
        action: Some(Action::Escape),
        global: false,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::View,
        keys: "Esc",
        label: "Dismiss / leave edit",
    },
    // Assistant
    KeymapRow {
        chord: Some(KeyChord::cmd_alt(Key::I)),
        action: Some(Action::ToggleAssistant),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Assistant,
        keys: "Ctrl+Alt+I",
        label: "Toggle assistant",
    },
    KeymapRow {
        // Old code: command+shift, alt not checked.
        chord: Some(KeyChord {
            key: Key::A,
            command: true,
            alt: false,
            shift: Some(true),
        }),
        action: Some(Action::AttachOrSelectText),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Assistant,
        keys: "Ctrl+Shift+A",
        label: "Attach / select text",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd_alt(Key::S)),
        action: Some(Action::BeginRegionCapture),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Assistant,
        keys: "Ctrl+Alt+S",
        label: "Capture region",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd_alt(Key::E)),
        action: Some(Action::ExplainSelection),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Assistant,
        keys: "Ctrl+Alt+E",
        label: "Explain selection",
    },
    KeymapRow {
        chord: Some(KeyChord::cmd(Key::Period)),
        action: Some(Action::AssistantStop),
        global: true,
        when_not_editing: false,
        cheatsheet: true,
        gate: KeyGate::Always,
        section: ShortcutSection::Assistant,
        keys: "Ctrl+.",
        label: "Stop assistant",
    },
];

/// Cheat-sheet rows derived from [`KEYMAP`] (`cheatsheet: true`).
pub(crate) static SHORTCUTS: &[ShortcutEntry] = &SHORTCUT_ENTRIES;

const SHORTCUT_ENTRIES: [ShortcutEntry; SHORTCUT_COUNT] = make_shortcuts();

const SHORTCUT_COUNT: usize = {
    let mut n = 0;
    let mut i = 0;
    while i < KEYMAP.len() {
        if KEYMAP[i].cheatsheet {
            n += 1;
        }
        i += 1;
    }
    n
};

const fn make_shortcuts() -> [ShortcutEntry; SHORTCUT_COUNT] {
    let mut out = [ShortcutEntry {
        section: ShortcutSection::Tools,
        keys: "",
        action: "",
    }; SHORTCUT_COUNT];
    let mut i = 0;
    let mut o = 0;
    while i < KEYMAP.len() {
        if KEYMAP[i].cheatsheet {
            out[o] = ShortcutEntry {
                section: KEYMAP[i].section,
                keys: KEYMAP[i].keys,
                action: KEYMAP[i].label,
            };
            o += 1;
        }
        i += 1;
    }
    out
}

impl MarkerApp {
    pub(crate) fn nudge(&mut self, dx: f32, dy: f32) {
        let view = self.view_rect;
        if let Some(doc) = self.doc_mut() {
            doc.scroll_x += dx;
            doc.scroll_y += dy;
            doc.clamp_scroll(view);
        }
    }

    /// Page-sized scroll used by PageDown/PageUp and Space / Shift+Space.
    pub(crate) fn page_scroll(&mut self, down: bool) {
        let height = self.view_rect.height();
        let view = self.view_rect;
        if let Some(doc) = self.doc_mut() {
            let delta = height * 0.9;
            if down {
                doc.scroll_y += delta;
            } else {
                doc.scroll_y -= delta;
            }
            doc.clamp_scroll(view);
        }
    }

    pub(crate) fn jump_doc_start(&mut self) {
        self.queue_jump(0, Some(0.0));
    }

    pub(crate) fn jump_doc_end(&mut self) {
        let last = self
            .doc()
            .map(|doc| doc.pages.len().saturating_sub(1))
            .unwrap_or(0);
        self.queue_jump(last, Some(0.0));
    }

    /// Nudge selected annotations in PDF space (screen pixels ÷ scale).
    pub(crate) fn nudge_selected(&mut self, dx_screen: f32, dy_screen: f32) {
        let scale = self.doc().map(|doc| doc.scale).unwrap_or(1.0).max(0.01);
        let dx = dx_screen / scale;
        let dy = dy_screen / scale;
        let ids = self
            .tab()
            .map(|tab| tab.selected.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            return;
        }
        self.seal_then_arm();
        if let Some(tab) = self.tab_mut() {
            for id in &ids {
                if let Some(annot) = tab.doc.session.get_mut(*id) {
                    annot.kind.translate(dx, dy);
                }
            }
        }
        self.seal_undo();
    }

    pub(crate) fn toggle_cheatsheet(&mut self) {
        self.cheatsheet_open = !self.cheatsheet_open;
    }

    /// Vim-style navigation. Returns true when the key should not also switch tools.
    pub(crate) fn handle_vim(&mut self, input: &egui::InputState) -> bool {
        if self.doc().is_none() {
            return false;
        }
        if input.modifiers.command && input.key_pressed(Key::D) {
            let steps = self.take_count();
            let dy = self.view_rect.height() * 0.5 * steps;
            self.nudge(0.0, dy);
            return true;
        }
        if input.modifiers.command && input.key_pressed(Key::U) {
            let steps = self.take_count();
            let dy = self.view_rect.height() * 0.5 * steps;
            self.nudge(0.0, -dy);
            return true;
        }
        if !input.modifiers.command && !input.modifiers.alt {
            if let Some(digit) = vim_digit(input) {
                self.vim_count = self.vim_count.saturating_mul(10).saturating_add(digit);
                return true;
            }
        }
        if input.key_pressed(Key::G) && !input.modifiers.command {
            if input.modifiers.shift {
                let count = self.vim_count;
                self.vim_count = 0;
                self.vim_g = false;
                let last = self
                    .doc()
                    .map(|doc| doc.pages.len().saturating_sub(1))
                    .unwrap_or(0);
                let page = if count == 0 {
                    last
                } else {
                    count.saturating_sub(1) as usize
                };
                self.queue_jump(page, Some(0.0));
            } else if self.vim_g {
                let page = self.take_count() as usize;
                let page = page.saturating_sub(1);
                self.vim_g = false;
                self.queue_jump(page, Some(0.0));
            } else {
                self.vim_g = true;
            }
            return true;
        }
        let step = (self.view_rect.height() * 0.045).clamp(28.0, 72.0);
        if input.key_pressed(Key::J) && !input.modifiers.command {
            let n = self.take_count();
            self.vim_g = false;
            self.nudge(0.0, step * n);
            return true;
        }
        if input.key_pressed(Key::K) && !input.modifiers.command {
            let n = self.take_count();
            self.vim_g = false;
            self.nudge(0.0, -step * n);
            return true;
        }
        // `H` is Highlight; vim `l` pans right. Shift+L pans left (stand-in for vim `h`).
        // Directions match the old H/L pair: L → +scroll_x (right), Shift+L → −scroll_x (left).
        if input.key_pressed(Key::L) && !input.modifiers.command {
            let n = self.take_count();
            self.vim_g = false;
            if input.modifiers.shift {
                self.nudge(-step * n, 0.0);
            } else {
                self.nudge(step * n, 0.0);
            }
            return true;
        }
        if input
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::Key { pressed: true, .. }))
        {
            self.vim_count = 0;
            self.vim_g = false;
        }
        false
    }

    pub(crate) fn leave_typing(&mut self) {
        let mut exit_zen = false;
        let mut seal = false;
        if self.capture != CaptureMode::None {
            self.cancel_capture();
            return;
        }
        if self.preset_editor_open {
            self.preset_editor_open = false;
            return;
        }
        if self.cheatsheet_open {
            self.cheatsheet_open = false;
            return;
        }
        if self.tab_list.take().is_some() {
            return;
        }
        if self.tab_menu.take().is_some() {
            return;
        }
        if self.tab_drag.take().is_some() {
            self.tab_drag_over = None;
            return;
        }
        if let Some(tab) = self.tab_mut() {
            if tab.menu.take().is_some() {
                return;
            }
            if tab.style_bar.take().is_some() {
                // Dismiss the style strip first; a second Escape clears selection.
                return;
            }
            if tab.editing.take().is_some() {
                tab.focus_edit = false;
                tab.live_math = None;
                tab.pending_text_caret = None;
                seal = true;
            } else if tab.assistant.learning.take().is_some()
                || !tab.selected.is_empty()
                || tab.text_sel.take().is_some()
            {
                tab.selected.clear();
                tab.editing = None;
                // Cleared learning selection and/or annotation/text selection.
            } else if tab.search.open {
                tab.search.open = false;
                tab.search.hits.clear();
                tab.search.query.clear();
                tab.search.last_sent.clear();
            } else {
                exit_zen = self.zen;
            }
        } else {
            exit_zen = self.zen;
        }
        if seal {
            self.end_edit_undo();
        }
        if exit_zen {
            self.zen = false;
            self.zen_chrome_until = None;
        }
    }

    /// Apply pending fullscreen exit from Escape (needs a ctx for the viewport command).
    pub(crate) fn sync_zen_viewport(&mut self, ctx: &egui::Context) {
        let fullscreen = ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
        if self.zen != fullscreen {
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(self.zen));
        }
    }

    pub(crate) fn take_count(&mut self) -> f32 {
        let count = self.vim_count.max(1);
        self.vim_count = 0;
        count as f32
    }

    pub(crate) fn handle_keys(&mut self, ctx: &egui::Context) {
        // Global chords (fire even while a text field wants input).
        self.dispatch_keymap(ctx, true, false, false);

        for file in ctx.input(|input| input.raw.dropped_files.clone()) {
            if let Some(path) = file.path {
                if instance::is_pdf(&path) {
                    self.open_path(path);
                }
            }
        }
        if ctx.wants_keyboard_input() {
            if ctx.input(|input| input.key_pressed(Key::Escape)) {
                if self.capture != CaptureMode::None {
                    self.cancel_capture();
                } else {
                    self.leave_typing();
                    self.sync_zen_viewport(ctx);
                }
            }
            return;
        }

        let editing = self.tab().is_some_and(|tab| tab.editing.is_some());
        let paste_image = ctx.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Paste(_)))
                || (input.modifiers.command && input.modifiers.shift && input.key_pressed(Key::V))
        });
        if paste_image && !editing {
            let _ = self.paste_clipboard_image();
        }
        let want_copy = ctx.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Copy))
                || (input.modifiers.command && !input.modifiers.shift && input.key_pressed(Key::C))
        });
        if want_copy && !editing {
            if let Some(text) = self.copyable_selection_text() {
                ctx.copy_text(text);
            }
        }

        let vim = ctx.input(|input| self.handle_vim(input));
        // Non-global keymap rows (tools, nav, undo, …). Vim blocks tool letters.
        self.dispatch_keymap(ctx, false, editing, vim);
    }

    pub(crate) fn dispatch_keymap(
        &mut self,
        ctx: &egui::Context,
        global_pass: bool,
        editing: bool,
        vim: bool,
    ) {
        let has_selection = self.tab().is_some_and(|tab| !tab.selected.is_empty());
        let style_bar_open = self.tab().is_some_and(|tab| tab.style_bar.is_some());
        let search_hits = self
            .tab()
            .is_some_and(|tab| tab.search.open && !tab.search.hits.is_empty());
        let step = (self.view_rect.height() * 0.045).clamp(28.0, 72.0);

        // Collect matching actions first so we do not hold `input` across `&mut self` dispatch.
        let mut matched: Vec<Action> = Vec::new();
        ctx.input(|input| {
            for row in KEYMAP {
                if row.global != global_pass {
                    continue;
                }
                let Some(chord) = row.chord else {
                    continue;
                };
                let Some(action) = row.action else {
                    continue;
                };
                if row.when_not_editing && editing {
                    continue;
                }
                if !chord.matches(input) {
                    continue;
                }
                // Tool letters and bare N are suppressed while vim consumed the key.
                if vim && matches!(action, Action::SetTool(_)) {
                    continue;
                }
                // Search-hit N must win over Note tool when find is open.
                if matches!(action, Action::SetTool(Tool::Note)) && search_hits {
                    continue;
                }
                if !row
                    .gate
                    .allows(has_selection, style_bar_open, search_hits, editing)
                {
                    continue;
                }
                // Undo/redo skipped while vim handled the key (old behavior).
                if vim && matches!(action, Action::Undo | Action::Redo) {
                    continue;
                }
                // Tool switch requires no command modifier (chord already bare).
                if matches!(action, Action::SetTool(_)) && input.modifiers.command {
                    continue;
                }
                matched.push(action);
            }
        });

        for action in matched {
            self.dispatch_action(ctx, action, step, has_selection, editing);
        }
    }

    pub(crate) fn dispatch_action(
        &mut self,
        ctx: &egui::Context,
        action: Action,
        step: f32,
        has_selection: bool,
        editing: bool,
    ) {
        match action {
            Action::Save => {
                if let Some(tab) = self.tab_mut() {
                    tab.force_save = true;
                }
            }
            Action::SaveAs => self.save_as_dialog(),
            Action::Open => self.open_dialog(),
            Action::Search | Action::OpenSearchSlash => self.open_search(),
            Action::JumpPageFocus => self.page_focus = true,
            Action::InsertPage => self.insert_page_after_current(),
            Action::DeletePage => self.delete_current_page(),
            Action::CloseTab => {
                let active = self.active;
                self.close_tab(active);
            }
            Action::ToggleToolbar => self.toggle_toolbar(),
            Action::ToggleZen => self.toggle_zen(ctx),
            Action::ToggleAssistant => self.toggle_assistant(),
            Action::AttachOrSelectText => {
                if self.tab().is_some_and(|t| t.assistant.learning.is_some()) {
                    self.attach_learning_text();
                } else {
                    self.begin_learning_select();
                }
            }
            Action::BeginRegionCapture => self.begin_region_capture(),
            Action::ExplainSelection => self.explain_selection(),
            Action::AssistantStop => self.assistant_stop(),
            Action::NextTab => {
                if self.tabs.len() >= 2 {
                    if let Some(next) = self.adjacent_tab_id(self.active, 1) {
                        self.activate_tab(next);
                    }
                }
            }
            Action::PrevTab => {
                if self.tabs.len() >= 2 {
                    if let Some(next) = self.adjacent_tab_id(self.active, -1) {
                        self.activate_tab(next);
                    }
                }
            }
            Action::ToggleSplitSide => self.toggle_split(false),
            Action::ToggleSplitStacked => self.toggle_split(true),
            Action::FocusOtherPane => self.focus_split_other(),
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::ToggleCheatsheet => self.toggle_cheatsheet(),
            Action::SetTool(tool) => {
                // Pressing H / U / X / W / R / E / I again restores the factory swatch (#85).
                if self.tool == tool {
                    self.restore_default_tool_color(tool);
                }
                self.tool = tool;
            }
            Action::NextSearchHit => self.search_step(1),
            Action::PrevSearchHit => self.search_step(-1),
            Action::DeleteSelected => self.delete_selected(),
            Action::ZoomIn => self.zoom_by(1.1),
            Action::ZoomOut => self.zoom_by(1.0 / 1.1),
            Action::FitToggle => self.fit_toggle(),
            Action::PageDown => self.page_scroll(true),
            Action::PageUp => self.page_scroll(false),
            Action::JumpStart => self.jump_doc_start(),
            Action::JumpEnd => self.jump_doc_end(),
            Action::ArrowDown => {
                if !editing {
                    if has_selection {
                        self.nudge_selected(0.0, step);
                    } else {
                        self.nudge(0.0, step);
                    }
                }
            }
            Action::ArrowUp => {
                if !editing {
                    if has_selection {
                        self.nudge_selected(0.0, -step);
                    } else {
                        self.nudge(0.0, -step);
                    }
                }
            }
            Action::ArrowRight => {
                if !editing {
                    if has_selection {
                        self.nudge_selected(step, 0.0);
                    } else {
                        self.nudge(step, 0.0);
                    }
                }
            }
            Action::ArrowLeft => {
                if !editing {
                    if has_selection {
                        self.nudge_selected(-step, 0.0);
                    } else {
                        self.nudge(-step, 0.0);
                    }
                }
            }
            Action::Escape => {
                if self.capture != CaptureMode::None {
                    self.cancel_capture();
                } else {
                    self.leave_typing();
                    self.sync_zen_viewport(ctx);
                }
            }
        }
    }
}

impl KeyGate {
    pub(crate) fn allows(
        self,
        has_selection: bool,
        style_bar_open: bool,
        search_hits: bool,
        editing: bool,
    ) -> bool {
        match self {
            Self::Always => true,
            Self::SearchHits => search_hits,
            Self::HasSelection => !editing && has_selection,
            Self::HorizontalArrow => !has_selection || !style_bar_open,
        }
    }
}

pub(crate) fn vim_digit(input: &egui::InputState) -> Option<u32> {
    const KEYS: [(egui::Key, u32); 10] = [
        (egui::Key::Num0, 0),
        (egui::Key::Num1, 1),
        (egui::Key::Num2, 2),
        (egui::Key::Num3, 3),
        (egui::Key::Num4, 4),
        (egui::Key::Num5, 5),
        (egui::Key::Num6, 6),
        (egui::Key::Num7, 7),
        (egui::Key::Num8, 8),
        (egui::Key::Num9, 9),
    ];
    KEYS.into_iter()
        .find(|(key, _)| input.key_pressed(*key))
        .map(|(_, digit)| digit)
}

#[cfg(test)]
mod keymap_tests {
    use super::*;

    #[test]
    pub(crate) fn keymap_chords_do_not_collide() {
        let chords: Vec<(usize, KeyChord, KeyGate)> = KEYMAP
            .iter()
            .enumerate()
            .filter_map(|(i, row)| row.chord.map(|c| (i, c, row.gate)))
            .collect();
        for (i, (ia, a, ga)) in chords.iter().enumerate() {
            for (ib, b, gb) in &chords[i + 1..] {
                if !a.conflicts_with(*b) {
                    continue;
                }
                // Same chord may appear with mutually exclusive gates (Note tool vs search N).
                if gates_exclusive(*ga, *gb) {
                    continue;
                }
                // Same action duplicates (PageDown via Space and PageDown key share action
                // but different chords — conflict only when chords compatible).
                let act_a = KEYMAP[*ia].action;
                let act_b = KEYMAP[*ib].action;
                if act_a == act_b {
                    // Identical action+compatible chord is a duplicate row — still a collision.
                }
                panic!(
                    "keymap collision: rows {ia} and {ib} ({:?} vs {:?}, gates {:?}/{:?}, actions {:?}/{:?})",
                    a, b, ga, gb, act_a, act_b
                );
            }
        }
    }

    pub(crate) fn gates_exclusive(a: KeyGate, b: KeyGate) -> bool {
        matches!(
            (a, b),
            (KeyGate::SearchHits, KeyGate::Always)
                | (KeyGate::Always, KeyGate::SearchHits)
                | (KeyGate::SearchHits, KeyGate::HasSelection)
                | (KeyGate::HasSelection, KeyGate::SearchHits)
        )
    }

    #[test]
    pub(crate) fn page_down_and_undo_map_from_expected_chords() {
        let page_down = KEYMAP.iter().find(|row| {
            row.chord == Some(KeyChord::key(Key::PageDown)) && row.action == Some(Action::PageDown)
        });
        assert!(page_down.is_some(), "PageDown → PageDown action");

        let space = KEYMAP.iter().find(|row| {
            row.chord == Some(KeyChord::key(Key::Space)) && row.action == Some(Action::PageDown)
        });
        assert!(space.is_some(), "Space → PageDown action");

        let undo = KEYMAP.iter().find(|row| {
            row.chord == Some(KeyChord::cmd_exact(Key::Z, false))
                && row.action == Some(Action::Undo)
        });
        assert!(undo.is_some(), "Ctrl+Z → Undo");
    }

    #[test]
    pub(crate) fn shortcuts_cover_every_section() {
        for section in ShortcutSection::ALL {
            assert!(
                SHORTCUTS.iter().any(|row| row.section == section),
                "missing shortcuts for {:?}",
                section
            );
            assert!(
                KEYMAP.iter().any(|row| row.section == section),
                "missing KEYMAP section {:?}",
                section
            );
        }
        assert!(SHORTCUTS.iter().any(|row| row.keys.contains('?')));
        assert!(SHORTCUTS.iter().any(|row| row.keys.contains("Space")));
        assert!(SHORTCUTS.iter().any(|row| row.keys == "U"));
        assert!(SHORTCUTS.iter().any(|row| row.keys == "X"));
        assert!(SHORTCUTS.iter().any(|row| row.keys == "W"));
        assert!(SHORTCUTS.iter().any(|row| row.keys == "N"));
        assert!(SHORTCUTS
            .iter()
            .any(|row| row.keys == "Ctrl+Shift+Backspace" && row.action.contains("Delete")));
    }

    #[test]
    pub(crate) fn shortcut_labels_are_subset_of_keymap_labels() {
        for entry in SHORTCUTS {
            assert!(
                KEYMAP.iter().any(|row| {
                    row.section == entry.section
                        && (row.keys == entry.keys || row.label == entry.action)
                }),
                "SHORTCUTS row {:?} / {:?} missing from KEYMAP",
                entry.keys,
                entry.action
            );
        }
    }
}
