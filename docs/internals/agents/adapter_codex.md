# Codex adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Binding, same-pane ownership, and session death are in [instances.md](./instances.md); accounts, balances, and spend are in [providers.md](./providers.md); the raw upstream protocol is in [codex-reference.md](../../externals/agent-adapter/codex-reference.md).

`CodexAdapter` maps Codex hooks, rollout files, and the read-only `codex app-server` onto RimZ's agent model, and `CODEX_DESCRIPTOR` declares its launch facts. Codex has no statusline, so the live gauge comes from the rollout tail and the remaining session metadata comes from the app-server. The code lives under [`agents/adapters/codex/`](../../../crates/rimz/src/agents/adapters/codex/mod.rs):

| Module | Owns |
| --- | --- |
| `mod.rs` | Hook catalog, `decode_hook`, lifecycle mapping, `CODEX_DESCRIPTOR`, capability impls |
| `payloads.rs` | Typed hook payloads |
| `ask.rs` | Structured answers for `rimz answer` |
| `install.rs` | `config.toml` hook merge, uninstall, hook trust, `codex_config_path` |
| `project_trust.rs` | Read-only `[projects]` directory trust for the launch preflight |
| `rollout.rs` | Rollout JSONL decoder: header, messages, usage, terminal records |
| `transcript.rs` | Rollout lookup, tail scan, local context refresh, turn-death confirmation |
| `session_index.rs` | `session_index.jsonl` thread names |
| `local_sessions.rs` | Local session discovery over the rollout stores |
| `process.rs` | Codex CLI and daemon process classifiers |
| `app_server.rs`, `app_server/wire.rs`, `app_server/transport.rs` | Read-only app-server client, response models, JSON-RPC transport |
| `app_server/daemon.rs` | Managed remote-control daemon lifecycle |
| `broker.rs` | Per-session warm app-server broker |
| `account.rs`, `oauth_usage.rs` | Account probe, window and credit normalization, direct OAuth usage probe |
| `spend.rs`, `spend/parse.rs` | Full-history spend parser |

## Hooks and lifecycle

Every hook runs `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex`, and the neutral answer is empty stdout: permission and question prompts stay in Codex's own UI. [`map_codex_lifecycle_signal`](../../../crates/rimz/src/agents/adapters/codex/mod.rs) turns each installed event into one [`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs). The upstream payloads are in [codex-reference.md](../../externals/agent-adapter/codex-reference.md#hooks).

| Event | Matcher | Signal |
| --- | --- | --- |
| `SessionStart` | `startup\|resume\|clear\|compact` | `Registered`; source `compact` gives `CompactionEnded` with no trigger |
| `UserPromptSubmit` | none | `TurnStarted` |
| `SubagentStart` | `.*` | `SubagentStarted` |
| `SubagentStop` | `.*` | `SubagentStopped { errored }` |
| `Stop` | none | `AwaitingInput { PlanApproval }` when the rollout rests on a plan, otherwise `TurnEnded { errored, parked_on_background: false }` |
| `Interrupt` | none | `TurnInterrupted { turn_id }` |
| `PermissionRequest` | `.*` | `AwaitingInput { Permission }` |
| `PreToolUse`, tool `request_user_input` | `.*` | `AwaitingInput { Question }` |
| `PreToolUse`, any other tool | `.*` | `ToolUsed` with no name, `mutates` and `edits` false |
| `PostToolUse` | `.*` | `ToolUsed` with the tool name, `mutates`, `edits`, and `turn_id` |
| `PreCompact` | `.*` | `Compacting` |
| `PostCompact` | `.*` | `CompactionEnded` with the auto or manual trigger |

