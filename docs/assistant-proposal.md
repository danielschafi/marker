# Learning Assistant Panel — Design Proposal

Status: proposal only  
Tracks: GitHub issue #12

## Summary

Add a docked, right-side learning assistant to Marker. The panel is closed on startup and uses the user's installed Cursor Agent CLI for all AI interaction. A user can explicitly attach extracted PDF text or a raster crop of a page region, then ask Cursor to explain, summarize, or answer a question about that material.

The integration must not call OpenAI, Anthropic, or another generic model API. Marker delegates authentication, model selection, chat storage, and inference to Cursor.

## Goals

- Provide a VS Code-like assistant panel docked on the right and closed by default.
- Let the user ask Cursor about an open PDF without leaving Marker.
- Attach an explicit text selection, a page-region screenshot, or both.
- Preserve context across turns in one Cursor chat per open Marker tab.
- Stream responses without blocking PDF rendering or input.
- Make every transfer of document content visible and user-controlled.
- Fit the existing ownership boundaries: application state and workers in `app.rs`, chrome in `ui.rs`, and page interaction/coordinate conversion in `view.rs`.

## Non-goals

- Supporting generic LLM providers or direct provider APIs.
- Reimplementing Cursor authentication, model selection, tool execution, or chat storage.
- Uploading or indexing the entire PDF automatically.
- OCR in the first release. Image-only PDFs can still contribute screenshots.
- Editing the PDF or annotations from assistant responses.
- Giving the Cursor process unrestricted access to the PDF's directory or Marker workspace.
- Building a full browser inside Marker. “Look up in browser” only opens the configured system browser after an explicit action.

## UX

### Panel behavior

- Add an **Assistant** toggle to the tab bar and a keyboard shortcut.
- The panel is closed on every app launch. Opening it creates no Cursor chat and sends no data until the first message is submitted.
- Use an `egui::SidePanel::right` so the document viewport shrinks rather than being covered. Proposed default width is 360 px, resizable from 300 px to the smaller of 640 px or 45% of the window.
- Entering Zen mode hides the panel without discarding its state.
- Conversation state belongs to the active PDF tab. Switching tabs switches conversations and pending attachments, preventing accidental cross-document context.
- The panel header shows **Cursor**, connection state, **New chat**, and close controls.
- The body shows user/assistant turns, streaming state, errors, and a stop action.
- The composer uses Enter to send and Shift+Enter for a newline. Attachments appear as removable chips above it:
  - `Text · 347 characters`
  - `Image · page 12` with a thumbnail
  - optional `Page 12 · filename.pdf` metadata
- Attachments are snapshots. Later scrolling, zooming, selecting, or switching tools does not silently change a queued attachment.

### Text attachment

Marker needs an ephemeral learning selection separate from annotations and undo history. It is represented by page index plus glyph range or PDF-space rectangles and is painted as a temporary selection overlay.

Text can be attached in three ways:

1. Select text in learning-selection mode, then choose **Attach selected text**.
2. Right-click the selection and choose **Attach text to Assistant** or **Explain with Cursor**.
3. Select an existing highlight annotation; Marker derives text from glyphs intersecting its quads.

Extracted text is shown in a preview before sending. If a PDF has no text layer, the action explains that no text is available and offers a screenshot attachment instead. Attaching text does not create or modify a PDF annotation.

### Screenshot attachment

**Attach screenshot region** enters a modal capture state:

1. The pointer becomes a crosshair.
2. The user drags one rectangle within one PDF page.
3. Marker stores the rectangle in PDF coordinates, not screen coordinates.
4. A thumbnail chip is added to the composer.
5. Escape cancels without creating a file or changing the document.

The crop is rendered from the PDF page rather than captured from the desktop. This avoids including Marker chrome, the pointer, another window, partially loaded tiles, or zoom-dependent pixels. MVP crops contain page contents only; whether to composite unsaved Marker annotations is an open question.

For an existing text selection or highlight, **Attach screenshot of selection** uses the union of its rectangles plus a small padding.

## Shortcuts and context menu hooks

Proposed defaults, subject to platform testing:

| Action | Shortcut |
| --- | --- |
| Toggle/focus Assistant | Ctrl/Command+Alt+I |
| Attach selected text | Ctrl/Command+Shift+A |
| Capture screenshot region | Ctrl/Command+Alt+S |
| Explain current selection with Cursor | Ctrl/Command+Alt+E |
| Stop current response | Ctrl/Command+. |

Existing Marker shortcuts take precedence while an inline annotation editor or the assistant composer owns keyboard focus. Escape first cancels region capture, then closes transient menus/selections; it does not discard a conversation.

Extend the page context menu with:

- Attach selected text to Assistant
- Attach screenshot of selection
- Explain with Cursor
- Look up selected text in browser

**Explain with Cursor** opens the panel, attaches the current selection, pre-fills a concise learning prompt, and sends only after the user confirms in the composer for the MVP. The browser action opens an encoded query with the system browser and is explicitly separate from Cursor; it never runs automatically.

