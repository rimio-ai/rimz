# Cursor adapter

> The agent-agnostic boundary and state machine are in [model.md](./model.md). The verified upstream hook and CLI surface is in [cursor-reference.md](../../externals/agent-adapter/cursor-reference.md).

Cursor runs as `agent` or its `cursor-agent` alias; `cursor` names the IDE and is intentionally outside binary discovery. RimZ installs additive user hooks in `~/.cursor/hooks.json`, launches the resolved CLI path, and keys every session on `conversation_id`.

## Hooks and lifecycle

| Native event | Normalized signal | Fields |
| --- | --- | --- |
| `sessionStart` | `Registered` | `conversation_id`, model, transcript path |
| `beforeSubmitPrompt` | `TurnStarted` | sanitized `prompt` |
| `postToolUse` for `Shell`/`Write`/`Delete` | `ToolUsed { mutates: true, edits }` | `Write` and `Delete` edit; `Shell` only mutates |
| `postToolUseFailure` | — | activity heartbeat only |
| `stop` | `TurnEnded` | only `status: completed` is clean; `aborted` and `error` set `errored` |
| `preCompact` | `Compacting` | context percentage and window |
| `sessionEnd` | `Ended` | tombstones the session |

Cursor exposes no post-compaction hook. The next lifecycle signal closes the open bracket in the shared `step` state machine, and the projection expires the compaction head after its display window; coverage is partial because the landing phase and status are not native. Cursor also exposes `subagentStart`, but `subagentStop` omits the child id supplied at start, so the adapter leaves both signals unsupported until a live capture proves a stable stop-side correlation key.

Cursor's local hooks expose no permission request, plan approval, question, or idle notification. RimZ therefore emits no `AwaitingInput` signal and no waiting row: the native Cursor prompt remains usable in its pane, but there is no reliable event that says it is open. Every installed event returns Cursor's documented-safe neutral `{}` JSON. The Claude adapter drops payloads carrying `cursor_version`, preventing Cursor's optional Claude-compatible hook loading from double-recording one event.

## Context and transcript

`preCompact.context_usage_percent` supplies the rounded, clamped context gauge, and `context_window_size` supplies the window. `context_tokens` is occupancy rather than cumulative usage, so it does not populate `total_tokens`. `model_id` labels the row, with legacy `model` as fallback; the common `model_params` entry named `effort` supplies the displayed effort and unknown parameters remain forward-compatible.

Cursor publishes `transcript_path` but not the transcript schema. RimZ carries the path as metadata and does not parse or tail the file. Workspace ownership continues to come from the stamped pane/session relationship; `postToolUse.cwd` is enrichment only.

## Account and balance

Cursor documents `status --format json` and `about --format json` without publishing either response schema or a credential format. A logged-out `2026.07.09-a3815c0` capture pins the shape of both: `status` carries `isAuthenticated` and token-presence booleans, and `about` carries `subscriptionTier` (the plan label, `null` logged out), `userEmail` (the account identity, `null` logged out), and `cliVersion`. Those three `about` fields are the concrete candidate `probe_account` source ([cursor-reference.md → Authentication](../../externals/agent-adapter/cursor-reference.md#authentication-and-account-surface)). The adapter still reports account state, spend, and quota as unsupported: the logged-in arm is uncaptured, so the tier string values and the `userEmail` shape stay unverified, and building a probe from one login arm is the failure mode the model warns against ([model.md → Adding an agent](./model.md#adding-an-agent)). The only machine-readable usage feed Cursor ships is the team-scoped Admin API behind an admin token, which is not a stock per-user CLI credential.

## Cost

The documented hook and headless streams expose no machine-readable tokens, dollars, or price attribution. Realtime cost and historical account spend remain unsupported, and the adapter has no spend parser.

## Launch and permission modes

RimZ maps Ask to Cursor's default launch, Plan to `--mode=plan`, Auto to `--auto-review`, and Yolo to `--force --sandbox disabled`. Auto-review lets allowlisted and sandboxable calls proceed and sends the remainder through Cursor's classifier; Yolo explicitly selects the unrestricted posture. `/summarize` is the manual compaction command, `--resume <conversation_id>` resumes, and no CLI-by-id fork surface is declared.

The shared command classifier now reads adapter `bin_names`, so both stock entrypoints (`agent` and `cursor-agent`) produce Cursor presence before the first session hook; this is the first built-in whose executable basename differs from its kind. The installed Linux binary reports `MainThread` as its kernel `comm`, so hook attribution continues to use the installer-stamped `$PPID` and durable pane/session stamps rather than treating that generic runtime label as Cursor identity.

## Wired now

Session identity, turn boundaries, mutating-tool activity and the acting phase, the compaction-open bracket, the context gauge and window, session end, hook install/uninstall, resume, the four permission modes, and manual `/summarize` compaction. The launch-flag surface (`--mode=plan`, `--auto-review`, `--force`, `--sandbox disabled`, `--resume`) is verified against the installed `2026.07.09-a3815c0` build; the runtime *interaction* of those flags with neutral `{}` output still needs a live session.

## Deferred to a later round

- **Interrupted turns read as failed.** A user Esc lands `stop.status: "aborted"`, which the adapter maps to the errored bit, so the row escalates to `!` rather than settling at rest — the same conservative approximation Pi takes for its aborted `stopReason` ([model.md → Extending the signal vocabulary](./model.md#the-state-machine)). Cursor's `stop` is a stronger interruption certificate than the derived markers Codex and Claude rely on, so a future round can settle the row to `idle` (see the abstraction note below).
- **Subagents.** `subagentStop` omits the child id `subagentStart` supplies, so neither signal is wired until a live capture proves a stable stop-side correlation key.
- **Account and cost.** Blocked on a logged-in `about --format json` capture (above) and, for spend, on any machine-readable per-user usage feed Cursor does not yet ship.
- **Supervised `-p` runs.** The shared run abstraction extracts the final answer from an interactive terminal hook or a supported transcript; Cursor exposes its final response on a separate `afterAgentResponse` event and keeps the transcript schema opaque. A future `-p` transport can teach the run layer to correlate that event with the following `stop`, or host Cursor's documented stream-JSON mode; the adapter does not persist response text merely to bridge that gap.
- **Live verification** remains required for the hook command's shell and `$PPID` semantics on each platform, the `--` prompt terminator, neutral `{}` under every approval mode, `conversation_id` stability across resume and clear, and Claude-compatible third-party-hook cross-fire.

## A note for the RimZ abstraction

`LifecycleSignal::TurnEnded` carries only `errored` and `parked_on_background`, so it cannot express a turn that *ended at rest because it was interrupted* — a clean landing that is neither `success` nor `failed`. Today both Cursor (`stop.status: "aborted"`) and Pi (aborted `stopReason`) collapse that case onto `errored`, painting a false failure, while Codex and Claude reach `idle` only through the projection-side turn-interruption marker ([model.md → Displayed status](./model.md#displayed-status)), which rescues a still-`running` row and never a `failed` one. A provider that reports its own interruption on the turn-end event has no clean path to the calm landing. Worth considering in a future round: an interrupted variant of `TurnEnded`, or folding a native interrupted end into the same `turn_interrupted` enrichment the projection already reads.
