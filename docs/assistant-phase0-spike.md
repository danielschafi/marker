# Phase 0 — Cursor Agent CLI compatibility spike

Date: 2026-09-29  
Tracks: GitHub issue #12  
Machine: local Linux; Cursor only (no provider APIs)

## Verdict

`agent` create-chat / `--resume`, ask-mode `--print` + `stream-json`, sandbox + trust + isolated workspace, and PNG visual inspection all work on CLI `2026.09.28-64d2043`. Phase 1 MVP is unblocked with a few adapter caveats (process-group kill, optional `--stream-partial-output`, unreliable `cursor agent` fallback, soft failure on bogus chat IDs).

## 1. CLI location and version

| Item | Result |
| --- | --- |
| Preferred binary | `/home/daniel/.local/bin/agent` → `…/cursor-agent/versions/2026.09.28-64d2043/cursor-agent` |
| `agent --version` | `2026.09.28-64d2043` |
| `cursor agent --version` | **Unreliable** — `timeout 3` returned exit **124** (hung). Prefer standalone `agent` only; do not treat desktop `cursor` as a headless probe. |

Relevant flags confirmed via `agent --help`: `--print`, `--output-format text\|json\|stream-json`, `--stream-partial-output`, `--mode ask\|plan`, `--resume [chatId]`, `--workspace`, `--sandbox enabled\|disabled`, `--trust`. Commands: `status` / `whoami`, `create-chat`.

## 2. Auth (`agent status`)

```text
✓ Logged in as <redacted-email>
```

Stream init reported `"apiKeySource":"login"` (no tokens printed). Marker should surface “signed in / not signed in” only — never env keys or cookies.

## 3. `create-chat` / `--resume`

- `agent create-chat` prints a UUID on stdout (e.g. `f36900ce-d1c8-4219-b8dc-978128f4e130`) and exits 0.
- `--resume <chat-id>` binds the turn: every stream event carries matching `session_id`.
- **Works.**

## 4. Isolated workspace invoke

Workspace: `/tmp/marker-assistant-spike-1303921/`

```text
request.md
attachments/sample.png   # 64×64 solid bright red RGBA PNG (~155 bytes)
```

Invocation (as proposed):

```text
agent --print --mode ask --output-format stream-json \
  --sandbox enabled --trust --workspace <dir> \
  --resume <chat-id> "Read request.md and answer."
```

- Exit: 0  
- Duration: ~27s (`result.duration_ms` ≈ 26635)  
- Stderr: empty on success  
- Saved capture: `/tmp/marker-spike-stream.jsonl` (77 NDJSON lines; not committed)

## 5. Observed `stream-json` schema

NDJSON, one JSON object per line. Event types seen in the happy path:

| `type` | `subtype` | Key fields |
| --- | --- | --- |
| `system` | `init` | `apiKeySource`, `cwd`, `session_id`, `model`, `permissionMode` |
| `user` | — | `message.role`, `message.content[]` (`type: text`), `session_id` |
| `thinking` | `delta` | `text` (short chunks), `session_id`, `timestamp_ms` |
| `thinking` | `completed` | `session_id`, `timestamp_ms` |
| `assistant` | — | `message.content[]` with full text chunks (not token deltas unless `--stream-partial-output`), `session_id`, optional `model_call_id`, `timestamp_ms` |
| `tool_call` | `started` / `completed` | `call_id`, `model_call_id`, `session_id`, `tool_call.<kind>` |
| `result` | `success` | `duration_ms`, `duration_api_ms`, `is_error`, `result` (concatenated assistant text), `session_id`, `request_id`, `usage` |

Tool call kinds observed under `tool_call`:

- `readToolCall` — args `path`; success may include text `content` or, for PNG, `dataBlobId` + `fileSize` (no raw pixels in the stream)
- `globToolCall` — args `targetDirectory`, `globPattern`; success `files[]`
- `shellToolCall` — in ask mode often `result.permissionDenied` (`error: "Command blocked by permissions configuration"`)
- `getMcpToolsToolCall` — incidental MCP schema fetch

**Streaming note:** Without `--stream-partial-output`, user-visible assistant text arrives as whole `assistant` messages between tool rounds, not fine-grained deltas. Thinking uses `thinking`/`delta`. For UI token streaming, Marker should pass `--stream-partial-output` and re-validate the schema.