`view.rs` currently distinguishes annotation right-clicks (style bar) from empty-page right-clicks (context menu). The context state should become a typed snapshot containing the hit annotation, page, PDF point, and optional learning selection. Assistant actions can then be offered for both highlights and ordinary page text without weakening existing style controls.

## Architecture

### Fit with the current egui application

Marker already uses non-UI workers and polls replies from `MarkerApp::update`. The assistant should follow that pattern:

- `app.rs`
  - Own an `AssistantWorker` alongside `PdfWorker` and `MathWorker`.
  - Keep panel visibility at app level, initialized to `false`.
  - Keep conversation, draft, attachment, request sequence, and streaming state per `Tab`.
  - Poll typed assistant events and request repaint while a response is active.
  - Kill a running child and invalidate stale events when a tab closes or a request is stopped.
- `ui.rs`
  - Render the right `SidePanel` before the central viewport.
  - Render the tab-bar toggle, transcript, attachment chips, composer, status, and errors.
  - Keep process and PDF work out of UI closures.
- `view.rs`
  - Own learning-selection gestures, temporary overlays, context-menu hooks, and screen/PDF coordinate conversion.
  - Store selections in PDF coordinates so zoom and panel resizing do not invalidate them.
  - Submit immutable capture requests; do not encode images on the UI thread.
- PDF worker
  - Add bounded crop rendering and text extraction jobs.
  - Reuse MuPDF's display list and structured glyph data. The current engine already renders content-only tiles and extracts accurately bounded glyphs, so this is an extension of existing behavior rather than a desktop screenshot path.

### Cursor CLI adapter

The installed CLI was checked locally:

- `agent --version` reports `2026.09.28-64d2043`.
- `agent --help` exposes `--print`, `--output-format text|json|stream-json`, `--mode ask`, `--resume <chatId>`, `--workspace`, `--sandbox`, and `--trust`.
- `agent create-chat` creates an empty chat and returns its ID.
- `agent status` provides an authentication preflight.
- The help text does not advertise a dedicated image-attachment flag.

Prefer the standalone `agent` executable. Probe it with a short timeout and fall back to a configured `cursor agent` executable only if that probe fails; on this machine the desktop `cursor` command is not a reliable headless probe. Cache capability results for the process lifetime and show an actionable “Cursor Agent CLI not installed / not signed in” state rather than silently falling back to another provider.

On the first send for a tab:

1. Run `agent status`.
2. Run `agent create-chat` and retain the returned chat ID in that tab.
3. Build an isolated context workspace.
4. Start one non-interactive process for the turn and stream its output.

Illustrative invocation:

```text
agent --print --mode ask --output-format stream-json \
  --sandbox enabled --trust --workspace <bundle-dir> \
  --resume <chat-id> "Read request.md and answer the user's question."
```

The exact argument order and stream event schema belong behind a small version-aware adapter. Marker should pass arguments directly with `std::process::Command`, never construct a shell command. The full question and document text live in `request.md`, avoiding shell quoting, process-list disclosure, and command-line length limits.

`--trust` applies only to a Marker-created bundle directory containing the disclosed context. Do not use `--force`/`--yolo`, do not auto-approve MCP servers, and do not expose the PDF's parent directory as the workspace. Ask mode and the enabled sandbox are defense in depth; the generated workspace is the primary access boundary.

One background worker owns child processes, reads stdout and stderr concurrently, parses `stream-json`, and emits events such as started, text delta, completed, cancelled, authentication required, and failed. Every event carries tab generation and request sequence so late output cannot land in a different tab. Cancellation kills the child process group and marks that turn incomplete.

Chat IDs remain in memory for MVP. Starting **New chat** discards the local ID and pending transcript view, then creates another Cursor chat on the next send. Cursor may retain chats under the user's account according to Cursor's own retention policy even when Marker does not persist the ID.

## Context packaging

Each turn gets an isolated directory under `$XDG_RUNTIME_DIR/marker/assistant/` when available, with a private cache-directory fallback. Use owner-only directory/file permissions. A bundle contains only explicitly disclosed material:

```text
request.md
manifest.json
attachments/
  crop-<id>.png
```

`request.md` contains:

- the user's question;
- selected text, delimited and labelled as untrusted PDF content;
- attachment filenames and short descriptions;
- optional document filename and 1-based page number;
- a directive to answer the user's question and not treat document content as instructions.

`manifest.json` records local bookkeeping: schema version, attachment type, tab generation, page index, PDF-space crop rectangle, dimensions, and truncation flags. It is not a copy of the PDF.

### Raster crop

- Render the requested PDF-space rectangle at a fixed target density, initially 144 DPI.
- Clamp to page bounds.
- Preserve aspect ratio while limiting the longest edge to 2048 px and encoded PNG size to approximately 5 MiB.
- Encode off the UI thread using the existing PNG-capable image dependency.
- Do not depend on visible tile cache state.
- Do not copy the full PDF into the bundle.

