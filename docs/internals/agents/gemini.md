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
