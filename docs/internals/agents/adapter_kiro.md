# Kiro adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. The raw upstream protocol is in [kiro-reference.md](../../externals/agent-adapter/kiro-reference.md).

Kiro support targets the stock v3 engine selected by `kiro-cli chat --v3`, verified on Kiro CLI 2.21.4. RimZ owns launch, exact resume, process identity, the managed global hook file, validated local-session discovery, transcript history, assistant streaming, and transient live state. Provider files are pulled display truth: the adapter and snapshot fold never append them to the RimZ event log.

Only the registry reaches the concrete adapter. Installation and the local session store stay private to the Kiro adapter; other modules consume the provider-neutral capabilities.

Since Kiro CLI 2.13.0, global hooks run in every workspace, so session, turn, and tool lifecycle arrive as native hooks. The validated local store still supplies what no hook reports: the newborn card before the first prompt, pending tool approvals, cancelled and failed turn outcomes, context percentage, and history.

## Launch and resume

Fresh sessions run `kiro-cli chat --v3`, with a launch prompt as a bare trailing positional: the v3 TUI parser reads `--` as an unknown flag that swallows the next token, so no argv form passes a prompt starting with `-`. Profiles map `model` and `effort` to chat-level flags. Exact resume runs `kiro-cli chat --v3 --resume-id <session_id>`; both split and joined flag forms are recognized on the launcher and `kiro-cli-chat` engine process. `kiro-cli-term` remains excluded because it is the shell-integration daemon.

Fresh session binding starts from a live pane's effective Kiro kind and exact absolute cwd. Direct `kiro-cli-chat` commands identify the v3 engine alongside launcher commands, while `kiro-cli-term` and shared runtimes stay excluded. RimZ hashes the cwd into Kiro's 16-hex workspace bucket, validates provider metadata, then pairs exact resume IDs first. For fresh sessions, validated `createdAt` authorizes same-cwd binding: a recordless session requires a compatible pane process start, and fresh sessions pair newest-first to the newest uniquely compatible process. Missing process evidence for an empty session and indistinguishable candidates remain process rows.

A provider session replacing a provisional launch row inherits launch-owned name, profile, permission mode, role, team, cohort, channel, description, model and effort fallbacks, worktree metadata, and budget. Provider sessions are transient in the snapshot; a dead pane removes the card while history remains on disk.

## Hooks and lifecycle

`rimz hooks install kiro` writes `~/.kiro/hooks/rimz.json` whole, from the embedded [`hooks.json`](../../../crates/rimz/src/agents/adapters/kiro/hooks.json), through the shared managed source. The file carries the first-line `_rimz_managed` marker, which Kiro's non-strict hook schema accepts; an unmarked file is refused unless every command in it is a RimZ feed, which is the file earlier RimZ builds wrote and install reclaims. Kiro reads global hooks and sessions from the OS home (`os.homedir()`), never from `KIRO_HOME`; `RIMZ_KIRO_HOOKS` overrides the path for tests.

Install and preview refuse when `kiro-cli --version` reports a release older than 2.13.0, where `~/.kiro/hooks` ran only in the home workspace; an unprobeable binary installs. Each command is `rimz hooks feed --source kiro || true` with a 10-second timeout, because a `Stop` hook exiting 1 continues the turn. Neutral output is empty stdout and exit 0 on every trigger.

| Native event | `LifecycleSignal` | Notes |
| --- | --- | --- |
| `SessionStart` | `Registered` | Fires at the first prompt, not at launch, so the pre-prompt card still binds from the store. |
| `UserPromptSubmit` | `TurnStarted` | Carries the sanitized prompt as task and prompt metadata. |
| `PostToolUse` | `ToolUsed { mutates, edits }` | Fires after an approved tool completes. |
| `Stop` | `TurnEnded { errored: false, parked_on_background: false }` | Runs for `end_turn`, `max_tokens`, and `refusal`, before `turn_end` is written; the final message is the session's closing `Say` record, already on disk when the hook runs. |

Every payload carries `session_id` (the `sess_<uuid>` store directory name), `hook_event_name`, and `cwd`; the hook process inherits the pane environment. The engine skips `Stop` when a model error or a cancel throws out of the turn graph, and default subagents run with hooks skipped. `/compact` fires no hook and keeps the session id; the next prompt's `UserPromptSubmit` re-arms the compaction guard.

