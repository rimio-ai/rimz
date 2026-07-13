# Droid adapter

> The agent-agnostic boundary and state machine are in [model.md](./model.md). The verified upstream hook, CLI, transcript, and settings surfaces are in [droid-reference.md](../../externals/agent-adapter/droid-reference.md).

Factory Droid reports basic lifecycle through native `settings.json` hooks. RimZ installs a slim additive hook set into `~/.factory/settings.json`; it leaves Droid's user-owned `statusLine` and persistent autonomy settings untouched. The adapter is intentionally interactive-only: supervised `rimz agents droid -p` launches the stock TUI with a positional prompt and completes from the native `Stop` hook rather than replacing the TUI with a separate exec transport.

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

Droid 0.170.0's stock TUI starts an internal `droid exec --input-format stream-jsonrpc --output-format stream-jsonrpc` worker, and both processes apply the global hook configuration. RimZ suppresses structurally recognized outer-TUI hooks before workspace or store work and accepts the worker's complete stream; accepted worker observations retain the outer TUI PID as their runtime owner so liveness follows the pane process. A directly launched `droid exec` stays accepted and self-owned, while unreadable or unrecognized process metadata fails open.

## Context and transcript

Every hook's `transcript_path` is authoritative carry-forward metadata. For Droid 0.170.0, RimZ parses only a `session_start.version = 2` JSONL source: complete history follows the physically latest visible message through its `parentId` chain, while incremental streaming emits newly appended visible assistant records in physical order because a suffix lacks its ancestors. Visible user and assistant text blocks reach history; thinking, tool/document content, `llm_only` context, `user_only` hook audit rows, malformed records, and unknown versions stay out. `Stop` tail-reads the latest visible assistant answer for durable transcript, supervised result, streaming, and message-reply capture.

The sibling `<session-id>.settings.json` snapshot supplies raw `model` and `reasoningEffort` at hook cadence, with the newest visible assistant's `modelId` and effort as fallback. Session-wide `tokenUsage`, `inclusiveTokenUsage`, and Factory credits have no current-context denominator or authoritative USD conversion, so context usage, token composition, live cost, smart-compaction state, and spend remain unset. The README's Live grade therefore stays none even though identity enrichment is available.

## Account and balance

Droid exposes no machine-readable local auth, plan, quota, or account-usage surface. The adapter reports no account panel or rate-limit windows.

## Cost

Droid exposes no authoritative transcript USD billing field. The adapter has no spend parser and declares both live cost and account spend unsupported.

## Launch, resume, fork, and permissions

Fresh launch uses `droid -- <prompt>`, resume uses `droid --resume <id>`, fork uses `droid --fork <id>`, and `/compact` is the smart-compaction command. Profiles map `append-system-prompt-file` to `--append-system-prompt-file`; `model`, `effort`, and replacement `system-prompt-file` fail at profile validation. Interactive Droid 0.170.0 carries no `--model` or `--reasoning-effort` flag — both are `droid exec`-only, and its parser accepts unknown options, so an emitted `--model <id>` would be silently ignored and leak the id into the positional prompt. Model and effort are chosen in-session (`/model`, Tab) or through `droid exec`.

Current interactive Droid accepts launch-scoped `--auto <level>` and `--use-spec`. `droid-auto` selects `--auto medium`, the closest fit for normal local development; `droid-plan` starts with `--use-spec`; and `droid`/`droid-ask` retain the user's configured autonomy and native permission UI. `droid-yolo` remains unavailable because `--skip-permissions-unsafe` belongs to `droid exec`; RimZ does not rewrite persistent autonomy settings.

The stock CLI, hook wire fixtures, and goldens are researched against Droid CLI 0.170.0; the private transcript reader additionally requires version 2 at runtime and abstains on drift. The structured exec research remains pinned separately to the public SDK version named in the upstream reference. Refresh the upstream reference and fixtures when Droid's behavior changes.

The 0.170.0 binary's help verifies the launch flags without authentication: interactive `droid --help` lists `--auto`, `--use-spec`, `--resume`/`--fork`, `--append-system-prompt[-file]`, `--settings`, `--cwd`, and `--worktree[-dir]`, but no `--model` or `--reasoning-effort` (those are `droid exec`-only, and interactive `-r` is `--resume`). A logged-in live-pane pass still needs to confirm hook delivery, session-id rotation, and the exact first-turn posture for each suffix.

## Deferred integration work

The current adapter deliberately stays on the stock interactive pane and its native hooks. Its internal exec worker is only the canonical hook emitter; RimZ does not speak its JSON-RPC transport. A future direct supervised `droid exec` integration can add authoritative permission/question requests, context stats, token usage, model/effort updates, failed outcomes, and identified mission workers, but it needs a process-lifecycle and ask-answer path that is larger than this adapter.

Launch-time model and effort selection is deferred with it. The interactive CLI has no `--model`/`--reasoning-effort`, so `render_preset` rejects both today. Interactive `--settings <path>` merges a process-only settings file that *can* carry `model`, `reasoningEffort`, and `autonomyLevel`, so a per-launch model is reachable once RimZ grows a launch-scoped temp-file lifecycle: `render_preset` currently returns argv only, with no channel to materialize and clean up a generated settings file for the pane's lifetime. That temp-config plumbing is the missing abstraction — a note for a future `AgentAdapter` round, since a launch-scoped side-file would let every built-in carry richer presets than flags express.

Compaction can rotate Droid's session id while one native event both closes the old bracket and introduces the replacement. `AgentAdapter::observe_lifecycle` emits one observation per event today, so the adapter records the close on the reported id and lets later activity establish the replacement row. Supporting a close-old/register-new pair requires a kind-agnostic multi-observation or explicit session-replacement abstraction.

Factory recommends an absolute hook command because hook cwd can change, while RimZ's built-in installers currently use the shared PATH-resolved `rimz hooks feed` shape. Resolving and migrating the running RimZ executable belongs in the common hook-install abstraction so every built-in gets identical trust hashing, drift detection, and upgrades.
