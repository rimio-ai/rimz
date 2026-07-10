# Copilot adapter

> The agent-agnostic boundary and state machine are in [model.md](./model.md); the pinned upstream hook, launch, telemetry, and storage surfaces are in [copilot-reference.md](../../externals/agent-adapter/copilot-reference.md).

GitHub Copilot CLI reports through native camelCase command hooks installed as one RimZ-owned user file at `~/.copilot/hooks/rimz.json`. Each command passes its event name to `rimz hooks feed --source copilot --event <event>` because native payloads carry no event-name field. Hook stdout stays empty, preserving Copilot's own permission engine and question UI.

## Hooks and lifecycle

| Native event | Channel | Normalized signal | Detail |
| --- | --- | --- | --- |
| `sessionStart` | lifecycle | `Registered` | `sessionId`, `cwd`; startup and resume share the edge |
| `userPromptSubmitted` | lifecycle | `TurnStarted` | sanitized `prompt` labels the row |
| `preToolUse(ask_user)` | awaiting-user (`Question`) | `AwaitingInput` | best-effort question text from `toolArgs` |
| `preToolUse` (other tools) | lifecycle | `ToolUsed { false, false }` | proof-of-work for clearing a prior ask; excluded from activity to avoid the ask race |
| `postToolUse` / `postToolUseFailure` | lifecycle | `ToolUsed` for mutating tools | `create`/`edit` mark the acting phase; shell tools mutate without editing |
| `permissionRequest` | awaiting-user (`Permission`) | `AwaitingInput` | Copilot's native UI remains the answer surface |
| `agentStop` | lifecycle | `TurnEnded` | the native final turn boundary |
| `preCompact` | lifecycle | `Compacting` | opens the compaction bracket |
| `errorOccurred` | lifecycle enrichment | — | non-recoverable errors become display-only turn-error markers |
| `sessionEnd` | lifecycle | `Ended` | tombstones the session |

Copilot publishes no post-compaction hook. `preCompact` opens the bracket and the shared state machine closes it on the next lifecycle signal; the display window is the missed-edge backstop. This makes compaction partial rather than fully wired.

`errorOccurred` is a marker rather than a lifecycle end because recoverable model-call retries occur mid-turn. Only `recoverable: false` creates an `AgentTurnError`; `agentStop` remains turn truth and `sessionEnd` remains session truth.

Subagent hooks are deliberately absent from the installed file. Their payload identifies only an agent definition name, not a child invocation, and the built-in `general-purpose` agent emits neither event, so concurrent children cannot be durably keyed.

Install owns the whole file through its first-line `_rimz_managed` marker. Reinstall reclaims a marked file, install refuses an unmarked file at that path, and uninstall removes only the marked file. Live verification must confirm that the supported Copilot release tolerates the unknown top-level marker; if it stops doing so, move the marker into the first hook entry's `env` overlay.

## Launch and resume

`copilot` launches a fresh interactive session and `copilot --resume <session_id>` restores one. Ask mode adds no flags, plan uses `--plan`, auto uses `--autopilot`, and yolo uses `--allow-all`; model and effort profiles map to `--model` and `--effort`.

Copilot exposes no verified interactive initial-prompt flag. A prompt-seeded launch, including `rimz agents copilot -p`, fails preflight instead of dropping the prompt or substituting unverified programmatic-mode behavior. Fork and ping stay unsupported for the same evidence boundary.

## Context and transcript

Command hooks carry no context gauge, model, effort, or documented transcript schema. Context usage and rich context remain unsupported until captured custom-statusline or OTel fixtures establish a typed transport. `events.jsonl` remains opaque until a pinned release fixture proves its schema.

## Account and balance

Copilot publishes no machine-readable authentication or usage-status probe. The adapter reports no account plan, quota windows, or balance rather than inferring them from interactive output.

## Cost

Realtime and historical cost remain unsupported. The hook payloads carry no usage figures, the local event log is undocumented, and OTel is not wired as enrichment. Add cost only after captured OTel or session-store fixtures make attribution and deduplication testable.

Live hook discovery and firing, unknown-top-level-key tolerance, the exact `permissionRequest` input shape, and programmatic `-p` behavior remain unverified on this development machine because Copilot CLI is not installed.
