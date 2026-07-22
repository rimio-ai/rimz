# Cursor adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Accounts, balances, and spend are in [providers.md](./providers.md); the raw upstream protocol is in [cursor-reference.md](../../externals/agent-adapter/cursor-reference.md).

Cursor runs as `agent` or its `cursor-agent` alias; `cursor` names the IDE and is intentionally outside binary discovery. RimZ installs additive user hooks in `~/.cursor/hooks.json` and a canonical command statusline in `~/.cursor/cli-config.json`, launches a verified resolved path or the provider-unique `cursor-agent` alias, and keys every session on `conversation_id`. User hooks run from `~/.cursor`, so ingress accepts a nonempty absolute `CURSOR_PROJECT_DIR` as the participant start path before the shared verified-pin and `WorkspaceResolver` flow; an absent, empty, or relative value falls back to `.`.

## Hooks and lifecycle

| Native event | Normalized signal | Fields |
| --- | --- | --- |
| `sessionStart` | `Registered` | `conversation_id`, model, transcript path |
| `beforeSubmitPrompt` | `TurnStarted` | sanitized `prompt` |
| `postToolUse` for `Shell`/`Write`/`Delete` | `ToolUsed { mutates: true, edits }` | `Write` and `Delete` edit; `Shell` only mutates |
| `postToolUseFailure` | — | activity heartbeat only |
| `afterAgentResponse` | — | safe final visible assistant text; content only |
| `stop` | `TurnEnded` or `TurnInterrupted` | `completed` is clean, `error` fails, and `aborted` lands idle |
| `subagentStart` | `SubagentStarted` | exact child/parent IDs, type, task, child model, branch |
| `subagentStop` | `SubagentStopped` | exact child/parent IDs, status, task fallback, child transcript |
| `preCompact` | `Compacting` | context percentage and window |
| `sessionEnd` | `Ended` | stamps `ended_at`; runtime hides the retained resumable row |

Live verification against Cursor CLI `2026.07.09-a3815c0` shows that `/clear` fires no hook: the next `beforeSubmitPrompt` is the first event carrying the new `conversation_id`, in the same process and pane. Cursor therefore uses the shared `FollowLatest` same-pane policy to collapse each superseded root conversation. On process exit, `sessionEnd` still names the conversation that the process started with; later conversations without their own end hook leave through same-process supersession or dead-process reaping.

Cursor exposes no post-compaction hook. The next lifecycle signal closes the open bracket in the shared `step` state machine, and the projection expires the compaction head after its display window.

**Neutral output.** Every installed event returns Cursor's documented-safe neutral `{}` JSON. The Claude adapter drops payloads carrying `cursor_version`, preventing Cursor's optional Claude-compatible hook loading from double-recording one event.

**Install.** Cursor installation writes the hook and CLI configuration as one transaction. RimZ builds both JSON candidates before writing, writes each by temp-file plus rename, rolls the hook file back byte-for-byte if the statusline write fails, and reports both diffs at consent. Statusline ownership comes from the exact `rimz statusline feed --source cursor` command because Cursor re-serializes `cli-config.json` from its typed model and discards unknown private fields. The marker-free canonical object retains `padding`, `updateIntervalMs`, and `timeoutMs`, so Cursor's rewrite is byte-idempotent and keeps detection installed.

When installation displaces a user statusline, RimZ first stores its exact JSON value in `$XDG_CONFIG_HOME/rimz/cursor-statusline.json` through a durable temp-file-plus-rename write and surfaces that sidecar as a third consent artifact. Statusline forwarding reads the saved command from this RimZ-owned file, and uninstall restores the saved value before deleting the sidecar. Existing inline `_rimz_wrapped` state migrates on reinstall and remains an uninstall fallback. Incomplete-hook detection requires both the canonical hook set and the canonical statusline command, so `rimz hooks install cursor` repairs either half while preserving user-owned and unknown entries.

### Subagents come from the chats store

Cursor CLI `2026.07.09-a3815c0` defines `subagentStart` and `subagentStop` requests with exact `subagent_id` and `parent_conversation_id` fields, and RimZ retains that native mapping and its installed hook entries. Live verification shows that this build never issues either request: the shipped `PreparedTaskSubagent.configured_steps` stays empty, so the child harness receives no hook steps. The native mapper still requires distinct trimmed IDs, uses `subagent_type` for the child name and role, sanitizes the task, and attaches only the stop-side child transcript, so a future Cursor build that starts firing the hooks folds onto the same child rows.

The working lifecycle source is Cursor's version-pinned chats store. After a root Cursor hook lands, RimZ scans the bounded newest directories in `~/.cursor/chats/<md5(workspace)>`; a child has a regular non-symlink `store.db`, no `meta.json`, a safe directory ID matching `meta['0'].agentId`, SQLite user version 1, and a distinct nonempty `subagentInfo.parentAgentId`. The optional `typeName` becomes the child name and role, `createdAt` orders observations, and a parent join admits only children whose exact parent already exists as a Cursor row in this workspace. Per-child mismatches and reader failures abstain without failing the parent hook.

