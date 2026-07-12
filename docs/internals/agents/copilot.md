# Copilot adapter

> The agent-agnostic boundary and state machine are in [model.md](./model.md); the pinned upstream hook, launch, telemetry, and storage surfaces are in [copilot-reference.md](../../externals/agent-adapter/copilot-reference.md).

GitHub Copilot CLI reports through native camelCase command hooks installed as one RimZ-owned user file at `~/.copilot/hooks/rimz.json`. Each command passes its event name to `rimz hooks feed --source copilot --event <event>` because native payloads carry no event-name field. Hook stdout stays empty, preserving Copilot's own permission engine and question UI.

## Hooks and lifecycle

| Native event | Channel | Normalized signal | Detail |
| --- | --- | --- | --- |
| `sessionStart` | lifecycle | `Registered` | `sessionId`, `cwd`; `startup`/`new` mark fresh identity while `resume` preserves the existing lineage |
| `userPromptSubmitted` | lifecycle | `TurnStarted` | sanitized `prompt` labels the row |
| `preToolUse(ask_user)` | awaiting-user (`Question`) | `AwaitingInput` | best-effort question text from object or JSON-string `toolArgs` |
| `preToolUse` (other tools) | lifecycle | `ToolUsed { false, false }` | proof-of-work for clearing a prior ask; excluded from activity to avoid the ask race |
| `postToolUse` / `postToolUseFailure` | lifecycle | `ToolUsed` for mutating tools | `create`/`edit` mark the acting phase; shell tools mutate without editing |
| `permissionRequest` | awaiting-user (`Permission`) | `AwaitingInput` | Copilot's native UI remains the answer surface |
| `agentStop` | lifecycle | `TurnEnded` | the native final turn boundary |
| `preCompact` | lifecycle | `Compacting` | opens the compaction bracket |
| `errorOccurred` | lifecycle enrichment | — | non-recoverable errors become display-only turn-error markers |
| `sessionEnd` | lifecycle | `Ended` | tombstones the session |

Copilot publishes no post-compaction hook. `preCompact` opens the bracket and the shared state machine closes it on the next lifecycle signal; the display window is the missed-edge backstop. This makes compaction partial rather than fully wired.

`errorOccurred` is a marker rather than a lifecycle end because recoverable model-call retries occur mid-turn. Only `recoverable: false` creates an `AgentTurnError`; `agentStop` remains turn truth and `sessionEnd` remains session truth.

The payload parser accepts both documented `toolArgs` shapes: the hooks reference leaves it as `unknown`, while GitHub's hook tutorial describes a JSON-encoded string. It also keeps the rest of `postToolUseFailure` intact when that event's `error` is a string rather than the object carried by `errorOccurred`.

Subagent hooks are deliberately absent from the installed file. Their payload identifies only an agent definition name, not a child invocation, and the built-in `general-purpose` agent emits neither event, so concurrent children cannot be durably keyed.

Install owns the whole file through its first-line `_rimz_managed` marker. Reinstall reclaims a marked file, install refuses an unmarked file at that path, and uninstall removes only the marked file. Copilot CLI 1.0.70 accepts the unknown top-level marker and discovered the marked file during an access-denied startup probe; if a later release stops doing so, move the marker into the first hook entry's `env` overlay.

## Launch and resume

`copilot` launches a fresh interactive session and `copilot --resume <session_id>` restores one. Ask mode adds no flags, plan uses `--plan`, auto uses `--autopilot`, and yolo uses `--allow-all`; model and effort profiles map to `--model` and `--effort`.

Copilot CLI 1.0.70 exposes `-i, --interactive <prompt>`, which starts the stock interactive UI and automatically executes the prompt. RimZ appends `--interactive <prompt>` after profile and permission arguments for prompt-seeded panes and supervised `rimz agents copilot -p` runs, preserving native asks and the hook-driven completion path. Copilot's native non-interactive `-p` mode remains deferred because adopting its process output would introduce a second supervised-run backend and remove the pane's interactive answer surface. Fork and ping stay unsupported.

## Context and transcript

Command hooks carry no context gauge, model, effort, or documented transcript schema. Context usage and rich context remain unsupported until captured custom-statusline or OTel fixtures establish a typed transport. `events.jsonl` remains opaque until a pinned release fixture proves its schema.

## Account and balance

Copilot publishes no machine-readable authentication or usage-status probe. The adapter reports no account plan, quota windows, or balance rather than inferring them from interactive output.

## Cost

Realtime and historical cost remain unsupported. The hook payloads carry no usage figures, the local event log is undocumented, and OTel is not wired as enrichment. Add cost only after captured OTel or session-store fixtures make attribution and deduplication testable.

## Deferred integration

Copilot CLI 1.0.70 is installed, but an eligible account is unavailable. A temporary `COPILOT_HOME` probe verified hook-file discovery, tolerance of the `_rimz_managed` key, the documented millisecond `sessionEnd` payload, and `--interactive`; the request then stopped at account policy before `sessionStart` and a model turn. Successful-turn hook ordering, the exact `permissionRequest` input, `ask_user` argument shape, resume PID ancestry, and native `-p` output remain live-verification gaps.

Subagent coverage needs a stable child invocation identity. Copilot hooks expose the parent session and agent definition name, so concurrent children with the same definition collide; a future RimZ abstraction may admit provider-scoped synthetic child IDs only if OTel trace identity or another durable upstream key makes replay deterministic.

Context, quota, and cost coverage needs an enrichment transport independent of lifecycle truth. A future adapter can map the published OTel schema through the existing context boundary after file concurrency, flush timing, and cumulative-versus-turn accounting have pinned fixtures; statusline wrapping stays deferred until its input and command-chaining behavior are documented or captured.