## Context and transcript

### Local store validation

The stock layout is `~/.kiro/sessions/<sha256(cwd)[0..16]>/<sess_uuid>/{session.json,messages.jsonl}`. Discovery inspects direct `sess_*` children of the requested workspace bucket only. Exact live/conversation lookup validates the requested session candidate across sorted immediate workspace buckets without enumerating sibling sessions, while fleet spending discovery excludes Kiro because it has no spend parser.

The producer batches exact workspaces and retains each requested bucket plus the selected valid sessions' directory, metadata, and message stamps. An unchanged tick reuses the normalized observation; a changed selected dependency re-runs validation and the bounded tail fold for that session, while invalidation reconciles the bucket so another valid child can fill the existing 128-valid-session window. Bucket topology, exact inputs, or the 30-second backstop rescans with validation before the cap; full-history transcript helpers remain uncached.

A session is accepted when the directory and paired files are regular, non-symlink entries under the bucket, the ID is `sess_<uuid>` and matches metadata, `schemaVersion` is `1.0.0`, `dataModelVersion` is `1`, `workspacePaths` contains the requested absolute cwd, and `createdAt` parses. A `status` of `in_progress` means running, `failed` means failed, and anything else, missing included, means idle; and an empty regular `messages.jsonl` produces an immediate pre-prompt idle card once process identity binds. ACP UUID directories, v2/readline history, mismatched metadata, unsupported schema, and symlink escapes stay excluded.

### The transcript fold

The adapter walks complete JSONL records in physical order and ignores malformed or unknown complete records. Cursor reads retain a torn final record until it becomes complete. This validated fold is lifecycle-authoritative for status, phase, prompt, native wait, context percentage, and provider activity clocks; when merged over an exact durable row, it must be at least as current as durable `last_activity` at second precision.

- Non-empty `user.content` becomes user transcript text.
- Non-empty assistant `content` becomes assistant text only when `operationType` is `Say`.
- Late `session_start`, steering, tools, metadata, usage summaries, and internal context never enter conversation history.
- `turn_start` enters running/reasoning.
- Verified approved tool calls and successful results refresh work; observed `fs_write` enters acting/editing.
- An unresolved `pending_interaction` with `interactionType: tool_approval` enters waiting. Matching `interaction_resolved` clears it. This pane-only native wait is visible lifecycle truth, not a routable RimZ ask.
- A successful `session_pause` settles the turn. `turn_end.stopReason` closes it: `cancelled` returns to idle, `refusal` and `error` fail, and any other reason succeeds.
- Tool calls whose status is `approved`, `executing`, or `completed` (the 2.21.4 value) count as running work.
- The latest finite `contextUsage.usagePercentage` is rounded and clamped to `0..=100`.

## Account and balance

Kiro exposes no account probe, so RimZ reports no account panel or rate-limit windows.

## Cost

Kiro usage summaries report credits, not tokens or dollars. RimZ does not infer an active model, tokens, a context-window denominator, dollars, realtime cost, or historical/account spend from credits; a provisional RimZ launch may retain model and effort values it already owns. Kiro has no spend parser and is excluded from fleet spending discovery.

## Known gaps

Run `rimz coverage` for the current wired/partial/unsupported matrix. The gaps below are the ones with a reason worth recording.

- **Errored and cancelled turns fire no hook.** The card settles once the store records `turn_end`, but a supervised run, `message --wait`, or a scheduled turn waiting on that turn ends at its deadline. A refused turn fires `Stop`, so its hook outcome is a success while the card folds to failed.
- **Kiro CLI older than 2.13.0 gets no hooks.** `rimz hooks install kiro` refuses there, detected installs skip Kiro, and supervised runs keep refusing because hooks are not installed.
- **Pending approvals and questions have no hook.** The waiting card comes from the store and stays out of `rimz asks`.
- **Credits are not dollars.** Until Kiro publishes token counts or a dollar figure, no spend parser can be honest.
- **Native Ask/Answer routing, plan and question handling, compaction events, subagents, background parking, remote control, and account probing** have no available surface.
