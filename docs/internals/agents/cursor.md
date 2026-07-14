# Cursor adapter

> The agent-agnostic boundary and state machine are in [model.md](./model.md). The verified upstream hook and CLI surface is in [cursor-reference.md](../../externals/agent-adapter/cursor-reference.md).

Cursor runs as `agent` or its `cursor-agent` alias; `cursor` names the IDE and is intentionally outside binary discovery. RimZ installs additive user hooks in `~/.cursor/hooks.json` and a managed command statusline in `~/.cursor/cli-config.json`, launches the resolved CLI path, and keys every session on `conversation_id`.

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

The command statusline is the rich-context authority. Its structured payload supplies the display model, model parameters, agent version, output style, vim mode, context window and fill, and current input/output/cache composition. RimZ normalizes Cursor's internal `default` model sentinel to `auto`. For an explicit selection, the adapter separates an exact `param_summary` suffix from the base display model, normalizes its recognized reasoning level, and retains qualifiers such as `Fast` and `Thinking` on the model identity; an ambiguous or nonmatching display stays intact. The independent live `context_window_size` is the displayed window and gauge denominator, rather than the summary's nominal selector magnitude. A stock session with no explicit display selection remains `Auto`, and every optional field is parsed independently so one malformed value does not discard the rest.

`preCompact.context_usage_percent` and `context_window_size` remain fallbacks and open the compaction bracket. `context_tokens` is occupancy rather than cumulative usage, so it does not populate `total_tokens`. Interactive `stop.input_tokens` includes cache-read and cache-write tokens; RimZ derives fresh input with saturating subtraction and retains output, cache-read, and cache-write independently. Explicit zeroes remain visible and these per-turn counters never populate cumulative `total_tokens`. A missed-stop transcript recovery restores only terminal state because the JSONL terminal row carries no tokens. Hook `model_id` labels the row when no statusline context is available, with legacy `model` as fallback; the common `model_params` entry named `effort` supplies the displayed effort.

Cursor CLI `2026.07.09-a3815c0` writes one JSONL file at `~/.cursor/projects/<workspace>/agent-transcripts/<conversation_id>/<conversation_id>.jsonl`. An authenticated native resume rewrote that same path as a full conversation snapshot, replacing the prior terminal placement rather than appending a new suffix. RimZ stats the file, reads its bounded whole tail, and recovers a missed success, interruption, or error boundary only when a complete recognized `turn_ended` row is the last meaningful record and no torn suffix follows it. A later nonterminal, unknown, malformed, or partial record keeps the active turn running until a new complete terminal row or full snapshot arrives. RimZ never models or consumes assistant, thinking, user, tool, or message content from this file. Resolution prefers the current hook path, then the persisted path, then one unambiguous exact conversation match beneath the immediate project directories. Workspace ownership continues to come from the stamped pane/session relationship; `postToolUse.cwd` is enrichment only.

`afterAgentResponse.text` is Cursor's sole safe final-text source. Hook ingestion appends that trimmed response to RimZ's durable transcript and seeds an active supervised run without ending it; the later `stop` remains the delivery checkpoint and terminal status transition. Cursor's native JSONL merges visible assistant commentary with model thinking into indistinguishable text blocks, so native history paging and reply streaming deliberately remain empty.

Cursor installation manages two files as one operation. RimZ builds both JSON candidates before writing, writes each by temp-file plus rename, rolls the hook file back byte-for-byte if the statusline write fails, and reports both diffs at consent. The statusline wrapper retains the user's exact prior value under a managed marker, forwards its JSON stdin to that command by direct argv, and restores the value on uninstall. Existing rendering keys remain in place. Incomplete-hook detection requires both the canonical hook set and the managed statusline, so `rimz hooks install cursor` repairs either half while preserving user-owned and unknown entries.

## Account and balance