Because the CLI has no documented image flag, the prompt tells Cursor to inspect the PNG in its isolated workspace. Image capability must be verified in an integration spike against supported CLI versions; inability to inspect the PNG is surfaced as an attachment error, not replaced with a direct model API.

### Extracted text

- Reconstruct reading order from existing structured glyphs.
- Insert spaces on word changes and newlines on line changes.
- Select glyphs by explicit range first, rectangle intersection second.
- Normalize control characters while preserving Unicode and paragraph breaks.
- Show the exact outgoing text in the attachment preview.
- Cap an individual text attachment (proposed: 20,000 characters), visibly mark truncation, and let the user narrow the selection.

### Document metadata

- Include the PDF filename and 1-based page number by default because they orient the answer without granting file access.
- Exclude the absolute path by default. Offer a setting or per-send checkbox to include it.
- Never attach the whole PDF or adjacent local files implicitly.
- If future full-document access is added, it requires a separate, explicit permission flow and a narrower design review.

## Privacy and local-file considerations

- “Cursor only” does not mean local inference. Prompt text and attached crops are sent through the user's Cursor account to Cursor's service. Explain this before the first send and link to Cursor's applicable data policy.
- The previewed attachment chips are the disclosure boundary: content is sent only when the user submits.
- Marker inherits Cursor CLI authentication. It does not collect, store, or print API keys.
- Generated context directories use private permissions, are removed when the chat/tab closes and on normal shutdown, and are cleaned as stale files on later startup after crashes.
- Do not persist transcripts or chat IDs locally in MVP. Avoid writing document text or response bodies to application logs.
- Sanitize generated filenames; never derive writable paths directly from PDF text.
- Treat PDF text and images as untrusted prompt content. Clearly delimit them and instruct Cursor not to execute embedded instructions.
- Use ask mode, sandboxing, no force flag, no automatic MCP approval, and a generated workspace that contains no source tree or PDF parent directory.
- Show the effective Cursor executable and account status in diagnostics, but do not display tokens or sensitive environment variables.
- The browser lookup action is a separate disclosure to the user's configured search engine. Show the query before opening it and never combine it with an assistant send.

## Phased rollout

### Phase 0 — CLI compatibility spike

- Verify `create-chat`/`--resume` behavior and record the supported `stream-json` event schema.
- Verify that Cursor Agent can inspect a PNG placed in the isolated workspace.
- Verify cancellation, authentication failure, offline failure, and a CLI upgrade mismatch.
- Gate the feature with a clear unavailable state if required capabilities are absent.

### Phase 1 — MVP

- Closed-by-default, resizable right panel and tab-bar toggle.
- Per-tab in-memory Cursor chat.
- Plain text transcript with streaming, stop, retry, and visible errors.
- Explicit selected-text attachment and page-region PNG crop.
- Filename/page metadata, first-send disclosure, attachment previews, and size limits.
- Explain-selection shortcut and context-menu actions.
- Isolated workspace, ask mode, sandbox, direct argument spawning, and cleanup.
- Unit tests for glyph-to-text reconstruction, crop bounds, bundle redaction, and stale-event routing; adapter tests use a fake CLI process.

### Phase 2 — Polish

- Markdown/code rendering, copy buttons, better keyboard navigation, and accessibility labels.
- Multiple/reorderable attachments and crop re-selection.
- Optional composition of live Marker annotations into crops.
- Configurable shortcuts and browser search provider.
- Cursor CLI capability/version diagnostics and improved recovery after upgrades.
- Opt-in restoration of per-document chat IDs after a separate retention/privacy decision.
- Better long-selection controls, token/size estimates, and page-context presets.
- OCR as a separately scoped feature if image-only PDFs are common.

## Open questions

1. Should a conversation be per open tab, per canonical PDF path, or explicitly user-created? This proposal recommends per tab for isolation.
2. Should screenshot crops include Marker annotations and unsaved edits, or only original page content? MVP recommends page content only.
3. Is including filename and page number by default acceptable, or should all metadata also require opt-in?
4. Should Explain send immediately or always stop at a preview? MVP recommends confirmation to make disclosure obvious.
5. Does the supported Cursor CLI guarantee PNG inspection from a workspace path, and which `stream-json` fields are stable across upgrades?
6. Should Marker ever persist Cursor chat IDs? If yes, how are moved/renamed PDFs and Cursor-side retention presented?
7. How should text selection coexist with Select-tool panning and Highlight-tool gestures? A dedicated temporary capture mode is least ambiguous but adds one interaction step.
8. Should existing highlight annotations be included as extracted text, screenshot geometry, or both by default?
9. Which shortcuts are conflict-free across Linux, macOS, Windows, desktop environments, and international keyboard layouts?
10. Which search engine should the browser action use, and should it be disabled until explicitly configured?