Root `Registered`, `TurnStarted`, and compaction closes from `SessionStart` also carry the rollout lineage ([session registration](#session-registration-and-launch-quirks)). Codex has a `SessionEnd` hook, and RimZ leaves it unwired because upstream also fires it when the app-server unloads an idle thread, which does not prove the pane died. The [reaper](./instances.md#session-death) stamps the end once pane liveness proves the process gone. Codex has no `Notification` hook and no background-task parking, so `parked_on_background` is always false.

### Tool events

`PostToolUse` carries the tool name, so the store appends every one and replay counts it into `tool_calls`. The unnamed `PreToolUse` signal is proof of work: it keeps the heartbeat current and is appended only under the rule in [model.md](./model.md#the-state-machine), so one call counts once. `apply_patch` sets `edits`.

The hooks are a progress signal and not an enforcement boundary. Upstream routes shell calls (reported as `Bash`), `apply_patch`, and MCP calls through `PreToolUse` and `PostToolUse`; web search and some other tool paths bypass both. The [spend parser](#cost) reads tool calls from the rollout, so historical counts stay complete.

### Turn endings

On `Stop` and `SubagentStop`, `decode_hook` scans the rollout tail once with [`scan_transcript_tail`](../../../crates/rimz/src/agents/adapters/codex/transcript.rs). An explicit rollout error record, or an error flag in the stop payload, marks the signal errored, so a provider-killed turn never ends as success. The same scan decides whether `Stop` rests on a plan ([Plan-approval marker](#plan-approval-marker)).

`Interrupt` fires only for a root turn and forwards Codex's `turn_id`; so do `PreToolUse` and `PostToolUse`. The rollup records the interrupted turn id and ignores a trailing tool completion from that turn, so a late `PostToolUse` cannot reopen the rested root. Codex versions without the hook ignore the installed key, and the rollout's [interruption marker](#turn-interruption-marker) settles those turns.

### Asks and answers

| Ask | Opened by | Closed by | `rimz answer` |
| --- | --- | --- | --- |
| Permission | `PermissionRequest` | the agent's next activity ([waiting and asks](./model.md#waiting-and-asks)) | refused: "permission answers require the Codex pane" |
| Question | `PreToolUse` for `request_user_input`, with question ids, headings, labels, and descriptions parsed into options | `PostToolUse` for the same tool, carrying the id-keyed answers | one option per question: Down to the option, then Enter, which advances to the next question and submits after the last |
| Plan approval | `Stop` resting on a rollout plan | `UserPromptSubmit`, which records the submitted prompt (capped at 1000 characters) as the answer | the single option `implement`: Enter on the default "Yes, implement this plan" row, which also switches Codex from Plan mode to Default mode |

[`ask.rs`](../../../crates/rimz/src/agents/adapters/codex/ask.rs) drives the subset verified against Codex 0.144.3. Multi-select questions, free-text and "other" options, plan refinement text, and alternate plan actions stay in the pane. `update_plan` is a todo tracker and opens no ask. Leaving Plan mode with Shift+Tab without submitting a prompt leaves the plan ask open until the next hook.

### Install

[`install.rs`](../../../crates/rimz/src/agents/adapters/codex/install.rs) rewrites Codex's `config.toml` in place. `codex_config_path` resolves the file the way Codex does: `RIMZ_CODEX_CONFIG` (a test and tooling override), then `$CODEX_HOME/config.toml`, then `~/.codex/config.toml`. It reads those keys from the login env its caller passes, so a named account's `CODEX_HOME` applies without touching the process env. Hook install, hook trust, and the [directory trust preflight](#directory-trust-preflight) all share it.

Install writes one inline `[[hooks.<Event>]]` table per catalog event, each with a `type = "command"` handler, the command, a timeout, and `statusMessage = "Routing <Event> through RimZ"`. Every hook gets 10 seconds (`CODEX_HOOK_TIMEOUT_SECS`) except `Interrupt`, which gets 1 second because Codex awaits it inline with a 1 second default and a 3 second ceiling. TOML has no `_rimz_managed` marker, so install and uninstall reclaim RimZ's entries by the `rimz hooks feed --source codex` command substring and leave user hooks untouched. Uninstall also removes the legacy `[hooks.rimz]` table, which Codex ignores. A missing or empty file installs into an empty table; invalid TOML fails with a parse error.

Codex gates each installed hook behind its own per-hash trust state and silently skips an untrusted one ([codex-reference.md](../../externals/agent-adapter/codex-reference.md#trust-state)). Only the user can trust it, with `/hooks` inside Codex. `untrusted_installed_hooks` reads the `[hooks.state]` table and reports every installed but untrusted event to `rimz start` and `rimz doctor`. Hook-driven launch preflights (`untrusted_preflight_hooks`) require the same set minus `Interrupt`, because older Codex versions ignore that key and the rollout marker covers them.

## Launch

`CODEX_DESCRIPTOR` renders launch intent into Codex argv. The initial prompt is positional, after `--`.

| Intent | Codex argv |
| --- | --- |
| Resume a session | `codex resume <id>` |
| Fork a session | `codex fork <id>` |
| Permission `ask`, `plan` | no flags |
| Permission `auto` | `--ask-for-approval never --sandbox workspace-write` |
| Permission `yolo` | `--dangerously-bypass-approvals-and-sandbox` |
| Model | `--model <model>` |
| Effort | `-c model_reasoning_effort=<effort>` |
| Profile `auto-compact` | `-c model_auto_compact_token_limit=<tokens>`; Codex caps it at 90% of the context window ([upstream](../../externals/agent-adapter/codex-reference.md#cli-config-overrides)) |
| System prompt replacement | `-c model_instructions_file=<path>`: the resolved base file, or a content-addressed composed file when fragments exist |
| Appended text, such as the subagent no-delegation reminder | `-c developer_instructions=<text>`, a developer-role message that leaves the user prompt unchanged |
| Supervised child lockdown | drops every `features.multi_agent` override, then appends `-c features.multi_agent=false` |
| Max turns | unsupported |
| Compaction | bare `/compact`; Codex accepts no summary brief, so the configured brief is never sent |

The descriptor also sets `registers_lazily`, `ThreadKey::PerFile`, the `KeepPrimary` same-pane policy, and process names `codex` and `node`.

### Directory trust preflight

An undecided launch directory stops Codex at its trust screen before the first prompt, so a supervised `rimz agents -p` run started there would sit in `Pending` until its deadline. [`preflight_agent`](../../../crates/rimz/src/cli/supervised.rs) therefore checks Codex's `[projects]` trust for the resolved checkout after the hook preflight and before the run record, the store append, and any mux action; a `--worktree` launch has already created its tree by then. Interactive `rimz agents codex` launches skip the check, because a human at the pane can answer the screen.

[`trust_gap_at`](../../../crates/rimz/src/agents/adapters/codex/project_trust.rs) is read-only: it never records a level or answers the prompt. It follows [upstream's rules](../../externals/agent-adapter/codex-reference.md#project-directory-trust), which stay the authority:

- The launch is decided when any `trust_level`, `trusted` or `untrusted`, is recorded for the launch cwd, the nearest `project_root_markers` ancestor (default `.git`), or the main repository root. Each key matches both canonicalized and as written.
- The main repository root is `LaunchCheckout.repo_root`, which the workspace resolver already walked up from a linked worktree, so the check runs no git probe. An explicit `--root` replaces it, and a peer launch may then need its own trust entry.
- A refusal names both fixes: run `codex` once in the directory, or add `[projects."<root>"]` with `trust_level = "trusted"`.
- Valid TOML with malformed trust values refuses with a repair instruction even when another key would match. A missing or unreadable file normally fails the hook preflight first; this check still reports it if the file changes between the reads.

RimZ reads the one resolved config file, not Codex's merged layer stack. Provider passthrough `--cd`/`-C`, `-c` overrides, and launch-specific environment changes are not modeled.

## Session registration and launch quirks

Codex registers its session lazily. A plain launch fires no hook; the first prompt fires `SessionStart` and `UserPromptSubmit` together. Until then the pane is an instance with no session, and the sidebar shows its [idle placeholder row](./instances.md#before-a-session-binds). A `codex resume <id>` launch already knows its id, so the exec wrapper attaches the card before Codex registers ([binding](./instances.md#binding-a-session)).

Codex hooks are daemon-routed: they fire from the shared per-user app-server with the session cwd as working directory, the daemon's pid as `RIMZ_AGENT_PID`, and the daemon's environment. `rimz hooks feed` classifies that owner as a daemon, ignores the ambient workspace and pane pins, and recovers the room from the in-pane `codex` process at the same cwd ([adapter.md](./adapter.md#hooks-resolve-the-room-they-live-in)). The session stays unstamped until pane recovery binds it and re-owns liveness to the in-pane CLI; an unbound daemon-owned session ages out through the ghost TTL, with `thread/loaded/list` as the faster keep or drop signal ([session death](./instances.md#session-death)). Hooks from app-servers RimZ spawns itself carry `RIMZ_CODEX_INTERNAL_APP_SERVER` and are dropped, so enrichment never feeds back into hooks.

Several roots can share one Codex pane, and the rollout header tells them apart. [`session_origin`](../../../crates/rimz/src/agents/adapters/codex/transcript.rs) reads each root's `session_meta` on identity events:

| Rollout header | Meaning | Rule in instances.md |
| --- | --- | --- |
| `forked_from_id` null | a fresh `/clear` or `/new` conversation (Codex also fires `SessionStart` for it) | [fresh conversation](./instances.md#supersession) retires the rested predecessor |
| `forked_from_id` set, continuation of a compaction | the same conversation under a new id; RimZ stamps `compacted_from` | [compaction continuation](./instances.md#supersession), and the successor inherits `registered_at` |
| `forked_from_id` set, anything else | `/side`, `/btw`, `/fork`, or a provider-death retry, which Codex does not distinguish | [forked retry](./instances.md#supersession) retires only an errored predecessor; otherwise [`KeepPrimary`](./instances.md#same-pane-ownership) decides |
| unreadable | lineage unknown | no lineage rule applies |

Every same-instance root carries the [launch identity](./instances.md#launch-identity-across-conversations), and launched children keep their parent link across forks and continuations ([subagents.md](../harness/subagents.md#launch-generations-and-parentage)). On the first prompt after `/clear`, daemon-routed recovery stamps the new root onto the focused Codex pane, or without focus evidence onto the sole occupied pane whose owner is at rest, so the fresh-conversation rule can fire.

### Local session discovery

[`local_sessions.rs`](../../../crates/rimz/src/agents/adapters/codex/local_sessions.rs) reads the first `session_meta` line of recent rollouts under `$CODEX_HOME`, or `~/.codex`. The elected room producer runs one discovery for all admitted Codex workspaces and publishes the observations; consumers never enumerate the rollout store ([local session observations](./instances.md#local-session-observations)).

| Rule | Value |
| --- | --- |
| Active rollouts | `sessions/YYYY/MM/DD/`, the 15 UTC date directories from today back 14 days, newest first |
| Archived rollouts | flat `archived_sessions/` files whose filename date is within 14 days; an active twin wins |
| Scan cap | 512 files |
| Rescan | cold start, topology change, UTC date rollover, or the 30 second backstop; unchanged stamps reuse the cached header |
| Admission | header `cwd` exactly equals an admitted workspace |
| Excluded | `thread_source = "subagent"` or `source.subagent.thread_spawn`; `forked_from_id` alone does not exclude |
| Session id | header `id`, else the filename UUID |
| Clocks | created at the header timestamp; last activity at the file mtime |
| Projection | `IdentityOnly` |

The cache never reads the store, so a cwd match alone is not identity proof: the snapshot fold admits the fallback only when the rollout postdates the pane's process start or RimZ's launch clock.

## Subagents

A child hook needs a non-empty `agent_id` that differs from the root `session_id`; `resolve_subagent_identity` quarantines a malformed `SubagentStart` or `SubagentStop` instead of folding it onto the parent. Codex also puts the child's `agent_id` on `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, and `PostCompact`, so those signals update the child row and its heartbeat ([model.md](./model.md#subagents)). The hook-reported root stays `parent_agent_id` for every descendant; the rollout `parent_thread_id` is metadata only.

Child hooks never merge into the root's context or spawn the app-server refresh, so the root session id and rollout cannot contaminate the child. The child's rollout is `agent_transcript_path`, else `transcript_path` when its header id matches the child, else a search by the child's `agent_id`, never by the root id. When the header marks a subagent, it supplies display metadata:

| Card field | Source | Fallback |
| --- | --- | --- |
| name | header `agent_nickname` | hook `agent_type` |
| task | header `agent_path`, with the leading `root` segment removed | hook `agent_type` |
| role | header `agent_role` | hook `agent_type` |

Descendants stay flat under the root card in creation order, with the relative path showing nesting. A supervised child launched by RimZ gets the lockdown and reminder args from [Launch](#launch).

## Context and transcript

Codex writes one rollout per session at `sessions/YYYY/MM/DD/rollout-<timestamp>-<session_id>.jsonl` under `RIMZ_CODEX_SESSIONS`, `$CODEX_HOME`, or `~/.codex`, and moves archived rollouts into the flat `archived_sessions/`. A root hook uses its `transcript_path`; otherwise [`find_session_transcript`](../../../crates/rimz/src/agents/adapters/codex/transcript.rs) walks back up to 16 day directories and then the archive. The header feeds lineage and child metadata; the tail, read under the shared [reading rules](./adapter.md#reading-rules), feeds usage and the resting-turn markers.

| Rollout record | Field | Internal |
| --- | --- | --- |
| `token_count` | `info.last_token_usage.input_tokens` | current context tokens |
| `token_count` | `info.model_context_window` | context window; 272,000 (`DEFAULT_CONTEXT_WINDOW`) until the first reading |
| `token_count` | `info.last_token_usage.total_tokens` | `total_tokens` for a child |
| `token_count` | `info.last_token_usage.cached_input_tokens` | `cache_read_input_tokens` (the card's `◌`) |
| `token_count` | `info.last_token_usage.cache_write_input_tokens` | `cache_write_input_tokens` (the card's `◍`) |
| `token_count` | `input_tokens − cached_input_tokens − cache_write_input_tokens` | `fresh_input_tokens` (the card's `↘`); `input_tokens` includes both cache slices |
| `token_count` | `info.last_token_usage.output_tokens` | `output_tokens` (the card's `↗`) |
| `token_count` | deltas of `info.total_token_usage` across the rollout | cumulative `session_usage` and `cost` |
| `turn_context` | `model`, `effort` | model and live reasoning effort |

The adapter emits raw tokens and the window; the snapshot fold derives the gauge percentage. A rollout with no `token_count` yet reports zeros against the fallback window. For a child, `total_token_usage` includes the copied parent history, so it never becomes child lifetime usage or spend. The model falls back from the rollout to `model` in `config.toml`.

`parse_transcript_messages` is the one conversation parser: it keeps `user_message` and `agent_message` events and completed user and agent message items, with their timestamps.

### Local refresh

[`refresh_transcript_context`](../../../crates/rimz/src/agents/adapters/codex/transcript.rs) runs inline and stat-gated: it reads the tail only when the rollout's stat changed, or when a live spend fold still needs token-counter backfill. It resumes the spend fold from the byte cursor on the context sidecar, so later refreshes price only appended requests, and an unknown model adds zero until the history walk's reprice heals the tally ([providers.md](./providers.md#token-pricing)). The same pass reads the newest name for the session from `$CODEX_HOME/session_index.jsonl` into `session_name`, ahead of the first-message preview.

Four callers run it, and all write the runtime sidecar and wake the sidebar without touching the durable store:

- `rimz hooks feed` on `SessionStart`, `UserPromptSubmit`, `PostToolUse`, and `Stop`, after the decision is written.
- The hidden `rimz agents refresh-context` helper, before its app-server work.
- The elder renderer's transcript watcher, on rollout growth ([state.md](../sidebar/state.md#push-channels)).
- The elected snapshot producer, as a tick backstop for live root rows.

### App-server enrichment

The official `codex app-server` supplies what the rollout cannot: rate-limit windows, account plan and credits, model display name, thread name and preview, and Codex version. [`app_server.rs`](../../../crates/rimz/src/agents/adapters/codex/app_server.rs) speaks only read-only methods (`initialize`, `account/rateLimits/read`, `model/list`, `thread/read`, with `thread/list` as the name and preview fallback, and `thread/loaded/list`) and never `thread/resume` or `turn/start`, which would take over the user's live thread. Token usage appears only on a subscribing `thread/resume` notification, which is why the rollout owns the gauge. `model/list`'s `defaultReasoningEffort` is a catalog default, so it never fills the row's effort. Method schemas are in [codex-reference.md](../../externals/agent-adapter/codex-reference.md#app-server-api).

Hooks never call the app-server inline. `SessionStart`, `UserPromptSubmit`, and `Stop` spawn `rimz agents refresh-context` detached with null stdio, and the producer spawns the same helper for each live root session at most once per 60 seconds (`SESSION_REFRESH_INTERVAL`), gated by marker files in the runtime shared directory that store GC reaps after 5 minutes (`SESSION_PROBE_MARKER_TTL`). The helper does the local refresh, then the app-server read only when `app_server_due` finds `rate_limits_observed_at` older than 20 seconds. Every failure leaves a field unset.

The client connects to the warmest server available, each with a 6 second deadline:

1. The per-session [broker](../../../crates/rimz/src/agents/adapters/codex/broker.rs), run as `rimz codex app-server serve` in the `rimzd` tab, holds one handshaked `codex app-server` behind a unix socket. It answers `initialize` from cache, respawns and retries once on a closed stream, and respawns when `auth.json` changes.
2. The per-user remote-control daemon's WebSocket control socket, at `RIMZ_CODEX_APP_SERVER_SOCK` or `$CODEX_HOME/app-server-control/app-server-control.sock`. An empty `RIMZ_CODEX_APP_SERVER_SOCK` skips it.
3. A cold-spawned `codex app-server` (`RIMZ_CODEX_BIN` overrides the binary), so headless use still enriches.

### Resting-turn markers

The tail scan also classifies how the newest turn rests, which settles rows whose closing hook never came. `scan_transcript_tail` walks the tail newest first and returns a [`RestingTurnOutcome`](../../../crates/rimz/src/agents/adapters/codex/transcript.rs): a live record (`turn_context`, `agent_message`, `task_started`, or `user_message`) proves recovery and ends the scan with no outcome, while `token_count` is ignored. The local refresh writes the outcome into the context sidecar, as `AgentContext.settle` or `AgentContext.turn_error` with the record's timestamp, and clears both when the tail proves recovery. The projection uses a marker only while it postdates `last_activity` ([displayed status](./model.md#displayed-status)), so the next prompt retires it.

| Tail rests on | Outcome | Marker |
| --- | --- | --- |
| clean `task_complete` whose turn has a completed non-empty `Plan` item | `PlanProposed` | [plan approval](#plan-approval-marker) |
| an error record | `Died` | [turn death](#turn-death-marker) |
| `task_complete` with no error field and an empty or missing `last_agent_message` | `Died`, class unknown | [turn death](#turn-death-marker) |
| `task_complete` with no error field and a non-empty `last_agent_message` | `Complete` | [turn completion](#turn-completion-marker) |
| `turn_aborted` | `Interrupted` | [turn interruption](#turn-interruption-marker) |
| `task_complete` with an empty error field (`null`, `false`, `""`, `{}`) | none | none |

The stat gate skips an unchanged rollout, so a marker lands only after the rollout's next write.

#### Plan-approval marker

Codex Plan mode ends a planning turn with an `item_completed` record whose item is a non-empty `Plan`, followed by a clean `task_complete` for the same `turn_id`; the plan body is not in `last_agent_message`. On `Stop`, that shape replaces `TurnEnded` with `AwaitingInput { PlanApproval }`, so the ask feed records the plan and a supervised `-p` run waits instead of completing with an empty result.

When `Stop` was missed, the local refresh stamps `settle` as `PlanProposed`. A `running` row then displays `waiting`, which keeps message delivery away from the plan prompt. The marker creates no durable ask.

#### Turn-death marker

Codex writes most provider errors to the rollout. An error record is a `turn_error`, `stream_error`, or `error` event, or a `task_complete` that carries an error. RimZ caps its label at 80 characters and classifies it: the Codex error kind decides first, in either the app-server camelCase or the rollout snake_case spelling.

| Codex error kind | Class |
| --- | --- |
| `usage_limit_exceeded` | `PausedRateLimit` |
| `server_overloaded`, `internal_server_error` | `PausedOverloaded` |
| `context_window_exceeded`, `unauthorized`, `bad_request`, `sandbox_error`, `cyber_policy`, `thread_rollback_failed` | `Failed` |
| `other`, unknown, or absent | the label, through [`TurnErrorClass::classify_label`](../../../crates/rimz/src/agents/context.rs) |

`classify_label` maps spend-limit text to `PausedSpendLimit`, rate-limit and quota text or HTTP 429 to `PausedRateLimit`, overload, server-error, timeout, and connection text or HTTP 5xx to `PausedOverloaded`, and everything else to `Failed`. On `Stop` the same record marks the turn errored; the projection shows the row paused or failed by class.

A `task_complete` with no final message is the silent capacity kill: Codex can end a turn on a capacity or usage limit with no error record, no `Stop`, and the warning shown only in the TUI. The local refresh records it as class unknown with the label `turn ended with no final message`, and two confirmation steps try to name it:

1. [`confirm_codex_turn_death_from_pane`](../../../crates/rimz/src/sidebar/refresh/sessions.rs) captures the last 60 lines of a bound live pane and scans the frame above the `›` input prompt from the bottom up. A line counts when it is a `⚠` banner or its label classifies as a limit, so ordinary agent output that mentions a server error cannot impersonate a provider warning. The nearest match supplies the label and class; an unrecognized `⚠` banner lands as `Failed` with its upstream text. Continuation lines join the label until a blank line, the prompt, or a line that starts with no letter or digit.
2. Without pane proof, `infer_turn_death_from_spent_window` reads the account's fused windows and sets `PausedRateLimit`, labelled `usage limit inferred (rate-limit window spent)`, only when a window is spent with a future reset.

The producer retries these steps each tick while the marker is unconfirmed and younger than 10 minutes (`CODEX_TURN_DEATH_RETRY_WINDOW`), and `rimz agents refresh @codex` runs them on demand. An explicit error record never needs them.

#### Turn-completion marker

Codex `/review` runs in review mode and ends on a clean `task_complete` without firing `Stop`, so the lifecycle row stays `running`. A clean `task_complete` has no error field and a non-empty `last_agent_message`. The local refresh stamps `settle` as `Complete`, and the projection shows the row as `success`.

#### Turn-interruption marker

Codex writes `turn_aborted` when Esc aborts a turn and when `/clear` interrupts one. RimZ accepts any `reason`, because the abort resting at the tail is what matters. The local refresh stamps `settle` as `Interrupted`, and the projection shows the row as `idle`. This marker is the fallback for Codex versions without the [`Interrupt` hook](#turn-endings). A steer that replaces the turn writes new live records after the abort, so it never settles.

## Remote control

[`app_server/daemon.rs`](../../../crates/rimz/src/agents/adapters/codex/app_server/daemon.rs) manages the per-user daemon that `codex remote-control start` runs. Room birth and runtime toggles call it through `readiness`, `ensure`, and `reconcile`; the provider-neutral remote-control module coordinates the result with the room and sidebar. Provider commands run from the durable `CODEX_HOME`.

- `readiness` is ready when the managed standalone binary exists at `$CODEX_HOME/packages/standalone/current/codex`, and otherwise returns install guidance (`curl -fsSL https://chatgpt.com/codex/install.sh | sh`).
- `ensure` recovers a stale updater, then starts the daemon. Recovery signals the updater only when its pid records, process start times, ownership, executable, argv, and sole zombie child match the known upstream stale shape.
- `reconcile` retries once: after a stale recovery, or after a 3 second settle for a failed start, which absorbs the upstream stop-then-start teardown race.
- `updater_skew` is a `rimz doctor` advisory. It fires only when the control socket exists, the updater pid still matches its recorded identity and uid, and its executable differs from the managed standalone binary. The message says the updater's next hourly pass converges on its own and prints the managed binary's `app-server daemon bootstrap --remote-control` repair, which restarts both processes under Codex's lifecycle lock.

The app-server client reuses this daemon as its second connection choice, and the loaded-thread reaper reads its `thread/loaded/list` ([session death](./instances.md#session-death)).

## Account and balance

Codex has two account sources, and both normalize through [`account.rs`](../../../crates/rimz/src/agents/adapters/codex/account.rs) into the model in [providers.md](./providers.md).

| Source | Supplies | When |
| --- | --- | --- |
| App-server `account/rateLimits/read` | plan (`planType`), `primary` and `secondary` windows with `windowDurationMins`, credits | on the session refresh, and first on each account usage refresh |
| Direct OAuth usage endpoint ([`oauth_usage.rs`](../../../crates/rimz/src/agents/adapters/codex/oauth_usage.rs)) | plan (`plan_type`), windows, credits, reset credits | after the realtime read, on `OAUTH_USAGE_TTL` (5 minutes), for a file-backed token, unless `RIMZ_OAUTH_USAGE_OFFLINE` is set |
| Account probe | login state and metering | out of band, [providers.md](./providers.md#the-out-of-band-probe) |

The producer's `rimz agents refresh-usage` helper runs for a metered Codex login whether or not a session is live: it claims the read in `credits.json`, publishes the realtime read, then runs the direct probe with the `tokens.account_id` it read. An account id change drops cached windows and refetches. A present realtime plan wins; an absent one keeps the cached OAuth plan for idle and keyring-backed display.

Normalization clamps and rounds percentages and orders windows short to long. When an authoritative reading carries timed windows but no 5-hour row, it adds an empty lifted 5-hour row so the dashboard still shows the expected pair. Credits map as follows:

| Wire field | `ExtraCredits` |
| --- | --- |
| `overageLimitReached` | known, 0 remaining |
| `unlimited` | usable, balance unknown |
| `balance`, a number or numeric string | known, clamped at 0 |
| `hasCredits: false` | disabled |

The probe reads `$CODEX_HOME/auth.json` first:

| `auth.json` | Result |
| --- | --- |
| missing | fall back to `codex login status`, for credentials in the OS keyring |
| unreadable | `Unavailable` |
| unparsable, or no credential | `LoggedOut` |
| `OPENAI_API_KEY` | logged in, unmetered |
| `tokens.access_token` | logged in, metered, with the trimmed `tokens.account_id` |

`codex login status` answers `LoggedOut` for "Not logged in", metered for ChatGPT, unmetered for an API key or Bedrock, and `Unavailable` for an unrecognized answer or a timeout.

## Cost

[`spend.rs`](../../../crates/rimz/src/agents/adapters/codex/spend.rs) parses Codex's full history for the [`SpendingWalker`](../../../crates/rimz/src/agents/spending/mod.rs), read-only and without network, into the windows described in [providers.md](./providers.md#cost-history). The stateful line parser is [`spend/parse.rs`](../../../crates/rimz/src/agents/adapters/codex/spend/parse.rs); it reads both the interactive rollout format and the headless flat `usage` format.

- Discovery covers `sessions/`, `archived_sessions/`, and legacy JSONL files elsewhere in each Codex home: every comma-separated `CODEX_HOME` entry, or `~/.codex` when it is unset. An active rollout wins over its archived twin, so archiving never double-counts.
- Ordinary scans prune date partitions outside the 365-day window; the 15-minute complete reconcile ignores pruning.
- Forked and subagent rollouts open with a replay of the parent's cumulative history. The parser suppresses that prefix and keeps its last cumulative total as the baseline for the first new delta.
- A fingerprint of `codex:`, timestamp, model, and token split collapses events copied across files while keeping events that differ at subsecond precision.
- Each `CodexTokenEvent` is priced per model: fresh input (input minus cache reads and writes) at the input rate, cache writes at the cache-create rate, cache reads at the cache-read rate when the price book states one and at the input rate otherwise, and output (which includes reasoning) at the output rate ([token pricing](./providers.md#token-pricing)).
- The fallback model is `gpt-5`. A model with no price keeps its tokens at zero dollars while the pricing refresh looks for a rate.
- Tool calls come from `function_call`, `custom_tool_call`, `local_shell_call`, and `web_search_call` records and attach to the next token event, so tools that bypass hooks still count.
