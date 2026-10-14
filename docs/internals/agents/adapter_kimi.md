# Kimi Code adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md); the raw upstream protocol is in [kimi-reference.md](../../externals/agent-adapter/kimi-reference.md).

Kimi runs in its stock interactive pane. Command hooks carry lifecycle boundaries and blocking waits; the durable per-agent `wire.jsonl` supplies transcript, model, token, and recovery enrichment.

Only the registry reaches the concrete adapter. Account and quota probes, payloads, wire parsing, transcript normalization, child-record derivation, installation, and spend stay private to the Kimi adapter; other modules consume the provider-neutral capabilities.

**Product boundary.** This adapter maps [`MoonshotAI/kimi-code`](https://github.com/MoonshotAI/kimi-code) and uses `$KIMI_CODE_HOME` (default `~/.kimi-code`) plus top-level agent-record logs. Binary discovery includes the official `~/.kimi-code/bin/kimi` install location, and process binding accepts both the executable name `kimi` and Kimi Code's runtime process title `kimi-code`. The retired Python `MoonshotAI/kimi-cli` protocol is unsupported: its `$KIMI_SHARE_DIR`, `~/.kimi`, `--afk`, enveloped Wire messages, and tool names stay outside the adapter.

## Hooks and lifecycle

RimZ installs additive `[[hooks]]` entries in `${KIMI_CODE_HOME:-~/.kimi-code}/config.toml`. Every installed command uses the stable marker `rimz hooks feed --source kimi`, stamps owner-pid context where required, and preserves hook stdout as the decision channel. Uninstall reclaims RimZ-owned entries and preserves user hooks.

Installation and preview refuse a detected CLI version below 2.1.1 with `kimi upgrade -y` as the fix. An unknown or unparseable version does not block installation. Every existing hook entry is validated against Kimi Code's full 20-event catalog and strict four-field schema before any rewrite. An invalid entry makes the installed check false and installation/preview refuse with the config path, entry, and fix: Kimi Code ignores every hook while one entry is invalid. Valid foreign hooks survive reinstall.

| Kimi event | RimZ signal |
| --- | --- |
| `SessionStart` | `registered`; bind the pane to `session_id` |
| `UserPromptSubmit` | `turn_started` with the submitted content parts |
| `TurnStarted` | `turn_started` for main-agent non-user origins, excluding `retry` and `system_trigger`/`subagent` |
| `PermissionRequest` | `awaiting_input(Permission)`; specialize `ExitPlanMode` to `PlanApproval` |
| `PreToolUse` for `AskUserQuestion` | `awaiting_input(Question)` |
| `PermissionResult` | non-mutating answer edge that clears the native wait |
| every `PostToolUse` / `PostToolUseFailure` | `tool_used`; successful `Write` and `Edit` set edit proof, successful `Bash` sets mutation proof, and failure clears a native wait without claiming mutation |
| `Stop` / `StopFailure` | clean / errored `turn_ended`, unless the main wire identifies a child-fired hook |
| `Interrupt` | clean `turn_ended`, unless the main wire identifies a child-fired hook |
| `PreCompact` / `PostCompact` | `compacting` / `compaction_ended` with `manual` or `auto` trigger |
| `SessionEnd` | `ended` |

An `AskUserQuestion` call with `background: true` creates durable background work and leaves the main turn runnable, so its `PreToolUse` stays lifecycle-only rather than opening a false foreground wait. `Notification` refreshes parent activity and the bounded record tail when background work reaches a terminal state.

`PermissionRequest` and `PermissionResult` replace the old approval Wire scan. They fire around an approval that reaches the native RPC client and correlate by `tool_call_id`. Policy-approved, auto-approved, YOLO-approved, and statically denied calls do not open a native approval wait.

Kimi exposes no separate question hook. `PreToolUse` for `AskUserQuestion` opens the wait; the correlated post-tool hook, interrupt, or turn/session close resolves it.

`Stop` is blockable. RimZ's observer returns empty stdout and exit 0 so a normal stop remains a clean boundary. A user interrupt skips `Stop` and emits `Interrupt`; a fatal turn emits `StopFailure`.

### Subagents and background work

After each recorded root observation, the shared derive pass reads the main wire's durable `subagent.spawned`, `subagent.completed`, `subagent.failed`, and `subagent.cancelled` records. Child rollup ids are namespaced as `<session-id>:<subagentId>`, with the root `session_id` as the parent link. Child ids must be one normal path component. Starts carry the spawn's name, sanitized description, model, and effort; absent attribution falls back to the child wire at `agents/<subagentId>/wire.jsonl`.

The fold retains each child's latest run. Failed records settle errored; completed and cancelled records settle not-errored. Completion usage supplies the child's run-token total, never an extra spend charge. The shared derive pass creates missing rows and settles nonterminal rows once; finished rows do not reopen on resume. `SubagentStart` and `SubagentStop` are not installed, and stale installations' copies record nothing: Start precedes durable identity, and Stop fires only on success.

A child turn also fires session-shaped turn hooks. Suppression requires a `subagent.spawned` record in the main wire and either an open main step (latest `step.begin`/`step.end` is `step.begin`) or a latest turn already closed with a reason this hook does not report. The open-step rule applies to Stop, StopFailure, Interrupt, and non-user TurnStarted. The closed-turn rule applies to Stop for every reason, StopFailure except `failed`, and Interrupt except `cancelled`. `llm.request` alone does not open a step. Bounded 10/20/40/80/100 ms rereads confirm suppression; unreadable wires or evidence that clears fail open. PreCompact and PostCompact always pass through, because main compaction can overlap an open step.

Background task state lives under each agent's `agents/<agent-id>/tasks/` tree, and terminal status reaches the root through `Notification`. The parent transcript, model, and context tokens stay main-only. Fleet spend, seat lifetime spend, and the parent card's live dollars include every child wire; child cards do not receive live dollar refreshes.

## Launch and resume

Bare RimZ `--resume` uses the native `kimi --session` picker ([upstream](../../externals/agent-adapter/kimi-reference.md)).

Permission launch cells map to the current CLI:

| RimZ mode | Kimi argv |
| --- | --- |
| `ask` | no permission flag; native `manual` mode |
| `auto` | `--auto` |
| `yolo` | `--yolo` |
| `plan` | `--plan` |

`--auto` handles approvals automatically and suppresses questions. `--yolo` skips regular tool approvals but still asks to exit plan mode. The retired `--afk` flag is invalid.

Resume uses `kimi --session <id>`; `--resume` is a hidden alias and `--continue` selects the worktree's latest session. `/fork` and `kimi fork [sessionId]` copy a session and print the new id without opening it, and no launch flag forks and opens in one command, so `rimz agents fork` refuses. Smart compaction sends `/compact` through the pane.

Supervised execution uses `kimi -p <prompt> --output-format stream-json`. Prompt mode applies auto permission and rejects explicit `--auto`, `--yolo`, and `--plan`. Stdout carries stream records and stderr carries thinking, progress, and resume notices.

## Context and transcript

### Session binding and durable records

Sessions live under `${KIMI_CODE_HOME:-~/.kimi-code}/sessions/wd_<slug>_<sha256-prefix>/<session-id>/`. Resolve the directory through the parsed `session_index.jsonl`, which Kimi compacts since 2.0.1: each valid line names `sessionId`, absolute `sessionDir`, and `workDir`, and the latest valid line wins for an id. Validate the indexed directory against the data root and session-id basename, then use the `state.json` working directory as the authoritative workspace check: metadata version 2 records `cwd`, and a session Kimi has not rewritten yet still carries `workDir`. The index's workdir can be stale. Do not reproduce the bucket-key algorithm for identity binding.

The main record path is `agents/main/wire.jsonl`; each child owns `agents/<agent-id>/wire.jsonl`. Records are top-level tagged JSON objects, beginning with metadata such as `{"type":"metadata","protocol_version":"1.5",...}`. They are not the retired Python envelope `{timestamp,message:{type,payload}}`. The tolerant tail parser skips metadata after recognizing the top-level shape.

The adapter binds the resolved main path into the live context sidecar. Hook refreshes seed that path at session/prompt boundaries and retain it on every changed stat, so the shared filesystem watcher drives record-by-record updates even when the session index is temporarily stale. On a changed stat, one torn-safe full read parses every complete record once: cumulative cost consumes the full typed snapshot while context sees only its record-aligned logical 64 KiB tail, expanding for one oversized newest record. The historical spend cursor still consumes only complete appended lines by byte offset, and unknown record types and fields remain forward-compatible.

The tail mapper consumes:

- genuine-user `turn.prompt` and `turn.steer` records for sanitized human transcript anchors;
- `context.append_loop_event` step boundaries and text parts for ordinary assistant reconstruction, excluding thinking and tool plumbing;
- visible assistant-role `context.append_message` records for explicit hook/block output while ignoring duplicated user and injected context messages;
- `config.update` plus `llm.request` for normalized display alias, exact provider/model attribution, and thinking effort;
- every additive `usage.record` for spend, with only explicit turn scope supplying the current-turn split;
- nonzero `step.end.usage`, `context.clear`, and `context.apply_compaction.tokensAfter` as ordered context-fill boundaries.

The durable log also exposes permission, compaction-bracket, tool-snapshot, and child-agent records, and protocol 1.5 adds `turn.ended`, `interaction.request` and `interaction.resolved`, `turn.step.retrying`, and `task.started` and `task.terminated`. Child derivation consumes the `subagent.*` records; the child-fired hook discriminator consumes `turn.ended` and its reason.

Hooks own turn, approval, and question edges; pane/process liveness reconciles missed delivery.

### Model, tokens, and the context gauge

The session `state.json` title publishes as the card's session preview. Kimi's pre-prompt `"New Session"` placeholder is dropped, so a new session publishes no preview until Kimi initializes the title from the first prompt; an explicit rename replaces it and keeps the activity description stable across later turns.

`usage.record` carries its model and the exact `inputOther`, `inputCacheRead`, `inputCacheCreation`, and `output` split. `llm.request` carries provider, canonical model id, model alias, effective thinking controls, and request hashes. Strip only a leading `kimi-code/` wrapper from display/config aliases.

The stock pane does not persist the live `agent.status.updated` ratio. Replace turn-grained fill from the newest nonzero `step.end.usage` sum across all four fields, reset it on `context.clear`, and replace it with `context.apply_compaction.tokensAfter` after compaction. A zero-usage step preserves earlier evidence. When a clear or a compaction replaced the window after the latest counted step, publish that count as `current_context_tokens` too: the tail still holds the older step's split, and the scalar outranks it as the card's numerator. A step's own count includes its output and sits beside that step's split, so it publishes no scalar. Resolve the window from the normalized effective `[models.<alias>]` entry, honoring `[models.<alias>.overrides].max_context_size`, and fall back to 262,144 tokens. Publish the shared unknown-fill, zero-usage sentinel when the bounded tail contains no context boundary so an established meter stays stable.

## Account and balance

The account probe resolves the credential slot from the `oauth.key` of `[providers."managed:kimi-code"]` in `config.toml`: `oauth/kimi-code` is `credentials/kimi-code.json`, and a scoped key such as the one a `global` (kimi.ai) login writes names `credentials/kimi-code-env-<hex>.json`. Without that entry it reads the default slot. It parses the typed credential shape; token bytes never enter output, logs, diagnostics, or hashes. A refresh token is enough to report a managed login, while the quota probe uses an access token only when its numeric `expires_at` remains more than 60 seconds in the future. RimZ leaves token refresh and credential-file writes to Kimi Code.

The managed OAuth quota probe resolves the base as Kimi Code does (`$KIMI_CODE_BASE_URL`, then the `managed:kimi-code` `base_url`, then the mainland default) and sends `Accept: application/json` and the bearer only to `https://api.kimi.com/coding/v1/usages` or `https://api.kimi.ai/coding/v1/usages`, with redirects disabled. Any other effective base makes the probe unavailable before it reads the token, so a custom model endpoint never receives the managed credential.

The `usages` block supplies `limit_5h` (300 minutes), `limit_7d` (10,080 minutes), `limit_month_total` (scope `month_total`, label `mo`), and `limit_month_code` (scope `month_code`, label `mcd`). Month scopes have no fixed duration. A finite numeric or numeric-string `used_ratio` is clamped to 0..1 and rounded to a percentage; missing or invalid ratios drop that entry. An invalid `reset_time` leaves the window without a reset. Legacy `usage`/`limits` keys are ignored. A Booster-only response remains observable with no windows and a debug message about missing `usages`; a body with neither fails, even if it names a plan. Booster money reaches `ExtraCredits` only when every declared money value is USD.

An API-key provider may run Kimi Code without managed Kimi OAuth. The managed account and quota cache remains kind-wide, while transcript spend can cover effective custom providers only where model attribution and the shared price book establish a local estimate; provider quota and local dollars remain separate facts.

## Cost

Walk records in order and price each usage row from its recognized model, then the latest `llm.request` provider/canonical-model key, then a recognized exact configured model. Unknown aliases stay visible at zero cost and enter the shared unknown-model refresh path; the adapter does not guess K2.5. The byte-offset cursor retains request attribution across appends. `usageScope` says whether an additive request charge also belongs to the current turn; both `turn` and missing/default `session` records contribute to session spend, including full-compaction requests.

Fleet discovery, seat lifetime spend, and live parent cost use the main wire plus every `agents/<id>/wire.jsonl` in the session. Child entries retain the parent session's thread id. Only the per-card refresh stat includes a companion stamp of child lengths and newest child mtime, so growing child wires invalidate its live-cost gate. Fleet parsing uses each wire's physical stat and independent incremental cursor. The turn-end fallback also sees the physical stat; its upward-only reconciliation preserves the parent's child-inclusive cost. Only `usage.record` is priced; `subagent.completed.usage` is not added again.

Subscription limits remain account enrichment rather than token spend. The managed `/usages` response also exposes optional Booster balance and monthly-cap money fields. The shared `ExtraCredits` projection currently carries USD only, so the adapter publishes USD Booster values and omits other declared currencies rather than mislabeling them.

## Known gaps

Run `rimz coverage` for the current wired/partial/unsupported matrix. The gaps below are the ones with a reason worth recording.

- **Structured answering stays in the Kimi UI.** RimZ has no SDK, server, or ACP client able to return Kimi's typed answer protocol, so `rimz answer` is unsupported.
- **Background parking is unsupported.** Terminal task status reaches the root through `Notification`, but that notification does not prove the active-task set and a clean `Stop` does not include it. `bg` stays unsupported until the adapter joins durable task state and can prove the parent parked while work remains in flight.
- **Subagent coverage is partial.** Children surface at the next recorded root hook after durable spawn records land. Resumed runs stay closed because the shared seam has no run discriminator. Cancellation, including upstream timeouts, shows as finished rather than failed. A child's compaction shows on the parent card; if cancelled before PostCompact, the card stays compacting until another root signal.
- **Answered-ask recovery and tool replay stay deferred** until their consumers can preserve those facts without guessing.
- **CLI compatibility has a floor, not an identity probe.** Installation refuses detected versions below 2.1.1; unknown versions pass. A colliding executable with an unparseable version is not automatically rejected. The Kimi Code data root, hook protocol, and process title remain the product boundary.
- **Unused hook events stay unwired.** `TaskStarted`, `UserPromptQueued`, and `SessionHeartbeat` have no mapped RimZ behavior. Valid foreign entries using them are preserved.
- **Supervised exit codes stay generic.** Prompt mode exits 1 for a failed, cancelled, or hook-blocked turn alike, and for startup, auth, and option errors, so a non-zero exit carries no failure class RimZ could map.
