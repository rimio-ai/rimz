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

Fresh launch uses `droid -- <prompt>`, resume uses `droid --resume <id>`, fork uses `droid --fork <id>`, and `/compact` is the smart-compaction command. Profiles map `append-system-prompt-file` to `--append-system-prompt-file`; `model`, `effort`, and replacement `system-prompt-file` fail at profile validation. Interactive Droid 0.170.0 carries no `--model` or `--reasoning-effort` flag — both are `droid exec`-only, and its parser accepts unknown options, so an emitted `--model <id>` would be silently ignored and leak the id into the positional prompt. Model and effort are chosen in-session (`/model`, Tab) or through `droid exec`.

Current interactive Droid accepts launch-scoped `--auto <level>` and `--use-spec`. `droid-auto` selects `--auto medium`, the closest fit for normal local development; `droid-plan` starts with `--use-spec`; and `droid`/`droid-ask` retain the user's configured autonomy and native permission UI. `droid-yolo` remains unavailable because `--skip-permissions-unsafe` belongs to `droid exec`; RimZ does not rewrite persistent autonomy settings.

The stock CLI, hook wire fixtures, and goldens are researched against Droid CLI 0.170.0; the structured exec research remains pinned separately to the public SDK version named in the upstream reference. RimZ applies no runtime version gate; refresh the upstream reference and fixtures when Droid's behavior drifts.

The 0.170.0 binary's help verifies the launch flags without authentication: interactive `droid --help` lists `--auto`, `--use-spec`, `--resume`/`--fork`, `--append-system-prompt[-file]`, `--settings`, `--cwd`, and `--worktree[-dir]`, but no `--model` or `--reasoning-effort` (those are `droid exec`-only, and interactive `-r` is `--resume`). A logged-in live-pane pass still needs to confirm hook delivery, session-id rotation, and the exact first-turn posture for each suffix.

## Deferred integration work

The current adapter deliberately stays on the stock interactive pane and its native hooks. A future supervised `droid exec` transport can add authoritative permission/question requests, context stats, token usage, model/effort updates, failed outcomes, and identified mission workers, but it needs a process-lifecycle and ask-answer path that is larger than this adapter.

Launch-time model and effort selection is deferred with it. The interactive CLI has no `--model`/`--reasoning-effort`, so `render_preset` rejects both today. Interactive `--settings <path>` merges a process-only settings file that *can* carry `model`, `reasoningEffort`, and `autonomyLevel`, so a per-launch model is reachable once RimZ grows a launch-scoped temp-file lifecycle: `render_preset` currently returns argv only, with no channel to materialize and clean up a generated settings file for the pane's lifetime. That temp-config plumbing is the missing abstraction — a note for a future `AgentAdapter` round, since a launch-scoped side-file would let every built-in carry richer presets than flags express.

Compaction can rotate Droid's session id while one native event both closes the old bracket and introduces the replacement. `AgentAdapter::observe_lifecycle` emits one observation per event today, so the adapter records the close on the reported id and lets later activity establish the replacement row. Supporting a close-old/register-new pair requires a kind-agnostic multi-observation or explicit session-replacement abstraction.

Factory recommends an absolute hook command because hook cwd can change, while RimZ's built-in installers currently use the shared PATH-resolved `rimz hooks feed` shape. Resolving and migrating the running RimZ executable belongs in the common hook-install abstraction so every built-in gets identical trust hashing, drift detection, and upgrades.
