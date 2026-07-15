# Kiro adapter

> The agent-agnostic boundary and state machine are in [model.md](./model.md). The pinned upstream surface and record evidence are in [kiro-reference.md](../../externals/agent-adapter/kiro-reference.md).

Kiro support targets the stock v3 engine selected by `kiro-cli chat --v3`. RimZ owns launch, exact resume, process identity, validated local-session discovery, transcript history, assistant streaming, and transient live state. Provider files are pulled display truth: the adapter and snapshot fold never append them to the RimZ event log.

## Launch, resume, and presence

Fresh sessions run `kiro-cli chat --v3`. Profiles map `model` and `effort` to chat-level flags. Exact resume runs `kiro-cli chat --v3 --resume-id <session_id>`; both split and joined flag forms are recognized on the launcher and `kiro-cli-chat` engine process. `kiro-cli-term` remains excluded because it is the shell-integration daemon.

Fresh session binding starts from a live pane's effective Kiro kind and exact absolute cwd. Direct `kiro-cli-chat` commands identify the v3 engine alongside launcher commands, while `kiro-cli-term` and shared runtimes stay excluded. RimZ hashes the cwd into Kiro's 16-hex workspace bucket, validates provider metadata, then pairs exact resume IDs first. For fresh sessions, validated `createdAt` authorizes same-cwd binding: a recordless session requires a compatible pane process start, and fresh sessions pair newest-first to the newest uniquely compatible process. Missing process evidence for an empty session and indistinguishable candidates remain process rows.

A provider session replacing a provisional launch row inherits launch-owned name, profile, permission mode, role, team, cohort, channel, description, model and effort fallbacks, worktree metadata, and budget. Provider sessions are transient in the snapshot; a dead pane removes the card while history remains on disk.

## Local store validation

The stock layout is `${KIRO_HOME:-~/.kiro}/sessions/<sha256(cwd)[0..16]>/<sess_uuid>/{session.json,messages.jsonl}`. Discovery inspects direct `sess_*` children of the requested workspace bucket only.

A session is accepted when the directory and paired files are regular, non-symlink entries under the bucket, the ID is `sess_<uuid>` and matches metadata, `schemaVersion` is `1.0.0`, `dataModelVersion` is `1`, `workspacePaths` contains the requested absolute cwd, and `createdAt` parses. Missing or null `status` means idle, and an empty regular `messages.jsonl` produces an immediate pre-prompt idle card once process identity binds. ACP UUID directories, v2/readline history, mismatched metadata, unsupported schema, and symlink escapes stay excluded.

## Transcript, lifecycle, and context

The adapter walks complete JSONL records in physical order and ignores malformed or unknown complete records. Cursor reads retain a torn final record until it becomes complete. This validated fold is lifecycle-authoritative for status, phase, prompt, native wait, context percentage, and provider activity clocks; when merged over an exact durable row, it must be at least as current as durable `last_activity` at second precision.

- Non-empty `user.content` becomes user transcript text.
- Non-empty assistant `content` becomes assistant text only when `operationType` is `Say`.
- Late `session_start`, steering, tools, metadata, usage summaries, and internal context never enter conversation history.
- `turn_start` enters running/reasoning.
- Verified approved tool calls and successful results refresh work; observed `fs_write` enters acting/editing.
- An unresolved `pending_interaction` with `interactionType: tool_approval` enters waiting. Matching `interaction_resolved` clears it. This pane-only native wait is visible lifecycle truth, not a routable RimZ ask.
- Successful `session_pause` and `turn_end` settle the turn. Uncaptured failure and cancellation shapes remain unknown.
- The latest finite `contextUsage.usagePercentage` is rounded and clamped to `0..=100`.

Kiro usage summaries report credits. RimZ does not infer an active model, tokens, a context-window denominator, dollars, realtime cost, or historical/account spend from credits; a provisional RimZ launch may retain model and effort values it already owns.

## Hooks and supervised runs

Kiro CLI 2.12.1 did not execute the documented standalone hook configurations in authenticated stock-v3 verification. The adapter classifies manually fed Kiro events as unknown, installs no hooks, and retains uninstall-only cleanup for legacy RimZ-owned hook files.

`rimz agents kiro -p` fails before pane or run-record creation. Pulled files can describe a turn after the fact but cannot provide the executable completion, cancellation, and output contract a supervised run requires. Native Ask/Answer routing, plan/question handling, compaction events, subagents, background parking, remote control, and account probing remain unsupported.
