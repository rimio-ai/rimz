# Gemini adapter

> The agent-agnostic boundary and state machine are in [model.md](./model.md); account and spend rules are in [providers.md](./providers.md). The pinned upstream hook, transcript, auth, and CLI surface is in [gemini-reference.md](../../externals/agent-adapter/gemini-reference.md).

Gemini CLI reports through user-global command hooks merged into `~/.gemini/settings.json`. Each hook is a child of the interactive pane and carries the session id, transcript path, and cwd, so registration is eager and pane attribution is direct. The adapter installs the stock CLI's native events and returns Gemini's neutral `{}` result; the agent's own confirmation UI remains the answer surface.

## Hooks and lifecycle

| Native event | Channel | Normalized signal | Notes |
| --- | --- | --- | --- |
| `SessionStart` | lifecycle | `Registered` | `startup` and `clear` mark fresh lineage; `resume` retains the session |
| `BeforeAgent` | lifecycle | `TurnStarted` | sanitized `prompt` labels the row |
| `AfterAgent` | lifecycle | `TurnEnded { errored: false }` | carries the final response; the native payload has no error bit |
| `BeforeTool` (`ask_user`) | awaiting-user | `Question` | structured questions and choices are retained |
| `BeforeTool` (`exit_plan_mode`) | awaiting-user | `PlanApproval` | the native plan dialog remains open in the pane |
| `BeforeTool` (other) | lifecycle | — | classification only; pre-tool work is not activity |
| `AfterTool` | lifecycle | `ToolUsed { mutates, edits }` | every completed tool clears a resolved wait; `write_file` and `replace` edit, while `run_shell_command` mutates only |
| `Notification` | awaiting-user | `Permission` | ordinary native confirmation dialogs; question and plan notifications duplicate the richer `BeforeTool` event and are ignored |
| `PreCompress` | lifecycle | `Compacting` | the next lifecycle signal closes the one-sided bracket |
| `SessionEnd` | lifecycle | `Ended` | best-effort and asynchronous; pane liveness remains the backstop |

Gemini exposes no post-compression event. The shared lifecycle step closes an open compaction bracket on the next signal, so the card cannot pulse forever, but the landing follows that later signal rather than the original `auto` or `manual` trigger. Model hooks remain unwired because `AfterModel` fires for every streaming chunk.

Gemini emits both `BeforeTool` and `Notification` for `ask_user` and `exit_plan_mode`. The adapter records the typed `BeforeTool` payload once because it retains questions, choices, and plan identity; the later notification carries only a title and does not open a duplicate ask. Ordinary edit, shell, MCP, information, sandbox-expansion, and plan-entry confirmations enter waiting from `Notification`. A completed `AfterTool` clears either path immediately.

Gemini's plan input changed across the supported hook surface: stable 0.50 sends `plan_filename`, while the pinned nightly reference names `plan_path`. The tolerant payload parser accepts both and exposes whichever non-empty value is present as the approval detail.

The shared lifecycle coverage descriptor currently names one native event per signal kind. Gemini reaches `AwaitingInput` through two native events—typed questions and plan approval through `BeforeTool`, ordinary permissions through `Notification`—so the descriptor names `Notification` while the adapter and this table preserve the full mapping. A future descriptor shape can accept a native-event set when another adapter also needs multi-event provenance; this adapter does not broaden the shared abstraction alone.

The upstream name `Notification` describes tool-confirmation notifications, not an idle-timeout nudge. Conformance therefore treats that event as a requirement when an adapter declares idle notification fully wired, without inferring full idle coverage merely from the native name. Gemini retains partial idle-notification coverage through turn boundaries, asks, and the shared stall window.

Subagents stay declared off until hook behavior inside child sessions is live-verified. Gemini records child transcripts and parent `invoke_agent` calls, but exposes no dedicated child start/stop event. Native session fork is also absent; resume uses `gemini --resume <id>`, while `/compress` is the manual compaction command.

## Context and transcript

Gemini stores one main JSONL session per project under `~/.gemini/tmp/*/chats/`. The live hook's absolute `transcript_path` wins; historical discovery includes main `.jsonl` files and migrated legacy `.json` records while skipping nested subagent directories.