The exact child transcript under `~/.cursor/projects/*/agent-transcripts/<child>/<child>.jsonl` supplies a sanitized task from the first `<user_query>` block and the stop certificate from its trailing row. A missing transcript or one without `turn_ended` leaves the child running; `success`, `completed`, and `aborted` close cleanly, while error, unknown, or malformed terminal rows fail closed. RimZ emits start before stop, includes the transcript path only on stop, and deduplicates both facts against the current rollup before append. Native and derived paths share the same enrichment and store append path, so the child inherits the parent's owner and pane binding. Detection runs when the next root Cursor hook feeds, often only at the parent's turn end; a child has no instant start/stop signal while the upstream hook requests remain dormant.

### Waits come from the local store too

Cursor's local hooks expose no permission request, plan approval, question, or idle notification. The official `AskQuestion` tool writes its synchronous pending call to the local chat root while the native prompt is open, and a completed `CreatePlan` call leaves the native **Ready to build?** approval prompt open after the plan turn ends. RimZ pulls both states as transient display truth, raises the exact hook-bound card to `Waiting`, and retains the pane as the answer surface. It emits no `AwaitingInput` hook event, durable ask, or synthetic answer operation; `rimz asks` remains empty, and disappearance of the pending call or a later conversation message restores the durable hook lifecycle on the next pull.

The wait and child readers are pinned to Cursor CLI `2026.07.09-a3815c0` and share workspace hashing, bounded directory admission, read-only SQLite with no busy wait, user-version checking, and hex JSON decoding from `meta['0']`. For each admitted absolute UTF-8 pane workspace the root wait arm requires schema-1 `meta.json` with matching `cwd`, ordered timestamps, and `hasConversation`; symlinks, subagent chats, mismatched paths, and malformed files fail closed. It verifies the content-addressed SHA-256 root blob and uses `prost` to read repeated protobuf field 4 for pending calls and field 1 for ordered conversation-message IDs.

Exactly one synchronous `AskQuestion` with a stable call ID, start timestamp, and sanitized nonempty question becomes a local observation. With no pending calls, plan approval requires store mode `plan`, a nonempty `currentPlanUri`, and a last conversation message containing exactly one `CreatePlan` tool result; that message read is size-bounded and SHA-256-verified before the fixed `Ready to build?` detail enters RimZ state. A later message self-clears the plan wait. Dismissing the prompt with Esc or `p` writes no store change, so the card remains `Waiting` until the next turn. Async, ambiguous, oversized, hash-invalid, or schema-drifted state produces no wait.

Root wait discovery caches both validated waits and validated absence for the newest 32 chat directories per workspace. The dependency bundle covers `meta.json`, the main database, WAL and rollback journal, and the uniquely resolved public transcript while excluding `store.db-shm` coordination churn; pre/post stamps bracket each read-only SQLite open. A full scan enumerates Cursor project roots once to resolve every selected session ID, and transcript topology, chat topology, exact inputs, or the 30-second backstop rebuilds that index. Subagent-chat derivation remains an uncached hook-triggered read.

The normalized observation retains only the session ID, workspace, sanitized first question or fixed plan-approval detail, provider timestamps, and the public Cursor JSONL path. Raw pending JSON, reasoning, assistant text, option labels, arguments, provider blobs, plan paths, and `store.db` paths never enter RimZ state. `fresh_binding_at = None` prevents disk history from inventing a card.

## Launch and resume

RimZ maps Ask to Cursor's default launch, Plan to `--mode=plan`, Auto to `--auto-review`, and Yolo to `--force --sandbox disabled`. Auto-review lets allowlisted and sandboxable calls proceed and sends the remainder through Cursor's classifier; Yolo explicitly selects the unrestricted posture. `/summarize` is the manual compaction command, `--resume <conversation_id>` resumes, and no CLI-by-id fork surface is declared.

Fresh and supervised launches remain ordinary interactive positional argv: the launcher opens the real interactive CLI in a pane and supplies the initial prompt after `--`, and does not pass `-p` or `--print`. Cursor's native headless `--print` transport is separate and outside the hook-driven coverage contract.

Binary discovery accepts `cursor-agent` by its provider-unique name and accepts `agent` only after its zero-padded date-build version banner proves Cursor identity. Basename-only command and process classification abstains on `agent`, so another provider's colliding alias never creates Cursor presence; a Cursor session launched under that name binds to its pane through the first native hook, and source-known liveness retains that binding afterward. The installed Linux binary reports `MainThread` as its kernel `comm`, so hook attribution continues to use the installer-stamped `$PPID` and durable pane/session stamps rather than treating that generic runtime label as Cursor identity.

## Context and transcript

The command statusline is the rich-context authority. Its structured payload supplies the display model, model parameters, agent version, output style, vim mode, context window and fill, and current input/output/cache composition. Before `beforeSubmitPrompt` records the first user prompt, the shared idle-card projection treats the session name as provider presentation text and keeps the compose animation; after that durable prompt evidence exists, the session name participates in the normal description precedence.

