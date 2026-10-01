# Assistant session lifecycle (issue #27)

Marker’s learning assistant talks to the Cursor Agent CLI. Continuity is a
**chat id**, not a long-lived CLI process.

## Model

1. On the first send for a tab, Marker runs `agent create-chat` and stores the
   returned id on that tab.
2. Every turn spawns a short-lived
   `agent --print … --resume <chat_id> …` process, streams `stream-json`, then
   exits.
3. The next send reuses the same id so Cursor keeps conversation context.

Local transcript + `chat_id` live in memory on the PDF tab only (not written to
disk in MVP).

## Keep-alive

These actions **do not** end the session or cancel a running turn:

- Closing / hiding the Assistant panel (× or shortcut)
- Zen mode / chrome auto-hide (panel is not drawn; state is unchanged)
- Switching to another tab (background tabs keep receiving worker events)
- Sitting idle between turns while the tab stays open

Reopening the panel reconnects to the same transcript, draft, attachments, and
any in-flight stream.

## Ending a local session

| Action | Effect |
| --- | --- |
| **New chat** | Clears local `chat_id` and transcript; next send calls `create-chat` |
| Close the PDF tab | Cancels an in-flight turn; drops the session with the tab |
| Quit Marker | Active child is killed; chat ids are not persisted |

Cursor may still retain chats under the user’s account; Marker does not delete
remote history when starting a new chat.

## Timeouts

| Scope | Behavior |
| --- | --- |
| Session idle | **None** while the tab is open — the `chat_id` is reused indefinitely for that tab |
| Per-turn agent child | No Marker wall-clock kill; user **Stop**, **New chat**, or tab close cancels via process-group kill |
| CLI helpers | `create-chat` 15s, `--version` 3s, `status` 8s (see `src/assistant/adapter.rs`) |

Policy is also documented in code at `src/assistant/session.rs`.
