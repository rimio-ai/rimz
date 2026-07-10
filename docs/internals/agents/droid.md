# Droid adapter

> The agent-agnostic boundary and state machine are in [model.md](./model.md). The verified upstream hook, CLI, transcript, and settings surfaces are in [droid-reference.md](../../externals/agent-adapter/droid-reference.md).

Factory Droid reports basic lifecycle through native `settings.json` hooks. RimZ installs a slim additive hook set into `~/.factory/settings.json`; it leaves Droid's user-owned `statusLine` and persistent autonomy settings untouched. The adapter is intentionally interactive-only: supervised `rimz agents droid -p` launches the stock TUI with a positional prompt and completes from the native `Stop` hook rather than using `droid exec` or stream JSON-RPC.

## Hooks and lifecycle

| Native event | `LifecycleSignal` | Notes |
| --- | --- | --- |
| `SessionStart` | `Registered` | `startup` and `clear` mark a fresh session; `resume` registers the replacement session id Droid assigns. |
| `SessionStart` (`source: compact`) | `CompactionEnded { auto: None }` | Closes the bracket because Droid has no `PostCompact` hook. If compaction rotates the session id, the close on the unseen id folds away and the replacement registers on its next prompt or tool signal. |
| `UserPromptSubmit` | `TurnStarted` | Carries the sanitized prompt as task and prompt metadata. |
| `PostToolUse` | `ToolUsed { mutates, edits }` | `Create`, `Edit`, and `ApplyPatch` edit; `Execute` mutates without ending the reasoning phase. |
| `Stop` | `TurnEnded { errored: false, parked_on_background: false }` | Droid exposes no structured failure or background-work field. The displayed-status ladder and stall window surface failures without guessing from silence. |
| `PreCompact` | `Compacting` | Opens the compaction bracket; `trigger` is retained by the typed wire but the close cannot report it. |
| `SessionEnd` | `Ended` | Eagerly tombstones the session. |
| `Notification` | — | Silent idle enrichment: either a permission attention nudge or 60-second input idle, with no structured discriminator. |

Droid draws native permission prompts, but its stock hooks expose no permission request, question, plan-approval, or answer event. `Notification` carries only display text, so RimZ does not invent an `AwaitingInput` signal from it. `SubagentStop` similarly carries no child identity, so the adapter does not install it or render child rows. Ctrl+C has no guaranteed `Stop`; pane liveness and the stall window settle that path.

Neutral hook output is byte-empty stdout with exit status zero, handing control back to Droid's own UI. The installed command is `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source droid`, with a 10-second timeout and no matcher so every completed tool reaches the classifier.

## Context and transcript

Every hook's `transcript_path` is retained as carry-forward metadata and is never parsed. Factory does not publish the transcript schema or a structured statusline input, and the lifecycle payload carries no model, effort, token, context-window, or cost fields. Context usage, rich context, live cost, and transcript messages therefore stay unsupported rather than inferred.

## Account and balance

Droid exposes no machine-readable local auth, plan, quota, or account-usage surface. The adapter reports no account panel or rate-limit windows.

## Cost

Droid exposes no supported transcript billing schema or cost field. The adapter has no spend parser and declares both live cost and account spend unsupported.

## Launch, resume, fork, and permissions

Fresh launch uses `droid -- <prompt>`, resume uses `droid --resume <id>`, fork uses `droid --fork <id>`, and `/compact` is the smart-compaction command. Profiles map `model` to `--model` and `append-system-prompt-file` to `--append-system-prompt-file`; `effort` and replacement `system-prompt-file` fail at profile validation.

Interactive Droid documents no launch-only autonomy flag. `droid`, `droid-ask`, and `droid-plan` use the user's stock posture; `droid-auto` and `droid-yolo` are unavailable and fail layout parsing. RimZ does not rewrite `~/.factory/settings.json` autonomy settings. The similarly named `--auto`, `--use-spec`, and `--skip-permissions-unsafe` switches belong to `droid exec`, outside this adapter's interactive surface.

The wire fixtures and goldens are researched against Droid CLI 0.121.0. RimZ applies no runtime version gate; refresh the upstream reference and fixtures when Droid's hook behavior drifts.