RimZ normalizes Cursor's internal `default` model sentinel to `auto`. For an explicit selection, the adapter separates an exact `param_summary` suffix from the base display model, normalizes its recognized reasoning level, and retains qualifiers such as `Fast` and `Thinking` on the model identity; an ambiguous or nonmatching display stays intact. The independent live `context_window_size` is the displayed window and gauge denominator, rather than the summary's nominal selector magnitude. A stock session with no explicit display selection remains `Auto`, and every optional field is parsed independently so one malformed value does not discard the rest.

`preCompact.context_usage_percent` and `context_window_size` remain fallbacks and open the compaction bracket. `context_tokens` is occupancy rather than cumulative usage, so it does not populate `total_tokens`. Interactive `stop.input_tokens` includes cache-read and cache-write tokens; RimZ derives fresh input with saturating subtraction and retains output, cache-read, and cache-write independently. Explicit zeroes remain visible and these per-turn counters never populate cumulative `total_tokens`. A missed-stop transcript recovery restores only terminal state because the JSONL terminal row carries no tokens. Hook `model_id` labels the row when no statusline context is available, with legacy `model` as fallback; the common `model_params` entry named `effort` supplies the displayed effort.

Cursor CLI `2026.07.09-a3815c0` writes one JSONL file at `~/.cursor/projects/<workspace>/agent-transcripts/<conversation_id>/<conversation_id>.jsonl`. An authenticated native resume rewrote that same path as a full conversation snapshot, replacing the prior terminal placement rather than appending a new suffix. For root sessions RimZ stats the file, reads its bounded whole tail, and recovers a missed success, interruption, or error boundary only when a complete recognized `turn_ended` row is the last meaningful record and no torn suffix follows it. A later nonterminal, unknown, malformed, or partial record keeps the active root turn running until a new complete terminal row or full snapshot arrives.

Root transcript handling never models or consumes assistant, thinking, user, tool, or message content; the child derivation reads only the first `<user_query>` block for its task and the trailing terminal discriminator. Resolution prefers the current hook path, then the persisted path, then one unambiguous exact conversation match beneath the immediate project directories. Workspace ownership comes from the shared participant resolver using `CURSOR_PROJECT_DIR` and the verified room pin; `workspace_roots` and `postToolUse.cwd` remain enrichment only.

`afterAgentResponse.text` is Cursor's sole safe final-text source. Hook ingestion appends that trimmed response to RimZ's durable transcript and seeds an active supervised run without ending it; the later `stop` remains the delivery checkpoint and terminal status transition.

## Account and balance

The producer probes the resolved Cursor CLI with `status --format json`, then calls `about --format json` only after status positively establishes authentication. An explicit logged-out fact is authoritative; contradictory auth fields, unknown auth states, malformed JSON, command failures, and mismatched status/about emails return the retryable unavailable arm. The probe reads no credential file, token, browser state, SQLite database, or web API.

A found account carries the reconciled email, raw `subscriptionTier`, and `cliVersion`. A tier marks metering as known; a tierless authenticated account leaves metering unknown rather than guessing API-key or subscription semantics. Cursor account identity can therefore keep an idle provider block visible, while quota windows, paid usage, historical spend, and provider-account day caps remain unsupported.

## Cost

The `afterAgentResponse` and `stop` hooks repeat per-turn token composition but expose no dollars. RimZ calculates the same API-equivalent local price from either event: a response needs no status because the event certifies a completed response, while a stop must report `completed`, `aborted`, or `error`. Cursor Auto uses `$1.25/M` input and cache-create, `$6.00/M` output, and `$0.25/M` cache-read, while explicit model IDs use the shared price book and its fast-variant multiplier. Unknown models and incomplete pricing stay absent rather than publishing a known zero.

The agent-context sidecar stores the last priced generation and cumulative locally priced total under a per-session lock. Response-to-stop duplication is ignored by generation ID, later generations add exactly once, and statusline refreshes preserve the total. `stop` remains the sole lifecycle boundary and per-turn token projection. The plain-dollar session value drives the live card, cockpit add-back, and live agent/room budgets, then resets when that live session sidecar ends.

## Known gaps

Run `rimz coverage` for the current wired/partial/unsupported matrix. The gaps below are the ones with a reason worth recording.

- **Account usage and historical spend.** Defensive status/about identity probing covers explicit auth facts, while the unverified expired, API-key, service-account, proxy, and server-error arms remain retryable rather than authoritative. Account billing, quota windows, historical spend, and account-day caps are blocked on a machine-readable per-user usage feed Cursor does not ship. The only machine-readable usage feed available is the team-scoped Admin API behind an admin token, which is not a stock per-user CLI credential.
- **Full native history and streaming.** Safe final responses reach RimZ's transcript and supervised output, but native assistant-history replay and incremental reply streaming remain empty because Cursor's JSONL merges visible assistant commentary with model thinking into indistinguishable text blocks.
- **Native headless transport.** Live `-p` probes complete their requested work without firing any configured hooks. RimZ does not parse the native result or claim hook coverage for this transport.
- **Live verification** remains required for interactive hook command shell and `$PPID` semantics on each platform, the `--` prompt terminator, neutral `{}` under every approval mode, `conversation_id` stability across resume, Claude-compatible third-party-hook cross-fire, and the runtime interaction of the permission flags with neutral `{}` output.
