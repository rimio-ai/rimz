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
| `afterAgentResponse` | — | safe final visible assistant text; content only |
| `stop` | `TurnEnded` or `TurnInterrupted` | `completed` is clean, `error` fails, and `aborted` lands idle |
| `preCompact` | `Compacting` | context percentage and window |
| `sessionEnd` | `Ended` | tombstones the session |

Cursor exposes no post-compaction hook. The next lifecycle signal closes the open bracket in the shared `step` state machine, and the projection expires the compaction head after its display window; coverage is partial because the landing phase and status are not native. Cursor also exposes `subagentStart`, but `subagentStop` omits the child id supplied at start, so the adapter leaves both signals unsupported until a live capture proves a stable stop-side correlation key.

Cursor's local hooks expose no permission request, plan approval, question, or idle notification. RimZ therefore emits no `AwaitingInput` signal and no waiting row: the native Cursor prompt remains usable in its pane, but there is no reliable event that says it is open. Every installed event returns Cursor's documented-safe neutral `{}` JSON. The Claude adapter drops payloads carrying `cursor_version`, preventing Cursor's optional Claude-compatible hook loading from double-recording one event.

These lifecycle, response, and token claims apply to ordinary interactive Cursor sessions. RimZ supervised runs stay on that transport: the launcher opens the real interactive CLI in a pane and supplies the initial prompt positionally after `--`; it does not pass `-p` or `--print`. Cursor's native headless `--print` transport is separate and outside the hook-driven coverage contract.

## Context and transcript

`preCompact.context_usage_percent` supplies the rounded, clamped context gauge, and `context_window_size` supplies the window. `context_tokens` is occupancy rather than cumulative usage, so it does not populate `total_tokens`. Interactive `stop` supplies per-turn fresh input, output, cache-read, and cache-write counts; explicit zeroes remain visible and these counters never populate cumulative `total_tokens`. A missed-stop transcript recovery restores only terminal state because the JSONL terminal row carries no tokens. `model_id` labels the row, with legacy `model` as fallback; the common `model_params` entry named `effort` supplies the displayed effort and malformed or unknown parameters remain field-locally ignorable.

Cursor CLI `2026.07.09-a3815c0` writes one JSONL file at `~/.cursor/projects/<workspace>/agent-transcripts/<conversation_id>/<conversation_id>.jsonl`. An authenticated native resume rewrote that same path as a full conversation snapshot, replacing the prior terminal placement rather than appending a new suffix. RimZ stats the file, reads its bounded whole tail, and recovers a missed success, interruption, or error boundary only when a complete recognized `turn_ended` row is the last meaningful record and no torn suffix follows it. A later nonterminal, unknown, malformed, or partial record keeps the active turn running until a new complete terminal row or full snapshot arrives. RimZ never models or consumes assistant, thinking, user, tool, or message content from this file. Resolution prefers the current hook path, then the persisted path, then one unambiguous exact conversation match beneath the immediate project directories. Workspace ownership continues to come from the stamped pane/session relationship; `postToolUse.cwd` is enrichment only.

`afterAgentResponse.text` is Cursor's sole safe final-text source. Hook ingestion appends that trimmed response to RimZ's durable transcript and seeds an active supervised run without ending it; the later `stop` remains the delivery checkpoint and terminal status transition. Cursor's native JSONL merges visible assistant commentary with model thinking into indistinguishable text blocks, so native history paging and reply streaming deliberately remain empty.

Cursor hook sets installed before `afterAgentResponse` are incomplete because they cannot capture safe final text. Incomplete-hook detection reports the gap; run `rimz hooks install cursor` once after upgrading to add the RimZ-owned response hook while preserving user-owned and unknown entries.

## Account and balance

Cursor documents `status --format json` and `about --format json` without publishing either response schema or a credential format. A sanitized authenticated browser-login capture on `2026.07.09-a3815c0` pins one arm: `status` carries `isAuthenticated`, token-presence booleans, and structured `userInfo`; `about` carries non-empty string `subscriptionTier` and `userEmail` fields plus `cliVersion`. Those fields are candidate `probe_account` inputs ([cursor-reference.md → Authentication](../../externals/agent-adapter/cursor-reference.md#authentication-and-account-surface)). The adapter still reports account state, spend, and quota as unsupported because unauthenticated, expired, API-key, service-account, proxy, and server-error arms remain unverified; building a probe from one successful login arm is the failure mode the model warns against ([model.md → Adding an agent](./model.md#adding-an-agent)). The only machine-readable usage feed Cursor ships is the team-scoped Admin API behind an admin token, which is not a stock per-user CLI credential.

## Cost

The stop hook exposes per-turn token composition but no dollars or trustworthy historical model-priced totals. Realtime cost and historical account spend remain unsupported, and the adapter has no spend parser.

## Launch and permission modes

RimZ maps Ask to Cursor's default launch, Plan to `--mode=plan`, Auto to `--auto-review`, and Yolo to `--force --sandbox disabled`. Auto-review lets allowlisted and sandboxable calls proceed and sends the remainder through Cursor's classifier; Yolo explicitly selects the unrestricted posture. Fresh and supervised launches remain ordinary interactive positional argv with no `-p` or `--print`. `/summarize` is the manual compaction command, `--resume <conversation_id>` resumes, and no CLI-by-id fork surface is declared.

The shared command classifier now reads adapter `bin_names`, so both stock entrypoints (`agent` and `cursor-agent`) produce Cursor presence before the first session hook; this is the first built-in whose executable basename differs from its kind. The installed Linux binary reports `MainThread` as its kernel `comm`, so hook attribution continues to use the installer-stamped `$PPID` and durable pane/session stamps rather than treating that generic runtime label as Cursor identity.

## Wired now

Interactive session identity, turn boundaries including native interruption, safe final responses, per-turn token composition, bounded transcript-tail recovery, mutating-tool activity and the acting phase, the compaction-open bracket, the context gauge and window, session end, hook install/uninstall, resume, the four permission modes, and manual `/summarize` compaction. The launch-flag surface (`--mode=plan`, `--auto-review`, `--force`, `--sandbox disabled`, `--resume`) is verified against the installed `2026.07.09-a3815c0` build; the runtime *interaction* of those flags with neutral `{}` output still needs a live interactive session.

## Deferred to a later round

- **Subagents.** `subagentStop` omits the child id `subagentStart` supplies, so neither signal is wired until a live capture proves a stable stop-side correlation key.
- **Account and cost.** One authenticated browser-login status/about arm is captured, but the other auth and failure arms remain unverified; spend is also blocked on a machine-readable per-user usage feed Cursor does not yet ship.
- **Full native history and streaming.** Safe final responses reach RimZ's transcript and supervised output, but native assistant-history replay and incremental reply streaming remain empty because Cursor's JSONL text merges visible output with thinking.
- **Native headless transport.** One authenticated `--mode=ask --print --resume` turn returned its requested text but emitted two byte-identical `sessionEnd` hooks and no prompt, response, stop, or token hooks. RimZ does not parse its native result or claim hook coverage for this transport.
- **Live verification** remains required for interactive hook command shell and `$PPID` semantics on each platform, the `--` prompt terminator, neutral `{}` under every approval mode, `conversation_id` stability across resume and clear, and Claude-compatible third-party-hook cross-fire.