**Sample lines (truncated):**

```json
{"type":"system","subtype":"init","apiKeySource":"login","cwd":"/tmp/marker-assistant-spike-1303921","session_id":"<uuid>","model":"Auto","permissionMode":"default"}
{"type":"thinking","subtype":"delta","text":"Reading request.md to","session_id":"<uuid>","timestamp_ms":…}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"I'll read `request.md` and answer from there."}]},"session_id":"<uuid>",…}
{"type":"tool_call","subtype":"completed","call_id":"…","tool_call":{"readToolCall":{"args":{"path":"…/attachments/sample.png"},"result":{"success":{"fileSize":155,"dataBlobId":"…","totalLines":0}}}},…}
{"type":"result","subtype":"success","is_error":false,"result":"…","usage":{"inputTokens":…,"outputTokens":…,"cacheReadTokens":…,"cacheWriteTokens":0}}
```

## 6. PNG inspection

**Yes.** The model described the sample image correctly after `readToolCall` on `attachments/sample.png` (stream result included `dataBlobId`, indicating image ingest).

Quote:

> The sample image is a solid bright red square. From visual inspection alone I can’t give reliable pixel dimensions (ask mode blocks reading the PNG header).

Caveats for Marker:

- Ask mode blocks shell inspection of PNG headers; color/content still works via the Read/image path.
- `globToolCall` for `**/attachments/sample.png` returned **zero** files in this run; `**/*` listed only `request.md`. Direct `readToolCall` on the known path still succeeded. Prefer absolute paths in `request.md` rather than relying on glob discovery of `attachments/`.

## 7. Cancellation

| Approach | Behavior |
| --- | --- |
| Kill shell wrapper only | Agent child can **orphan** and keep running |
| `setsid` so `agent` is PGID/SID leader, then `kill -TERM -<pgid>` | Process **dies**; stream stops mid-flight (**no** `result` event); no lingering process for that chat |

Marker must spawn with a new process group / session and kill the **agent** group, not only a parent shell. Treat missing terminal `result` after kill as cancelled/incomplete.

## 8. Failure modes (safe)

| Probe | Observation |
| --- | --- |
| Missing binary | `No such file or directory`, exit **127** — map to “CLI not installed” |
| Nonexistent `--workspace` | Stderr: `Error: Workspace directory does not exist: …` — fail closed before send |
| Bogus `--resume` UUID (`00000000-…`) | **Does not fail** — turn runs and succeeds with that `session_id`. Do not rely on CLI rejecting bad IDs; always use IDs from `create-chat` |
| `cursor agent` as fallback probe | Hang / timeout — avoid for capability checks |
| Ask-mode shell | `permissionDenied` in stream — expected; not a Marker crash |

Auth-lost / offline / CLI-upgrade mismatch were not forcibly broken on this machine; gate Phase 1 on `agent status` + version probe and treat non-zero spawn / empty stream / stderr errors as unavailable.

## 9. Implications for Phase 1 MVP

Unblocked:

- Prefer `agent` executable; cache version + `status`.
- Per-tab `create-chat` ID + `--resume`.
- Build private workspace with `request.md` + `attachments/*.png`; `--workspace` + `--trust` + `--sandbox enabled` + `--mode ask`.
- Parse NDJSON `stream-json`; map `assistant` / optional partial deltas / `result` / tool noise / errors.
- Cancel via process-group kill; mark turn incomplete if no `result`.

Adapter caveats / blockers to design around (not show-stoppers):

1. **Spawn model:** new process group; never shell-string commands.
2. **Streaming UX:** enable `--stream-partial-output` or accept chunky `assistant` messages.
3. **PNG:** works via workspace Read; put explicit paths in `request.md`; do not depend on glob of `attachments/`.
4. **Chat ID validation:** CLI accepts arbitrary UUIDs; Marker owns validity.
5. **No `cursor agent` fallback** on this platform unless a future probe proves non-blocking.
6. Schema may drift across CLI upgrades — keep a version-aware parser and capability gate.

## 10. Artifacts (local, not committed)

- Workspace: `/tmp/marker-assistant-spike-1303921/`
- Happy-path stream: `/tmp/marker-spike-stream.jsonl`
- Cancel stream (incomplete): `/tmp/marker-spike-cancel5.jsonl`