The fold applies message-id replacement, `$set.messages` checkpoint replacement, and `$rewindTo` pruning before selecting the newest active `type: "gemini"` message. Its `tokens.total` drives context fill, and `model` chooses the window: current Gemini families and unknown routes use 1,048,576 tokens; Gemma uses 256,000. Local refreshes stat-gate a bounded tail read. An unreadable transcript leaves context unknown, while a readable fresh transcript reports explicit zero usage.

## Account and balance

The account probe reads only local non-secret metadata: `security.auth.selectedType` in settings names the method, and `google_accounts.json.active` labels an `oauth-personal` login. API-key, Vertex, environment, and gateway methods retain their auth label without reading credentials.

The Code Assist `retrieveUserQuota` probe is deferred. It depends on an internal API and OAuth material held in OS secure storage, so Gemini's account-spend coverage remains partial and no quota windows are reported in this round.

## Cost

Session records carry token categories but no dollars. The spend fold prices every active Gemini message through the shared price book: uncached input is `input - cached`, cached input uses the cache-read rate, and billable output is `output + thoughts`. Rewinds and checkpoints can invalidate earlier messages, so each changed Gemini file cold-folds and replaces its cached entry set rather than appending a suffix.

These figures are usage insight rather than billing truth. The transcript does not record the session's auth type, so Google-login Code Assist quota traffic and metered API or Vertex traffic receive the same uniform price-book estimate.

## Deferred surfaces and future work

This round lands the interactive-hook lifecycle, the transcript context and spend gauges, the local account-identity probe, and the launch, resume, permission, compaction, and ping surfaces. The gaps below are deliberate deferrals, each with the evidence it waits on. The upstream detail sits in [gemini-reference.md](../../externals/agent-adapter/gemini-reference.md); the live-verification list is that reference's implementation checklist.

- **Turn-failure backstop.** `AfterAgent` fires only on a clean final response, so a provider or API error that bypasses it leaves the row `running` until the shared stall window settles it. No hook carries an error bit. A transcript backstop is the natural fix — a session line is `type: "error"` — routed through the same `LocalContextRefresh::turn_error` channel Codex and Claude use. It waits on a live-captured error record to pin the field names and to prove an `error` line is turn-fatal rather than a recoverable retry, so the current refresh leaves `turn_error`, `turn_complete`, and `turn_interrupted` unset.
- **Subagents.** Gemini records child transcripts and parent `invoke_agent` calls but emits no child start/stop hook, so `subagents` stays off until hook firing inside a child context is live-verified. If child hooks fire, the parent recovers from the nested transcript path; if they do not, a bounded transcript watcher is the fallback, with its latency made explicit.
- **Code Assist quota.** The `retrieveUserQuota` window depends on an internal API and OAuth material in OS secure storage, so `AccountSpend` stays partial and no rate-limit windows are published.
- **Native structured answer.** `ask_user` carries typed questions and choices, retained as the ask detail, but `rimz answer` targets no native Gemini TUI action yet; text and choice answers go through pane send. `Answer` is unsupported.
- **ACP and remote control.** `gemini --acp` owns a fresh stdio session rather than observing a running TUI pane, so it is not an out-of-band read channel for the interactive adapter; remote control stays unsupported.
- **Rich context transport.** Gemini publishes no out-of-band per-session channel, so context and cost ride the transcript tail alone; `rich_context` stays off.
- **Ping model pinning.** `ping_args` is empty, so a `gemini-ping` window primer inherits `auto` routing rather than pinning a cheap model the way Claude and Codex do. Pinning waits on a confirmed stable Flash model id, since an unknown `--model` id would break the primer rather than cheapen it.

## Notes for the rimz abstraction

Footprints for a future round that touches the shared model rather than this adapter alone.

- **Multi-event ask provenance.** The `lifecycle_hooks` matrix names one native event per signal kind, but Gemini reaches `AwaitingInput` through two — typed questions and plan approval on `BeforeTool`, ordinary permissions on `Notification`. The descriptor names `Notification` while this adapter and the hooks table above carry the full mapping. A descriptor shape that accepts a native-event set would let the matrix state both; this adapter does not broaden the shared type alone.
- **Provider-general turn-failure marker.** Wiring the turn-failure backstop above would exercise the `turn_error` merge in [`sidebar/refresh/sessions.rs`](../../../crates/rimz/src/sidebar/refresh/sessions.rs), whose confirmation ladder is Codex-specific today. A Gemini marker would take the general merge path, so that path's success-row and self-clear behaviour wants a conformance case before a second provider depends on it.
