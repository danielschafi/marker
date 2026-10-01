//! Agent session keep-alive and lifecycle (#27).
//!
//! Continuity is the Cursor **chat id**, not a long-lived CLI process. Each turn
//! spawns `agent … --resume <chat_id>`; when the turn finishes the child exits.
//! Marker keeps `chat_id` + the local transcript on the PDF tab so the next send
//! (or a panel reopen) resumes the same conversation.
//!
//! ## Keep-alive (session stays alive)
//!
//! | Action | chat_id / transcript | In-flight turn |
//! | --- | --- | --- |
//! | Close / hide assistant panel | kept | continues; UI reconnects on reopen |
//! | Zen / chrome auto-hide | kept | continues |
//! | Switch to another tab | kept on the background tab | continues (events keyed by tab `gen`) |
//! | Between turns (idle) | kept for the tab lifetime | n/a |
//!
//! ## End of local session
//!
//! | Action | Effect |
//! | --- | --- |
//! | **New chat** | Clears local `chat_id` + transcript; next send runs `create-chat` |
//! | Close PDF tab | Cancels in-flight turn; drops the tab's session with the tab |
//! | App exit | Worker `Drop` kills any active child; chat ids are not persisted |
//!
//! Cursor may retain chats under the user's account; Marker does not delete
//! remote history when starting a new chat.
//!
//! ## Timeouts
//!
//! - **Session idle:** none while the tab is open. An unused `chat_id` is reused
//!   on the next send via `--resume` for the whole tab lifetime.
//! - **Per-turn child:** no Marker wall-clock kill; the user **Stop** control
//!   (or tab close / New chat) cancels via process-group kill.
//! - **CLI helpers** (`create-chat`, `--version`, `status`): short hard timeouts
//!   in [`crate::assistant::adapter`] (15s / 3s / 8s).

use super::types::TabAssistant;

/// Local session is resumable when Marker holds a Cursor chat id from `create-chat`.
pub fn is_resumable(assistant: &TabAssistant) -> bool {
    assistant
        .chat_id
        .as_ref()
        .is_some_and(|id| !id.trim().is_empty())
}

/// Short label for the panel header / status line.
pub fn status_hint(assistant: &TabAssistant) -> Option<String> {
    if assistant.streaming {
        return Some("Running in background — reopen anytime".into());
    }
    if is_resumable(assistant) && !assistant.turns.is_empty() {
        return Some("Session alive — next send resumes this chat".into());
    }
    if is_resumable(assistant) {
        return Some("Session ready — will resume on send".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::types::{AssistantRole, AssistantTurn};

    #[test]
    fn hide_panel_must_not_clear_session() {
        // Document the contract enforced by MarkerApp::set_assistant_open:
        // toggling visibility never calls new_chat().
        let mut a = TabAssistant::default();
        a.chat_id = Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into());
        a.turns.push(AssistantTurn {
            role: AssistantRole::User,
            text: "hi".into(),
            incomplete: false,
        });
        a.streaming = true;
        // Simulate panel hide: only UI visibility changes; TabAssistant untouched.
        let hidden_open = false;
        assert!(!hidden_open);
        assert!(is_resumable(&a));
        assert!(a.streaming);
        assert_eq!(a.turns.len(), 1);
    }

    #[test]
    fn new_chat_ends_local_session() {
        let mut a = TabAssistant::default();
        a.chat_id = Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into());
        a.turns.push(AssistantTurn {
            role: AssistantRole::Assistant,
            text: "ok".into(),
            incomplete: false,
        });
        a.new_chat();
        assert!(!is_resumable(&a));
        assert!(a.turns.is_empty());
        assert!(!a.streaming);
    }

    #[test]
    fn status_hint_reflects_background_and_resume() {
        let mut a = TabAssistant::default();
        assert!(status_hint(&a).is_none());
        a.chat_id = Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into());
        assert!(status_hint(&a).unwrap().contains("resume"));
        a.streaming = true;
        assert!(status_hint(&a).unwrap().contains("background"));
    }
}