Cursor documents `status --format json` and `about --format json` without publishing either response schema or a credential format. A sanitized authenticated browser-login capture on `2026.07.09-a3815c0` pins one arm: `status` carries `isAuthenticated`, token-presence booleans, and structured `userInfo`; `about` carries non-empty string `subscriptionTier` and `userEmail` fields plus `cliVersion`. Those fields are candidate `probe_account` inputs ([cursor-reference.md → Authentication](../../externals/agent-adapter/cursor-reference.md#authentication-and-account-surface)). The adapter still reports account state, spend, and quota as unsupported because unauthenticated, expired, API-key, service-account, proxy, and server-error arms remain unverified; building a probe from one successful login arm is the failure mode the model warns against ([model.md → Adding an agent](./model.md#adding-an-agent)). The only machine-readable usage feed Cursor ships is the team-scoped Admin API behind an admin token, which is not a stock per-user CLI credential.

## Cost

The stop hook exposes per-turn token composition but no dollars. RimZ calculates an API-equivalent estimate for completed, aborted, and errored stops with a non-empty `generation_id`: Cursor Auto uses `$1.25/M` input and cache-create, `$6.00/M` output, and `$0.25/M` cache-read, while explicit model IDs use the shared price book and its fast-variant multiplier. Unknown models and incomplete pricing stay absent rather than publishing a known zero.

The agent-context sidecar stores the last priced generation and cumulative estimate under a per-session lock. A duplicate stop is ignored, later generations add exactly once, and statusline refreshes preserve the total. The value drives the live card, cockpit add-back, and agent budget, then resets when that live session sidecar ends. Cursor still has no per-user historical usage ledger, provider-billing total, account spend, or quota parser.

## Launch and permission modes

RimZ maps Ask to Cursor's default launch, Plan to `--mode=plan`, Auto to `--auto-review`, and Yolo to `--force --sandbox disabled`. Auto-review lets allowlisted and sandboxable calls proceed and sends the remainder through Cursor's classifier; Yolo explicitly selects the unrestricted posture. Fresh and supervised launches remain ordinary interactive positional argv with no `-p` or `--print`. `/summarize` is the manual compaction command, `--resume <conversation_id>` resumes, and no CLI-by-id fork surface is declared.

The shared command classifier now reads adapter `bin_names`, so both stock entrypoints (`agent` and `cursor-agent`) produce Cursor presence before the first session hook; this is the first built-in whose executable basename differs from its kind. The installed Linux binary reports `MainThread` as its kernel `comm`, so hook attribution continues to use the installer-stamped `$PPID` and durable pane/session stamps rather than treating that generic runtime label as Cursor identity.

## Wired now

Interactive session identity, turn boundaries including native interruption, safe final responses, normalized per-turn token composition, bounded transcript-tail recovery, mutating-tool activity and the acting phase, the compaction-open bracket, statusline-backed model/window/fill context, cumulative live-session cost estimation, two-file hook/statusline install and uninstall, session end, resume, the four permission modes, and manual `/summarize` compaction. The launch-flag and statusline surfaces are verified against the installed `2026.07.09-a3815c0` build; the runtime *interaction* of the permission flags with neutral `{}` output still needs a live interactive session.

## Deferred to a later round

- **Subagents.** `subagentStop` omits the child id `subagentStart` supplies, so neither signal is wired until a live capture proves a stable stop-side correlation key.
- **Account and historical spend.** One authenticated browser-login status/about arm is captured, but the other auth and failure arms remain unverified; account billing, quota windows, and historical spend remain blocked on a machine-readable per-user usage feed Cursor does not yet ship.
- **Full native history and streaming.** Safe final responses reach RimZ's transcript and supervised output, but native assistant-history replay and incremental reply streaming remain empty because Cursor's JSONL text merges visible output with thinking.
- **Native headless transport.** One authenticated `--mode=ask --print --resume` turn returned its requested text but emitted two byte-identical `sessionEnd` hooks and no prompt, response, stop, or token hooks. RimZ does not parse its native result or claim hook coverage for this transport.
- **Live verification** remains required for interactive hook command shell and `$PPID` semantics on each platform, the `--` prompt terminator, neutral `{}` under every approval mode, `conversation_id` stability across resume and clear, and Claude-compatible third-party-hook cross-fire.
