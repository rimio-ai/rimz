# Kimi Code adapter

Kimi runs in its stock interactive pane. Command hooks carry lifecycle boundaries and blocking waits; the durable per-agent `wire.jsonl` supplies transcript, model, token, and recovery enrichment. This document maps the current [`MoonshotAI/kimi-code`](https://github.com/MoonshotAI/kimi-code) product. The retired Python `MoonshotAI/kimi-cli` protocol is unsupported.

## Product boundary

The adapter targets Kimi Code and uses `$KIMI_CODE_HOME` (default `~/.kimi-code`) plus top-level agent-record logs. Binary discovery includes the official `~/.kimi-code/bin/kimi` install location, and process binding accepts both the executable name `kimi` and Kimi Code's runtime process title `kimi-code`. The retired Python CLI's `$KIMI_SHARE_DIR`, `~/.kimi`, `--afk`, enveloped Wire messages, and tool names stay outside the adapter.

The pinned upstream contract is [kimi-reference.md](../../externals/agent-adapter/kimi-reference.md).

## Hooks and lifecycle

RimZ installs additive `[[hooks]]` entries in `${KIMI_CODE_HOME:-~/.kimi-code}/config.toml`. Every installed command uses the stable marker `rimz hooks feed --source kimi`, stamps owner-pid context where required, and preserves hook stdout as the decision channel. Uninstall reclaims RimZ-owned entries and preserves user hooks.

| Kimi event | RimZ signal |
| --- | --- |
| `SessionStart` | `registered`; bind the pane to `session_id` |
| `UserPromptSubmit` | `turn_started` with the submitted content parts |
| `PermissionRequest` | `awaiting_input(Permission)`; specialize `ExitPlanMode` to `PlanApproval` |
| `PreToolUse` for `AskUserQuestion` | `awaiting_input(Question)` |
| `PermissionResult` | non-mutating answer edge that clears the native wait |
| every `PostToolUse` / `PostToolUseFailure` | `tool_used`; successful `Write` and `Edit` set edit proof, successful `Bash` sets mutation proof, and failure clears a native wait without claiming mutation |
| `Stop` / `StopFailure` | clean / errored `turn_ended` |
| `Interrupt` | clean `turn_ended`; settle the row to idle |
| `PreCompact` / `PostCompact` | `compacting` / `compaction_ended` with `manual` or `auto` trigger |
| `SessionEnd` | `ended` |

An `AskUserQuestion` call with `background: true` creates durable background work and leaves the main turn runnable, so its `PreToolUse` stays lifecycle-only rather than opening a false foreground wait. `Notification` refreshes parent activity and the bounded record tail when background work reaches a terminal state; background parking remains unsupported because the notification does not prove the active-task set.

`PermissionRequest` and `PermissionResult` replace the old approval Wire scan. They fire around an approval that reaches the native RPC client and correlate by `tool_call_id`. Policy-approved, auto-approved, YOLO-approved, and statically denied calls do not open a native approval wait.

Kimi exposes no separate question hook. `PreToolUse` for `AskUserQuestion` opens the wait; the correlated post-tool hook, interrupt, or turn/session close resolves it. Keep structured answering in the Kimi UI until RimZ owns an SDK, server, or ACP client capable of returning the typed answer protocol.

`Stop` is blockable. RimZ's observer returns empty stdout and exit 0 so a normal stop remains a clean boundary. A user interrupt skips `Stop` and emits `Interrupt`; a fatal turn emits `StopFailure`.

## Session binding and durable records

Sessions live under `${KIMI_CODE_HOME:-~/.kimi-code}/sessions/wd_<slug>_<sha256-prefix>/<session-id>/`. Resolve the directory through the parsed append-only `session_index.jsonl`: each valid line names `sessionId`, absolute `sessionDir`, and `workDir`, and the latest valid line wins for an id. Validate the indexed directory against the data root and session-id basename, then use `state.json.workDir` as the authoritative workspace check; the index's workdir can be stale and `state.json` carries no session id. Do not reproduce the bucket-key algorithm for identity binding.

The main record path is `agents/main/wire.jsonl`; each child owns `agents/<agent-id>/wire.jsonl`. Records are top-level tagged JSON objects, beginning with metadata such as `{"type":"metadata","protocol_version":"1.4",...}`. They are not the retired Python envelope `{timestamp,message:{type,payload}}`. The current tolerant tail parser skips metadata after recognizing the top-level shape; enforcing a supported agent-record version range remains coupled to the deferred CLI compatibility probe.

The adapter binds the resolved main path into the live context sidecar. Hook refreshes seed that path at session/prompt boundaries and retain it on every changed stat, so the shared filesystem watcher drives record-by-record updates even when the session index is temporarily stale. The adapter reads a bounded tail under the stat gate for context and transcript enrichment; cumulative live cost reparses the full main file after each changed stat. The spend cursor consumes complete appended lines by byte offset, and unknown record types and fields remain forward-compatible.

The current tail mapper consumes:

- genuine-user `turn.prompt` and `turn.steer` records for sanitized human transcript anchors;
- `context.append_loop_event` step boundaries and text parts for ordinary assistant reconstruction, excluding thinking and tool plumbing;
- visible assistant-role `context.append_message` records for explicit hook/block output while ignoring duplicated user and injected context messages;
- `config.update` plus `llm.request` for normalized display alias, exact provider/model attribution, and thinking effort;
- every additive `usage.record` for spend, with only explicit turn scope supplying the current-turn split;
- nonzero `step.end.usage`, `context.clear`, and `context.apply_compaction.tokensAfter` as ordered context-fill boundaries.

The durable log also exposes permission, compaction-bracket, tool-snapshot, and child-agent records. Keep answered-ask recovery, tool replay, and child-row joins deferred until their consumers can preserve those facts without guessing.

Clean turn end, open approvals, open questions, retry waits, and live status snapshots are not complete durable-record facts. Hooks own those lifecycle edges; pane/process liveness reconciles missed delivery.

## Context and transcript

`usage.record` carries its model and the exact `inputOther`, `inputCacheRead`, `inputCacheCreation`, and `output` split. `llm.request` carries provider, canonical model id, model alias, effective thinking controls, and request hashes. Strip only a leading `kimi-code/` wrapper from display/config aliases.

The stock pane does not persist the live `agent.status.updated` ratio. Replace turn-grained fill from the newest nonzero `step.end.usage` sum across all four fields, reset it on `context.clear`, and replace it with `context.apply_compaction.tokensAfter` after compaction. A zero-usage step preserves earlier evidence. Resolve the window from the normalized effective `[models.<alias>]` entry, honoring `[models.<alias>.overrides].max_context_size`, and fall back to 262,144 tokens. Publish the shared unknown-fill, zero-usage sentinel when the bounded tail contains no context boundary so an established meter stays stable.

## Cost

Walk records in order and price each usage row from its recognized model, then the latest `llm.request` provider/canonical-model key, then a recognized exact configured model. Unknown aliases stay visible at zero cost and enter the shared unknown-model refresh path; the adapter does not guess K2.5. The byte-offset cursor retains request attribution across appends. `usageScope` says whether an additive request charge also belongs to the current turn; both `turn` and missing/default `session` records contribute to session spend, including full-compaction requests.

Subscription limits remain account enrichment rather than token spend. The managed `/usages` response also exposes optional Booster balance and monthly-cap money fields. The shared `ExtraCredits` projection currently carries USD only, so the adapter publishes USD Booster values and omits other declared currencies rather than mislabeling them.

## Account and balance

The account probe reads only the shape of `${KIMI_CODE_HOME:-~/.kimi-code}/credentials/kimi-code.json`; token bytes never enter output, logs, diagnostics, or hashes. The OAuth quota probe calls `$KIMI_CODE_BASE_URL`, then the configured `managed:kimi-code` base, then the official base, and tolerantly maps summary/limit rows, reset variants, and optional USD Booster wallet fields into provider windows and balances.

An API-key provider may run Kimi Code without managed Kimi OAuth. The current kind-level account seam cannot attribute account and spend to the effective provider selected by the model, so `spend` remains partial. A future provider-aware account key should carry provider identity and declared money currency without changing the lifecycle adapter.

Binary discovery currently identifies an adapter from executable names and install directories. Add an adapter compatibility probe before relying on automatic rejection of the retired Python `kimi` executable; until then, the Kimi Code data root, hook protocol, and process title form the implemented product boundary, while legacy collision refusal remains deferred.

## Subagents and background work

`SubagentStart` and `SubagentStop` refresh parent activity immediately but carry only the profile name and truncated prompt/response. Exact child rows require joining `state.json.agents` with `agents/<agent-id>/wire.jsonl`; the session map supplies child id, parent id, agent type, and home directory. Keep `sub` partial until that join and child liveness land.

Background task state lives under the session's `tasks/` tree, and terminal status reaches the root through `Notification`. A clean `Stop` does not include the active-task set. Keep `bg` unsupported until the adapter joins durable task state and can prove that the parent parked while work remains in flight. Main-session transcript and cost deliberately exclude `agents/<child>/wire.jsonl`; child-inclusive history and spend require the same state-map join as exact subagent rows.

## Launch and supervised runs

Permission launch cells map to the current CLI:

| RimZ mode | Kimi argv |
| --- | --- |
| `ask` | no permission flag; native `manual` mode |
| `auto` | `--auto` |
| `yolo` | `--yolo` |
| `plan` | `--plan` |

`--auto` handles approvals automatically and suppresses questions. `--yolo` skips regular tool approvals but still asks to exit plan mode. The retired `--afk` flag is invalid.

Resume uses `kimi --session <id>`; `--resume` is a hidden alias and `--continue` selects the worktree's latest session. Interactive `/fork` has no documented launch argv, so `rimz agents fork` refuses. Smart compaction sends `/compact` through the pane.

Supervised execution uses `kimi -p <prompt> --output-format stream-json`. Prompt mode applies auto permission and rejects explicit `--auto`, `--yolo`, and `--plan`. Preserve stdout for stream records, stderr for thinking/progress/resume notices, and release-pin non-zero exit semantics before exposing them as a stable RimZ contract.
